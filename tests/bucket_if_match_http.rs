mod common;
mod ingress_support;

use common::TestDir;
use ingress_support::{Server, gzip};
use serde_json::{Value, json};
use std::sync::{Arc, Barrier};

const BUCKET: &str = "/storage/v1/b/etag-bucket";
const IAM: &str = "/storage/v1/b/etag-bucket/iam";
const JSON: &str = "Content-Type: application/json\r\n";

fn setup(server: &Server) -> Value {
    let created = server.request(
        "POST",
        "/storage/v1/b?project=project-a",
        JSON,
        br#"{"name":"etag-bucket"}"#,
    );
    created.status(200);
    created.json()
}

fn policy(member: &str) -> Vec<u8> {
    json!({"bindings":[{"role":"roles/storage.objectViewer","members":[member]}]})
        .to_string()
        .into_bytes()
}

fn assert_condition_not_met(response: &ingress_support::Reply) {
    response.status(412);
    assert_eq!(
        response.json()["error"]["errors"][0]["reason"],
        "conditionNotMet"
    );
}

#[test]
fn bucket_updates_accept_and_ignore_entity_tag_headers_like_cloud_storage() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let mut resource = setup(&server);
    let first = resource["etag"].as_str().unwrap().to_owned();
    resource["labels"] = json!({"fixture":"replaced"});
    // The .NET client sends the returned resource etag on a gzip full-resource PUT.
    let replaced = server.request(
        "PUT",
        BUCKET,
        &format!("{JSON}Content-Encoding: gzip\r\nIf-Match: {first}\r\n"),
        &gzip(resource.to_string().as_bytes()),
    );
    replaced.status(200);
    assert_eq!(replaced.json()["metageneration"], "2");
    assert_eq!(replaced.json()["labels"]["fixture"], "replaced");
    let fresh = replaced.json()["etag"].as_str().unwrap().to_owned();
    assert_ne!(fresh, first);
    // Cloud Storage answered 200 and applied the change for a stale If-Match and for
    // If-None-Match on bucket PATCH and PUT, so neither header is a condition here.
    let mut resource = replaced.json();
    let mut generation = 2;
    for (method, headers) in [
        ("PATCH", format!("If-Match: {first}\r\n")),
        ("PUT", format!("If-Match: {first}\r\n")),
        ("PATCH", format!("If-None-Match: {fresh}\r\n")),
        ("PATCH", "If-None-Match: *\r\n".to_owned()),
        ("PATCH", "If-Match: \"unterminated\r\n".to_owned()),
        ("PATCH", format!("If-Match: W/{fresh}, bare-tag\r\n")),
    ] {
        generation += 1;
        resource["labels"] = json!({"step": generation.to_string()});
        let body = if method == "PUT" {
            resource.to_string()
        } else {
            json!({"labels":{"step": generation.to_string()}}).to_string()
        };
        let applied = server.request(method, BUCKET, &format!("{JSON}{headers}"), body.as_bytes());
        applied.status(200);
        assert_eq!(applied.json()["metageneration"], generation.to_string());
        assert_eq!(applied.json()["labels"]["step"], generation.to_string());
        resource = applied.json();
        // PATCH answers with the ACL fields by default; the stored resource reads without them.
        for field in ["acl", "defaultObjectAcl", "owner"] {
            resource.as_object_mut().unwrap().remove(field);
        }
    }
    // Query preconditions keep their atomic evaluation alongside the ignored header.
    let failed = server.request(
        "PATCH",
        &format!("{BUCKET}?ifMetagenerationMatch=1"),
        &format!("{JSON}If-Match: {}\r\n", resource["etag"].as_str().unwrap()),
        br#"{"labels":{"fixture":"lost"}}"#,
    );
    assert_condition_not_met(&failed);
    let before = server.request("GET", BUCKET, "", b"").json();
    assert_eq!(before, resource);
    // Deletion is outside this scope; the header keeps its explicit rejection.
    server
        .request("DELETE", BUCKET, "If-Match: *\r\n", b"")
        .status(400);
    assert_eq!(server.request("GET", BUCKET, "", b"").json(), before);
    server
        .request(
            "PATCH",
            "/storage/v1/b/missing-bucket",
            &format!("{JSON}If-Match: *\r\n"),
            b"{}",
        )
        .status(404);
}

#[test]
fn iam_policy_writes_compare_if_match_with_the_policy_etag() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let bucket = setup(&server);
    let current = server.request("GET", IAM, "", b"");
    current.status(200);
    let first = current.json()["etag"].as_str().unwrap().to_owned();
    assert!(!first.starts_with('"'));
    // Google.Apis copies the policy's unquoted JSON etag verbatim into If-Match.
    let set = server.request(
        "PUT",
        IAM,
        &format!("{JSON}If-Match: {first}\r\n"),
        &policy("user:first@example.invalid"),
    );
    set.status(200);
    let second = set.json()["etag"].as_str().unwrap().to_owned();
    assert_ne!(second, first);
    // The quoted form equals the HTTP ETag that getIamPolicy returns.
    server
        .request(
            "PUT",
            IAM,
            &format!("{JSON}If-Match: \"{second}\"\r\n"),
            &policy("user:second@example.invalid"),
        )
        .status(200);
    let before = server.request("GET", IAM, "", b"").json();
    let current = before["etag"].as_str().unwrap().to_owned();
    for condition in [
        first.clone(),
        format!("\"{second}\""),
        bucket["etag"].as_str().unwrap().to_owned(),
        format!("W/\"{current}\""),
    ] {
        let failed = server.request(
            "PUT",
            IAM,
            &format!("{JSON}If-Match: {condition}\r\n"),
            &policy("user:lost@example.invalid"),
        );
        assert_condition_not_met(&failed);
        assert_eq!(server.request("GET", IAM, "", b"").json(), before);
    }
    // The body etag and the header are separate conditions; both must name the current policy.
    let with_body_etag = |etag: &str| {
        json!({"etag": etag, "bindings":[{"role":"roles/storage.objectViewer","members":["user:body@example.invalid"]}]})
            .to_string()
            .into_bytes()
    };
    for (header, body_etag) in [
        (String::new(), first.as_str()),
        (format!("If-Match: {first}\r\n"), current.as_str()),
        (format!("If-Match: {current}\r\n"), first.as_str()),
    ] {
        let failed = server.request(
            "PUT",
            IAM,
            &format!("{JSON}{header}"),
            &with_body_etag(body_etag),
        );
        assert_condition_not_met(&failed);
        assert_eq!(server.request("GET", IAM, "", b"").json(), before);
    }
    // Google.Apis replaces the fetched policy's etag with the quoted HTTP ETag of
    // getIamPolicy and sends it back in both the body and If-Match.
    let header = server.request("GET", IAM, "", b"");
    let http_etag = header
        .headers
        .lines()
        .find_map(|line| line.strip_prefix("etag: "))
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(http_etag, format!("\"{current}\""));
    server
        .request(
            "PUT",
            IAM,
            &format!("{JSON}If-Match: {http_etag}\r\n"),
            &with_body_etag(&http_etag),
        )
        .status(200);
    let before = server.request("GET", IAM, "", b"").json();
    let current = before["etag"].as_str().unwrap().to_owned();
    server
        .request(
            "PUT",
            IAM,
            &format!("{JSON}If-Match: {current}\r\n"),
            &with_body_etag(&current),
        )
        .status(200);
    assert_condition_not_met(&server.request(
        "PUT",
        IAM,
        &format!("{JSON}If-Match: {current}\r\n"),
        &with_body_etag(&http_etag),
    ));
    let before = server.request("GET", IAM, "", b"").json();
    for headers in [
        "If-Match: \"unterminated\r\n",
        "If-None-Match: *\r\n",
        "If-Match: *\r\nIf-Range: Wed, 21 Oct 2015 07:28:00 GMT\r\n",
    ] {
        server
            .request(
                "PUT",
                IAM,
                &format!("{JSON}{headers}"),
                &policy("user:lost@example.invalid"),
            )
            .status(400);
        assert_eq!(server.request("GET", IAM, "", b"").json(), before);
    }
    server
        .request(
            "PUT",
            IAM,
            &format!("{JSON}If-Match: *\r\n"),
            &policy("user:wildcard@example.invalid"),
        )
        .status(200);
}

#[test]
fn concurrent_policy_writes_with_one_if_match_etag_have_exactly_one_winner() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server);
    let etag = server.request("GET", IAM, "", b"").json()["etag"]
        .as_str()
        .unwrap()
        .to_owned();
    let headers = format!("{JSON}If-Match: {etag}\r\n");
    let bodies = [
        policy("user:first@example.invalid"),
        policy("user:second@example.invalid"),
    ];
    let barrier = Arc::new(Barrier::new(2));
    let mut results = std::thread::scope(|scope| {
        let tasks: Vec<_> = bodies
            .iter()
            .map(|body| {
                let barrier = barrier.clone();
                let server = &server;
                let headers = &headers;
                scope.spawn(move || {
                    barrier.wait();
                    server.request("PUT", IAM, headers, body).status
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
    let stored = server.request("GET", IAM, "", b"").json();
    assert_ne!(stored["etag"], etag.as_str());
    let members = stored["bindings"].to_string();
    assert_ne!(
        members.contains("first@example.invalid"),
        members.contains("second@example.invalid")
    );
}
