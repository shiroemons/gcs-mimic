use crate::common;

use common::TestDir;
use common::ingress::{Server, gzip};
use serde_json::Value;
use std::sync::Barrier;

const OBJECT: &str = "/storage/v1/b/etag-bucket/o/object";
const JSON: &str = "Content-Type: application/json\r\n";

fn setup(server: &Server, auth: &str) -> Value {
    server
        .request(
            "POST",
            "/storage/v1/b?project=project-a",
            &format!("{JSON}{auth}"),
            br#"{"name":"etag-bucket"}"#,
        )
        .status(200);
    let uploaded = server.request(
        "POST",
        "/upload/storage/v1/b/etag-bucket/o?uploadType=media&name=object",
        auth,
        b"unchanged object bytes",
    );
    uploaded.status(200);
    uploaded.json()
}

// Cloud Storage applied metadata PATCH and PUT requests whose If-Match named a superseded or
// bogus entity tag (differential case obj-gaps-json), so the header guards nothing here either.
#[test]
fn metadata_if_match_is_ignored_while_query_conditions_still_apply() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let mut resource = setup(&server, "");
    let first_etag = resource["etag"].as_str().unwrap().to_owned();
    resource["metadata"] = serde_json::json!({"fixture":"updated"});
    let updated = server.request(
        "PUT",
        OBJECT,
        &format!("{JSON}Content-Encoding: gzip\r\nIf-Match: {first_etag}\r\n"),
        &gzip(resource.to_string().as_bytes()),
    );
    updated.status(200);
    assert_eq!(updated.json()["metageneration"], "2");
    assert_eq!(updated.json()["metadata"]["fixture"], "updated");
    assert_ne!(updated.json()["etag"], resource["etag"]);
    for (condition, metageneration) in [(first_etag.as_str(), "3"), ("\"bogus\"", "4")] {
        let applied = server.request(
            "PATCH",
            OBJECT,
            &format!("{JSON}If-Match: {condition}\r\n"),
            br#"{"metadata":{"fixture":"applied"}}"#,
        );
        applied.status(200);
        assert_eq!(applied.json()["metageneration"], metageneration);
    }
    let current = server.request("GET", OBJECT, "", b"").json();
    let failed = server.request(
        "PATCH",
        &format!("{OBJECT}?ifMetagenerationMatch=1"),
        &format!("{JSON}If-Match: {}\r\n", current["etag"].as_str().unwrap()),
        br#"{"metadata":{"fixture":"lost"}}"#,
    );
    failed.status(412);
    assert_eq!(
        failed.json()["error"]["errors"][0]["reason"],
        "conditionNotMet"
    );
    assert_eq!(server.request("GET", OBJECT, "", b"").json(), current);
    server
        .request(
            "POST",
            OBJECT,
            &format!("{JSON}X-HTTP-Method-Override: PATCH\r\nIf-Match: {first_etag}\r\n"),
            br#"{"metadata":{"override":"yes"}}"#,
        )
        .status(200);
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        "unchanged object bytes"
    );
}

// Only well-formed tags were recorded; a value that is not one is ignored the same way, since
// the header is not evaluated at all. The other conditional headers have no recording on this
// method and keep their rejection.
#[test]
fn metadata_if_match_values_are_ignored_and_other_conditions_stay_rejected() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server, "");
    let etag = server.request("GET", OBJECT, "", b"").json()["etag"]
        .as_str()
        .unwrap()
        .to_owned();
    for value in [
        "*".to_owned(),
        format!("\"other\", {etag}"),
        format!("\"other\"\r\nIf-Match: {etag}"),
        format!("{etag}\r\nIf-Unmodified-Since: Wed, 21 Oct 2015 07:28:00 GMT"),
        format!("W/{etag}"),
        "\"stale\"".to_owned(),
        String::new(),
        "bare-tag".to_owned(),
        "bare tag".to_owned(),
        "bare\"tag".to_owned(),
        format!("{etag}, bad tag"),
        format!("*, {etag}"),
        "*\r\nIf-Match: *".to_owned(),
    ] {
        server
            .request(
                "PATCH",
                OBJECT,
                &format!("{JSON}If-Match: {value}\r\n"),
                b"{}",
            )
            .status(200);
    }
    let before = server.request("GET", OBJECT, "", b"").json();
    for (method, path, headers) in [
        ("PATCH", OBJECT.to_string(), "If-None-Match: *\r\n"),
        ("DELETE", OBJECT.to_string(), "If-Match: *\r\n"),
        (
            "POST",
            "/upload/storage/v1/b/etag-bucket/o?uploadType=media&name=object".into(),
            "If-Match: *\r\n",
        ),
    ] {
        let body = if method == "DELETE" {
            b"".as_slice()
        } else {
            b"{}".as_slice()
        };
        server
            .request(method, &path, &format!("{JSON}{headers}"), body)
            .status(400);
        assert_eq!(server.request("GET", OBJECT, "", b"").json(), before);
    }
    for condition in [
        "If-Modified-Since: Wed, 21 Oct 2015 07:28:00 GMT\r\n",
        "If-Unmodified-Since: Wed, 21 Oct 2015 07:28:00 GMT\r\n",
        "If-Range: Wed, 21 Oct 2015 07:28:00 GMT\r\n",
        "If-Match: *\r\nIf-Modified-Since: Wed, 21 Oct 2015 07:28:00 GMT\r\n",
        "If-Match: *\r\nIf-Range: Wed, 21 Oct 2015 07:28:00 GMT\r\n",
    ] {
        server
            .request("PATCH", OBJECT, &format!("{JSON}{condition}"), b"{}")
            .status(400);
        assert_eq!(server.request("GET", OBJECT, "", b"").json(), before);
    }
}

#[test]
fn metadata_if_match_keeps_missing_resource_and_strict_permission_errors() {
    let dir = TestDir::new();
    let registry = common::process::write_identities(
        &dir.0,
        &serde_json::json!({"identities":[
            {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project-a":["roles/storage.admin"]}},
            {"id":"viewer@example.invalid","tokens":["viewer"],"project_roles":{"project-a":["roles/storage.objectViewer"]}}
        ]}),
    );
    let server = Server::start(
        &dir,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
        ],
    );
    let admin = "Authorization: Bearer admin\r\n";
    let original = setup(&server, admin);
    for (token, status) in [("invalid", 401), ("viewer", 403)] {
        server
            .request(
                "PATCH",
                OBJECT,
                &format!("{JSON}Authorization: Bearer {token}\r\nIf-Match: *\r\n"),
                b"{}",
            )
            .status(status);
    }
    for value in ["*", "bare-tag"] {
        server
            .request(
                "PATCH",
                "/storage/v1/b/etag-bucket/o/missing",
                &format!("{JSON}{admin}If-Match: {value}\r\n"),
                b"{}",
            )
            .status(404);
    }
    assert_eq!(server.request("GET", OBJECT, admin, b"").json(), original);
}

/// Sends two metadata PATCH requests at once and returns their sorted statuses.
fn race(server: &Server, path: &str, headers: &str) -> [u16; 2] {
    let barrier = Barrier::new(2);
    let mut statuses = std::thread::scope(|scope| {
        // Both threads start before either is joined, so the requests overlap.
        let tasks = ["first", "second"].map(|name| {
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                let body = serde_json::json!({"metadata": {"winner": name}}).to_string();
                server
                    .request("PATCH", path, headers, body.as_bytes())
                    .status
            })
        });
        tasks.map(|task| task.join().unwrap())
    });
    statuses.sort_unstable();
    statuses
}

#[test]
fn concurrent_metadata_updates_need_a_query_precondition_for_one_winner() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let original = setup(&server, "");
    let if_match = format!("{JSON}If-Match: {}\r\n", original["etag"].as_str().unwrap());
    assert_eq!(race(&server, OBJECT, &if_match), [200, 200]);
    let current = server.request("GET", OBJECT, "", b"").json();
    assert_eq!(current["metageneration"], "3");
    let conditional = format!("{OBJECT}?ifMetagenerationMatch=3");
    assert_eq!(race(&server, &conditional, JSON), [200, 412]);
    let current = server.request("GET", OBJECT, "", b"").json();
    assert_eq!(current["generation"], original["generation"]);
    assert_eq!(current["metageneration"], "4");
}
