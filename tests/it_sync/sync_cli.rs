#![cfg(any(target_os = "linux", target_os = "macos"))]

use crate::common;
use gcs_mimic::{
    config_input::RenamedEnvironmentVariable,
    sync::{Outcome, WaitReason},
};
use std::fs;
mod ambiguous;
mod authority;
mod deletion;
mod recovery;
mod support;
mod workspace;
use support::*;

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
    ready: Value,
}

impl Server {
    fn request(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &[u8],
    ) -> common::effects::Reply {
        let mut headers = common::sync_version_header();
        if let Some(token) = token {
            headers.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        common::effects::request(
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
        command.arg("serve");
        if !options.contains(&"--scheme") {
            command.args(["--scheme", "http"]);
        }
        command
            .args([
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
            ready,
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
            client.env("GCSM_SYNC_TOKEN", token);
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
        let value: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(
            status.success(),
            success,
            "command={command} options={options:?} stdout={} stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        if command == "wait" {
            assert_eq!(status.code(), Some(wait_exit(&value)), "{value}");
        }
        value
    }
}

/// The exit status `sync wait` documents for its `answer`: 0 when ready, 2 when someone must
/// act, 3 when its timeout passed while work was in progress, and 1 otherwise.
fn wait_exit(answer: &Value) -> i32 {
    let outcome = answer["reason"]
        .as_str()
        .and_then(|reason| reason.parse::<WaitReason>().ok())
        .map_or(Outcome::Failed, WaitReason::outcome);
    i32::from(outcome.exit_code())
}

/// What one `gcs-mimic sync` run reported: whether it succeeded, its exit status, its JSON
/// answer and the lines it wrote to standard error.
struct CliRun {
    success: bool,
    code: Option<i32>,
    answer: Value,
    stderr: Vec<Value>,
}

impl Server {
    fn cli_run(&self, command: &str, options: &[&str]) -> CliRun {
        let Output {
            status,
            stdout,
            stderr,
        } = common::server_command()
            .args(["sync", command, "--endpoint", &self.endpoint])
            .args(options)
            .output()
            .unwrap();
        CliRun {
            success: status.success(),
            code: status.code(),
            // A command refused before any answer prints none.
            answer: if stdout.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&stdout).unwrap()
            },
            stderr: stderr
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice(line).unwrap())
                .collect(),
        }
    }
}

impl CliRun {
    /// The diagnostics this run reported under `key`.
    fn diagnostics(&self, key: &str) -> Vec<String> {
        self.stderr
            .iter()
            .flat_map(|line| line["diagnostics"].as_array().cloned().unwrap_or_default())
            .filter(|diagnostic| diagnostic["key"] == key)
            .map(|diagnostic| diagnostic["fix"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// An object whose name the directory cannot hold does not fail `sync wait`: it exits 0 and
/// prints one line naming the ready-to-run command that lists it, and nothing while none
/// remain. `--require-no-findings` fails it as FindingsPending, exiting 2, with one error that
/// names the objects and what to run next, and no hint besides.
#[test]
fn live_cli_wait_succeeds_past_an_unmapped_name_and_names_the_command_that_lists_it() {
    let data = TestDir::new();
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    let mut server = Server::start(&data, Some(&tree));
    let clear = server.cli_run("wait", &["--timeout", "30s"]);
    assert!(clear.success, "{:?}", clear.stderr);
    assert_eq!(clear.answer["findings"]["unmapped"], 0);
    assert!(
        clear.diagnostics("sync-findings").is_empty(),
        "{:?}",
        clear.stderr
    );
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=dir%5Cfile",
            None,
            b"backslash",
        )
        .expect(200);
    let waited = server.cli_run("wait", &["--timeout", "30s"]);
    assert_eq!(waited.code, Some(0), "{:?}", waited.stderr);
    assert_eq!(waited.answer["reason"], "Ready");
    assert_eq!(waited.answer["findings"]["unmapped"], 1);
    let revision = waited.answer["findings"]["revision"].as_str().unwrap();
    let hint = waited.diagnostics("sync-findings");
    assert_eq!(
        hint,
        [format!(
            "1 object in the API cannot be written to the sync directory because of its name and stays reachable only through the API; list it with gcs-mimic sync status --view findings --revision {revision} --endpoint {}",
            server.endpoint
        )]
    );
    let listed = server.cli(
        "status",
        &["--view", "findings", "--revision", revision],
        true,
    );
    assert_eq!(listed["items"][0]["object"], r"dir\file", "{listed}");
    assert_eq!(server.cli("status", &[], true)["state"], "Ready");
    let strict = server.cli_run("wait", &["--timeout", "30s", "--require-no-findings"]);
    assert_eq!(strict.code, Some(2), "{:?}", strict.stderr);
    assert_eq!(strict.answer["reason"], "FindingsPending");
    let [error] = strict.diagnostics("sync").try_into().unwrap();
    // The remedy names the objects and the findings view at the revision this wait read its
    // findings at.
    assert!(
        error.starts_with(
            "sync wait ended with FindingsPending: 1 object in the API cannot be written to the sync directory because of its name"
        ),
        "{error}"
    );
    assert!(
        error.ends_with(&format!(
            "the findings view lists it: gcs-mimic sync status --view findings --revision {} --endpoint {}",
            strict.answer["findings"]["revision"].as_str().unwrap(),
            server.endpoint
        )),
        "{error}"
    );
    assert!(!error.contains("--endpoint URL"), "{error}");
    assert!(strict.diagnostics("sync-findings").is_empty());
    server.stop();
}

/// Without --endpoint, `sync wait` reaches an HTTPS server where the healthcheck does: through
/// GCS_MIMIC_SCHEME, or through the config file GCS_MIMIC_CONFIG selects. When the configuration
/// gives plain HTTP instead, the failure names the URL it tried, what went wrong, and the scheme
/// setting to check, rather than a generic failure; in a container, the options of the server
/// running as PID 1 are tried next, which the unit tests cover.
#[test]
fn live_cli_wait_without_an_endpoint_finds_an_https_server_as_the_healthcheck_does() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let mut server = Server::start_with(&data, Some(&tree), &["--scheme", "https"]);
    let listen = server.ready["listen"].as_str().unwrap().to_owned();
    let wait = |environment: &[(&str, &str)]| {
        common::server_command()
            .envs(environment.iter().copied())
            .args(["sync", "wait", "--timeout", "30s"])
            .output()
            .unwrap()
    };
    let config = TestDir::new();
    let file = config.0.join("server.json");
    fs::write(
        &file,
        serde_json::to_vec(&serde_json::json!({"scheme":"https","listen":listen})).unwrap(),
    )
    .unwrap();
    let configured: [&[(&str, &str)]; 2] = [
        &[("GCS_MIMIC_LISTEN", &listen), ("GCS_MIMIC_SCHEME", "https")],
        &[("GCS_MIMIC_CONFIG", file.to_str().unwrap())],
    ];
    for environment in configured {
        let output = wait(environment);
        assert!(output.status.success(), "{environment:?}: {output:?}");
        let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(answer["reason"], "Ready", "{answer}");
    }
    let output = wait(&[("GCS_MIMIC_LISTEN", listen.as_str())]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr: Value = serde_json::from_slice(&output.stderr).unwrap();
    let fix = stderr["diagnostics"][0]["fix"].as_str().unwrap();
    assert!(
        fix.starts_with(&format!(
            "Cannot use the synchronization server at http://{listen}: "
        )),
        "{fix}"
    );
    assert!(
        fix.contains("check that the server serves HTTP, the scheme in the server configuration (GCS_MIMIC_LISTEN, GCS_MIMIC_SCHEME or the config file GCS_MIMIC_CONFIG selects)"),
        "{fix}"
    );
    assert!(!fix.contains("Command failed"), "{fix}");
    server.stop();
}

/// An object renamed through the API to a name the directory can hold, copied and its original
/// deleted, takes its finding away by the next wait alone: no rescan stands between the
/// correction and a wait that requires no findings, which is why the finding's remedy ends by
/// asking to wait again.
#[test]
fn live_cli_wait_takes_in_an_unmapped_object_renamed_through_the_api_without_a_rescan() {
    let data = TestDir::new();
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    let mut server = Server::start(&data, Some(&tree));
    // The first wait takes in the bucket directory, so the bucket exists for the upload.
    assert_eq!(
        server.cli("wait", &["--timeout", "30s"], true)["reason"],
        "Ready"
    );
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=dir%5Cfile",
            None,
            b"backslash",
        )
        .expect(200);
    let strict = server.cli_run("wait", &["--timeout", "30s", "--require-no-findings"]);
    assert_eq!(
        strict.answer["reason"], "FindingsPending",
        "{:?}",
        strict.stderr
    );
    let revision = strict.answer["findings"]["revision"].as_str().unwrap();
    let listed = server.cli(
        "status",
        &["--view", "findings", "--revision", revision],
        true,
    );
    let remedy = listed["items"][0]["remedy"].as_str().unwrap();
    assert!(
        remedy.ends_with("; then wait again: gcs-mimic sync wait --endpoint URL"),
        "{remedy}"
    );
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/dir%5Cfile/copyTo/b/test-bucket/o/dir%2Ffile",
            None,
            b"",
        )
        .expect(200);
    server
        .request(
            "DELETE",
            "/storage/v1/b/test-bucket/o/dir%5Cfile",
            None,
            b"",
        )
        .expect(204);
    let renamed = server.cli_run("wait", &["--timeout", "30s", "--require-no-findings"]);
    assert_eq!(renamed.code, Some(0), "{:?}", renamed.stderr);
    assert_eq!(renamed.answer["reason"], "Ready");
    assert_eq!(renamed.answer["findings"]["unmapped"], 0);
    assert_eq!(
        fs::read(tree.0.join("test-bucket/dir/file")).unwrap(),
        b"backslash"
    );
    server.stop();
}

/// A name anyone can give a directory in the tree reaches the terminal of whoever waits, as an
/// entry the wait ended on. Its control characters, ESC and a newline as well as DEL and the C1
/// CSI that the JSON envelope leaves alone, reach standard error escaped and never raw, while
/// the JSON answer keeps the name as it is.
#[test]
fn live_cli_wait_escapes_the_control_characters_of_an_entry_it_names() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let hostile = "Bad\u{1b}[2J\nname\u{7f}\u{9b}31m";
    fs::create_dir(tree.0.join(hostile)).unwrap();
    let mut server = Server::start(&data, Some(&tree));
    let Output {
        status,
        stdout,
        stderr,
    } = common::server_command()
        .args(["sync", "wait", "--endpoint", &server.endpoint])
        .args(["--timeout", "30s"])
        .output()
        .unwrap();
    let answer: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(answer["reason"], "SourceUnrepresentable", "{answer}");
    assert_eq!(answer["source_entries"]["entries"][0]["path"], hostile);
    assert_eq!(status.code(), Some(2));
    let stderr = String::from_utf8(stderr).unwrap();
    let lines: Vec<&str> = stderr.lines().collect();
    assert!(
        lines.iter().all(|line| !line.chars().any(char::is_control)),
        "{stderr:?}"
    );
    let errors: Vec<String> = lines
        .iter()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .flat_map(|line| line["diagnostics"].as_array().cloned().unwrap_or_default())
        .filter(|diagnostic| diagnostic["key"] == "sync")
        .map(|diagnostic| diagnostic["fix"].as_str().unwrap().to_owned())
        .collect();
    let [error] = errors.try_into().unwrap();
    assert!(
        error.starts_with(
            r"sync wait ended with SourceUnrepresentable (1 source entry: Bad\u{1b}[2J\nname\u{7f}\u{9b}31m [unmappable-name]): "
        ),
        "{error}"
    );
    server.stop();
}

#[test]
fn live_cli_wait_after_writing_observes_bytes_without_retrying() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let mut server = Server::start(&data, Some(&tree));
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/body"), b"fresh body").unwrap();
    let waited = server.cli("wait", &["--timeout", "10s"], true);
    assert_eq!(waited["completed"], true);
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/body?alt=media",
                None,
                b""
            )
            .body,
        b"fresh body"
    );
    server.stop();
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
    let stale = server.cli("approve", &["--deletion-set", &"0".repeat(32)], false);
    assert_eq!(stale["error"]["code"], 409);
    // --revision is not approval's option, so the command refuses it without a request.
    let refused = common::server_command()
        .args(["sync", "approve", "--endpoint", &server.endpoint])
        .args(["--revision", &"0".repeat(32)])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1));
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--deletion-set"));
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
fn live_cli_rejects_old_token_environment_without_sending_a_request() {
    let root = TestDir::new();
    let token = root.0.join("token");
    fs::write(&token, "secret-file-token").unwrap();
    let variable = RenamedEnvironmentVariable::SyncToken;
    for (new, file, old) in [
        (None, None, "secret-old-token"),
        (Some("secret-new-token"), None, "secret-old-token"),
        (None, Some(&token), "secret-old-token"),
        (Some(""), None, ""),
    ] {
        let mut command = common::server_command();
        command.env(variable.old_name(), old);
        if let Some(new) = new {
            command.env(variable.name(), new);
        }
        let mut args = vec!["sync", "status", "--endpoint", "http://127.0.0.1:1"];
        if file.is_some() {
            args.extend(["--token-file", token.to_str().unwrap()]);
        }
        let output = command.args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        let event: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(event["diagnostics"][0]["key"], "sync");
        assert_eq!(event["diagnostics"][0]["fix"], variable.fix());
        assert!(!text.contains("secret"));
    }
    let output = common::server_command()
        .env(variable.name(), "")
        .args(["sync", "status", "--endpoint", "http://127.0.0.1:1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(
        text.contains("GCSM_SYNC_TOKEN must contain one nonempty bearer token"),
        "{text}"
    );
    assert!(!text.contains("Token file"), "{text}");
}

/// Runs `gcs-mimic sync status` against a server that answers its one request with `status`,
/// the extra header lines `headers` and the JSON `body`.
fn status_against(status: &'static str, headers: String, body: &'static [u8]) -> Output {
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
        write!(stream.get_mut(), "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.get_mut().write_all(body).unwrap();
    });
    let output = common::server_command()
        .args(["sync", "status", "--endpoint", &endpoint])
        .output()
        .unwrap();
    receiver.join().unwrap();
    output
}

#[test]
fn native_cli_prints_unavailable_json_and_fails_without_retry() {
    let output = status_against(
        "503 Service Unavailable",
        common::sync_version_header(),
        br#"{"error":{"code":503,"message":"busy"}}"#,
    );
    assert!(!output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["error"]["code"],
        503
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP 503"));
}

/// A server of another release, or one from before the version check, answers in a protocol
/// this command may misread, so even a success fails naming the release to use. The answer is
/// still printed, as every answer is.
#[test]
fn native_cli_refuses_a_server_of_another_release() {
    for (headers, expected) in [
        ("x-gcs-mimic-version: 0.0.1\r\n".to_owned(), "0.0.1"),
        (String::new(), "reported no version"),
    ] {
        let output = status_against("200 OK", headers, b"{}");
        assert_eq!(output.status.code(), Some(1), "{expected}");
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            serde_json::json!({})
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected) && stderr.contains(gcs_mimic::VERSION),
            "{stderr}"
        );
    }
}
