use super::{JsonError, read_body};
use crate::storage::Catalog;
use http_body_util::Full;
use hyper::{
    Request, Response, StatusCode,
    body::{Body, Bytes},
    header::{CACHE_CONTROL, HeaderValue},
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
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = cancelled_status();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// Google answers a cancelled resumable session with the nonstandard 499 status.
#[expect(
    clippy::expect_used,
    reason = "499 lies within the 100..=999 range StatusCode accepts, and from_u16 is not const"
)]
fn cancelled_status() -> StatusCode {
    StatusCode::from_u16(499).expect("499 is a valid status code")
}
