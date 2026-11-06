use crate::common;
use crate::common::process::declare_server;

use common::TestDir;
use common::process::http_command;
use serde_json::{Value, json};
use std::time::Duration;

impl Server {
    fn start(dir: &TestDir, options: &[&str]) -> Self {
        Self::launch(http_command(&dir.0.join("data")).args(options))
    }

    fn send(&self, token: Option<&str>, method: &str, path: &str, body: &str) -> Reply {
        let authorization = token
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        Client {
            address: &self.address,
            host: "local.invalid",
            headers: "Content-Type: application/json\r\n",
            timeout: Duration::from_secs(5),
        }
        .request_eof(method, path, &authorization, body.as_bytes())
        .into_text()
    }

    fn request(&self, method: &str, path: &str, body: &str) -> Reply {
        self.send(None, method, path, body)
    }

    fn upload(&self, bucket: &str, name: &str, data: &str) -> Value {
        let reply = self.request(
            "POST",
            &format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name={name}"),
            data,
        );
        reply.expect(200);
        reply.json()
    }

    fn stop(&mut self) {
        self.0.stop();
    }
}

use common::http::{Client, TextReply as Reply};

const POLICY: &str = r#"{"retentionDurationSeconds":"604800"}"#;

fn create(server: &Server, name: &str, policy: Option<&str>) -> Value {
    let body = match policy {
        Some(policy) => format!(r#"{{"name":"{name}","softDeletePolicy":{policy}}}"#),
        None => format!(r#"{{"name":"{name}"}}"#),
    };
    server
        .request("POST", "/storage/v1/b?project=project", &body)
        .json_ok()
}

fn generation(bucket: &Value) -> String {
    let generation = bucket["generation"].as_str().expect("string generation");
    assert!(generation.parse::<i64>().unwrap() > 0, "{bucket}");
    generation.to_owned()
}

#[test]
fn bucket_soft_delete_get_list_and_restore_round_trip() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, &[]);
    let bucket = create(&server, "soft-bucket", Some(POLICY));
    let generation = generation(&bucket);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/soft-bucket", "")
            .json_ok()["generation"],
        generation
    );
    let object = server.upload("soft-bucket", "kept.txt", "kept");
    let object_generation = object["generation"].as_str().unwrap().to_owned();
    server
        .request("DELETE", "/storage/v1/b/soft-bucket/o/kept.txt", "")
        .expect(204);
    server
        .request("DELETE", "/storage/v1/b/soft-bucket", "")
        .expect(204);
    server
        .request("GET", "/storage/v1/b/soft-bucket", "")
        .json_reason_value(404, "notFound");
    let required = server
        .request("GET", "/storage/v1/b/soft-bucket?softDeleted=true", "")
        .json_reason_value(400, "required");
    assert_eq!(
        required["error"]["message"],
        "Bucket generation is required."
    );
    server
        .request(
            "GET",
            &format!("/storage/v1/b/soft-bucket?generation={generation}"),
            "",
        )
        .json_reason_value(400, "required");
    let soft = server
        .request(
            "GET",
            &format!("/storage/v1/b/soft-bucket?softDeleted=true&generation={generation}"),
            "",
        )
        .json_ok();
    assert_eq!(soft["name"], "soft-bucket");
    assert_eq!(soft["generation"], generation);
    assert!(soft["softDeleteTime"].is_string(), "{soft}");
    assert!(soft["hardDeleteTime"].is_string(), "{soft}");
    server
        .request(
            "GET",
            "/storage/v1/b/soft-bucket?softDeleted=true&generation=1",
            "",
        )
        .json_reason_value(404, "notFound");
    let listed = server
        .request("GET", "/storage/v1/b?project=project&softDeleted=true", "")
        .json_ok();
    assert_eq!(listed["items"].as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed["items"][0]["generation"], generation);
    assert!(listed["items"][0]["softDeleteTime"].is_string());
    // The soft-deleted version is not a live bucket.
    let live = server
        .request("GET", "/storage/v1/b?project=project", "")
        .json_ok();
    assert!(live.get("items").is_none(), "{live}");

    // A recreated live bucket blocks restore and never sees the retained objects.
    let recreated = create(&server, "soft-bucket", None);
    assert_ne!(recreated["generation"], generation);
    let hidden = server
        .request("GET", "/storage/v1/b/soft-bucket/o?softDeleted=true", "")
        .json_ok();
    assert!(hidden.get("items").is_none(), "{hidden}");
    server
        .request(
            "POST",
            &format!("/storage/v1/b/soft-bucket/restore?generation={generation}"),
            "",
        )
        .json_reason_value(409, "conflict");
    server
        .request("DELETE", "/storage/v1/b/soft-bucket", "")
        .expect(204);

    server
        .request("POST", "/storage/v1/b/soft-bucket/restore", "")
        .json_reason_value(400, "required");
    server
        .request("POST", "/storage/v1/b/soft-bucket/restore?generation=1", "")
        .json_reason_value(404, "notFound");
    // Restart between deletion and restore: the soft-deleted state is durable.
    server.stop();
    let server = Server::start(&dir, &[]);
    let restored = server
        .request(
            "POST",
            &format!("/storage/v1/b/soft-bucket/restore?generation={generation}"),
            "",
        )
        .json_ok();
    assert_eq!(restored["generation"], generation);
    assert_eq!(restored["softDeletePolicy"], bucket["softDeletePolicy"]);
    assert!(restored.get("softDeleteTime").is_none(), "{restored}");
    assert!(restored.get("hardDeleteTime").is_none(), "{restored}");
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/soft-bucket", "")
            .json_ok(),
        restored
    );
    let soft_list = server
        .request("GET", "/storage/v1/b?project=project&softDeleted=true", "")
        .json_ok();
    assert!(soft_list.get("items").is_none(), "{soft_list}");

    // Restoring the bucket leaves its objects soft-deleted; they restore separately.
    server
        .request("GET", "/storage/v1/b/soft-bucket/o/kept.txt", "")
        .json_reason_value(404, "notFound");
    let retained = server
        .request("GET", "/storage/v1/b/soft-bucket/o?softDeleted=true", "")
        .json_ok();
    assert_eq!(retained["items"][0]["generation"], object_generation);
    server
        .request(
            "POST",
            &format!("/storage/v1/b/soft-bucket/o/kept.txt/restore?generation={object_generation}"),
            "",
        )
        .json_ok();
    let media = server.request("GET", "/storage/v1/b/soft-bucket/o/kept.txt?alt=media", "");
    assert_eq!((media.status, media.body.as_str()), (200, "kept"));
}

#[test]
fn bucket_without_soft_delete_policy_is_deleted_permanently() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let bucket = create(&server, "hard-bucket", None);
    let generation = generation(&bucket);
    server
        .request("DELETE", "/storage/v1/b/hard-bucket", "")
        .expect(204);
    server
        .request(
            "GET",
            &format!("/storage/v1/b/hard-bucket?softDeleted=true&generation={generation}"),
            "",
        )
        .json_reason_value(404, "notFound");
    server
        .request(
            "POST",
            &format!("/storage/v1/b/hard-bucket/restore?generation={generation}"),
            "",
        )
        .json_reason_value(404, "notFound");
}

#[test]
fn soft_deleted_bucket_listing_pages_by_name_and_generation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let mut expected = Vec::new();
    for name in ["page-b", "page-a", "page-a", "other-c"] {
        let generation = generation(&create(&server, name, Some(POLICY)));
        server
            .request("DELETE", &format!("/storage/v1/b/{name}"), "")
            .expect(204);
        if name.starts_with("page") {
            expected.push((name.to_owned(), generation));
        }
    }
    expected.sort_by_key(|(name, generation)| (name.clone(), generation.parse::<i64>().unwrap()));
    let mut seen = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut path =
            "/storage/v1/b?project=project&softDeleted=true&prefix=page&maxResults=1".to_owned();
        if let Some(token) = &token {
            path.push_str(&format!("&pageToken={token}"));
        }
        let page = server.request("GET", &path, "").json_ok();
        for item in page["items"].as_array().unwrap() {
            seen.push((
                item["name"].as_str().unwrap().to_owned(),
                item["generation"].as_str().unwrap().to_owned(),
            ));
        }
        match page["nextPageToken"].as_str() {
            Some(next) => token = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(seen, expected);
    // A live-listing token cannot continue a soft-deleted listing.
    let live_token = {
        create(&server, "page-live-1", None);
        create(&server, "page-live-2", None);
        server
            .request(
                "GET",
                "/storage/v1/b?project=project&prefix=page&maxResults=1",
                "",
            )
            .json_ok()["nextPageToken"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    server
        .request(
            "GET",
            &format!(
                "/storage/v1/b?project=project&prefix=page&softDeleted=true&pageToken={live_token}"
            ),
            "",
        )
        .json_reason_value(400, "invalidParameter");
}

#[test]
fn strict_soft_deleted_bucket_access_uses_project_permissions() {
    let dir = TestDir::new();
    let registry = common::process::write_identities(
        &dir.0,
        &json!({"identities":[
            {"id":"admin@example.test","tokens":["admin"],"project_roles":{"project":["roles/storage.admin"]}},
            {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"project":["roles/storage.bucketViewer"]}},
            {"id":"other@example.test","tokens":["other"],"project_roles":{"elsewhere":["roles/storage.admin"]}}
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
    let admin = Some("admin");
    let bucket = server
        .send(
            admin,
            "POST",
            "/storage/v1/b?project=project",
            &format!(r#"{{"name":"strict-soft","softDeletePolicy":{POLICY}}}"#),
        )
        .json_ok();
    let generation = generation(&bucket);
    server
        .send(admin, "DELETE", "/storage/v1/b/strict-soft", "")
        .expect(204);
    let get = format!("/storage/v1/b/strict-soft?softDeleted=true&generation={generation}");
    let restore = format!("/storage/v1/b/strict-soft/restore?generation={generation}");
    server
        .send(None, "GET", &get, "")
        .json_reason_value(401, "required");
    server
        .send(None, "POST", &restore, "")
        .json_reason_value(401, "required");
    server
        .send(Some("other"), "GET", &get, "")
        .json_reason_value(403, "forbidden");
    server
        .send(
            Some("other"),
            "GET",
            "/storage/v1/b?project=project&softDeleted=true",
            "",
        )
        .json_reason_value(403, "forbidden");
    server
        .send(Some("other"), "POST", &restore, "")
        .json_reason_value(403, "forbidden");
    // bucketViewer has storage.buckets.get and list but not storage.buckets.restore.
    assert_eq!(
        server.send(Some("viewer"), "GET", &get, "").json_ok()["generation"],
        generation
    );
    assert_eq!(
        server
            .send(
                Some("viewer"),
                "GET",
                "/storage/v1/b?project=project&softDeleted=true",
                ""
            )
            .json_ok()["items"][0]["generation"],
        generation
    );
    server
        .send(Some("viewer"), "POST", &restore, "")
        .json_reason_value(403, "forbidden");
    assert_eq!(
        server.send(admin, "POST", &restore, "").json_ok()["generation"],
        generation
    );
}

/// Servers keep no deleted data unless asked; this option gives new buckets the seven days that
/// Cloud Storage applies by default.
const DEFAULT_ON: [&str; 2] = ["--default-soft-delete-retention-seconds", "604800"];

#[test]
fn configured_default_gives_a_new_bucket_that_policy_and_soft_deletes_it() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &DEFAULT_ON);
    let bucket = create(&server, "defaulted", None);
    // Cloud Storage returns the policy with effectiveTime equal to timeCreated.
    assert_eq!(
        bucket["softDeletePolicy"],
        json!({"retentionDurationSeconds": "604800", "effectiveTime": bucket["timeCreated"]})
    );
    let generation = generation(&bucket);
    server
        .request("DELETE", "/storage/v1/b/defaulted", "")
        .expect(204);
    // The deletion is soft: the version is listed and restorable.
    let soft = server
        .request(
            "GET",
            &format!("/storage/v1/b/defaulted?softDeleted=true&generation={generation}"),
            "",
        )
        .json_ok();
    assert_eq!(soft["name"], "defaulted");
    server
        .request(
            "POST",
            &format!("/storage/v1/b/defaulted/restore?generation={generation}"),
            "",
        )
        .json_ok();
}

#[test]
fn unconfigured_native_server_frees_overwritten_and_deleted_bodies_at_once() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let bucket = create(&server, "genuine-default", None);
    assert!(bucket.get("softDeletePolicy").is_none(), "{bucket}");
    let blob_files = || {
        std::fs::read_dir(dir.0.join("data").join("blobs"))
            .unwrap()
            .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
            .sum::<usize>()
    };

    let first = server.upload("genuine-default", "object", "first");
    assert_eq!(blob_files(), 1);
    let second = server.upload("genuine-default", "object", "second");
    assert_ne!(first["generation"], second["generation"]);
    // Both responses are sent only after reclamation, so nothing here depends on timing.
    assert_eq!(
        blob_files(),
        1,
        "the overwritten body must not stay on disk"
    );
    let listed = server
        .request(
            "GET",
            "/storage/v1/b/genuine-default/o?softDeleted=true",
            "",
        )
        .json_ok();
    assert!(listed.get("items").is_none(), "{listed}");

    server
        .request("DELETE", "/storage/v1/b/genuine-default/o/object", "")
        .expect(204);
    assert_eq!(blob_files(), 0, "the deleted body must not stay on disk");
    let generation = second["generation"].as_str().unwrap();
    server
        .request(
            "GET",
            &format!(
                "/storage/v1/b/genuine-default/o/object?softDeleted=true&generation={generation}"
            ),
            "",
        )
        .json_reason_value(404, "notFound");
}

#[test]
fn unconfigured_legacy_server_gives_new_buckets_no_soft_delete_policy() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--compat-profile", "legacy-emulator"]);
    let bucket = create(&server, "legacy-default", None);
    assert!(bucket.get("softDeletePolicy").is_none(), "{bucket}");
}

#[test]
fn lowering_the_default_keeps_the_stored_policy_of_existing_buckets() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, &DEFAULT_ON);
    let stored = create(&server, "upgraded", None)["softDeletePolicy"].clone();
    assert_eq!(stored["retentionDurationSeconds"], "604800");
    server.stop();

    // As after an upgrade from a release whose default was seven days.
    let server = Server::start(&dir, &[]);
    let kept = server
        .request("GET", "/storage/v1/b/upgraded", "")
        .json_ok();
    assert_eq!(kept["softDeletePolicy"], stored);
    let object = server.upload("upgraded", "object", "retained");
    server
        .request("DELETE", "/storage/v1/b/upgraded/o/object", "")
        .expect(204);
    let retained = server
        .request("GET", "/storage/v1/b/upgraded/o?softDeleted=true", "")
        .json_ok();
    assert_eq!(retained["items"][0]["generation"], object["generation"]);
    let fresh = create(&server, "fresh", None);
    assert!(fresh.get("softDeletePolicy").is_none(), "{fresh}");
}

#[test]
fn explicit_zero_policy_overrides_the_default_and_deletes_permanently() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &DEFAULT_ON);
    let bucket = create(
        &server,
        "opt-out",
        Some(r#"{"retentionDurationSeconds":"0"}"#),
    );
    assert_eq!(bucket["softDeletePolicy"]["retentionDurationSeconds"], "0");
    let generation = generation(&bucket);
    server
        .request("DELETE", "/storage/v1/b/opt-out", "")
        .expect(204);
    server
        .request(
            "GET",
            &format!("/storage/v1/b/opt-out?softDeleted=true&generation={generation}"),
            "",
        )
        .json_reason_value(404, "notFound");
}

#[test]
fn zero_default_keeps_new_buckets_without_a_policy_and_existing_ones_are_unchanged() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, &[]);
    let bucket = create(&server, "plain", None);
    assert!(bucket.get("softDeletePolicy").is_none(), "{bucket}");
    server.stop();
    // Raising the default later does not rewrite a bucket created before.
    let server = Server::start(&dir, &DEFAULT_ON);
    let kept = server.request("GET", "/storage/v1/b/plain", "").json_ok();
    assert!(kept.get("softDeletePolicy").is_none(), "{kept}");
    let fresh = create(&server, "fresh", None);
    assert_eq!(
        fresh["softDeletePolicy"]["retentionDurationSeconds"],
        "604800"
    );
}

#[test]
fn invalid_default_retention_is_rejected_at_startup() {
    for value in ["3155760001", "18446744073709551616", "-1", "1.5", "week"] {
        let output = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--default-soft-delete-retention-seconds",
                value,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{value}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("default_soft_delete_retention_seconds"),
            "{value}: {stderr}"
        );
    }
    // The reference emulator profile never applies a default policy, so a positive one conflicts.
    let output = common::server_command()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--compat-profile",
            "legacy-emulator",
            "--default-soft-delete-retention-seconds",
            "604800",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

declare_server!();
