use crate::common::{self, TestDir};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    time::Duration,
};

pub(super) struct Server {
    child: Child,
    address: String,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    pub(super) fn start(dir: &TestDir, options: &[&str]) -> Self {
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&dir.0)
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
            address: ready["listen"].as_str().unwrap().into(),
            stdout,
        }
    }

    pub(super) fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        self.raw(&format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n", body.len()), body)
    }

    pub(super) fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
    }

    pub(super) fn raw(&self, head: &str, body: &[u8]) -> Reply {
        let mut stream = self.connect();
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        let mut reader = BufReader::new(stream);
        let raw = common::read_http_response(&mut reader, head.starts_with("HEAD ")).unwrap();
        let split = raw
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap();
        let headers = std::str::from_utf8(&raw[..split]).unwrap();
        let mut body = &raw[split + 4..];
        let mut decoded = Vec::new();
        if headers.contains("transfer-encoding: chunked") {
            loop {
                let end = body.windows(2).position(|bytes| bytes == b"\r\n").unwrap();
                let size =
                    usize::from_str_radix(std::str::from_utf8(&body[..end]).unwrap(), 16).unwrap();
                body = &body[end + 2..];
                if size == 0 {
                    break;
                }
                decoded.extend_from_slice(&body[..size]);
                assert_eq!(&body[size..size + 2], b"\r\n");
                body = &body[size + 2..];
            }
            body = &decoded;
        }
        let reply = Reply {
            status: headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
            headers: headers.into(),
            body: String::from_utf8(body.to_vec()).unwrap(),
        };
        if headers.contains("\r\nconnection: close") {
            common::await_server_close(&mut reader);
        }
        reply
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }
}

pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) headers: String,
    pub(super) body: String,
}

impl Reply {
    pub(super) fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
    pub(super) fn status(&self, expected: u16) {
        assert_eq!(self.status, expected, "{}", self.body);
        assert!(self.headers.contains("content-type:"));
    }
}

pub(super) fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}
