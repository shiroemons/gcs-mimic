#![cfg(unix)]

mod common;

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
}

impl Server {
    fn start(directory: &Path) -> Self {
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
            .args([
                "--api-base-url",
                "https://advertised.invalid",
                "--max-connections",
                "4",
            ])
            .stdout(Stdio::piped())
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
        assert_eq!(ready["api_base_url"], "https://advertised.invalid");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        Self {
            child,
            stdout,
            path,
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
