mod common;

use common::TestDir;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::{Child, ChildStdout, Stdio},
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

fn start_server(
    data_dir: &Path,
    connection_timeout: Option<&str>,
) -> (Server, BufReader<ChildStdout>, serde_json::Value) {
    let mut command = common::server_command();
    command
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--max-connections",
            "1",
            "--data-dir",
        ])
        .arg(data_dir);
    if let Some(timeout) = connection_timeout {
        command.args(["--connection-timeout-ms", timeout]);
    }
    let mut server = Server(
        command
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
    let (stdout, line) = receive.recv_timeout(Duration::from_secs(5)).unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["event"], "ready");
    (server, stdout, ready)
}

#[tokio::test]
async fn grpc_idle_admission_and_sigterm_drain() {
    let dir = TestDir::new();
    let (mut server, mut stdout, ready) = start_server(&dir.0, Some("500"));
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
        stream.write_all(&[0, 0, 0, 4, 0, 0, 0, 0, 0]).unwrap();
        let mut settings = [0; 9];
        stream.read_exact(&mut settings).unwrap();
        assert_eq!(
            settings[3], 4,
            "server SETTINGS proves gRPC owns the sole connection slot"
        );
        let length = u32::from_be_bytes([0, settings[0], settings[1], settings[2]]) as usize;
        stream.read_exact(&mut vec![0; length]).unwrap();
        stream.write_all(&[0, 0, 0, 4, 1, 0, 0, 0, 0]).unwrap();
        stream
    };
    let mut idle = connect_grpc();
    let mut http = TcpStream::connect(http_address).unwrap();
    // A one-shot client asks the server to close, so the response read ends at EOF.
    http.write_all(
        b"GET /_internal/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
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
    let mut frames = Vec::new();
    idle.read_to_end(&mut frames).unwrap();
    let mut goaway = false;
    while frames.len() >= 9 {
        let length = u32::from_be_bytes([0, frames[0], frames[1], frames[2]]) as usize;
        assert!(frames.len() >= 9 + length);
        if frames[3] == 7 {
            assert_eq!(&frames[13..17], &[0; 4]);
            goaway = true;
        }
        frames.drain(..9 + length);
    }
    assert!(goaway, "idle gRPC must send GOAWAY before normal EOF");

    let mut grpc = TcpStream::connect(grpc_address).unwrap();
    grpc.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    common::stop_server(&mut server.0, &mut stdout);
    if let Err(error) = grpc.read_to_end(&mut Vec::new()) {
        assert!(common::closed_by_server(&error), "{error}");
    }
    assert!(TcpStream::connect(http_address).is_err());
    assert!(TcpStream::connect(grpc_address).is_err());
    let catalog = gcs_mimic::storage::Catalog::open(&dir.0).await.unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_sigterm_default_deadlines_close_incomplete_handshake() {
    let dir = TestDir::new();
    let (mut server, mut stdout, ready) = start_server(&dir.0, None);
    assert_eq!(ready["connection_timeout_ms"], 5000);
    assert_eq!(ready["shutdown_timeout_ms"], 5000);
    let http_address = ready["listen"].as_str().unwrap();
    let grpc_address = ready["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["scheme"] == "h2c")
        .unwrap()["listen"]
        .as_str()
        .unwrap();
    let mut grpc = TcpStream::connect(grpc_address).unwrap();
    grpc.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
    // Server SETTINGS proves admission without sending any client preface.
    let mut settings = [0; 9];
    grpc.read_exact(&mut settings).unwrap();
    assert_eq!(settings[3], 4);
    common::stop_server(&mut server.0, &mut stdout);
    if let Err(error) = grpc.read_to_end(&mut Vec::new()) {
        assert!(common::closed_by_server(&error), "{error}");
    }
    assert!(TcpStream::connect(http_address).is_err());
    assert!(TcpStream::connect(grpc_address).is_err());
    let catalog = gcs_mimic::storage::Catalog::open(&dir.0).await.unwrap();
    catalog.close().await.unwrap();
}
