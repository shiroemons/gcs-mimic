use super::RequestBody as Incoming;
use super::{ObjectActivity, ObjectCtx, ResponseBody, boxed, empty_response, stream_upload};
use crate::{
    bucket::BucketAccess,
    json::{
        self, JsonError, ObjectInsertParams,
        resumable::{self, Range},
    },
    object::{ObjectError, ObjectRecord, UploadChunk},
    storage::{
        ResumableChunk, ResumableOptions, SessionState,
        failure::{BackendFailure, object_unavailable},
    },
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, header::HeaderValue};
use tokio::sync::{mpsc, watch};

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
    let (mime, expected, total) =
        resumable::headers(request.headers(), params, config.max_object_bytes)?;
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
    let base = &ctx.endpoints.api_base_url;
    let location_base = format!("{base}/upload/storage/v1/b/{bucket}/o");
    let query = request.uri().query().unwrap_or_default().to_owned();
    location_base
        .parse::<hyper::header::HeaderValue>()
        .map_err(|_| JsonError::invalid_parameter("Invalid session endpoint"))?;
    let bytes = json::read_body(request.into_body(), 64 * 1024).await?;
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
    let location = session_location(&location_base, &query, &id);
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
fn session_location(base: &str, query: &str, id: &str) -> String {
    let mut pairs: Vec<String> = query
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("upload_id="))
        .map(str::to_owned)
        .collect();
    pairs.push(format!("upload_id={id}"));
    pairs.sort_by(|a, b| {
        let key = |pair: &str| pair.split('=').next().unwrap_or_default().to_owned();
        key(a).cmp(&key(b))
    });
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
) -> Result<String, JsonError> {
    let state = ctx.state;
    super::retention_access::authorize_upload(state, &options.access, bucket, &options.metadata)
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
    offset: u64,
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
    // A decimal byte range is always a valid header value.
    if offset > 0
        && let Ok(range) = HeaderValue::try_from(format!("bytes=0-{}", offset - 1))
    {
        response.headers_mut().insert("range", range);
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

fn completed(value: &serde_json::Value) -> Response<ResponseBody> {
    let mut response = Response::new(Full::new(Bytes::from(crate::json::render_body(value))));
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/json; charset=UTF-8"),
    );
    if let Some(etag) = crate::json::etag_header(value) {
        response.headers_mut().insert("etag", etag);
    }
    // The cache fields of a stored-object response come from the JSON API envelope.
    boxed(response)
}

/// Whether a chunk request only queries the upload status or carries data.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChunkKind {
    Status,
    Data,
}

/// Where a chunk request places its bytes in the upload, derived from its headers and the
/// session's acknowledged offset.
struct ChunkPlan {
    start: u64,
    length: u64,
    end: u64,
    total: Option<u64>,
    kind: ChunkKind,
    finalize: bool,
    /// The body length is unknown: `length` only bounds it, and its EOF ends the object.
    open_end: bool,
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
) -> Result<(u64, Option<u64>, bool), JsonError> {
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
            Ok((remaining, Some(total), false))
        }
        (None, Some(length)) => {
            let end = start
                .checked_add(length)
                .ok_or_else(|| JsonError::session_protocol("Upload offset overflow"))?;
            Ok((length, Some(end), false))
        }
        (None, None) => Ok((max_bytes.saturating_sub(start), None, true)),
    }
}

fn plan_chunk(
    headers: &hyper::HeaderMap,
    range: Option<Range>,
    length: Option<u64>,
    session: &SessionPosition,
    max_bytes: u64,
    target: &SessionTarget<'_>,
) -> Result<ChunkPlan, JsonError> {
    let mut open_end = false;
    let (start, length, total, kind) = match range {
        Some(Range::Status(total)) => {
            if length != Some(0) || headers.contains_key("transfer-encoding") {
                return Err(JsonError::session_protocol("Status requires an empty body"));
            }
            (session.offset, 0, total, ChunkKind::Status)
        }
        Some(Range::Data { start, end, total }) => (start, end - start + 1, total, ChunkKind::Data),
        Some(Range::Open { start, total }) => {
            let (length, total, open) =
                open_extent(start, total, length, session.total, max_bytes)?;
            open_end = open;
            (start, length, total, ChunkKind::Data)
        }
        None => {
            if session.offset != 0 && session.state != SessionState::Complete {
                return Err(JsonError::session_protocol(
                    "Content-Range required after the first chunk",
                ));
            }
            let length = length.ok_or_else(|| {
                JsonError::session_protocol("Content-Length required without Content-Range")
            })?;
            (0, length, Some(length), ChunkKind::Data)
        }
    };
    if start > session.offset {
        return Err(JsonError::session_protocol("Upload offset leaves a gap"));
    }
    let end = start
        .checked_add(length)
        .ok_or_else(|| JsonError::session_protocol("Upload offset overflow"))?;
    if end > max_bytes || total.is_some_and(|n| n > max_bytes) {
        return Err(ObjectError::TooLarge.into());
    }
    if total.is_some_and(|n| session.total.is_some_and(|t| n != t) || n < session.offset)
        || session.total.is_some_and(|n| end > n)
    {
        return Err(JsonError::session_protocol("Inconsistent upload total"));
    }
    let total = total.or(session.total);
    let finalize = match kind {
        ChunkKind::Status => matches!(range, Some(Range::Status(Some(n))) if n == session.offset),
        ChunkKind::Data => open_end || total == Some(end),
    };
    if kind == ChunkKind::Data && !finalize && !length.is_multiple_of(256 * 1024) {
        let message = if target.name.is_some() || length >= 256 * 1024 {
            "Nonfinal chunk must be a multiple of 256 KiB".into()
        } else {
            format!(
                "Invalid request.  The number of bytes uploaded is required to be equal or greater than 262144, except for the final request (it's recommended to be the exact multiple of 262144).  The received request contained {length} bytes, which does not meet this requirement."
            )
        };
        return Err(JsonError::session_protocol(message));
    }
    Ok(ChunkPlan {
        start,
        length,
        end,
        total,
        kind,
        finalize,
        open_end,
    })
}

pub(super) async fn session_response(
    request: Request<Incoming>,
    bucket: &str,
    id: &str,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, JsonError> {
    if request.method() == hyper::Method::DELETE {
        return json::resumable_cancel::cancel_response(request, bucket, id, &ctx.state.catalog)
            .await
            .map(boxed);
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
        Transfer::Complete(value) => completed(&value),
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
    // The recording proves a terminal JSON session failure only for malformed syntax.
    if target.name.is_none() && framing.is_err() && resumable::range_has_syntax_error(headers) {
        let lease = ctx
            .await_catalog(
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
        .await_catalog(state.catalog.lock_resumable_session(id, bucket))
        .await?;
    if name.is_some_and(|name| name != lease.session.name) {
        return Err(crate::storage::ResumableError::NotFound.into());
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
    let plan = plan_chunk(
        &headers,
        range,
        length,
        &position,
        config.max_object_bytes,
        &target,
    )?;
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
        // Completing a session stores its response in the same transaction.
        let completion = session
            .completion
            .as_ref()
            .ok_or_else(|| object_unavailable(BackendFailure::MissingCompletion))?;
        return Ok(Transfer::Complete(completion.clone()));
    }
    if status_only {
        json::read_body(request.into_body(), 0).await?;
        let offset = session.offset;
        state.catalog.prepare_resumable_status(&lease).await?;
        if plan.total != session.total {
            ctx.await_catalog(
                state
                    .catalog
                    .checkpoint_resumable_session(&mut lease, offset, plan.total),
            )
            .await?;
        }
        return incomplete(offset, &headers).map(Transfer::Incomplete);
    }
    let permit = state.catalog.admit_object_upload()?;
    let offset = session.offset.max(plan.end);
    let render = completion_renderer(&session.options, ctx.endpoints.api_base_url.clone())?;
    let (catalog_wait, progress) = watch::channel(false);
    let chunk = ResumableChunk {
        start: plan.start,
        length: plan.length,
        total: plan.total,
        finalize: plan.finalize,
        open_end: plan.open_end,
        options,
        catalog_wait,
    };
    let (sender, body) = mpsc::channel(2);
    let upload = state
        .catalog
        .write_resumable_session(lease, chunk, body, permit, render);
    let upload = async { upload.await.map_err(JsonError::from) };
    match stream_chunk(request.into_body(), ctx, upload, sender, progress).await? {
        Some(value) => Ok(Transfer::Complete(value)),
        None => incomplete(offset, &headers).map(Transfer::Incomplete),
    }
}

/// Renders the completion response, which the session caches for retries. The uploader
/// supplied any customer key at initiation, so the response is rendered with that key.
fn completion_renderer(
    options: &ResumableOptions,
    base: String,
) -> Result<
    impl FnOnce(&ObjectRecord) -> Result<serde_json::Value, ObjectError> + Send + 'static,
    JsonError,
> {
    let fields = options
        .response_fields
        .as_deref()
        .map(json::Fields::parse_syntax)
        .transpose()?;
    let projection = if options.projection_full {
        json::Projection::Full
    } else {
        json::Projection::NoAcl
    };
    let key = options
        .metadata
        .customer_key_sha256
        .map(crate::csek::CustomerKey::from_sha256);
    Ok(move |record: &ObjectRecord| {
        let mut value = json::object_resource_keyed(record, &base, projection, key)
            .map_err(ObjectError::backend)?;
        if let Some(fields) = &fields {
            fields.apply(&mut value);
        }
        Ok(value)
    })
}

/// Streams the chunk body into the session write and returns its completion response, or
/// `None` while the upload stays incomplete.
async fn stream_chunk(
    body: Incoming,
    ctx: ObjectCtx<'_>,
    upload: impl std::future::Future<Output = Result<Option<serde_json::Value>, JsonError>>,
    sender: mpsc::Sender<Result<UploadChunk, std::io::Error>>,
    progress: watch::Receiver<bool>,
) -> Result<Option<serde_json::Value>, JsonError> {
    let ObjectCtx {
        config,
        activity,
        deadline,
        ..
    } = ctx;
    tokio::pin!(upload);
    let timeout = ctx.catalog_timeout();
    let result = tokio::select! {
        result=&mut upload=>result?,
        result=stream_upload(body,&sender,config.max_object_bytes,activity,None)=>{
            if let Err(error)=result {
                let _=sender.send(Err(std::io::Error::other(error.to_string()))).await;
                drop(sender);
                match await_session_upload(upload,progress,timeout,deadline,activity).await {Ok(_)=>{},Err(storage)if storage.kind==json::ErrorKind::IncompleteBody=>{},Err(storage)=>return Err(storage)}
                return Err(error.into());
            }
            await_session_upload(async{let _=sender.send(Ok(UploadChunk::End)).await;drop(sender);upload.await},progress,timeout,deadline,activity).await?
        }
    };
    Ok(result)
}

async fn await_session_upload<T>(
    operation: impl std::future::Future<Output = Result<T, JsonError>>,
    mut progress: watch::Receiver<bool>,
    timeout: std::time::Duration,
    deadline: tokio::time::Instant,
    activity: &watch::Sender<ObjectActivity>,
) -> Result<T, JsonError> {
    super::begin_catalog_wait(activity).await;
    tokio::pin!(operation);
    let result = loop {
        let waiting = *progress.borrow_and_update();
        let wait_deadline = if waiting {
            deadline.min(tokio::time::Instant::now() + timeout)
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
    };
    activity.send_modify(|progress| progress.awaiting_catalog = false);
    result
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
        waiting: bool,
    ) -> (
        JoinHandle<Result<(), JsonError>>,
        watch::Sender<bool>,
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
            await_session_upload(operation, updates, BUDGET, deadline, &activity).await
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
        let (task, _progress, finish) = wait(false).await;
        advance(BUDGET * 10).await;
        settle().await;
        assert!(!task.is_finished());
        finish.send(()).unwrap();
        task.await.unwrap().unwrap();

        let (task, _progress, _finish) = wait(false).await;
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
        let (task, _progress, _finish) = wait(true).await;
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
        let (task, progress, _finish) = wait(true).await;
        advance(BUDGET - Duration::from_millis(1)).await;
        progress.send_replace(false);
        settle().await;
        advance(BUDGET * 5).await;
        progress.send_replace(true);
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
