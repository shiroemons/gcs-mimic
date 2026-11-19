//! HTTP framing and response-envelope contracts shared by the adapters.

use super::*;
use crate::json::JsonErrorResponse;

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

/// Bodiless JSON methods read the declared length as a number, as Cloud Storage accepted
/// `Content-Length: 00` on them.
#[test]
fn bodiless_json_methods_accept_any_spelling_of_a_zero_length() {
    assert!(require_empty_body(&hyper::HeaderMap::new()).is_ok());
    for (name, value, accepted) in [
        ("content-length", "0", true),
        ("content-length", "00", true),
        ("content-length", "000", true),
        ("content-length", "5", false),
        ("content-length", "05", false),
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

fn probe_request(path: &str) -> Request<RequestBody> {
    Request::builder()
        .uri(path)
        .body(RequestBody::Buffered(Full::new(Bytes::new())))
        .unwrap()
}

async fn probe_body(path: &str, state: &State) -> (StatusCode, serde_json::Value) {
    let response = probe(&probe_request(path), state).unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

/// Readiness reports the mirror's state beside the API's own, read from memory: a stopped mirror
/// leaves the API ready, and a catalog whose every admission slot is taken cannot delay it.
#[tokio::test]
async fn readiness_reports_the_mirror_state_without_a_catalog_query() {
    use crate::sync::{MirrorState, MirrorStatus};
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let mut state = State::for_test(catalog, Auth::permissive());
    let (status, ready) = probe_body("/_internal/ready", &state).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ready.get("sync").is_none(), "{ready}");
    for mirror in [
        MirrorState::Starting,
        MirrorState::Running,
        MirrorState::Retrying,
        MirrorState::Paused,
        MirrorState::Stopped,
    ] {
        state.sync = Some(MirrorStatus::in_state(mirror));
        let held = state.catalog.hold_admission();
        for path in ["/_internal/ready", "/_internal/healthcheck"] {
            let (status, body) = probe_body(path, &state).await;
            assert_eq!(status, StatusCode::OK, "{path} {mirror}");
            assert_eq!(body["status"], "ok");
            assert!(body["unix_time"].is_u64());
            assert_eq!(body["sync"], serde_json::json!({"state": mirror.as_str()}));
        }
        let (_, health) = probe_body("/_internal/health", &state).await;
        assert!(health.get("sync").is_none(), "{health}");
        assert_eq!(state.catalog.admission_waiters(), 0, "no probe queued");
        drop(held);
    }
    state.catalog.close().await.unwrap();
}

/// The API's own failures still turn readiness off, whatever the mirror reports.
#[tokio::test]
async fn failing_maintenance_still_turns_readiness_off_beside_a_running_mirror() {
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let state = State {
        sync: Some(crate::sync::MirrorStatus::in_state(
            crate::sync::MirrorState::Running,
        )),
        ..State::for_test(catalog.clone(), Auth::permissive())
    };
    catalog.inject_maintenance_panics(usize::MAX);
    catalog.start_resumable_maintenance();
    crate::test_support::eventually_async("readiness to turn off", || async { !state.is_ready() })
        .await;
    for path in ["/_internal/ready", "/_internal/healthcheck"] {
        let (status, body) = probe_body(path, &state).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(body, serde_json::json!({"status": "not ready"}));
    }
    drop(state);
    catalog.close().await.unwrap();
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
