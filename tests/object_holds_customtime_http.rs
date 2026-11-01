mod object_effects_support;

use object_effects_support::{Server, common::TestDir};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpStream,
};

fn update(server: &Server, method: &str, value: Value) -> object_effects_support::Reply {
    server.request(
        method,
        "/storage/v1/b/test-bucket/o/held",
        "Authorization: Bearer admin\r\n",
        value.to_string().as_bytes(),
    )
}

fn upload(server: &Server, name: &str, metadata: Value) -> Value {
    let initiated = server.request(
        "POST",
        &format!("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name={name}"),
        "Authorization: Bearer admin\r\n",
        metadata.to_string().as_bytes(),
    );
    let uri: hyper::Uri = initiated.headers["location"].parse().unwrap();
    server
        .request(
            "PUT",
            uri.path_and_query().unwrap().as_str(),
            "Content-Range: bytes 0-2/3\r\n",
            b"abc",
        )
        .json()
}

#[test]
fn derived_custom_time_format_validation_precedes_unsupported_semantics() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("source", b"source bytes").json();
    let before = server.put("held", b"preserved bytes").json();
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
            .error(400, "invalid");
        assert_eq!(server.get("held").json(), before);
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
    assert_eq!(server.get("held").json(), created);
    // A PUT replaces the resource, so a hold it names as null is released (recorded).
    assert!(
        update(
            &server,
            "PUT",
            json!({"temporaryHold":null,"customTime":created["customTime"]})
        )
        .json()
        .get("temporaryHold")
        .is_none()
    );
    // Recorded: PATCH false on a hold that was never set (eventBasedHold after an upload that
    // named only temporaryHold) is reported back as false.
    assert_eq!(
        update(&server, "PATCH", json!({"temporaryHold":false})).json()["temporaryHold"],
        false
    );
    assert_eq!(
        update(&server, "PATCH", json!({"temporaryHold":true})).json()["temporaryHold"],
        true
    );
    assert_eq!(
        update(
            &server,
            "PUT",
            json!({"temporaryHold":false,"customTime":created["customTime"]})
        )
        .json()["temporaryHold"],
        false
    );
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket/o/held", "", b"")
            .status,
        204
    );
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
    server.put("source", b"source").json();
    upload(
        &server,
        "held",
        json!({"temporaryHold":true,"eventBasedHold":true}),
    );
    update(&server, "PATCH", json!({"eventBasedHold":false})).json();
    for release in [false, true] {
        if release {
            update(&server, "PATCH", json!({"temporaryHold":false})).json();
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
                reply.json();
            } else {
                reply.hold_error();
            }
        }
        let reply = server.request("DELETE", "/storage/v1/b/test-bucket/o/held", "", b"");
        if release {
            assert_eq!(reply.status, 204);
        } else {
            reply.hold_error();
            assert_eq!(server.get("held?alt=media").body, b"abc");
            let xml = server.request("PUT", "/test-bucket/held", "", b"replacement");
            assert_eq!(xml.status, 403);
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
    server.put("held", b"abc").json();
    let first = update(
        &server,
        "PATCH",
        json!({"customTime":"2026-10-20T01:02:03.123456789Z"}),
    )
    .json();
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
            .error(400, reason);
            assert_eq!(server.get("held").json(), first);
        }
    }
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/held",
            "Authorization: Bearer viewer\r\n",
            br#"{"temporaryHold":true}"#,
        )
        .error(403, "forbidden");
    let same = object_effects_support::common::without_acl(
        update(
            &server,
            "PUT",
            json!({"customTime":"2026-10-20T02:02:03.123456789+01:00"}),
        )
        .json(),
    );
    assert_eq!(same["generation"], first["generation"]);
    // A PUT that omits customTime keeps it (recorded 200), unlike an attempt to remove it.
    assert_eq!(
        update(&server, "PUT", json!({})).json()["customTime"],
        same["customTime"]
    );
    assert_eq!(
        update(&server, "PATCH", json!({})).json()["customTime"],
        same["customTime"]
    );
    let later = update(
        &server,
        "PATCH",
        json!({"customTime":"2026-10-20T01:02:04Z"}),
    )
    .json();
    assert_eq!(later["customTime"], "2026-10-20T01:02:04Z");
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/held?ifMetagenerationMatch=1",
            "Authorization: Bearer admin\r\n",
            br#"{"temporaryHold":true}"#,
        )
        .error(412, "conditionNotMet");
    assert_eq!(server.get("held").json(), later);
}

#[test]
fn hold_custom_time_migrate_existing_catalog() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    let original = server.put("held", b"legacy bytes").json();
    server.stop();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    object_effects_support::common::timestamp_support::downgrade_soft_delete_schema(&connection);
    connection.execute_batch("ALTER TABLE buckets DROP COLUMN retention_policy; ALTER TABLE buckets DROP COLUMN lifecycle; ALTER TABLE buckets DROP COLUMN uniform_bucket_level_access_locked_time; ALTER TABLE objects DROP COLUMN temporary_hold; ALTER TABLE objects DROP COLUMN custom_time; PRAGMA user_version = 21;").unwrap();
    drop(connection);
    server = Server::start(&dir, false);
    assert_eq!(server.get("held").json(), original);
    update(
        &server,
        "PUT",
        json!({"customTime":"2026-10-20T01:02:03Z","temporaryHold":true}),
    )
    .json();
    server.stop();
    server = Server::start(&dir, false);
    assert_eq!(server.get("held").json()["temporaryHold"], true);
    assert_eq!(server.get("held?alt=media").body, b"legacy bytes");
}

#[test]
fn multipart_and_pending_resumable_preserve_hold_custom_time() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    let metadata = r#"{"name":"held","temporaryHold":true,"customTime":"2026-10-20T01:02:03Z"}"#;
    let multipart = format!(
        "--part\r\nContent-Type: application/json\r\n\r\n{metadata}\r\n--part\r\nContent-Type: text/plain\r\n\r\nabc\r\n--part--\r\n"
    );
    let mut stream = TcpStream::connect(&server.address).unwrap();
    write!(stream, "POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=part\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{multipart}", multipart.len()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let created = server.get("held").json();
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
        .json();
    assert_eq!(published["temporaryHold"], true);
    assert_eq!(published["customTime"], created["customTime"]);
    server
        .request("DELETE", "/storage/v1/b/test-bucket/o/pending", "", b"")
        .hold_error();
}

/// Cloud Storage reports a hold as `false` once it was set, and omits one that never was
/// (recorded in the differential comparison). The state must survive a restart, and only a
/// true hold may block a delete.
#[test]
fn a_released_hold_reads_back_as_false_and_a_never_set_hold_is_omitted() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    let created = upload(&server, "held", json!({}));
    assert!(created.get("temporaryHold").is_none());
    assert!(created.get("eventBasedHold").is_none());
    let held = update(&server, "PATCH", json!({"temporaryHold":true})).json();
    assert_eq!(held["temporaryHold"], true);
    assert!(held.get("eventBasedHold").is_none());
    let released = update(&server, "PATCH", json!({"temporaryHold":false})).json();
    assert_eq!(released["temporaryHold"], false);
    assert!(released.get("eventBasedHold").is_none());
    // A value of the wrong type or null changes nothing, whatever state the hold is in.
    for body in [
        json!({"temporaryHold":"yes"}),
        json!({"temporaryHold":null}),
    ] {
        let unchanged = update(&server, "PATCH", body).json();
        assert_eq!(unchanged["temporaryHold"], false);
    }
    server.stop();
    server = Server::start(&dir, false);
    assert_eq!(server.get("held").json()["temporaryHold"], false);
    let selected = server
        .get("held?fields=name,temporaryHold,eventBasedHold")
        .json();
    assert_eq!(selected["temporaryHold"], false);
    assert!(selected.get("eventBasedHold").is_none());
    // A released hold no longer blocks deletion; a PUT that omits it returns to never set.
    let replaced = update(&server, "PUT", json!({"metadata":{"a":"1"}})).json();
    assert!(replaced.get("temporaryHold").is_none());
    update(&server, "PATCH", json!({"eventBasedHold":true})).json();
    server
        .request(
            "DELETE",
            "/storage/v1/b/test-bucket/o/held",
            "Authorization: Bearer admin\r\n",
            b"",
        )
        .error(403, "forbidden");
    let both = update(&server, "PATCH", json!({"eventBasedHold":false})).json();
    assert_eq!(both["eventBasedHold"], false);
    assert!(both.get("temporaryHold").is_none());
    let deleted = server.request(
        "DELETE",
        "/storage/v1/b/test-bucket/o/held",
        "Authorization: Bearer admin\r\n",
        b"",
    );
    assert_eq!(deleted.status, 204);
}
