//! The user-facing management routes are public contract: their response keys and the 64 KiB
//! request body they guarantee must not drift. These tests pin exactly that and nothing about
//! wording, which the release policy leaves free.
use super::*;
use std::collections::BTreeSet;

/// The guaranteed request body size of a management route that takes a body.
const GUARANTEED_BODY: usize = 64 * 1024;

fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("expected a JSON object: {value}"))
        .keys()
        .map(String::as_str)
        .collect()
}

/// A valid JSON body padded with whitespace to exactly the guaranteed size.
fn padded(body: &str) -> Vec<u8> {
    let mut padded = body.as_bytes().to_vec();
    padded.resize(GUARANTEED_BODY, b' ');
    padded
}

#[test]
fn probe_routes_keep_their_response_keys() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    for path in [
        "/_internal/health",
        "/_internal/ready",
        "/_internal/healthcheck",
    ] {
        let body = server.request("GET", path, "", b"").json_ok();
        assert_eq!(
            keys(&body),
            BTreeSet::from(["status", "unix_time"]),
            "{path}"
        );
        assert_eq!(body["status"], "ok", "{path}");
        assert!(body["unix_time"].is_u64(), "{path}: {body}");
    }
}

#[test]
fn config_and_reset_accept_the_guaranteed_body_size() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let config = padded(r#"{"externalUrl":"","publicHost":""}"#);
    server
        .request("PUT", "/_internal/config", ADMIN, &config)
        .expect(200);
    let reset = server
        .request(
            "POST",
            "/_internal/reset",
            ADMIN,
            &padded(r#"{"confirm":"gcs-mimic"}"#),
        )
        .json_ok();
    assert_eq!(
        keys(&reset),
        BTreeSet::from(["buckets", "objects", "cleanupPending"])
    );
}

#[test]
fn reseed_keeps_its_response_keys() {
    let (data, source) = (TestDir::new(), TestDir::new());
    std::fs::create_dir(source.0.join("test-bucket")).unwrap();
    std::fs::write(source.0.join("test-bucket/value"), b"seed").unwrap();
    let server = Server::start(
        &data,
        &[
            "--admin-enabled",
            "true",
            "--seed-dir",
            source.0.to_str().unwrap(),
        ],
    );
    let reseed = server
        .request("POST", "/_internal/reseed", ADMIN, b"")
        .json_ok();
    assert_eq!(
        keys(&reseed),
        BTreeSet::from([
            "outcome",
            "buckets",
            "objects",
            "replaced",
            "unchanged",
            "blocked",
            "skipped_os_metadata",
        ])
    );
}

/// The route answers the JSON API object resource, so it must equal what the JSON API returns
/// for the same object rather than any fixed list of keys.
#[test]
fn fixture_insertion_answers_the_json_api_object_resource() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let inserted = server
        .fixture(
            json!({"bucket":"test-bucket","name":"contract","contentType":"text/plain"}),
            b"fixture bytes",
            ADMIN,
        )
        .json_ok();
    let read = server
        .request("GET", "/storage/v1/b/test-bucket/o/contract", "", b"")
        .json_ok();
    assert_eq!(inserted["kind"], "storage#object");
    assert_eq!(inserted, read);
}
