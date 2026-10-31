mod common;
mod ingress_support;

use common::TestDir;
use ingress_support::{Server, gzip};
use serde_json::Value;
use std::sync::{Arc, Barrier};

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

#[test]
fn metadata_if_match_checks_returned_etag_and_query_conditions_atomically() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let mut resource = setup(&server, "");
    let etag = resource["etag"].as_str().unwrap().to_owned();
    resource["metadata"] = serde_json::json!({"fixture":"updated"});
    let updated = server.request(
        "PUT",
        OBJECT,
        &format!("{JSON}Content-Encoding: gzip\r\nIf-Match: {etag}\r\n"),
        &gzip(resource.to_string().as_bytes()),
    );
    updated.status(200);
    assert_eq!(updated.json()["metageneration"], "2");
    assert_eq!(updated.json()["metadata"]["fixture"], "updated");
    assert_ne!(updated.json()["etag"], resource["etag"]);
    for (path, condition) in [
        (OBJECT.to_string(), etag),
        (
            format!("{OBJECT}?ifMetagenerationMatch=1"),
            updated.json()["etag"].as_str().unwrap().into(),
        ),
    ] {
        let failed = server.request(
            "PATCH",
            &path,
            &format!("{JSON}If-Match: {condition}\r\n"),
            br#"{"metadata":{"fixture":"lost"}}"#,
        );
        failed.status(412);
        assert_eq!(
            failed.json()["error"]["errors"][0]["reason"],
            "conditionNotMet"
        );
    }
    let before = server.request("GET", OBJECT, "", b"").json();
    assert_eq!(before["metageneration"], "2");
    assert_eq!(before["metadata"]["fixture"], "updated");
    let headers = format!(
        "{JSON}X-HTTP-Method-Override: PATCH\r\nIf-Match: {}\r\n",
        before["etag"].as_str().unwrap()
    );
    server
        .request(
            "POST",
            OBJECT,
            &headers,
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

#[test]
fn metadata_if_match_reuses_strong_lists_wildcards_and_complete_syntax_validation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server, "");
    for coding in ["wildcard", "list", "duplicate", "ignored-date"] {
        let current = server.request("GET", OBJECT, "", b"").json();
        let etag = current["etag"].as_str().unwrap();
        let value = match coding {
            "wildcard" => "*".to_string(),
            "list" => format!("\"other\", {etag}"),
            "duplicate" => format!("\"other\"\r\nIf-Match: {etag}"),
            _ => format!("{etag}\r\nIf-Unmodified-Since: Wed, 21 Oct 2015 07:28:00 GMT"),
        };
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
    let etag = before["etag"].as_str().unwrap();
    for (value, status) in [
        (format!("W/{etag}"), 412),
        ("\"stale\"".into(), 412),
        (String::new(), 412),
        ("bare-tag".into(), 400),
        (format!("{etag}, bad"), 400),
        (format!("*, {etag}"), 400),
        ("*\r\nIf-Match: *".into(), 400),
    ] {
        server
            .request(
                "PATCH",
                OBJECT,
                &format!("{JSON}If-Match: {value}\r\n"),
                b"{}",
            )
            .status(status);
        assert_eq!(server.request("GET", OBJECT, "", b"").json(), before);
    }
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
fn metadata_if_match_preserves_missing_resource_and_strict_permission_errors() {
    let dir = TestDir::new();
    let registry = dir.0.join("identities.json");
    std::fs::write(&registry, serde_json::json!({"identities":[
        {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project-a":["roles/storage.admin"]}},
        {"id":"viewer@example.invalid","tokens":["viewer"],"project_roles":{"project-a":["roles/storage.objectViewer"]}}
    ]}).to_string()).unwrap();
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

#[test]
fn concurrent_metadata_updates_with_one_etag_have_exactly_one_winner() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let original = setup(&server, "");
    let headers = format!("{JSON}If-Match: {}\r\n", original["etag"].as_str().unwrap());
    let barrier = Arc::new(Barrier::new(2));
    let mut results = std::thread::scope(|scope| {
        let tasks: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|name| {
                let barrier = barrier.clone();
                let server = &server;
                let headers = &headers;
                scope.spawn(move || {
                    barrier.wait();
                    server
                        .request(
                            "PATCH",
                            OBJECT,
                            headers,
                            serde_json::json!({"metadata":{"winner":name}})
                                .to_string()
                                .as_bytes(),
                        )
                        .status
                })
            })
            .collect();
        tasks
            .into_iter()
            .map(|task| task.join().unwrap())
            .collect::<Vec<_>>()
    });
    results.sort_unstable();
    assert_eq!(results, [200, 412]);
    let current = server.request("GET", OBJECT, "", b"").json();
    assert_eq!(current["generation"], original["generation"]);
    assert_eq!(current["metageneration"], "2");
}
