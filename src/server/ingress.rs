//! Transport input normalization before API routing and operation authorization.

use super::{BodyRead, ObjectActivity, RESOURCE_BODY_LIMIT, RequestCtx};
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
use tokio::sync::watch;

mod upload_gzip;

/// Synthetic batch parts and decoded metadata use the same dispatch as network input.
pub(crate) enum RequestBody {
    Transport(TransportBody),
    Buffered(Full<Bytes>),
    Decoded(super::chunk_body::CheckedBody),
}

/// A request body arriving from the client. Every adapter reads through here, so the
/// connection's watchdog learns from one place when a request is waiting on the client:
/// a stalled body on any route is then bounded by body inactivity, not by the request deadline.
pub(crate) struct TransportBody {
    body: Incoming,
    activity: watch::Sender<ObjectActivity>,
}

impl TransportBody {
    fn report(&self, read: BodyRead) {
        self.activity.send_if_modified(|progress| {
            let changed = progress.body_read != read;
            progress.body_read = read;
            changed
        });
    }
}

impl Drop for TransportBody {
    fn drop(&mut self) {
        self.report(BodyRead::NotPending);
    }
}

impl Body for TransportBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.body).poll_frame(cx);
        match &polled {
            Poll::Pending => this.report(BodyRead::Pending),
            Poll::Ready(Some(Ok(_))) => {
                let read = if this.body.is_end_stream() {
                    BodyRead::NotPending
                } else {
                    BodyRead::Pending
                };
                // Notifying even when the state is unchanged makes every frame renew the limit.
                this.activity
                    .send_modify(|progress| progress.body_read = read);
            }
            Poll::Ready(Some(Err(_)) | None) => this.report(BodyRead::NotPending),
        }
        polled.map(|frame| frame.map(|result| result.map_err(io::Error::other)))
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

impl RequestBody {
    pub(super) fn transport(body: Incoming, activity: watch::Sender<ObjectActivity>) -> Self {
        Self::Transport(TransportBody { body, activity })
    }
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        match self.get_mut() {
            Self::Transport(body) => Pin::new(body).poll_frame(cx),
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

pub(super) async fn decode_metadata(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Request<RequestBody>, JsonError> {
    if request.method() == Method::POST
        && request.uri().path().starts_with("/upload/storage/v1/")
        && crate::json::parse_query(request.uri().query().unwrap_or_default())?
            .get("uploadType")
            .is_some_and(|kind| kind == "multipart" || kind == "media")
        && request.headers().contains_key(CONTENT_ENCODING)
    {
        return upload_gzip::request(request, ctx).await;
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
        .authenticate_request(
            request.headers(),
            request.uri().query(),
            ctx.state.clock.as_ref(),
            ctx.listener,
        )
        .await
        .map_err(JsonError::from)?;
    let (mut parts, mut body) = request.into_parts();
    if body.size_hint().lower() > RESOURCE_BODY_LIMIT as u64 {
        return Err(too_large());
    }
    let mut compressed = Vec::new();
    while let Some(frame) = body.frame().await {
        let data = frame
            .map_err(|_| bad_request("Could not read compressed request body"))?
            .into_data()
            .map_err(|_| bad_request("Compressed JSON request trailers are unsupported"))?;
        if data.len() > RESOURCE_BODY_LIMIT - compressed.len() {
            return Err(too_large());
        }
        compressed.extend_from_slice(&data);
    }
    let permit = ctx
        .state
        .pools
        .gzip_decoders
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| decoder_unavailable())?;
    let decoded = crate::task::run_blocking("json-gzip-decoder", move || {
        let _permit = permit;
        decode(&compressed, RESOURCE_BODY_LIMIT)
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
        format!(
            "Compressed or expanded JSON request body exceeds {}",
            crate::json::RESOURCE_BODY_LIMIT_TEXT
        ),
    )
}

fn decoder_unavailable() -> JsonError {
    JsonError::service_unavailable("Request decoder is unavailable")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::Auth,
        config::Config,
        server::{ObjectActivity, State, WorkPools},
        storage::Catalog,
    };
    use std::{io::Write, sync::Arc, time::Duration};

    #[test]
    fn work_pools_scale_with_blocking_threads_and_do_not_share_permits() {
        for (threads, expected) in [(1, 1), (8, 2), (16, 4)] {
            let config = Config {
                max_blocking_threads: threads,
                ..Config::default()
            };
            let pools = WorkPools::new(&config);
            assert_eq!(pools.gzip_decoders.available_permits(), expected);
            assert_eq!(pools.batches.available_permits(), 2);
            let other = WorkPools::new(&config);
            assert!(!Arc::ptr_eq(&pools.gzip_decoders, &other.gzip_decoders));
            assert!(!Arc::ptr_eq(&pools.batches, &other.batches));
        }
    }

    #[tokio::test]
    async fn gzip_metadata_admission_uses_the_server_pool() {
        let dir = crate::test_support::TestDir::new();
        let state = std::sync::Arc::new(State::for_test(
            Catalog::open(&dir.0).await.unwrap(),
            Auth::permissive(),
        ));
        let config = Config::default();
        let endpoints = config
            .runtime_endpoints
            .snapshot(&config, "http://localhost")
            .unwrap()
            .without_request();
        let (activity, _) = tokio::sync::watch::channel(ObjectActivity::default());
        let ctx = RequestCtx {
            state: &state,
            config: &config,
            endpoints: &endpoints,
            activity: &activity,
            listener: crate::auth::ListenerRole::Public,
        };
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(b"{}").unwrap();
        let compressed = encoder.finish().unwrap();
        let request = || {
            Request::builder()
                .method("POST")
                .uri("/storage/v1/b?project=local")
                .header("content-encoding", "gzip")
                .body(RequestBody::Buffered(Full::new(Bytes::from(
                    compressed.clone(),
                ))))
                .unwrap()
        };
        let first = state
            .pools
            .gzip_decoders
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), decode_metadata(request(), ctx))
                .await
                .unwrap()
                .is_ok()
        );
        let second = state
            .pools
            .gzip_decoders
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        let pending = decode_metadata(request(), ctx);
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut pending)
                .await
                .is_err()
        );
        drop(second);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), pending)
                .await
                .unwrap()
                .is_ok()
        );
        drop(first);
        state.catalog.close().await.unwrap();
    }
}
