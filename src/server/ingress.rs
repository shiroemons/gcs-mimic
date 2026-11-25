//! Transport input normalization before API routing and authentication: the request body every
//! adapter reads, the JSON method override, and the transport gzip that Cloud Storage's front end
//! removes before it routes a JSON API request.

use super::{BodyRead, ObjectActivity, RESOURCE_BODY_LIMIT, RequestCtx, ResponseBody, boxed};
use crate::{api_error::ApiError, json::JsonErrorResponse};
use http_body_util::{BodyExt, Full};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Body, Bytes, Frame, Incoming, SizeHint},
    header::{CONTENT_ENCODING, CONTENT_LENGTH, HeaderValue, TRANSFER_ENCODING},
};
use std::{
    io::{self, Read},
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
};
use tokio::sync::watch;

mod members;
mod upload_gzip;

/// Synthetic batch parts and decoded metadata use the same dispatch as network input.
pub(crate) enum RequestBody {
    Transport(TransportBody),
    Buffered(Full<Bytes>),
    Decoded(DecodedBody),
}

/// A request body arriving from the client. Every adapter reads through here, so the
/// connection's watchdog learns from one place when a request is waiting on the client:
/// a stalled body on any route is then bounded by body inactivity, not by the request deadline.
pub(crate) struct TransportBody<B = Incoming> {
    body: B,
    activity: watch::Sender<ObjectActivity>,
}

impl<B> TransportBody<B> {
    fn report(&self, read: BodyRead) {
        self.activity.send_if_modified(|progress| {
            let changed = progress.body_read != read;
            progress.body_read = read;
            changed
        });
    }
}

impl<B> Drop for TransportBody<B> {
    fn drop(&mut self) {
        self.report(BodyRead::NotPending);
    }
}

impl<B> Body for TransportBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.body).poll_frame(cx);
        match &polled {
            // Only here does the request wait on the client, so only this starts its silence.
            Poll::Pending => this.report(BodyRead::Pending),
            Poll::Ready(Some(Ok(_))) => {
                // A delivered frame is the server's until it asks for the next one, so work
                // between frames, such as a write that waits for the disk, counts against the
                // request deadline instead of the client's silence. Notifying even when the
                // state is unchanged makes every frame renew the limit.
                this.activity
                    .send_modify(|progress| progress.body_read = BodyRead::NotPending);
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

fn bad_request(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "badRequest", message)
}

pub(super) fn override_method(request: &mut Request<RequestBody>) -> Result<(), ApiError> {
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

/// What the `Content-Encoding` of a JSON API request applies to.
enum Coded {
    /// A JSON resource body or the metadata of a resumable initiation, within the 16 MiB
    /// resource limit.
    Metadata,
    /// The body of a media or multipart upload, streamed to its handler.
    Upload,
}

impl Coded {
    /// `None` for a request without a coding and for one whose handler reads the coding itself,
    /// such as a resumable session request, which refuses it.
    fn of(request: &Request<RequestBody>) -> Result<Option<Self>, ApiError> {
        if !request.headers().contains_key(CONTENT_ENCODING) {
            return Ok(None);
        }
        let path = request.uri().path();
        if path.starts_with("/storage/v1/") {
            return Ok(Some(Self::Metadata));
        }
        if request.method() != Method::POST || !path.starts_with("/upload/storage/v1/") {
            return Ok(None);
        }
        let query = crate::percent::parse_query(request.uri().query().unwrap_or_default())?;
        let initiation = !query.contains_key("upload_id")
            && path
                .strip_prefix("/upload/storage/v1/b/")
                .and_then(|path| path.split_once('/'))
                .is_some_and(|(bucket, rest)| !bucket.is_empty() && rest == "o");
        Ok(match query.get("uploadType").map(String::as_str) {
            Some("multipart" | "media") => Some(Self::Upload),
            Some("resumable") if initiation => Some(Self::Metadata),
            _ => None,
        })
    }
}

/// Expands a transport-compressed JSON API request body before the request is routed or
/// authenticated, as Cloud Storage's front end does (recorded 2026-11-25). A body that does not
/// decode answers [`ApiError::undecodable_body`] whoever sends it and whatever route it names,
/// and a body that decodes then meets the route's checks exactly as the same body sent plain. An
/// empty body stays empty without a decoder.
///
/// A media or multipart upload streams its body to its handler instead, so only its first bytes
/// are checked here; the returned [`UploadDecoder`] reports a failure found while the handler
/// reads the rest.
pub(super) async fn expand(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<(Request<RequestBody>, UploadDecoder), ApiError> {
    match Coded::of(&request)? {
        None => Ok((request, UploadDecoder(None))),
        Some(Coded::Upload) => upload_gzip::request(request, ctx).await,
        Some(Coded::Metadata) => Ok((expand_metadata(request, ctx).await?, UploadDecoder(None))),
    }
}

async fn expand_metadata(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Request<RequestBody>, ApiError> {
    let mut encodings = request.headers().get_all(CONTENT_ENCODING).iter();
    if encodings
        .next()
        .is_none_or(|encoding| !encoding.as_bytes().eq_ignore_ascii_case(b"gzip"))
        || encodings.next().is_some()
    {
        return Err(bad_request(
            "JSON request Content-Encoding must be a single gzip coding",
        ));
    }
    let (parts, mut body) = request.into_parts();
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
    if compressed.is_empty() {
        return Ok(without_coding(parts, Bytes::new()));
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
        DecodeError::Invalid(_) => ApiError::undecodable_body(),
    })?;
    Ok(without_coding(parts, Bytes::from(decoded)))
}

/// The request with `decoded` as its body and the headers that described the coded body removed.
fn without_coding(mut parts: hyper::http::request::Parts, decoded: Bytes) -> Request<RequestBody> {
    parts.headers.remove(CONTENT_ENCODING);
    parts.headers.remove(TRANSFER_ENCODING);
    parts.headers.remove("trailer");
    parts
        .headers
        .insert(CONTENT_LENGTH, HeaderValue::from(decoded.len()));
    Request::from_parts(parts, RequestBody::Buffered(Full::new(decoded)))
}

/// What became of a streamed upload's decoding, shared by its decoder, its body and its
/// [`UploadDecoder`].
#[derive(Default)]
struct Decoding {
    /// The decoder found that the body does not decode.
    undecodable: OnceLock<()>,
    /// The handler read that failure from the body.
    reported: OnceLock<()>,
}

/// The decoded body of a streamed upload.
pub(crate) struct DecodedBody {
    body: super::chunk_body::CheckedBody,
    decoding: Arc<Decoding>,
}

impl Body for DecodedBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.body).poll_frame(cx);
        // The decoder records the failure before it sends the error that reports it, so an
        // error read after the record is that failure.
        if matches!(polled, Poll::Ready(Some(Err(_)))) && this.decoding.undecodable.get().is_some()
        {
            let _ = this.decoding.reported.set(());
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

/// The decoder streaming an upload's body, if [`expand`] started one. A handler that reads a
/// body that does not decode answers for a failed read; Cloud Storage's front end answers
/// [`ApiError::undecodable_body`] instead, so [`Self::settle`] puts that answer in its place. A
/// handler that answered before it read the failure keeps its answer, so the response does not
/// depend on whether the decoder or the handler finished first.
pub(super) struct UploadDecoder(Option<Arc<Decoding>>);

impl UploadDecoder {
    pub(super) fn settle(self, response: Response<ResponseBody>) -> Response<ResponseBody> {
        if self
            .0
            .is_some_and(|decoding| decoding.reported.get().is_some())
        {
            return boxed(ApiError::undecodable_body().into_response());
        }
        response
    }
}

fn too_large() -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "uploadTooLarge",
        format!(
            "Compressed or expanded JSON request body exceeds {}",
            crate::bounded_body::RESOURCE_BODY_LIMIT_TEXT
        ),
    )
}

fn decoder_unavailable() -> ApiError {
    ApiError::service_unavailable("Request decoder is unavailable")
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
    members::Members::new(compressed)
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
    use std::{collections::VecDeque, convert::Infallible, io::Write, sync::Arc, time::Duration};

    type Polled = Poll<Option<Result<Frame<Bytes>, Infallible>>>;

    /// Answers each poll with the next scripted result.
    struct Scripted(VecDeque<Polled>);

    impl Body for Scripted {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Polled {
            self.0.pop_front().unwrap_or(Poll::Ready(None))
        }
    }

    fn client_paced(progress: &watch::Receiver<ObjectActivity>) -> bool {
        matches!(
            progress.borrow().step(),
            super::super::RequestStep::ClientPaced
        )
    }

    /// A slow write between frames is the server's work, so only a wait on the client counts as
    /// the client's silence, and every delivered frame still renews the limit.
    #[test]
    fn only_a_wait_on_the_client_counts_as_its_silence() {
        let frame = || Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"chunk")))));
        let (activity, mut progress) = watch::channel(ObjectActivity::default());
        let mut body = TransportBody {
            body: Scripted(VecDeque::from([frame(), frame(), Poll::Pending, frame()])),
            activity,
        };
        let mut cx = Context::from_waker(std::task::Waker::noop());
        for (poll, waits_on_client) in [(1, false), (2, false), (3, true), (4, false)] {
            let _ = Pin::new(&mut body).poll_frame(&mut cx);
            assert!(
                progress.has_changed().unwrap(),
                "poll {poll} restarts the watch's count"
            );
            progress.mark_unchanged();
            assert_eq!(client_paced(&progress), waits_on_client, "poll {poll}");
        }
    }

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
        let config = std::sync::Arc::new(Config::default());
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
            tokio::time::timeout(Duration::from_secs(1), expand(request(), ctx))
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
        let pending = expand(request(), ctx);
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

    fn gzip(bytes: &[u8]) -> Bytes {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        Bytes::from(encoder.finish().unwrap())
    }

    /// Cloud Storage's front end decodes a gzip body before it routes or authenticates the
    /// request (recorded 2026-11-25). Every caller's compressed body therefore waits for a
    /// decoder, while a request without a body answers without one; and a body that does not
    /// decode answers 400 to an anonymous caller, who gets 401 for the same body compressed well.
    #[tokio::test]
    async fn a_gzip_body_is_expanded_before_the_caller_is_checked() {
        let dir = crate::test_support::TestDir::new();
        let registry = crate::auth::Registry::parse(
            br#"{"identities":[
                {"id":"viewer@example.invalid","tokens":["viewer"],"project_roles":{"project-a":["roles/storage.objectViewer"]}}
            ]}"#,
        )
        .unwrap();
        let state = Arc::new(State::for_test(
            Catalog::open(&dir.0).await.unwrap(),
            Auth::strict(registry),
        ));
        let bucket = br#"{"name":"private-bucket"}"#;
        let record = crate::json::parse_insert(bucket, "project-a", std::time::UNIX_EPOCH).unwrap();
        state.catalog.insert_bucket(record).await.unwrap();
        let config = Arc::new(Config::default());
        let endpoints = config
            .runtime_endpoints
            .snapshot(&config, "http://localhost")
            .unwrap()
            .without_request();
        let (activity, _) = watch::channel(ObjectActivity::default());
        let ctx = RequestCtx {
            state: &state,
            config: &config,
            endpoints: &endpoints,
            activity: &activity,
            listener: crate::auth::ListenerRole::Public,
        };
        let request = |method: &str, uri: &str, token: Option<&str>, body: Bytes| {
            let mut builder = Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("content-encoding", "gzip");
            if let Some(token) = token {
                builder = builder.header("authorization", format!("Bearer {token}"));
            }
            builder
                .body(RequestBody::Buffered(Full::new(body)))
                .unwrap()
        };
        let respond = |request| super::super::routing::respond(request, ctx);
        let decoders = state.pools.gzip_decoders.available_permits();
        let held = (state.pools.gzip_decoders.clone())
            .acquire_many_owned(u32::try_from(decoders).unwrap())
            .await
            .unwrap();
        let (bucket, upload) = (
            "/storage/v1/b/private-bucket",
            "/upload/storage/v1/b/private-bucket/o",
        );
        let labels = gzip(br#"{"labels":{"k":"v"}}"#);
        for (method, uri) in [("GET", bucket), ("PATCH", bucket)] {
            let response = respond(request(method, uri, None, Bytes::new()));
            let response = tokio::time::timeout(Duration::from_secs(1), response)
                .await
                .unwrap_or_else(|_| panic!("{method} without a body waited for a decoder"));
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method}");
        }
        let media = format!("{upload}?uploadType=media&name=x");
        let not_gzip = respond(request(
            "POST",
            &media,
            None,
            Bytes::from_static(b"not gzip"),
        ));
        let not_gzip = tokio::time::timeout(Duration::from_secs(1), not_gzip)
            .await
            .expect("the first bytes of an upload are checked without a decoder");
        assert_eq!(not_gzip.status(), StatusCode::BAD_REQUEST);
        let initiation = format!("{upload}?uploadType=resumable&name=x");
        for (method, uri, token) in [
            ("PATCH", bucket, None),
            ("PATCH", bucket, Some("viewer")),
            ("POST", "/storage/v1/unknown", None),
            ("POST", initiation.as_str(), None),
        ] {
            let pending = respond(request(method, uri, token, labels.clone()));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), pending)
                    .await
                    .is_err(),
                "{method} {uri} {token:?} expands its body first"
            );
        }
        drop(held);
        let refused = respond(request("PATCH", bucket, None, labels)).await;
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
        let undecodable = respond(request(
            "PATCH",
            bucket,
            None,
            Bytes::from_static(b"not gzip"),
        ))
        .await;
        assert_eq!(undecodable.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            undecodable.headers()["content-type"],
            "text/plain; charset=utf-8"
        );
        let text = undecodable.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(text, "Could not decompress request body.");
        state.catalog.close().await.unwrap();
    }
}
