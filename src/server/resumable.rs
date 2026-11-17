use super::RequestBody as Incoming;
use super::upload_drive::{CatalogWait, UploadEnd, UploadSender, drive_upload, upload_channel};
use super::{
    ObjectActivity, ObjectCtx, ResponseBody, boxed, empty_response, json_response, stream_upload,
};
use crate::{
    bucket::BucketAccess,
    json::{
        self, JsonError, ObjectInsertParams,
        resumable::{self, Range},
        resumable_cancel::Cancellation,
    },
    object::{ObjectError, ObjectRecord},
    storage::{
        ChunkEnd, ChunkWait, Overrun, Progress, ResumableChunk, ResumableError, ResumableOptions,
        SessionLease, SessionState, Written,
        failure::{BackendFailure, object_unavailable},
    },
    time_limit::TimeLimit,
};
use hyper::{Request, Response, StatusCode, header::HeaderValue};
use tokio::sync::watch;

pub(super) async fn initiate(
    request: Request<Incoming>,
    bucket: &str,
    name: Option<String>,
    params: ObjectInsertParams,
    encoding: Option<String>,
    access: BucketAccess,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, JsonError> {
    let ObjectCtx { config, .. } = ctx;
    let response_fields =
        json::parse_query(request.uri().query().unwrap_or_default())?.remove("fields");
    let resumable::UploadHeaders {
        mime,
        expected,
        total,
    } = resumable::headers(request.headers(), config.max_object_bytes)?;
    let key = json::request_key(request.headers())?;
    let origin = resumable::single(request.headers(), "origin")?.map(str::to_owned);
    let metadata_type = resumable::single(request.headers(), "content-type")?.map(str::to_owned);
    let json_type = metadata_type.as_ref().is_some_and(|v| {
        v.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("application/json")
    });
    if super::has_body(request.headers()) && !json_type {
        return Err(JsonError::invalid_parameter(
            "Metadata requires application/json",
        ));
    }
    let base = ctx.endpoints.api_base_url();
    let location_base = format!("{base}/upload/storage/v1/b/{bucket}/o");
    let query = request.uri().query().unwrap_or_default().to_owned();
    location_base
        .parse::<hyper::header::HeaderValue>()
        .map_err(|_| JsonError::invalid_parameter("Invalid session endpoint"))?;
    let bytes = json::read_body(request.into_body(), json::RESOURCE_BODY_LIMIT).await?;
    if !bytes.is_empty() && !json_type {
        return Err(JsonError::invalid_parameter(
            "Metadata requires application/json",
        ));
    }
    let initiation = resumable::Initiation {
        name,
        mime,
        expected,
        params,
        access,
        origin,
        encoding,
    };
    let (name, mut options) = resumable::options(&bytes, initiation)?;
    options.response_fields = response_fields;
    // The session binds the key fingerprint; later chunks cannot change it.
    options.metadata.customer_key_sha256 = key.map(crate::csek::CustomerKey::sha256);
    let id = create_session(ctx, bucket, &name, options, total).await?;
    let location = session_location(&location_base, &query, id);
    let mut response = empty_response(StatusCode::OK);
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response.headers_mut().insert(
        "location",
        location
            .parse()
            .map_err(|_| JsonError::invalid_parameter("Invalid session endpoint"))?,
    );
    // Cloud Storage forbids every cache for the initiation reply, as for other JSON API calls.
    json::forbid_caching(response.headers_mut());
    Ok(boxed(response))
}

/// The session URI Cloud Storage returned: the initiation's own query parameters plus the
/// `upload_id`, sorted by name (recorded for `name`, `ifGenerationMatch` and `uploadType`).
fn session_location(base: &str, query: &str, id: crate::storage::SessionId) -> String {
    let mut pairs: Vec<&str> = query
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("upload_id="))
        .collect();
    let upload_id = format!("upload_id={id}");
    pairs.push(&upload_id);
    pairs.sort_by(|a, b| a.split('=').next().cmp(&b.split('=').next()));
    format!("{base}?{}", pairs.join("&"))
}

/// Opens a session for an authorized initiation and returns its opaque ID. Each protocol
/// adapter parses its own initiation request and builds its own session URI.
pub(crate) async fn create_session(
    ctx: ObjectCtx<'_>,
    bucket: &str,
    name: &str,
    options: ResumableOptions,
    total: Option<u64>,
) -> Result<crate::storage::SessionId, JsonError> {
    let state = ctx.state;
    super::retention_access::authorize_upload(
        &state.catalog,
        &options.access,
        bucket,
        &options.metadata,
    )
    .await?;
    let session = ctx
        .await_catalog(
            state
                .catalog
                .create_resumable_session(bucket, name, options, total),
        )
        .await?;
    Ok(session.id)
}

pub(super) fn incomplete(
    progress: Progress,
    headers: &hyper::HeaderMap,
) -> Result<Response<ResponseBody>, JsonError> {
    let override_status = resumable::single(headers, "x-guploader-no-308")? == Some("yes");
    let mut response = empty_response(if override_status {
        StatusCode::OK
    } else {
        StatusCode::PERMANENT_REDIRECT
    });
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/plain; charset=utf-8"),
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
struct SessionPosition {
    offset: u64,
    total: Option<u64>,
    state: SessionState,
}

/// Validates a chunk's framing headers against the object size limit before the session is
/// locked, and returns its Content-Range and Content-Length.
fn chunk_framing(
    headers: &hyper::HeaderMap,
    max_bytes: u64,
) -> Result<(Option<Range>, Option<u64>), JsonError> {
    let range = resumable::range(headers)?;
    let length = resumable::single(headers, "content-length")?
        .map(resumable::unsigned)
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
        return Err(JsonError::session_protocol(
            "Content-Length does not match Content-Range",
        ));
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
) -> Result<Extent, JsonError> {
    if total.is_some_and(|n| session_total.is_some_and(|t| n != t)) {
        return Err(JsonError::session_protocol("Inconsistent upload total"));
    }
    match (total.or(session_total), length) {
        (Some(total), length) => {
            let remaining = total
                .checked_sub(start)
                .ok_or_else(|| JsonError::session_protocol("Inconsistent upload total"))?;
            if length.is_some_and(|n| n != remaining) {
                return Err(JsonError::session_protocol(
                    "Content-Length does not match Content-Range",
                ));
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
                .ok_or_else(|| JsonError::session_protocol("Upload offset overflow"))?;
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
    Request(JsonError),
    /// A data chunk with an open total that runs past the session's total. Cloud Storage
    /// failed the JSON session on it (recorded 2026-11-15, case obj-resumable-declared-length).
    Overrun(Overrun),
}

impl From<JsonError> for Refusal {
    fn from(error: JsonError) -> Self {
        Self::Request(error)
    }
}

fn plan_chunk(
    headers: &hyper::HeaderMap,
    range: Option<Range>,
    length: Option<u64>,
    session: &SessionPosition,
    max_bytes: u64,
    target: &SessionTarget<'_>,
) -> Result<ChunkPlan, Refusal> {
    let Extent {
        start,
        length,
        total,
        kind,
    } = match range {
        Some(Range::Status(total)) => {
            if length != Some(0) || headers.contains_key("transfer-encoding") {
                return Err(JsonError::session_protocol("Status requires an empty body").into());
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
            if session.offset != 0 && session.state != SessionState::Complete {
                return Err(JsonError::session_protocol(
                    "Content-Range required after the first chunk",
                )
                .into());
            }
            let length = length.ok_or_else(|| {
                JsonError::session_protocol("Content-Length required without Content-Range")
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
        return Err(JsonError::session_protocol("Upload offset leaves a gap").into());
    }
    let end = start
        .checked_add(length)
        .ok_or_else(|| JsonError::session_protocol("Upload offset overflow"))?;
    if end > max_bytes || total.is_some_and(|n| n > max_bytes) {
        return Err(JsonError::from(ObjectError::TooLarge).into());
    }
    if total.is_some_and(|n| session.total.is_some_and(|t| n != t) || n < session.offset)
        || session.total.is_some_and(|n| end > n)
    {
        return Err(match (total, session.total) {
            // Only an open total reaches here with a known session total: the chunk overruns it.
            (None, Some(declared))
                if session.state == SessionState::Active && target.name.is_none() =>
            {
                Refusal::Overrun(Overrun {
                    received: length,
                    room: declared.saturating_sub(start),
                })
            }
            _ => JsonError::session_protocol("Inconsistent upload total").into(),
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
        let message = if target.name.is_some() || length >= 256 * 1024 {
            "Nonfinal chunk must be a multiple of 256 KiB".into()
        } else {
            format!(
                "Invalid request.  The number of bytes uploaded is required to be equal or greater than 262144, except for the final request (it's recommended to be the exact multiple of 262144).  The received request contained {length} bytes, which does not meet this requirement."
            )
        };
        return Err(JsonError::session_protocol(message).into());
    }
    Ok(ChunkPlan {
        start,
        length,
        total,
        kind,
        finalize,
    })
}

pub(super) async fn session_response(
    request: Request<Incoming>,
    bucket: &str,
    id: &str,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, JsonError> {
    if request.method() == hyper::Method::DELETE {
        let cancellation = ctx
            .await_session_writer(json::resumable_cancel::cancel_response(
                request,
                bucket,
                id,
                &ctx.state.catalog,
            ))
            .await?;
        return Ok(match cancellation {
            Cancellation::Cancelled => {
                boxed(JsonError::from(ResumableError::Cancelled).into_response())
            }
            Cancellation::Completed {
                mut resource,
                options,
            } => {
                super::context::rebase_stored_links(ctx.endpoints, &mut resource);
                let resource = CompletionView::of(&options)?.respond(ctx, resource).await?;
                boxed(json_response(&resource))
            }
        });
    }
    let target = SessionTarget {
        bucket,
        id,
        name: None,
    };
    Ok(match transfer(request, target, ctx).await? {
        // The shared 308 carries `no-store` for the XML adapter; the JSON front end sends the
        // status reply with no cache field (recorded for an empty status request).
        Transfer::Incomplete(mut response) => {
            response.headers_mut().remove("cache-control");
            response
        }
        // The cache fields of a stored-object response come from the JSON API envelope.
        Transfer::Complete(value) => boxed(json_response(&value)),
    })
}

/// The session a request addresses through its session URI.
pub(crate) struct SessionTarget<'a> {
    pub(crate) bucket: &'a str,
    pub(crate) id: &'a str,
    /// The object an XML session URI names; the JSON URI names only the bucket.
    pub(crate) name: Option<&'a str>,
}

/// The result of one chunk or status request against a session.
pub(crate) enum Transfer {
    /// The protocol-neutral 308 response with the persisted range.
    Incomplete(Response<ResponseBody>),
    /// The stored JSON object resource of the completed upload, which retries replay.
    Complete(serde_json::Value),
}

async fn session_framing(
    headers: &hyper::HeaderMap,
    target: &SessionTarget<'_>,
    ctx: ObjectCtx<'_>,
) -> Result<(Option<Range>, Option<u64>), JsonError> {
    let framing = chunk_framing(headers, ctx.config.max_object_bytes);
    // The recordings prove a terminal JSON session failure for malformed syntax and for a
    // chunk that overruns its declared total.
    if target.name.is_none() && framing.is_err() && resumable::range_fails_session(headers) {
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
) -> Result<Transfer, JsonError> {
    let ObjectCtx { state, config, .. } = ctx;
    let SessionTarget { bucket, id, name } = target;
    let headers = request.headers().clone();
    let (range, length) = session_framing(&headers, &target, ctx).await?;
    let mut lease = ctx
        .await_session_writer(state.catalog.lock_resumable_session(id, bucket))
        .await?;
    if name.is_some_and(|name| name != lease.session.name) {
        return Err(ResumableError::NotFound.into());
    }
    let session = &lease.session;
    // Chunks may repeat the initiation key (as SDKs do) but never introduce or replace one;
    // a mismatch rejects only this request and leaves the session usable.
    if let Some(key) = json::request_key(&headers)? {
        crate::csek::check_data(session.options.metadata.customer_key_sha256, Some(key))?;
    }
    let position = SessionPosition {
        offset: session.offset,
        total: session.total,
        state: session.state,
    };
    let plan = match plan_chunk(
        &headers,
        range,
        length,
        &position,
        config.max_object_bytes,
        &target,
    ) {
        Ok(plan) => plan,
        Err(refusal) => return Err(refuse(refusal, &lease, ctx).await),
    };
    let status_only = plan.kind == ChunkKind::Status && !plan.finalize;
    // Validate the override before any bytes can be acknowledged.
    resumable::single(&headers, "x-guploader-no-308")?;
    // PHP repeats whole-object hashes on every data chunk; checkpoint them for completion.
    let options = match resumable::final_options(&headers, session.options.clone()) {
        Ok(options) => options,
        Err(error) => {
            if plan.finalize && session.state != SessionState::Complete {
                state.catalog.invalidate_resumable_session(&lease).await?;
            }
            return Err(error);
        }
    };
    if !plan.finalize
        && headers
            .keys()
            .any(|k| k.as_str().starts_with("x-goog-meta-"))
    {
        return Err(JsonError::invalid_parameter(
            "Final metadata requires a final request",
        ));
    }
    if (session.state == SessionState::Complete || status_only)
        && options.expected != session.options.expected
    {
        return Err(JsonError::invalid_parameter(
            "Status or completed retries cannot introduce new checksum declarations",
        ));
    }
    if session.state == SessionState::Complete {
        let view = CompletionView::of(&session.options)?;
        // Completing a session stores its resource in the same transaction.
        let mut completion = session
            .completion
            .clone()
            .ok_or_else(|| object_unavailable(BackendFailure::MissingCompletion))?;
        drop(lease);
        super::context::rebase_stored_links(ctx.endpoints, &mut completion);
        return view.respond(ctx, completion).await.map(Transfer::Complete);
    }
    if status_only {
        json::read_body(request.into_body(), 0).await?;
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
    let view = CompletionView::of(&session.options)?;
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
    let upload = async { upload.await.map_err(JsonError::from) };
    match stream_chunk(request.into_body(), ctx, upload, sender, progress).await? {
        Written::Complete(resource) => view.respond(ctx, resource).await.map(Transfer::Complete),
        Written::Open(progress) => incomplete(progress, &headers).map(Transfer::Incomplete),
    }
}

/// The answer to a chunk that cannot be planned. An overrun first fails the session, so that
/// every later request on it repeats the refusal.
async fn refuse(refusal: Refusal, lease: &SessionLease, ctx: ObjectCtx<'_>) -> JsonError {
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
) -> impl FnOnce(&ObjectRecord) -> Result<serde_json::Value, ObjectError> + Send + 'static {
    let projection = options.projection;
    let key = options
        .metadata
        .customer_key_sha256
        .map(crate::csek::CustomerKey::from_sha256);
    move |record: &ObjectRecord| {
        json::object_resource_keyed(record, &base, projection, key).map_err(ObjectError::backend)
    }
}

/// How each response of a completed session is made from the resource it stores. Cloud Storage
/// answered the completing request and a later status request with the full projection's `acl`
/// and `owner` when the initiation asked for it (recorded 2026-11-17, case
/// obj-full-projection-writes), so those fields are added to each response as the object has
/// them then, before the initiation's field selection. A refused cancellation answers the same
/// way by inference.
struct CompletionView {
    /// With `projection=full`, the initiator's stored access, which decides whether an answer
    /// may carry the ACL fields: requests on the session URI have no caller of their own.
    acl_reader: Option<BucketAccess>,
    fields: Option<json::Fields>,
}

impl CompletionView {
    /// Parses the initiation's field selection, so a bad one fails before any byte is stored.
    fn of(options: &ResumableOptions) -> Result<Self, JsonError> {
        Ok(Self {
            acl_reader: (options.projection == json::Projection::Full)
                .then(|| options.access.clone()),
            fields: options
                .response_fields
                .as_deref()
                .map(json::Fields::parse_syntax)
                .transpose()?,
        })
    }

    async fn respond(
        &self,
        ctx: ObjectCtx<'_>,
        mut resource: serde_json::Value,
    ) -> Result<serde_json::Value, JsonError> {
        if let Some(initiator) = &self.acl_reader {
            super::acl::add_full_projection_to_stored(
                ctx.state,
                super::acl::AclReader::Initiator(initiator),
                &mut resource,
                ctx.endpoints.api_base_url(),
            )
            .await?;
        }
        if let Some(fields) = &self.fields {
            fields.apply(&mut resource);
        }
        Ok(resource)
    }
}

/// Streams the chunk body into the session write and returns its completion response, or
/// `None` while the upload stays incomplete.
async fn stream_chunk(
    body: Incoming,
    ctx: ObjectCtx<'_>,
    upload: impl std::future::Future<Output = Result<Written, JsonError>>,
    sender: UploadSender,
    progress: watch::Receiver<ChunkWait>,
) -> Result<Written, JsonError> {
    let producer = async {
        stream_upload(body, &sender, ctx.config.max_object_bytes, None)
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
    .map_err(|error| error.into_error::<JsonError>())
}

pub(super) async fn await_session_upload<T, E: From<ObjectError>>(
    operation: impl std::future::Future<Output = Result<T, E>>,
    mut progress: watch::Receiver<ChunkWait>,
    timeout: TimeLimit,
    deadline: tokio::time::Instant,
    activity: &watch::Sender<ObjectActivity>,
) -> Result<T, E> {
    let _wait = super::begin_catalog_wait(activity).await;
    tokio::pin!(operation);
    loop {
        let waiting = *progress.borrow_and_update() == ChunkWait::Catalog;
        let wait_deadline = if waiting {
            deadline.min(timeout.deadline())
        } else {
            deadline
        };
        tokio::select! {
            biased;
            result = &mut operation => break result,
            Ok(()) = progress.changed() => {},
            _ = tokio::time::sleep_until(wait_deadline) => {
                let failure = if waiting { BackendFailure::UploadWaitTimeout } else { BackendFailure::FinalizationTimeout };
                break Err(object_unavailable(failure).into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::{sync::oneshot, task::JoinHandle, time::advance};

    const BUDGET: Duration = Duration::from_millis(200);
    const REQUEST: Duration = Duration::from_secs(10);

    // Returns once the task is waiting, so its deadlines start before any advance.
    async fn wait(
        waiting: ChunkWait,
    ) -> (
        JoinHandle<Result<(), JsonError>>,
        watch::Sender<ChunkWait>,
        oneshot::Sender<()>,
    ) {
        let (progress, updates) = watch::channel(waiting);
        let (finish, finished) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + REQUEST;
        let task = tokio::spawn(async move {
            let (activity, _updates) = watch::channel(ObjectActivity::default());
            let operation = async {
                finished
                    .await
                    .map_err(|_| JsonError::from(ObjectError::Unavailable("dropped".into())))
            };
            let budget = TimeLimit::from_millis(BUDGET.as_millis() as u64);
            await_session_upload(operation, updates, budget, deadline, &activity).await
        });
        settle().await;
        (task, progress, finish)
    }

    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    // Checks completion before awaiting: an idle paused runtime would otherwise
    // auto-advance to whichever later deadline is pending.
    async fn timed_out(task: JoinHandle<Result<(), JsonError>>) -> crate::json::ErrorKind {
        settle().await;
        assert!(task.is_finished());
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        error.kind
    }

    #[tokio::test(start_paused = true)]
    async fn work_outside_catalog_waits_uses_the_request_deadline() {
        let (task, _progress, finish) = wait(ChunkWait::Transfer).await;
        advance(BUDGET * 10).await;
        settle().await;
        assert!(!task.is_finished());
        finish.send(()).unwrap();
        task.await.unwrap().unwrap();

        let (task, _progress, _finish) = wait(ChunkWait::Transfer).await;
        advance(REQUEST - Duration::from_millis(1)).await;
        settle().await;
        assert!(!task.is_finished());
        advance(Duration::from_millis(1)).await;
        assert!(matches!(
            timed_out(task).await,
            crate::json::ErrorKind::Backend(BackendFailure::FinalizationTimeout)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn catalog_wait_times_out_after_its_budget() {
        let (task, _progress, _finish) = wait(ChunkWait::Catalog).await;
        advance(BUDGET - Duration::from_millis(1)).await;
        settle().await;
        assert!(!task.is_finished());
        advance(Duration::from_millis(1)).await;
        assert!(matches!(
            timed_out(task).await,
            crate::json::ErrorKind::Backend(BackendFailure::UploadWaitTimeout)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn each_catalog_wait_starts_a_new_budget() {
        let (task, progress, _finish) = wait(ChunkWait::Catalog).await;
        advance(BUDGET - Duration::from_millis(1)).await;
        progress.send_replace(ChunkWait::Transfer);
        settle().await;
        advance(BUDGET * 5).await;
        progress.send_replace(ChunkWait::Catalog);
        settle().await;
        advance(BUDGET - Duration::from_millis(1)).await;
        settle().await;
        assert!(!task.is_finished());
        advance(Duration::from_millis(1)).await;
        assert!(matches!(
            timed_out(task).await,
            crate::json::ErrorKind::Backend(BackendFailure::UploadWaitTimeout)
        ));
    }
}
