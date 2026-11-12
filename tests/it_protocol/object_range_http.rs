use crate::common;
use crate::common::process::declare_server;

mod etag;

use common::TestDir;
use common::process::{http_command, write_request};
use serde_json::Value;
use std::{
    io::{BufReader, Read},
    net::TcpStream,
    time::Duration,
};

use common::http::Reply;

impl Server {
    fn start(dir: &TestDir, strict: bool) -> Self {
        Self::start_with_profile(dir, strict, false)
    }

    fn start_with_profile(dir: &TestDir, strict: bool, legacy: bool) -> Self {
        let mut command = http_command(&dir.0);
        if legacy {
            command.args(["--compat-profile", "fake-gcs-server"]);
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
        let mut reply = Reply::read_head(&mut stream);
        reply.read_body(&mut stream);
        reply
    }

    fn setup(&self, auth: &str, body: &[u8]) -> Value {
        self.request(
            "POST",
            "/storage/v1/b?project=local-project",
            auth,
            br#"{"name":"range-bucket"}"#,
        )
        .expect(200);
        self.upload(auth, body)
    }

    fn upload(&self, auth: &str, body: &[u8]) -> Value {
        let reply = self.request(
            "POST",
            "/upload/storage/v1/b/range-bucket/o?uploadType=media&name=data",
            &format!("{auth}Content-Type: application/octet-stream\r\n"),
            body,
        );
        reply.expect(200);
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

#[test]
fn obj_range_http_single_ranges_and_representation_headers() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    let object = server.setup("", b"0123456789");
    server.request("PATCH", "/storage/v1/b/range-bucket/o/data", "", br#"{"contentDisposition":"attachment; filename=data.bin","cacheControl":"public, max-age=60"}"#).expect(200);
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
            reply.expect_with(206, format_args!("{range}"));
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
    full.expect(200);
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
        reply.html_or_json_error(416, "requestedRangeNotSatisfiable");
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
        reply.expect_with(200, format_args!("{range}"));
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
    multiple.expect(501);
    assert_eq!(multiple.body, b"Multiple Ranges are not supported");
    server.upload("", b"");
    for range in ["bytes=0-0", "bytes=0-", "bytes=-0"] {
        let reply = server.get("", &format!("Range: {range}\r\n"));
        reply.html_or_json_error(416, "requestedRangeNotSatisfiable");
        assert_eq!(reply.headers["content-range"], "bytes */0");
    }
    let suffix = server.get("", "Range: bytes=-1\r\n");
    suffix.expect(200);
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
    reply.expect(206);
    assert_eq!(reply.body, b"aaaa");
    for query in ["&ifGenerationMatch=0", "&ifMetagenerationMatch=2"] {
        server
            .get(query, "Range: bytes=999999999-\r\n")
            .html_or_json_error(412, "conditionNotMet");
    }
    for query in [
        format!("&ifGenerationNotMatch={generation}"),
        "&ifMetagenerationNotMatch=1".into(),
    ] {
        let reply = server.get(&query, "Range: bytes=999999999-\r\n");
        reply.expect(304);
        assert!(reply.body.is_empty());
    }
    server
        .get("&generation=1", "Range: bytes=999999999-\r\n")
        .html_or_json_error(404, "notFound");
    let mut stream = server.open(
        "GET",
        &format!("/download/storage/v1/b/range-bucket/o/data?alt=media&generation={generation}"),
        "Range: bytes=1-\r\n",
        b"",
    );
    let mut reply = Reply::read_head(&mut stream);
    reply.expect(206);
    let new = server.upload("", b"replacement");
    assert_ne!(new["generation"], object["generation"]);
    reply.read_body(&mut stream);
    assert_eq!(reply.headers["x-goog-generation"], generation);
    assert_eq!(reply.body.len(), 8 * 1024 * 1024 - 1);
    assert!(reply.body.iter().all(|byte| *byte == b'a'));
    server
        .get(&format!("&generation={generation}"), "Range: bytes=0-1\r\n")
        .html_or_json_error(404, "notFound");
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
            .html_or_json_error(401, "required");
        server
            .get(
                "",
                &format!("Authorization: Bearer local-creator-token\r\nRange: {range}\r\n"),
            )
            .html_or_json_error(403, "forbidden");
    }
    let reply = server.get(
        "",
        "Authorization: Bearer local-viewer-token\r\nRange: bytes=-3\r\n",
    );
    reply.expect(206);
    assert_eq!(reply.body, b"789");
    // If-Range is ignored (recorded): the Range still applies.
    let if_range = server.get(
        "",
        "Authorization: Bearer local-viewer-token\r\nRange: bytes=0-1\r\nIf-Range: opaque\r\n",
    );
    if_range.expect(206);
    server.stop();
}

#[test]
fn obj_range_http_gzip_ranges_use_stored_bytes_without_transcoding() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    let gzip = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03\xcb\x48\xcd\xc9\xc9\x07\x00\x86\xa6\x10\x36\x05\x00\x00\x00";
    server.setup("", gzip);
    server
        .request(
            "PATCH",
            "/storage/v1/b/range-bucket/o/data",
            "",
            br#"{"contentEncoding":"gzip"}"#,
        )
        .expect(200);
    let expanded = server.get("", "Range: bytes=2-5\r\n");
    expanded.expect(200);
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
    reply.expect(206);
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
    server
        .request(
            "PATCH",
            "/storage/v1/b/range-bucket/o/data",
            "",
            br#"{"cacheControl":"no-transform"}"#,
        )
        .expect(200);
    let reply = server.get("", "Range: bytes=-4\r\n");
    reply.expect(206);
    assert_eq!(reply.body, gzip[gzip.len() - 4..]);
    server.stop();
}

declare_server!();
