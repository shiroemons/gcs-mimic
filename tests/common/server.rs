#![expect(
    dead_code,
    reason = "Each integration binary uses a different subset of the harness."
)]

use super::{TestDir, server_command, stop_server};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

pub(crate) fn write_request(
    stream: &mut impl Write,
    method: &str,
    path: &str,
    host: &str,
    headers: &str,
    body: &[u8],
) {
    stream
        .write_all(request_head(method, path, host, headers, body.len()).as_bytes())
        .unwrap();
    stream.write_all(body).unwrap();
}

pub(crate) fn request_head(
    method: &str,
    path: &str,
    host: &str,
    headers: &str,
    length: usize,
) -> String {
    format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {length}\r\n{headers}\r\n")
}

pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: Vec<u8>,
}

impl Reply {
    pub(crate) fn json_error(&self, status: u16, reason: &str) {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        // The upload front end labelled some JSON envelopes text/html (recorded).
        let content_type = self.headers["content-type"].as_str();
        assert!(
            matches!(
                content_type,
                "application/json; charset=UTF-8" | "text/html; charset=UTF-8"
            ),
            "{content_type}"
        );
        let body: Value = serde_json::from_slice(&self.body).unwrap_or_else(|_| {
            panic!("not a JSON error: {}", String::from_utf8_lossy(&self.body))
        });
        let error = &body["error"];
        assert_eq!(error["code"], status);
        assert_eq!(error["errors"][0]["domain"], "global");
        assert_eq!(error["errors"][0]["reason"], reason, "{error}");
        assert_eq!(error["errors"][0]["message"], error["message"]);
    }

    pub(crate) fn framed(stream: TcpStream) -> Self {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0, "HTTP status");
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0, "HTTP headers");
            if line == "\r\n" {
                break;
            }
            let (key, value) = line.trim_end().split_once(':').unwrap();
            headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
        }
        let mut body = Vec::new();
        if !matches!(status, 204 | 304) {
            if headers
                .get("transfer-encoding")
                .is_some_and(|v| v == "chunked")
            {
                loop {
                    line.clear();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0, "chunk length");
                    let length = usize::from_str_radix(line.trim(), 16).unwrap();
                    if length == 0 {
                        break;
                    }
                    let start = body.len();
                    body.resize(start + length, 0);
                    reader.read_exact(&mut body[start..]).unwrap();
                    let mut crlf = [0; 2];
                    reader.read_exact(&mut crlf).unwrap();
                    assert_eq!(&crlf, b"\r\n");
                }
            } else {
                let length = headers["content-length"].parse().unwrap();
                let read = reader.by_ref().take(length).read_to_end(&mut body);
                assert!(
                    read.is_ok() && body.len() as u64 == length,
                    "HTTP {status} body ended after {} of {length} bytes ({read:?}); headers: {headers:?}",
                    body.len()
                );
            }
        }
        if headers
            .get("connection")
            .is_some_and(|value| value == "close")
        {
            super::await_server_close(&mut reader);
        }
        Self {
            status,
            headers,
            body,
        }
    }
}

pub(crate) struct Server {
    pub(crate) child: Child,
    pub(crate) address: String,
    pub(crate) stdout: BufReader<ChildStdout>,
    pub(crate) ready: Value,
    pub(crate) client_timeout: Duration,
    shutdown: Shutdown,
}

enum Shutdown {
    Kill,
    Graceful,
}

pub(crate) fn http_command(data: &Path) -> Command {
    let mut command = server_command();
    command
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
        .arg(data);
    command
}

impl Server {
    pub(crate) fn launch(command: &mut Command) -> Self {
        let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).expect("ready event after recovery");
        assert_eq!(ready["event"], "ready", "{ready}");
        Self {
            address: ready["listen"].as_str().unwrap().into(),
            child,
            stdout,
            ready,
            client_timeout: Duration::from_secs(5),
            shutdown: Shutdown::Kill,
        }
    }

    pub(crate) fn graceful(mut self) -> Self {
        self.shutdown = Shutdown::Graceful;
        self
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            return;
        }
        match self.shutdown {
            Shutdown::Graceful => stop_server(&mut self.child, &mut self.stdout),
            Shutdown::Kill if self.child.try_wait().unwrap().is_none() => {
                self.child.kill().unwrap();
                self.child.wait().unwrap();
            }
            Shutdown::Kill => {}
        }
    }
}

pub(crate) fn staging_count(dir: &TestDir) -> usize {
    std::fs::read_dir(dir.0.join("staging")).unwrap().count()
}

pub(crate) fn blob_count(dir: &TestDir) -> usize {
    std::fs::read_dir(dir.0.join("blobs"))
        .unwrap()
        .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum()
}

pub(crate) fn no_leaks(dir: &TestDir, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let staging = staging_count(dir);
        let blobs = blob_count(dir);
        if staging == 0 && blobs == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "staging={staging}, blobs={blobs}, expected={expected}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
