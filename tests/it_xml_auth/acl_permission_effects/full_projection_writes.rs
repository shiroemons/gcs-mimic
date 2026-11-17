//! Writes that ask for `projection=full` answer with the `acl` and `owner` a full read of the
//! written object reports, as Cloud Storage answered a resumable upload and a rewrite (recorded
//! 2026-11-17, case obj-full-projection-writes). The Go SDK's Writer and Copier always ask.

use super::*;

/// The member names of `resource`, sorted.
fn keys(resource: &Value) -> Vec<&str> {
    let mut keys: Vec<_> = resource
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

/// The sorted entity and role of each `acl` entry of `resource`.
fn grants(resource: &Value) -> Vec<(String, String)> {
    let mut grants: Vec<_> = resource["acl"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| (entry["entity"].to_string(), entry["role"].to_string()))
        .collect();
    grants.sort_unstable();
    grants
}

/// The `acl` and `owner` of `resource`, `Null` where it has none.
fn acl_fields(resource: &Value) -> (Value, Value) {
    (resource["acl"].clone(), resource["owner"].clone())
}

/// What a full read of `name` reports, which every full write response must repeat.
fn read_acl_fields(server: &Server, name: &str) -> (Value, Value) {
    acl_fields(
        &server
            .request(
                "GET",
                &format!("{BUCKET}/o/{name}?projection=full"),
                ADMIN,
                b"",
            )
            .json_ok(),
    )
}

/// Starts a five-byte resumable upload of `name` and returns its session path.
fn start_upload(server: &Server, name: &str, query: &str) -> String {
    let started = server.request(
        "POST",
        &format!("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name={name}{query}"),
        &format!("{ADMIN}X-Upload-Content-Length: 5\r\n"),
        b"{}",
    );
    assert_eq!(
        started.status,
        200,
        "{}",
        String::from_utf8_lossy(&started.body)
    );
    let location = &started.headers["location"];
    let path = location.find("/upload/").expect("session path");
    location[path..].to_owned()
}

/// A chunk or status request on `session`.
fn put(server: &Server, session: &str, range: &str, body: &[u8]) -> Value {
    server
        .request(
            "PUT",
            session,
            &format!("{ADMIN}Content-Range: {range}\r\n"),
            body,
        )
        .json_ok()
}

#[test]
fn a_full_projection_resumable_upload_answers_every_request_with_the_objects_acl() {
    let (data, inputs) = (TestDir::new(), TestDir::new());
    let server = Server::start(&data, &inputs, "disk");
    server.bucket();

    let session = start_upload(&server, "full", "&projection=full");
    let completed = put(&server, &session, "bytes 0-4/5", b"hello");
    let expected = read_acl_fields(&server, "full");
    assert!(
        expected.0.is_array() && expected.1.is_object(),
        "{expected:?}"
    );
    assert_eq!(acl_fields(&completed), expected);
    // A retry or status request on the completed session and a refused cancellation answer
    // from the stored resource, with the ACL as the object has it then. Their entry etags stay
    // those of the stored resource's metageneration, which an ACL change does not rewrite.
    assert_eq!(
        acl_fields(&put(&server, &session, "bytes */5", b"")),
        expected
    );
    server
        .admin(
            "POST",
            &format!("{BUCKET}/o/full/acl"),
            json!({"entity":"user-reader@example.test","role":"READER"}),
        )
        .json_ok();
    let granted = server
        .request(
            "GET",
            &format!("{BUCKET}/o/full?projection=full"),
            ADMIN,
            b"",
        )
        .json_ok();
    assert_ne!(grants(&granted), grants(&completed));
    for answer in [
        put(&server, &session, "bytes */5", b""),
        server.request("DELETE", &session, ADMIN, b"").json_ok(),
    ] {
        assert_eq!(grants(&answer), grants(&granted));
        assert_eq!(answer["owner"], granted["owner"]);
        assert_eq!(answer["acl"][0]["etag"], completed["acl"][0]["etag"]);
    }

    // The initiation's field selection applies after the ACL fields are added.
    let session = start_upload(
        &server,
        "selected",
        "&projection=full&fields=name,acl(entity)",
    );
    let entities = |resource: &Value| {
        assert_eq!(keys(resource), ["acl", "name"], "{resource}");
        assert_eq!(keys(&resource["acl"][0]), ["entity"], "{resource}");
    };
    entities(&put(&server, &session, "bytes 0-4/5", b"hello"));
    entities(&put(&server, &session, "bytes */5", b""));

    // Without the full projection the completion carries neither field.
    let session = start_upload(&server, "plain", "");
    let plain = put(&server, &session, "bytes 0-4/5", b"hello");
    assert_eq!(acl_fields(&plain), (Value::Null, Value::Null));
}

#[test]
fn a_full_projection_rewrite_answers_with_the_destinations_acl() {
    let (data, inputs) = (TestDir::new(), TestDir::new());
    let server = Server::start(&data, &inputs, "disk");
    server.bucket();
    server.put(b"rewritten bytes");
    let rewrite = |query: &str| {
        server
            .request(
                "POST",
                &format!("{OBJECT}/rewriteTo/b/test-bucket/o/rewritten{query}"),
                ADMIN,
                b"{}",
            )
            .json_ok()
    };

    let full = rewrite("?projection=full");
    assert_eq!(full["done"], true);
    let expected = read_acl_fields(&server, "rewritten");
    assert!(expected.0.is_array(), "{expected:?}");
    assert_eq!(acl_fields(&full["resource"]), expected);

    let selected = rewrite("?projection=full&fields=resource(name,acl(entity))");
    assert_eq!(keys(&selected["resource"]), ["acl", "name"], "{selected}");

    // Without it the resource names the creator as owner but carries no ACL (recorded
    // 2026-11-01).
    let plain = rewrite("");
    assert!(plain["resource"].get("acl").is_none(), "{plain}");
    assert_eq!(
        plain["resource"]["owner"],
        read_acl_fields(&server, "rewritten").1
    );
}
