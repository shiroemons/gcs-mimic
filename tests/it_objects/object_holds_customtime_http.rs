use crate::common;
use common::TestDir;

use common::effects::Server;
use serde_json::{Value, json};

#[test]
fn stale_conditions_and_invalid_storage_classes_keep_wire_precedence() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("source", b"source").json_ok();
    let before = server.put("held", b"preserved").json_ok();
    for class in ["REGIONAL", "standard", "UNKNOWN"] {
        let metadata = json!({"name":"held","storageClass":class}).to_string();
        let bucket_metadata = json!({"storageClass":class}).to_string();
        let multipart = format!(
            "--boundary\r\nContent-Type: application/json\r\n\r\n{metadata}\r\n--boundary\r\nContent-Type: text/plain\r\n\r\nx\r\n--boundary--\r\n"
        );
        for (label, method, path, headers, body) in [
            (
                "bucket-patch",
                "PATCH",
                "/storage/v1/b/test-bucket?ifMetagenerationMatch=0",
                "",
                bucket_metadata.as_bytes(),
            ),
            (
                "bucket-put",
                "PUT",
                "/storage/v1/b/test-bucket?ifMetagenerationMatch=0",
                "",
                bucket_metadata.as_bytes(),
            ),
            (
                "resumable",
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=held&ifGenerationMatch=0",
                "",
                metadata.as_bytes(),
            ),
            (
                "multipart",
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=multipart&ifGenerationMatch=0",
                "Content-Type: multipart/related; boundary=boundary\r\n",
                multipart.as_bytes(),
            ),
            (
                "copy-destination",
                "POST",
                "/storage/v1/b/test-bucket/o/source/copyTo/b/test-bucket/o/held?ifGenerationMatch=0",
                "",
                metadata.as_bytes(),
            ),
            (
                "rewrite-destination",
                "POST",
                "/storage/v1/b/test-bucket/o/source/rewriteTo/b/test-bucket/o/held?ifGenerationMatch=0",
                "",
                metadata.as_bytes(),
            ),
            (
                "copy-source",
                "POST",
                "/storage/v1/b/test-bucket/o/source/copyTo/b/test-bucket/o/held?ifSourceGenerationMatch=0",
                "",
                metadata.as_bytes(),
            ),
            (
                "rewrite-source",
                "POST",
                "/storage/v1/b/test-bucket/o/source/rewriteTo/b/test-bucket/o/held?ifSourceGenerationMatch=0",
                "",
                metadata.as_bytes(),
            ),
        ] {
            let reply = common::http::Client {
                address: &server.address,
                host: "localhost",
                headers: if label == "multipart" {
                    ""
                } else {
                    "Content-Type: application/json\r\n"
                },
                timeout: std::time::Duration::from_secs(10),
            }
            .request_eof(method, path, headers, body)
            .into_joined();
            let value: Value = serde_json::from_slice(&reply.body).unwrap();
            let (status, reason, message) = if label.ends_with("-source") {
                (
                    412,
                    "conditionNotMet",
                    "At least one of the pre-conditions you specified did not hold.".to_owned(),
                )
            } else if class == "REGIONAL" {
                (400, "badRequest", if matches!(label, "resumable" | "multipart") {
                    "gcs-mimic does not support object API setting storageClass=REGIONAL: storageClass requires a region or zone; omit this option or header (for example, use projection=noAcl)."
                } else {
                    "gcs-mimic does not support bucket API setting storageClass requires a region or zone; omit this setting (for example, create a bucket with only name)."
                }.to_owned())
            } else {
                (400, "invalid", match label {
                    "bucket-patch" | "bucket-put" => format!("Invalid storage class \"{class}\""),
                    "resumable" | "multipart" => "Invalid object storageClass; use STANDARD, NEARLINE, COLDLINE or ARCHIVE".into(),
                    _ => "The storage class you specified is not valid.".into(),
                })
            };
            assert_eq!(reply.status, status, "{label} {class}");
            assert_eq!(
                value["error"]["errors"][0]["reason"], reason,
                "{label} {class}"
            );
            assert_eq!(value["error"]["message"], message, "{label} {class}");
        }
        let reply = server.request(
            "PUT",
            "/test-bucket/held",
            &format!("x-goog-storage-class: {class}\r\nx-goog-if-generation-match: 0\r\n"),
            b"x",
        );
        assert_eq!(reply.status, 400, "xml {class}");
        let message = if class == "REGIONAL" {
            "gcs-mimic does not support XML API storageClass=REGIONAL: storageClass requires a region or zone; omit this option or use a supported operation (for example, GET /BUCKET/OBJECT without extra query parameters)."
        } else {
            "Invalid x-goog-storage-class; use STANDARD, NEARLINE, COLDLINE or ARCHIVE."
        };
        assert_eq!(
            String::from_utf8(reply.body).unwrap(),
            format!(
                "<?xml version='1.0' encoding='UTF-8'?><Error><Code>InvalidArgument</Code><Message>{message}</Message></Error>"
            ),
            "xml {class}"
        );
        assert_eq!(server.get("held").json_ok(), before, "{class}");
    }
}

fn update(server: &Server, method: &str, value: Value) -> common::effects::Reply {
    server.update(method, "held", &value)
}

fn upload(server: &Server, name: &str, metadata: Value) -> Value {
    let initiated = server.resumable(name, &metadata);
    let uri: hyper::Uri = initiated.headers["location"].parse().unwrap();
    server
        .request(
            "PUT",
            uri.path_and_query().unwrap().as_str(),
            "Content-Range: bytes 0-2/3\r\n",
            b"abc",
        )
        .json_ok()
}

#[test]
fn derived_custom_time_format_validation_precedes_unsupported_semantics() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("source", b"source bytes").json_ok();
    let before = server.put("held", b"preserved bytes").json_ok();
    for (path, body) in [
        (
            "/storage/v1/b/test-bucket/o/source/copyTo/b/test-bucket/o/held",
            json!({"customTime":"invalid"}),
        ),
        (
            "/storage/v1/b/test-bucket/o/source/rewriteTo/b/test-bucket/o/held",
            json!({"customTime":"invalid"}),
        ),
        (
            "/storage/v1/b/test-bucket/o/held/compose",
            json!({"sourceObjects":[{"name":"source"}],"destination":{"customTime":"invalid"}}),
        ),
    ] {
        server
            .request("POST", path, "", body.to_string().as_bytes())
            .json_reason(400, "invalid");
        assert_eq!(server.get("held").json_ok(), before);
        assert_eq!(server.get("held?alt=media").body, b"preserved bytes");
    }
}

#[test]
fn temporary_hold_set_release_and_restart() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    let created = upload(
        &server,
        "held",
        json!({"temporaryHold":true,"customTime":"2026-10-20T01:02:03.123456789Z"}),
    );
    assert_eq!(created["temporaryHold"], true);
    server.stop();
    server = Server::start(&dir, false);
    assert_eq!(server.get("held").json_ok(), created);
    // A PUT replaces the resource, so a hold it names as null is released (recorded).
    assert!(
        update(
            &server,
            "PUT",
            json!({"temporaryHold":null,"customTime":created["customTime"]})
        )
        .json_ok()
        .get("temporaryHold")
        .is_none()
    );
    // Recorded: PATCH false on a hold that was never set (eventBasedHold after an upload that
    // named only temporaryHold) is reported back as false.
    assert_eq!(
        update(&server, "PATCH", json!({"temporaryHold":false})).json_ok()["temporaryHold"],
        false
    );
    assert_eq!(
        update(&server, "PATCH", json!({"temporaryHold":true})).json_ok()["temporaryHold"],
        true
    );
    assert_eq!(
        update(
            &server,
            "PUT",
            json!({"temporaryHold":false,"customTime":created["customTime"]})
        )
        .json_ok()["temporaryHold"],
        false
    );
    server
        .request("DELETE", "/storage/v1/b/test-bucket/o/held", "", b"")
        .expect(204);
    // A create-time false is kept as sent (the recording has no create-time false); null is not.
    assert_eq!(
        upload(&server, "defaults", json!({"temporaryHold":false}))["temporaryHold"],
        false
    );
    assert!(
        upload(&server, "defaults-null", json!({"temporaryHold":null}))
            .get("temporaryHold")
            .is_none()
    );
}

#[test]
fn temporary_hold_protects_delete_and_all_replacements() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("source", b"source").json_ok();
    upload(
        &server,
        "held",
        json!({"temporaryHold":true,"eventBasedHold":true}),
    );
    update(&server, "PATCH", json!({"eventBasedHold":false})).json_ok();
    for release in [false, true] {
        if release {
            update(&server, "PATCH", json!({"temporaryHold":false})).json_ok();
        }
        let requests = [
            (
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=held",
                b"replacement".as_slice(),
            ),
            (
                "POST",
                "/storage/v1/b/test-bucket/o/source/copyTo/b/test-bucket/o/held",
                b"{}".as_slice(),
            ),
            (
                "POST",
                "/storage/v1/b/test-bucket/o/source/rewriteTo/b/test-bucket/o/held",
                b"{}".as_slice(),
            ),
            (
                "POST",
                "/storage/v1/b/test-bucket/o/held/compose",
                br#"{"sourceObjects":[{"name":"source"}]}"#.as_slice(),
            ),
        ];
        for (method, path, body) in requests {
            let reply = server.request(method, path, "", body);
            if release {
                reply.json_ok();
            } else {
                reply.hold_error_reason();
            }
        }
        let reply = server.request("DELETE", "/storage/v1/b/test-bucket/o/held", "", b"");
        if release {
            reply.expect(204);
        } else {
            reply.hold_error_reason();
            assert_eq!(server.get("held?alt=media").body, b"abc");
            let xml = server.request("PUT", "/test-bucket/held", "", b"replacement");
            xml.expect(403);
            assert!(
                String::from_utf8(xml.body)
                    .unwrap()
                    .contains("<Code>ObjectUnderActiveHold</Code>")
            );
            assert_eq!(server.get("held?alt=media").body, b"abc");
        }
    }
}

#[test]
fn custom_time_monotonic_updates_are_atomic() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("held", b"abc").json_ok();
    let first = update(
        &server,
        "PATCH",
        json!({"customTime":"2026-10-20T01:02:03.123456789Z"}),
    )
    .json_ok();
    for method in ["PATCH", "PUT"] {
        // `null` is not here: Cloud Storage kept the custom time for it (recorded 200).
        for (value, reason) in [
            (json!("2026-10-20T01:02:03.123456788Z"), "invalid"),
            (json!("bad"), "invalid"),
            (json!("2026-02-30T01:02:03Z"), "invalid"),
            (json!(42), "invalidArgument"),
        ] {
            update(
                &server,
                method,
                json!({"customTime":value,"temporaryHold":true}),
            )
            .json_reason(400, reason);
            assert_eq!(server.get("held").json_ok(), first);
        }
    }
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/held",
            "Authorization: Bearer viewer\r\n",
            br#"{"temporaryHold":true}"#,
        )
        .json_reason(403, "forbidden");
    let same = common::without_acl(
        update(
            &server,
            "PUT",
            json!({"customTime":"2026-10-20T02:02:03.123456789+01:00"}),
        )
        .json_ok(),
    );
    assert_eq!(same["generation"], first["generation"]);
    // A PUT that omits customTime keeps it (recorded 200), unlike an attempt to remove it.
    assert_eq!(
        update(&server, "PUT", json!({})).json_ok()["customTime"],
        same["customTime"]
    );
    assert_eq!(
        update(&server, "PATCH", json!({})).json_ok()["customTime"],
        same["customTime"]
    );
    let later = update(
        &server,
        "PATCH",
        json!({"customTime":"2026-10-20T01:02:04Z"}),
    )
    .json_ok();
    assert_eq!(later["customTime"], "2026-10-20T01:02:04Z");
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/held?ifMetagenerationMatch=1",
            "Authorization: Bearer admin\r\n",
            br#"{"temporaryHold":true}"#,
        )
        .json_reason(412, "conditionNotMet");
    assert_eq!(server.get("held").json_ok(), later);
}

#[test]
fn hold_custom_time_migrate_existing_catalog() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    let original = server.put("held", b"legacy bytes").json_ok();
    server.stop();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    common::timestamp_support::downgrade_soft_delete_schema(&connection);
    connection.execute_batch("ALTER TABLE buckets DROP COLUMN retention_policy; ALTER TABLE buckets DROP COLUMN lifecycle; ALTER TABLE buckets DROP COLUMN uniform_bucket_level_access_locked_time; ALTER TABLE objects DROP COLUMN temporary_hold; ALTER TABLE objects DROP COLUMN custom_time; PRAGMA user_version = 21;").unwrap();
    drop(connection);
    server = Server::start(&dir, false);
    assert_eq!(server.get("held").json_ok(), original);
    update(
        &server,
        "PUT",
        json!({"customTime":"2026-10-20T01:02:03Z","temporaryHold":true}),
    )
    .json_ok();
    server.stop();
    server = Server::start(&dir, false);
    assert_eq!(server.get("held").json_ok()["temporaryHold"], true);
    assert_eq!(server.get("held?alt=media").body, b"legacy bytes");
}

#[test]
fn multipart_and_pending_resumable_preserve_hold_custom_time() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    let metadata = r#"{"name":"held","temporaryHold":true,"customTime":"2026-10-20T01:02:03Z"}"#;
    server.multipart("part", metadata, b"abc", "").expect(200);
    let created = server.get("held").json_ok();
    assert_eq!(created["temporaryHold"], true);
    assert_eq!(created["customTime"], "2026-10-20T01:02:03Z");
    let initiated = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=pending",
        "",
        metadata.as_bytes(),
    );
    let uri: hyper::Uri = initiated.headers["location"].parse().unwrap();
    server.stop();
    server = Server::start(&dir, false);
    let published = server
        .request(
            "PUT",
            uri.path_and_query().unwrap().as_str(),
            "Content-Range: bytes 0-2/3\r\n",
            b"abc",
        )
        .json_ok();
    assert_eq!(published["temporaryHold"], true);
    assert_eq!(published["customTime"], created["customTime"]);
    server
        .request("DELETE", "/storage/v1/b/test-bucket/o/pending", "", b"")
        .hold_error_reason();
}

/// Cloud Storage reports a hold as `false` once it was set, and omits one that never was
/// (recorded in the differential comparison). The state must survive a restart, and only a
/// true hold may block a delete. The recording covers a mistyped value ignored on PATCH while
/// the hold is `true` and `null` ignored on PUT while it is never set (`put-null-hold`); the
/// other combinations exercised here are inferred from those two.
#[test]
fn a_released_hold_reads_back_as_false_and_a_never_set_hold_is_omitted() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    let created = upload(&server, "held", json!({}));
    assert!(created.get("temporaryHold").is_none());
    assert!(created.get("eventBasedHold").is_none());
    let held = update(&server, "PATCH", json!({"temporaryHold":true})).json_ok();
    assert_eq!(held["temporaryHold"], true);
    assert!(held.get("eventBasedHold").is_none());
    let released = update(&server, "PATCH", json!({"temporaryHold":false})).json_ok();
    assert_eq!(released["temporaryHold"], false);
    assert!(released.get("eventBasedHold").is_none());
    // Inferred, not recorded: a mistyped value or null on PATCH changes nothing in any state.
    // The recording only has a mistyped value on PATCH while the hold is true and null on PUT
    // while it is never set.
    for body in [
        json!({"temporaryHold":"yes"}),
        json!({"temporaryHold":null}),
    ] {
        let unchanged = update(&server, "PATCH", body).json_ok();
        assert_eq!(unchanged["temporaryHold"], false);
    }
    server.stop();
    server = Server::start(&dir, false);
    assert_eq!(server.get("held").json_ok()["temporaryHold"], false);
    let selected = server
        .get("held?fields=name,temporaryHold,eventBasedHold")
        .json_ok();
    assert_eq!(selected["temporaryHold"], false);
    assert!(selected.get("eventBasedHold").is_none());
    // A released hold no longer blocks deletion; a PUT that omits it returns to never set.
    let replaced = update(&server, "PUT", json!({"metadata":{"a":"1"}})).json_ok();
    assert!(replaced.get("temporaryHold").is_none());
    update(&server, "PATCH", json!({"eventBasedHold":true})).json_ok();
    server
        .request(
            "DELETE",
            "/storage/v1/b/test-bucket/o/held",
            "Authorization: Bearer admin\r\n",
            b"",
        )
        .json_reason(403, "forbidden");
    let both = update(&server, "PATCH", json!({"eventBasedHold":false})).json_ok();
    assert_eq!(both["eventBasedHold"], false);
    assert!(both.get("temporaryHold").is_none());
    let deleted = server.request(
        "DELETE",
        "/storage/v1/b/test-bucket/o/held",
        "Authorization: Bearer admin\r\n",
        b"",
    );
    deleted.expect(204);
}
