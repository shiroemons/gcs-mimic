mod common;

use common::TestDir;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    time::Duration,
};

struct Server {
    child: Child,
    address: String,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(dir: &TestDir, options: &[&str]) -> Self {
        Self::start_with(dir, options, false)
    }

    /// Starts without the suite's pinned default so the genuine, unconfigured default applies.
    fn start_unconfigured(dir: &TestDir, options: &[&str]) -> Self {
        Self::start_with(dir, options, true)
    }

    fn start_with(dir: &TestDir, options: &[&str], unconfigured: bool) -> Self {
        let mut command = common::server_command();
        if unconfigured {
            command.env_remove("GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS");
        }
        let mut child = command
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(dir.0.join("data"))
            .args(options)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).expect("ready event");
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            address: ready["listen"].as_str().unwrap().into(),
            stdout,
        }
    }

    fn send(&self, token: Option<&str>, method: &str, path: &str, body: &str) -> Reply {
        let authorization = token
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: local.invalid\r\n{authorization}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut reader = BufReader::new(stream);
        let reply = common::read_http_response(&mut reader, false).unwrap();
        let reply = String::from_utf8(reply).unwrap();
        let (headers, body) = reply.split_once("\r\n\r\n").unwrap();
        if headers.contains("\r\nconnection: close") {
            common::await_server_close(&mut reader);
        }
        Reply {
            status: headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
            body: body.into(),
        }
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
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()
    }

    fn stop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }

    fn ok(&self) -> Value {
        assert_eq!(self.status, 200, "{}", self.body);
        self.json()
    }

    fn error(&self, status: u16, reason: &str) -> Value {
        assert_eq!(self.status, status, "{}", self.body);
        let value = self.json();
        assert_eq!(
            value["error"]["errors"][0]["reason"], reason,
            "{}",
            self.body
        );
        value
    }
}

const POLICY: &str = r#"{"retentionDurationSeconds":"604800"}"#;

fn create(server: &Server, name: &str, policy: Option<&str>) -> Value {
    let body = match policy {
        Some(policy) => format!(r#"{{"name":"{name}","softDeletePolicy":{policy}}}"#),
        None => format!(r#"{{"name":"{name}"}}"#),
    };
    server
        .request("POST", "/storage/v1/b?project=project", &body)
        .ok()
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
        server.request("GET", "/storage/v1/b/soft-bucket", "").ok()["generation"],
        generation
    );
    let object = server.upload("soft-bucket", "kept.txt", "kept");
    let object_generation = object["generation"].as_str().unwrap().to_owned();
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/soft-bucket/o/kept.txt", "")
            .status,
        204
    );
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/soft-bucket", "")
            .status,
        204
    );
    server
        .request("GET", "/storage/v1/b/soft-bucket", "")
        .error(404, "notFound");
    let required = server
        .request("GET", "/storage/v1/b/soft-bucket?softDeleted=true", "")
        .error(400, "required");
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
        .error(400, "required");
    let soft = server
        .request(
            "GET",
            &format!("/storage/v1/b/soft-bucket?softDeleted=true&generation={generation}"),
            "",
        )
        .ok();
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
        .error(404, "notFound");
    let listed = server
        .request("GET", "/storage/v1/b?project=project&softDeleted=true", "")
        .ok();
    assert_eq!(listed["items"].as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed["items"][0]["generation"], generation);
    assert!(listed["items"][0]["softDeleteTime"].is_string());
    // The soft-deleted version is not a live bucket.
    let live = server
        .request("GET", "/storage/v1/b?project=project", "")
        .ok();
    assert!(live.get("items").is_none(), "{live}");

    // A recreated live bucket blocks restore and never sees the retained objects.
    let recreated = create(&server, "soft-bucket", None);
    assert_ne!(recreated["generation"], generation);
    let hidden = server
        .request("GET", "/storage/v1/b/soft-bucket/o?softDeleted=true", "")
        .ok();
    assert!(hidden.get("items").is_none(), "{hidden}");
    server
        .request(
            "POST",
            &format!("/storage/v1/b/soft-bucket/restore?generation={generation}"),
            "",
        )
        .error(409, "conflict");
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/soft-bucket", "")
            .status,
        204
    );

    server
        .request("POST", "/storage/v1/b/soft-bucket/restore", "")
        .error(400, "required");
    server
        .request("POST", "/storage/v1/b/soft-bucket/restore?generation=1", "")
        .error(404, "notFound");
    // Restart between deletion and restore: the soft-deleted state is durable.
    server.stop();
    let server = Server::start(&dir, &[]);
    let restored = server
        .request(
            "POST",
            &format!("/storage/v1/b/soft-bucket/restore?generation={generation}"),
            "",
        )
        .ok();
    assert_eq!(restored["generation"], generation);
    assert_eq!(restored["softDeletePolicy"], bucket["softDeletePolicy"]);
    assert!(restored.get("softDeleteTime").is_none(), "{restored}");
    assert!(restored.get("hardDeleteTime").is_none(), "{restored}");
    assert_eq!(
        server.request("GET", "/storage/v1/b/soft-bucket", "").ok(),
        restored
    );
    let soft_list = server
        .request("GET", "/storage/v1/b?project=project&softDeleted=true", "")
        .ok();
    assert!(soft_list.get("items").is_none(), "{soft_list}");

    // Restoring the bucket leaves its objects soft-deleted; they restore separately.
    server
        .request("GET", "/storage/v1/b/soft-bucket/o/kept.txt", "")
        .error(404, "notFound");
    let retained = server
        .request("GET", "/storage/v1/b/soft-bucket/o?softDeleted=true", "")
        .ok();
    assert_eq!(retained["items"][0]["generation"], object_generation);
    server
        .request(
            "POST",
            &format!("/storage/v1/b/soft-bucket/o/kept.txt/restore?generation={object_generation}"),
            "",
        )
        .ok();
    let media = server.request("GET", "/storage/v1/b/soft-bucket/o/kept.txt?alt=media", "");
    assert_eq!((media.status, media.body.as_str()), (200, "kept"));
}

#[test]
fn bucket_without_soft_delete_policy_is_deleted_permanently() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let bucket = create(&server, "hard-bucket", None);
    let generation = generation(&bucket);
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/hard-bucket", "")
            .status,
        204
    );
    server
        .request(
            "GET",
            &format!("/storage/v1/b/hard-bucket?softDeleted=true&generation={generation}"),
            "",
        )
        .error(404, "notFound");
    server
        .request(
            "POST",
            &format!("/storage/v1/b/hard-bucket/restore?generation={generation}"),
            "",
        )
        .error(404, "notFound");
}

#[test]
fn soft_deleted_bucket_listing_pages_by_name_and_generation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let mut expected = Vec::new();
    for name in ["page-b", "page-a", "page-a", "other-c"] {
        let generation = generation(&create(&server, name, Some(POLICY)));
        assert_eq!(
            server
                .request("DELETE", &format!("/storage/v1/b/{name}"), "")
                .status,
            204
        );
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
        let page = server.request("GET", &path, "").ok();
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
            .ok()["nextPageToken"]
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
        .error(400, "invalidParameter");
}

#[test]
fn strict_soft_deleted_bucket_access_uses_project_permissions() {
    let dir = TestDir::new();
    let registry = dir.0.join("identities.json");
    std::fs::write(
        &registry,
        json!({"identities":[
            {"id":"admin@example.test","tokens":["admin"],"project_roles":{"project":["roles/storage.admin"]}},
            {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"project":["roles/storage.bucketViewer"]}},
            {"id":"other@example.test","tokens":["other"],"project_roles":{"elsewhere":["roles/storage.admin"]}}
        ]})
        .to_string(),
    )
    .unwrap();
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
        .ok();
    let generation = generation(&bucket);
    assert_eq!(
        server
            .send(admin, "DELETE", "/storage/v1/b/strict-soft", "")
            .status,
        204
    );
    let get = format!("/storage/v1/b/strict-soft?softDeleted=true&generation={generation}");
    let restore = format!("/storage/v1/b/strict-soft/restore?generation={generation}");
    server.send(None, "GET", &get, "").error(401, "required");
    server
        .send(None, "POST", &restore, "")
        .error(401, "required");
    server
        .send(Some("other"), "GET", &get, "")
        .error(403, "forbidden");
    server
        .send(
            Some("other"),
            "GET",
            "/storage/v1/b?project=project&softDeleted=true",
            "",
        )
        .error(403, "forbidden");
    server
        .send(Some("other"), "POST", &restore, "")
        .error(403, "forbidden");
    // bucketViewer has storage.buckets.get and list but not storage.buckets.restore.
    assert_eq!(
        server.send(Some("viewer"), "GET", &get, "").ok()["generation"],
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
            .ok()["items"][0]["generation"],
        generation
    );
    server
        .send(Some("viewer"), "POST", &restore, "")
        .error(403, "forbidden");
    assert_eq!(
        server.send(admin, "POST", &restore, "").ok()["generation"],
        generation
    );
}

/// The suite's servers start with the default off (see `common::server_command`); this option
/// turns on Cloud Storage's seven-day default for new buckets.
const DEFAULT_ON: [&str; 2] = ["--default-soft-delete-retention-seconds", "604800"];

#[test]
fn new_bucket_without_a_policy_gets_the_seven_day_default_and_is_soft_deleted() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &DEFAULT_ON);
    let bucket = create(&server, "defaulted", None);
    // Cloud Storage returns the policy with effectiveTime equal to timeCreated.
    assert_eq!(
        bucket["softDeletePolicy"],
        json!({"retentionDurationSeconds": "604800", "effectiveTime": bucket["timeCreated"]})
    );
    let generation = generation(&bucket);
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/defaulted", "")
            .status,
        204
    );
    // The deletion is soft: the version is listed and restorable.
    let soft = server
        .request(
            "GET",
            &format!("/storage/v1/b/defaulted?softDeleted=true&generation={generation}"),
            "",
        )
        .ok();
    assert_eq!(soft["name"], "defaulted");
    server
        .request(
            "POST",
            &format!("/storage/v1/b/defaulted/restore?generation={generation}"),
            "",
        )
        .ok();
}

#[test]
fn unconfigured_native_server_gives_new_buckets_the_seven_day_default() {
    let dir = TestDir::new();
    let server = Server::start_unconfigured(&dir, &[]);
    let bucket = create(&server, "genuine-default", None);
    assert_eq!(
        bucket["softDeletePolicy"]["retentionDurationSeconds"],
        "604800"
    );
}

#[test]
fn unconfigured_legacy_server_gives_new_buckets_no_soft_delete_policy() {
    let dir = TestDir::new();
    let server = Server::start_unconfigured(&dir, &["--compat-profile", "legacy-emulator"]);
    let bucket = create(&server, "legacy-default", None);
    assert!(bucket.get("softDeletePolicy").is_none(), "{bucket}");
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
    assert_eq!(
        server.request("DELETE", "/storage/v1/b/opt-out", "").status,
        204
    );
    server
        .request(
            "GET",
            &format!("/storage/v1/b/opt-out?softDeleted=true&generation={generation}"),
            "",
        )
        .error(404, "notFound");
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
    let kept = server.request("GET", "/storage/v1/b/plain", "").ok();
    assert!(kept.get("softDeletePolicy").is_none(), "{kept}");
    let fresh = create(&server, "fresh", None);
    assert_eq!(
        fresh["softDeletePolicy"]["retentionDurationSeconds"],
        "604800"
    );
}

#[test]
fn invalid_default_retention_is_rejected_at_startup() {
    for value in ["1", "604799", "7776001", "-1", "week"] {
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
