//! Transport input normalization before API routing and operation authorization.

use super::{BUCKET_BODY_LIMIT, RequestCtx};
use crate::json::JsonError;
use flate2::read::MultiGzDecoder;
use http_body_util::{BodyExt, Full};
use hyper::{
    Method, Request, StatusCode,
    body::{Body, Bytes, Frame, Incoming, SizeHint},
    header::{CONTENT_ENCODING, CONTENT_LENGTH, HeaderValue, TRANSFER_ENCODING},
};
use std::{
    io::{self, Read},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::Semaphore;

mod upload_gzip;

/// Synthetic batch parts and decoded metadata use the same dispatch as network input.
pub(crate) enum RequestBody {
    Transport(Incoming),
    Buffered(Full<Bytes>),
    Decoded(upload_gzip::DecodedBody),
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        match self.get_mut() {
            Self::Transport(body) => Pin::new(body)
                .poll_frame(cx)
                .map(|frame| frame.map(|result| result.map_err(io::Error::other))),
            Self::Buffered(body) => Pin::new(body)
                .poll_frame(cx)
                .map(|frame| frame.map(|result| result.map_err(|never| match never {}))),
            Self::Decoded(body) => Pin::new(body).poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Transport(body) => body.is_end_stream(),
            Self::Buffered(body) => body.is_end_stream(),
            Self::Decoded(body) => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Transport(body) => body.size_hint(),
            Self::Buffered(body) => body.size_hint(),
            Self::Decoded(body) => body.size_hint(),
        }
    }
}

fn bad_request(message: &'static str) -> JsonError {
    JsonError::new(StatusCode::BAD_REQUEST, "badRequest", message)
}

pub(super) fn override_method(request: &mut Request<RequestBody>) -> Result<(), JsonError> {
    let mut values = request.headers().get_all("x-http-method-override").iter();
    let Some(value) = values.next() else {
        return Ok(());
    };
    if values.next().is_some() || value != "PATCH" || request.method() != Method::POST {
        return Err(bad_request(
            "Use one X-HTTP-Method-Override: PATCH header on a JSON API POST",
        ));
    }
    let segments: Vec<_> = request
        .uri()
        .path()
        .strip_prefix("/storage/v1/b/")
        .unwrap_or_default()
        .split('/')
        .collect();
    let allowed = matches!(
        segments.as_slice(),
        [_] | [_, "o", _] | [_, "acl" | "defaultObjectAcl", _] | [_, "o", _, "acl", _]
    );
    if !allowed || segments.iter().any(|segment| segment.is_empty()) {
        return Err(bad_request(
            "X-HTTP-Method-Override is supported only on JSON resource PATCH routes",
        ));
    }
    *request.method_mut() = Method::PATCH;
    request.headers_mut().remove("x-http-method-override");
    Ok(())
}

// One short job fits every supported runtime's blocking pool; connection admission bounds waiters.
static DECODERS: Semaphore = Semaphore::const_new(1);

pub(super) async fn decode_metadata(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Request<RequestBody>, JsonError> {
    if request.method() == Method::POST
        && request.uri().path().starts_with("/upload/storage/v1/")
        && crate::json::parse_query(request.uri().query().unwrap_or_default())?
            .get("uploadType")
            .is_some_and(|kind| kind == "multipart")
        && request.headers().contains_key(CONTENT_ENCODING)
    {
        return upload_gzip::request(request, ctx);
    }
    let resumable_metadata = request.method() == Method::POST
        && request
            .uri()
            .path()
            .strip_prefix("/upload/storage/v1/b/")
            .and_then(|path| path.split_once('/'))
            .is_some_and(|(bucket, rest)| !bucket.is_empty() && rest == "o")
        && {
            let query = crate::json::parse_query(request.uri().query().unwrap_or_default())?;
            query
                .get("uploadType")
                .is_some_and(|kind| kind == "resumable")
                && !query.contains_key("upload_id")
        };
    if !request.uri().path().starts_with("/storage/v1/") && !resumable_metadata {
        return Ok(request);
    }
    let mut encodings = request.headers().get_all(CONTENT_ENCODING).iter();
    let Some(encoding) = encodings.next() else {
        return Ok(request);
    };
    if encodings.next().is_some() || !encoding.as_bytes().eq_ignore_ascii_case(b"gzip") {
        return Err(bad_request(
            "JSON request Content-Encoding must be a single gzip coding",
        ));
    }
    ctx.state
        .auth
        .authenticate_at(
            request.headers(),
            request.uri().query(),
            ctx.state.clock.as_ref(),
        )
        .map_err(JsonError::from)?;
    let (mut parts, mut body) = request.into_parts();
    if body.size_hint().lower() > BUCKET_BODY_LIMIT as u64 {
        return Err(too_large());
    }
    let mut compressed = Vec::new();
    while let Some(frame) = body.frame().await {
        let data = frame
            .map_err(|_| bad_request("Could not read compressed request body"))?
            .into_data()
            .map_err(|_| bad_request("Compressed JSON request trailers are unsupported"))?;
        if data.len() > BUCKET_BODY_LIMIT - compressed.len() {
            return Err(too_large());
        }
        compressed.extend_from_slice(&data);
        super::body_progress(ctx.activity);
    }
    let permit = DECODERS
        .acquire()
        .await
        .map_err(|_| decoder_unavailable())?;
    let decoded = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        decode(&compressed, BUCKET_BODY_LIMIT)
    })
    .await
    .map_err(|_| decoder_unavailable())?
    .map_err(|error| match error {
        DecodeError::Limit => too_large(),
        DecodeError::Invalid(_) => bad_request("Invalid or truncated gzip request body"),
    })?;
    parts.headers.remove(CONTENT_ENCODING);
    parts.headers.remove(TRANSFER_ENCODING);
    parts.headers.remove("trailer");
    parts
        .headers
        .insert(CONTENT_LENGTH, HeaderValue::from(decoded.len()));
    Ok(Request::from_parts(
        parts,
        RequestBody::Buffered(Full::new(Bytes::from(decoded))),
    ))
}

fn too_large() -> JsonError {
    JsonError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "uploadTooLarge",
        "Compressed or expanded JSON request body exceeds 64 KiB",
    )
}

fn decoder_unavailable() -> JsonError {
    JsonError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "backendError",
        "Request decoder is unavailable",
    )
}

#[derive(Debug)]
enum DecodeError {
    Invalid(io::Error),
    Limit,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(_) => f.write_str("Invalid gzip input"),
            Self::Limit => f.write_str("Expanded input exceeds its limit"),
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(error) => Some(error),
            Self::Limit => None,
        }
    }
}

fn decode(compressed: &[u8], cap: usize) -> Result<Vec<u8>, DecodeError> {
    let mut decoded = Vec::new();
    MultiGzDecoder::new(compressed)
        .take(cap as u64 + 1)
        .read_to_end(&mut decoded)
        .map_err(DecodeError::Invalid)?;
    if decoded.len() > cap {
        return Err(DecodeError::Limit);
    }
    Ok(decoded)
}
