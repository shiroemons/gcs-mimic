//! Query parameters an object method does not declare. Cloud Storage served object insert (media
//! and multipart), get, media download, patch, update, copy, rewrite, compose, object ACL listing
//! and delete with an arbitrary `gcsmimicUnknown=1` (case obj-undeclared-query-writes).

use crate::common;
use common::TestDir;
use common::effects::Server;
use common::http::Client;
use std::time::Duration;

const AUTH: &str = "Authorization: Bearer admin\r\n";
const BUCKET: &str = "/storage/v1/b/test-bucket";
const UNKNOWN: &str = "gcsmimicUnknown=1";

fn multipart_insert(server: &Server, query: &str) -> serde_json::Value {
    let boundary = "undeclared";
    let body = format!(
        "--{boundary}\r\nContent-Type: application/json\r\n\r\n{{\"name\":\"multipart\"}}\r\n--{boundary}\r\nContent-Type: text/plain\r\n\r\nhello\r\n--{boundary}--\r\n"
    );
    Client {
        address: &server.address,
        host: "localhost",
        headers: "",
        timeout: Duration::from_secs(10),
    }
    .request_eof(
        "POST",
        &format!("/upload/storage/v1/b/test-bucket/o?uploadType=multipart&{query}"),
        &format!("{AUTH}Content-Type: multipart/related; boundary={boundary}\r\n"),
        body.as_bytes(),
    )
    .into_joined()
    .json_ok()
}

#[test]
fn object_methods_ignore_an_undeclared_query_parameter() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    let object = format!("{BUCKET}/o/main");
    let inserted = server
        .request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?uploadType=media&name=main&{UNKNOWN}"),
            AUTH,
            b"hello",
        )
        .json_ok();
    assert_eq!(inserted["size"], "5");
    assert_eq!(multipart_insert(&server, UNKNOWN)["name"], "multipart");

    let read = server
        .request(
            "GET",
            &format!("{object}?fields=name,size&{UNKNOWN}"),
            AUTH,
            b"",
        )
        .json_ok();
    assert_eq!(read, serde_json::json!({"name":"main","size":"5"}));
    let media = server.request("GET", &format!("{object}?alt=media&{UNKNOWN}"), AUTH, b"");
    media.expect(200);
    assert_eq!(media.body, b"hello");
    let patched = server
        .request(
            "PATCH",
            &format!("{object}?fields=metadata&{UNKNOWN}"),
            AUTH,
            br#"{"metadata":{"k":"v"}}"#,
        )
        .json_ok();
    assert_eq!(patched["metadata"]["k"], "v");
    let updated = server
        .request(
            "PUT",
            &format!("{object}?fields=contentType&{UNKNOWN}"),
            AUTH,
            br#"{"contentType":"text/plain"}"#,
        )
        .json_ok();
    assert_eq!(updated["contentType"], "text/plain");

    let copied = server
        .request(
            "POST",
            &format!("{object}/copyTo/b/test-bucket/o/copy?{UNKNOWN}"),
            AUTH,
            b"",
        )
        .json_ok();
    assert_eq!(copied["size"], "5");
    let rewritten = server
        .request(
            "POST",
            &format!("{object}/rewriteTo/b/test-bucket/o/rewritten?{UNKNOWN}"),
            AUTH,
            b"",
        )
        .json_ok();
    assert_eq!(rewritten["done"], true);
    assert_eq!(rewritten["resource"]["name"], "rewritten");
    let composed = server
        .request(
            "POST",
            &format!("{BUCKET}/o/composed/compose?{UNKNOWN}"),
            AUTH,
            br#"{"sourceObjects":[{"name":"main"}]}"#,
        )
        .json_ok();
    assert_eq!(composed["componentCount"], 1);
    let acl = server
        .request("GET", &format!("{object}/acl?{UNKNOWN}"), AUTH, b"")
        .json_ok();
    assert_eq!(acl["kind"], "storage#objectAccessControls");

    server
        .request("DELETE", &format!("{object}?{UNKNOWN}"), AUTH, b"")
        .expect(204);
    server.get("main").json_reason(404, "notFound");
}

#[test]
fn declared_but_unsupported_and_unrecorded_methods_still_reject() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("main", b"hello").json_ok();
    // A parameter the method declares but gcs-mimic does not implement is not silently dropped.
    for (method, path) in [
        (
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=other&kmsKeyName=key",
        ),
        ("GET", "/storage/v1/b/test-bucket/o/main?restoreToken=token"),
        (
            "PATCH",
            "/storage/v1/b/test-bucket/o/main?predefinedAcl=private",
        ),
    ] {
        server
            .request(method, path, AUTH, b"{}")
            .json_reason(400, "badRequest");
    }
    // Methods whose treatment of an undeclared name is unrecorded keep rejecting it.
    for path in [
        format!("{BUCKET}/o/main/acl/allUsers?{UNKNOWN}"),
        format!("{BUCKET}/iam?{UNKNOWN}"),
    ] {
        server
            .request("GET", &path, AUTH, b"")
            .json_reason(400, "invalidParameter");
    }
}
