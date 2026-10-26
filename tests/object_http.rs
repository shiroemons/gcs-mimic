mod common;

#[path = "object_http/upload_admission.rs"]
mod upload_admission;

use common::{TestDir, closed_by_server};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Server {
    child: Child,
    address: String,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(dir: &TestDir, limit: u64, timeout: u64) -> Self {
        Self::with_timeouts(dir, limit, timeout, 900_000, 30_000)
    }

    fn with_timeouts(
        dir: &TestDir,
        limit: u64,
        timeout: u64,
        object_timeout: u64,
        catalog_timeout: u64,
    ) -> Self {
        Self::with_connections(dir, limit, timeout, object_timeout, catalog_timeout, 32)
    }

    fn with_connections(
        dir: &TestDir,
        limit: u64,
        timeout: u64,
        object_timeout: u64,
        catalog_timeout: u64,
        connections: usize,
    ) -> Self {
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
            .args([
                "--api-base-url",
                "https://advertised.invalid/api",
                "--max-object-bytes",
                &limit.to_string(),
                "--connection-timeout-ms",
                &timeout.to_string(),
                "--object-request-timeout-ms",
                &object_timeout.to_string(),
                "--catalog-wait-timeout-ms",
                &catalog_timeout.to_string(),
                "--max-connections",
                &connections.to_string(),
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).expect("ready after recovery");
        assert_eq!(ready["event"], "ready");
        assert_eq!(ready["max_object_bytes"], limit);
        assert_eq!(ready["object_request_timeout_ms"], object_timeout);
        assert_eq!(ready["catalog_wait_timeout_ms"], catalog_timeout);
        Self {
            child,
            address: ready["listen"].as_str().unwrap().into(),
            stdout,
        }
    }

    fn stream(&self) -> TcpStream {
        let stream = TcpStream::connect(&self.address).unwrap();
        // A hang guard, not a latency bound: concurrent suites on slow x86 macOS
        // runners exceeded 5 s for ordinary responses.
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
    }

    fn raw(&self, request: &[u8]) -> Reply {
        let mut stream = self.stream();
        stream.write_all(request).unwrap();
        Reply::read(stream)
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        let mut request = format!("{method} {path} HTTP/1.1\r\nHost: untrusted.invalid\r\nX-Forwarded-Host: evil.invalid\r\nContent-Length: {}\r\n{headers}\r\n", body.len()).into_bytes();
        request.extend_from_slice(body);
        self.raw(&request)
    }

    fn bucket(&self) {
        assert_eq!(
            self.request(
                "POST",
                "/storage/v1/b?project=local",
                "Content-Type: application/json\r\n",
                br#"{"name":"test-bucket","storageClass":"NEARLINE"}"#
            )
            .status,
            200
        );
    }

    fn upload(&self, query: &str, headers: &str, body: &[u8]) -> Reply {
        self.request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?uploadType=media&{query}"),
            headers,
            body,
        )
    }

    fn get(&self, name: &str) -> Reply {
        self.request(
            "GET",
            &format!("/storage/v1/b/test-bucket/o/{name}"),
            "",
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

struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Reply {
    fn read(stream: TcpStream) -> Self {
        let mut reader = BufReader::new(stream);
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            assert_ne!(
                reader.read_until(b'\n', &mut bytes).unwrap(),
                0,
                "HTTP response headers"
            );
        }
        let boundary = bytes
            .windows(4)
            .position(|v| v == b"\r\n\r\n")
            .expect("HTTP response");
        let text = std::str::from_utf8(&bytes[..boundary]).unwrap();
        let mut lines = text.split("\r\n");
        let status = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers: BTreeMap<String, String> = lines
            .map(|line| {
                let (key, value) = line.split_once(':').unwrap();
                (key.to_ascii_lowercase(), value.trim().into())
            })
            .collect();
        // A complete framed response stays valid even if a late request write causes a reset.
        let length = if matches!(status, 204 | 304) {
            0
        } else {
            headers["content-length"].parse().unwrap()
        };
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        Self {
            status,
            headers,
            body,
        }
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    fn error(&self, status: u16, reason: &str) {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        assert_eq!(
            self.headers["content-type"],
            "application/json; charset=UTF-8"
        );
        let error = &self.json()["error"];
        assert_eq!(error["code"], status);
        assert_eq!(error["errors"][0]["domain"], "global");
        assert_eq!(error["errors"][0]["reason"], reason);
        assert_eq!(error["errors"][0]["message"], error["message"]);
    }
}

#[test]
fn obj_http_round_trip_exact_metadata_media_headers_and_nonempty_bucket() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1_000_000, 5000);
    server.bucket();
    let reply = server.upload(
        "name=notes%2Fhello%20%E6%97%A5.txt",
        "Content-Type: text/plain\r\nX-Goog-Hash: crc32c=4waSgw==,md5=JfnnlDI7RTiF9RgfG2JNCw==\r\n",
        b"123456789",
    );
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(
        reply.headers["content-type"],
        "application/json; charset=UTF-8"
    );
    let value = reply.json();
    let generation = value["generation"].as_str().unwrap();
    assert!(generation.parse::<i64>().unwrap() > 0);
    let time = value["timeCreated"].as_str().unwrap();
    assert!(time.ends_with('Z') && time.len() == 24);
    assert_eq!(
        value,
        json!({
            "kind":"storage#object", "id":format!("test-bucket/notes/hello 日.txt/{generation}"),
            "name":"notes/hello 日.txt", "bucket":"test-bucket", "generation":generation,
            "metageneration":"1", "contentType":"text/plain", "storageClass":"NEARLINE", "size":"9",
            "md5Hash":"JfnnlDI7RTiF9RgfG2JNCw==", "crc32c":"4waSgw==", "etag":value["etag"],
            "timeCreated":time, "updated":time, "timeStorageClassUpdated":time, "timeFinalized":time,
            "selfLink":"https://advertised.invalid/api/storage/v1/b/test-bucket/o/notes%2Fhello%20%E6%97%A5.txt",
            "mediaLink":format!("https://advertised.invalid/api/download/storage/v1/b/test-bucket/o/notes%2Fhello%20%E6%97%A5.txt?generation={generation}&alt=media")
        })
    );
    assert_eq!(server.get("notes%2Fhello%20%E6%97%A5.txt").json(), value);
    for prefix in ["storage", "download/storage"] {
        let media = server.request("GET", &format!("/{prefix}/v1/b/test-bucket/o/notes%2Fhello%20%E6%97%A5.txt?alt=media&generation={generation}"), "", b"");
        assert_eq!(media.status, 200);
        assert_eq!(media.body, b"123456789");
        for (name, expected) in [
            ("content-type", "text/plain"),
            ("content-length", "9"),
            ("x-goog-generation", generation),
            ("x-goog-metageneration", "1"),
            (
                "x-goog-hash",
                "crc32c=4waSgw==,md5=JfnnlDI7RTiF9RgfG2JNCw==",
            ),
            ("x-goog-stored-content-length", "9"),
            ("x-goog-stored-content-encoding", "identity"),
            ("etag", value["etag"].as_str().unwrap()),
        ] {
            assert_eq!(media.headers[name], expected, "{name}");
        }
    }
    server
        .request("DELETE", "/storage/v1/b/test-bucket", "", b"")
        .error(409, "conflict");
    let deleted = server.request(
        "DELETE",
        "/storage/v1/b/test-bucket/o/notes%2Fhello%20%E6%97%A5.txt",
        "",
        b"",
    );
    assert_eq!(deleted.status, 204);
    assert!(deleted.body.is_empty());
    server
        .get("notes%2Fhello%20%E6%97%A5.txt")
        .error(404, "notFound");
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket", "", b"")
            .status,
        204
    );
}

#[test]
fn obj_http_zero_multichunk_binary_and_size_limits() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 200_000, 5000);
    server.bucket();
    let empty = server.upload("name=empty", "", b"");
    assert_eq!(empty.status, 200);
    assert_eq!(empty.json()["contentType"], "application/octet-stream");
    assert_eq!(empty.json()["size"], "0");
    assert!(server.get("empty?alt=media").body.is_empty());
    let payload: Vec<u8> = (0..150_000).map(|n| (n % 256) as u8).collect();
    let mut request = b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=chunks HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for part in payload.chunks(17_000) {
        request.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
        request.extend_from_slice(part);
        request.extend_from_slice(b"\r\n");
    }
    request.extend_from_slice(b"0\r\n\r\n");
    assert_eq!(server.raw(&request).json()["size"], "150000");
    assert_eq!(server.get("chunks?alt=media").body, payload);
    server.raw(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=early HTTP/1.1\r\nHost: local\r\nContent-Length: 200001\r\n\r\n").error(413, "uploadTooLarge");
    server.get("early").error(404, "notFound");
    let small = TestDir::new();
    let limited = Server::start(&small, 5, 5000);
    limited.bucket();
    limited.raw(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=over HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n").error(413, "uploadTooLarge");
    limited.get("over").error(404, "notFound");
    assert_eq!(limited.upload("name=exact", "", b"12345").status, 200);
}

#[test]
fn obj_http_bad_hashes_and_unsupported_requests_never_publish() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 5000);
    server.bucket();
    for hash in ["md5=AAAAAAAAAAAAAAAAAAAAAA==", "crc32c=AAAAAA=="] {
        server
            .upload("name=bad", &format!("X-Goog-Hash: {hash}\r\n"), b"payload")
            .error(400, "invalid");
        server.get("bad").error(404, "notFound");
    }
    server.raw(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart&name=unsupported HTTP/1.1\r\nHost: local\r\nContent-Length: 999\r\n\r\n").error(400, "invalidParameter");
    server.get("unsupported").error(404, "notFound");
    let existing = server.upload("name=existing", "", b"old");
    assert_eq!(existing.status, 200);
    let generation = existing.json()["generation"].as_str().unwrap().to_owned();
    for (header, read_status, read_reason) in [
        ("If-Match: opaque\r\n", 400, "invalidParameter"),
        ("If-None-Match: *\r\n", 304, "notModified"),
        ("X-Goog-Encryption-Key: key\r\n", 501, "notImplemented"),
        ("X-Goog-Meta-Example: value\r\n", 501, "notImplemented"),
    ] {
        server
            .upload("name=existing", header, b"new")
            .error(501, "notImplemented");
        server.raw(format!("POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=existing HTTP/1.1\r\nHost: local\r\nContent-Length: 900\r\n{header}\r\n").as_bytes()).error(501, "notImplemented");
        server
            .request(
                "DELETE",
                "/storage/v1/b/test-bucket/o/existing",
                header,
                b"",
            )
            .error(501, "notImplemented");
        for query in ["", "?alt=media"] {
            let reply = server.request(
                "GET",
                &format!("/storage/v1/b/test-bucket/o/existing{query}"),
                header,
                b"",
            );
            if read_status == 304 {
                assert_eq!(reply.status, 304);
                assert!(reply.body.is_empty());
            } else {
                reply.error(read_status, read_reason);
            }
        }
    }
    server
        .request(
            "GET",
            &format!(
                "/storage/v1/b/test-bucket/o/existing?alt=media&ifGenerationNotMatch={generation}"
            ),
            "If-Match: \"x\"\r\n",
            b"",
        )
        .error(412, "conditionNotMet");
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/missing?alt=media",
            "X-Goog-Encryption-Key: key\r\n",
            b"",
        )
        .error(501, "notImplemented");
    let partial = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/existing?alt=media",
        "Range: bytes=0-1\r\n",
        b"",
    );
    assert_eq!(partial.status, 206);
    assert_eq!(partial.body, b"ol");
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/existing?alt=media",
            "If-Range: opaque\r\n",
            b"",
        )
        .error(501, "notImplemented");
    server
        .request(
            "GET",
            "/download/storage/v1/b/test-bucket/o/existing",
            "",
            b"",
        )
        .error(400, "notDownload");
    server
        .upload("name=existing&generation=1", "", b"new")
        .error(400, "invalidParameter");
    server
        .request(
            "DELETE",
            "/storage/v1/b/test-bucket/o/existing?projection=noAcl",
            "",
            b"",
        )
        .error(400, "invalidParameter");
    assert_eq!(server.get("existing?alt=media").body, b"old");
}

#[test]
fn obj_http_all_preconditions_read_304_mutation_412_and_missing_precedence() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let original = server.upload("name=conditions", "", b"original").json();
    let generation = original["generation"]
        .as_str()
        .unwrap()
        .parse::<i64>()
        .unwrap();
    for (key, passing, failing, status) in [
        ("ifGenerationMatch", generation, generation + 1, 412),
        ("ifGenerationNotMatch", generation + 1, generation, 304),
        ("ifMetagenerationMatch", 1, 2, 412),
        ("ifMetagenerationNotMatch", 2, 1, 304),
    ] {
        for alt in ["json", "media"] {
            assert_eq!(
                server
                    .get(&format!("conditions?alt={alt}&{key}={passing}"))
                    .status,
                200
            );
            let failed = server.get(&format!("conditions?alt={alt}&{key}={failing}"));
            if status == 304 {
                assert_eq!(failed.status, 304);
                assert!(failed.body.is_empty());
            } else {
                failed.error(412, "conditionNotMet");
            }
        }
        server
            .upload(&format!("name=conditions&{key}={failing}"), "", b"bad")
            .error(412, "conditionNotMet");
        server
            .request(
                "DELETE",
                &format!("/storage/v1/b/test-bucket/o/conditions?{key}={failing}"),
                "",
                b"",
            )
            .error(412, "conditionNotMet");
        assert_eq!(server.get("conditions").json(), original);
        server
            .get(&format!("absent?{key}={failing}"))
            .error(404, "notFound");
    }
    server
        .get("conditions?ifGenerationMatch=0&ifMetagenerationNotMatch=1")
        .error(412, "conditionNotMet");
    server
        .get(&format!("conditions?generation={}", generation + 1))
        .error(404, "notFound");
    assert_eq!(
        server
            .get(&format!("conditions?generation={generation}"))
            .json(),
        original
    );
    server
        .upload("name=conditions&ifGenerationMatch=0", "", b"bad")
        .error(412, "conditionNotMet");
    for query in [
        format!("ifGenerationMatch={generation}"),
        format!("ifGenerationNotMatch={}", generation + 1),
        "ifMetagenerationMatch=1".into(),
        "ifMetagenerationNotMatch=2".into(),
    ] {
        server
            .upload(&format!("name=pass&{query}"), "", b"new")
            .error(412, "conditionNotMet");
    }
    let replaced = server.upload(&format!("name=conditions&ifGenerationMatch={generation}&ifGenerationNotMatch={}&ifMetagenerationMatch=1&ifMetagenerationNotMatch=2", generation + 1), "", b"replacement");
    assert_eq!(replaced.status, 200);
    let next_generation = replaced.json()["generation"].as_str().unwrap().to_owned();
    assert_ne!(next_generation, generation.to_string());
    assert_eq!(server.request("DELETE", &format!("/storage/v1/b/test-bucket/o/conditions?generation={next_generation}&ifGenerationMatch={next_generation}&ifMetagenerationMatch=1&ifMetagenerationNotMatch=2"), "", b"").status, 204);
}

#[test]
fn obj_http_upload_tokens_with_generation_match_allow_one_create_and_overwrite() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let token = "X-Goog-Gcs-Idempotency-Token: opaque, retry=token\r\n";
    let create = "name=token&ifGenerationMatch=0";
    let original = server.upload(create, token, b"original").json();
    server
        .upload(create, token, b"original")
        .error(412, "conditionNotMet");
    assert_eq!(server.get("token").json(), original);
    assert_eq!(server.get("token?alt=media").body, b"original");
    let overwrite = format!(
        "name=token&ifGenerationMatch={}",
        original["generation"].as_str().unwrap()
    );
    let replaced = server.upload(&overwrite, token, b"replacement").json();
    assert_ne!(replaced["generation"], original["generation"]);
    server
        .upload(&overwrite, token, b"replacement")
        .error(412, "conditionNotMet");
    assert_eq!(server.get("token").json(), replaced);
    assert_eq!(server.get("token?alt=media").body, b"replacement");
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    let blobs: usize = std::fs::read_dir(dir.0.join("blobs"))
        .unwrap()
        .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum();
    assert_eq!(blobs, 1);
}

#[test]
fn obj_http_upload_tokens_reject_unsafe_or_invalid_headers_before_body() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let token = "X-Goog-Gcs-Idempotency-Token: retry-token\r\n";
    let original = server.upload("name=existing", "", b"original").json();
    for name in ["absent", "existing"] {
        for query in [
            "",
            "&ifGenerationNotMatch=0",
            "&ifMetagenerationMatch=1",
            "&ifMetagenerationNotMatch=2",
        ] {
            server.raw(format!("POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name={name}{query} HTTP/1.1\r\nHost: local\r\nContent-Length: 900\r\n{token}\r\n").as_bytes()).error(501, "notImplemented");
        }
        for headers in [
            "X-Goog-Gcs-Idempotency-Token:\r\n".to_owned(),
            format!("{token}{token}"),
        ] {
            for query in ["", "&ifGenerationMatch=0"] {
                server.raw(format!("POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name={name}{query} HTTP/1.1\r\nHost: local\r\nContent-Length: 900\r\n{headers}\r\n").as_bytes()).error(400, "invalidParameter");
            }
        }
    }
    server.get("absent").error(404, "notFound");
    assert_eq!(server.get("existing").json(), original);
    assert_eq!(server.get("existing?alt=media").body, b"original");
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn obj_http_idempotency_token_local_read_delete_rule_and_upload_rejection() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let token = "X-Goog-Gcs-Idempotency-Token: retry-token\r\n";
    server.raw(format!("POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=unsupported-token HTTP/1.1\r\nHost: local\r\nContent-Length: 900\r\n{token}\r\n").as_bytes()).error(501, "notImplemented");
    server.get("unsupported-token").error(404, "notFound");
    let original = server.upload("name=token", "", b"payload").json();
    let path = "/storage/v1/b/test-bucket/o/token";
    let metadata = server.request("GET", path, token, b"");
    assert_eq!(metadata.status, 200);
    assert_eq!(metadata.json(), original);
    let media = server.request("GET", &format!("{path}?alt=media"), token, b"");
    assert_eq!(media.status, 200);
    assert_eq!(media.body, b"payload");
    for headers in [
        "X-Goog-Gcs-Idempotency-Token:\r\n".to_owned(),
        format!("{token}{token}"),
    ] {
        for method in ["GET", "DELETE"] {
            server
                .request(method, path, &headers, b"")
                .error(400, "invalidParameter");
        }
    }
    assert_eq!(server.get("token").json(), original);
    let deleted = server.request("DELETE", path, token, b"");
    assert_eq!(deleted.status, 204);
    assert!(deleted.body.is_empty());
    // Tokens are accepted locally without deduplication: a retry executes the delete again.
    server
        .request("DELETE", path, token, b"")
        .error(404, "notFound");
    server
        .request("GET", path, token, b"")
        .error(404, "notFound");
}

#[test]
fn obj_http_disconnected_overwrite_preserves_the_previous_generation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let original = server.upload("name=preserved", "", b"original").json();
    let mut stream = server.stream();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=preserved HTTP/1.1\r\nHost: local\r\nContent-Length: 100\r\n\r\npartial").unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    Reply::read(stream).error(400, "badRequest");
    assert_eq!(server.get("preserved").json(), original);
    assert_eq!(server.get("preserved?alt=media").body, b"original");
}

#[test]
fn obj_http_encoded_names_and_invalid_names() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 5000);
    server.bucket();
    for (encoded, decoded) in [
        ("folder%2Ffile", "folder/file"),
        ("space%20name", "space name"),
        ("%E6%97%A5", "日"),
        ("literal%252F", "literal%2F"),
    ] {
        let value = server
            .upload(&format!("name={encoded}"), "", decoded.as_bytes())
            .json();
        assert_eq!(value["name"], decoded);
        assert_eq!(server.get(encoded).json(), value);
        assert_eq!(
            server.get(&format!("{encoded}?alt=media")).body,
            decoded.as_bytes()
        );
    }
    for name in [
        "",
        ".",
        "..",
        "%0A",
        "%0D",
        "%FF",
        "%GG",
        ".well-known%2Facme-challenge%2Fbad",
        &"x".repeat(1025),
    ] {
        server
            .upload(&format!("name={name}"), "", b"bad")
            .error(400, "invalidParameter");
    }
}

#[test]
fn obj_http_concurrent_create_if_absent_has_one_winner() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let results = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let barrier = barrier.clone();
                let server = &server;
                scope.spawn(move || {
                    barrier.wait();
                    server
                        .upload("name=race&ifGenerationMatch=0", "", b"winner")
                        .status
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results.iter().filter(|status| **status == 200).count(),
        1,
        "{results:?}"
    );
    assert_eq!(
        results.iter().filter(|status| **status == 412).count(),
        7,
        "{results:?}"
    );
    assert_eq!(server.get("race?alt=media").body, b"winner");
}

#[test]
fn obj_http_restart_and_sigkill_mid_upload_clean_staging() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, 1_000_000, 5000);
    server.bucket();
    let original = server.upload("name=durable", "", b"survives").json();
    server.stop();
    let mut server = Server::start(&dir, 1_000_000, 5000);
    assert_eq!(server.get("durable").json(), original);
    let mut interrupted = server.stream();
    interrupted.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=partial HTTP/1.1\r\nHost: local\r\nContent-Length: 900000\r\n\r\npartial").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while std::fs::read_dir(dir.0.join("staging")).unwrap().count() == 0 {
        assert!(Instant::now() < deadline, "upload reached staging");
        thread::sleep(Duration::from_millis(10));
    }
    server.child.kill().unwrap();
    assert!(!server.child.wait().unwrap().success());
    drop(interrupted);
    drop(server);
    let server = Server::start(&dir, 1_000_000, 5000);
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    server.get("partial").error(404, "notFound");
    assert_eq!(server.get("durable").json(), original);
    assert_eq!(server.get("durable?alt=media").body, b"survives");
}

#[test]
fn obj_http_active_upload_outlives_total_deadline_then_idle_body_times_out() {
    let dir = TestDir::new();
    // The 80 ms chunk gap stays far below the 600 ms timeout so scheduler
    // stalls on shared CI runners cannot turn activity into inactivity.
    let server = Server::start(&dir, 1000, 600);
    server.bucket();
    let mut stream = server.stream();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=slow HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
    for _ in 0..10 {
        stream.write_all(b"1\r\na\r\n").unwrap();
        thread::sleep(Duration::from_millis(80));
    }
    stream.write_all(b"0\r\n\r\n").unwrap();
    assert_eq!(Reply::read(stream).status, 200);
    assert_eq!(server.get("slow?alt=media").body, b"aaaaaaaaaa");
    let mut stream = server.stream();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=idle HTTP/1.1\r\nHost: local\r\nContent-Length: 100\r\n\r\nx").unwrap();
    let start = Instant::now();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert!(start.elapsed() >= Duration::from_millis(500));
    assert!(start.elapsed() < Duration::from_secs(2));
    server.get("idle").error(404, "notFound");
    assert_eq!(server.upload("name=after-timeout", "", b"ok").status, 200);
}

#[test]
fn obj_http_trickle_upload_deadline_releases_permits_without_publication() {
    let dir = TestDir::new();
    let server = Server::with_timeouts(&dir, 1000, 300, 900, 5000);
    server.bucket();
    let original = server.upload("name=held-0", "", b"original").json();
    let started = Instant::now();
    let mut uploads: Vec<_> = (0..8).map(|index| {
        let mut stream = server.stream();
        stream.write_all(format!("POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=held-{index} HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n").as_bytes()).unwrap();
        stream
    }).collect();
    let admitted = Instant::now() + Duration::from_secs(2);
    while std::fs::read_dir(dir.0.join("staging")).unwrap().count() < 8 {
        assert!(Instant::now() < admitted, "all eight uploads admitted");
        thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(70));
        for stream in &mut uploads {
            // A slow runner can reach the deadline while the client still
            // trickles; the server closing first is the outcome under test.
            if let Err(error) = stream.write_all(b"1\r\na\r\n") {
                assert!(closed_by_server(&error), "{error}");
            }
        }
    }
    for mut stream in uploads {
        let mut response = Vec::new();
        if let Err(error) = stream.read_to_end(&mut response) {
            assert!(closed_by_server(&error), "{error}");
        }
        assert!(response.is_empty(), "unfinished upload must not succeed");
    }
    assert!(started.elapsed() >= Duration::from_millis(800));
    assert!(started.elapsed() < Duration::from_secs(3));
    let cleaned = Instant::now() + Duration::from_secs(3);
    while std::fs::read_dir(dir.0.join("staging")).unwrap().count() != 0 {
        assert!(Instant::now() < cleaned, "cancelled stages cleaned");
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.get("held-0").json(), original);
    assert_eq!(server.get("held-0?alt=media").body, b"original");
    for index in 1..8 {
        server.get(&format!("held-{index}")).error(404, "notFound");
    }
    assert_eq!(
        std::fs::read_dir(dir.0.join("blobs"))
            .unwrap()
            .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
            .sum::<usize>(),
        1
    );
    // Released permits admit a new upload; cleanup of the cancelled stages can
    // still hold the catalog briefly on a loaded runner, so retry its 503.
    let admitted = Instant::now() + Duration::from_secs(10);
    loop {
        let status = server.upload("name=after-deadline", "", b"ok").status;
        if status == 200 {
            break;
        }
        assert!(status == 503 && Instant::now() < admitted, "{status}");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn obj_http_trickle_download_stops_at_absolute_deadline() {
    let dir = TestDir::new();
    let size = 32 * 1024 * 1024;
    // Setup only: a slow runner can need more than 5 s to upload 32 MiB.
    let mut server = Server::start(&dir, size as u64, 30_000);
    server.bucket();
    assert_eq!(
        server.upload("name=large", "", &vec![b'x'; size]).status,
        200
    );
    server.stop();
    let server = Server::with_timeouts(&dir, size as u64, 3000, 1200, 5000);
    let mut stream = server.stream();
    let started = Instant::now();
    stream
        .write_all(
            b"GET /storage/v1/b/test-bucket/o/large?alt=media HTTP/1.1\r\nHost: local\r\n\r\n",
        )
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        assert_ne!(reader.read_until(b'\n', &mut headers).unwrap(), 0);
    }
    assert!(headers.starts_with(b"HTTP/1.1 200"));
    let mut received = 0;
    let mut chunk = vec![0; 256 * 1024];
    // Time the server's close itself, not the client's remaining sleeps.
    let closed = loop {
        match reader.read(&mut chunk) {
            Ok(0) => break started.elapsed(),
            Ok(read) => received += read,
            Err(error) if closed_by_server(&error) => break started.elapsed(),
            Err(error) => panic!("{error}"),
        }
        assert!(received < size, "trickled download completed");
        thread::sleep(Duration::from_millis(80));
    };
    assert!(
        received > 0 && received < size,
        "truncated media: {received}/{size}"
    );
    assert!(
        closed < Duration::from_millis(2800),
        "deadline precedes inactivity: closed after {closed:?}"
    );
    assert_eq!(server.get("large").json()["size"], size.to_string());
}

// Truncating a file the server holds open is Unix-only; Windows denies the resize.
#[cfg(unix)]
#[test]
fn obj_http_media_read_failure_after_headers_names_its_cause() {
    let dir = TestDir::new();
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    let server = Server {
        child,
        address: ready["listen"].as_str().unwrap().into(),
        stdout,
    };
    server.bucket();
    let size = 8 * 1024 * 1024;
    assert_eq!(
        server.upload("name=large", "", &vec![b'r'; size]).status,
        200
    );
    let blob = std::fs::read_dir(dir.0.join("blobs"))
        .unwrap()
        .flat_map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap())
        .map(|entry| entry.unwrap().path())
        .next()
        .unwrap();
    let mut stream = server.stream();
    stream
        .write_all(
            b"GET /storage/v1/b/test-bucket/o/large?alt=media HTTP/1.1\r\nHost: local\r\n\r\n",
        )
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        assert_ne!(reader.read_until(b'\n', &mut headers).unwrap(), 0);
    }
    assert!(headers.starts_with(b"HTTP/1.1 200"));
    let mut chunk = vec![0; 64 * 1024];
    let mut received = reader.read(&mut chunk).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&blob)
        .unwrap()
        .set_len(0)
        .unwrap();
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => received += read,
            Err(error) if closed_by_server(&error) => break,
            Err(error) => panic!("{error}"),
        }
    }
    assert!(received < size, "truncated media: {received}/{size}");
    drop(server);
    let mut events = String::new();
    stderr.read_to_string(&mut events).unwrap();
    let failure = events
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| event["event"] == "object_body_read_failed")
        .unwrap_or_else(|| panic!("no body read event in {events}"));
    assert_eq!(failure["level"], "warn");
    assert_eq!(failure["cause"], "unexpected end of file");
    assert!(!events.contains(dir.0.to_str().unwrap()));
}

#[test]
fn obj_http_catalog_wait_deadlines_return_backend_error_and_commit_can_finish() {
    let dir = TestDir::new();
    // The inactivity timeout stays below the catalog budget, so the waits prove
    // that inactivity is suspended during them. It also bounds the header read of
    // every request, including setup; at 100 ms a CPU-starved runner closed the
    // bucket request before its headers were dispatched.
    let server = Server::with_timeouts(&dir, 1000, 500, 5000, 2000);
    server.bucket();
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = Instant::now();
    let mut first = server.stream();
    first.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=committing HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\none").unwrap();
    let published = Instant::now() + Duration::from_secs(30);
    while std::fs::read_dir(dir.0.join("blobs"))
        .unwrap()
        .all(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count() == 0)
    {
        assert!(
            Instant::now() < published,
            "first upload reached publication"
        );
        thread::sleep(Duration::from_millis(5));
    }
    // The first commit owns the serialized catalog slot while the external lock is held.
    thread::sleep(Duration::from_millis(250));
    let mut second = server.stream();
    second.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=queued HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\ntwo").unwrap();
    // Both replies arrive while the lock is still held, so the catalog budget,
    // not the lock release, ended both waits.
    for reply in [Reply::read(first), Reply::read(second)] {
        reply.error(503, "backendError");
        assert!(
            reply.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("upload catalog wait timed out")
        );
        assert!(
            reply.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("catalog_wait_timeout_ms")
        );
    }
    assert!(started.elapsed() >= Duration::from_secs(2));
    database.execute_batch("ROLLBACK").unwrap();
    // GET response startup still has the inactivity deadline; let SQLite's busy retry finish first.
    let committed = Instant::now() + Duration::from_secs(30);
    while !database
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = 'test-bucket' AND name = 'committing')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    {
        assert!(Instant::now() < committed, "timed-out upload committed");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(server.get("committing?alt=media").body, b"one");
    server.get("queued").error(404, "notFound");
    assert_eq!(
        server
            .upload("name=after-catalog-timeout", "", b"ok")
            .status,
        200
    );
}

#[test]
fn obj_http_completed_body_waits_for_commit_past_inactivity_deadline() {
    let dir = TestDir::new();
    // Holding the lock for four inactivity timeouts shows the completed body is
    // exempt; 500 ms keeps setup header reads clear of CPU starvation.
    let server = Server::start(&dir, 1000, 500);
    server.bucket();
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut stream = server.stream();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=committed&ifGenerationMatch=0 HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\nnew").unwrap();
    thread::sleep(Duration::from_secs(2));
    database.execute_batch("ROLLBACK").unwrap();
    assert_eq!(Reply::read(stream).status, 200);
    assert_eq!(server.get("committed?alt=media").body, b"new");
}

#[test]
fn obj_http_queued_precheck_waits_past_body_inactivity_deadline() {
    let dir = TestDir::new();
    // As above: four inactivity timeouts of queueing, and 500 ms for header reads.
    let server = Server::start(&dir, 1000, 500);
    server.bucket();
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut first = server.stream();
    first.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=first HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\none").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::fs::read_dir(dir.0.join("blobs"))
        .unwrap()
        .all(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count() == 0)
    {
        assert!(
            Instant::now() < deadline,
            "first upload reached publication"
        );
        thread::sleep(Duration::from_millis(10));
    }
    // Let the first commit take the serialized catalog slot while SQLite is locked.
    thread::sleep(Duration::from_millis(250));
    let mut second = server.stream();
    second.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=second HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\ntwo").unwrap();
    let waiting = Instant::now();
    thread::sleep(Duration::from_secs(2));
    database.execute_batch("ROLLBACK").unwrap();
    assert_eq!(Reply::read(second).status, 200);
    assert!(waiting.elapsed() >= Duration::from_secs(2));
    assert_eq!(Reply::read(first).status, 200);
    assert_eq!(server.get("first?alt=media").body, b"one");
    assert_eq!(server.get("second?alt=media").body, b"two");
}

#[test]
fn obj_http_additional_upload_waits_for_advisory_check() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let mut uploads = Vec::new();
    for index in 0..8 {
        let mut stream = server.stream();
        stream.write_all(format!("POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=held-{index} HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\n").as_bytes()).unwrap();
        uploads.push(stream);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while std::fs::read_dir(dir.0.join("staging")).unwrap().count() < 8 {
        assert!(Instant::now() < deadline, "all upload bodies admitted");
        thread::sleep(Duration::from_millis(10));
    }
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    uploads[0].write_all(b"one").unwrap();
    thread::sleep(Duration::from_millis(100));
    let reply = thread::scope(|scope| {
        let reply = scope.spawn(|| {
            server.request(
                "POST",
                "/upload/storage/v1/b/missing-bucket/o?uploadType=media&name=excess",
                "",
                b"two",
            )
        });
        database.execute_batch("ROLLBACK").unwrap();
        reply.join().unwrap()
    });
    reply.error(404, "notFound");
    assert_eq!(Reply::read(uploads.remove(0)).status, 200);
}

#[test]
fn obj_http_precheck_and_method_alt_reject_before_consuming_body() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1_000_000, 2000);
    server.bucket();
    assert_eq!(server.upload("name=exists", "", b"old").status, 200);
    for (path, status, reason) in [
        (
            "/upload/storage/v1/b/missing-bucket/o?uploadType=media&name=exists",
            404,
            "notFound",
        ),
        (
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=exists&ifGenerationMatch=0",
            412,
            "conditionNotMet",
        ),
        (
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=exists&alt=media",
            400,
            "invalidParameter",
        ),
    ] {
        let start = Instant::now();
        server
            .raw(
                format!("POST {path} HTTP/1.1\r\nHost: local\r\nContent-Length: 900000\r\n\r\n")
                    .as_bytes(),
            )
            .error(status, reason);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "response must not wait for the advertised body"
        );
    }
    server
        .request(
            "DELETE",
            "/storage/v1/b/test-bucket/o/exists?alt=media",
            "",
            b"",
        )
        .error(400, "invalidParameter");
    assert_eq!(server.get("exists?alt=media").body, b"old");
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn obj_http_advisory_failures_precede_chunked_body_errors_regardless_of_timing() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 2000);
    server.bucket();
    assert_eq!(server.upload("name=exists", "", b"old").status, 200);
    let bad_hash = "X-Goog-Hash: md5=AAAAAAAAAAAAAAAAAAAAAA==\r\n";
    server
        .upload("name=exists&ifGenerationMatch=0", bad_hash, b"bad")
        .error(412, "conditionNotMet");
    server
        .request(
            "POST",
            "/upload/storage/v1/b/missing-bucket/o?uploadType=media&name=exists",
            bad_hash,
            b"bad",
        )
        .error(404, "notFound");
    for delay in [0, 300] {
        for _ in 0..3 {
            let mut stream = server.stream();
            let mut request = b"POST /upload/storage/v1/b/missing-bucket/o?uploadType=media&name=large HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            let mut body = b"1388\r\n".to_vec();
            body.extend_from_slice(&vec![b'x'; 5000]);
            body.extend_from_slice(b"\r\n0\r\n\r\n");
            if delay == 0 {
                request.extend_from_slice(&body);
                stream.write_all(&request).unwrap();
                Reply::read(stream).error(404, "notFound");
            } else {
                stream.write_all(&request).unwrap();
                Reply::read(stream.try_clone().unwrap()).error(404, "notFound");
                thread::sleep(Duration::from_millis(delay));
                // An early 404 may have closed the connection before this delayed body.
                let _ = stream.write_all(&body);
            }
        }
    }
    assert_eq!(server.get("exists?alt=media").body, b"old");
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn obj_http_early_rejections_survive_body_larger_than_socket_buffers() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 2000);
    server.bucket();
    for (query, status, reason) in [
        ("uploadType=media&name=large", 413, "uploadTooLarge"),
        ("uploadType=multipart&name=large", 400, "invalidParameter"),
        ("uploadType=media", 400, "required"),
    ] {
        let mut stream = server.stream();
        stream.write_all(format!("POST /upload/storage/v1/b/test-bucket/o?{query} HTTP/1.1\r\nHost: local\r\nContent-Length: 8388608\r\n\r\n").as_bytes()).unwrap();
        let mut writer = stream.try_clone().unwrap();
        let sending = thread::spawn(move || {
            // Rejection may interrupt the write; the response must still reach the reader.
            let _ = writer.write_all(&vec![b'x'; 8 * 1024 * 1024]);
        });
        Reply::read(stream).error(status, reason);
        sending.join().unwrap();
    }
}

#[test]
fn obj_http_sigkill_after_blob_rename_before_commit_and_after_success() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let original = server.upload("name=durable", "", b"old").json();
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    // An external reserved lock blocks the catalog transaction after durable blob publication.
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut upload = server.stream();
    upload.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=durable HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\nnew").unwrap();
    let count_blobs = || {
        std::fs::read_dir(dir.0.join("blobs"))
            .unwrap()
            .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
            .sum::<usize>()
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    while count_blobs() < 2 {
        assert!(
            Instant::now() < deadline,
            "upload reached blob publication before commit"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        database
            .query_row(
                "SELECT generation FROM objects WHERE name='durable'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap()
            .to_string(),
        original["generation"].as_str().unwrap()
    );
    server.child.kill().unwrap();
    assert!(!server.child.wait().unwrap().success());
    drop(upload);
    database.execute_batch("ROLLBACK").unwrap();
    drop(database);
    drop(server);
    let mut server = Server::start(&dir, 1000, 5000);
    assert_eq!(count_blobs(), 1);
    assert_eq!(server.get("durable").json(), original);
    assert_eq!(server.get("durable?alt=media").body, b"old");
    let committed = server.upload("name=durable", "", b"committed").json();
    server.child.kill().unwrap();
    assert!(!server.child.wait().unwrap().success());
    drop(server);
    let server = Server::start(&dir, 1000, 5000);
    assert_eq!(server.get("durable").json(), committed);
    assert_eq!(server.get("durable?alt=media").body, b"committed");
    assert_eq!(count_blobs(), 1);
}

fn resumable_path(server: &Server) -> String {
    let reply = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=durable",
        "Content-Type: application/json\r\n",
        br#"{"contentType":"application/test","metadata":{"source":"initiation"}}"#,
    );
    assert_eq!(reply.status, 200);
    reply.headers["location"]
        .strip_prefix("https://advertised.invalid/api")
        .unwrap()
        .to_owned()
}

fn resumable_progress(database: &rusqlite::Connection) -> (u64, String) {
    database
        .query_row("SELECT offset, state FROM resumable_sessions", [], |row| {
            Ok((u64::try_from(row.get::<_, i64>(0)?).unwrap(), row.get(1)?))
        })
        .unwrap()
}

fn resumable_files(
    dir: &TestDir,
    database: &rusqlite::Connection,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let id: String = database
        .query_row("SELECT blob_id FROM resumable_sessions", [], |row| {
            row.get(0)
        })
        .unwrap();
    (
        dir.0.join("staging").join(&id),
        dir.0.join("blobs").join(&id[..2]).join(&id),
    )
}

fn wait_for_boundary(message: &str, mut reached: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !reached() {
        assert!(Instant::now() < deadline, "{message}");
        thread::sleep(Duration::from_millis(1));
    }
}

fn send_resumable_chunk(
    server: &Server,
    path: &str,
    start: u64,
    length: u64,
    total: &str,
    byte: u8,
) -> TcpStream {
    let mut stream = server.stream();
    write!(stream, "PUT {path} HTTP/1.1\r\nHost: local\r\nContent-Length: {length}\r\nContent-Range: bytes {start}-{}/{total}\r\n\r\n", start + length - 1).unwrap();
    let buffer = [byte; 64 * 1024];
    let mut remaining = length;
    while remaining > 0 {
        let n = remaining.min(buffer.len() as u64) as usize;
        // Crash tests kill the server at a checkpoint, possibly before the body
        // ends; callers then assert the response or recovered state.
        if let Err(error) = stream.write_all(&buffer[..n]) {
            assert!(closed_by_server(&error), "{error}");
            break;
        }
        remaining -= n as u64;
    }
    stream
}

fn assert_resumable_complete(server: &Server, dir: &TestDir, resource: &Value) {
    assert_eq!(server.get("durable").json(), *resource);
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let (offset, state) = resumable_progress(&database);
    assert_eq!(state, "COMPLETE");
    assert_eq!(offset.to_string(), resource["size"].as_str().unwrap());
    let (objects, generation): (i64, i64) = database
        .query_row("SELECT count(*), max(generation) FROM objects", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(objects, 1);
    assert_eq!(
        generation.to_string(),
        resource["generation"].as_str().unwrap()
    );
    let last: i64 = database
        .query_row("SELECT last_generation FROM object_generation", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(last, generation);
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    let blobs: usize = std::fs::read_dir(dir.0.join("blobs"))
        .unwrap()
        .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum();
    assert_eq!(blobs, 1, "only the committed object blob remains");
}

#[test]
fn resumable_sigkill_before_offset_commit_truncates_unacknowledged_tail() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, 1024 * 1024, 5000);
    server.bucket();
    let path = resumable_path(&server);
    let prefix = 256 * 1024;
    assert_eq!(
        Reply::read(send_resumable_chunk(&server, &path, 0, prefix, "*", b'a')).status,
        308
    );
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let (stage, _) = resumable_files(&dir, &database);
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let sending = send_resumable_chunk(&server, &path, prefix, prefix, "*", b'x');
    wait_for_boundary(
        "chunk bytes must reach staging before the offset commit",
        || std::fs::metadata(&stage).unwrap().len() > prefix,
    );
    assert_eq!(resumable_progress(&database), (prefix, "ACTIVE".into()));
    server.child.kill().unwrap();
    assert!(!server.child.wait().unwrap().success());
    drop(sending);
    database.execute_batch("ROLLBACK").unwrap();
    drop(server);

    let server = Server::start(&dir, 1024 * 1024, 5000);
    assert_eq!(std::fs::metadata(&stage).unwrap().len(), prefix);
    let status = server.request("PUT", &path, "Content-Range: bytes */*\r\n", b"");
    assert_eq!(status.status, 308);
    assert_eq!(status.headers["range"], format!("bytes=0-{}", prefix - 1));
    let final_reply = Reply::read(send_resumable_chunk(
        &server,
        &path,
        prefix,
        prefix,
        &(prefix * 2).to_string(),
        b'b',
    ));
    assert_eq!(final_reply.status, 200);
    let mut expected = vec![b'a'; prefix as usize];
    expected.extend(vec![b'b'; prefix as usize]);
    assert_eq!(server.get("durable?alt=media").body, expected);
    assert_resumable_complete(&server, &dir, &final_reply.json());
}

#[test]
fn resumable_sigkill_after_chunk_acknowledgment_preserves_range_and_bytes() {
    for no_308 in [false, true] {
        let dir = TestDir::new();
        let mut server = Server::start(&dir, 1024 * 1024, 5000);
        server.bucket();
        let path = resumable_path(&server);
        let prefix = vec![b'a'; 256 * 1024];
        let headers = format!(
            "Content-Range: bytes 0-{}/*\r\n{}",
            prefix.len() - 1,
            if no_308 {
                "X-GUploader-No-308: yes\r\n"
            } else {
                ""
            }
        );
        let acknowledged = server.request("PUT", &path, &headers, &prefix);
        assert_eq!(acknowledged.status, if no_308 { 200 } else { 308 });
        if no_308 {
            assert_eq!(acknowledged.headers["x-http-status-code-override"], "308");
        }
        assert!(acknowledged.body.is_empty());
        server.child.kill().unwrap();
        assert!(!server.child.wait().unwrap().success());
        drop(server);

        let server = Server::start(&dir, 1024 * 1024, 5000);
        let status = server.request("PUT", &path, "Content-Range: bytes */*\r\n", b"");
        assert_eq!(status.status, 308);
        assert_eq!(status.headers["range"], acknowledged.headers["range"]);
        let total = prefix.len() + 4;
        let final_reply = server.request(
            "PUT",
            &path,
            &format!(
                "Content-Range: bytes {}-{}/{total}\r\n",
                prefix.len(),
                total - 1
            ),
            b"tail",
        );
        assert_eq!(final_reply.status, 200);
        let mut expected = prefix;
        expected.extend_from_slice(b"tail");
        assert_eq!(server.get("durable?alt=media").body, expected);
        assert_resumable_complete(&server, &dir, &final_reply.json());
    }
}

#[test]
fn resumable_sigkill_after_final_checkpoint_restores_active_staging() {
    assert_resumable_precommit_recovery(false);
}

#[test]
fn resumable_sigkill_after_final_rename_restores_staging_and_preserves_old_object() {
    assert_resumable_precommit_recovery(true);
}

fn assert_resumable_precommit_recovery(after_rename: bool) {
    let dir = TestDir::new();
    let prefix = 64 * 1024 * 1024;
    let total = prefix + 4;
    let mut server = Server::start(&dir, total, 5000);
    server.bucket();
    let original = server.upload("name=durable", "", b"old").json();
    let path = resumable_path(&server);
    assert_eq!(
        Reply::read(send_resumable_chunk(&server, &path, 0, prefix, "*", b'a')).status,
        308
    );
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let (stage, blob) = resumable_files(&dir, &database);
    let sending = send_resumable_chunk(&server, &path, prefix, 4, &total.to_string(), b'b');
    // Hashing a fixed disk prefix leaves time to acquire the lock after the final checkpoint.
    wait_for_boundary(
        "final chunk must checkpoint before hashing and publication",
        || resumable_progress(&database).0 == total,
    );
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(
        resumable_progress(&database),
        (total, "ACTIVE".into()),
        "missed the final checkpoint-to-publication lock window"
    );
    if after_rename {
        wait_for_boundary(
            "final blob rename must precede the blocked Object/COMPLETE commit",
            || blob.is_file(),
        );
        assert!(!stage.exists());
    }
    let generation: i64 = database
        .query_row(
            "SELECT generation FROM objects WHERE name='durable'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        generation.to_string(),
        original["generation"].as_str().unwrap()
    );
    server.child.kill().unwrap();
    assert!(!server.child.wait().unwrap().success());
    if !after_rename {
        assert!(
            stage.is_file() && !blob.exists(),
            "missed the checkpoint-before-rename crash window"
        );
    }
    drop(sending);
    database.execute_batch("ROLLBACK").unwrap();
    drop(server);

    let server = Server::start(&dir, total, 5000);
    assert_eq!(resumable_progress(&database), (total, "ACTIVE".into()));
    assert_eq!(std::fs::metadata(&stage).unwrap().len(), total);
    assert!(
        !blob.exists(),
        "uncommitted blob must be restored to staging"
    );
    assert_eq!(server.get("durable").json(), original);
    assert_eq!(server.get("durable?alt=media").body, b"old");
    let status = server.request("PUT", &path, "Content-Range: bytes */*\r\n", b"");
    assert_eq!(status.status, 308);
    assert_eq!(status.headers["range"], format!("bytes=0-{}", total - 1));
    let completed = Reply::read(send_resumable_chunk(
        &server,
        &path,
        prefix,
        4,
        &total.to_string(),
        b'b',
    ));
    assert_eq!(completed.status, 200);
    let resource = completed.json();
    assert_ne!(resource["generation"], original["generation"]);
    let bytes = server.get("durable?alt=media").body;
    assert_eq!(bytes.len() as u64, total);
    assert!(bytes[..prefix as usize].iter().all(|byte| *byte == b'a'));
    assert_eq!(&bytes[prefix as usize..], b"bbbb");
    let repeated = server.request("PUT", &path, "Content-Range: bytes */*\r\n", b"");
    assert_eq!(repeated.status, 200);
    assert_eq!(repeated.json(), resource);
    assert_resumable_complete(&server, &dir, &resource);
}

#[test]
fn resumable_sigkill_after_final_commit_preserves_object_and_completion_snapshot() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let path = resumable_path(&server);
    let headers = "Content-Range: bytes 0-4/5\r\nX-Goog-Meta-final: committed\r\n";
    let completed = server.request("PUT", &path, headers, b"exact");
    assert_eq!(completed.status, 200);
    let resource = completed.json();
    assert_eq!(
        resource["metadata"],
        json!({"source":"initiation", "final":"committed"})
    );
    server.child.kill().unwrap();
    assert!(!server.child.wait().unwrap().success());
    drop(server);

    let server = Server::start(&dir, 1000, 5000);
    assert_eq!(server.get("durable?alt=media").body, b"exact");
    let status = server.request("PUT", &path, "Content-Range: bytes */5\r\n", b"");
    assert_eq!(status.status, 200);
    assert!(!status.headers.contains_key("x-http-status-code-override"));
    assert_eq!(status.json(), resource);
    let repeated = server.request("PUT", &path, headers, b"exact");
    assert_eq!(repeated.status, 200);
    assert_eq!(repeated.json(), resource);
    assert_resumable_complete(&server, &dir, &resource);
}
