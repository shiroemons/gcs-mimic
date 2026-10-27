#[path = "../../tests/common/mod.rs"]
mod common;

use common::TestDir;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, Stdio},
    time::Duration,
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}

#[tokio::test]
async fn grpc_idle_admission_and_sigterm_drain() {
    let dir = TestDir::new();
    let mut server = Server(
        common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--max-connections",
                "1",
                "--connection-timeout-ms",
                "500",
                "--data-dir",
            ])
            .arg(&dir.0)
            .env("GCS_MIMIC_GRPC_LISTEN", "127.0.0.1:0")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = server.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let _ = send.send((stdout, line));
    });
    let (mut stdout, line) = receive.recv_timeout(Duration::from_secs(5)).unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["event"], "ready");
    let http_address = ready["listen"].as_str().unwrap();
    let grpc_address = ready["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["scheme"] == "h2c")
        .unwrap()["listen"]
        .as_str()
        .unwrap();
    let connect_grpc = || {
        let mut stream = TcpStream::connect(grpc_address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
            .unwrap();
        let mut settings = [0; 9];
        stream.read_exact(&mut settings).unwrap();
        assert_eq!(
            settings[3], 4,
            "server SETTINGS proves gRPC owns the sole connection slot"
        );
        stream
    };
    let mut idle = connect_grpc();
    let mut http = TcpStream::connect(http_address).unwrap();
    http.write_all(b"GET /_internal/ready HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    http.set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let error = http.read(&mut [0; 1]).unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ));
    http.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut response = String::new();
    http.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "idle gRPC must release shared HTTP admission: {response}"
    );
    if let Err(error) = idle.read_to_end(&mut Vec::new()) {
        assert!(common::closed_by_server(&error), "{error}");
    }

    let mut grpc = connect_grpc();
    common::stop_server(&mut server.0, &mut stdout);
    if let Err(error) = grpc.read_to_end(&mut Vec::new()) {
        assert!(common::closed_by_server(&error), "{error}");
    }
    assert!(TcpStream::connect(http_address).is_err());
    assert!(TcpStream::connect(grpc_address).is_err());
    let catalog = gcs_mimic::storage::Catalog::open(&dir.0).await.unwrap();
    catalog.close().await.unwrap();
}
