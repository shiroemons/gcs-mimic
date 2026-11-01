//! Response headers shared by every JSON API metadata response.
//!
//! Cloud Storage sends the same caching, `Expires`, `Pragma` and `Vary` fields with every JSON
//! API response, chosen only by the request method and whether the call succeeded (observed
//! 2026-10-31 against the recorded reference). Applying them once after routing keeps each
//! handler from re-deriving them and keeps errors, mutations and reads consistent. Media
//! downloads and the XML API carry their own headers and are left alone.

use super::{
    Decoding, NO_STORE_CACHE_CONTROL, decode, error::MEDIA_ERROR_CACHE_CONTROL, http_date,
};
use hyper::{
    Method, Response, StatusCode,
    header::{self, HeaderValue},
};
use serde_json::Value;
use std::time::SystemTime;

/// The `Cache-Control` of a successful JSON API read.
pub(crate) const RESOURCE_CACHE_CONTROL: &str = "private, max-age=0, must-revalidate, no-transform";

/// An `Expires` in the past, sent with every response that must not be cached.
const EXPIRED: &str = "Mon, 01 Jan 1990 00:00:00 GMT";

const VARY: &str = "Origin, X-Origin";

/// Whether a request path belongs to the JSON API (metadata, uploads and resumable sessions).
fn is_json_api(path: &str) -> bool {
    path.starts_with("/storage/v1/") || path.starts_with("/upload/storage/v1/")
}

/// The `ETag` header of a resource response: the resource's own `etag`, unquoted as the body has it.
pub(crate) fn etag_header(resource: &Value) -> Option<HeaderValue> {
    resource
        .get("etag")
        .and_then(Value::as_str)
        .and_then(|etag| HeaderValue::from_str(etag).ok())
}

/// What a JSON API path call asks for: an object download keeps its own media headers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Call {
    Metadata,
    MediaRead,
    /// An upload call the upload front end handles before the JSON API sees it (a session
    /// continuation, an unknown `uploadType`, a multipart request without a boundary): its
    /// bare `text/plain` replies carry no cache fields and no `Vary`.
    UploadFrontEnd,
}

impl Call {
    pub(crate) fn of_request<B>(request: &hyper::Request<B>) -> Self {
        let query = request.uri().query().unwrap_or_default();
        // Names and values are compared decoded, because `alt=%6Dedia` is `alt=media` to the
        // handlers; a malformed escape never matches and the handler reports it.
        let param = |name: &str| {
            query.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                (decode(key, Decoding::Form).ok()? == name)
                    .then(|| decode(value, Decoding::Form).ok())
                    .flatten()
            })
        };
        if request.uri().path().starts_with("/upload/") {
            let boundary_missing = param("uploadType").as_deref() == Some("multipart")
                && !request
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.contains("boundary="));
            let unknown_type = param("uploadType")
                .is_some_and(|kind| !matches!(kind.as_str(), "media" | "multipart" | "resumable"));
            if param("upload_id").or_else(|| param("uploadId")).is_some()
                || boundary_missing
                || unknown_type
            {
                return Self::UploadFrontEnd;
            }
        }
        if param("alt").as_deref() == Some("media") {
            Self::MediaRead
        } else {
            Self::Metadata
        }
    }
}

/// Marks a response as one Cloud Storage forbids every cache for: `Cache-Control`, `Expires`
/// in the past and `Pragma`. Mutations, errors and the resumable initiation reply share it.
pub(crate) fn forbid_caching(headers: &mut header::HeaderMap) {
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(NO_STORE_CACHE_CONTROL),
    );
    headers.insert(header::EXPIRES, HeaderValue::from_static(EXPIRED));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

/// The `Expires` that goes with a media response's `Cache-Control`: Cloud Storage derives it
/// from the cache policy, so a download that forbids caching expires in the past and a plain
/// media failure (`private, max-age=0`) expires at the response time (recorded for 200, 206,
/// 304, 412, 404, 416 and 501 media reads). A public object's own `Cache-Control` has no
/// recorded `Expires`, so it gets none.
fn media_expires(headers: &mut header::HeaderMap, now: SystemTime) {
    if headers.contains_key(header::EXPIRES) {
        return;
    }
    let policy = headers
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok());
    if policy.is_some_and(|value| value.starts_with(NO_STORE_CACHE_CONTROL)) {
        headers.insert(header::EXPIRES, HeaderValue::from_static(EXPIRED));
    } else if policy == Some(MEDIA_ERROR_CACHE_CONTROL)
        && let Some(expires) = http_date(now)
            .ok()
            .and_then(|date| HeaderValue::from_str(&date).ok())
    {
        headers.insert(header::EXPIRES, expires);
    }
}

/// Adds the shared JSON API fields to a routed response.
///
/// A `MediaRead` keeps its own headers and gains only the `Expires` its cache policy implies.
/// A response is a JSON API response when it is JSON (or the JSON error envelope the upload
/// front end labels `text/html`), or an empty `204`/`304` for a non-media call.
pub(crate) fn apply<B>(
    method: &Method,
    path: &str,
    call: Call,
    now: SystemTime,
    response: &mut Response<B>,
) {
    if !is_json_api(path) {
        return;
    }
    if call == Call::MediaRead {
        media_expires(response.headers_mut(), now);
        return;
    }
    let status = response.status();
    let headers = response.headers_mut();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    // The upload front end answers its own protocol failures with a bare `text/plain` page:
    // none of the JSON API fields apply, and the CORS `Vary` added while routing is not
    // sent either (recorded for session status, range, unknown-session and `uploadType`
    // failures).
    if call == Call::UploadFrontEnd
        && content_type.is_some_and(|value| value.starts_with("text/plain"))
    {
        headers.remove(header::VARY);
        return;
    }
    // A JSON error the upload front end labelled `text/html` is still a JSON API response.
    let json = content_type.is_some_and(|value| {
        value.starts_with("application/json")
            || (value.starts_with("text/html") && status.is_client_error())
    });
    let empty = !headers.contains_key(header::CONTENT_TYPE)
        && matches!(status, StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED);
    if !json && !empty {
        return;
    }
    if empty {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    let read = matches!(*method, Method::GET | Method::HEAD) && status.is_success()
        || status == StatusCode::NOT_MODIFIED && matches!(*method, Method::GET | Method::HEAD);
    if read {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(RESOURCE_CACHE_CONTROL),
        );
        // `Expires` is the response time, as a cache that revalidates every use sees it.
        if let Some(expires) = http_date(now)
            .ok()
            .and_then(|date| HeaderValue::from_str(&date).ok())
        {
            headers.insert(header::EXPIRES, expires);
        }
        headers.remove(header::PRAGMA);
    } else {
        forbid_caching(headers);
    }
    if !headers.contains_key(header::VARY) {
        headers.insert(header::VARY, HeaderValue::from_static(VARY));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn json_response(status: StatusCode) -> Response<()> {
        let mut response = Response::new(());
        *response.status_mut() = status;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=UTF-8"),
        );
        response
    }

    fn header<'a>(response: &'a Response<()>, name: &str) -> Option<&'a str> {
        response.headers().get(name).and_then(|v| v.to_str().ok())
    }

    #[test]
    fn reads_are_revalidated_and_expire_now() {
        let mut response = json_response(StatusCode::OK);
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        apply(
            &Method::GET,
            "/storage/v1/b/x",
            Call::Metadata,
            now,
            &mut response,
        );
        assert_eq!(
            header(&response, "cache-control"),
            Some(RESOURCE_CACHE_CONTROL)
        );
        assert_eq!(
            header(&response, "expires"),
            Some("Tue, 14 Nov 2023 22:13:20 GMT")
        );
        assert_eq!(header(&response, "pragma"), None);
        assert_eq!(header(&response, "vary"), Some(VARY));
    }

    #[test]
    fn mutations_and_errors_forbid_caching() {
        for (method, status) in [
            (Method::POST, StatusCode::OK),
            (Method::DELETE, StatusCode::NO_CONTENT),
            (Method::GET, StatusCode::NOT_FOUND),
        ] {
            let mut response = json_response(status);
            apply(
                &method,
                "/storage/v1/b/x",
                Call::Metadata,
                UNIX_EPOCH,
                &mut response,
            );
            assert_eq!(
                header(&response, "cache-control"),
                Some(NO_STORE_CACHE_CONTROL)
            );
            assert_eq!(header(&response, "expires"), Some(EXPIRED));
            assert_eq!(header(&response, "pragma"), Some("no-cache"));
        }
    }

    #[test]
    fn media_xml_and_non_json_responses_are_untouched() {
        let mut media = json_response(StatusCode::OK);
        apply(
            &Method::GET,
            "/storage/v1/b/x/o/y",
            Call::MediaRead,
            UNIX_EPOCH,
            &mut media,
        );
        assert_eq!(header(&media, "cache-control"), None);
        let mut xml = json_response(StatusCode::OK);
        apply(
            &Method::GET,
            "/bucket/object",
            Call::Metadata,
            UNIX_EPOCH,
            &mut xml,
        );
        assert_eq!(header(&xml, "vary"), None);
        let mut text = Response::new(());
        text.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=UTF-8"),
        );
        apply(
            &Method::GET,
            "/storage/v1/b/x",
            Call::Metadata,
            UNIX_EPOCH,
            &mut text,
        );
        assert_eq!(header(&text, "cache-control"), None);
    }

    #[test]
    fn an_existing_vary_is_kept_and_empty_replies_become_json() {
        let mut response = Response::new(());
        *response.status_mut() = StatusCode::NO_CONTENT;
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
        apply(
            &Method::DELETE,
            "/storage/v1/b/x",
            Call::Metadata,
            UNIX_EPOCH,
            &mut response,
        );
        assert_eq!(header(&response, "vary"), Some("Origin"));
        assert_eq!(header(&response, "content-type"), Some("application/json"));
    }

    #[test]
    fn head_reads_and_not_modified_replies_follow_the_read_rule() {
        for (method, status) in [
            (Method::HEAD, StatusCode::OK),
            (Method::GET, StatusCode::NOT_MODIFIED),
        ] {
            let mut response = json_response(status);
            apply(
                &method,
                "/storage/v1/b/x",
                Call::Metadata,
                UNIX_EPOCH,
                &mut response,
            );
            assert_eq!(
                header(&response, "cache-control"),
                Some(RESOURCE_CACHE_CONTROL)
            );
            assert_eq!(header(&response, "pragma"), None);
        }
    }

    fn text_response(status: StatusCode, content_type: &'static str) -> Response<()> {
        let mut response = Response::new(());
        *response.status_mut() = status;
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        headers.insert(header::VARY, HeaderValue::from_static(VARY));
        response
    }

    #[test]
    fn upload_front_end_text_replies_carry_no_cache_fields_or_vary() {
        for status in [StatusCode::PERMANENT_REDIRECT, StatusCode::BAD_REQUEST] {
            let mut reply = text_response(status, "text/plain; charset=utf-8");
            apply(
                &Method::PUT,
                "/upload/storage/v1/b/x/o",
                Call::UploadFrontEnd,
                UNIX_EPOCH,
                &mut reply,
            );
            for name in ["cache-control", "expires", "pragma", "vary"] {
                assert_eq!(header(&reply, name), None, "{name} on {status}");
            }
        }
    }

    #[test]
    fn upload_front_end_json_replies_get_the_json_api_fields() {
        for status in [StatusCode::OK, StatusCode::BAD_REQUEST] {
            let mut reply = json_response(status);
            apply(
                &Method::PUT,
                "/upload/storage/v1/b/x/o",
                Call::UploadFrontEnd,
                UNIX_EPOCH,
                &mut reply,
            );
            assert_eq!(
                header(&reply, "cache-control"),
                Some(NO_STORE_CACHE_CONTROL)
            );
            assert_eq!(header(&reply, "expires"), Some(EXPIRED));
            assert_eq!(header(&reply, "pragma"), Some("no-cache"));
            assert_eq!(header(&reply, "vary"), Some(VARY));
        }
    }

    #[test]
    fn html_labelled_upload_errors_expire_in_the_past() {
        let mut reply = text_response(StatusCode::BAD_REQUEST, "text/html; charset=UTF-8");
        reply.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(NO_STORE_CACHE_CONTROL),
        );
        apply(
            &Method::POST,
            "/upload/storage/v1/b/x/o",
            Call::Metadata,
            UNIX_EPOCH,
            &mut reply,
        );
        assert_eq!(header(&reply, "expires"), Some(EXPIRED));
        assert_eq!(header(&reply, "pragma"), Some("no-cache"));
    }

    #[test]
    fn media_expires_follows_the_cache_policy() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for (policy, expected) in [
            (NO_STORE_CACHE_CONTROL, Some(EXPIRED)),
            (
                "no-cache, no-store, max-age=0, must-revalidate, no-transform",
                Some(EXPIRED),
            ),
            (
                MEDIA_ERROR_CACHE_CONTROL,
                Some("Tue, 14 Nov 2023 22:13:20 GMT"),
            ),
            ("public, max-age=3600", None),
        ] {
            let mut reply = json_response(StatusCode::OK);
            reply.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_str(policy).unwrap(),
            );
            apply(
                &Method::GET,
                "/storage/v1/b/x/o/y",
                Call::MediaRead,
                now,
                &mut reply,
            );
            assert_eq!(header(&reply, "expires"), expected, "{policy}");
        }
    }

    #[test]
    fn the_call_is_read_from_the_decoded_query() {
        let call = |uri: &str| Call::of_request(&hyper::Request::get(uri).body(()).unwrap());
        assert!(call("/storage/v1/b/x/o/y?alt=%6Dedia") == Call::MediaRead);
        assert!(call("/storage/v1/b/x/o/y?%61lt=media") == Call::MediaRead);
        assert!(call("/storage/v1/b/x/o/y?alt=json") == Call::Metadata);
        assert!(call("/upload/storage/v1/b/x/o?uploadType=%62ogus") == Call::UploadFrontEnd);
    }

    #[test]
    fn the_etag_header_is_the_resource_etag() {
        assert_eq!(
            etag_header(&serde_json::json!({"etag": "CAE="})),
            Some(HeaderValue::from_static("CAE="))
        );
        assert_eq!(etag_header(&serde_json::json!({"items": []})), None);
    }
}
