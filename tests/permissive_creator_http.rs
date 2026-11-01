//! The permissive profile records a configured local creator as owner; it never authorizes.

mod object_effects_support;

use object_effects_support::{Server, common::TestDir};
use serde_json::{Value, json};

const BUCKET: &str = "/storage/v1/b/test-bucket";
const DEFAULT: &str = "user-local-user@gcs-mimic.invalid";

fn full(server: &Server, name: &str) -> Value {
    server
        .request(
            "GET",
            &format!("{BUCKET}/o/{name}?projection=full"),
            "",
            b"",
        )
        .json()
}

fn owner_and_acl(object: &Value) -> (String, Vec<String>) {
    (
        object["owner"]["entity"].as_str().unwrap().to_owned(),
        object["acl"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["entity"].as_str().unwrap().to_owned())
            .collect(),
    )
}

fn assert_creator(object: &Value, entity: &str) {
    let (owner, acl) = owner_and_acl(object);
    assert_eq!(owner, entity);
    // The recorded order: project teams first, then the creator.
    assert_eq!(
        acl,
        [
            "project-owners-0",
            "project-editors-0",
            "project-viewers-0",
            entity
        ]
    );
    let creator = object["acl"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["entity"] == entity)
        .unwrap();
    assert_eq!(creator["role"], "OWNER");
}

#[test]
fn default_creator_owns_every_creation_path_in_json_and_xml() {
    let dir = TestDir::new();
    let server = Server::start_with_args(&dir, &[]);
    server.bucket();
    server.put("plain", b"a").json();
    assert_creator(&full(&server, "plain"), DEFAULT);
    server.put("second", b"b").json();
    let copied = server
        .request(
            "POST",
            &format!("{BUCKET}/o/plain/copyTo/b/test-bucket/o/copied"),
            "",
            b"",
        )
        .json();
    assert_eq!(copied["name"], "copied");
    // Without the full projection a copy response names the owner but not the ACL.
    assert_eq!(copied["owner"], json!({"entity": DEFAULT}));
    assert!(copied.get("acl").is_none());
    assert_creator(&full(&server, "copied"), DEFAULT);
    let composed = server
        .request(
            "POST",
            &format!("{BUCKET}/o/composed/compose"),
            "Content-Type: application/json\r\n",
            json!({"sourceObjects":[{"name":"plain"},{"name":"second"}],"destination":{"contentType":"text/plain"}})
                .to_string()
                .as_bytes(),
        )
        .json();
    assert_eq!(composed["name"], "composed");
    assert_creator(&full(&server, "composed"), DEFAULT);
    let rewritten = server
        .request(
            "POST",
            &format!("{BUCKET}/o/plain/rewriteTo/b/test-bucket/o/rewritten"),
            "",
            b"",
        )
        .json();
    assert_eq!(rewritten["done"], true);
    assert_eq!(rewritten["resource"]["owner"], json!({"entity": DEFAULT}));
    assert_creator(&full(&server, "rewritten"), DEFAULT);
    let moved = server.request(
        "POST",
        &format!("{BUCKET}/o/second/moveTo/o/moved"),
        "",
        b"",
    );
    assert_eq!(
        moved.status,
        200,
        "{}",
        String::from_utf8_lossy(&moved.body)
    );
    assert_creator(&full(&server, "moved"), DEFAULT);
    let started = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=resumed",
        "",
        b"{}",
    );
    assert_eq!(started.status, 200);
    let location = started.headers["location"].clone();
    let path = location
        .split_once("//")
        .and_then(|(_, rest)| rest.split_once('/'))
        .map(|(_, path)| format!("/{path}"))
        .unwrap();
    let finished = server.request("PUT", &path, "", b"resumed bytes");
    assert_eq!(
        finished.status,
        200,
        "{}",
        String::from_utf8_lossy(&finished.body)
    );
    assert_creator(&full(&server, "resumed"), DEFAULT);
    // XML PUT is a creation path too.
    let xml = server.request("PUT", "/test-bucket/xml-object", "", b"x");
    assert_eq!(xml.status, 200);
    assert_creator(&full(&server, "xml-object"), DEFAULT);
}

#[test]
fn changing_or_disabling_the_creator_follows_restarts_without_authorizing_anything() {
    let dir = TestDir::new();
    let mut server = Server::start_with_args(&dir, &[]);
    server.bucket();
    server.put("kept", b"a").json();
    server.stop();

    let mut server = Server::start_with_args(&dir, &["--local-creator-email", "dev@example.test"]);
    // The stored creator is shown under the configured address after a restart.
    assert_creator(&full(&server, "kept"), "user-dev@example.test");
    server.put("fresh", b"b").json();
    assert_creator(&full(&server, "fresh"), "user-dev@example.test");
    server.stop();

    let server = Server::start_with_args(&dir, &["--local-creator-email", ""]);
    // Disabled: what was recorded stays; new objects keep the project owners.
    assert_creator(&full(&server, "kept"), "user-dev@example.test");
    server.put("plain-owners", b"c").json();
    let object = full(&server, "plain-owners");
    assert_eq!(object["owner"]["entity"], "project-owners-0");
    assert_eq!(object["acl"].as_array().unwrap().len(), 3);
    // Anonymous requests were always allowed in permissive mode; the creator is only a label.
    assert_eq!(
        server
            .request("DELETE", &format!("{BUCKET}/o/kept"), "", b"")
            .status,
        204
    );
}

#[test]
fn legacy_profile_and_invalid_addresses_are_not_accepted_or_changed() {
    let dir = TestDir::new();
    let mut server = Server::start_with_args(&dir, &["--compat-profile", "legacy-emulator"]);
    server.bucket();
    server.put("legacy", b"a").json();
    let object = full(&server, "legacy");
    assert_ne!(object["owner"]["entity"], DEFAULT);
    let copied = server
        .request(
            "POST",
            &format!("{BUCKET}/o/legacy/copyTo/b/test-bucket/o/legacy-copy"),
            "",
            b"",
        )
        .json();
    assert!(copied.get("owner").is_none());
    assert!(
        !object["acl"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["entity"] == DEFAULT)
    );
    server.stop();
    for args in [
        &[
            "--compat-profile",
            "legacy-emulator",
            "--local-creator-email",
            "a@example.test",
        ][..],
        &["--local-creator-email", "not-an-email"],
    ] {
        let output = object_effects_support::common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&dir.0)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("local_creator_email"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
