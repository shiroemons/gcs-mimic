#[path = "object_effects_support.rs"]
mod support;
use serde_json::json;
use support::{Server, common::TestDir};

const ADMIN: &str = "Authorization: Bearer admin\r\n";

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
        .json();
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
            .json();
        assert_eq!(value, json!({"labels":{"team":label}}));
        assert_eq!(
            server
                .request(
                    "GET",
                    "/storage/v1/b/test-bucket?fields=name,labels(team),versioning/enabled",
                    ADMIN,
                    b""
                )
                .json(),
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
        .json();
    let page = server
        .request(
            "GET",
            "/storage/v1/b?project=local&maxResults=1&fields=kind,items(name),nextPageToken",
            ADMIN,
            b"",
        )
        .json();
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
        .json();
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
            .json(),
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
            .json(),
        json!({"bindings":[{"members":["allUsers"]}]})
    );
    server.stop();
}

#[test]
fn fields_object_methods_lists_and_zero_byte_metadata() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    assert_eq!(server.request("POST", "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=empty&fields=name,size,contentEncoding", "", b"").json(),
        json!({"name":"empty","size":"0"}));
    server.put("second", b"data").json();
    server.put("dir/file", b"data").json();
    for method in ["PATCH", "PUT"] {
        assert_eq!(
            server
                .request(
                    method,
                    "/storage/v1/b/test-bucket/o/empty?fields=metadata/team",
                    "",
                    &json!({"metadata":{"team":method}}).to_string().into_bytes()
                )
                .json(),
            json!({"metadata":{"team":method}})
        );
        let full = server.get("empty").json();
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
                .json(),
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
    server.put("empty", b"").json();
    let original = server.get("empty").json();
    for selector in [
        "name,",
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
            .error(400, "invalidParameter");
    }
    for selector in ["", "%20name", "name,%20size"] {
        server
            .request(
                "GET",
                &format!("/storage/v1/b/test-bucket/o/empty?fields={selector}"),
                ADMIN,
                b"",
            )
            .error(501, "notImplemented");
    }
    for path in [
        "/storage/v1/b/test-bucket?fields=name",
        "/storage/v1/b/test-bucket/iam?fields=etag",
        "/storage/v1/b/test-bucket/o/empty?fields=name",
    ] {
        server
            .request("GET", path, "Authorization: Bearer creator\r\n", b"")
            .error(403, "forbidden");
    }
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/missing?fields=name",
            ADMIN,
            b"",
        )
        .error(404, "notFound");
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/empty?fields=name&ifMetagenerationMatch=9",
            ADMIN,
            br#"{"metadata":{"bad":"change"}}"#,
        )
        .error(412, "conditionNotMet");
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/empty?fields=name",
            "Authorization: Bearer invalid\r\n",
            b"",
        )
        .error(401, "authError");
    assert_eq!(server.get("empty").json(), original);
    server.stop();
}

#[test]
fn fields_delete_is_ignored_media_is_explicitly_unsupported_and_xml_unchanged() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    server.put("file", b"data").json();
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/file?alt=media&fields=name",
            "",
            b"",
        )
        .error(501, "notImplemented");
    let media = server.request("GET", "/storage/v1/b/test-bucket/o/file?alt=media", "", b"");
    assert_eq!(media.body, b"data");
    let xml = server.request("GET", "/test-bucket/file", "", b"");
    assert_eq!(xml.status, 200);
    assert_eq!(xml.body, b"data");
    let deleted = server.request(
        "DELETE",
        "/storage/v1/b/test-bucket/o/file?fields=(",
        "",
        b"",
    );
    assert_eq!(deleted.status, 204);
    assert!(deleted.body.is_empty());
    let deleted = server.request("DELETE", "/storage/v1/b/test-bucket?fields=", "", b"");
    assert_eq!(deleted.status, 204);
    assert!(deleted.body.is_empty());
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
    assert_eq!(initiation.status, 200);
    assert!(initiation.body.is_empty());
    let location = initiation.headers["location"].clone();
    let path = &location[location.find("/upload/").unwrap()..];
    server.stop();
    let mut server = Server::start(&dir, false);
    assert_eq!(
        server.request("PUT", path, "", b"data").json(),
        json!({"name":"resume","size":"4"})
    );
    assert_eq!(server.get("resume").json()["size"], "4");
    server.stop();
    let mut server = Server::start(&dir, false);
    assert_eq!(
        server
            .request("PUT", path, "Content-Range: bytes */4\r\n", b"")
            .json(),
        json!({"name":"resume","size":"4"})
    );
    server.stop();
}
