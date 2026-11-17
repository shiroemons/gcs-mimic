//! Query parameters a JSON method does not declare. Cloud Storage served object insert (media
//! and multipart), get, media download, patch, update, copy, rewrite, compose, object ACL listing
//! and delete with an arbitrary `gcsmimicUnknown=1` (case obj-undeclared-query-writes), and
//! objects.move and objects.restore (case obj-probe-undeclared-query-move-restore), the other
//! object ACL methods and the bucket and default object ACL list and get (case
//! obj-probe-undeclared-query-acls), and bucket getIamPolicy and testIamPermissions, the
//! notification list and get and the HMAC key list (case bkt-probe-undeclared-query-resources).

use crate::common;
use common::TestDir;
use common::effects::Server;
use common::http::Client;
use serde_json::json;
use std::time::Duration;

const AUTH: &str = "Authorization: Bearer admin\r\n";
const BUCKET: &str = "/storage/v1/b/test-bucket";
const UNKNOWN: &str = "gcsmimicUnknown=1";
const ENTITY: &str = "user-liz@example.com";

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
fn move_and_restore_ignore_an_undeclared_query_parameter() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("source", b"hello").json_ok();
    let moved = server.request(
        "POST",
        &format!("{BUCKET}/o/source/moveTo/o/moved?fields=name,size&{UNKNOWN}"),
        AUTH,
        b"",
    );
    assert_eq!(moved.json_ok(), json!({"name": "moved", "size": "5"}));
    // Cloud Storage sends no ETag header with a move or restore response.
    assert_eq!(moved.headers.get("etag"), None);
    server.get("moved").json_ok();
    server.get("source").json_reason(404, "notFound");

    let soft =
        json!({"name": "soft-bucket", "softDeletePolicy": {"retentionDurationSeconds": "604800"}});
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            AUTH,
            soft.to_string().as_bytes(),
        )
        .json_ok();
    let uploaded = server
        .request(
            "POST",
            "/upload/storage/v1/b/soft-bucket/o?uploadType=media&name=kept",
            AUTH,
            b"hello",
        )
        .json_ok();
    let generation = uploaded["generation"].as_str().unwrap();
    let object = "/storage/v1/b/soft-bucket/o/kept";
    server.request("DELETE", object, AUTH, b"").expect(204);
    let restored = server.request(
        "POST",
        &format!("{object}/restore?generation={generation}&fields=name,size&{UNKNOWN}"),
        AUTH,
        b"",
    );
    assert_eq!(restored.json_ok(), json!({"name": "kept", "size": "5"}));
    assert_eq!(restored.headers.get("etag"), None);
    let live = server
        .request("GET", &format!("{object}?fields=size"), AUTH, b"")
        .json_ok();
    assert_eq!(live, json!({"size": "5"}));
}

#[test]
fn acl_methods_ignore_an_undeclared_query_parameter() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("main", b"hello").json_ok();
    let acl = format!("{BUCKET}/o/main/acl");
    let entry = format!("{acl}/{ENTITY}");
    let inserted = server
        .request(
            "POST",
            &format!("{acl}?{UNKNOWN}"),
            AUTH,
            json!({"entity": ENTITY, "role": "READER"})
                .to_string()
                .as_bytes(),
        )
        .json_ok();
    assert_eq!(inserted["role"], "READER");
    let read = server
        .request("GET", &format!("{entry}?{UNKNOWN}"), AUTH, b"")
        .json_ok();
    assert_eq!(read["entity"], ENTITY);
    let patched = server
        .request(
            "PATCH",
            &format!("{entry}?{UNKNOWN}"),
            AUTH,
            br#"{"role":"OWNER"}"#,
        )
        .json_ok();
    assert_eq!(patched["role"], "OWNER");
    let updated = server
        .request(
            "PUT",
            &format!("{entry}?{UNKNOWN}"),
            AUTH,
            json!({"entity": ENTITY, "role": "READER"})
                .to_string()
                .as_bytes(),
        )
        .json_ok();
    assert_eq!(updated["role"], "READER");
    server
        .request("DELETE", &format!("{entry}?{UNKNOWN}"), AUTH, b"")
        .expect(204);
    let listed = server.request("GET", &acl, AUTH, b"").json_ok();
    let entities: Vec<_> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["entity"].as_str().unwrap())
        .collect();
    assert!(!entities.contains(&ENTITY), "{entities:?}");

    for collection in [
        format!("{BUCKET}/acl"),
        format!("{BUCKET}/defaultObjectAcl"),
    ] {
        let body = json!({"entity": ENTITY, "role": "READER"}).to_string();
        server
            .request("POST", &collection, AUTH, body.as_bytes())
            .json_ok();
        let listed = server
            .request("GET", &format!("{collection}?{UNKNOWN}"), AUTH, b"")
            .json_ok();
        assert!(listed["items"].is_array(), "{collection}");
        let read = server
            .request(
                "GET",
                &format!("{collection}/{ENTITY}?{UNKNOWN}"),
                AUTH,
                b"",
            )
            .json_ok();
        assert_eq!(read["role"], "READER", "{collection}");
    }
}

#[test]
fn iam_notification_and_hmac_key_reads_ignore_an_undeclared_query_parameter() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    let policy = server
        .request("GET", &format!("{BUCKET}/iam?{UNKNOWN}"), AUTH, b"")
        .json_ok();
    assert_eq!(policy["kind"], "storage#policy");
    let tested = server
        .request(
            "GET",
            &format!("{BUCKET}/iam/testPermissions?permissions=storage.buckets.get&{UNKNOWN}"),
            AUTH,
            b"",
        )
        .json_ok();
    assert_eq!(tested["permissions"], json!(["storage.buckets.get"]));
    let configs = format!("{BUCKET}/notificationConfigs");
    let listed = server
        .request("GET", &format!("{configs}?{UNKNOWN}"), AUTH, b"")
        .json_ok();
    assert_eq!(listed, json!({"kind": "storage#notifications"}));
    // A missing configuration is still not found, as Cloud Storage answered.
    server
        .request("GET", &format!("{configs}/999999?{UNKNOWN}"), AUTH, b"")
        .json_reason(404, "notFound");
    let keys = server
        .request(
            "GET",
            &format!("/storage/v1/projects/local/hmacKeys?{UNKNOWN}"),
            AUTH,
            b"",
        )
        .json_ok();
    assert_eq!(keys["kind"], "storage#hmacKeysMetadata");
}

/// fake-gcs-server reads only the parameters it knows on the routes it serves (object and bucket
/// ACL list, insert, get, update and delete, and notifications), so its profile ignores an
/// undeclared one there. It has no move, restore, default object ACL, ACL PATCH or IAM route,
/// and the profile keeps rejecting one on those.
#[test]
fn the_fake_gcs_server_profile_ignores_an_undeclared_parameter_only_on_its_routes() {
    let dir = TestDir::new();
    let server = Server::start_with_args(&dir, &["--compat-profile", "fake-gcs-server"]);
    server.bucket();
    server.put("main", b"hello").json_ok();
    let acl = format!("{BUCKET}/o/main/acl");
    let entry = format!("{acl}/{ENTITY}");
    let body = json!({"entity": ENTITY, "role": "READER"}).to_string();
    for (method, path, body) in [
        ("POST", acl.clone(), body.as_bytes()),
        ("GET", acl, b""),
        ("GET", entry.clone(), b""),
        ("PUT", entry.clone(), body.as_bytes()),
        ("POST", format!("{BUCKET}/acl"), body.as_bytes()),
        ("GET", format!("{BUCKET}/acl"), b""),
        ("GET", format!("{BUCKET}/acl/{ENTITY}"), b""),
        ("GET", format!("{BUCKET}/notificationConfigs"), b""),
    ] {
        server
            .request(method, &format!("{path}?{UNKNOWN}"), AUTH, body)
            .expect_with(200, format_args!("{method} {path}"));
    }
    for (method, path, body) in [
        ("PATCH", entry.clone(), &br#"{"role":"OWNER"}"#[..]),
        ("GET", format!("{BUCKET}/defaultObjectAcl"), b""),
        ("GET", format!("{BUCKET}/iam"), b""),
        (
            "GET",
            format!("{BUCKET}/iam/testPermissions?permissions=storage.buckets.get"),
            b"",
        ),
        ("POST", format!("{BUCKET}/o/main/moveTo/o/moved"), b""),
        ("POST", format!("{BUCKET}/o/main/restore?generation=1"), b""),
    ] {
        let separator = if path.contains('?') { '&' } else { '?' };
        let reply = server.request(method, &format!("{path}{separator}{UNKNOWN}"), AUTH, body);
        let error = reply.json_reason_value(400, "invalidParameter");
        assert_eq!(
            error["error"]["message"], "Unknown query parameter for this method: gcsmimicUnknown",
            "{method} {path}"
        );
    }
    // The refused PATCH and move changed nothing.
    let read = server.request("GET", &entry, AUTH, b"").json_ok();
    assert_eq!(read["role"], "READER");
    server.get("main").json_ok();
    server.get("moved").json_reason(404, "notFound");
    server
        .request("DELETE", &format!("{entry}?{UNKNOWN}"), AUTH, b"")
        .expect(204);
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
    ] {
        server
            .request(method, path, AUTH, b"{}")
            .json_reason(400, "badRequest");
    }
    // Methods whose treatment of an undeclared name is unrecorded keep rejecting it.
    server
        .request(
            "GET",
            &format!("{BUCKET}/storageLayout?{UNKNOWN}"),
            AUTH,
            b"",
        )
        .json_reason(400, "invalidParameter");
}
