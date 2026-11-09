//! The `key` standard query parameter is accepted and ignored on every JSON route, and is never a
//! credential.

use crate::common;
use common::TestDir;
use common::effects::Server;
use serde_json::json;

const KEY: &str = "key=AIzaSyLocalTestValue";
const ADMIN: &str = "Authorization: Bearer admin\r\n";
const SCOPED: &str = "Authorization: Bearer scoped\r\n";
const INVALID: &str = "Authorization: Bearer not-registered\r\n";
const BUCKET: &str = "/storage/v1/b/test-bucket";

fn keyed(path: &str) -> String {
    let separator = if path.contains('?') { '&' } else { '?' };
    format!("{path}{separator}{KEY}")
}

#[test]
fn api_key_is_accepted_and_ignored_on_every_json_route() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("source", b"source-bytes").json_ok();
    server.put("second", b"second-bytes").json_ok();
    let compose = json!({"sourceObjects":[{"name":"source"},{"name":"second"}]}).to_string();
    let patch = r#"{"metadata":{"team":"key"}}"#;
    let cases = [
        (
            "POST",
            "/storage/v1/b?project=local".to_owned(),
            r#"{"name":"second-bucket"}"#,
            200,
        ),
        ("GET", "/storage/v1/b?project=local".to_owned(), "", 200),
        ("GET", BUCKET.to_owned(), "", 200),
        ("PATCH", BUCKET.to_owned(), r#"{"labels":{"a":"b"}}"#, 200),
        (
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=inserted".to_owned(),
            "inserted-bytes",
            200,
        ),
        ("GET", format!("{BUCKET}/o/inserted"), "", 200),
        ("GET", format!("{BUCKET}/o"), "", 200),
        ("PATCH", format!("{BUCKET}/o/inserted"), patch, 200),
        (
            "POST",
            format!("{BUCKET}/o/source/copyTo/b/test-bucket/o/copied"),
            "",
            200,
        ),
        (
            "POST",
            format!("{BUCKET}/o/source/rewriteTo/b/test-bucket/o/rewritten"),
            "",
            200,
        ),
        (
            "POST",
            format!("{BUCKET}/o/composed/compose"),
            &compose,
            200,
        ),
        ("POST", format!("{BUCKET}/o/copied/moveTo/o/moved"), "", 200),
        ("GET", format!("{BUCKET}/acl"), "", 200),
        ("GET", format!("{BUCKET}/defaultObjectAcl"), "", 200),
        ("GET", format!("{BUCKET}/o/inserted/acl"), "", 200),
        ("GET", format!("{BUCKET}/iam"), "", 200),
        (
            "GET",
            format!("{BUCKET}/iam/testPermissions?permissions=storage.objects.get"),
            "",
            200,
        ),
        ("GET", format!("{BUCKET}/notificationConfigs"), "", 200),
        (
            "GET",
            "/storage/v1/projects/local/serviceAccount".to_owned(),
            "",
            200,
        ),
        (
            "GET",
            "/storage/v1/projects/local/hmacKeys".to_owned(),
            "",
            200,
        ),
        ("DELETE", format!("{BUCKET}/o/moved"), "", 204),
        ("DELETE", "/storage/v1/b/second-bucket".to_owned(), "", 204),
    ];
    for (method, path, body, status) in cases {
        server
            .request(method, &keyed(&path), "", body.as_bytes())
            .expect_with(status, format!("{method} {path}"));
    }
    assert_eq!(
        server
            .request(
                "GET",
                &keyed(&format!("{BUCKET}/o/inserted?alt=media")),
                "",
                b""
            )
            .expect(200)
            .body,
        b"inserted-bytes"
    );
    assert_eq!(
        server
            .request(
                "GET",
                &keyed("/download/storage/v1/b/test-bucket/o/inserted?alt=media"),
                "",
                b""
            )
            .expect(200)
            .body,
        b"inserted-bytes"
    );
    // The key changes nothing about a response, whatever its value looks like.
    for path in [
        BUCKET.to_owned(),
        format!("{BUCKET}/o"),
        format!("{BUCKET}/o/source"),
    ] {
        let plain = server.request("GET", &path, "", b"").json_ok();
        for query in [KEY, "key=", "key=not%20a%20key", "key=%E7%8C%AB"] {
            let separator = if path.contains('?') { '&' } else { '?' };
            let reply = server.request("GET", &format!("{path}{separator}{query}"), "", b"");
            assert_eq!(reply.json_ok(), plain, "{path} {query}");
        }
    }
}

#[test]
fn a_resumable_upload_initiated_with_an_api_key_completes() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    let initiated = server.request(
        "POST",
        &keyed("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=resumed"),
        "",
        b"{}",
    );
    initiated.expect(200);
    let session = initiated.headers["location"]
        .strip_prefix("http://127.0.0.1:4443")
        .unwrap()
        .to_owned();
    server
        .request("PUT", &session, "", b"resumable-bytes")
        .json_ok();
    assert_eq!(
        server.get("resumed?alt=media").expect(200).body,
        b"resumable-bytes"
    );
}

#[test]
fn api_key_is_accepted_under_the_legacy_emulator_profile() {
    let dir = TestDir::new();
    let server = Server::start_with_args(&dir, &["--compat-profile", "legacy-emulator"]);
    server.bucket();
    server.put("object", b"object-bytes").json_ok();
    for path in [
        BUCKET.to_owned(),
        format!("{BUCKET}/o"),
        format!("{BUCKET}/o/object"),
    ] {
        server.request("GET", &keyed(&path), "", b"").expect(200);
    }
    server
        .request(
            "POST",
            &keyed("/upload/storage/v1/b/test-bucket/o?uploadType=media&name=keyed"),
            "",
            b"keyed-bytes",
        )
        .json_ok();
}

/// Strict mode must treat a keyed request as whatever the rest of the request presents.
#[test]
fn api_key_is_not_a_credential_under_strict_authentication() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("private", b"private-bytes").json_ok();
    for path in [
        BUCKET.to_owned(),
        format!("{BUCKET}/o"),
        format!("{BUCKET}/o/private"),
        format!("{BUCKET}/o/private?alt=media"),
    ] {
        // With a key and no bearer token the caller stays anonymous, and an unusable token is
        // still refused, so the responses match the same request without a key.
        for (credential, status) in [("", 401), (INVALID, 401), (SCOPED, 403)] {
            let plain = server.request("GET", &path, credential, b"");
            plain.expect_with(status, format!("{path} {credential:?}"));
            let with_key = server.request("GET", &keyed(&path), credential, b"");
            with_key.expect_with(status, format!("{path} {credential:?} with key"));
            assert_eq!(with_key.body, plain.body, "{path} {credential:?}");
        }
        // A valid bearer token keeps its identity alongside a key.
        let plain = server.request("GET", &path, ADMIN, b"");
        let with_key = server.request("GET", &keyed(&path), ADMIN, b"");
        with_key.expect_with(200, format!("{path} admin with key"));
        assert_eq!(with_key.body, plain.body, "{path} admin");
    }
    let upload = keyed("/upload/storage/v1/b/test-bucket/o?uploadType=media&name=anonymous");
    server.request("POST", &upload, "", b"data").expect(401);
    server.get("anonymous").json_reason(404, "notFound");
}

#[test]
fn api_key_keeps_anonymous_access_to_a_public_object_under_strict_authentication() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("public", b"public-bytes").json_ok();
    server
        .request(
            "POST",
            &format!("{BUCKET}/o/public/acl"),
            ADMIN,
            br#"{"entity":"allUsers","role":"READER"}"#,
        )
        .json_ok();
    let path = keyed(&format!("{BUCKET}/o/public?alt=media"));
    assert_eq!(
        server.request("GET", &path, "", b"").expect(200).body,
        b"public-bytes"
    );
}
