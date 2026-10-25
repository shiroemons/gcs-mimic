mod common;

use common::TestDir;
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
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

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    ready: Value,
}

impl Server {
    fn start(command: &mut Command) -> Self {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            stdout,
            ready,
        }
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
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}\r\n{body}", body.len()).unwrap();
        let mut reader = BufReader::new(stream);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut body = Vec::new();
        reader.read_to_end(&mut body).unwrap();
        (status, headers, body)
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
        common::stop_server(&mut self.child, &mut self.stdout);
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

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
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
    for (index, location) in [json!("invalid"), json!(""), Value::Null]
        .into_iter()
        .enumerate()
    {
        let body =
            json!({"name":format!("invalid-bucket-{index}"),"location":location}).to_string();
        let reply = server.request("POST", "/storage/v1/b?project=project-a", "", &body);
        assert!(matches!(reply.0, 400 | 501));
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
        vec!["--default-location", ""],
        vec!["--default-location", "us"],
        vec!["--default-location", "US-CENTRAL1"],
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
    let dir = TestDir::new();
    let output = command(&dir.0)
        .env("FAKE_GCS_LOCATION", "EU")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!dir.0.join("data").exists());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("GCS_MIMIC_DEFAULT_LOCATION=EU")
    );
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
        changed
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
        json!([{"name":"valid-bucket"},{"name":"typo-bucket","storageClas":"STANDARD"},{"name":"iam-bucket","iam":{}},{"name":"lifecycle-bucket","lifecycle":{"rule":[]}}]),
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
    ] {
        assert!(keys.contains(&key));
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
