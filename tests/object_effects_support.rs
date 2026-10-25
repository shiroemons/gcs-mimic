#![allow(dead_code)]

#[path = "common/mod.rs"]
pub(crate) mod common;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    time::Duration,
};

pub struct Server {
    pub child: Child,
    pub address: String,
    stdout: BufReader<ChildStdout>,
}
pub struct Reply {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
impl Reply {
    pub fn json(&self) -> Value {
        assert_eq!(self.status, 200, "{}", String::from_utf8_lossy(&self.body));
        serde_json::from_slice(&self.body).unwrap()
    }
    pub fn error(&self, status: u16, reason: &str) {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        let value: Value = serde_json::from_slice(&self.body).unwrap();
        assert_eq!(value["error"]["errors"][0]["reason"], reason);
    }
}
impl Server {
    pub fn start(dir: &common::TestDir, strict: bool) -> Self {
        Self::start_with_catalog_wait(dir, strict, 30000)
    }
    pub fn start_with_catalog_wait(dir: &common::TestDir, strict: bool, milliseconds: u64) -> Self {
        Self::start_with_timeouts(dir, strict, milliseconds, 900000)
    }
    pub fn start_with_timeouts(
        dir: &common::TestDir,
        strict: bool,
        catalog_milliseconds: u64,
        request_milliseconds: u64,
    ) -> Self {
        let mut command = common::server_command();
        command
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&dir.0)
            .args(["--api-base-url", "http://127.0.0.1:4443"])
            .arg("--catalog-wait-timeout-ms")
            .arg(catalog_milliseconds.to_string())
            .arg("--object-request-timeout-ms")
            .arg(request_milliseconds.to_string());
        if strict {
            let identities = dir.0.join("identities.json");
            std::fs::write(&identities, serde_json::json!({"identities":[
                {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
                {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"local":["roles/storage.objectViewer"]}},
                {"id":"creator@example.test","tokens":["creator"],"project_roles":{"local":["roles/storage.objectCreator"]}}
            ]}).to_string()).unwrap();
            command
                .args(["--auth-mode", "strict", "--identity-file"])
                .arg(identities);
        }
        let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
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
    pub fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        request(&self.address, method, path, headers, body)
    }
    pub fn bucket(&self) {
        let reply = self.request(
            "POST",
            "/storage/v1/b?project=local",
            "Authorization: Bearer admin\r\n",
            br#"{"name":"test-bucket"}"#,
        );
        reply.json();
    }
    pub fn put(&self, name: &str, bytes: &[u8]) -> Reply {
        self.request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?uploadType=media&name={name}"),
            "Authorization: Bearer admin\r\n",
            bytes,
        )
    }
    pub fn get(&self, name: &str) -> Reply {
        self.request(
            "GET",
            &format!("/storage/v1/b/test-bucket/o/{name}"),
            "Authorization: Bearer admin\r\n",
            b"",
        )
    }
    pub fn list(&self, query: &str) -> Value {
        self.request(
            "GET",
            &format!("/storage/v1/b/test-bucket/o?{query}"),
            "Authorization: Bearer admin\r\n",
            b"",
        )
        .json()
    }
    pub fn stop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
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
pub fn settings(dir: &common::TestDir, versioning: bool, hold: bool) {
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap().execute(
        "UPDATE buckets SET versioning_enabled = ?1, default_event_based_hold = ?2 WHERE name='test-bucket'", rusqlite::params![versioning, hold],
    ).unwrap();
}
pub fn request(address: &str, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(body).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (key, value) = line.trim_end().split_once(':').unwrap();
        headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
    }
    let mut body = Vec::new();
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value == "chunked")
    {
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            let size = usize::from_str_radix(line.trim(), 16).unwrap();
            if size == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..]).unwrap();
            let mut crlf = [0; 2];
            reader.read_exact(&mut crlf).unwrap();
            assert_eq!(&crlf, b"\r\n");
        }
    } else {
        reader.read_to_end(&mut body).unwrap();
    }
    Reply {
        status,
        headers,
        body,
    }
}
pub fn blobs(dir: &common::TestDir) -> usize {
    std::fs::read_dir(dir.0.join("blobs"))
        .unwrap()
        .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum()
}
