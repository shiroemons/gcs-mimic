//! Transport input normalization before API routing and authentication: the JSON method override,
//! and the transport gzip that Cloud Storage's front end removes before it routes a JSON API
//! request.

use super::{RESOURCE_BODY_LIMIT, RequestCtx};
use crate::service::body::{Decoding, RequestBody, ResponseBody, boxed};
use crate::{auth::Caller, json::ApiError, json::JsonErrorResponse};
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode,
    body::{Body, Bytes},
    header::{CONTENT_ENCODING, CONTENT_LENGTH, HeaderValue, TRANSFER_ENCODING},
};
use std::{
    io::{self, Read},
    sync::Arc,
};
use tokio::sync::Semaphore;

mod members;
mod upload_gzip;

fn bad_request(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "badRequest", message)
}

pub(super) fn override_method(request: &mut Request<RequestBody>) -> Result<(), ApiError> {
    let Some(value) = crate::http_header::sole(request.headers(), "x-http-method-override")
        .value()
        .map_err(|_| {
            bad_request("Use one X-HTTP-Method-Override: PATCH header on a JSON API POST")
        })?
    else {
        return Ok(());
    };
    if value != "PATCH" || request.method() != Method::POST {
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
    fn of(
        request: &Request<RequestBody>,
        uri: &crate::percent::RequestUri,
    ) -> Result<Option<Self>, ApiError> {
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
        let query = uri.parameters()?;
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
    match Coded::of(&request, ctx.uri)? {
        None => Ok((request, UploadDecoder(None))),
        Some(Coded::Upload) => upload_gzip::request(request, ctx).await,
        Some(Coded::Metadata) => Ok((expand_metadata(request, ctx).await?, UploadDecoder(None))),
    }
}

fn has_single_gzip_coding(headers: &HeaderMap) -> bool {
    crate::http_header::sole(headers, CONTENT_ENCODING)
        .value()
        .ok()
        .flatten()
        .is_some_and(|encoding| encoding.as_bytes().eq_ignore_ascii_case(b"gzip"))
}

async fn expand_metadata(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Request<RequestBody>, ApiError> {
    if !has_single_gzip_coding(request.headers()) {
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
    let permit = decoders_for(&parts, ctx)
        .await
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

/// The decoder permits that the compressed body of `parts` takes. The body is expanded before its
/// caller is authorized, so a caller who does not authenticate, anonymous on the public listener
/// or with credentials that fail, shares one permit of its own and cannot hold back a caller who
/// does. Authentication reads only the headers and the query, and the handler authenticates the
/// request again, so the answer does not change.
async fn decoders_for(parts: &hyper::http::request::Parts, ctx: RequestCtx<'_>) -> Arc<Semaphore> {
    let pools = &ctx.state.pools;
    match ctx.authenticate(&parts.headers, parts.uri.query()).await {
        Ok(None | Some(Caller::Principal(_))) => Arc::clone(&pools.gzip_decoders),
        Ok(Some(Caller::Anonymous)) | Err(_) => Arc::clone(&pools.unauthenticated_gzip_decoders),
    }
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

/// The decoder streaming an upload's body, if [`expand`] started one. A handler that reads a
/// body that does not decode answers for a failed read; Cloud Storage's front end answers
/// [`ApiError::undecodable_body`] instead, so [`Self::settle`] puts that answer in its place. A
/// handler that answered before it read the failure keeps its answer, so the response does not
/// depend on whether the decoder or the handler finished first.
pub(super) struct UploadDecoder(Option<Arc<Decoding>>);

impl UploadDecoder {
    pub(super) fn settle(self, response: Response<ResponseBody>) -> Response<ResponseBody> {
        if self.0.is_some_and(|decoding| decoding.is_reported()) {
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

#[derive(Debug, thiserror::Error)]
enum DecodeError {
    #[error("Invalid gzip input")]
    Invalid(#[source] io::Error),
    #[error("Expanded input exceeds its limit")]
    Limit,
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
        auth::Auth, config::Config, server::State, service::activity::ObjectActivity,
        storage::Catalog,
    };
    use std::{io::Write, sync::Arc, time::Duration};
    use tokio::sync::watch;

    /// Bounds an answer that must not wait for a decoder permit. A wait for a held permit never
    /// ends, while a loaded runner can take seconds over an answer that waits for none, so only a
    /// limit far beyond that tells the two apart.
    const HANG_GUARD: Duration = Duration::from_secs(20);

    #[tokio::test]
    async fn gzip_metadata_admission_uses_the_server_pool() {
        let fixture = Fixture::new(Auth::permissive()).await;
        let (state, ctx) = (&fixture.state, fixture.ctx());
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
            tokio::time::timeout(HANG_GUARD, expand(request(), ctx))
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
            tokio::time::timeout(HANG_GUARD, pending)
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

    /// One server's state and the context its public listener gives a request.
    struct Fixture {
        _dir: crate::test_support::TestDir,
        state: Arc<State>,
        config: Arc<Config>,
        endpoints: crate::config::RequestEndpoints,
        activity: watch::Sender<ObjectActivity>,
    }

    impl Fixture {
        async fn new(auth: Auth) -> Self {
            let dir = crate::test_support::TestDir::new();
            let state = Arc::new(State::for_test(Catalog::open(&dir.0).await.unwrap(), auth));
            let config = Arc::new(Config::default());
            let endpoints = config
                .runtime_endpoints()
                .snapshot(&config, "http://localhost")
                .unwrap()
                .without_request();
            let (activity, _) = watch::channel(ObjectActivity::default());
            Self {
                _dir: dir,
                state,
                config,
                endpoints,
                activity,
            }
        }

        /// A strict server whose token `viewer` reads objects of `project-a`, which owns
        /// `private-bucket`.
        async fn strict() -> Self {
            let registry = crate::auth::Registry::parse(
                br#"{"identities":[
                    {"id":"viewer@example.invalid","tokens":["viewer"],"project_roles":{"project-a":["roles/storage.objectViewer"]}}
                ]}"#,
            )
            .unwrap();
            let fixture = Self::new(Auth::strict(registry)).await;
            let record = crate::storage::test_fixtures::bucket("private-bucket", "project-a");
            fixture.state.catalog.insert_bucket(record).await.unwrap();
            fixture
        }

        fn ctx(&self) -> RequestCtx<'_> {
            RequestCtx {
                uri: &crate::percent::EMPTY_REQUEST_URI,
                state: &self.state,
                config: &self.config,
                endpoints: &self.endpoints,
                activity: &self.activity,
                listener: crate::auth::ListenerRole::Public,
            }
        }
    }

    /// Takes every permit `pool` has left.
    async fn hold(pool: &Arc<Semaphore>) -> tokio::sync::OwnedSemaphorePermit {
        let permits = u32::try_from(pool.available_permits()).unwrap();
        Arc::clone(pool).acquire_many_owned(permits).await.unwrap()
    }

    /// A JSON request whose body is sent with `Content-Encoding: gzip`, from the caller of
    /// `token` or from an anonymous one.
    fn coded(method: &str, uri: &str, token: Option<&str>, body: Bytes) -> Request<RequestBody> {
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
    }

    /// Cloud Storage's front end decodes a gzip body before it routes or authenticates the
    /// request (recorded 2026-11-25). Every caller's compressed body therefore waits for a
    /// decoder, while a request without a body answers without one; and a body that does not
    /// decode answers 400 to an anonymous caller, who gets 401 for the same body compressed well.
    #[tokio::test]
    async fn a_gzip_body_is_expanded_before_the_caller_is_checked() {
        let fixture = Fixture::strict().await;
        let respond = |request| super::super::routing::respond(request, fixture.ctx());
        let pools = &fixture.state.pools;
        let held = (
            hold(&pools.gzip_decoders).await,
            hold(&pools.unauthenticated_gzip_decoders).await,
        );
        let (bucket, upload) = (
            "/storage/v1/b/private-bucket",
            "/upload/storage/v1/b/private-bucket/o",
        );
        let labels = gzip(br#"{"labels":{"k":"v"}}"#);
        for (method, uri) in [("GET", bucket), ("PATCH", bucket)] {
            let response = respond(coded(method, uri, None, Bytes::new()));
            let response = tokio::time::timeout(HANG_GUARD, response)
                .await
                .unwrap_or_else(|_| panic!("{method} without a body waited for a decoder"));
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method}");
        }
        let media = format!("{upload}?uploadType=media&name=x");
        let not_gzip = respond(coded("POST", &media, None, Bytes::from_static(b"not gzip")));
        let not_gzip = tokio::time::timeout(HANG_GUARD, not_gzip)
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
            let pending = respond(coded(method, uri, token, labels.clone()));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), pending)
                    .await
                    .is_err(),
                "{method} {uri} {token:?} expands its body first"
            );
        }
        drop(held);
        let refused = respond(coded("PATCH", bucket, None, labels)).await;
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
        let undecodable = respond(coded(
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
        fixture.state.catalog.close().await.unwrap();
    }

    /// A compressed body is expanded before its caller is authorized, so the callers who do not
    /// authenticate share a decoder permit of their own. Holding it holds back none of the callers
    /// who authenticate, and holding every other permit still leaves them their answer.
    #[tokio::test]
    async fn an_unauthenticated_gzip_body_cannot_hold_back_an_authenticated_one() {
        let fixture = Fixture::strict().await;
        let respond = |request| super::super::routing::respond(request, fixture.ctx());
        let pools = &fixture.state.pools;
        let bucket = "/storage/v1/b/private-bucket";
        let labels = gzip(br#"{"labels":{"k":"v"}}"#);
        let unauthenticated = hold(&pools.unauthenticated_gzip_decoders).await;
        for token in [None, Some("unknown")] {
            let pending = respond(coded("PATCH", bucket, token, labels.clone()));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), pending)
                    .await
                    .is_err(),
                "{token:?} waits for the unauthenticated permit"
            );
        }
        let viewer = respond(coded("PATCH", bucket, Some("viewer"), labels.clone()));
        let viewer = tokio::time::timeout(HANG_GUARD, viewer)
            .await
            .expect("a caller who authenticates does not wait for the unauthenticated permit");
        assert_eq!(viewer.status(), StatusCode::FORBIDDEN);
        drop(unauthenticated);
        let authenticated = hold(&pools.gzip_decoders).await;
        let pending = respond(coded("PATCH", bucket, Some("viewer"), labels.clone()));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), pending)
                .await
                .is_err(),
            "the viewer waits for an authenticated permit"
        );
        for (token, body, expected) in [
            (None, labels.clone(), StatusCode::UNAUTHORIZED),
            (Some("unknown"), labels, StatusCode::UNAUTHORIZED),
            (
                None,
                Bytes::from_static(b"not gzip"),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let response = respond(coded("PATCH", bucket, token, body));
            let response = tokio::time::timeout(HANG_GUARD, response)
                .await
                .unwrap_or_else(|_| panic!("{token:?} waited for an authenticated permit"));
            assert_eq!(response.status(), expected, "{token:?}");
        }
        drop(authenticated);
        fixture.state.catalog.close().await.unwrap();
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = DecodeError::Limit;
    assert_eq!(error.to_string(), "Expanded input exceeds its limit");
    assert!(std::error::Error::source(&error).is_none());
    let invalid = decode(b"not gzip", 256).unwrap_err();
    assert_eq!(invalid.to_string(), "Invalid gzip input");
    assert!(
        std::error::Error::source(&invalid)
            .unwrap()
            .is::<io::Error>()
    );
    crate::error::assert_distinct_causes(&invalid);
}
