use super::{ObjectActivity, ResponseBody, boxed, empty_response};
use crate::{
    bucket::BucketAccess,
    config::Config,
    json::{
        self, JsonError, ObjectInsertParams,
        resumable::{self, Range},
    },
    object::ObjectError,
    server::State,
    storage::SessionState,
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use tokio::sync::watch;

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
    let bytes = json::read_body(request.into_body(), 64 * 1024).await?;
    if !bytes.is_empty()
        && !metadata_type.is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
    {
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
    let location = format!(
        "{}/upload/storage/v1/b/{}/o?uploadType=resumable&upload_id={}",
        state.api_base_url, bucket, session.id
    );
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
    _config: &Config,
    _activity: &watch::Sender<ObjectActivity>,
    _deadline: tokio::time::Instant,
) -> Result<Response<ResponseBody>, JsonError> {
    if request.method() == hyper::Method::DELETE {
        return Err(JsonError::unsupported_object("Session cancellation"));
    }
    let range = resumable::range(request.headers())?;
    let Some(Range::Status(total)) = range else {
        return Err(JsonError::unsupported_object("Session data writes"));
    };
    if resumable::single(request.headers(), "content-length")? != Some("0")
        || request.headers().contains_key("transfer-encoding")
    {
        return Err(JsonError::invalid_parameter(
            "Status requires an empty body",
        ));
    }
    let headers = request.headers().clone();
    json::read_body(request.into_body(), 0).await?;
    let lease = state.catalog.lock_resumable_session(id, bucket).await?;
    let session = &lease.session;
    if total.is_some_and(|n| session.total.is_some_and(|t| n != t) || n < session.offset) {
        return Err(JsonError::invalid_parameter("Inconsistent upload total"));
    }
    if session.state == SessionState::Complete {
        return Ok(completed(
            session.completion.clone().expect("validated completion"),
        ));
    }
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
    incomplete(session.offset, &headers)
}
