mod object_effects_support;

use object_effects_support::{Server, common::TestDir};
use serde_json::{Value, json};

#[test]
fn unsupported_google_routes_and_inputs() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    let inventory: Value =
        serde_json::from_str(include_str!("../compatibility/upstream/json-methods.json")).unwrap();
    let implemented = [
        "storage.buckets.delete",
        "storage.buckets.get",
        "storage.buckets.getIamPolicy",
        "storage.buckets.insert",
        "storage.buckets.list",
        "storage.buckets.lockRetentionPolicy",
        "storage.buckets.patch",
        "storage.buckets.setIamPolicy",
        "storage.buckets.testIamPermissions",
        "storage.buckets.update",
        "storage.notifications.delete",
        "storage.notifications.get",
        "storage.notifications.insert",
        "storage.notifications.list",
        "storage.objects.compose",
        "storage.objects.copy",
        "storage.objects.delete",
        "storage.objects.get",
        "storage.objects.list",
        "storage.objects.move",
        "storage.objects.patch",
        "storage.objects.rewrite",
        "storage.objects.restore",
        "storage.objects.update",
        "storage.projects.hmacKeys.create",
        "storage.projects.hmacKeys.delete",
        "storage.projects.hmacKeys.get",
        "storage.projects.hmacKeys.list",
        "storage.projects.hmacKeys.update",
        "storage.projects.serviceAccount.get",
    ];
    for operation in inventory["methods"].as_array().unwrap() {
        if implemented.contains(&operation["id"].as_str().unwrap()) {
            continue;
        }
        let template = operation["path"].as_str().unwrap();
        let path = template
            .split('/')
            .map(|segment| {
                if segment == "{bucket}" {
                    "test-bucket"
                } else if segment.starts_with('{') {
                    "missing%2Fresource"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        let reply = server.request(
            operation["http"].as_str().unwrap(),
            &format!("/storage/v1/{path}"),
            "",
            b"{}",
        );
        reply.error(400, "badRequest");
        let value: Value = serde_json::from_slice(&reply.body).unwrap();
        let message = value["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(template) && message.contains("for example"),
            "{value}"
        );
    }
    let reply = server.request(
        "POST",
        "/storage/v1/b?project=local",
        "",
        json!({"name":"rejected-bucket", "website":{"mainPageSuffix":"index.html"}})
            .to_string()
            .as_bytes(),
    );
    reply.error(400, "badRequest");
    assert!(String::from_utf8_lossy(&reply.body).contains("website"));
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o?includeFoldersAsPrefixes=true",
            "",
            b"",
        )
        .error(400, "badRequest");
    server
        .request("GET", "/storage/v1/b/rejected-bucket", "", b"")
        .error(404, "notFound");
    server
        .request("GET", "/storage/v1/b/test-bucket/o/missing", "", b"")
        .error(404, "notFound");
    for path in ["/_internal/unknown", "/storage/v1/b/test-bucket/unknown"] {
        assert_eq!(server.request("GET", path, "", b"").status, 404);
    }
    let strict_dir = TestDir::new();
    let strict = Server::start(&strict_dir, true);
    strict
        .request(
            "GET",
            "/storage/v1/b/missing/acl",
            "Authorization: Bearer unknown\r\n",
            b"",
        )
        .error(401, "authError");
    strict
        .request(
            "GET",
            "/storage/v1/b/missing/acl",
            "Authorization: Bearer admin\r\n",
            b"",
        )
        .error(400, "badRequest");
}
