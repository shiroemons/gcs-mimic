use crate::common;
use common::TestDir;
use common::effects::{Reply, Server, settings};
use serde_json::{Value, json};

const ADMIN: &str = "Authorization: Bearer admin\r\n";

fn setup(strict: bool) -> (TestDir, Server) {
    let dir = TestDir::new();
    let server = Server::start(&dir, strict);
    server.bucket();
    (dir, server)
}

fn path(source: &str, bucket: &str, target: &str, query: &str) -> String {
    format!("/storage/v1/b/test-bucket/o/{source}/copyTo/b/{bucket}/o/{target}{query}")
}

fn copy(server: &Server, source: &str, target: &str, query: &str, body: &[u8]) -> Reply {
    server.request(
        "POST",
        &path(source, "test-bucket", target, query),
        ADMIN,
        body,
    )
}

fn media(server: &Server, bucket: &str, name: &str) -> Vec<u8> {
    let reply = server.request(
        "GET",
        &format!("/storage/v1/b/{bucket}/o/{name}?alt=media"),
        ADMIN,
        b"",
    );
    reply.expect(200);
    reply.body
}

fn assert_no_transfer_files(dir: &TestDir) {
    common::eventually("no derived transfers or staging files", || {
        let count = common::catalog::count(
            &rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap(),
            "derived_transfers",
        );
        count == 0 && common::process::staging_count(dir) == 0
    });
}

#[test]
fn copy_token_replays_original_response_after_changes_deletion_and_restart() {
    let (dir, mut server) = setup(false);
    server.put("source", b"original bytes").json_ok();
    let path = path(
        "source",
        "test-bucket",
        "copy",
        "?ifGenerationMatch=0&projection=full",
    );
    let headers = "X-Goog-Gcs-Idempotency-Token: retained-copy\r\n";
    let body = br#"{"metadata":{"copied":"yes"}}"#;
    server.put("copy", b"existing destination").json_ok();
    server
        .request("POST", &path, headers, body)
        .json_reason(412, "conditionNotMet");
    server
        .request("DELETE", "/storage/v1/b/test-bucket/o/copy", ADMIN, b"")
        .expect(204);
    let original = server.request("POST", &path, headers, body);
    original.expect(200);
    assert!(original.json_ok()["acl"].is_array());
    server.put("source", b"changed source").json_ok();
    server.put("copy", b"changed destination").json_ok();
    assert_eq!(
        server.request("POST", &path, headers, body).body,
        original.body
    );
    assert_eq!(
        media(&server, "test-bucket", "copy"),
        b"changed destination"
    );
    for name in ["source", "copy"] {
        server
            .request(
                "DELETE",
                &format!("/storage/v1/b/test-bucket/o/{name}"),
                ADMIN,
                b"",
            )
            .expect(204);
    }
    server.stop();
    let server = Server::start(&dir, false);
    assert_eq!(
        server.request("POST", &path, headers, body).body,
        original.body
    );
    server.get("copy").json_reason(404, "notFound");
    for changed in [
        path.replace("ifGenerationMatch=0", "ifGenerationMatch=1"),
        path.replace("/o/copy?", "/o/other?"),
        format!("{path}&sourceGeneration=1"),
    ] {
        server
            .request("POST", &changed, headers, body)
            .json_reason(400, "invalid");
    }
    server
        .request("POST", &path, headers, b"{}")
        .json_reason(400, "invalid");
    server
        .request(
            "POST",
            &path,
            "X-Goog-Gcs-Idempotency-Token: fresh-copy\r\n",
            body,
        )
        .json_reason(404, "notFound");
    assert_no_transfer_files(&dir);
    assert_eq!(common::effects::blobs(&dir), 0);
}

#[test]
fn concurrent_copy_token_retries_publish_one_generation_without_extra_blobs() {
    let (dir, server) = setup(false);
    server.put("source", &vec![b'x'; 1024 * 1024 + 3]).json_ok();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let barrier = barrier.clone();
            let address = server.address.clone();
            std::thread::spawn(move || {
                barrier.wait();
                common::effects::request(
                    &address,
                    "POST",
                    &path("source", "test-bucket", "copy", "?ifGenerationMatch=0"),
                    "X-Goog-Gcs-Idempotency-Token: concurrent-copy\r\n",
                    b"",
                )
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap().json_ok())
        .collect();
    assert!(results.iter().all(|value| value == &results[0]));
    let last: i64 = rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
        .unwrap()
        .query_row("SELECT last_generation FROM object_generation", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(last.to_string(), results[0]["generation"].as_str().unwrap());
    assert_no_transfer_files(&dir);
    assert_eq!(common::effects::blobs(&dir), 2);
}

#[test]
fn copy_token_rechecks_source_destination_and_acl_permissions() {
    let (dir, server) = setup(true);
    server.put("source", b"data").json_ok();
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            ADMIN,
            br#"{"name":"destination-bucket"}"#,
        )
        .json_ok();
    let policy = |bucket: &str, role: Option<&str>| {
        let bindings = role
            .map(|role| vec![json!({"role":role,"members":["user:scoped@example.test"]})])
            .unwrap_or_default();
        server
            .request(
                "PUT",
                &format!("/storage/v1/b/{bucket}/iam"),
                ADMIN,
                json!({"bindings":bindings}).to_string().as_bytes(),
            )
            .json_ok();
    };
    policy("test-bucket", Some("roles/storage.objectViewer"));
    policy("destination-bucket", Some("roles/storage.objectAdmin"));
    let path = path(
        "source",
        "destination-bucket",
        "copy",
        "?projection=full&ifGenerationMatch=0",
    );
    let headers = "Authorization: Bearer scoped\r\nX-Goog-Gcs-Idempotency-Token: scoped-copy\r\n";
    let original = server.request("POST", &path, headers, b"");
    original.expect(200);
    assert!(original.json_ok()["acl"].is_array());
    policy("test-bucket", None);
    server
        .request("POST", &path, headers, b"")
        .json_reason(403, "forbidden");
    policy("test-bucket", Some("roles/storage.objectViewer"));
    policy("destination-bucket", None);
    server
        .request("POST", &path, headers, b"")
        .json_reason(403, "forbidden");
    policy("destination-bucket", Some("roles/storage.objectCreator"));
    let retry = server.request("POST", &path, headers, b"");
    retry.expect(200);
    assert_eq!(retry.json_ok(), original.json_ok());
    policy("destination-bucket", Some("roles/storage.objectAdmin"));
    assert_eq!(
        server.request("POST", &path, headers, b"").body,
        original.body
    );
    server
        .request(
            "POST",
            &path,
            "Authorization: Bearer admin\r\nX-Goog-Gcs-Idempotency-Token: scoped-copy\r\n",
            b"",
        )
        .json_reason(400, "invalid");
    assert_no_transfer_files(&dir);
}

#[test]
fn copy_token_rejects_recreated_source_or_destination_buckets() {
    for recreate in ["test-bucket", "destination-bucket"] {
        let (_dir, server) = setup(false);
        server.put("source", b"data").json_ok();
        server
            .request(
                "POST",
                "/storage/v1/b?project=local",
                ADMIN,
                br#"{"name":"destination-bucket"}"#,
            )
            .json_ok();
        let path = path("source", "destination-bucket", "copy", "");
        let headers = "X-Goog-Gcs-Idempotency-Token: incarnation-copy\r\n";
        server.request("POST", &path, headers, b"").json_ok();
        let object = if recreate == "test-bucket" {
            "source"
        } else {
            "copy"
        };
        server
            .request(
                "DELETE",
                &format!("/storage/v1/b/{recreate}/o/{object}"),
                ADMIN,
                b"",
            )
            .expect(204);
        server
            .request("DELETE", &format!("/storage/v1/b/{recreate}"), ADMIN, b"")
            .expect(204);
        server
            .request(
                "POST",
                "/storage/v1/b?project=local",
                ADMIN,
                json!({"name":recreate}).to_string().as_bytes(),
            )
            .json_ok();
        server
            .request("POST", &path, headers, b"")
            .json_reason(400, "invalid");
    }
}

#[test]
fn copy_keeps_an_explicit_false_hold_and_ignores_null_custom_time() {
    let (_dir, server) = setup(false);
    server.put("source", b"source bytes").json_ok();
    for body in [
        br#"{"temporaryHold":false}"#.as_slice(),
        br#"{"customTime":null}"#,
    ] {
        let result = copy(&server, "source", "target", "", body).json_ok();
        assert_eq!(result["name"], "target");
        // An explicit false is kept as sent (not recorded at create time); null names nothing.
        assert_eq!(
            result.get("temporaryHold").and_then(Value::as_bool),
            std::str::from_utf8(body)
                .unwrap()
                .contains("false")
                .then_some(false)
        );
        assert!(result.get("customTime").is_none());
        assert_eq!(media(&server, "test-bucket", "target"), b"source bytes");
    }
}

#[test]
fn copy_inherits_metadata_but_not_holds_and_explicit_body_replaces_it() {
    let (_dir, server) = setup(false);
    let source = server.put("dir%2F%E7%8C%AB%252F", b"contents").json_ok();
    server.request("PATCH", "/storage/v1/b/test-bucket/o/dir%2F%E7%8C%AB%252F", ADMIN,
        br#"{"contentType":"text/plain","contentEncoding":"identity","contentDisposition":"inline","contentLanguage":"en","cacheControl":"private","metadata":{"source":"one","other":"two"},"eventBasedHold":true}"#).json_ok();
    let copied = copy(&server, "dir%2F%E7%8C%AB%252F", "copy", "", b"").json_ok();
    assert_eq!(copied["metadata"], json!({"source":"one","other":"two"}));
    assert_eq!(copied["contentType"], "text/plain");
    assert_eq!(copied["contentLanguage"], "en");
    assert!(copied.get("eventBasedHold").is_none());
    assert_eq!(copied["md5Hash"], source["md5Hash"]);
    assert_eq!(copied["crc32c"], source["crc32c"]);
    assert_ne!(copied["generation"], source["generation"]);
    assert_eq!(copied["metageneration"], "1");
    assert_eq!(media(&server, "test-bucket", "copy"), b"contents");
    let null_copy = copy(&server, "dir%2F%E7%8C%AB%252F", "null-copy", "", b" null\n").json_ok();
    assert_eq!(null_copy["metadata"], copied["metadata"]);
    assert_eq!(null_copy["contentType"], copied["contentType"]);
    assert!(null_copy.get("eventBasedHold").is_none());
    let replaced = copy(&server, "dir%2F%E7%8C%AB%252F", "replaced", "", br#"{"bucket":"ignored","name":"ignored","metadata":{"new":"three"},"storageClass":"NEARLINE"}"#).json_ok();
    assert_eq!(replaced["bucket"], "test-bucket");
    assert_eq!(replaced["name"], "replaced");
    assert_eq!(replaced["metadata"], json!({"new":"three"}));
    assert_eq!(replaced["storageClass"], "NEARLINE");
    for field in [
        "contentType",
        "contentEncoding",
        "contentLanguage",
        "contentDisposition",
        "cacheControl",
    ] {
        assert!(replaced.get(field).is_none(), "{field}");
    }
    let empty = copy(&server, "dir%2F%E7%8C%AB%252F", "empty-metadata", "", b"{}").json_ok();
    // An empty resource keeps the source metadata like an absent one (recorded for `{}`).
    assert_eq!(empty["metadata"], copied["metadata"]);
    assert_eq!(empty["contentType"], copied["contentType"]);
    // A blank body and a null or empty metadata map inherit too (recorded 2026-11-13).
    for body in [&b" "[..], br#"{"metadata":null}"#, br#"{"metadata":{}}"#] {
        let kept = copy(&server, "dir%2F%E7%8C%AB%252F", "kept", "", body).json_ok();
        assert_eq!(kept["metadata"], copied["metadata"]);
        assert_eq!(kept["contentType"], copied["contentType"]);
        assert_eq!(kept["contentLanguage"], copied["contentLanguage"]);
    }
    assert_eq!(
        server.get("dir%2F%E7%8C%AB%252F").json_ok()["eventBasedHold"],
        true
    );
}

#[test]
fn copy_cross_bucket_same_object_history_fields_and_restart_preserve_bytes() {
    let (dir, mut server) = setup(false);
    settings(&dir, true, false);
    let payload = vec![b'x'; 200_003];
    let original = server.put("source", &payload).json_ok();
    server.put("source", b"new live bytes").json_ok();
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            ADMIN,
            br#"{"name":"destination-bucket"}"#,
        )
        .json_ok();
    let copied = server
        .request(
            "POST",
            &path(
                "source",
                "destination-bucket",
                "cross",
                &format!(
                    "?sourceGeneration={}&projection=full",
                    original["generation"].as_str().unwrap()
                ),
            ),
            ADMIN,
            b"",
        )
        .json_ok();
    // The copy receives the destination bucket's default object ACL.
    assert_eq!(copied["acl"][0]["bucket"], "destination-bucket");
    let copied = common::without_acl(copied);
    assert_eq!(copied["md5Hash"], original["md5Hash"]);
    assert_eq!(media(&server, "destination-bucket", "cross"), payload);
    let before = server.get("source").json_ok();
    let same = copy(
        &server,
        "source",
        "source",
        &format!(
            "?ifGenerationMatch={}",
            before["generation"].as_str().unwrap()
        ),
        b"",
    )
    .json_ok();
    assert_ne!(before["generation"], same["generation"]);
    assert_eq!(media(&server, "test-bucket", "source"), b"new live bytes");
    let mut retired = server
        .request(
            "GET",
            &format!(
                "/storage/v1/b/test-bucket/o/source?generation={}",
                before["generation"].as_str().unwrap()
            ),
            ADMIN,
            b"",
        )
        .json_ok();
    assert!(retired["timeDeleted"].is_string());
    retired.as_object_mut().unwrap().remove("timeDeleted");
    assert_eq!(retired, before);
    let selected = copy(
        &server,
        "source",
        "selected",
        "?fields=name,size&projection=noAcl",
        b"",
    )
    .json_ok();
    assert_eq!(selected, json!({"name":"selected","size":"14"}));
    server.put("zero", b"").json_ok();
    assert_eq!(
        copy(&server, "zero", "zero-copy", "", b"").json_ok()["size"],
        "0"
    );
    server.stop();
    let server = Server::start(&dir, false);
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/destination-bucket/o/cross",
                ADMIN,
                b""
            )
            .json_ok(),
        copied
    );
    assert_eq!(media(&server, "destination-bucket", "cross"), payload);
}

#[test]
fn copy_checks_all_source_and_destination_conditions_and_preserves_failed_targets() {
    let (dir, server) = setup(false);
    let source = server.put("source", b"source").json_ok();
    let target = server.put("target", b"target").json_ok();
    let generation = source["generation"].as_str().unwrap();
    for query in [
        "?ifSourceGenerationMatch=1".to_owned(),
        "?ifSourceGenerationMatch=0".to_owned(),
        format!("?ifSourceGenerationNotMatch={generation}"),
        "?ifSourceMetagenerationMatch=2".to_owned(),
        "?ifSourceMetagenerationNotMatch=1".to_owned(),
        "?ifGenerationMatch=0".to_owned(),
        "?ifMetagenerationMatch=2".to_owned(),
    ] {
        copy(&server, "source", "target", &query, b"").json_reason(412, "conditionNotMet");
        assert_eq!(server.get("target").json_ok(), target);
        assert_no_transfer_files(&dir);
    }
    // Cloud Storage answers an empty 304 when a destination not-match names the current
    // destination (recorded 2026-11 for ifGenerationNotMatch).
    for query in [
        format!(
            "?ifGenerationNotMatch={}",
            target["generation"].as_str().unwrap()
        ),
        "?ifMetagenerationNotMatch=1".to_owned(),
    ] {
        let reply = copy(&server, "source", "target", &query, b"");
        reply.expect(304);
        assert!(reply.body.is_empty(), "{query}");
        assert_eq!(server.get("target").json_ok(), target);
        assert_no_transfer_files(&dir);
    }
    // A failing match still wins over a not-match naming the current destination.
    copy(
        &server,
        "source",
        "target",
        "?ifGenerationMatch=0&ifMetagenerationNotMatch=1",
        b"",
    )
    .json_reason(412, "conditionNotMet");
    assert_eq!(server.get("target").json_ok(), target);
    copy(&server, "source", "missing", "?ifGenerationNotMatch=0", b"")
        .json_reason(412, "conditionNotMet");
    let first = copy(&server, "source", "missing", "?ifGenerationMatch=0", b"").json_ok();
    copy(
        &server,
        "source",
        "missing",
        "?ifGenerationNotMatch=0&ifMetagenerationMatch=1",
        b"",
    )
    .json_ok();
    assert_ne!(
        server.get("missing").json_ok()["generation"],
        first["generation"]
    );
    let combined = format!(
        "?ifSourceGenerationMatch={generation}&ifSourceGenerationNotMatch=0&ifSourceMetagenerationMatch=1&ifSourceMetagenerationNotMatch=2&ifGenerationMatch={}&ifGenerationNotMatch=0&ifMetagenerationMatch=1&ifMetagenerationNotMatch=2",
        target["generation"].as_str().unwrap()
    );
    copy(&server, "source", "target", &combined, b"").json_ok();
    assert_eq!(media(&server, "test-bucket", "target"), b"source");
    let same = copy(
        &server,
        "source",
        "source",
        &format!("?ifSourceGenerationMatch={generation}"),
        b"",
    )
    .json_ok();
    assert_ne!(same["generation"], source["generation"]);
    assert_eq!(media(&server, "test-bucket", "source"), b"source");
}

#[test]
fn copy_validates_checksums_and_holds_without_inheriting_source_hold() {
    let (dir, server) = setup(false);
    let original = server.put("source", b"bytes").json_ok();
    server.put("target", b"old").json_ok();
    copy(&server, "source", "target", "", br#"{"crc32c":"AAAAAA=="}"#).json_reason(400, "invalid");
    assert_eq!(media(&server, "test-bucket", "target"), b"old");
    assert_no_transfer_files(&dir);
    copy(
        &server,
        "source",
        "target",
        "",
        br#"{"md5Hash":"AAAAAAAAAAAAAAAAAAAAAA=="}"#,
    )
    .json_reason(400, "invalid");
    assert_eq!(media(&server, "test-bucket", "target"), b"old");
    assert_no_transfer_files(&dir);
    copy(
        &server,
        "source",
        "valid",
        "",
        json!({"md5Hash":original["md5Hash"],"crc32c":original["crc32c"]})
            .to_string()
            .as_bytes(),
    )
    .json_ok();
    settings(&dir, true, true);
    assert_eq!(
        copy(&server, "source", "held", "", b"").json_ok()["eventBasedHold"],
        true
    );
    copy(&server, "source", "held", "", b"").hold_error_reason();
    copy(&server, "held", "held", "", b"").hold_error_reason();
    assert_no_transfer_files(&dir);
    assert_eq!(
        copy(
            &server,
            "source",
            "override",
            "",
            br#"{"eventBasedHold":false}"#
        )
        .json_ok()["eventBasedHold"],
        false
    );
}

#[test]
fn copy_rejects_invalid_unsupported_and_missing_inputs_before_mutation() {
    let (dir, server) = setup(false);
    server.put("source", b"source").json_ok();
    for query in [
        "destinationPredefinedAcl=publicRead",
        "destinationKmsKeyName=key",
        "userProject=local",
        "dropContextGroups=custom",
    ] {
        copy(&server, "source", "target", &format!("?{query}"), b"").json_reason(400, "badRequest");
    }
    // prettyPrint only selects the body layout; a non-boolean value is still rejected.
    copy(&server, "source", "pretty", "?prettyPrint=true", b"").json_ok();
    copy(&server, "source", "target", "?prettyPrint=maybe", b"")
        .json_reason(400, "invalidParameter");
    for query in [
        "sourceGeneration=0",
        "sourceGeneration=-1",
        "ifSourceGenerationMatch=bad",
        "ifGenerationMatch=1&ifGenerationMatch=2",
    ] {
        copy(&server, "source", "target", &format!("?{query}"), b"")
            .json_reason(400, "invalidParameter");
    }
    // An undeclared parameter is ignored, as Cloud Storage did (case obj-undeclared-query-writes).
    copy(&server, "source", "undeclared", "?unknown=yes", b"").json_ok();
    for body in [
        br#"{"acl":[]}"#.as_slice(),
        br#"{"temporaryHold":true}"#,
        br#"{"customTime":"2026-10-20T01:02:03Z"}"#,
        br#"{"retention":{}}"#,
        br#"{"name":"target","generation":"1"}"#,
    ] {
        copy(&server, "source", "target", "", body).json_reason(400, "badRequest");
    }
    // Cloud Storage asks for a name when the resource names only a generation (recorded
    // 2026-11-13).
    copy(&server, "source", "target", "", br#"{"generation":"1"}"#).json_reason(400, "required");
    copy(&server, "source", "target", "", b"invalid").json_reason(400, "invalid");
    copy(&server, "missing", "target", "", b"").json_reason(404, "notFound");
    server
        .request(
            "POST",
            &path("source", "missing-bucket", "target", ""),
            ADMIN,
            b"",
        )
        .json_reason(404, "notFound");
    server
        .request(
            "POST",
            "/storage/v1/b/missing-bucket/o/source/copyTo/b/test-bucket/o/target",
            ADMIN,
            b"",
        )
        .json_reason(404, "notFound");
    copy(&server, "source", "bad%0Aname", "", b"").json_reason(400, "invalidParameter");
    server
        .request(
            "GET",
            &path("source", "test-bucket", "target", ""),
            ADMIN,
            b"",
        )
        .json_reason(405, "methodNotAllowed");
    // Incomplete customer-supplied keys fail with their documented reason before mutation.
    for header in [
        "X-Goog-Encryption-Key: key\r\n",
        "X-Goog-Copy-Source-Encryption-Key: key\r\n",
    ] {
        server
            .request(
                "POST",
                &path("source", "test-bucket", "target", ""),
                header,
                b"",
            )
            .json_reason(400, "customerEncryptionAlgorithmIsInvalid");
    }
    for header in [
        "X-Goog-Encryption-Kms-Key-Name: key\r\n",
        "If-Match: value\r\n",
        "Range: bytes=0-1\r\n",
    ] {
        server
            .request(
                "POST",
                &path("source", "test-bucket", "target", ""),
                header,
                b"",
            )
            .json_reason(400, "badRequest");
    }
    server.get("target").json_reason(404, "notFound");
    assert_no_transfer_files(&dir);
}

#[test]
fn strict_copy_requires_source_read_destination_create_and_delete_for_overwrite() {
    let (dir, server) = setup(true);
    server.put("source", b"source").json_ok();
    let route = path("source", "test-bucket", "copy", "");
    server
        .request("POST", &route, "Authorization: Bearer viewer\r\n", b"")
        .json_reason(403, "forbidden");
    server
        .request("POST", &route, "Authorization: Bearer creator\r\n", b"")
        .json_reason(403, "forbidden");
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        )
        .json_ok();
    let first = server
        .request("POST", &route, "Authorization: Bearer creator\r\n", b"")
        .json_ok();
    server
        .request("POST", &route, "Authorization: Bearer creator\r\n", b"")
        .json_reason(403, "forbidden");
    assert_eq!(
        first["owner"],
        json!({"entity": "user-creator@example.test"})
    );
    assert_eq!(server.get("copy").json_ok(), common::without_acl(first));
    server
        .request(
            "POST",
            &path("source", "test-bucket", "source", ""),
            "Authorization: Bearer creator\r\n",
            b"",
        )
        .json_reason(403, "forbidden");
    assert_no_transfer_files(&dir);
    server.request("POST", &route, ADMIN, b"").json_ok();
    assert_eq!(media(&server, "test-bucket", "copy"), b"source");
}

#[test]
fn concurrent_copy_create_if_absent_publishes_exactly_one_generation() {
    use std::sync::{Arc, Barrier};
    let (_dir, server) = setup(false);
    let payload = vec![b's'; 200_003];
    server.put("source", &payload).json_ok();
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let address = server.address.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                common::effects::request(
                    &address,
                    "POST",
                    &path("source", "test-bucket", "target", "?ifGenerationMatch=0"),
                    ADMIN,
                    b"",
                )
                .status
            })
        })
        .collect();
    barrier.wait();
    let mut statuses: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    statuses.sort();
    assert_eq!(statuses, [200, 412]);
    assert_eq!(media(&server, "test-bucket", "target"), payload);
}

#[test]
fn copy_preserves_composite_identity_and_inherited_class_until_class_or_location_changes() {
    let (dir, server) = setup(false);
    server.put("source", b"composite bytes").json_ok();
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap().execute("UPDATE objects SET component_count = 2, storage_class = 'NEARLINE' WHERE name = 'source'", []).unwrap();
    let inherited = copy(&server, "source", "inherited", "", b"").json_ok();
    assert_eq!(inherited["storageClass"], "NEARLINE");
    assert_eq!(inherited["componentCount"], 2);
    assert!(inherited.get("md5Hash").is_none());
    let replaced = copy(
        &server,
        "source",
        "different-class",
        "",
        br#"{"storageClass":"STANDARD"}"#,
    )
    .json_ok();
    assert_eq!(replaced["storageClass"], "STANDARD");
    assert_eq!(replaced["componentCount"], 1);
    assert!(replaced.get("md5Hash").is_none());
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            ADMIN,
            br#"{"name":"asia-bucket","location":"ASIA"}"#,
        )
        .json_ok();
    let moved = server
        .request(
            "POST",
            &path("source", "asia-bucket", "different-location", ""),
            ADMIN,
            b"",
        )
        .json_ok();
    assert_eq!(moved["storageClass"], "NEARLINE");
    assert_eq!(moved["componentCount"], 1);
    assert!(moved.get("md5Hash").is_none());
    assert_eq!(
        media(&server, "asia-bucket", "different-location"),
        b"composite bytes"
    );
}

#[test]
fn strict_cross_bucket_copy_uses_distinct_source_and_destination_grants() {
    let (dir, server) = setup(true);
    server.put("source", b"source").json_ok();
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            ADMIN,
            br#"{"name":"target-bucket"}"#,
        )
        .json_ok();
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        )
        .json_ok();
    let route = path("source", "target-bucket", "copy", "");
    server
        .request("POST", &route, "Authorization: Bearer creator\r\n", b"")
        .json_ok();
    let reverse = "/storage/v1/b/target-bucket/o/copy/copyTo/b/test-bucket/o/reverse";
    server
        .request("POST", reverse, "Authorization: Bearer creator\r\n", b"")
        .json_ok();
    assert_eq!(media(&server, "test-bucket", "reverse"), b"source");
    server
        .request(
            "POST",
            "/upload/storage/v1/b/target-bucket/o?uploadType=media&name=private-source",
            ADMIN,
            b"private",
        )
        .json_ok();
    server
        .request(
            "POST",
            "/storage/v1/b/target-bucket/o/private-source/copyTo/b/test-bucket/o/denied-private",
            "Authorization: Bearer creator\r\n",
            b"",
        )
        .json_reason(403, "forbidden");
    server
        .request(
            "GET",
            "/storage/v1/b/target-bucket/o/private-source?alt=media",
            "Authorization: Bearer creator\r\n",
            b"",
        )
        .json_reason(403, "forbidden");
    server
        .request("POST", &route, "Authorization: Bearer viewer\r\n", b"")
        .json_reason(403, "forbidden");
    server.request("PUT", "/storage/v1/b/target-bucket/iam", ADMIN, br#"{"bindings":[{"role":"roles/storage.objectCreator","members":["user:viewer@example.test"]}]}"#).json_ok();
    server
        .request(
            "POST",
            &path("source", "target-bucket", "viewer-copy", ""),
            "Authorization: Bearer viewer\r\n",
            b"",
        )
        .json_ok();
    server
        .request(
            "POST",
            &path("source", "test-bucket", "denied", ""),
            "Authorization: Bearer viewer\r\n",
            b"",
        )
        .json_reason(403, "forbidden");
    assert_no_transfer_files(&dir);
}
