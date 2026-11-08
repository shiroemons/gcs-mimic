use crate::common;
use crate::common::process::declare_server;
use common::http::Body;

use common::TestDir;
use common::process::write_request;
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufReader, Read},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

fn command(root: &Path) -> Command {
    let mut command = common::server_command();
    command
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--api-base-url",
            "http://api.example.invalid",
            "--data-dir",
        ])
        .arg(root.join("data"));
    command
}

fn input(root: &Path, buckets: Value) {
    fs::write(
        root.join("buckets.json"),
        json!({"version":1,"project":"project-a","buckets":buckets}).to_string(),
    )
    .unwrap();
}

fn selected(root: &Path) -> Command {
    let mut command = command(root);
    command.arg("--buckets-file").arg(root.join("buckets.json"));
    command
}

impl Server {
    fn start(command: &mut Command) -> Self {
        Self::launch(command.stderr(Stdio::piped()))
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        headers: &str,
        body: &str,
    ) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(self.ready["listen"].as_str().unwrap()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write_request(
            &mut stream,
            method,
            path,
            "localhost",
            &format!("Content-Type: application/json\r\n{headers}"),
            body.as_bytes(),
        );
        let raw = common::read_http_response(&mut BufReader::new(stream), Body::for_method(method))
            .unwrap();
        let end = raw
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 2;
        let headers = String::from_utf8(raw[..end].to_vec()).unwrap();
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, headers, raw[end + 2..].to_vec())
    }

    fn json(&self, method: &str, path: &str, body: Value) -> Value {
        let body = if body.is_null() {
            String::new()
        } else {
            body.to_string()
        };
        let reply = self.request(method, path, "", &body);
        assert_eq!(reply.0, 200, "{}", String::from_utf8_lossy(&reply.2));
        serde_json::from_slice(&reply.2).unwrap()
    }

    fn stop(mut self) -> String {
        self.0.stop();
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        stderr
    }
}

fn counts(server: &Server, created: usize, existing: usize, mismatches: usize) {
    assert_eq!(
        server.ready["buckets_file"],
        json!({"outcome":"applied","created":created,"existing":existing,"project_mismatches":mismatches})
    );
}

#[test]
fn default_location_applies_to_new_api_and_initializer_buckets_only() {
    let dir = TestDir::new();
    input(
        &dir.0,
        json!([{"name":"default-bucket"},{"name":"explicit-bucket","location":"US"}]),
    );
    let server = Server::start(selected(&dir.0).args(["--default-location", "EU"]));
    let initial = server.json("GET", "/storage/v1/b/default-bucket", Value::Null);
    assert_eq!(initial["location"], "EU");
    assert_eq!(
        server.json("GET", "/storage/v1/b/explicit-bucket", Value::Null)["location"],
        "US"
    );
    let api = server.json(
        "POST",
        "/storage/v1/b?project=project-a",
        json!({"name":"api-bucket"}),
    );
    assert_eq!(api["location"], "EU");
    assert_eq!(
        server.json(
            "POST",
            "/storage/v1/b?project=project-a",
            json!({"name":"api-explicit-bucket","location":"ASIA"})
        )["location"],
        "ASIA"
    );
    for (index, location) in [json!("invalid"), json!("MARS-CENTRAL1"), json!(1)]
        .into_iter()
        .enumerate()
    {
        let body =
            json!({"name":format!("invalid-bucket-{index}"),"location":location}).to_string();
        let reply = server.request("POST", "/storage/v1/b?project=project-a", "", &body);
        assert_eq!(reply.0, 400);
        assert_eq!(
            server
                .request(
                    "GET",
                    &format!("/storage/v1/b/invalid-bucket-{index}"),
                    "",
                    ""
                )
                .0,
            404
        );
    }
    server.stop();
    input(
        &dir.0,
        json!([{"name":"default-bucket"},{"name":"explicit-bucket","location":"EU"},{"name":"new-bucket"}]),
    );
    let server = Server::start(selected(&dir.0).args(["--default-location", "ASIA"]));
    counts(&server, 1, 2, 0);
    assert_eq!(
        server.json("GET", "/storage/v1/b/default-bucket", Value::Null),
        initial
    );
    assert_eq!(
        server.json("GET", "/storage/v1/b/api-bucket", Value::Null),
        api
    );
    assert_eq!(
        server.json("GET", "/storage/v1/b/explicit-bucket", Value::Null)["location"],
        "US"
    );
    assert_eq!(
        server.json("GET", "/storage/v1/b/new-bucket", Value::Null)["location"],
        "ASIA"
    );
    server.stop();
}

#[test]
fn default_location_invalid_or_legacy_settings_fail_before_catalog_creation() {
    for options in [
        vec!["--default-location", "MARS-CENTRAL1"],
        vec!["--default-location", " US"],
        vec!["--default-location", "US-CENTRAL1-A"],
        vec![
            "--default-location",
            "US",
            "--compat-profile",
            "legacy-emulator",
        ],
    ] {
        let dir = TestDir::new();
        let output = command(&dir.0).args(options).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!dir.0.join("data").exists());
        let event: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(event["diagnostics"][0]["key"], "--default-location");
    }
    // A translated fake-gcs-server location is validated like the native setting.
    let dir = TestDir::new();
    let output = command(&dir.0)
        .env("FAKE_GCS_LOCATION", "MARS-CENTRAL1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!dir.0.join("data").exists());
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(text.contains("FAKE_GCS_LOCATION"), "{text}");
    assert!(text.contains("default_location must"), "{text}");
}

#[test]
fn regional_and_dual_region_initializer_locations_use_shared_defaults() {
    let dir = TestDir::new();
    input(
        &dir.0,
        json!([
            {"name":"default-bucket"},
            {"name":"regional-bucket","location":"asia-northeast1"},
            {"name":"empty-bucket","location":""},
            {"name":"null-bucket","location":null}
        ]),
    );
    let server = Server::start(selected(&dir.0).args(["--default-location", "nam4"]));
    for name in ["default-bucket", "empty-bucket", "null-bucket"] {
        let bucket = server.json("GET", &format!("/storage/v1/b/{name}"), Value::Null);
        assert_eq!(bucket["location"], "NAM4");
        assert_eq!(bucket["locationType"], "dual-region");
    }
    let regional = server.json("GET", "/storage/v1/b/regional-bucket", Value::Null);
    assert_eq!(regional["location"], "ASIA-NORTHEAST1");
    assert_eq!(regional["locationType"], "region");
    server.stop();
}

#[test]
fn initialized_settings_are_independent_and_api_changes_survive_restart() {
    let dir = TestDir::new();
    input(
        &dir.0,
        json!([
            {"name":"assets-bucket","cors":[{"origin":["https://allowed.example"],"method":["GET"]}],"versioning":{"enabled":true}},
        {"name":"private-bucket","project":"project-b","iamConfiguration":{"publicAccessPrevention":"enforced"}},
        {"name":"held-bucket","defaultEventBasedHold":true}
        ]),
    );
    let server = Server::start(&mut selected(&dir.0));
    counts(&server, 3, 0, 0);
    let held = server.json(
        "POST",
        "/upload/storage/v1/b/held-bucket/o?uploadType=media&name=file",
        json!("held"),
    );
    assert_eq!(held["eventBasedHold"], true);
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/held-bucket/o/file", "", "")
            .0,
        403
    );
    for bucket in ["assets-bucket", "private-bucket"] {
        for bytes in ["first", "second"] {
            assert_eq!(
                server
                    .request(
                        "POST",
                        &format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name=file"),
                        "",
                        bytes
                    )
                    .0,
                200
            );
        }
        let versions = server.json(
            "GET",
            &format!("/storage/v1/b/{bucket}/o?versions=true"),
            Value::Null,
        );
        assert_eq!(
            versions["items"].as_array().unwrap().len(),
            if bucket == "assets-bucket" { 2 } else { 1 }
        );
        assert!(
            !versions["items"][0]["eventBasedHold"]
                .as_bool()
                .unwrap_or(false)
        );
        for origin in ["https://allowed.example", "https://denied.example"] {
            let reply = server.request(
                "GET",
                &format!("/{bucket}/file"),
                &format!("Origin: {origin}\r\n"),
                "",
            );
            assert_eq!(reply.0, 200);
            assert_eq!(
                reply.1.contains("access-control-allow-origin:"),
                bucket == "assets-bucket" && origin == "https://allowed.example"
            );
        }
    }
    assert_eq!(
        server.json("GET", "/storage/v1/b?project=project-b", Value::Null)["items"][0]["name"],
        "private-bucket"
    );
    let changed = server.json(
        "PATCH",
        "/storage/v1/b/assets-bucket?ifMetagenerationMatch=1",
        json!({"labels":{"owner":"api"},"cors":[],"storageClass":"NEARLINE"}),
    );
    let policy = server.json(
        "PUT",
        "/storage/v1/b/assets-bucket/iam",
        json!({"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}),
    );
    let after_policy = server.json("GET", "/storage/v1/b/assets-bucket", Value::Null);
    assert_eq!(changed["metageneration"], "2");
    assert_eq!(after_policy["metageneration"], "3");
    assert_ne!(after_policy["etag"], changed["etag"]);
    let object = server.json("GET", "/storage/v1/b/assets-bucket/o/file", Value::Null);
    server.stop();
    input(
        &dir.0,
        json!([{"name":"assets-bucket","labels":{"owner":"file"},"versioning":{"enabled":false}},{"name":"new-bucket"}]),
    );
    let server = Server::start(&mut selected(&dir.0));
    counts(&server, 1, 1, 0);
    assert_eq!(
        server.json("GET", "/storage/v1/b/assets-bucket", Value::Null),
        after_policy
    );
    assert_eq!(
        server.json("GET", "/storage/v1/b/assets-bucket/iam", Value::Null),
        policy
    );
    assert_eq!(
        server.json("GET", "/storage/v1/b/assets-bucket/o/file", Value::Null),
        object
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/assets-bucket/o/file?alt=media",
                "",
                ""
            )
            .2,
        b"second"
    );
    assert_eq!(
        server.json(
            "GET",
            "/storage/v1/b/assets-bucket/o?versions=true",
            Value::Null
        )["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/private-bucket", "", "")
            .0,
        200
    );
    assert_eq!(
        server
            .request(
                "PATCH",
                "/storage/v1/b/assets-bucket?ifMetagenerationMatch=1",
                "",
                "{\"labels\":{}}"
            )
            .0,
        412
    );
    server.stop();
}

/// An entry and a file above their former limits of 64 KiB and 1 MiB reach the catalog whole.
#[test]
fn a_large_bucket_entry_starts_and_survives_a_restart() {
    let dir = TestDir::new();
    let cors: Vec<_> = (0..2000)
        .map(|index| json!({"origin":[format!("https://{index:04}{}.invalid", "o".repeat(1000))]}))
        .collect();
    input(&dir.0, json!([{"name":"large-bucket","cors":cors}]));
    let size = fs::metadata(dir.0.join("buckets.json")).unwrap().len();
    assert!(size > 1024 * 1024, "{size}");
    let path = "/storage/v1/b/large-bucket";
    let server = Server::start(&mut selected(&dir.0));
    counts(&server, 1, 0, 0);
    assert_eq!(server.json("GET", path, Value::Null)["cors"], json!(cors));
    server.stop();
    let server = Server::start(&mut selected(&dir.0));
    counts(&server, 0, 1, 0);
    assert_eq!(server.json("GET", path, Value::Null)["cors"], json!(cors));
    server.stop();
}

/// A file past both the former default of 1000 buckets and the 1 MiB that went with it.
fn large_file(root: &Path, count: usize) {
    let filler = "x".repeat(63);
    let entries = (0..count)
        .map(|index| {
            let labels = (0..12)
                .map(|label| (format!("label-{label:02}"), filler.clone()))
                .collect::<std::collections::BTreeMap<_, _>>();
            json!({"name":format!("bucket-{index:05}"),"labels":labels})
        })
        .collect::<Vec<_>>();
    input(root, Value::Array(entries));
    let size = fs::metadata(root.join("buckets.json")).unwrap().len();
    assert!(
        size > 1024 * 1024,
        "{size} bytes need the raised file limit"
    );
    assert!(
        size <= 1024 * count as u64,
        "{size} bytes for {count} buckets"
    );
}

fn diagnostic(output: &std::process::Output) -> Value {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    serde_json::from_slice(&output.stderr).unwrap()
}

#[test]
fn a_file_past_the_former_default_starts_again_after_a_restart_and_a_named_limit_stops_it() {
    const BUCKETS: usize = 1200;
    let dir = TestDir::new();
    large_file(&dir.0, BUCKETS);
    let mut limited = selected(&dir.0);
    limited.args(["--max-buckets-per-batch", &(BUCKETS - 1).to_string()]);
    let limited = diagnostic(&limited.output().unwrap());
    assert!(
        limited.to_string().contains("max_buckets_per_batch"),
        "{limited}"
    );
    let server = Server::start(&mut selected(&dir.0));
    counts(&server, BUCKETS, 0, 0);
    assert_eq!(
        server.json("GET", "/storage/v1/b/bucket-01199", Value::Null)["name"],
        "bucket-01199"
    );
    server.stop();
    let server = Server::start(&mut selected(&dir.0));
    counts(&server, 0, BUCKETS, 0);
    let (mut listed, mut token) = (0, String::new());
    loop {
        let page = server.json(
            "GET",
            &format!("/storage/v1/b?project=project-a&pageToken={token}"),
            Value::Null,
        );
        listed += page["items"].as_array().unwrap().len();
        let Some(next) = page["nextPageToken"].as_str() else {
            break;
        };
        token = next.to_owned();
    }
    assert_eq!(listed, BUCKETS);
    server.stop();
}

#[test]
fn a_huge_failing_file_keeps_its_report_within_the_diagnostic_budget() {
    const BUCKETS: usize = 30_000;
    let dir = TestDir::new();
    let entries = (0..BUCKETS)
        .map(|index| {
            if index % 2 == 0 {
                json!({"name":format!("bad name {index}")})
            } else {
                json!({"name":format!("bucket-{index:05}")})
            }
        })
        .collect::<Vec<_>>();
    input(&dir.0, Value::Array(entries));
    let output = selected(&dir.0)
        .args(["--max-buckets-per-batch", &BUCKETS.to_string()])
        .output()
        .unwrap();
    let event = diagnostic(&output);
    assert!(output.stderr.len() <= 64 * 1024, "{}", output.stderr.len());
    let summary = event["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["key"] == "$")
        .unwrap();
    assert!(
        summary["fix"].as_str().unwrap().contains("more ranges"),
        "{summary}"
    );
    let server = Server::start(&mut command(&dir.0));
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/bucket-00001", "", "")
            .0,
        404
    );
    server.stop();
}

#[test]
fn project_mismatch_warns_without_mutation_and_deleted_buckets_are_recreated() {
    let dir = TestDir::new();
    input(&dir.0, json!([{"name":"recreated-bucket"}]));
    let server = Server::start(&mut selected(&dir.0));
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/recreated-bucket", "", "")
            .0,
        204
    );
    server.json(
        "POST",
        "/storage/v1/b?project=project-b",
        json!({"name":"recreated-bucket","labels":{"owner":"api"}}),
    );
    server.stop();
    let server = Server::start(&mut selected(&dir.0));
    counts(&server, 0, 1, 1);
    assert_eq!(
        server.json("GET", "/storage/v1/b/recreated-bucket", Value::Null)["labels"]["owner"],
        "api"
    );
    assert_eq!(
        server.json("GET", "/storage/v1/b?project=project-b", Value::Null)["items"][0]["name"],
        "recreated-bucket"
    );
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/recreated-bucket", "", "")
            .0,
        204
    );
    let warning = server.stop();
    let event: Value = serde_json::from_str(warning.lines().next().unwrap()).unwrap();
    assert_eq!(event["level"], "warn");
    assert_eq!(event["diagnostics"][0]["key"], "buckets[0].project");
    assert!(!warning.contains("project-a"));
    let server = Server::start(&mut selected(&dir.0));
    counts(&server, 1, 0, 0);
    server.stop();
}

#[test]
fn validation_reports_all_entries_before_seed_or_mutation() {
    let dir = TestDir::new();
    input(
        &dir.0,
        json!([
            {"name":"valid-bucket"},
            {"name":"typo-bucket","storageClas":"STANDARD"},
            {"name":"iam-bucket","iam":{}},
            {"name":"lifecycle-bucket","lifecycle":{"rule":[]}},
            {"name":"policy-bucket","iamPolicy":{"bindings":[{"role":"roles/owner","members":["group:team@example.invalid"]}]}}
        ]),
    );
    let output = selected(&dir.0)
        .arg("--seed-dir")
        .arg(dir.0.join("missing-seed"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let event: Value = serde_json::from_slice(&output.stderr).unwrap();
    let keys: Vec<_> = event["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["key"].as_str().unwrap())
        .collect();
    for key in [
        "buckets[1].storageClas",
        "buckets[2].iam",
        "buckets[3].lifecycle",
        "buckets[4].iamPolicy.bindings[0].role",
        "buckets[4].iamPolicy.bindings[0].members[0]",
    ] {
        assert!(keys.contains(&key), "{key} in {keys:?}");
    }
    let server = Server::start(&mut command(&dir.0));
    assert_eq!(server.ready["buckets_file"], json!({"outcome":"disabled"}));
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/valid-bucket", "", "")
            .0,
        404
    );
    server.stop();
}

#[test]
fn committed_initialization_survives_later_seed_and_bind_failures() {
    for bind_failure in [false, true] {
        let dir = TestDir::new();
        input(&dir.0, json!([{"name":"committed-bucket"}]));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut command = selected(&dir.0);
        if bind_failure {
            command = common::server_command();
            command
                .args(["serve", "--scheme", "http", "--listen"])
                .arg(listener.local_addr().unwrap().to_string())
                .arg("--data-dir")
                .arg(dir.0.join("data"))
                .arg("--buckets-file")
                .arg(dir.0.join("buckets.json"));
        } else {
            command.arg("--seed-dir").arg(dir.0.join("missing-seed"));
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        drop(listener);
        input(
            &dir.0,
            json!([{"name":"committed-bucket"},{"name":"remaining-bucket"}]),
        );
        let server = Server::start(&mut selected(&dir.0));
        counts(&server, 1, 1, 0);
        server.stop();
    }
}

#[test]
fn initializer_precedes_seed_and_empty_file_is_applied() {
    let dir = TestDir::new();
    fs::create_dir_all(dir.0.join("seed/seed-bucket")).unwrap();
    fs::write(dir.0.join("seed/seed-bucket/file"), b"seed").unwrap();
    input(
        &dir.0,
        json!([{"name":"seed-bucket","labels":{"owner":"initializer"}}]),
    );
    let server = Server::start(
        selected(&dir.0)
            .arg("--seed-dir")
            .arg(dir.0.join("seed"))
            .args(["--seed-project", "project-a"]),
    );
    assert_eq!(
        server.json("GET", "/storage/v1/b/seed-bucket", Value::Null)["labels"]["owner"],
        "initializer"
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/seed-bucket/o/file?alt=media", "", "")
            .2,
        b"seed"
    );
    server.stop();
    input(&dir.0, json!([]));
    let server = Server::start(&mut selected(&dir.0));
    counts(&server, 0, 0, 0);
    server.stop();
    let other = TestDir::new();
    input(&other.0, json!([{"name":"seed-bucket"}]));
    let output = selected(&other.0)
        .arg("--seed-dir")
        .arg(dir.0.join("seed"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("belongs to another project"));
}

#[test]
fn strict_initialization_grants_no_api_permissions_and_legacy_preserves_native_metadata() {
    let dir = TestDir::new();
    input(
        &dir.0,
        json!([
            {"name":"native-bucket","location":"EU","storageClass":"NEARLINE","labels":{"owner":"native"},"versioning":{"enabled":true}},
            {"name":"protected-bucket","iamConfiguration":{"publicAccessPrevention":"enforced"}}
        ]),
    );
    let identity = dir.0.join("identity.json");
    fs::write(
        &identity,
        json!({"identities":[
            {"id":"nobody@example.invalid","tokens":["no-roles"]},
            {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project-a":["roles/storage.admin"]}}
        ]}).to_string(),
    )
    .unwrap();
    let server = Server::start(
        selected(&dir.0)
            .args(["--auth-mode", "strict", "--identity-file"])
            .arg(&identity)
            .args(["--compat-profile", "legacy-emulator"]),
    );
    counts(&server, 2, 0, 0);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/native-bucket", "", "")
            .0,
        401
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/native-bucket",
                "Authorization: Bearer no-roles\r\n",
                ""
            )
            .0,
        403
    );
    for bucket in ["native-bucket", "protected-bucket"] {
        let auth = "Authorization: Bearer admin\r\n";
        assert_eq!(
            server
                .request(
                    "POST",
                    &format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name=file"),
                    auth,
                    "private"
                )
                .0,
            200
        );
        assert_eq!(
            server
                .request(
                    "PUT",
                    &format!("/storage/v1/b/{bucket}/iam"),
                    auth,
                    r#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#
                )
                .0,
            if bucket == "native-bucket" { 200 } else { 412 }
        );
        assert_eq!(
            server.request("GET", &format!("/{bucket}/file"), "", "").0,
            if bucket == "native-bucket" { 200 } else { 401 }
        );
    }
    server.stop();
    let server = Server::start(&mut selected(&dir.0));
    counts(&server, 0, 2, 0);
    let metadata = server.json("GET", "/storage/v1/b/native-bucket", Value::Null);
    assert_eq!(metadata["location"], "EU");
    assert_eq!(metadata["storageClass"], "NEARLINE");
    assert_eq!(metadata["labels"]["owner"], "native");
    assert_eq!(metadata["versioning"]["enabled"], true);
    server.stop();
}

fn acl_grants(server: &Server, bucket: &str) -> Vec<(String, String)> {
    server.json("GET", &format!("/storage/v1/b/{bucket}/acl"), Value::Null)["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["entity"].as_str().unwrap().to_owned(),
                item["role"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn a_file_policy_replaces_the_default_policy_as_set_iam_policy_does() {
    let dir = TestDir::new();
    let policy = json!({"bindings":[
        {"role":"roles/storage.objectViewer","members":["allUsers"]},
        {"role":"roles/storage.legacyBucketReader","members":["allAuthenticatedUsers"]},
        {"role":"roles/storage.legacyBucketOwner","members":[]}
    ]});
    input(
        &dir.0,
        json!([{"name":"file-bucket","iamPolicy":policy},{"name":"default-bucket"}]),
    );
    let server = Server::start(&mut selected(&dir.0));
    counts(&server, 2, 0, 0);
    server.json(
        "POST",
        "/storage/v1/b?project=project-a",
        json!({"name":"api-bucket"}),
    );
    let set = server.json("PUT", "/storage/v1/b/api-bucket/iam", policy);
    let file = server.json("GET", "/storage/v1/b/file-bucket/iam", Value::Null);
    // The empty binding is dropped and the default legacy bindings are gone, as after the PUT.
    assert_eq!(
        file["bindings"],
        json!([
            {"role":"roles/storage.objectViewer","members":["allUsers"]},
            {"role":"roles/storage.legacyBucketReader","members":["allAuthenticatedUsers"]}
        ])
    );
    assert_eq!(file["bindings"], set["bindings"]);
    assert_eq!(
        acl_grants(&server, "file-bucket"),
        acl_grants(&server, "api-bucket")
    );
    assert_eq!(
        acl_grants(&server, "file-bucket"),
        [("allAuthenticatedUsers".to_owned(), "READER".to_owned())]
    );
    assert_eq!(
        server.json("GET", "/storage/v1/b/default-bucket/iam", Value::Null)["bindings"],
        json!([
            {"role":"roles/storage.legacyBucketOwner","members":["projectEditor:project-a","projectOwner:project-a"]},
            {"role":"roles/storage.legacyBucketReader","members":["projectViewer:project-a"]}
        ])
    );
    // Creation stores the policy: it is not a later policy write that advances metageneration.
    assert_eq!(
        server.json("GET", "/storage/v1/b/file-bucket", Value::Null)["metageneration"],
        "1"
    );
    let object = server.json(
        "POST",
        "/upload/storage/v1/b/file-bucket/o?uploadType=media&name=file",
        json!("public"),
    );
    assert_eq!(object["name"], "file");
    server.stop();
}

#[test]
fn a_memory_backend_starts_buckets_with_their_file_policy() {
    let dir = TestDir::new();
    input(
        &dir.0,
        json!([{"name":"memory-bucket","iamPolicy":{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}}]),
    );
    let server = Server::start(selected(&dir.0).args(["--backend", "memory"]));
    counts(&server, 1, 0, 0);
    assert_eq!(
        server.json("GET", "/storage/v1/b/memory-bucket/iam", Value::Null)["bindings"],
        json!([{"role":"roles/storage.objectViewer","members":["allUsers"]}])
    );
    server.stop();
    assert!(!dir.0.join("data").exists());
}

fn strict_identity(root: &Path) -> std::path::PathBuf {
    let identity = root.join("identity.json");
    fs::write(
        &identity,
        json!({
            "identities":[{"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project-a":["roles/storage.admin"],"project-p":["roles/storage.admin"]}}],
            "public_access_prevention_projects":["project-p"]
        })
        .to_string(),
    )
    .unwrap();
    identity
}

#[test]
fn strict_public_reads_follow_the_file_policy_and_buckets_without_one_stay_private() {
    let dir = TestDir::new();
    input(
        &dir.0,
        json!([
            {"name":"public-bucket","iamPolicy":{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}},
            {"name":"private-bucket"}
        ]),
    );
    let identity = strict_identity(&dir.0);
    let server = Server::start(
        selected(&dir.0)
            .args(["--auth-mode", "strict", "--identity-file"])
            .arg(&identity),
    );
    counts(&server, 2, 0, 0);
    for bucket in ["public-bucket", "private-bucket"] {
        assert_eq!(
            server
                .request(
                    "POST",
                    &format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name=file"),
                    "Authorization: Bearer admin\r\n",
                    "content"
                )
                .0,
            200
        );
        let public = bucket == "public-bucket";
        assert_eq!(
            server.request("GET", &format!("/{bucket}/file"), "", "").0,
            if public { 200 } else { 401 }
        );
        let media = server.request(
            "GET",
            &format!("/storage/v1/b/{bucket}/o/file?alt=media"),
            "",
            "",
        );
        assert_eq!(media.0, if public { 200 } else { 401 });
        if public {
            assert_eq!(media.2, b"content");
        }
        // Reading objects is all the binding grants.
        assert_eq!(
            server
                .request("DELETE", &format!("/storage/v1/b/{bucket}/o/file"), "", "")
                .0,
            401
        );
    }
    server.stop();
}

#[test]
fn public_access_prevention_refuses_a_public_file_policy_before_any_mutation() {
    let dir = TestDir::new();
    let public = json!({"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]});
    input(
        &dir.0,
        json!([
            {"name":"valid-bucket","iamPolicy":public},
            {"name":"enforced-bucket","iamConfiguration":{"publicAccessPrevention":"enforced"},
             "iamPolicy":{"bindings":[{"role":"roles/storage.objectViewer","members":["allAuthenticatedUsers"]}]}},
            {"name":"parent-bucket","project":"project-p","iamPolicy":public},
            {"name":"private-parent-bucket","project":"project-p","iamPolicy":{"bindings":[{"role":"roles/storage.objectViewer","members":["user:reader@example.invalid"]}]}}
        ]),
    );
    let identity = strict_identity(&dir.0);
    let output = selected(&dir.0)
        .args(["--auth-mode", "strict", "--identity-file"])
        .arg(&identity)
        .output()
        .unwrap();
    let event = diagnostic(&output);
    let keys: Vec<_> = event["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        ["buckets[1].iamPolicy", "buckets[2].iamPolicy"],
        "{event}"
    );
    assert!(
        event.to_string().contains("Public access prevention"),
        "{event}"
    );
    let server = Server::start(&mut command(&dir.0));
    for bucket in ["valid-bucket", "private-parent-bucket"] {
        assert_eq!(
            server
                .request("GET", &format!("/storage/v1/b/{bucket}"), "", "")
                .0,
            404
        );
    }
    server.stop();
}

#[test]
fn selected_file_only_is_loaded_and_invalid_paths_fail_without_ready() {
    let dir = TestDir::new();
    input(&dir.0, json!([]));
    let config = dir.0.join("settings.json");
    fs::write(&config, r#"{"buckets_file":"missing-overridden.json"}"#).unwrap();
    let server = Server::start(
        selected(&dir.0)
            .arg("--config")
            .arg(&config)
            .env("GCS_MIMIC_BUCKETS_FILE", "missing-env.json"),
    );
    counts(&server, 0, 0, 0);
    server.stop();
    fs::remove_file(dir.0.join("buckets.json")).unwrap();
    let output = selected(&dir.0).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let event: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(event["diagnostics"][0]["source"], "initializer");
    assert_eq!(event["diagnostics"][0]["key"], "buckets_file");
    for value in [json!(4), json!(null), json!(false)] {
        fs::write(&config, json!({"buckets_file":value}).to_string()).unwrap();
        let output = command(&dir.0)
            .arg("--config")
            .arg(&config)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn soft_delete_default_reaches_initializer_and_seed_buckets_and_an_entry_can_override_it() {
    let dir = TestDir::new();
    input(
        &dir.0,
        json!([
            {"name":"default-bucket"},
            {"name":"permanent-bucket","softDeletePolicy":{"retentionDurationSeconds":"0"}},
            {"name":"long-bucket","softDeletePolicy":{"retentionDurationSeconds":1209600}},
            {"name":"brief-bucket","softDeletePolicy":{"retentionDurationSeconds":"5"}}
        ]),
    );
    let seed = dir.0.join("seed");
    fs::create_dir_all(seed.join("seeded-bucket")).unwrap();
    fs::write(seed.join("seeded-bucket").join("file.txt"), b"seeded").unwrap();
    let server = Server::start(
        selected(&dir.0)
            .args(["--default-soft-delete-retention-seconds", "604800"])
            .arg("--seed-dir")
            .arg(&seed),
    );
    let retention = |name: &str| {
        server.json("GET", &format!("/storage/v1/b/{name}"), Value::Null)["softDeletePolicy"]
            ["retentionDurationSeconds"]
            .clone()
    };
    assert_eq!(retention("default-bucket"), "604800");
    assert_eq!(retention("seeded-bucket"), "604800");
    assert_eq!(retention("long-bucket"), "1209600");
    assert_eq!(retention("brief-bucket"), "5");
    // An explicit zero is kept as sent and reported as "0": the bucket is deleted permanently.
    assert_eq!(retention("permanent-bucket"), "0");
    server.stop();
}

#[test]
fn invalid_soft_delete_policy_in_the_buckets_file_is_reported_before_any_mutation() {
    let dir = TestDir::new();
    input(
        &dir.0,
        json!([
            {"name":"bad-high","softDeletePolicy":{"retentionDurationSeconds":"3155760001"}},
            {"name":"bad-shape","softDeletePolicy":5},
            {"name":"bad-field","softDeletePolicy":{"effectiveTime":"2026-01-01T00:00:00Z"}}
        ]),
    );
    let output = selected(&dir.0).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let event: Value = serde_json::from_slice(&output.stderr).unwrap();
    let keys: Vec<_> = event["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["key"].as_str().unwrap().to_owned())
        .collect();
    for key in [
        "buckets[0].softDeletePolicy.retentionDurationSeconds",
        "buckets[1].softDeletePolicy",
        "buckets[2].softDeletePolicy.effectiveTime",
    ] {
        assert!(keys.iter().any(|k| k == key), "{key} in {keys:?}");
    }
}

declare_server!();
