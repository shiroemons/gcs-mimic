use super::{JsonError, read_body};
use crate::storage::{Catalog, ResumableError};
use http_body_util::Full;
use hyper::{
    Request, Response, StatusCode,
    body::{Body, Bytes},
};

pub async fn cancel_response<B: Body + Unpin>(
    request: Request<B>,
    bucket: &str,
    id: &str,
    catalog: &Catalog,
) -> Result<Response<Full<Bytes>>, JsonError> {
    read_body(request.into_body(), 0)
        .await
        .map_err(|_| JsonError::invalid_parameter("Cancellation requires an empty body"))?;
    catalog
        .cancel_resumable_session(id, bucket)
        .await
        .map_err(error)?;
    Ok(Response::builder()
        .status(499)
        .header("cache-control", "no-store")
        .body(Full::new(Bytes::new()))
        .unwrap())
}

pub(crate) fn error(error: ResumableError) -> JsonError {
    match error {
        ResumableError::NotFound => JsonError::new(
            StatusCode::NOT_FOUND,
            "notFound",
            "Upload session not found",
        ),
        ResumableError::Gone => JsonError::new(
            StatusCode::GONE,
            "gone",
            "Upload session is no longer active",
        ),
        ResumableError::Invalid(message) => JsonError::invalid_parameter(&message),
        ResumableError::Object(error) => error.into(),
    }
}
