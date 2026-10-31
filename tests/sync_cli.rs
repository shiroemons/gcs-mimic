#![cfg(any(target_os = "linux", target_os = "macos"))]

mod object_effects_support;
use object_effects_support::common;
mod sync_cli {
    use super::*;
    use std::fs;
    mod ambiguous;
    mod authority;
    mod deletion;
    mod recovery;
    mod support;
    mod workspace;
    use support::*;
}

use common::TestDir;
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    process::{Child, ChildStdout, Output, Stdio},
    time::Duration,
};

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    endpoint: String,
}

impl Server {
    fn request(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &[u8],
    ) -> object_effects_support::Reply {
        let headers = token
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        object_effects_support::request(
            self.endpoint.strip_prefix("http://").unwrap(),
            method,
            path,
            &headers,
            body,
        )
    }

    fn stop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }

    fn start(data: &TestDir, tree: Option<&TestDir>) -> Self {
        Self::start_with(data, tree, &[])
    }

    fn start_with(data: &TestDir, tree: Option<&TestDir>, options: &[&str]) -> Self {
        let mut command = common::server_command();
        command
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--api-base-url",
                "http://127.0.0.1:4443",
                "--data-dir",
            ])
            .arg(&data.0);
        if !options.contains(&"--seed-project") {
            command.args(["--seed-project", "local"]);
        }
        if let Some(tree) = tree {
            command.arg("--sync-dir").arg(&tree.0);
        }
        let mut child = command
            .args(options)
            .stdout(Stdio::piped())
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
            endpoint: format!("http://{}", ready["listen"].as_str().unwrap()),
        }
    }

    fn cli(&self, command: &str, options: &[&str], success: bool) -> Value {
        self.cli_with_token(command, options, None, success)
    }

    fn cli_with_token(
        &self,
        command: &str,
        options: &[&str],
        token: Option<&str>,
        success: bool,
    ) -> Value {
        let mut client = common::server_command();
        if let Some(token) = token {
            client.env("GCS_MIMIC_SYNC_TOKEN", token);
        }
        let Output {
            status,
            stdout,
            stderr,
        } = client
            .args(["sync", command, "--endpoint", &self.endpoint])
            .args(options)
            .output()
            .unwrap();
        assert_eq!(
            status.success(),
            success,
            "command={command} options={options:?} stdout={} stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        serde_json::from_slice(&stdout).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            common::stop_server(&mut self.child, &mut self.stdout);
        }
    }
}

#[test]
fn live_cli_observes_rescans_waits_and_rejects_stale_revisions() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let server = Server::start(&data, Some(&tree));
    server.cli("rescan", &[], true);
    let wait = server.cli("wait", &["--timeout-ms", "5000"], true);
    assert_eq!(wait["completed"], true);
    let status = server.cli("status", &[], true);
    assert_eq!(status["auth_mode"], "permissive");
    let revision = status["findings_revision"].as_str().unwrap();
    let findings = server.cli("findings", &["--revision", revision, "--limit", "1"], true);
    assert_eq!(findings["items"].as_array().unwrap().len(), 0);
    let conflicts = server.cli("conflicts", &["--limit", "1"], true);
    assert_eq!(conflicts["items"].as_array().unwrap().len(), 0);
    let error = server.cli(
        "resolve",
        &[
            "--conflict",
            &"0".repeat(32),
            "--revision",
            &"0".repeat(32),
            "--action",
            "discard",
            "--expected-generation",
            "0",
            "--operation-id",
            "cli-probe",
        ],
        false,
    );
    assert_eq!(error["error"]["code"], 409);
    let stale = server.cli("findings", &["--revision", &"0".repeat(48)], false);
    assert_eq!(stale["error"]["code"], 409);
    let stale = server.cli("approve", &["--revision", &"0".repeat(32)], false);
    assert_eq!(stale["error"]["code"], 409);
}

#[test]
fn live_cli_reports_disabled_synchronization_without_success() {
    let data = TestDir::new();
    let server = Server::start(&data, None);
    let error = server.cli("status", &[], false);
    assert_eq!(error["error"]["code"], 404);
}

#[test]
fn live_cli_strict_controls_require_project_authority_and_honor_token_file_precedence() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let inputs = TestDir::new();
    let registry = inputs.0.join("identities.json");
    std::fs::write(&registry, br#"{"identities":[{"id":"admin","tokens":["sync-admin"],"project_roles":{"local":["roles/storage.admin"]}},{"id":"reader","tokens":["sync-reader"],"project_roles":{"local":["roles/storage.objectViewer"]}}]}"#).unwrap();
    let server = Server::start_with(
        &data,
        Some(&tree),
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
        ],
    );
    for token in [None, Some("invalid")] {
        let error = server.cli_with_token("status", &[], token, false);
        assert_eq!(error["error"]["code"], 401);
    }
    let error = server.cli_with_token("status", &[], Some("sync-reader"), false);
    assert_eq!(error["error"]["code"], 403);
    let status = server.cli_with_token("status", &[], Some("sync-admin"), true);
    assert_eq!(status["auth_mode"], "strict");
    let token = inputs.0.join("token");
    std::fs::write(&token, b"sync-admin\n").unwrap();
    let status = server.cli_with_token(
        "status",
        &["--token-file", token.to_str().unwrap()],
        Some("invalid"),
        true,
    );
    assert_eq!(status["auth_mode"], "strict");
}

#[test]
fn native_cli_prints_unavailable_json_and_fails_without_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let receiver = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).unwrap();
        assert_eq!(line, "GET /_internal/sync HTTP/1.1\r\n");
        loop {
            line.clear();
            assert_ne!(stream.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
        }
        let body = br#"{"error":{"code":503,"message":"busy"}}"#;
        write!(stream.get_mut(), "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.get_mut().write_all(body).unwrap();
    });
    let output = common::server_command()
        .args(["sync", "status", "--endpoint", &endpoint])
        .output()
        .unwrap();
    receiver.join().unwrap();
    assert!(!output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["error"]["code"],
        503
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP 503"));
}
