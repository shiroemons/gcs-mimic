use crate::common::server::declare_server;
mod common;

#[path = "object_range_http/etag.rs"]
mod etag;

use common::TestDir;
use common::server::{http_command, write_request};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read},
    net::TcpStream,
    time::Duration,
};

struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Server {
    fn start(dir: &TestDir, strict: bool) -> Self {
        Self::start_with_profile(dir, strict, false)
    }

    fn start_with_profile(dir: &TestDir, strict: bool, legacy: bool) -> Self {
        let mut command = http_command(&dir.0);
        if legacy {
            command.args(["--compat-profile", "legacy-emulator"]);
        }
        if strict {
            command.args([
                "--auth-mode",
                "strict",
                "--identity-file",
                "config/identities.json",
            ]);
        }
        Self::launch(&mut command)
    }

    fn open(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> BufReader<TcpStream> {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write_request(&mut stream, method, path, "local", headers, body);
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
        self.0.stop();
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

    fn read_body(&mut self, stream: &mut impl BufRead) {
        if self
            .headers
            .get("transfer-encoding")
            .is_some_and(|value| value == "chunked")
        {
            loop {
                let mut line = String::new();
                assert_ne!(
                    stream.read_line(&mut line).unwrap(),
                    0,
                    "incomplete chunked body"
                );
                let size = usize::from_str_radix(line.trim(), 16).unwrap();
                if size == 0 {
                    line.clear();
                    stream.read_line(&mut line).unwrap();
                    assert_eq!(line, "\r\n");
                    return;
                }
                let offset = self.body.len();
                self.body.resize(offset + size, 0);
                stream.read_exact(&mut self.body[offset..]).unwrap();
                let mut crlf = [0; 2];
                stream.read_exact(&mut crlf).unwrap();
                assert_eq!(&crlf, b"\r\n");
            }
        }
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
        // A missing media object and an unsatisfiable range are plain HTML-typed messages.
        if self.headers["content-type"] == "text/html; charset=UTF-8" {
            let body = String::from_utf8_lossy(&self.body);
            match (status, reason) {
                (404, "notFound") => assert!(body.starts_with("No such object: "), "{body}"),
                (412, "conditionNotMet") => assert_eq!(body, "Precondition Failed"),
                (416, "requestedRangeNotSatisfiable") => {
                    assert_eq!(body, "Request range not satisfiable")
                }
                _ => panic!("unexpected plain page {status} {reason}: {body}"),
            }
            return;
        }
        let value: Value = serde_json::from_slice(&self.body).unwrap_or_else(|_| {
            panic!("not a JSON error: {}", String::from_utf8_lossy(&self.body))
        });
        assert_eq!(value["error"]["code"], status);
        assert_eq!(value["error"]["errors"][0]["reason"], reason, "{value}");
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
                "x-goog-generation",
                "x-goog-metageneration",
                "x-goog-stored-content-length",
                "x-goog-stored-content-encoding",
            ] {
                assert_eq!(reply.headers[header], full.headers[header], "{header}");
            }
            // Cloud Storage omits hashes from a partial JSON range and sends only the
            // CRC32C when the range covers the whole object.
            if bounds == "0-9" {
                assert_eq!(reply.headers["x-goog-hash"], "crc32c=KAwGng==", "{range}");
            } else {
                assert!(!reply.headers.contains_key("x-goog-hash"), "{range}");
            }
            assert_eq!(
                reply.headers["x-goog-generation"],
                object["generation"].as_str().unwrap()
            );
            // Cloud Storage sends no accept-ranges on a JSON media response (observed 2026-10-31).
            assert!(!reply.headers.contains_key("accept-ranges"), "{range}");
            assert_eq!(
                reply.headers["cache-control"],
                "no-cache, no-store, max-age=0, must-revalidate"
            );
        }
    }
    assert_eq!(full.status, 200);
    assert_eq!(
        full.headers["x-goog-hash"],
        "crc32c=KAwGng==,md5=eB5eJF1ptWaXm4bijSPyxw=="
    );
    assert!(!full.headers.contains_key("content-range"));
    assert!(!full.headers.contains_key("accept-ranges"));
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
        // The recorded 416 is HTML-typed with private caching, not a JSON error.
        assert_eq!(reply.headers["content-type"], "text/html; charset=UTF-8");
        assert_eq!(reply.headers["cache-control"], "private, max-age=0");
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
        "bytes=0-1\r\nRange: bytes=4-5",
    ] {
        let reply = server.get("", &format!("Range: {range}\r\n"));
        assert_eq!(reply.status, 200, "{range}");
        assert_eq!(reply.body, b"0123456789");
        assert!(!reply.headers.contains_key("content-range"));
    }
    for validator in ["\"etag\"", "Wed, 21 Oct 2015 07:28:00 GMT", "W/\"weak\""] {
        // If-Range is not evaluated: the Range applies whatever the validator (recorded).
        let reply = server.get(
            "",
            &format!("Range: bytes=0-1\r\nIf-Range: {validator}\r\n"),
        );
        assert_eq!((reply.status, reply.body.as_slice()), (206, &b"01"[..]));
    }
    // Several ranges are refused instead of ignored (recorded 501).
    let multiple = server.get("", "Range: bytes=0-1,4-5\r\n");
    assert_eq!(multiple.status, 501);
    assert_eq!(multiple.body, b"Multiple Ranges are not supported");
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
    // If-Range is ignored (recorded): the Range still applies.
    let if_range = server.get(
        "",
        "Authorization: Bearer local-viewer-token\r\nRange: bytes=0-1\r\nIf-Range: opaque\r\n",
    );
    assert_eq!(if_range.status, 206);
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
    let expanded = server.get("", "Range: bytes=2-5\r\n");
    assert_eq!(expanded.status, 200);
    assert_eq!(expanded.body, b"hello");
    for name in [
        "content-encoding",
        "content-range",
        "content-length",
        "accept-ranges",
    ] {
        assert!(!expanded.headers.contains_key(name), "{name}");
    }
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

declare_server!();
