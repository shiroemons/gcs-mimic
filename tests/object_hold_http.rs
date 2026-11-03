mod common;
use common::effects as object_effects_support;
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
    server.update(method, name, &value)
}
fn resumable(server: &Server, name: &str, metadata: Value) -> String {
    let reply = server.resumable(name, &metadata);
    reply.expect(200);
    let location = &reply.headers["location"];
    let index = location.find("/upload/").unwrap();
    location[index..].to_owned()
}
fn multipart(server: &Server, name: &str, hold: Option<bool>) -> Value {
    let mut metadata = json!({"name":name});
    if let Some(hold) = hold {
        metadata["eventBasedHold"] = json!(hold);
    }
    server
        .multipart(
            "boundary",
            &metadata.to_string(),
            b"bytes",
            "Authorization: Bearer admin\r\n",
        )
        .json_ok()
}

#[test]
fn default_hold_applies_to_all_upload_modes() {
    let (dir, server) = setup(false);
    assert_eq!(
        server.put("media", b"bytes").json_ok()["eventBasedHold"],
        true
    );
    assert_eq!(
        multipart(&server, "multipart", None)["eventBasedHold"],
        true
    );
    assert_eq!(
        multipart(&server, "override", Some(false))["eventBasedHold"],
        false
    );
    let path = resumable(&server, "resumable", json!({}));
    assert_eq!(
        server
            .request("PUT", &path, "Content-Range: bytes 0-4/5\r\n", b"bytes")
            .json_ok()["eventBasedHold"],
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
    assert_eq!(
        server
            .request("PUT", &path, "Content-Range: bytes 0-4/5\r\n", b"bytes")
            .json_ok()["eventBasedHold"],
        false
    );
    let path = resumable(&server, "changed-default", json!({}));
    settings(&dir, true, false);
    assert!(
        server
            .request("PUT", &path, "Content-Range: bytes 0-4/5\r\n", b"bytes")
            .json_ok()
            .get("eventBasedHold")
            .is_none()
    );
    assert_eq!(server.get("media").json_ok()["eventBasedHold"], true);
    assert!(
        server
            .put("after-disable", b"bytes")
            .json_ok()
            .get("eventBasedHold")
            .is_none()
    );
    assert_eq!(
        multipart(&server, "explicit", Some(true))["eventBasedHold"],
        true
    );
    let previous = server.get("override").json_ok();
    let path = resumable(&server, "override", json!({"eventBasedHold":false}));
    server
        .request("PUT", &path, "Content-Range: bytes 0-2/3\r\n", b"new")
        .json_ok();
    let old = server
        .get(&format!(
            "override?generation={}",
            previous["generation"].as_str().unwrap()
        ))
        .json_ok();
    assert!(old.get("timeDeleted").is_some());
    assert_eq!(old["eventBasedHold"], false);
}

#[test]
fn held_objects_reject_delete_and_replacement() {
    let (_dir, server) = setup(false);
    let original = server.put("held", b"original").json_ok();
    server
        .request("DELETE", "/storage/v1/b/test-bucket/o/held", "", b"")
        .hold_error();
    server.put("held", b"replacement").hold_error();
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
        .hold_error();
    assert_eq!(server.get("held").json_ok(), original);
    assert_eq!(server.get("held?alt=media").body, b"original");
    let path = resumable(&server, "race", json!({}));
    server.put("race", b"winner").json_ok();
    server
        .request("PUT", &path, "Content-Range: bytes 0-4/5\r\n", b"loser")
        .hold_error();
    assert_eq!(server.get("race?alt=media").body, b"winner");
    update(&server, "PATCH", "held", json!({"eventBasedHold":false})).json_ok();
    server.put("held", b"new").json_ok();
    update(
        &server,
        "PATCH",
        &format!(
            "held?generation={}",
            original["generation"].as_str().unwrap()
        ),
        json!({"eventBasedHold":true}),
    )
    .json_ok();
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
        .hold_error();
}

#[test]
fn event_hold_metadata_release_and_restart() {
    let (dir, mut server) = setup(false);
    let original = server.put("held", b"bytes").json_ok();
    let unchanged = update(
        &server,
        "PATCH",
        "held",
        json!({"metadata":{"key":"value"}}),
    )
    .json_ok();
    assert_eq!(unchanged["eventBasedHold"], true);
    // A PUT replaces the resource, so it keeps a hold only when it names it (recorded).
    let unchanged = update(
        &server,
        "PUT",
        "held",
        json!({"contentType":"text/plain","eventBasedHold":true}),
    )
    .json_ok();
    assert_eq!(unchanged["eventBasedHold"], true);
    update(
        &server,
        "PATCH",
        "held?ifMetagenerationMatch=1",
        json!({"eventBasedHold":false}),
    )
    .json_reason(412, "conditionNotMet");
    let released = update(
        &server,
        "PATCH",
        "held?ifMetagenerationMatch=3",
        json!({"eventBasedHold":false}),
    )
    .json_ok();
    assert_eq!(released["eventBasedHold"], false);
    assert_eq!(released["metageneration"], "4");
    for field in ["generation", "size", "md5Hash", "crc32c", "timeCreated"] {
        assert_eq!(released[field], original[field]);
    }
    server.stop();
    drop(server);
    let server = Server::start(&dir, false);
    assert_eq!(server.get("held").json_ok(), released);
    let generation = original["generation"].as_str().unwrap();
    server.put("held", b"new").json_ok();
    update(
        &server,
        "PATCH",
        &format!("held?generation={generation}"),
        json!({"eventBasedHold":true}),
    )
    .json_ok();
    let archived = update(
        &server,
        "PUT",
        &format!("held?generation={generation}"),
        json!({"eventBasedHold":false}),
    )
    .json_ok();
    assert_eq!(archived["eventBasedHold"], false);
    assert!(archived.get("timeDeleted").is_some());
    assert_eq!(server.get("held").json_ok()["eventBasedHold"], true);
    server
        .request(
            "DELETE",
            &format!("/storage/v1/b/test-bucket/o/held?generation={generation}"),
            "",
            b"",
        )
        .expect(204);
}

#[test]
fn event_hold_permissions_and_invalid_input() {
    let (_dir, server) = setup(true);
    let original = server.put("held", b"bytes").json_ok();
    // A hold of another type is not an error: Cloud Storage left the hold as it was (see the
    // parity tests), so only authorization is refused here.
    for token in ["viewer", "creator"] {
        server
            .request(
                "PATCH",
                "/storage/v1/b/test-bucket/o/held",
                &format!("Authorization: Bearer {token}\r\n"),
                br#"{"eventBasedHold":false}"#,
            )
            .json_reason(403, "forbidden");
        server
            .request(
                "DELETE",
                "/storage/v1/b/test-bucket/o/held",
                &format!("Authorization: Bearer {token}\r\n"),
                b"",
            )
            .json_reason(403, "forbidden");
    }
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/held",
            "Authorization: Bearer creator\r\n",
            b"",
        )
        .json_reason(403, "forbidden");
    assert_eq!(server.get("held").json_ok(), original);
    update(&server, "PUT", "held", json!({"eventBasedHold":false})).json_ok();
    server
        .request(
            "DELETE",
            "/storage/v1/b/test-bucket/o/held",
            "Authorization: Bearer admin\r\n",
            b"",
        )
        .expect(204);
}
