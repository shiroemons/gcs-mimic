mod object_effects_support;
use object_effects_support::{Reply, Server, common::TestDir, settings};
use serde_json::json;

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
    assert_eq!(reply.status, 200);
    reply.body
}

#[test]
fn copy_inherits_metadata_but_not_holds_and_explicit_body_replaces_it() {
    let (_dir, server) = setup(false);
    let source = server.put("dir%2F%E7%8C%AB%252F", b"contents").json();
    server.request("PATCH", "/storage/v1/b/test-bucket/o/dir%2F%E7%8C%AB%252F", ADMIN,
        br#"{"contentType":"text/plain","contentEncoding":"identity","contentDisposition":"inline","contentLanguage":"en","cacheControl":"private","metadata":{"source":"one","other":"two"},"eventBasedHold":true}"#).json();
    let copied = copy(&server, "dir%2F%E7%8C%AB%252F", "copy", "", b"").json();
    assert_eq!(copied["metadata"], json!({"source":"one","other":"two"}));
    assert_eq!(copied["contentType"], "text/plain");
    assert_eq!(copied["contentLanguage"], "en");
    assert!(copied.get("eventBasedHold").is_none());
    assert_eq!(copied["md5Hash"], source["md5Hash"]);
    assert_eq!(copied["crc32c"], source["crc32c"]);
    assert_ne!(copied["generation"], source["generation"]);
    assert_eq!(copied["metageneration"], "1");
    assert_eq!(media(&server, "test-bucket", "copy"), b"contents");
    let null_copy = copy(&server, "dir%2F%E7%8C%AB%252F", "null-copy", "", b" null\n").json();
    assert_eq!(null_copy["metadata"], copied["metadata"]);
    assert_eq!(null_copy["contentType"], copied["contentType"]);
    assert!(null_copy.get("eventBasedHold").is_none());
    let replaced = copy(&server, "dir%2F%E7%8C%AB%252F", "replaced", "", br#"{"bucket":"ignored","name":"ignored","metadata":{"new":"three"},"storageClass":"NEARLINE"}"#).json();
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
    let empty = copy(&server, "dir%2F%E7%8C%AB%252F", "empty-metadata", "", b"{}").json();
    assert!(empty.get("metadata").is_none());
    assert!(empty.get("contentType").is_none());
    assert_eq!(
        server.get("dir%2F%E7%8C%AB%252F").json()["eventBasedHold"],
        true
    );
}

#[test]
fn copy_cross_bucket_same_object_history_fields_and_restart_preserve_bytes() {
    let (dir, mut server) = setup(false);
    settings(&dir, true, false);
    let payload = vec![b'x'; 200_003];
    let original = server.put("source", &payload).json();
    server.put("source", b"new live bytes").json();
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            ADMIN,
            br#"{"name":"destination-bucket"}"#,
        )
        .json();
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
        .json();
    assert_eq!(copied["md5Hash"], original["md5Hash"]);
    assert_eq!(media(&server, "destination-bucket", "cross"), payload);
    let before = server.get("source").json();
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
    .json();
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
        .json();
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
    .json();
    assert_eq!(selected, json!({"name":"selected","size":"14"}));
    server.put("zero", b"").json();
    assert_eq!(
        copy(&server, "zero", "zero-copy", "", b"").json()["size"],
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
            .json(),
        copied
    );
    assert_eq!(media(&server, "destination-bucket", "cross"), payload);
}

#[test]
fn copy_checks_all_source_and_destination_conditions_and_preserves_failed_targets() {
    let (_dir, server) = setup(false);
    let source = server.put("source", b"source").json();
    let target = server.put("target", b"target").json();
    let generation = source["generation"].as_str().unwrap();
    for query in [
        "?ifSourceGenerationMatch=1".to_owned(),
        format!("?ifSourceGenerationNotMatch={generation}"),
        "?ifSourceMetagenerationMatch=2".to_owned(),
        "?ifSourceMetagenerationNotMatch=1".to_owned(),
        "?ifGenerationMatch=0".to_owned(),
        format!(
            "?ifGenerationNotMatch={}",
            target["generation"].as_str().unwrap()
        ),
        "?ifMetagenerationMatch=2".to_owned(),
        "?ifMetagenerationNotMatch=1".to_owned(),
    ] {
        copy(&server, "source", "target", &query, b"").error(412, "conditionNotMet");
        assert_eq!(server.get("target").json(), target);
    }
    copy(&server, "source", "missing", "?ifGenerationNotMatch=0", b"")
        .error(412, "conditionNotMet");
    let first = copy(&server, "source", "missing", "?ifGenerationMatch=0", b"").json();
    copy(
        &server,
        "source",
        "missing",
        "?ifGenerationNotMatch=0&ifMetagenerationMatch=1",
        b"",
    )
    .json();
    assert_ne!(
        server.get("missing").json()["generation"],
        first["generation"]
    );
    let combined = format!(
        "?ifSourceGenerationMatch={generation}&ifSourceGenerationNotMatch=0&ifSourceMetagenerationMatch=1&ifSourceMetagenerationNotMatch=2&ifGenerationMatch={}&ifGenerationNotMatch=0&ifMetagenerationMatch=1&ifMetagenerationNotMatch=2",
        target["generation"].as_str().unwrap()
    );
    copy(&server, "source", "target", &combined, b"").json();
    assert_eq!(media(&server, "test-bucket", "target"), b"source");
    let same = copy(
        &server,
        "source",
        "source",
        &format!("?ifSourceGenerationMatch={generation}"),
        b"",
    )
    .json();
    assert_ne!(same["generation"], source["generation"]);
    assert_eq!(media(&server, "test-bucket", "source"), b"source");
}

#[test]
fn copy_validates_checksums_and_holds_without_inheriting_source_hold() {
    let (dir, server) = setup(false);
    let original = server.put("source", b"bytes").json();
    server.put("target", b"old").json();
    copy(&server, "source", "target", "", br#"{"crc32c":"AAAAAA=="}"#).error(400, "invalid");
    assert_eq!(media(&server, "test-bucket", "target"), b"old");
    copy(
        &server,
        "source",
        "valid",
        "",
        json!({"md5Hash":original["md5Hash"],"crc32c":original["crc32c"]})
            .to_string()
            .as_bytes(),
    )
    .json();
    settings(&dir, true, true);
    assert_eq!(
        copy(&server, "source", "held", "", b"").json()["eventBasedHold"],
        true
    );
    copy(&server, "source", "held", "", b"").error(403, "objectUnderActiveHold");
    assert!(
        copy(
            &server,
            "source",
            "override",
            "",
            br#"{"eventBasedHold":false}"#
        )
        .json()
        .get("eventBasedHold")
        .is_none()
    );
}

#[test]
fn copy_rejects_invalid_unsupported_and_missing_inputs_before_mutation() {
    let (_dir, server) = setup(false);
    server.put("source", b"source").json();
    for query in [
        "destinationPredefinedAcl=publicRead",
        "destinationKmsKeyName=key",
        "userProject=local",
        "dropContextGroups=custom",
        "prettyPrint=true",
    ] {
        copy(&server, "source", "target", &format!("?{query}"), b"").error(501, "notImplemented");
    }
    for query in [
        "sourceGeneration=0",
        "sourceGeneration=-1",
        "ifSourceGenerationMatch=bad",
        "ifGenerationMatch=1&ifGenerationMatch=2",
        "unknown=yes",
    ] {
        copy(&server, "source", "target", &format!("?{query}"), b"").error(400, "invalidParameter");
    }
    for body in [
        br#"{"acl":[]}"#.as_slice(),
        br#"{"temporaryHold":true}"#,
        br#"{"retention":{}}"#,
        br#"{"unknown":true}"#,
        br#"{"generation":"1"}"#,
    ] {
        copy(&server, "source", "target", "", body).error(501, "notImplemented");
    }
    copy(&server, "source", "target", "", b"invalid").error(400, "parseError");
    copy(&server, "missing", "target", "", b"").error(404, "notFound");
    server
        .request(
            "POST",
            &path("source", "missing-bucket", "target", ""),
            ADMIN,
            b"",
        )
        .error(404, "notFound");
    server
        .request(
            "POST",
            "/storage/v1/b/missing-bucket/o/source/copyTo/b/test-bucket/o/target",
            ADMIN,
            b"",
        )
        .error(404, "notFound");
    copy(&server, "source", "bad%0Aname", "", b"").error(400, "invalidParameter");
    server
        .request(
            "GET",
            &path("source", "test-bucket", "target", ""),
            ADMIN,
            b"",
        )
        .error(405, "methodNotAllowed");
    for header in [
        "X-Goog-Encryption-Key: key\r\n",
        "X-Goog-Copy-Source-Encryption-Key: key\r\n",
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
            .error(501, "notImplemented");
    }
    server.get("target").error(404, "notFound");
}

#[test]
fn strict_copy_requires_source_read_destination_create_and_delete_for_overwrite() {
    let (_dir, server) = setup(true);
    server.put("source", b"source").json();
    let route = path("source", "test-bucket", "copy", "");
    server
        .request("POST", &route, "Authorization: Bearer viewer\r\n", b"")
        .error(403, "forbidden");
    server
        .request("POST", &route, "Authorization: Bearer creator\r\n", b"")
        .error(403, "forbidden");
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        )
        .json();
    let first = server
        .request("POST", &route, "Authorization: Bearer creator\r\n", b"")
        .json();
    server
        .request("POST", &route, "Authorization: Bearer creator\r\n", b"")
        .error(403, "forbidden");
    assert_eq!(server.get("copy").json(), first);
    server.request("POST", &route, ADMIN, b"").json();
    assert_eq!(media(&server, "test-bucket", "copy"), b"source");
}

#[test]
fn concurrent_copy_create_if_absent_publishes_exactly_one_generation() {
    use std::sync::{Arc, Barrier};
    let (_dir, server) = setup(false);
    server.put("source", b"source").json();
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let address = server.address.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                object_effects_support::request(
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
    assert_eq!(media(&server, "test-bucket", "target"), b"source");
}

#[test]
fn copy_preserves_composite_identity_and_inherited_class_until_class_or_location_changes() {
    let (dir, server) = setup(false);
    server.put("source", b"composite bytes").json();
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap().execute("UPDATE objects SET component_count = 2, storage_class = 'NEARLINE' WHERE name = 'source'", []).unwrap();
    let inherited = copy(&server, "source", "inherited", "", b"").json();
    assert_eq!(inherited["storageClass"], "NEARLINE");
    assert_eq!(inherited["componentCount"], 2);
    assert!(inherited.get("md5Hash").is_none());
    let replaced = copy(&server, "source", "different-class", "", b"{}").json();
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
        .json();
    let moved = server
        .request(
            "POST",
            &path("source", "asia-bucket", "different-location", ""),
            ADMIN,
            b"",
        )
        .json();
    assert_eq!(moved["storageClass"], "NEARLINE");
    assert_eq!(moved["componentCount"], 1);
    assert!(moved.get("md5Hash").is_none());
    assert_eq!(
        media(&server, "asia-bucket", "different-location"),
        b"composite bytes"
    );
}
