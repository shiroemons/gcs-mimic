mod common;

use common::TestDir;
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::Stdio,
    time::{Duration, Instant},
};

fn events(bytes: &[u8]) -> Vec<Value> {
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn log_levels_filter_runtime_events_and_preserve_lifecycle_and_input_warnings() {
    for level in ["debug", "info", "warn", "warning", "error"] {
        let directory = TestDir::new();
        let seed = directory.0.join("seed");
        fs::create_dir(&seed).unwrap();
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--log-level",
                level,
                "--seed-dir",
            ])
            .arg(&seed)
            .arg("--data-dir")
            .arg(directory.0.join("data"))
            .env("FAKE_GCS_UNRELATED", "ignored")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        for request in [
            "GET /_internal/health HTTP/1.1\r\nHost: localhost\r\n\r\n",
            "GET /?X-Goog-Signature=secret HTTP/1.1\r\nAuthorization: secret\r\nbad\r\n\r\n",
        ] {
            let mut connection = TcpStream::connect(ready["listen"].as_str().unwrap()).unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            connection.write_all(request.as_bytes()).unwrap();
            common::read_http_response(&mut BufReader::new(connection), false).unwrap();
        }
        for (method, path, body) in [
            (
                "POST",
                "/storage/v1/b?project=example-project",
                r#"{"name":"example-bucket"}"#,
            ),
            ("GET", "/storage/v1/b/example-bucket/o?softDeleted=true", ""),
            ("GET", "/storage/v1/b/example-bucket/o", ""),
            (
                "PATCH",
                "/storage/v1/b/example-bucket",
                r#"{"softDeletePolicy":{"retentionDurationSeconds":"604800"}}"#,
            ),
            ("GET", "/storage/v1/b/example-bucket/o?softDeleted=true", ""),
        ] {
            let mut connection = TcpStream::connect(ready["listen"].as_str().unwrap()).unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            write!(connection, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            let response =
                common::read_http_response(&mut BufReader::new(connection), false).unwrap();
            let response = String::from_utf8(response).unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            if method == "GET" {
                let (_, body) = response.split_once("\r\n\r\n").unwrap();
                assert_eq!(
                    serde_json::from_str::<Value>(body).unwrap(),
                    serde_json::json!({"kind":"storage#objects"})
                );
            }
        }
        common::stop_server(&mut child, &mut stdout);
        let mut stderr = Vec::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_end(&mut stderr)
            .unwrap();
        let records = events(&stderr);
        let listing_events: Vec<_> = records
            .iter()
            .filter(|event| event["event"] == "soft_deleted_listing_empty")
            .collect();
        assert_eq!(listing_events.len(), usize::from(level == "debug"));
        for event in listing_events {
            assert_eq!(event["level"], "debug");
            let message = event["message"].as_str().unwrap();
            assert!(message.contains("not configured"));
            assert!(message.contains("retentionDurationSeconds=604800"));
        }
        assert!(!String::from_utf8_lossy(&stderr).contains("secret"));
        assert!(
            records
                .iter()
                .any(|event| event["event"] == "diagnostics" && event["level"] == "warn")
        );
        for (name, expected) in [
            ("connection_closed", level == "debug"),
            ("seeding", matches!(level, "debug" | "info")),
            ("stopping", matches!(level, "debug" | "info")),
            ("connection_failed", level != "error"),
        ] {
            assert_eq!(
                records.iter().any(|event| event["event"] == name),
                expected,
                "{level}: {records:?}"
            );
        }
    }
}

#[test]
fn keep_alive_idle_close_is_quiet_and_a_stalled_next_head_warns() {
    let directory = TestDir::new();
    let mut child = common::server_command()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--connection-timeout-ms",
            "300",
            "--keep-alive-timeout-ms",
            "3000",
            "--data-dir",
        ])
        .arg(directory.0.join("data"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["keep_alive_timeout_ms"], 3000);
    let served = || {
        let connection = TcpStream::connect(ready["listen"].as_str().unwrap()).unwrap();
        connection
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut connection = BufReader::new(connection);
        connection
            .get_mut()
            .write_all(b"GET /_internal/health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        common::read_http_response(&mut connection, false).unwrap();
        connection
    };
    // Waiting for the next request uses the keep-alive timeout, not the header deadline.
    let mut idle = served();
    let started = Instant::now();
    assert_eq!(idle.read(&mut [0]).unwrap(), 0);
    assert!(
        started.elapsed() >= Duration::from_millis(2500),
        "{:?}",
        started.elapsed()
    );
    // Once the next request head starts, the header deadline applies again.
    let mut stalled = served();
    stalled
        .get_mut()
        .write_all(b"GET /_internal/health HTTP/1.1\r\n")
        .unwrap();
    let started = Instant::now();
    assert_eq!(stalled.read(&mut [0]).unwrap(), 0);
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "{:?}",
        started.elapsed()
    );
    common::stop_server(&mut child, &mut stdout);
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    let records = events(&stderr);
    let timeouts: Vec<_> = records
        .iter()
        .filter(|event| event["event"] == "connection_timeout")
        .collect();
    // Only the stalled head warns; the idle close is a normal keep-alive ending.
    assert_eq!(timeouts.len(), 1, "{records:?}");
    assert!(
        timeouts[0]["message"]
            .as_str()
            .unwrap()
            .contains("request headers"),
        "{records:?}"
    );
    assert!(
        !records
            .iter()
            .any(|event| event["event"] == "connection_failed"),
        "{records:?}"
    );
}

#[test]
fn invalid_log_levels_and_legacy_diagnostics_fail_before_catalog() {
    for level in ["", "trace", "off", "fatal", "INFO"] {
        let directory = TestDir::new();
        let data = directory.0.join("data");
        let output = common::server_command()
            .args(["serve", "--log-level", level, "--data-dir"])
            .arg(&data)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!data.exists());
        let records = events(&output.stderr);
        assert_eq!(records[0]["diagnostics"][0]["key"], "--log-level");
    }
    for level in ["debug", "info", "warn", "error"] {
        let directory = TestDir::new();
        let data = directory.0.join("data");
        let output = common::server_command()
            .args(["serve", "--log-level", level, "--data-dir"])
            .arg(&data)
            .env("FAKE_GCS_LOG_LEVEL", "warning")
            .env("FAKE_GCS_UNRELATED", "ignored")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!data.exists());
        let records = events(&output.stderr);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["level"], "warn");
        assert_eq!(records[1]["level"], "error");
        assert_eq!(records[1]["diagnostics"][0]["key"], "FAKE_GCS_LOG_LEVEL");
        assert!(
            records[1]["diagnostics"][0]["fix"]
                .as_str()
                .unwrap()
                .contains("GCS_MIMIC_LOG_LEVEL=warn")
        );
    }
}
