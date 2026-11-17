//! Entity-tag headers on collection responses. A listing without an entity tag ignores them:
//! Cloud Storage listed the objects with `If-None-Match: *` and with a bogus `If-Match` (case
//! obj-collection-validators), and the notification configurations and HMAC keys likewise (case
//! bkt-probe-collection-validators). The bucket, default object and object ACL listings carry
//! their owner's entity tag, and Cloud Storage evaluated both headers against it (case
//! bkt-probe-collection-validators).

use crate::common;
use common::TestDir;
use common::effects::Server;
use serde_json::json;

const AUTH: &str = "Authorization: Bearer admin\r\n";

#[test]
fn collection_listings_ignore_entity_tag_headers() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("listed", b"hello").json_ok();
    for path in [
        "/storage/v1/b/test-bucket/o?prefix=listed",
        "/storage/v1/b?project=local",
        "/storage/v1/b/test-bucket/notificationConfigs",
        "/storage/v1/projects/local/hmacKeys",
    ] {
        let plain = server.request("GET", path, AUTH, b"");
        assert_eq!(plain.headers.get("etag"), None, "{path}");
        for condition in ["If-None-Match: *\r\n", "If-Match: \"bogus\"\r\n"] {
            let reply = server
                .request("GET", path, &format!("{AUTH}{condition}"), b"")
                .json_ok();
            assert_eq!(reply, plain.json_ok(), "{path} {condition}");
        }
    }
    // A method without an entity tag that is not a listing keeps rejecting the headers.
    server
        .request(
            "DELETE",
            "/storage/v1/b/test-bucket/o/listed",
            &format!("{AUTH}If-Match: \"bogus\"\r\n"),
            b"",
        )
        .json_reason(400, "badRequest");
    server.get("listed").json_ok();
}

#[test]
fn acl_listings_evaluate_entity_tag_headers_against_their_owner() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("listed", b"hello").json_ok();
    for path in [
        "/storage/v1/b/test-bucket/acl",
        "/storage/v1/b/test-bucket/defaultObjectAcl",
        "/storage/v1/b/test-bucket/o/listed/acl",
    ] {
        let plain = server.request("GET", path, AUTH, b"");
        let listing = plain.json_ok();
        let etag = plain.headers["etag"].clone();
        let request =
            |condition: &str| server.request("GET", path, &format!("{AUTH}{condition}\r\n"), b"");
        for condition in [
            "If-None-Match: *".to_owned(),
            format!("If-None-Match: {etag}"),
        ] {
            let reply = request(&condition);
            reply.expect_with(304, format_args!("{path} {condition}"));
            assert!(reply.body.is_empty(), "{path} {condition}");
            assert_eq!(reply.headers.get("etag"), Some(&etag), "{path} {condition}");
        }
        for condition in [
            format!("If-Match: {etag}"),
            "If-None-Match: \"other\"".to_owned(),
        ] {
            assert_eq!(request(&condition).json_ok(), listing, "{path} {condition}");
        }
        let failed = request("If-Match: \"bogus\"");
        let error = failed.json_reason_value(412, "conditionNotMet");
        assert_eq!(
            error["error"]["errors"][0]["location"], "If-Match",
            "{path}"
        );
        assert_eq!(
            error["error"]["errors"][0]["locationType"], "header",
            "{path}"
        );
        assert_eq!(failed.headers.get("etag"), Some(&etag), "{path}");
        request("If-Match: \"unterminated").json_reason(400, "invalidParameter");
    }

    // The tag follows the owner's metadata, so an ACL change makes an earlier one stale.
    let path = "/storage/v1/b/test-bucket/acl";
    let before = server.request("GET", path, AUTH, b"").headers["etag"].clone();
    let entry = json!({"entity": "user-liz@example.com", "role": "READER"}).to_string();
    server
        .request("POST", path, AUTH, entry.as_bytes())
        .json_ok();
    let stale = server.request(
        "GET",
        path,
        &format!("{AUTH}If-None-Match: {before}\r\n"),
        b"",
    );
    let listing = stale.json_ok();
    assert_ne!(stale.headers["etag"], before);
    assert!(
        listing["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["entity"] == "user-liz@example.com"),
        "{listing}"
    );
}
