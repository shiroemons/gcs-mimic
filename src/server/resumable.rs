use super::{ObjectActivity, ResponseBody, boxed, empty_response, stream_upload};
use crate::{
    bucket::BucketAccess,
    config::Config,
    json::{
        self, JsonError, ObjectInsertParams,
        resumable::{self, Range},
    },
    object::{ObjectError, UploadChunk},
    server::State,
    storage::{ResumableChunk, SessionState},
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use tokio::sync::{mpsc, watch};

#[allow(
    clippy::too_many_arguments,
    reason = "The upload adapter receives the existing request context"
)]
pub(super) async fn initiate(
    request: Request<Incoming>,
    bucket: &str,
    name: Option<String>,
    params: ObjectInsertParams,
    encoding: Option<String>,
    access: BucketAccess,
    state: &State,
    max: u64,
) -> Result<Response<ResponseBody>, JsonError> {
    let (mime, expected, total) = resumable::headers(request.headers(), params, max)?;
    let origin = resumable::single(request.headers(), "origin")?.map(str::to_owned);
    let metadata_type = resumable::single(request.headers(), "content-type")?.map(str::to_owned);
    let json_type = metadata_type.as_ref().is_some_and(|v| {
        v.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("application/json")
    });
    let has_body = request
        .headers()
        .get("content-length")
        .is_some_and(|v| v != "0")
        || request.headers().contains_key("transfer-encoding");
    if has_body && !json_type {
        return Err(JsonError::invalid_parameter(
            "Metadata requires application/json",
        ));
    }
    let base = crate::url::normalize_base_url(&state.api_base_url)
        .map_err(JsonError::invalid_parameter)?;
    let location_prefix =
        format!("{base}/upload/storage/v1/b/{bucket}/o?uploadType=resumable&upload_id=");
    location_prefix
        .parse::<hyper::header::HeaderValue>()
        .map_err(|_| JsonError::invalid_parameter("Invalid session endpoint"))?;
    let bytes = json::read_body(request.into_body(), 64 * 1024).await?;
    if !bytes.is_empty() && !json_type {
        return Err(JsonError::invalid_parameter(
            "Metadata requires application/json",
        ));
    }
    let (name, options) = resumable::options(
        &bytes, name, mime, expected, params, access, origin, encoding,
    )?;
    let session = state
        .catalog
        .create_resumable_session(bucket, &name, options, total)
        .await?;
    let location = format!("{location_prefix}{}", session.id);
    let mut response = empty_response(StatusCode::OK);
    response.headers_mut().insert(
        "location",
        location
            .parse()
            .map_err(|_| JsonError::invalid_parameter("Invalid session endpoint"))?,
    );
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    Ok(boxed(response))
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
    if offset > 0 {
        response
            .headers_mut()
            .insert("range", format!("bytes=0-{}", offset - 1).parse().unwrap());
    }
    if override_status {
        response
            .headers_mut()
            .insert("x-http-status-code-override", "308".parse().unwrap());
    }
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    Ok(boxed(response))
}

pub(super) fn completed(value: serde_json::Value) -> Response<ResponseBody> {
    let mut response = Response::new(Full::new(Bytes::from(value.to_string())));
    response.headers_mut().insert(
        "content-type",
        "application/json; charset=UTF-8".parse().unwrap(),
    );
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    boxed(response)
}

#[allow(
    clippy::too_many_arguments,
    reason = "Session operations share the server request context"
)]
pub(super) async fn session_response(
    request: Request<Incoming>,
    bucket: &str,
    id: &str,
    state: &State,
    config: &Config,
    activity: &watch::Sender<ObjectActivity>,
    deadline: tokio::time::Instant,
) -> Result<Response<ResponseBody>, JsonError> {
    if request.method() == hyper::Method::DELETE {
        return json::resumable_cancel::cancel_response(request, bucket, id, &state.catalog)
            .await
            .map(boxed);
    }
    let range = resumable::range(request.headers())?;
    let headers = request.headers().clone();
    let length = resumable::single(&headers, "content-length")?
        .map(resumable::unsigned)
        .transpose()?;
    if length.is_some_and(|n| n > config.max_object_bytes) {
        return Err(ObjectError::TooLarge.into());
    }
    let oversized = match range {
        Some(Range::Data { end, total, .. }) => {
            end >= config.max_object_bytes || total.is_some_and(|n| n > config.max_object_bytes)
        }
        Some(Range::Status(total)) => total.is_some_and(|n| n > config.max_object_bytes),
        None => false,
    };
    if oversized {
        return Err(ObjectError::TooLarge.into());
    }
    if let Some(Range::Data { start, end, .. }) = range
        && length.is_some_and(|length| length != end - start + 1)
    {
        return Err(JsonError::invalid_parameter(
            "Content-Length does not match Content-Range",
        ));
    }
    let mut lease = state.catalog.lock_resumable_session(id, bucket).await?;
    let session = &lease.session;
    let (start, length, total, status) = match range {
        Some(Range::Status(total)) => {
            if length != Some(0) || headers.contains_key("transfer-encoding") {
                return Err(JsonError::invalid_parameter(
                    "Status requires an empty body",
                ));
            }
            (session.offset, 0, total, true)
        }
        Some(Range::Data { start, end, total }) => (start, end - start + 1, total, false),
        None => {
            if session.offset != 0 && session.state != SessionState::Complete {
                return Err(JsonError::invalid_parameter(
                    "Content-Range required after the first chunk",
                ));
            }
            let length = length.ok_or_else(|| {
                JsonError::invalid_parameter("Content-Length required without Content-Range")
            })?;
            (0, length, Some(length), false)
        }
    };
    if start > session.offset {
        return Err(JsonError::invalid_parameter("Upload offset leaves a gap"));
    }
    let end = start
        .checked_add(length)
        .ok_or_else(|| JsonError::invalid_parameter("Upload offset overflow"))?;
    if end > config.max_object_bytes || total.is_some_and(|n| n > config.max_object_bytes) {
        return Err(ObjectError::TooLarge.into());
    }
    if total.is_some_and(|n| session.total.is_some_and(|t| n != t) || n < session.offset)
        || session.total.is_some_and(|n| end > n)
    {
        return Err(JsonError::invalid_parameter("Inconsistent upload total"));
    }
    let total = total.or(session.total);
    let finalize = if status {
        matches!(range,Some(Range::Status(Some(n))) if n==session.offset)
    } else {
        total == Some(end)
    };
    if !status && !finalize && !length.is_multiple_of(256 * 1024) {
        return Err(JsonError::invalid_parameter(
            "Nonfinal chunk must be a multiple of 256 KiB",
        ));
    }
    // Validate the override before any bytes can be acknowledged.
    resumable::single(&headers, "x-guploader-no-308")?;
    let options = match resumable::final_options(&headers, session.options.clone()) {
        Ok(options) => options,
        Err(error) => {
            if finalize {
                state.catalog.invalidate_resumable_session(&lease).await?;
            }
            return Err(error);
        }
    };
    if !finalize
        && (headers.contains_key("x-goog-hash")
            || headers
                .keys()
                .any(|k| k.as_str().starts_with("x-goog-meta-")))
    {
        return Err(JsonError::invalid_parameter(
            "Final metadata requires a final request",
        ));
    }
    if session.state == SessionState::Complete {
        return Ok(completed(
            session.completion.clone().expect("validated completion"),
        ));
    }
    if status && !finalize {
        json::read_body(request.into_body(), 0).await?;
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(lease.staging_path())
            .await
            .map_err(|e| ObjectError::Unavailable(e.to_string()))?;
        if file
            .metadata()
            .await
            .map_err(|e| ObjectError::Unavailable(e.to_string()))?
            .len()
            < session.offset
        {
            return Err(
                ObjectError::Unavailable("Acknowledged upload prefix is missing".into()).into(),
            );
        }
        file.set_len(session.offset)
            .await
            .map_err(|e| ObjectError::Unavailable(e.to_string()))?;
        let offset = session.offset;
        if total != session.total {
            state
                .catalog
                .checkpoint_resumable_session(&mut lease, offset, total)
                .await?;
        }
        return incomplete(offset, &headers);
    }
    let permit = state.catalog.admit_object_upload()?;
    let offset = session.offset.max(end);
    let (catalog_wait, progress) = watch::channel(false);
    let chunk = ResumableChunk {
        start,
        length,
        total,
        finalize,
        options,
        catalog_wait,
    };
    let (sender, body) = mpsc::channel(2);
    let base = state.api_base_url.clone();
    let projection = if lease.session.options.projection_full {
        json::Projection::Full
    } else {
        json::Projection::NoAcl
    };
    let upload = state
        .catalog
        .write_resumable_session(lease, chunk, body, permit, move |record| {
            json::object_resource(record, &base, projection)
                .map_err(|e| ObjectError::Unavailable(e.to_string()))
        });
    let upload = async { upload.await.map_err(JsonError::from) };
    tokio::pin!(upload);
    let timeout = std::time::Duration::from_millis(config.catalog_wait_timeout_ms);
    let result = tokio::select! {
        result=&mut upload=>result?,
        result=stream_upload(request.into_body(),&sender,config.max_object_bytes,activity)=>{
            if let Err(error)=result {
                let _=sender.send(Err(std::io::Error::other(error.to_string()))).await;
                drop(sender);
                match await_session_upload(upload,progress,timeout,deadline,activity).await {Ok(_)=>{},Err(storage)if storage.reason=="badRequest"=>{},Err(storage)=>return Err(storage)}
                return Err(error.into());
            }
            await_session_upload(async{let _=sender.send(Ok(UploadChunk::End)).await;drop(sender);upload.await},progress,timeout,deadline,activity).await?
        }
    };
    match result {
        Some(value) => Ok(completed(value)),
        None => incomplete(offset, &headers),
    }
}

async fn await_session_upload<T>(
    operation: impl std::future::Future<Output = Result<T, JsonError>>,
    mut progress: watch::Receiver<bool>,
    timeout: std::time::Duration,
    deadline: tokio::time::Instant,
    activity: &watch::Sender<ObjectActivity>,
) -> Result<T, JsonError> {
    activity.send_modify(|progress| progress.awaiting_catalog = true);
    tokio::pin!(operation);
    let result = loop {
        let waiting = *progress.borrow_and_update();
        let wait_deadline = if waiting {
            deadline.min(tokio::time::Instant::now() + timeout)
        } else {
            deadline
        };
        tokio::select! {
            result = &mut operation => break result,
            Ok(()) = progress.changed() => {},
            _ = tokio::time::sleep_until(wait_deadline) => {
                let message = if waiting { "upload catalog wait timed out" } else { "upload finalization timed out" };
                break Err(ObjectError::Unavailable(message.into()).into());
            }
        }
    };
    activity.send_modify(|progress| progress.awaiting_catalog = false);
    result
}
