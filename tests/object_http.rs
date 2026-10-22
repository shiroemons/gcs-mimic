mod common;

use common::TestDir;
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
        let mut child = common::server_command()
            .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
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
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
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
    for header in [
        "If-Match: opaque\r\n",
        "If-None-Match: *\r\n",
        "X-Goog-Encryption-Key: key\r\n",
        "X-Goog-Meta-Example: value\r\n",
    ] {
        server
            .upload("name=existing", header, b"new")
            .error(501, "notImplemented");
        server.raw(format!("POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=existing HTTP/1.1\r\nHost: local\r\nContent-Length: 900\r\n{header}\r\n").as_bytes()).error(501, "notImplemented");
        for method in ["GET", "DELETE"] {
            server
                .request(method, "/storage/v1/b/test-bucket/o/existing", header, b"")
                .error(501, "notImplemented");
        }
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/existing?alt=media",
                header,
                b"",
            )
            .error(501, "notImplemented");
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
        .error(501, "notImplemented");
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
    let server = Server::start(&dir, 1000, 300);
    server.bucket();
    let mut stream = server.stream();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=slow HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
    for _ in 0..6 {
        stream.write_all(b"1\r\na\r\n").unwrap();
        thread::sleep(Duration::from_millis(80));
    }
    stream.write_all(b"0\r\n\r\n").unwrap();
    assert_eq!(Reply::read(stream).status, 200);
    assert_eq!(server.get("slow?alt=media").body, b"aaaaaa");
    let mut stream = server.stream();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=idle HTTP/1.1\r\nHost: local\r\nContent-Length: 100\r\n\r\nx").unwrap();
    let start = Instant::now();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert!(start.elapsed() >= Duration::from_millis(200));
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
            stream.write_all(b"1\r\na\r\n").unwrap();
        }
    }
    for mut stream in uploads {
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
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
    assert_eq!(server.upload("name=after-deadline", "", b"ok").status, 200);
}

#[test]
fn obj_http_trickle_download_stops_at_absolute_deadline() {
    let dir = TestDir::new();
    let size = 32 * 1024 * 1024;
    let mut server = Server::start(&dir, size as u64, 5000);
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
    for _ in 0..20 {
        received += reader.read(&mut chunk).unwrap();
        thread::sleep(Duration::from_millis(80));
    }
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    received += rest.len();
    assert!(
        received > 0 && received < size,
        "truncated media: {received}/{size}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(2800),
        "deadline precedes inactivity"
    );
    assert_eq!(server.get("large").json()["size"], size.to_string());
}

#[test]
fn obj_http_catalog_wait_deadlines_return_backend_error_and_commit_can_finish() {
    let dir = TestDir::new();
    let server = Server::with_timeouts(&dir, 1000, 100, 5000, 600);
    server.bucket();
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = Instant::now();
    let mut first = server.stream();
    first.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=committing HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\none").unwrap();
    let published = Instant::now() + Duration::from_secs(2);
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
    thread::sleep(Duration::from_millis(100));
    let mut second = server.stream();
    second.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=queued HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\ntwo").unwrap();
    Reply::read(first).error(503, "backendError");
    Reply::read(second).error(503, "backendError");
    assert!(started.elapsed() >= Duration::from_millis(500));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "both waits bounded before lock release"
    );
    database.execute_batch("ROLLBACK").unwrap();
    // GET response startup still has a 100 ms deadline; let SQLite's busy retry finish first.
    let committed = Instant::now() + Duration::from_secs(2);
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
    let server = Server::start(&dir, 1000, 200);
    server.bucket();
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut stream = server.stream();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=committed&ifGenerationMatch=0 HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\nnew").unwrap();
    thread::sleep(Duration::from_millis(700));
    database.execute_batch("ROLLBACK").unwrap();
    assert_eq!(Reply::read(stream).status, 200);
    assert_eq!(server.get("committed?alt=media").body, b"new");
}

#[test]
fn obj_http_queued_precheck_waits_past_body_inactivity_deadline() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 200);
    server.bucket();
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut first = server.stream();
    first.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=first HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\none").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
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
    thread::sleep(Duration::from_millis(100));
    let mut second = server.stream();
    second.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=second HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\ntwo").unwrap();
    let waiting = Instant::now();
    thread::sleep(Duration::from_millis(700));
    database.execute_batch("ROLLBACK").unwrap();
    assert_eq!(Reply::read(second).status, 200);
    assert!(waiting.elapsed() >= Duration::from_millis(700));
    assert_eq!(Reply::read(first).status, 200);
    assert_eq!(server.get("first?alt=media").body, b"one");
    assert_eq!(server.get("second?alt=media").body, b"two");
}

#[test]
fn obj_http_upload_limit_precedes_advisory_check_without_catalog_wait() {
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
        assert!(Instant::now() < deadline, "all eight upload slots admitted");
        thread::sleep(Duration::from_millis(10));
    }
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    uploads[0].write_all(b"one").unwrap();
    thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    server
        .request(
            "POST",
            "/upload/storage/v1/b/missing-bucket/o?uploadType=media&name=excess",
            "",
            b"two",
        )
        .error(503, "backendError");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "excess upload must not queue on the catalog"
    );
    database.execute_batch("ROLLBACK").unwrap();
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
