use crate::common;
use common::TestDir;
use common::effects::Server;
use serde_json::json;

const ADMIN: &str = "Authorization: Bearer admin\r\n";

#[test]
fn fields_read_etag_validators_use_the_complete_resource() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    server.put("file", b"data").json_ok();
    for (path, name) in [
        ("/storage/v1/b/test-bucket", "test-bucket"),
        ("/storage/v1/b/test-bucket/o/file", "file"),
    ] {
        let full = server.request("GET", path, "", b"");
        let etag = full.json_ok()["etag"].as_str().unwrap().to_owned();
        assert_eq!(full.headers["etag"], etag);
        let selected = format!("{path}?fields=name");
        for headers in [String::new(), format!("If-Match: {etag}\r\n")] {
            let reply = server.request("GET", &selected, &headers, b"");
            assert_eq!(reply.json_ok(), json!({"name":name}));
            assert_eq!(reply.headers["etag"], etag);
        }
        let cached = server.request("GET", &selected, &format!("If-None-Match: {etag}\r\n"), b"");
        cached.expect(304);
        assert_eq!(cached.headers["etag"], etag);
        assert!(cached.body.is_empty());
        let failed = server.request("GET", &selected, "If-Match: \"stale\"\r\n", b"");
        failed.json_reason(412, "conditionNotMet");
        let error: serde_json::Value = serde_json::from_slice(&failed.body).unwrap();
        assert_eq!(error["error"]["code"], 412);
        assert!(error["error"]["message"].is_string());
        assert_eq!(error["error"]["errors"].as_array().unwrap().len(), 1);
    }
    server.stop();
}

/// A selection may use the whole query, which holds far more than the former 4,096 bytes and
/// 256 selections.
#[test]
fn fields_may_take_as_much_of_the_query_as_it_holds() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    server.put("file", b"data").json_ok();
    let path = "/storage/v1/b/test-bucket/o/file?fields=";
    let selection = "name,".repeat(15 * 1024 / 5) + "name";
    assert!(selection.len() > 8 * 1024);
    assert!(selection.matches("name").count() > 256);
    assert_eq!(
        server
            .request("GET", &format!("{path}{selection}"), "", b"")
            .json_ok(),
        json!({"name":"file"})
    );
    server.stop();
}

#[test]
fn fields_invalid_selection_precedes_read_etag_evaluation() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    server.put("file", b"data").json_ok();
    for path in [
        "/storage/v1/b/test-bucket",
        "/storage/v1/b/test-bucket/o/file",
    ] {
        let full = server.request("GET", path, "", b"").json_ok();
        let etag = full["etag"].as_str().unwrap();
        for headers in [
            "If-Match: \"stale\"\r\n".to_owned(),
            format!("If-None-Match: {etag}\r\n"),
        ] {
            server
                .request("GET", &format!("{path}?fields=name("), &headers, b"")
                .json_reason(400, "invalidParameter");
        }
    }
    server.stop();
}

#[test]
fn fields_bucket_methods_and_policy_preserve_mutations() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, true);
    let value = server
        .request(
            "POST",
            "/storage/v1/b?project=local&fields=name",
            ADMIN,
            br#"{"name":"test-bucket","labels":{"team":"before"}}"#,
        )
        .json_ok();
    assert_eq!(value, json!({"name":"test-bucket"}));
    for method in ["PATCH", "PUT"] {
        let label = method.to_ascii_lowercase();
        let value = server
            .request(
                method,
                "/storage/v1/b/test-bucket?fields=labels/team",
                ADMIN,
                &json!({"labels":{"team":label}}).to_string().into_bytes(),
            )
            .json_ok();
        assert_eq!(value, json!({"labels":{"team":label}}));
        assert_eq!(
            server
                .request(
                    "GET",
                    "/storage/v1/b/test-bucket?fields=name,labels(team),versioning/enabled",
                    ADMIN,
                    b""
                )
                .json_ok(),
            json!({"name":"test-bucket","labels":{"team":label}})
        );
    }
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            ADMIN,
            br#"{"name":"test-bucket-more"}"#,
        )
        .json_ok();
    let page = server
        .request(
            "GET",
            "/storage/v1/b?project=local&maxResults=1&fields=kind,items(name),nextPageToken",
            ADMIN,
            b"",
        )
        .json_ok();
    assert_eq!(page["kind"], "storage#buckets");
    assert_eq!(page["items"], json!([{"name":"test-bucket"}]));
    let token = page["nextPageToken"].as_str().unwrap();
    let page = server
        .request(
            "GET",
            &format!(
                "/storage/v1/b?project=local&maxResults=1&pageToken={token}&fields=items/name"
            ),
            ADMIN,
            b"",
        )
        .json_ok();
    assert_eq!(page, json!({"items":[{"name":"test-bucket-more"}]}));
    let policy = br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#;
    assert_eq!(
        server
            .request(
                "PUT",
                "/storage/v1/b/test-bucket/iam?fields=bindings(role)",
                ADMIN,
                policy
            )
            .json_ok(),
        json!({"bindings":[{"role":"roles/storage.objectViewer"}]})
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/iam?fields=bindings/members",
                ADMIN,
                b""
            )
            .json_ok(),
        json!({"bindings":[{"members":["allUsers"]}]})
    );
    server.stop();
}

#[test]
fn fields_object_methods_lists_and_zero_byte_metadata() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    assert_eq!(server.request("POST", "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=empty&fields=name,size,contentEncoding", "", b"").json_ok(),
        json!({"name":"empty","size":"0"}));
    server.put("second", b"data").json_ok();
    server.put("dir/file", b"data").json_ok();
    for method in ["PATCH", "PUT"] {
        assert_eq!(
            server
                .request(
                    method,
                    "/storage/v1/b/test-bucket/o/empty?fields=metadata/team",
                    "",
                    &json!({"metadata":{"team":method}}).to_string().into_bytes()
                )
                .json_ok(),
            json!({"metadata":{"team":method}})
        );
        let full = server.get("empty").json_ok();
        assert_eq!(full["metadata"]["team"], method);
        assert_eq!(full["size"], "0");
        assert_eq!(
            server
                .request(
                    "GET",
                    "/storage/v1/b/test-bucket/o/empty?alt=json&fields=*",
                    "",
                    b""
                )
                .json_ok(),
            full
        );
    }
    let page =
        server.list("delimiter=/&maxResults=2&fields=kind,prefixes,items(name,size),nextPageToken");
    assert_eq!(page["kind"], "storage#objects");
    assert_eq!(page["prefixes"], json!(["dir/"]));
    assert_eq!(page["items"], json!([{"name":"empty","size":"0"}]));
    let token = page["nextPageToken"].as_str().unwrap();
    assert_eq!(
        server.list(&format!(
            "delimiter=/&maxResults=2&pageToken={token}&fields=items/name"
        )),
        json!({"items":[{"name":"second"}]})
    );
    server.stop();
}

#[test]
fn fields_invalid_selectors_and_errors_never_filter_or_mutate() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, true);
    server.bucket();
    server.put("empty", b"").json_ok();
    let original = server.get("empty").json_ok();
    for selector in [
        "name(",
        "metadata(",
        "metadata//key",
        "unknown",
        "items/name",
        "name/child",
        "*,unknown",
    ] {
        server
            .request(
                "PATCH",
                &format!("/storage/v1/b/test-bucket/o/empty?fields={selector}"),
                ADMIN,
                br#"{"metadata":{"bad":"change"}}"#,
            )
            .json_reason(400, "invalidParameter");
    }
    // An empty object selection selects nothing (recorded), and so does whitespace alone; a
    // space inside a name is still unsupported.
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/empty?fields=",
                ADMIN,
                b""
            )
            .json_ok(),
        json!({})
    );
    for selector in ["na%20me", "name,si%20ze"] {
        server
            .request(
                "GET",
                &format!("/storage/v1/b/test-bucket/o/empty?fields={selector}"),
                ADMIN,
                b"",
            )
            .json_reason(400, "badRequest");
    }
    for path in [
        "/storage/v1/b/test-bucket?fields=name",
        "/storage/v1/b/test-bucket/iam?fields=etag",
        "/storage/v1/b/test-bucket/o/empty?fields=name",
    ] {
        server
            .request("GET", path, "Authorization: Bearer creator\r\n", b"")
            .json_reason(403, "forbidden");
    }
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/missing?fields=name",
            ADMIN,
            b"",
        )
        .json_reason(404, "notFound");
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/empty?fields=name&ifMetagenerationMatch=9",
            ADMIN,
            br#"{"metadata":{"bad":"change"}}"#,
        )
        .json_reason(412, "conditionNotMet");
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/empty?fields=name",
            "Authorization: Bearer invalid\r\n",
            b"",
        )
        .json_reason(401, "authError");
    assert_eq!(server.get("empty").json_ok(), original);
    server.stop();
}

#[test]
fn fields_invalid_selectors_leave_all_routes_unchanged() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, true);
    server.bucket();
    server.put("existing", b"original").json_ok();
    let bucket = server
        .request("GET", "/storage/v1/b/test-bucket", ADMIN, b"")
        .json_ok();
    let object = server.get("existing").json_ok();
    let policy = server
        .request("GET", "/storage/v1/b/test-bucket/iam", ADMIN, b"")
        .json_ok();
    let objects = server.list("");
    let buckets = server
        .request("GET", "/storage/v1/b?project=local", ADMIN, b"")
        .json_ok();
    for (method, path, body) in [
        (
            "POST",
            "/storage/v1/b?project=local&fields=items/name",
            br#"{"name":"rejected-bucket"}"#.as_slice(),
        ),
        (
            "PATCH",
            "/storage/v1/b/test-bucket?fields=items/name",
            br#"{"labels":{"bad":"change"}}"#.as_slice(),
        ),
        (
            "PUT",
            "/storage/v1/b/test-bucket?fields=items/name",
            br#"{"labels":{"bad":"change"}}"#.as_slice(),
        ),
        (
            "GET",
            "/storage/v1/b?project=local&fields=name",
            b"".as_slice(),
        ),
        (
            "GET",
            "/storage/v1/b/test-bucket/o?fields=name",
            b"".as_slice(),
        ),
        (
            "GET",
            "/storage/v1/b/test-bucket/iam?fields=name",
            b"".as_slice(),
        ),
        (
            "PUT",
            "/storage/v1/b/test-bucket/iam?fields=name",
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#
                .as_slice(),
        ),
        (
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=rejected&fields=items/name",
            b"replacement".as_slice(),
        ),
        (
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=multipart&name=rejected&fields=items/name",
            b"rejected before MIME parsing".as_slice(),
        ),
        (
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=rejected&fields=items/name",
            b"".as_slice(),
        ),
    ] {
        let reply = server.request(method, path, ADMIN, body);
        reply.json_reason(400, "invalidParameter");
        assert!(!reply.headers.contains_key("location"));
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/test-bucket", ADMIN, b"")
                .json_ok(),
            bucket,
            "{method} {path}"
        );
        assert_eq!(server.get("existing").json_ok(), object, "{method} {path}");
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/test-bucket/iam", ADMIN, b"")
                .json_ok(),
            policy,
            "{method} {path}"
        );
        assert_eq!(server.list(""), objects, "{method} {path}");
        assert_eq!(
            server
                .request("GET", "/storage/v1/b?project=local", ADMIN, b"")
                .json_ok(),
            buckets,
            "{method} {path}"
        );
        server
            .request("GET", "/storage/v1/b/rejected-bucket", ADMIN, b"")
            .json_reason(404, "notFound");
        server.get("rejected").json_reason(404, "notFound");
    }
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/existing?alt=media",
                ADMIN,
                b""
            )
            .body,
        b"original"
    );
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/existing",
            ADMIN,
            br#"{"metadata":{"other":"value"}}"#,
        )
        .json_ok();
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/existing?fields=metadata/missing",
                ADMIN,
                b""
            )
            .json_ok(),
        // A selection that matches nothing inside metadata drops the key (recorded).
        json!({})
    );
    server.stop();
}

#[test]
/// The ledger names this test; despite its name, `fields` on a media download is now ignored.
fn fields_on_delete_and_media_are_ignored_and_xml_unchanged() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    server.put("file", b"data").json_ok();
    // Cloud Storage returned the object bytes and ignored `fields` (case obj-fields-selectors).
    let selected = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/file?alt=media&fields=name",
        "",
        b"",
    );
    selected.expect(200);
    assert_eq!(selected.body, b"data");
    let media = server.request("GET", "/storage/v1/b/test-bucket/o/file?alt=media", "", b"");
    assert_eq!(media.body, b"data");
    let xml = server.request("GET", "/test-bucket/file", "", b"");
    xml.expect(200);
    assert_eq!(xml.body, b"data");
    let deleted = server.request(
        "DELETE",
        "/storage/v1/b/test-bucket/o/file?fields=(",
        "",
        b"",
    );
    deleted.expect(204);
    assert!(deleted.body.is_empty());
    let deleted = server.request("DELETE", "/storage/v1/b/test-bucket?fields=", "", b"");
    deleted.expect(204);
    assert!(deleted.body.is_empty());
    server.stop();
}

/// Selector edges Cloud Storage recorded on an object read (case obj-fields-selectors).
#[test]
fn fields_object_whitespace_and_empty_parentheses_follow_cloud_storage() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    server.put("file", b"hello").json_ok();
    server
        .update("PATCH", "file", &json!({"metadata":{"k":"v"}}))
        .json_ok();
    for (fields, expected) in [
        ("%20", json!({})),
        ("name%20%2C%20size", json!({"name":"file","size":"5"})),
        ("metadata%28%29", json!({})),
    ] {
        let reply = server.request(
            "GET",
            &format!("/storage/v1/b/test-bucket/o/file?fields={fields}"),
            "",
            b"",
        );
        assert_eq!(reply.json_ok(), expected, "{fields}");
    }
    let trailing = server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/file?fields=metadata%2F",
            "",
            b"",
        )
        .json_reason_value(400, "invalidParameter");
    assert_eq!(
        trailing["error"]["message"],
        "Invalid field selection metadata/"
    );
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/file?fields=na%20me",
            "",
            b"",
        )
        .json_reason(400, "badRequest");
    server.stop();
}

#[test]
fn fields_resumable_final_response_survives_restart_and_replay() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    let initiation = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=resume&fields=name,size",
        "",
        b"",
    );
    initiation.expect(200);
    assert!(initiation.body.is_empty());
    let location = initiation.headers["location"].clone();
    let path = &location[location.find("/upload/").unwrap()..];
    server.stop();
    let mut server = Server::start(&dir, false);
    assert_eq!(
        server.request("PUT", path, "", b"data").json_ok(),
        json!({"name":"resume","size":"4"})
    );
    assert_eq!(server.get("resume").json_ok()["size"], "4");
    server.stop();
    let mut server = Server::start(&dir, false);
    assert_eq!(
        server
            .request("PUT", path, "Content-Range: bytes */4\r\n", b"")
            .json_ok(),
        json!({"name":"resume","size":"4"})
    );
    server.stop();
}
