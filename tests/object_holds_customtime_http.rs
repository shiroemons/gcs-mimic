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
            .error(400, "invalidArgument");
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
    assert_eq!(
        update(
            &server,
            "PUT",
            json!({"temporaryHold":null,"customTime":created["customTime"]})
        )
        .json()["temporaryHold"],
        true
    );
    assert!(
        update(&server, "PATCH", json!({"temporaryHold":false}))
            .json()
            .get("temporaryHold")
            .is_none()
    );
    assert_eq!(
        update(&server, "PATCH", json!({"temporaryHold":true})).json()["temporaryHold"],
        true
    );
    assert!(
        update(
            &server,
            "PUT",
            json!({"temporaryHold":false,"customTime":created["customTime"]})
        )
        .json()
        .get("temporaryHold")
        .is_none()
    );
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket/o/held", "", b"")
            .status,
        204
    );
    for hold in [json!(false), json!(null)] {
        assert!(
            upload(&server, "defaults", json!({"temporaryHold":hold}))
                .get("temporaryHold")
                .is_none()
        );
    }
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
                reply.error(403, "objectUnderActiveHold");
            }
        }
        let reply = server.request("DELETE", "/storage/v1/b/test-bucket/o/held", "", b"");
        if release {
            assert_eq!(reply.status, 204);
        } else {
            reply.error(403, "objectUnderActiveHold");
            assert_eq!(server.get("held?alt=media").body, b"abc");
            let xml = server.request("PUT", "/test-bucket/held", "", b"replacement");
            assert_eq!(xml.status, 403);
            assert!(
                String::from_utf8(xml.body)
                    .unwrap()
                    .contains("<Code>AccessDenied</Code>")
            );
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
        for value in [
            json!(null),
            json!("2026-10-20T01:02:03.123456788Z"),
            json!("bad"),
            json!("2026-02-30T01:02:03Z"),
            json!(42),
        ] {
            update(
                &server,
                method,
                json!({"customTime":value,"temporaryHold":true}),
            )
            .error(400, "invalidArgument");
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
    let same = update(
        &server,
        "PUT",
        json!({"customTime":"2026-10-20T02:02:03.123456789+01:00"}),
    )
    .json();
    assert_eq!(same["generation"], first["generation"]);
    update(&server, "PUT", json!({})).error(400, "invalidArgument");
    assert_eq!(server.get("held").json(), same);
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
    connection.execute_batch("ALTER TABLE objects DROP COLUMN temporary_hold; ALTER TABLE objects DROP COLUMN custom_time; PRAGMA user_version = 21;").unwrap();
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
        .error(403, "objectUnderActiveHold");
}
