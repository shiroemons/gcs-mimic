use super::*;
use crate::{
    auth::Auth,
    config::Config,
    server::{ObjectActivity, RequestCtx, State},
    storage::Catalog,
};
use http_body_util::{BodyExt, Full};

#[tokio::test]
async fn xml_read_validators_wrap_routed_public_and_virtual_host_responses() {
    let dir = crate::test_support::TestDir::new();
    let state = std::sync::Arc::new(State::for_test(
        Catalog::open(&dir.0).await.unwrap(),
        Auth::permissive(),
    ));
    let config = Config {
        xml_virtual_host_domains: vec!["storage.invalid".into()],
        ..Config::default()
    };
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
    };
    for (method, uri, data) in [
        (
            "POST",
            "/storage/v1/b?project=local",
            br#"{"name":"bucket"}"#.as_slice(),
        ),
        (
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=media&name=object",
            b"body",
        ),
        (
            "PUT",
            "/storage/v1/b/bucket/iam",
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        ),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .body(Incoming::Buffered(Full::new(
                bytes::Bytes::copy_from_slice(data),
            )))
            .unwrap();
        let response = crate::server::routing::respond(request, ctx).await;
        assert_eq!(response.status(), StatusCode::OK);
    }
    let request = Request::builder()
        .uri("/bucket/object")
        .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
        .unwrap();
    let full = crate::server::routing::respond(request, ctx).await;
    assert_eq!(full.status(), StatusCode::OK);
    let etag = full.headers()["etag"].to_str().unwrap().to_owned();
    let modified = full.headers()["last-modified"].clone();
    for method in ["GET", "HEAD"] {
        for (host, path) in [
            ("localhost", "/bucket/object"),
            ("bucket.storage.invalid", "/object"),
        ] {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("host", host)
                .header("if-none-match", format!("W/{etag}"))
                .header("range", "bytes=999-")
                .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
                .unwrap();
            let response = crate::server::routing::respond(request, ctx).await;
            assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
            assert_eq!(response.headers()["etag"], etag);
            assert_eq!(response.headers()["cache-control"], "public, max-age=3600");
            assert_eq!(response.headers()["last-modified"], modified);
            assert!(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .is_empty()
            );
        }
    }
    let request = Request::builder()
        .uri("/bucket/object")
        .header("if-match", "\"other\"")
        .header("if-none-match", etag)
        .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
        .unwrap();
    assert_eq!(
        crate::server::routing::respond(request, ctx).await.status(),
        StatusCode::PRECONDITION_FAILED
    );
    let request = Request::builder()
        .uri("/bucket/object")
        .header("range", "bytes=0-0,2-2")
        .header("range", "bytes=1-1")
        .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
        .unwrap();
    let response = crate::server::routing::respond(request, ctx).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        b"body"[..]
    );
    state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn encrypted_head_validators_do_not_bypass_data_key_checks() {
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
    };
    let request = Request::builder()
        .method("POST")
        .uri("/storage/v1/b?project=local")
        .body(Incoming::Buffered(Full::new(bytes::Bytes::from_static(
            br#"{"name":"bucket"}"#,
        ))))
        .unwrap();
    assert_eq!(
        crate::server::routing::respond(request, ctx).await.status(),
        StatusCode::OK
    );
    let raw_key = [7; 32];
    let key = crate::base64::encode(&raw_key);
    let hash =
        crate::base64::encode(ring::digest::digest(&ring::digest::SHA256, &raw_key).as_ref());
    let request = Request::builder()
        .method("POST")
        .uri("/upload/storage/v1/b/bucket/o?uploadType=media&name=encrypted")
        .header("x-goog-encryption-algorithm", "AES256")
        .header("x-goog-encryption-key", key)
        .header("x-goog-encryption-key-sha256", hash)
        .body(Incoming::Buffered(Full::new(bytes::Bytes::from_static(
            b"body",
        ))))
        .unwrap();
    assert_eq!(
        crate::server::routing::respond(request, ctx).await.status(),
        StatusCode::OK
    );
    let request = Request::builder()
        .method("HEAD")
        .uri("/bucket/encrypted")
        .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
        .unwrap();
    let head = crate::server::routing::respond(request, ctx).await;
    assert_eq!(head.status(), StatusCode::OK);
    assert!(!head.headers().contains_key("x-goog-hash"));
    let etag = head.headers()["etag"].clone();
    for (method, status) in [
        ("HEAD", StatusCode::NOT_MODIFIED),
        ("GET", StatusCode::BAD_REQUEST),
    ] {
        let request = Request::builder()
            .method(method)
            .uri("/bucket/encrypted")
            .header("if-none-match", etag.clone())
            .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
            .unwrap();
        let response = crate::server::routing::respond(request, ctx).await;
        assert_eq!(response.status(), status);
        assert!(!response.headers().contains_key("x-goog-hash"));
        if status == StatusCode::NOT_MODIFIED {
            assert_eq!(response.headers()["etag"], etag);
            assert!(response.headers().contains_key("expires"));
            assert!(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .is_empty()
            );
        }
    }
    state.catalog.close().await.unwrap();
}
