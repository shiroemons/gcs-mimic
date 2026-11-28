use super::RequestBody as Incoming;
use super::upload_drive::{CatalogWait, UploadEnd, UploadSender, drive_upload, upload_channel};
use super::{
    ObjectActivity, ObjectCtx, ResponseBody, boxed, empty_response, json_response, stream_upload,
};
use crate::json::JsonErrorResponse;
use crate::object::ObjectKey;
use crate::{
    bucket::{BucketAccess, name::BucketKey},
    cause::BackendFailure,
    input::InputError,
    json::ApiError,
    json::{
        self, ObjectInsertParams,
        resumable::{self, Range},
        resumable_cancel::Cancellation,
    },
    object::{ObjectError, ObjectRecord, media_headers::CUSTOM_METADATA_PREFIX},
    service::{
        ServiceError,
        activity::{CatalogWaitLimit, begin_catalog_wait},
    },
    storage::{
        ChunkEnd, ChunkWait, Overrun, Progress, ResumableChunk, ResumableError, ResumableOptions,
        SessionLease, SessionState, Written, failure::object_unavailable,
    },
    time_limit::TimeLimit,
};
use hyper::{Request, Response, StatusCode, header::HeaderValue};
use tokio::sync::watch;

pub(super) async fn initiate(
    request: Request<Incoming>,
    bucket: &BucketKey,
    name: Option<String>,
    params: ObjectInsertParams,
    encoding: Option<String>,
    access: BucketAccess,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, ApiError> {
    let ObjectCtx { config, .. } = ctx;
    let response_fields = ctx.uri.parameters()?.get("fields").cloned();
    let resumable::UploadHeaders { mime, total } =
        resumable::headers(request.headers(), config.max_object_bytes())?;
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
        return Err(ApiError::invalid_parameter(
            "Metadata requires application/json",
        ));
    }
    let base = ctx.endpoints.api_base_url();
    let prefix = super::routing::session_path_prefix(&request);
    let location_base = format!("{base}{prefix}{bucket}/o");
    let query = request.uri().query().unwrap_or_default().to_owned();
    location_base
        .parse::<hyper::header::HeaderValue>()
        .map_err(|_| ApiError::invalid_parameter("Invalid session endpoint"))?;
    let bytes = crate::bounded_body::read_body(
        request.into_body(),
        crate::bounded_body::RESOURCE_BODY_LIMIT,
    )
    .await?;
    if !bytes.is_empty() && !json_type {
        return Err(ApiError::invalid_parameter(
            "Metadata requires application/json",
        ));
    }
    let initiation = resumable::Initiation {
        name,
        mime,
        params,
        access,
        origin,
        encoding,
    };
    let (name, mut options) = resumable::options(&bytes, initiation)?;
    options.response_fields = response_fields;
    // The session binds the key fingerprint; later chunks cannot change it.
    options.metadata.customer_key_sha256 = key.map(crate::csek::CustomerKey::sha256);
    let key = ObjectKey::from_bucket(bucket.as_key(), &name);
    let id = create_session(ctx, key, options, total).await?;
    let location = session_location(&location_base, &query, id);
    let mut response = empty_response(StatusCode::OK);
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static(crate::mime::TEXT_PLAIN_UTF8),
    );
    response.headers_mut().insert(
        "location",
        location
            .parse()
            .map_err(|_| ApiError::invalid_parameter("Invalid session endpoint"))?,
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
    key: ObjectKey<'_>,
    options: ResumableOptions,
    total: Option<u64>,
) -> Result<crate::storage::SessionId, ServiceError> {
    let state = ctx.state;
    crate::service::access::retention::authorize_upload(
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

pub(super) fn incomplete(
    progress: Progress,
    headers: &hyper::HeaderMap,
) -> Result<Response<ResponseBody>, ServiceError> {
    let override_status = resumable::single(headers, "x-guploader-no-308")? == Some("yes");
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
                if *session.state == SessionState::Active && target.name.is_none() =>
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
        let message = if target.name.is_some() || length >= 256 * 1024 {
            "Nonfinal chunk must be a multiple of 256 KiB".into()
        } else {
            format!(
                "Invalid request.  The number of bytes uploaded is required to be equal or greater than 262144, except for the final request (it's recommended to be the exact multiple of 262144).  The received request contained {length} bytes, which does not meet this requirement."
            )
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

pub(super) async fn session_response(
    request: Request<Incoming>,
    bucket: &str,
    id: &str,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, ApiError> {
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
                boxed(ApiError::from(ResumableError::Cancelled).into_response())
            }
            Cancellation::Completed {
                mut resource,
                options,
            } => {
                super::rebase_stored_links(ctx.endpoints, &mut resource);
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
) -> Result<(Option<Range>, Option<u64>), ServiceError> {
    let framing = chunk_framing(headers, ctx.config.max_object_bytes());
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
) -> Result<Transfer, ServiceError> {
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
        state: &session.state,
    };
    let plan = match plan_chunk(
        &headers,
        range,
        length,
        &position,
        config.max_object_bytes(),
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
        let view = CompletionView::of(&session.options)?;
        // Completing a session stores its resource in the same transaction.
        let mut completion = completion
            .clone()
            .into_value()
            .ok_or_else(|| object_unavailable(BackendFailure::MissingCompletion))?;
        drop(lease);
        super::rebase_stored_links(ctx.endpoints, &mut completion);
        return view.respond(ctx, completion).await.map(Transfer::Complete);
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
    match stream_chunk(request.into_body(), ctx, upload, sender, progress).await? {
        Written::Complete(resource) => view.respond(ctx, resource).await.map(Transfer::Complete),
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
    fn of(options: &ResumableOptions) -> Result<Self, ServiceError> {
        Ok(Self {
            acl_reader: (options.projection == crate::acl::Projection::Full)
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
    ) -> Result<serde_json::Value, ServiceError> {
        if let Some(initiator) = &self.acl_reader {
            super::acl::add_full_projection_to_stored(
                ctx.state,
                crate::service::access::acl::AclReader::Initiator(initiator),
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
    .map_err(crate::server::upload_drive::DriveError::into_error::<ResumableError>)
}

pub(super) async fn await_session_upload<T, E: From<ObjectError>>(
    operation: impl std::future::Future<Output = Result<T, E>>,
    mut progress: watch::Receiver<ChunkWait>,
    timeout: TimeLimit,
    deadline: tokio::time::Instant,
    activity: &watch::Sender<ObjectActivity>,
) -> Result<T, E> {
    let _wait = begin_catalog_wait(activity).await;
    tokio::pin!(operation);
    loop {
        // Only a catalog wait has a budget; a transfer runs until the request deadline.
        let budget = if *progress.borrow_and_update() == ChunkWait::Catalog {
            timeout
        } else {
            TimeLimit::DISABLED
        };
        let limit = CatalogWaitLimit::starting_now(budget, deadline);
        tokio::select! {
            biased;
            result = &mut operation => break result,
            Ok(()) = progress.changed() => {},
            _ = tokio::time::sleep_until(limit.deadline()) => {
                break Err(object_unavailable(limit.expired()).into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;
    use std::time::Duration;
    use tokio::{sync::oneshot, task::JoinHandle, time::advance};

    const BUDGET: Duration = Duration::from_millis(200);
    const REQUEST: Duration = Duration::from_secs(10);

    type Waiting = (
        JoinHandle<Result<(), ApiError>>,
        watch::Sender<ChunkWait>,
        oneshot::Sender<()>,
    );

    async fn wait(waiting: ChunkWait) -> Waiting {
        wait_within(waiting, REQUEST).await
    }

    // Returns once the task is waiting, so its deadlines start before any advance.
    async fn wait_within(waiting: ChunkWait, request: Duration) -> Waiting {
        let (progress, updates) = watch::channel(waiting);
        let (finish, finished) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + request;
        let task = tokio::spawn(async move {
            let (activity, _updates) = watch::channel(ObjectActivity::default());
            let operation = async {
                finished
                    .await
                    .map_err(|_| ApiError::from(ObjectError::Unavailable("dropped".into())))
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
    async fn timed_out(task: JoinHandle<Result<(), ApiError>>) -> BackendFailure {
        settle().await;
        assert!(task.is_finished());
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        error
            .backend_failure()
            .expect("expected a classified upload timeout")
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
        assert_matches!(timed_out(task).await, BackendFailure::FinalizationTimeout);
    }

    #[tokio::test(start_paused = true)]
    async fn catalog_wait_times_out_after_its_budget() {
        let (task, _progress, _finish) = wait(ChunkWait::Catalog).await;
        advance(BUDGET - Duration::from_millis(1)).await;
        settle().await;
        assert!(!task.is_finished());
        advance(Duration::from_millis(1)).await;
        assert_matches!(timed_out(task).await, BackendFailure::UploadWaitTimeout);
    }

    /// A larger catalog budget would not have helped, so the reply advises the request deadline.
    #[tokio::test(start_paused = true)]
    async fn a_catalog_wait_the_request_deadline_ends_reports_finalization() {
        let (task, _progress, _finish) = wait_within(ChunkWait::Catalog, BUDGET / 2).await;
        advance(BUDGET / 2 - Duration::from_millis(1)).await;
        settle().await;
        assert!(!task.is_finished());
        advance(Duration::from_millis(1)).await;
        assert_matches!(timed_out(task).await, BackendFailure::FinalizationTimeout);
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
        assert_matches!(timed_out(task).await, BackendFailure::UploadWaitTimeout);
    }

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
