//! HTTP framing and response-envelope contracts shared by the adapters.

use super::*;

#[test]
fn body_presence_compares_content_length_numerically() {
    for (value, expected) in [
        ("0", false),
        ("00", false),
        (" 00 ", false),
        ("01", true),
        ("invalid", true),
    ] {
        let mut headers = hyper::HeaderMap::new();
        headers.insert("content-length", value.parse().unwrap());
        assert_eq!(has_body(&headers), expected, "{value}");
        headers.insert("transfer-encoding", "chunked".parse().unwrap());
        assert!(has_body(&headers));
    }
    assert!(!has_body(&hyper::HeaderMap::new()));
}

/// Bodiless JSON methods compare the declared length as text, unlike `has_body`: they have
/// always refused `Content-Length: 00`. Accepting it is a separate decision that changes this.
#[test]
fn bodiless_json_methods_accept_only_a_literal_zero_length() {
    assert!(require_empty_body(&hyper::HeaderMap::new()).is_ok());
    for (name, value, accepted) in [
        ("content-length", "0", true),
        ("content-length", "00", false),
        ("content-length", "5", false),
        ("transfer-encoding", "chunked", false),
    ] {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(name, value.parse().unwrap());
        match require_empty_body(&headers) {
            Ok(()) => assert!(accepted, "{name}: {value}"),
            Err(error) => {
                assert!(!accepted, "{name}: {value}");
                assert_eq!(error.status, StatusCode::BAD_REQUEST);
                assert_eq!(error.message, "This method requires an empty body");
            }
        }
    }
}

#[test]
fn not_modified_response_tolerates_missing_representation_headers() {
    let response = not_modified_response(None, None);
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(!response.headers().contains_key("etag"));
    assert!(!response.headers().contains_key("cache-control"));
}

#[tokio::test]
async fn unknown_routes_use_json_error_envelopes_and_probes_accept_zero_lengths() {
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let state = State::for_test(catalog, Auth::permissive());
    let request = Request::builder()
        .uri("/unknown")
        .body(RequestBody::Buffered(Full::new(Bytes::new())))
        .unwrap();
    assert!(probe(&request, &state).is_none());
    let response = unknown_route().into_response();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response.headers()["content-type"],
        "application/json; charset=UTF-8"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let error: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error["error"]["code"], 404);
    assert_eq!(error["error"]["errors"][0]["reason"], "notFound");
    let request = Request::builder()
        .uri("/_internal/health")
        .header("content-length", "00")
        .body(RequestBody::Buffered(Full::new(Bytes::new())))
        .unwrap();
    assert_eq!(probe(&request, &state).unwrap().status(), StatusCode::OK);
    state.catalog.close().await.unwrap();
}
