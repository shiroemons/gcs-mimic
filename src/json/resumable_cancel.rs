use super::{JsonError, read_body};
use crate::storage::Catalog;
use http_body_util::Full;
use hyper::{
    Request, Response,
    body::{Body, Bytes},
};

pub(crate) async fn cancel_response<B: Body + Unpin>(
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
        .map_err(JsonError::from)?;
    Ok(JsonError::from(crate::storage::ResumableError::Cancelled).into_response())
}
