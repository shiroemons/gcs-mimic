//! Bounded JSON API batching without a second listener connection or catalog transaction.

use super::{RequestBody, RequestCtx, ResponseBody, boxed};
use crate::json::{self, JsonError};
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode,
    body::{Body, Bytes},
};
use tokio::sync::Semaphore;

mod parse;

const REQUEST_LIMIT: usize = 10_000_000;
const RESPONSE_LIMIT: usize = 10_000_000;
const PART_RESPONSE_LIMIT: usize = 512 * 1024;
const BOUNDARY: &str = "gcs_mimic_batch_response";
static PARSERS: Semaphore = Semaphore::const_new(1);

struct Part {
    id: Option<String>,
    request: Request<RequestBody>,
}

fn invalid(message: &'static str) -> JsonError {
    JsonError::new(StatusCode::BAD_REQUEST, "badRequest", message)
}

pub(super) async fn respond(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Response<ResponseBody> {
    match execute(request, ctx).await {
        Ok(response) => boxed(response),
        Err(error) => boxed(error.into_response()),
    }
}

async fn execute(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Response<Full<Bytes>>, JsonError> {
    if request.method() != Method::POST {
        return Err(invalid("Batch requests require POST /batch/storage/v1"));
    }
    let boundary = json::batch_boundary(request.headers())?;
    if request.headers().contains_key("content-encoding") {
        return Err(invalid("Compressed batch envelopes are unsupported"));
    }
    let (outer, mut body) = request.into_parts();
    if body.size_hint().lower() >= REQUEST_LIMIT as u64 {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let data = frame
            .map_err(|_| invalid("Could not read batch request"))?
            .into_data()
            .map_err(|_| invalid("Batch request trailers are unsupported"))?;
        if data.len() >= REQUEST_LIMIT - bytes.len() {
            return Err(too_large());
        }
        bytes.extend_from_slice(&data);
        super::body_progress(ctx.activity);
    }
    let permit = PARSERS
        .acquire()
        .await
        .map_err(|_| invalid("Batch parser unavailable"))?;
    let parts = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        parse::parts(
            &Bytes::from(bytes),
            &boundary,
            &outer.headers,
            outer.uri.query(),
        )
    })
    .await
    .map_err(|_| invalid("Batch parser failed"))??;
    let mut output = Vec::new();
    for part in parts {
        // Reserve enough space to render every remaining per-part error, even after large lists.
        let cap = PART_RESPONSE_LIMIT.min(RESPONSE_LIMIT.saturating_sub(output.len() + 128 * 1024));
        let response = Box::pin(super::routing::respond(part.request, ctx)).await;
        append_response(&mut output, part.id.as_deref(), response, cap).await;
    }
    output.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    let mut response = Response::new(Full::new(Bytes::from(output)));
    response.headers_mut().insert(
        "content-type",
        format!("multipart/mixed; boundary={BOUNDARY}")
            .parse()
            .map_err(|_| invalid("Batch response header failed"))?,
    );
    Ok(response)
}

fn too_large() -> JsonError {
    JsonError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "uploadTooLarge",
        "Batch request must be smaller than 10 MB",
    )
}

async fn bounded_response(
    response: Response<ResponseBody>,
    cap: usize,
) -> Result<(StatusCode, HeaderMap, Vec<u8>), JsonError> {
    let (parts, mut body) = response.into_parts();
    let header_bytes: usize = parts
        .headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.len() + 4)
        .sum();
    if header_bytes > 8192 || header_bytes > cap {
        return Err(response_too_large());
    }
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let data = frame
            .map_err(|_| invalid("Batched response stream failed"))?
            .into_data()
            .map_err(|_| invalid("Batched response trailers are unsupported"))?;
        if data.len() > cap.saturating_sub(bytes.len() + header_bytes) {
            return Err(response_too_large());
        }
        bytes.extend_from_slice(&data);
    }
    Ok((parts.status, parts.headers, bytes))
}

fn response_too_large() -> JsonError {
    JsonError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "responseTooLarge",
        "Batched response exceeds the local response budget; the operation may have completed",
    )
}

async fn append_response(
    output: &mut Vec<u8>,
    id: Option<&str>,
    response: Response<ResponseBody>,
    cap: usize,
) {
    let (status, headers, body) = match bounded_response(response, cap).await {
        Ok(response) => response,
        Err(error) => {
            let (parts, body) = error.into_response().into_parts();
            let body = match body.collect().await {
                Ok(body) => body.to_bytes(),
                Err(never) => match never {},
            };
            (parts.status, parts.headers, body.to_vec())
        }
    };
    output.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Type: application/http\r\n").as_bytes(),
    );
    if let Some(id) = id {
        let id = if let Some(id) = id.strip_prefix('<').and_then(|id| id.strip_suffix('>')) {
            format!("<response-{id}>")
        } else {
            format!("response-{id}")
        };
        output.extend_from_slice(format!("Content-ID: {id}\r\n").as_bytes());
    }
    output.extend_from_slice(
        format!(
            "\r\nHTTP/1.1 {} {}\r\n",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        )
        .as_bytes(),
    );
    for (name, value) in &headers {
        if !parse::hop_header(name.as_str()) && name != "content-length" {
            output.extend_from_slice(name.as_str().as_bytes());
            output.extend_from_slice(b": ");
            output.extend_from_slice(value.as_bytes());
            output.extend_from_slice(b"\r\n");
        }
    }
    output.extend_from_slice(format!("content-length: {}\r\n\r\n", body.len()).as_bytes());
    output.extend_from_slice(&body);
    output.extend_from_slice(b"\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn oversized_part_responses_keep_complete_error_framing_and_content_ids() {
        let response = boxed(Response::new(Full::new(Bytes::from(vec![b'x'; 1024]))));
        let mut bytes = Vec::new();
        append_response(&mut bytes, Some("<bounded>"), response, 100).await;
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("Content-ID: <response-bounded>\r\n"));
        assert!(text.contains("HTTP/1.1 413 Payload Too Large\r\n"));
        let (head, body) = text.split_once("\r\n\r\nHTTP/").unwrap();
        assert!(head.starts_with("--gcs_mimic_batch_response\r\n"));
        let (head, body) = body.split_once("\r\n\r\n").unwrap();
        let length = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert_eq!(length, body.len() - 2);
        assert!(body.contains("responseTooLarge"));
        assert!(body.contains("operation may have completed"));
        let mut response = Response::new(Full::new(Bytes::new()));
        response.headers_mut().insert(
            "x-header",
            hyper::header::HeaderValue::from_static("header-only"),
        );
        let mut output = Vec::new();
        append_response(&mut output, None, boxed(response), 0).await;
        assert!(
            std::str::from_utf8(&output)
                .unwrap()
                .contains("HTTP/1.1 413 Payload Too Large")
        );
    }
}
