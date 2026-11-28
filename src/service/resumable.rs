//! The resumable upload engine behind every session URI: it opens a session for an authorized
//! initiation, plans each request against the session, streams its bytes into the catalog and
//! answers with the session's progress or its completed resource. Each protocol adapter parses
//! its own initiation and session URI.
//!
//! A session stores the JSON API object resource when it completes, whichever API's session URI
//! completes it, and every answer is made from that resource through the response options its
//! initiation set. The JSON API owns both, so the engine reaches them only through
//! [`StoredCompletion`], [`CompletionAnswer`] and [`AnswerView`], as the notification payload
//! reaches its resource through `notification::JsonApiObject`.

mod session_headers;

#[cfg(test)]
pub(crate) use session_headers::final_options;

use super::ServiceError;
use super::body::{RequestBody as Incoming, ResponseBody, boxed, empty_response};
use super::context::ObjectCtx;
use super::object_headers::{single, unsigned};
use super::upload_drive::{
    CatalogWait, DriveError, UploadEnd, UploadSender, drive_upload, stream_upload, upload_channel,
};
use crate::{
    acl::Projection,
    cause::BackendFailure,
    csek::CustomerKey,
    input::InputError,
    object::{ObjectError, ObjectKey, ObjectRecord, media_headers::CUSTOM_METADATA_PREFIX},
    storage::{
        ChunkEnd, ChunkWait, Overrun, Progress, ResumableChunk, ResumableError, ResumableOptions,
        SessionId, SessionLease, SessionState, Written, failure::object_unavailable,
    },
    time_format::TimeOutOfRange,
};
use hyper::{Request, Response, StatusCode, header::HeaderValue};
use serde_json::Value;
use session_headers::Range;
use tokio::sync::watch;

/// Renders the object resource a completed session stores. The JSON adapter owns that
/// representation and implements this for `ObjectRecord`.
pub(crate) trait StoredCompletion {
    /// The whole resource in `projection`, its links on `base_url`, with the encryption fields
    /// that the uploader's `key` may see.
    fn to_stored_completion(
        &self,
        base_url: &str,
        projection: Projection,
        key: Option<CustomerKey>,
    ) -> Result<Value, TimeOutOfRange>;
}

/// How each request on a completed session's URI answers with the resource the session stores:
/// through the response options a JSON initiation sets (`projection` and `fields`), whichever
/// API's session URI the request uses. The JSON session handler owns those options and
/// implements this for `ResumableOptions`.
pub(crate) trait CompletionAnswer {
    /// What the answers take from the options; the engine holds it while a chunk is written.
    type View: AnswerView;

    /// Prepared before a chunk stores any byte, so options the adapter cannot apply fail the
    /// request without creating the object.
    fn view(&self) -> Result<Self::View, ServiceError>;
}

/// The part of a session's options that its answers apply, held apart from the session.
pub(crate) trait AnswerView: Send + Sync {
    /// The answer made from the stored `resource`.
    fn respond(
        &self,
        ctx: ObjectCtx<'_>,
        resource: Value,
        links: Links,
    ) -> impl Future<Output = Result<Value, ServiceError>> + Send;
}

/// Which endpoint the links of a completion resource name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Links {
    /// This request completed the session, and the resource was rendered on its endpoint.
    ThisRequest,
    /// An earlier request completed the session, and the stored links name its endpoint.
    Stored,
}

/// Opens a session for an authorized initiation and returns its opaque ID. Each protocol
/// adapter parses its own initiation request and builds its own session URI.
pub(crate) async fn create_session(
    ctx: ObjectCtx<'_>,
    key: ObjectKey<'_>,
    options: ResumableOptions,
    total: Option<u64>,
) -> Result<SessionId, ServiceError> {
    let state = ctx.state;
    super::access::retention::authorize_upload(
        &state.catalog,
        &options.access,
        key.bucket().into_inner(),
        &options.metadata,
    )
    .await?;
    let session = ctx
        .await_catalog(state.catalog.create_resumable_session(key, options, total))
        .await?;
    Ok(session.id)
}

fn incomplete(
    progress: Progress,
    headers: &hyper::HeaderMap,
) -> Result<Response<ResponseBody>, ServiceError> {
    let override_status = single(headers, "x-guploader-no-308")? == Some("yes");
    let mut response = empty_response(if override_status {
        StatusCode::OK
    } else {
        StatusCode::PERMANENT_REDIRECT
    });
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static(crate::mime::TEXT_PLAIN_UTF8),
    );
    if progress.offset > 0 {
        add_persisted_headers(response.headers_mut(), progress);
    }
    if override_status {
        response.headers_mut().insert(
            "x-http-status-code-override",
            HeaderValue::from_static("308"),
        );
    }
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(boxed(response))
}

/// Describes the persisted bytes of a session that holds some: their range, and the digests
/// Cloud Storage sent beside it on the chunk's and a later status request's 308 (recorded
/// 2026-11-15, case obj-resumable-declared-length): the CRC32C in `x-goog-running-hash` and the
/// hex MD5 in `x-range-md5`. The XML session URI sends them too by inference.
fn add_persisted_headers(headers: &mut hyper::HeaderMap, progress: Progress) {
    // Decimal, base64 and hex text is always a valid header value.
    let range = format!("bytes=0-{}", progress.offset.saturating_sub(1));
    let digests = progress.hashes.map(|hashes| {
        [
            (
                "x-goog-running-hash",
                format!(
                    "crc32c={}",
                    crate::base64::encode(&hashes.crc32c.to_be_bytes())
                ),
            ),
            ("x-range-md5", crate::hashing::encode_lower(&hashes.md5)),
        ]
    });
    let fields = std::iter::once(("range", range)).chain(digests.into_iter().flatten());
    for (name, value) in fields {
        if let Ok(value) = HeaderValue::try_from(value) {
            headers.insert(name, value);
        }
    }
}

/// What a chunk request's body is, derived from its Content-Range.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChunkKind {
    /// A status query (`bytes */total`) with an empty body.
    Status,
    /// Data whose length the headers fix.
    Data,
    /// Data that runs to the body's EOF: an open range without a known total or
    /// Content-Length. Its length only bounds the body, and it always completes the object.
    OpenData,
}

/// Where a range puts a chunk's bytes, before they are checked against the session.
struct Extent {
    start: u64,
    length: u64,
    total: Option<u64>,
    kind: ChunkKind,
}

/// Where a chunk request places its bytes in the upload, derived from its headers and the
/// session's acknowledged offset.
struct ChunkPlan {
    start: u64,
    length: u64,
    total: Option<u64>,
    kind: ChunkKind,
    finalize: bool,
}

impl ChunkPlan {
    /// Where the stored chunk's body ends.
    fn body_end(&self) -> ChunkEnd {
        match self.kind {
            ChunkKind::OpenData => ChunkEnd::Open,
            ChunkKind::Status | ChunkKind::Data => ChunkEnd::Exact {
                total: self.total,
                finalize: self.finalize,
            },
        }
    }
}

/// The session state a chunk is planned against.
struct SessionPosition<'a> {
    offset: u64,
    total: Option<u64>,
    state: &'a SessionState,
}

/// Validates a chunk's framing headers against the object size limit before the session is
/// locked, and returns its Content-Range and Content-Length.
fn chunk_framing(
    headers: &hyper::HeaderMap,
    max_bytes: u64,
) -> Result<(Option<Range>, Option<u64>), ServiceError> {
    let range = session_headers::range(headers)?;
    let length = single(headers, "content-length")?
        .map(unsigned)
        .transpose()?;
    if length.is_some_and(|n| n > max_bytes) {
        return Err(ObjectError::TooLarge.into());
    }
    let oversized = match range {
        Some(Range::Data { end, total, .. }) => {
            end >= max_bytes || total.is_some_and(|n| n > max_bytes)
        }
        Some(Range::Status(total)) => total.is_some_and(|n| n > max_bytes),
        Some(Range::Open { start, total }) => {
            start > max_bytes
                || total.is_some_and(|n| n > max_bytes)
                || length.is_some_and(|n| start.saturating_add(n) > max_bytes)
        }
        None => false,
    };
    if oversized {
        return Err(ObjectError::TooLarge.into());
    }
    let mismatch = match range {
        Some(Range::Data { start, end, .. }) => {
            length.is_some_and(|length| length != end - start + 1)
        }
        // An open range runs to the end of the object, so a known total fixes the body length.
        Some(Range::Open {
            start,
            total: Some(total),
        }) => length.is_some_and(|length| length != total - start),
        _ => false,
    };
    if mismatch {
        return Err(
            InputError::session_protocol("Content-Length does not match Content-Range").into(),
        );
    }
    Ok((range, length))
}

/// Resolves an open range (`bytes start-*/total`) into the bytes it carries. The request
/// always completes the object: a known total (declared here or at initiation) fixes the
/// body length; otherwise a declared Content-Length does, and a chunked body without either
/// is bounded only by the object size limit and completes at its EOF.
fn open_extent(
    start: u64,
    total: Option<u64>,
    length: Option<u64>,
    session_total: Option<u64>,
    max_bytes: u64,
) -> Result<Extent, ServiceError> {
    if total.is_some_and(|n| session_total.is_some_and(|t| n != t)) {
        return Err(InputError::session_protocol("Inconsistent upload total").into());
    }
    match (total.or(session_total), length) {
        (Some(total), length) => {
            let remaining = total
                .checked_sub(start)
                .ok_or_else(|| InputError::session_protocol("Inconsistent upload total"))?;
            if length.is_some_and(|n| n != remaining) {
                return Err(InputError::session_protocol(
                    "Content-Length does not match Content-Range",
                )
                .into());
            }
            Ok(Extent {
                start,
                length: remaining,
                total: Some(total),
                kind: ChunkKind::Data,
            })
        }
        (None, Some(length)) => {
            let end = start
                .checked_add(length)
                .ok_or_else(|| InputError::session_protocol("Upload offset overflow"))?;
            Ok(Extent {
                start,
                length,
                total: Some(end),
                kind: ChunkKind::Data,
            })
        }
        (None, None) => Ok(Extent {
            start,
            length: max_bytes.saturating_sub(start),
            total: None,
            kind: ChunkKind::OpenData,
        }),
    }
}

/// Why a chunk request cannot be planned.
enum Refusal {
    /// Refuses this request and leaves the session as it was.
    Request(ServiceError),
    /// A data chunk with an open total that runs past the session's total. Cloud Storage
    /// failed the JSON session on it (recorded 2026-11-15, case obj-resumable-declared-length).
    Overrun(Overrun),
}

impl From<ServiceError> for Refusal {
    fn from(error: ServiceError) -> Self {
        Self::Request(error)
    }
}

impl From<InputError> for Refusal {
    fn from(error: InputError) -> Self {
        Self::Request(error.into())
    }
}

fn plan_chunk(
    headers: &hyper::HeaderMap,
    range: Option<Range>,
    length: Option<u64>,
    session: &SessionPosition<'_>,
    max_bytes: u64,
    uri: SessionUri<'_>,
) -> Result<ChunkPlan, Refusal> {
    let Extent {
        start,
        length,
        total,
        kind,
    } = match range {
        Some(Range::Status(total)) => {
            if length != Some(0) || headers.contains_key("transfer-encoding") {
                return Err(InputError::session_protocol("Status requires an empty body").into());
            }
            Extent {
                start: session.offset,
                length: 0,
                total,
                kind: ChunkKind::Status,
            }
        }
        Some(Range::Data { start, end, total }) => Extent {
            start,
            length: end - start + 1,
            total,
            kind: ChunkKind::Data,
        },
        Some(Range::Open { start, total }) => {
            open_extent(start, total, length, session.total, max_bytes)?
        }
        None => {
            if session.offset != 0 && !session.state.is_complete() {
                return Err(InputError::session_protocol(
                    "Content-Range required after the first chunk",
                )
                .into());
            }
            let length = length.ok_or_else(|| {
                InputError::session_protocol("Content-Length required without Content-Range")
            })?;
            Extent {
                start: 0,
                length,
                total: Some(length),
                kind: ChunkKind::Data,
            }
        }
    };
    if start > session.offset {
        return Err(InputError::session_protocol("Upload offset leaves a gap").into());
    }
    let end = start
        .checked_add(length)
        .ok_or_else(|| InputError::session_protocol("Upload offset overflow"))?;
    if end > max_bytes || total.is_some_and(|n| n > max_bytes) {
        return Err(ServiceError::from(ObjectError::TooLarge).into());
    }
    if total.is_some_and(|n| session.total.is_some_and(|t| n != t) || n < session.offset)
        || session.total.is_some_and(|n| end > n)
    {
        return Err(match (total, session.total) {
            // Only an open total reaches here with a known session total: the chunk overruns it.
            (None, Some(declared))
                if *session.state == SessionState::Active && uri.range_refusal_fails_session() =>
            {
                Refusal::Overrun(Overrun {
                    received: length,
                    room: declared.saturating_sub(start),
                })
            }
            _ => InputError::session_protocol("Inconsistent upload total").into(),
        });
    }
    let total = total.or(session.total);
    let finalize = match kind {
        ChunkKind::Status => matches!(range, Some(Range::Status(Some(n))) if n == session.offset),
        ChunkKind::Data => total == Some(end),
        // The body's EOF ends the object.
        ChunkKind::OpenData => true,
    };
    if kind == ChunkKind::Data && !finalize && !length.is_multiple_of(256 * 1024) {
        let message = match uri {
            SessionUri::Json if length < 256 * 1024 => format!(
                "Invalid request.  The number of bytes uploaded is required to be equal or greater than 262144, except for the final request (it's recommended to be the exact multiple of 262144).  The received request contained {length} bytes, which does not meet this requirement."
            ),
            SessionUri::Json | SessionUri::Xml { .. } => {
                "Nonfinal chunk must be a multiple of 256 KiB".into()
            }
        };
        return Err(InputError::session_protocol(message).into());
    }
    Ok(ChunkPlan {
        start,
        length,
        total,
        kind,
        finalize,
    })
}

/// The session a request addresses through its session URI.
pub(crate) struct SessionTarget<'a> {
    pub(crate) bucket: &'a str,
    pub(crate) id: &'a str,
    pub(crate) uri: SessionUri<'a>,
}

/// Which API's session URI a request uses. The engine applies each API's recorded session rules
/// by this, not by the session, which either initiation may have opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionUri<'a> {
    /// `/upload/storage/v1/b/BUCKET/o?upload_id=`, which names only the bucket.
    Json,
    /// `/BUCKET/OBJECT?upload_id=`, which names the object as well.
    Xml { name: &'a str },
}

impl SessionUri<'_> {
    /// Whether the URI names an object other than the session's, which makes the session
    /// unknown at this URI.
    pub(crate) fn names_other_object(self, session_name: &str) -> bool {
        match self {
            Self::Json => false,
            Self::Xml { name } => name != session_name,
        }
    }

    /// Whether a refused `Content-Range` that breaks the syntax or overruns the total fails the
    /// whole session, not just this request. Cloud Storage failed the JSON session on a malformed
    /// range (recorded 2026-11-13) and on an overrun (recorded 2026-11-15, case
    /// obj-resumable-declared-length); no recording shows it for the XML session URI.
    fn range_refusal_fails_session(self) -> bool {
        match self {
            Self::Json => true,
            Self::Xml { .. } => false,
        }
    }
}

/// The result of one chunk or status request against a session.
pub(crate) enum Transfer {
    /// The protocol-neutral 308 response with the persisted range.
    Incomplete(Response<ResponseBody>),
    /// The answer made from the JSON object resource the completed upload stores, which
    /// retries replay.
    Complete(Value),
}

async fn session_framing(
    headers: &hyper::HeaderMap,
    target: &SessionTarget<'_>,
    ctx: ObjectCtx<'_>,
) -> Result<(Option<Range>, Option<u64>), ServiceError> {
    let framing = chunk_framing(headers, ctx.config.max_object_bytes());
    if target.uri.range_refusal_fails_session()
        && framing.is_err()
        && session_headers::range_fails_session(headers)
    {
        let lease = ctx
            .await_session_writer(
                ctx.state
                    .catalog
                    .lock_resumable_session(target.id, target.bucket),
            )
            .await?;
        if lease.session.state == SessionState::Active {
            ctx.await_catalog(ctx.state.catalog.reject_resumable_content_range(&lease))
                .await?;
        }
    }
    framing
}

/// Writes or queries one session: the shared engine behind every session URI.
pub(crate) async fn transfer(
    request: Request<Incoming>,
    target: SessionTarget<'_>,
    ctx: ObjectCtx<'_>,
) -> Result<Transfer, ServiceError> {
    let ObjectCtx { state, config, .. } = ctx;
    let SessionTarget { bucket, id, uri } = target;
    let headers = request.headers().clone();
    let (range, length) = session_framing(&headers, &target, ctx).await?;
    let mut lease = ctx
        .await_session_writer(state.catalog.lock_resumable_session(id, bucket))
        .await?;
    if uri.names_other_object(&lease.session.name) {
        return Err(ResumableError::NotFound.into());
    }
    let session = &lease.session;
    // Chunks may repeat the initiation key (as SDKs do) but never introduce or replace one;
    // a mismatch rejects only this request and leaves the session usable.
    if let Some(key) = crate::csek::request_key(&headers)? {
        crate::csek::check_data(session.options.metadata.customer_key_sha256, Some(key))?;
    }
    let position = SessionPosition {
        offset: session.offset,
        total: session.total,
        state: &session.state,
    };
    let plan = match plan_chunk(
        &headers,
        range,
        length,
        &position,
        config.max_object_bytes(),
        uri,
    ) {
        Ok(plan) => plan,
        Err(refusal) => return Err(refuse(refusal, &lease, ctx).await),
    };
    let status_only = plan.kind == ChunkKind::Status && !plan.finalize;
    // Validate the override before any bytes can be acknowledged.
    single(&headers, "x-guploader-no-308")?;
    // PHP repeats whole-object hashes on every data chunk; checkpoint them for completion.
    let options = match session_headers::final_options(&headers, session.options.clone()) {
        Ok(options) => options,
        Err(error) => {
            if plan.finalize && !session.state.is_complete() {
                state.catalog.invalidate_resumable_session(&lease).await?;
            }
            return Err(error);
        }
    };
    if !plan.finalize
        && headers
            .keys()
            .any(|k| k.as_str().starts_with(CUSTOM_METADATA_PREFIX))
    {
        return Err(InputError::parameter("Final metadata requires a final request").into());
    }
    if (session.state.is_complete() || status_only) && options.expected != session.options.expected
    {
        return Err(InputError::parameter(
            "Status or completed retries cannot introduce new checksum declarations",
        )
        .into());
    }
    if let SessionState::Complete(completion) = &session.state {
        let view = session.options.view()?;
        // Completing a session stores its resource in the same transaction.
        let completion = completion
            .clone()
            .into_value()
            .ok_or_else(|| object_unavailable(BackendFailure::MissingCompletion))?;
        drop(lease);
        return view
            .respond(ctx, completion, Links::Stored)
            .await
            .map(Transfer::Complete);
    }
    if status_only {
        crate::bounded_body::read_body(request.into_body(), 0).await?;
        let progress = session.progress()?;
        state.catalog.prepare_resumable_status(&lease).await?;
        if plan.total != session.total {
            ctx.await_catalog(state.catalog.checkpoint_resumable_session(
                &mut lease,
                progress.offset,
                plan.total,
            ))
            .await?;
        }
        return incomplete(progress, &headers).map(Transfer::Incomplete);
    }
    // The session lease holds an activity lease, so no exclusive job can run here.
    let permit = state.catalog.admit_object_upload()?;
    let view = session.options.view()?;
    let render = completion_renderer(&session.options, ctx.endpoints.api_base_url().to_owned());
    let (catalog_wait, progress) = watch::channel(ChunkWait::Transfer);
    let chunk = ResumableChunk {
        start: plan.start,
        length: plan.length,
        end: plan.body_end(),
        options,
        catalog_wait,
    };
    let (sender, body) = upload_channel();
    let upload = state
        .catalog
        .write_resumable_session(lease, chunk, body, permit, render);
    match stream_chunk(request.into_body(), ctx, upload, sender, progress).await? {
        Written::Complete(resource) => view
            .respond(ctx, resource, Links::ThisRequest)
            .await
            .map(Transfer::Complete),
        Written::Open(progress) => incomplete(progress, &headers).map(Transfer::Incomplete),
    }
}

/// The answer to a chunk that cannot be planned. An overrun first fails the session, so that
/// every later request on it repeats the refusal.
async fn refuse(refusal: Refusal, lease: &SessionLease, ctx: ObjectCtx<'_>) -> ServiceError {
    match refusal {
        Refusal::Request(error) => error,
        Refusal::Overrun(overrun) => {
            let failed = ctx
                .await_catalog(ctx.state.catalog.reject_resumable_overrun(lease, overrun))
                .await;
            match failed {
                Ok(()) => ResumableError::Overrun(overrun).into(),
                Err(error) => error.into(),
            }
        }
    }
}

/// Renders the whole resource the session stores for its completion. The uploader supplied any
/// customer key at initiation, so the resource is rendered with that key.
fn completion_renderer(
    options: &ResumableOptions,
    base: String,
) -> impl FnOnce(&ObjectRecord) -> Result<Value, ObjectError> + Send + 'static {
    let projection = options.projection;
    let key = options
        .metadata
        .customer_key_sha256
        .map(CustomerKey::from_sha256);
    move |record: &ObjectRecord| {
        record
            .to_stored_completion(&base, projection, key)
            .map_err(ObjectError::backend)
    }
}

/// Streams the chunk body into the session write and returns where the write ended: the
/// completed resource, or the progress of a session that stays open.
async fn stream_chunk(
    body: Incoming,
    ctx: ObjectCtx<'_>,
    upload: impl std::future::Future<Output = Result<Written, ResumableError>>,
    sender: UploadSender,
    progress: watch::Receiver<ChunkWait>,
) -> Result<Written, ResumableError> {
    let producer = async {
        stream_upload(body, &sender, ctx.config.max_object_bytes(), None)
            .await
            .map(|_| UploadEnd::Eof)
    };
    drive_upload(
        CatalogWait::Session { ctx, progress },
        upload,
        &sender,
        producer,
    )
    .await
    .map_err(DriveError::into_error::<ResumableError>)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bucket::BucketAccess;

    /// `resumable_sessions.completion` holds the rendered resource as JSON text, and a retry
    /// after an upgrade reads it back, so the text an earlier release wrote is fixed. The JSON
    /// adapter writes it for XML uploads as well; XML renders its own response from the record.
    #[test]
    fn json_completion_keeps_the_text_stored_in_sqlite() {
        use crate::storage::test_fixtures::{bare_object_record, populated_object_record};
        let base = "http://localhost:4443/".to_owned();
        let mut keyed = ResumableOptions::new(BucketAccess::Permissive);
        keyed.metadata.customer_key_sha256 = Some([2; 32]);
        for (options, record, expected) in [
            (
                ResumableOptions::new(BucketAccess::Permissive),
                bare_object_record(),
                BARE_RESOURCE,
            ),
            (keyed, populated_object_record(), POPULATED_RESOURCE),
        ] {
            let stored = completion_renderer(&options, base.clone())(&record)
                .unwrap()
                .to_string();
            assert_eq!(stored, expected);
            // The catalog reads the column back with `serde_json::from_str`.
            let reread: serde_json::Value = serde_json::from_str(expected).unwrap();
            assert_eq!(reread.to_string(), expected);
        }
    }

    const BARE_RESOURCE: &str = r#"{"kind":"storage#object","id":"bucket/bare.txt/1700000000123456","selfLink":"http://localhost:4443/storage/v1/b/bucket/o/bare.txt","mediaLink":"http://localhost:4443/download/storage/v1/b/bucket/o/bare.txt?generation=1700000000123456&alt=media","name":"bare.txt","bucket":"bucket","generation":"1700000000123456","metageneration":"1","contentType":"application/octet-stream","storageClass":"STANDARD","size":"3","md5Hash":"AQEBAQEBAQEBAQEBAQEBAQ==","crc32c":"AAAABw==","etag":"CMDEgMHBxIIDEAE=","timeCreated":"2023-11-14T22:13:20.123Z","updated":"2023-11-14T22:13:20.123Z","timeStorageClassUpdated":"2023-11-14T22:13:20.123Z","timeFinalized":"2023-11-14T22:13:20.123Z"}"#;
    const POPULATED_RESOURCE: &str = r#"{"kind":"storage#object","id":"bucket/folder/full object.txt/1700000000123456","selfLink":"http://localhost:4443/storage/v1/b/bucket/o/folder%2Ffull%20object.txt","mediaLink":"http://localhost:4443/download/storage/v1/b/bucket/o/folder%2Ffull%20object.txt?generation=1700000000123456&alt=media","name":"folder/full object.txt","bucket":"bucket","generation":"1700000000123456","metageneration":"3","contentType":"text/plain","storageClass":"NEARLINE","size":"11","contentEncoding":"gzip","etag":"CMDEgMHBxIIDEAM=","timeCreated":"2023-11-14T22:13:20.123Z","updated":"2023-11-14T22:16:40.000Z","timeStorageClassUpdated":"2023-11-14T22:15:00.000Z","timeFinalized":"2023-11-14T22:13:20.123Z","componentCount":2,"contentDisposition":"inline","contentLanguage":"en","cacheControl":"private","metadata":{"alpha":"one","beta":"two"},"eventBasedHold":true,"temporaryHold":false,"retentionExpirationTime":"2030-03-17T17:46:40.123Z","retention":{"mode":"Locked","retainUntilTime":"2030-03-17T17:46:40.123Z"},"customTime":"2026-01-02T03:04:05.123456789Z","timeDeleted":"2023-11-14T22:18:20.000Z","softDeleteTime":"2023-11-14T22:18:20.000Z","hardDeleteTime":"2023-11-25T08:18:20.000Z","customerEncryption":{"encryptionAlgorithm":"AES256","keySha256":"AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI="},"crc32c":"AAAAew=="}"#;
}
