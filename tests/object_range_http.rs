mod common;

use common::TestDir;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    time::Duration,
};

struct Server {
    child: Child,
    address: String,
    stdout: BufReader<ChildStdout>,
}

struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Server {
    fn start(dir: &TestDir, strict: bool) -> Self {
        let mut command = common::server_command();
        command
            .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
            .arg(&dir.0);
        if strict {
            command.args([
                "--auth-mode",
                "strict",
                "--identity-file",
                "config/identities.json",
            ]);
        }
        let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        Self {
            child,
            address: ready["listen"].as_str().unwrap().into(),
            stdout,
        }
    }

    fn open(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> BufReader<TcpStream> {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: local\r\nContent-Length: {}\r\n{headers}\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
        BufReader::new(stream)
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        let mut stream = self.open(method, path, headers, body);
        let mut reply = Reply::headers(&mut stream);
        reply.read_body(&mut stream);
        reply
    }

    fn setup(&self, auth: &str, body: &[u8]) -> Value {
        assert_eq!(
            self.request(
                "POST",
                "/storage/v1/b?project=local-project",
                auth,
                br#"{"name":"range-bucket"}"#
            )
            .status,
            200
        );
        self.upload(auth, body)
    }

    fn upload(&self, auth: &str, body: &[u8]) -> Value {
        let reply = self.request(
            "POST",
            "/upload/storage/v1/b/range-bucket/o?uploadType=media&name=data",
            &format!("{auth}Content-Type: application/octet-stream\r\n"),
            body,
        );
        assert_eq!(reply.status, 200);
        serde_json::from_slice(&reply.body).unwrap()
    }

    fn get(&self, query: &str, headers: &str) -> Reply {
        self.request(
            "GET",
            &format!("/storage/v1/b/range-bucket/o/data?alt=media{query}"),
            headers,
            b"",
        )
    }

    fn stop(&mut self) {
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

impl Reply {
    fn headers(stream: &mut impl BufRead) -> Self {
        let mut line = String::new();
        assert_ne!(stream.read_line(&mut line).unwrap(), 0);
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            assert_ne!(stream.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            let (key, value) = line.split_once(':').unwrap();
            headers.insert(key.to_ascii_lowercase(), value.trim().into());
        }
        Self {
            status,
            headers,
            body: Vec::new(),
        }
    }

    fn read_body(&mut self, stream: &mut impl Read) {
        let length = if matches!(self.status, 204 | 304) {
            0
        } else {
            self.headers["content-length"].parse().unwrap()
        };
        self.body.resize(length, 0);
        stream.read_exact(&mut self.body).unwrap();
    }

    fn error(&self, status: u16, reason: &str) {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        let value: Value = serde_json::from_slice(&self.body).unwrap();
        assert_eq!(value["error"]["code"], status);
        assert_eq!(value["error"]["errors"][0]["reason"], reason);
    }
}

#[test]
fn obj_range_http_single_ranges_and_representation_headers() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    let object = server.setup("", b"0123456789");
    assert_eq!(server.request("PATCH", "/storage/v1/b/range-bucket/o/data", "", br#"{"contentDisposition":"attachment; filename=data.bin","cacheControl":"public, max-age=60"}"#).status, 200);
    let full = server.get("", "");
    for prefix in ["", "/download"] {
        for (range, want, bounds) in [
            ("bytes=2-5", "2345", "2-5"),
            ("bytes=8-999", "89", "8-9"),
            ("bytes=6-", "6789", "6-9"),
            ("bytes=-3", "789", "7-9"),
            ("bytes=-99", "0123456789", "0-9"),
            ("bytes=0-0", "0", "0-0"),
            ("bytes=9-9", "9", "9-9"),
            ("BYTES=0002-0005", "2345", "2-5"),
            ("bytes= 2-5", "2345", "2-5"),
            ("bytes=2-999999999999999999999999999", "23456789", "2-9"),
            ("bytes=-999999999999999999999999999", "0123456789", "0-9"),
        ] {
            let reply = server.request(
                "GET",
                &format!("{prefix}/storage/v1/b/range-bucket/o/data?alt=media"),
                &format!("Range: {range}\r\n"),
                b"",
            );
            assert_eq!(reply.status, 206, "{range}");
            assert_eq!(reply.body, want.as_bytes(), "{range}");
            assert_eq!(reply.headers["content-length"], want.len().to_string());
            assert_eq!(reply.headers["content-range"], format!("bytes {bounds}/10"));
            for header in [
                "etag",
                "content-type",
                "content-disposition",
                "cache-control",
                "x-goog-hash",
                "x-goog-generation",
                "x-goog-metageneration",
                "x-goog-stored-content-length",
                "x-goog-stored-content-encoding",
            ] {
                assert_eq!(reply.headers[header], full.headers[header], "{header}");
            }
            assert_eq!(
                reply.headers["x-goog-generation"],
                object["generation"].as_str().unwrap()
            );
            assert_eq!(reply.headers["accept-ranges"], "bytes");
            assert_eq!(reply.headers["cache-control"], "no-cache");
        }
    }
    assert_eq!(full.status, 200);
    assert!(!full.headers.contains_key("content-range"));
    server.stop();
}

#[test]
fn obj_range_http_unsatisfiable_empty_and_ignored_ranges() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.setup("", b"0123456789");
    for range in [
        "bytes=10-",
        "bytes=100-200",
        "bytes=-0",
        "bytes=999999999999999999999999999-",
    ] {
        let reply = server.get("", &format!("Range: {range}\r\n"));
        reply.error(416, "requestedRangeNotSatisfiable");
        assert_eq!(reply.headers["content-range"], "bytes */10");
        assert_eq!(
            reply.headers["content-type"],
            "application/json; charset=UTF-8"
        );
        assert!(!reply.headers.contains_key("x-goog-hash"));
    }
    for range in [
        "",
        "invalid",
        "items=0-1",
        "bytes=",
        "bytes=-",
        "bytes=3-2",
        "bytes=+1-2",
        "bytes=1-2-3",
        "bytes=1 -2",
        "bytes=999999999999999999999999999-999999999999999999999999998",
        "bytes=0-1,4-5",
        "bytes=0-1\r\nRange: bytes=4-5",
    ] {
        let reply = server.get("", &format!("Range: {range}\r\n"));
        assert_eq!(reply.status, 200, "{range}");
        assert_eq!(reply.body, b"0123456789");
        assert!(!reply.headers.contains_key("content-range"));
    }
    for validator in ["\"etag\"", "Wed, 21 Oct 2015 07:28:00 GMT", "W/\"weak\""] {
        server
            .get(
                "",
                &format!("Range: bytes=0-1\r\nIf-Range: {validator}\r\n"),
            )
            .error(501, "notImplemented");
    }
    server.upload("", b"");
    for range in ["bytes=0-0", "bytes=0-", "bytes=-0"] {
        let reply = server.get("", &format!("Range: {range}\r\n"));
        reply.error(416, "requestedRangeNotSatisfiable");
        assert_eq!(reply.headers["content-range"], "bytes */0");
    }
    let suffix = server.get("", "Range: bytes=-1\r\n");
    assert_eq!(suffix.status, 200);
    assert!(suffix.body.is_empty());
    assert!(!suffix.headers.contains_key("content-range"));
    assert_eq!(server.get("", "").body, b"");
    server.stop();
}

#[test]
fn obj_range_http_generation_preconditions_and_overwrite_pin_open_reader() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    let object = server.setup("", &vec![b'a'; 8 * 1024 * 1024]);
    let generation = object["generation"].as_str().unwrap();
    let reply = server.get(
        &format!("&generation={generation}&ifGenerationMatch={generation}&ifMetagenerationMatch=1"),
        "Range: bytes=2-5\r\n",
    );
    assert_eq!(reply.status, 206);
    assert_eq!(reply.body, b"aaaa");
    for query in ["&ifGenerationMatch=0", "&ifMetagenerationMatch=2"] {
        server
            .get(query, "Range: bytes=999999999-\r\n")
            .error(412, "conditionNotMet");
    }
    for query in [
        format!("&ifGenerationNotMatch={generation}"),
        "&ifMetagenerationNotMatch=1".into(),
    ] {
        let reply = server.get(&query, "Range: bytes=999999999-\r\n");
        assert_eq!(reply.status, 304);
        assert!(reply.body.is_empty());
    }
    server
        .get("&generation=1", "Range: bytes=999999999-\r\n")
        .error(404, "notFound");
    let mut stream = server.open(
        "GET",
        &format!("/download/storage/v1/b/range-bucket/o/data?alt=media&generation={generation}"),
        "Range: bytes=1-\r\n",
        b"",
    );
    let mut reply = Reply::headers(&mut stream);
    assert_eq!(reply.status, 206);
    let new = server.upload("", b"replacement");
    assert_ne!(new["generation"], object["generation"]);
    reply.read_body(&mut stream);
    assert_eq!(reply.headers["x-goog-generation"], generation);
    assert_eq!(reply.body.len(), 8 * 1024 * 1024 - 1);
    assert!(reply.body.iter().all(|byte| *byte == b'a'));
    server
        .get(&format!("&generation={generation}"), "Range: bytes=0-1\r\n")
        .error(404, "notFound");
    assert_eq!(server.get("", "Range: bytes=0-2\r\n").body, b"rep");
    server.stop();
}

#[test]
fn obj_range_http_strict_authorization_precedes_range_evaluation() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, true);
    server.setup("Authorization: Bearer local-admin-token\r\n", b"0123456789");
    for range in ["bytes=0-1", "bytes=100-", "bytes=0-1\r\nIf-Range: opaque"] {
        server
            .get("", &format!("Range: {range}\r\n"))
            .error(401, "required");
        server
            .get(
                "",
                &format!("Authorization: Bearer local-creator-token\r\nRange: {range}\r\n"),
            )
            .error(403, "forbidden");
    }
    let reply = server.get(
        "",
        "Authorization: Bearer local-viewer-token\r\nRange: bytes=-3\r\n",
    );
    assert_eq!(reply.status, 206);
    assert_eq!(reply.body, b"789");
    server.stop();
}

#[test]
fn obj_range_http_gzip_ranges_use_stored_bytes_without_transcoding() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    let gzip = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03\xcb\x48\xcd\xc9\xc9\x07\x00\x86\xa6\x10\x36\x05\x00\x00\x00";
    server.setup("", gzip);
    assert_eq!(
        server
            .request(
                "PATCH",
                "/storage/v1/b/range-bucket/o/data",
                "",
                br#"{"contentEncoding":"gzip"}"#
            )
            .status,
        200
    );
    server
        .get("", "Range: bytes=2-5\r\n")
        .error(501, "notImplemented");
    let reply = server.get("", "Range: bytes=2-5\r\nAccept-Encoding: gzip\r\n");
    assert_eq!(reply.status, 206);
    assert_eq!(reply.body, gzip[2..6]);
    assert_eq!(reply.headers["content-encoding"], "gzip");
    assert_eq!(
        reply.headers["content-range"],
        format!("bytes 2-5/{}", gzip.len())
    );
    assert_eq!(
        reply.headers["x-goog-stored-content-length"],
        gzip.len().to_string()
    );
    assert_eq!(
        server
            .request(
                "PATCH",
                "/storage/v1/b/range-bucket/o/data",
                "",
                br#"{"cacheControl":"no-transform"}"#
            )
            .status,
        200
    );
    let reply = server.get("", "Range: bytes=-4\r\n");
    assert_eq!(reply.status, 206);
    assert_eq!(reply.body, gzip[gzip.len() - 4..]);
    server.stop();
}
