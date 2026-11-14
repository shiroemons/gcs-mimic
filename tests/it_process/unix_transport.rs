#![cfg(unix)]

use crate::common;

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Stdio},
    time::Duration,
};

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    path: PathBuf,
    ready: serde_json::Value,
}

impl Server {
    fn start(directory: &Path) -> Self {
        let server = Self::launch(directory, &["--api-base-url", "https://advertised.invalid"]);
        common::process::assert_ready_fields(&server.ready, &["unix_socket"]);
        assert_eq!(server.ready["api_base_url"], "https://advertised.invalid");
        server
    }

    fn launch(directory: &Path, args: &[&str]) -> Self {
        Self::launch_with_stderr(directory, args, Stdio::inherit())
    }

    fn launch_with_stderr(directory: &Path, args: &[&str], stderr: Stdio) -> Self {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("s");
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--backend",
                "memory",
                "--unix-socket",
            ])
            .arg(&path)
            .args(args)
            .args(["--max-connections", "4"])
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        assert_eq!(ready["transport"], "unix");
        assert_eq!(ready["unix_socket"], path.to_str().unwrap());
        assert!(ready["listen"].is_null());
        assert_eq!(ready["listeners"], serde_json::json!([]));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        Self {
            child,
            stdout,
            path,
            ready,
        }
    }

    fn request(&self, method: &str, target: &str, body: &[u8]) -> String {
        let mut socket = UnixStream::connect(&self.path).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(socket,"{method} {target} HTTP/1.1\r\nHost: original.invalid\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
        socket.write_all(body).unwrap();
        let mut data = String::new();
        socket.read_to_string(&mut data).unwrap();
        data
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

#[test]
fn unix_wire_uses_shared_crud_conditions_parallel_clients_and_graceful_cleanup() {
    let directory = common::TestDir::new();
    let mut server = Server::start(&directory.0);
    let created = server.request(
        "POST",
        "/storage/v1/b?project=local",
        br#"{"name":"unix-bucket"}"#,
    );
    assert!(created.starts_with("HTTP/1.1 200"), "{created}");
    assert!(created.contains("https://advertised.invalid"), "{created}");
    let uploaded = server.request(
        "POST",
        "/upload/storage/v1/b/unix-bucket/o?uploadType=media&name=value",
        b"shared bytes",
    );
    assert!(uploaded.starts_with("HTTP/1.1 200"), "{uploaded}");
    let rejected = server.request(
        "POST",
        "/upload/storage/v1/b/unix-bucket/o?uploadType=media&name=value&ifGenerationMatch=0",
        b"must not replace",
    );
    assert!(rejected.starts_with("HTTP/1.1 412"), "{rejected}");
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let read =
                    server.request("GET", "/storage/v1/b/unix-bucket/o/value?alt=media", b"");
                assert!(
                    read.starts_with("HTTP/1.1 200") && read.contains("shared bytes"),
                    "{read}"
                );
            });
        }
    });
    common::stop_server(&mut server.child, &mut server.stdout);
    assert!(
        !server.path.exists(),
        "graceful shutdown must await socket cleanup"
    );
    assert!(UnixStream::connect(&server.path).is_err());
}

#[test]
fn unix_socket_cleanup_failure_is_reported_once_against_the_socket() {
    let directory = common::TestDir::new();
    let mut server = Server::launch_with_stderr(&directory.0, &[], Stdio::piped());
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o500)).unwrap();
    // The server runs as this test's user, so the probe sees what the cleanup will.
    let probe = directory.0.join("probe");
    if fs::write(&probe, b"").is_ok() {
        eprintln!(
            "SKIP: a read-only directory permits new entries (for example when running as root)"
        );
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
        common::stop_server(&mut server.child, &mut server.stdout);
        return;
    }
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &server.child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let mut stderr = String::new();
    server
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = server.child.wait().unwrap();
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!status.success(), "{stderr}");
    assert!(server.path.exists(), "the denied cleanup leaves the socket");
    let errors: Vec<serde_json::Value> = stderr
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["event"] == "diagnostics" && event["level"] == "error")
        .flat_map(|event| event["diagnostics"].as_array().unwrap().clone())
        .collect();
    assert_eq!(errors.len(), 1, "{stderr}");
    assert_eq!(errors[0]["source"], "listener", "{stderr}");
    assert_eq!(errors[0]["key"], "unix_socket", "{stderr}");
    let fix = errors[0]["fix"].as_str().unwrap();
    assert!(
        fix.starts_with("Cannot remove Unix socket (PermissionDenied): ")
            && fix.ends_with("; remove the stale entry before restarting."),
        "{fix}"
    );
    assert_eq!(fix.matches("Cannot remove").count(), 1, "{fix}");
}

#[test]
fn unix_links_use_http_and_the_request_host_without_an_explicit_base() {
    let directory = common::TestDir::new();
    let mut server = Server::launch(&directory.0, &[]);
    assert_eq!(server.ready["api_base_url"], "http://localhost");
    let created = server.request(
        "POST",
        "/storage/v1/b?project=local",
        br#"{"name":"unix-links"}"#,
    );
    assert!(created.starts_with("HTTP/1.1 200"), "{created}");
    assert!(
        created.contains("\"http://original.invalid/storage/v1/b/unix-links\""),
        "{created}"
    );
    common::stop_server(&mut server.child, &mut server.stdout);
}

#[test]
fn unix_cli_rejects_explicit_tcp_tls_and_foreign_entries_before_ready() {
    let directory = common::TestDir::new();
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.0.join("s");
    for extra in [
        vec!["--listen", "127.0.0.1:0"],
        vec!["--grpc-listen", "127.0.0.1:0"],
        vec!["--listen-http", "127.0.0.1:0"],
        vec!["--scheme", "https"],
    ] {
        let output = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--backend",
                "memory",
                "--unix-socket",
            ])
            .arg(&path)
            .args(extra)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("\"event\":\"ready\""));
        assert!(!path.exists());
    }
    fs::write(&path, b"foreign file").unwrap();
    let output = common::server_command()
        .args([
            "serve",
            "--scheme",
            "http",
            "--backend",
            "memory",
            "--unix-socket",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(&path).unwrap(), b"foreign file");
}
