//! The XML object read through the routes, after objects created through the JSON API.

use crate::service::body::RequestBody as Incoming;
use crate::{
    auth::Auth,
    config::{Config, VirtualHostDomains},
    service::{activity::ObjectActivity, context::RequestCtx, state::State},
    storage::Catalog,
};
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};

#[tokio::test]
async fn xml_read_validators_wrap_routed_public_and_virtual_host_responses() {
    let dir = crate::test_support::TestDir::new();
    let state = std::sync::Arc::new(State::for_test(
        Catalog::open(&dir.0).await.unwrap(),
        Auth::permissive(),
    ));
    let config = std::sync::Arc::new(
        crate::config::ConfigDraft {
            xml_virtual_host_domains: VirtualHostDomains::of(&["storage.invalid"]),
            ..Default::default()
        }
        .build(),
    );
    let endpoints = config
        .runtime_endpoints()
        .snapshot(&config, "http://localhost")
        .unwrap()
        .without_request();
    let (activity, _) = tokio::sync::watch::channel(ObjectActivity::default());
    let ctx = RequestCtx {
        uri: &crate::percent::EMPTY_REQUEST_URI,
        state: &state,
        config: &config,
        endpoints: &endpoints,
        activity: &activity,
        listener: crate::auth::ListenerRole::Public,
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
    let config = std::sync::Arc::new(Config::default());
    let endpoints = config
        .runtime_endpoints()
        .snapshot(&config, "http://localhost")
        .unwrap()
        .without_request();
    let (activity, _) = tokio::sync::watch::channel(ObjectActivity::default());
    let ctx = RequestCtx {
        uri: &crate::percent::EMPTY_REQUEST_URI,
        state: &state,
        config: &config,
        endpoints: &endpoints,
        activity: &activity,
        listener: crate::auth::ListenerRole::Public,
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
    let keyed = [
        ("x-goog-encryption-algorithm", "AES256".to_owned()),
        ("x-goog-encryption-key", crate::base64::encode(&raw_key)),
        (
            "x-goog-encryption-key-sha256",
            crate::base64::encode(ring::digest::digest(&ring::digest::SHA256, &raw_key).as_ref()),
        ),
    ];
    let respond = |method: &str, uri: &str, headers: &[(&str, String)], body: &'static [u8]| {
        let request = headers
            .iter()
            .fold(
                Request::builder().method(method).uri(uri),
                |builder, (name, value)| builder.header(*name, value),
            )
            .body(Incoming::Buffered(Full::new(bytes::Bytes::from_static(
                body,
            ))))
            .unwrap();
        crate::server::routing::respond(request, ctx)
    };
    let upload = "/upload/storage/v1/b/bucket/o?uploadType=media&name=encrypted";
    assert_eq!(
        respond("POST", upload, &keyed, b"body").await.status(),
        StatusCode::OK
    );
    let object = "/bucket/encrypted";
    let head = respond("HEAD", object, &keyed, b"").await;
    assert_eq!(head.status(), StatusCode::OK);
    let etag = head.headers()["etag"].to_str().unwrap().to_owned();
    // Without the key the XML API refuses HEAD as it refuses GET (recorded 2026-11-17, case
    // xml-gaps-objects), and naming the current validator does not get past that check.
    for validator in [None, Some(("if-none-match", etag))] {
        for method in ["HEAD", "GET"] {
            let response = respond(method, object, validator.as_slice(), b"").await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{method} {validator:?}"
            );
            assert!(!response.headers().contains_key("x-goog-hash"));
            assert!(!response.headers().contains_key("etag"));
        }
    }
    state.catalog.close().await.unwrap();
}
