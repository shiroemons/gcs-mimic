mod object_effects_support;
use object_effects_support::{Server, common::TestDir, settings};
use serde_json::{Value, json};

fn setup(strict: bool) -> (TestDir, Server) {
    let dir = TestDir::new();
    let server = Server::start(&dir, strict);
    server.bucket();
    settings(&dir, true, true);
    (dir, server)
}
fn update(
    server: &Server,
    method: &str,
    name: &str,
    value: Value,
) -> object_effects_support::Reply {
    server.request(
        method,
        &format!("/storage/v1/b/test-bucket/o/{name}"),
        "Authorization: Bearer admin\r\n",
        value.to_string().as_bytes(),
    )
}
fn resumable(server: &Server, name: &str, metadata: Value) -> String {
    let reply = server.request(
        "POST",
        &format!("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name={name}"),
        "Authorization: Bearer admin\r\n",
        metadata.to_string().as_bytes(),
    );
    assert_eq!(reply.status, 200);
    let location = &reply.headers["location"];
    let index = location.find("/upload/").unwrap();
    location[index..].to_owned()
}
fn multipart(server: &Server, name: &str, hold: Option<bool>) -> Value {
    let mut metadata = json!({"name":name});
    if let Some(hold) = hold {
        metadata["eventBasedHold"] = json!(hold);
    }
    let body = format!(
        "--boundary\r\nContent-Type: application/json\r\n\r\n{metadata}\r\n--boundary\r\nContent-Type: text/plain\r\n\r\nbytes\r\n--boundary--\r\n"
    );
    // The normal helper sends JSON Content-Type, so supply the MIME request directly.
    use std::{
        io::{Read, Write},
        net::TcpStream,
    };
    let mut stream = TcpStream::connect(&server.address).unwrap();
    write!(stream,"POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer admin\r\nContent-Type: multipart/related; boundary=boundary\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    let (head, body) = reply.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("HTTP/1.1 200"), "{reply}");
    serde_json::from_str(body).unwrap()
}

#[test]
fn default_hold_applies_to_all_upload_modes() {
    let (dir, server) = setup(false);
    assert_eq!(server.put("media", b"bytes").json()["eventBasedHold"], true);
    assert_eq!(
        multipart(&server, "multipart", None)["eventBasedHold"],
        true
    );
    assert!(
        multipart(&server, "override", Some(false))
            .get("eventBasedHold")
            .is_none()
    );
    let path = resumable(&server, "resumable", json!({}));
    assert_eq!(
        server
            .request("PUT", &path, "Content-Range: bytes 0-4/5\r\n", b"bytes")
            .json()["eventBasedHold"],
        true
    );
    let path = resumable(
        &server,
        "resumable-override",
        json!({"eventBasedHold":false}),
    );
    // Explicit false must survive the serialized session and process restart.
    drop(server);
    let server = Server::start(&dir, false);
    assert!(
        server
            .request("PUT", &path, "Content-Range: bytes 0-4/5\r\n", b"bytes")
            .json()
            .get("eventBasedHold")
            .is_none()
    );
    let path = resumable(&server, "changed-default", json!({}));
    settings(&dir, true, false);
    assert!(
        server
            .request("PUT", &path, "Content-Range: bytes 0-4/5\r\n", b"bytes")
            .json()
            .get("eventBasedHold")
            .is_none()
    );
    assert_eq!(server.get("media").json()["eventBasedHold"], true);
    assert!(
        server
            .put("after-disable", b"bytes")
            .json()
            .get("eventBasedHold")
            .is_none()
    );
    assert_eq!(
        multipart(&server, "explicit", Some(true))["eventBasedHold"],
        true
    );
    let previous = server.get("override").json();
    let path = resumable(&server, "override", json!({"eventBasedHold":false}));
    server
        .request("PUT", &path, "Content-Range: bytes 0-2/3\r\n", b"new")
        .json();
    let old = server
        .get(&format!(
            "override?generation={}",
            previous["generation"].as_str().unwrap()
        ))
        .json();
    assert!(old.get("timeDeleted").is_some());
    assert!(old.get("eventBasedHold").is_none());
}

#[test]
fn held_objects_reject_delete_and_replacement() {
    let (_dir, server) = setup(false);
    let original = server.put("held", b"original").json();
    server
        .request("DELETE", "/storage/v1/b/test-bucket/o/held", "", b"")
        .error(403, "objectUnderActiveHold");
    server
        .put("held", b"replacement")
        .error(403, "objectUnderActiveHold");
    server
        .request(
            "DELETE",
            &format!(
                "/storage/v1/b/test-bucket/o/held?generation={}",
                original["generation"].as_str().unwrap()
            ),
            "",
            b"",
        )
        .error(403, "objectUnderActiveHold");
    assert_eq!(server.get("held").json(), original);
    assert_eq!(server.get("held?alt=media").body, b"original");
    let path = resumable(&server, "race", json!({}));
    server.put("race", b"winner").json();
    server
        .request("PUT", &path, "Content-Range: bytes 0-4/5\r\n", b"loser")
        .error(403, "objectUnderActiveHold");
    assert_eq!(server.get("race?alt=media").body, b"winner");
    update(&server, "PATCH", "held", json!({"eventBasedHold":false})).json();
    server.put("held", b"new").json();
    update(
        &server,
        "PATCH",
        &format!(
            "held?generation={}",
            original["generation"].as_str().unwrap()
        ),
        json!({"eventBasedHold":true}),
    )
    .json();
    server
        .request(
            "DELETE",
            &format!(
                "/storage/v1/b/test-bucket/o/held?generation={}",
                original["generation"].as_str().unwrap()
            ),
            "",
            b"",
        )
        .error(403, "objectUnderActiveHold");
}

#[test]
fn event_hold_metadata_release_and_restart() {
    let (dir, mut server) = setup(false);
    let original = server.put("held", b"bytes").json();
    let unchanged = update(
        &server,
        "PATCH",
        "held",
        json!({"metadata":{"key":"value"}}),
    )
    .json();
    assert_eq!(unchanged["eventBasedHold"], true);
    let unchanged = update(&server, "PUT", "held", json!({"contentType":"text/plain"})).json();
    assert_eq!(unchanged["eventBasedHold"], true);
    update(
        &server,
        "PATCH",
        "held?ifMetagenerationMatch=1",
        json!({"eventBasedHold":false}),
    )
    .error(412, "conditionNotMet");
    let released = update(
        &server,
        "PATCH",
        "held?ifMetagenerationMatch=3",
        json!({"eventBasedHold":false}),
    )
    .json();
    assert!(released.get("eventBasedHold").is_none());
    assert_eq!(released["metageneration"], "4");
    for field in ["generation", "size", "md5Hash", "crc32c", "timeCreated"] {
        assert_eq!(released[field], original[field]);
    }
    server.stop();
    drop(server);
    let server = Server::start(&dir, false);
    assert_eq!(server.get("held").json(), released);
    let generation = original["generation"].as_str().unwrap();
    server.put("held", b"new").json();
    update(
        &server,
        "PATCH",
        &format!("held?generation={generation}"),
        json!({"eventBasedHold":true}),
    )
    .json();
    let archived = update(
        &server,
        "PUT",
        &format!("held?generation={generation}"),
        json!({"eventBasedHold":false}),
    )
    .json();
    assert!(archived.get("eventBasedHold").is_none());
    assert!(archived.get("timeDeleted").is_some());
    assert_eq!(server.get("held").json()["eventBasedHold"], true);
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("/storage/v1/b/test-bucket/o/held?generation={generation}"),
                "",
                b""
            )
            .status,
        204
    );
}

#[test]
fn event_hold_permissions_and_invalid_input() {
    let (_dir, server) = setup(true);
    let original = server.put("held", b"bytes").json();
    for hold in [Value::Null, json!(1), json!("false"), json!({})] {
        update(&server, "PATCH", "held", json!({"eventBasedHold":hold}))
            .error(400, "invalidArgument");
    }
    for hold in [true, false] {
        update(&server, "PATCH", "held", json!({"temporaryHold":hold}))
            .error(501, "notImplemented");
    }
    for token in ["viewer", "creator"] {
        server
            .request(
                "PATCH",
                "/storage/v1/b/test-bucket/o/held",
                &format!("Authorization: Bearer {token}\r\n"),
                br#"{"eventBasedHold":false}"#,
            )
            .error(403, "forbidden");
        server
            .request(
                "DELETE",
                "/storage/v1/b/test-bucket/o/held",
                &format!("Authorization: Bearer {token}\r\n"),
                b"",
            )
            .error(403, "forbidden");
    }
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/held",
            "Authorization: Bearer creator\r\n",
            b"",
        )
        .error(403, "forbidden");
    assert_eq!(server.get("held").json(), original);
    update(&server, "PUT", "held", json!({"eventBasedHold":false})).json();
    assert_eq!(
        server
            .request(
                "DELETE",
                "/storage/v1/b/test-bucket/o/held",
                "Authorization: Bearer admin\r\n",
                b""
            )
            .status,
        204
    );
}
