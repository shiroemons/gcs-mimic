mod common;

use common::TestDir;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

struct Server {
    child: Child,
    address: String,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(dir: &TestDir, limit: u64, deadline: u64) -> Self {
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
            .args(["--api-base-url", "http://127.0.0.1:4443"])
            .args([
                "--max-object-bytes",
                &limit.to_string(),
                "--object-request-timeout-ms",
                &deadline.to_string(),
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
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

    fn bucket(&self) {
        assert_eq!(
            self.request(
                "POST",
                "/storage/v1/b?project=local",
                "",
                br#"{"name":"test-bucket"}"#
            )
            .status,
            200
        );
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        request(&self.address, method, path, headers, body)
    }

    fn upload(&self, query: &str, body: &[u8]) -> Reply {
        self.request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?uploadType=multipart&{query}"),
            "Content-Type: multipart/related; boundary=\"boundary\"\r\n",
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
    body: Vec<u8>,
    headers: BTreeMap<String, String>,
}

impl Reply {
    fn read(stream: TcpStream) -> Self {
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
            .is_some_and(|v| v == "chunked")
        {
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
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
            reader.read_to_end(&mut body).unwrap();
        }
        Self {
            status,
            headers,
            body,
        }
    }

    fn json(&self) -> Value {
        assert_eq!(self.status, 200, "{}", String::from_utf8_lossy(&self.body));
        serde_json::from_slice(&self.body).unwrap()
    }

    fn error(&self, status: u16, reason: &str) {
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

fn socket(address: &str) -> TcpStream {
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
}

fn request(address: &str, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
    let mut stream = socket(address);
    write!(stream,"{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",body.len()).unwrap();
    stream.write_all(body).unwrap();
    Reply::read(stream)
}

fn wire(
    metadata: &[u8],
    content_type: Option<&str>,
    media: &[u8],
    checksums: Option<&[u8]>,
) -> Vec<u8> {
    let mut body = b"--boundary\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n".to_vec();
    body.extend_from_slice(metadata);
    body.extend_from_slice(b"\r\n--boundary\r\n");
    if let Some(value) = content_type {
        body.extend_from_slice(format!("Content-Type: {value}\r\n").as_bytes());
    }
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(media);
    if let Some(checksums) = checksums {
        body.extend_from_slice(b"\r\n--boundary\r\nContent-Type: application/json\r\n\r\n");
        body.extend_from_slice(checksums);
    }
    body.extend_from_slice(b"\r\n--boundary--\r\n");
    body
}

#[test]
fn fields_multipart_response_preserves_uploaded_metadata_and_bytes() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let body = wire(
        br#"{"name":"fields","metadata":{"team":"selected","other":"stored"}}"#,
        Some("text/plain"),
        b"data",
        None,
    );
    let reply = server.upload("fields=name,metadata/team,size", &body);
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.json(),
        json!({"name":"fields","metadata":{"team":"selected"},"size":"4"})
    );
    assert_eq!(server.get("fields").json()["metadata"]["other"], "stored");
    let media = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/fields?alt=media",
        "",
        b"",
    );
    assert_eq!(media.status, 200);
    assert_eq!(media.body, b"data");
    server.stop();
}

fn no_leaks(dir: &TestDir, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let staging = std::fs::read_dir(dir.0.join("staging")).unwrap().count();
        let blobs: usize = std::fs::read_dir(dir.0.join("blobs"))
            .unwrap()
            .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
            .sum();
        if staging == 0 && blobs == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "staging={staging}, blobs={blobs}, expected={expected}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn obj_http_malformed_conditions_reject_every_object_method_without_mutation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let original = server
        .upload(
            "name=existing",
            &wire(br#"{"metadata":{"key":"old"}}"#, None, b"old", None),
        )
        .json();
    for key in [
        "ifGenerationMatch",
        "ifGenerationNotMatch",
        "ifMetagenerationMatch",
        "ifMetagenerationNotMatch",
    ] {
        for value in [
            "",
            "abc",
            "%2B1",
            "1.0",
            "9223372036854775808",
            "-9223372036854775809",
            &format!("1&{key}=1"),
        ] {
            let query = format!("{key}={value}");
            let message = if value.contains('&') {
                format!("Duplicate query parameter: {key}")
            } else {
                format!("{key} must be an int64")
            };
            for upload in ["media", "multipart", "resumable"] {
                let body = match upload {
                    "multipart" => wire(b"{}", None, b"new", None),
                    "resumable" => b"{}".to_vec(),
                    _ => b"new".to_vec(),
                };
                let headers = if upload == "multipart" {
                    "Content-Type: multipart/related; boundary=boundary\r\n"
                } else {
                    "Content-Type: application/json\r\n"
                };
                let reply = server.request(
                    "POST",
                    &format!("/upload/storage/v1/b/test-bucket/o?uploadType={upload}&name=existing&{query}"),
                    headers,
                    &body,
                );
                reply.error(400, "invalidParameter");
                let error: Value = serde_json::from_slice(&reply.body).unwrap();
                assert_eq!(error["error"]["message"], message);
            }
            for method in ["GET", "PATCH", "PUT", "DELETE"] {
                let body = if matches!(method, "PATCH" | "PUT") {
                    b"{}".as_slice()
                } else {
                    b""
                };
                server
                    .request(
                        method,
                        &format!("/storage/v1/b/test-bucket/o/existing?{query}"),
                        "",
                        body,
                    )
                    .error(400, "invalidParameter");
            }
            server
                .get(&format!("existing?alt=media&{query}"))
                .error(400, "invalidParameter");
            assert_eq!(server.get("existing").json(), original);
        }
    }
    assert_eq!(server.get("existing?alt=media").body, b"old");
    no_leaks(&dir, 1);
}

#[test]
fn obj_http_media_and_multipart_recheck_each_condition_after_streaming() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    // A future high-water mark avoids wall time; setup assumes no concurrent writer or cached allocator.
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE object_generation SET last_generation = 4000000000000000000",
            [],
        )
        .unwrap();
    for upload in ["media", "multipart"] {
        for key in [
            "ifGenerationMatch",
            "ifGenerationNotMatch",
            "ifMetagenerationMatch",
            "ifMetagenerationNotMatch",
        ] {
            let original = server
                .upload(
                    "name=existing",
                    &wire(br#"{"metadata":{"key":"old"}}"#, None, b"old", None),
                )
                .json();
            let generation = original["generation"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap();
            let condition = match key {
                "ifGenerationMatch" => generation,
                "ifGenerationNotMatch" => generation + 1,
                "ifMetagenerationMatch" => 1,
                _ => 2,
            };
            let body = if upload == "media" {
                b"replacement".to_vec()
            } else {
                wire(br#"{"metadata":{"key":"new"}}"#, None, b"replacement", None)
            };
            let mut stream = socket(&server.address);
            write!(stream, "POST /upload/storage/v1/b/test-bucket/o?uploadType={upload}&name=existing&{key}={condition} HTTP/1.1\r\nHost: local\r\nContent-Type: multipart/related; boundary=boundary\r\nContent-Length: {}\r\n\r\n", body.len()).unwrap();
            stream.write_all(&body[..body.len() - 1]).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while std::fs::read_dir(dir.0.join("staging")).unwrap().count() != 1 {
                assert!(
                    Instant::now() < deadline,
                    "upload must pass advisory checks"
                );
                thread::sleep(Duration::from_millis(5));
            }
            let current = if key.contains("Metageneration") {
                server
                    .request(
                        "PATCH",
                        "/storage/v1/b/test-bucket/o/existing",
                        "",
                        br#"{"metadata":{"key":"changed"}}"#,
                    )
                    .json()
            } else {
                server
                    .upload(
                        "name=existing",
                        &wire(br#"{"metadata":{"key":"changed"}}"#, None, b"changed", None),
                    )
                    .json()
            };
            stream.write_all(&body[body.len() - 1..]).unwrap();
            Reply::read(stream).error(412, "conditionNotMet");
            assert_eq!(server.get("existing").json(), current);
            assert_eq!(
                server.get("existing?alt=media").body,
                if key.contains("Metageneration") {
                    b"old".as_slice()
                } else {
                    b"changed"
                }
            );
            no_leaks(&dir, 1);
        }
    }
}

#[test]
fn obj_http_media_and_multipart_independent_checksum_vectors_across_chunks() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    // MD5 vectors are independently generated with OpenSSL; CRC32C uses RFC 3720 B.4.
    for (bytes, md5, crc32c) in [
        (b"".as_slice(), "1B2M2Y8AsgTpgAmY7PhCfg==", "AAAAAA=="),
        (b"123456789", "JfnnlDI7RTiF9RgfG2JNCw==", "4waSgw=="),
        (&[0; 32], "cLyPS3KoaSFGi/joRB3OUQ==", "ipE2qg=="),
    ] {
        for upload in ["media", "multipart"] {
            for supplied in [false, true] {
                let body = if upload == "media" {
                    bytes.to_vec()
                } else {
                    wire(b"{}", None, bytes, None)
                };
                let mut stream = socket(&server.address);
                let hashes = if supplied {
                    format!("X-Goog-Hash: md5={md5},crc32c={crc32c}\r\n")
                } else {
                    String::new()
                };
                write!(stream, "POST /upload/storage/v1/b/test-bucket/o?uploadType={upload}&name=vector HTTP/1.1\r\nHost: local\r\nContent-Type: multipart/related; boundary=boundary\r\n{hashes}Transfer-Encoding: chunked\r\n\r\n").unwrap();
                for chunk in body.chunks(7) {
                    write!(stream, "{:x}\r\n", chunk.len()).unwrap();
                    stream.write_all(chunk).unwrap();
                    stream.write_all(b"\r\n").unwrap();
                }
                stream.write_all(b"0\r\n\r\n").unwrap();
                let resource = Reply::read(stream).json();
                assert_eq!(resource["md5Hash"], md5);
                assert_eq!(resource["crc32c"], crc32c);
                assert_eq!(server.get("vector?alt=media").body, bytes);
                no_leaks(&dir, 1);
            }
        }
    }
}

#[test]
fn obj_http_failed_media_and_multipart_overwrites_preserve_full_resource_without_leaks() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 32, 30_000);
    server.bucket();
    let original = server
        .upload(
            "name=existing",
            &wire(
                br#"{"contentType":"text/old","cacheControl":"private","metadata":{"key":"old"}}"#,
                None,
                b"old",
                None,
            ),
        )
        .json();
    for upload in ["media", "multipart"] {
        let path = format!("/upload/storage/v1/b/test-bucket/o?uploadType={upload}&name=existing");
        for hash in ["md5=AAAAAAAAAAAAAAAAAAAAAA==", "crc32c=AAAAAA=="] {
            let body = if upload == "media" {
                b"new".to_vec()
            } else {
                wire(br#"{"metadata":{"key":"new"}}"#, None, b"new", None)
            };
            server.request("POST", &path, &format!("Content-Type: multipart/related; boundary=boundary\r\nX-Goog-Hash: {hash}\r\n"), &body).error(400, "invalid");
            assert_eq!(server.get("existing").json(), original);
            assert_eq!(server.get("existing?alt=media").body, b"old");
            no_leaks(&dir, 1);
        }
        let body = if upload == "media" {
            vec![b'x'; 33]
        } else {
            wire(b"{}", None, &[b'x'; 33], None)
        };
        server
            .request(
                "POST",
                &path,
                "Content-Type: multipart/related; boundary=boundary\r\n",
                &body,
            )
            .error(413, "uploadTooLarge");
        assert_eq!(server.get("existing").json(), original);
        assert_eq!(server.get("existing?alt=media").body, b"old");
        no_leaks(&dir, 1);
        let mut stream = socket(&server.address);
        write!(stream, "POST {path} HTTP/1.1\r\nHost: local\r\nContent-Type: multipart/related; boundary=boundary\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
        let body = if upload == "media" {
            b"new".to_vec()
        } else {
            wire(b"{}", None, b"new", None)
        };
        write!(stream, "{:x}\r\n", body.len()).unwrap();
        stream.write_all(&body).unwrap();
        stream.write_all(b"\r\ninvalid-chunk\r\n").unwrap();
        Reply::read(stream).error(
            400,
            if upload == "media" {
                "badRequest"
            } else {
                "invalidParameter"
            },
        );
        assert_eq!(server.get("existing").json(), original);
        assert_eq!(server.get("existing?alt=media").body, b"old");
        no_leaks(&dir, 1);
    }
}

#[test]
fn obj_multipart_http_round_trip_metadata_hashes_and_restart() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, 1_000_000, 30_000);
    server.bucket();
    let metadata = json!({"name":"ignored", "bucket":"different-bucket", "kind":"storage#object", "contentType":"text/custom", "contentEncoding":"identity", "contentDisposition":"inline", "contentLanguage":"en", "cacheControl":"private, no-transform", "metadata":{"猫":"値", "empty":""}, "md5Hash":"XUFAKrxLKna5cZ2REBfFkg==", "crc32c":"mnG7TA=="});
    let object = server
        .upload(
            "name=dir%2Ffile&projection=full",
            &wire(
                metadata.to_string().as_bytes(),
                Some("text/plain"),
                b"hello",
                None,
            ),
        )
        .json();
    assert_eq!(object["name"], "dir/file");
    assert_eq!(object["bucket"], "test-bucket");
    assert_eq!(object["metageneration"], "1");
    assert_eq!(object["size"], "5");
    for key in [
        "contentType",
        "contentEncoding",
        "contentDisposition",
        "contentLanguage",
        "cacheControl",
        "metadata",
        "md5Hash",
        "crc32c",
    ] {
        assert_eq!(object[key], metadata[key], "{key}");
    }
    assert_eq!(server.get("dir%2Ffile").json(), object);
    server.stop();
    let server = Server::start(&dir, 1_000_000, 30_000);
    assert_eq!(server.get("dir%2Ffile").json(), object);
    let media = server.get("dir%2Ffile?alt=media");
    assert_eq!(media.body, b"hello");
    assert_eq!(media.headers["content-type"], "text/custom");
    assert_eq!(media.headers["content-language"], "en");
    assert_eq!(media.headers["content-disposition"], "inline");
    no_leaks(&dir, 1);
}

#[test]
fn obj_multipart_http_streams_one_byte_chunks_and_boundary_like_binary() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1_000_000, 30_000);
    server.bucket();
    let payload = b"\x00\xff\r\n--boundar\r\n--\r\n--bound\r\nfinal\r\n";
    let reference = server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=reference",
            "",
            payload,
        )
        .json();
    let checksums = json!({"crc32c":reference["crc32c"]}).to_string();
    let body = wire(
        br#"{"name":"binary"}"#,
        None,
        payload,
        Some(checksums.as_bytes()),
    );
    let mut stream = socket(&server.address);
    write!(stream,"POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nContent-Type: multipart/related; boundary=boundary\r\n\r\n").unwrap();
    for byte in body {
        stream
            .write_all(&[b'1', b'\r', b'\n', byte, b'\r', b'\n'])
            .unwrap();
    }
    stream.write_all(b"0\r\n\r\n").unwrap();
    let object = Reply::read(stream).json();
    assert_eq!(object["contentType"], "application/octet-stream");
    assert_eq!(server.get("binary?alt=media").body, payload);
    no_leaks(&dir, 2);
}

#[test]
fn obj_multipart_http_null_and_empty_content_type_fall_back_to_media_part() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    for value in [Value::Null, json!("")] {
        let metadata = json!({"contentType":value});
        for part_type in [None, Some("text/plain")] {
            let expected = part_type.unwrap_or("application/octet-stream");
            let object = server
                .upload(
                    "name=fallback",
                    &wire(metadata.to_string().as_bytes(), part_type, b"hello", None),
                )
                .json();
            assert_eq!(object["contentType"], expected);
            let media = server.get("fallback?alt=media");
            assert_eq!(media.headers["content-type"], expected);
            assert_eq!(media.body, b"hello");
        }
    }
    no_leaks(&dir, 1);
}

#[test]
fn obj_multipart_http_custom_metadata_cap_rejects_before_media_or_staging() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 16 * 1024 * 1024, 30_000);
    server.bucket();
    let original = server
        .upload("name=existing", &wire(b"{}", None, b"old", None))
        .json();
    // Null entries are absent from the fresh metadata set; Unicode counts by UTF-8 bytes.
    let accepted = json!({"metadata":{"k":"猫".repeat(2730) + "x", "ignored":null}});
    let object = server
        .upload(
            "name=exact",
            &wire(accepted.to_string().as_bytes(), None, b"", None),
        )
        .json();
    assert!(object["metadata"].get("ignored").is_none());
    for query in ["name=existing", ""] {
        let metadata = json!({"name":"existing", "metadata":{"k":"猫".repeat(2731)}});
        let body = wire(metadata.to_string().as_bytes(), None, b"", None);
        let media_header = b"\r\n--boundary\r\n\r\n";
        let end = body
            .windows(media_header.len())
            .position(|v| v == media_header)
            .unwrap()
            + media_header.len();
        let mut stream = socket(&server.address);
        write!(stream,"POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart&{query} HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=boundary\r\nContent-Length: {}\r\n\r\n", 8 * 1024 * 1024).unwrap();
        stream.write_all(&body[..end]).unwrap();
        Reply::read(stream).error(400, "invalidArgument");
        assert_eq!(server.get("existing").json(), original);
        assert_eq!(server.get("existing?alt=media").body, b"old");
        no_leaks(&dir, 2);
    }
}

#[test]
fn obj_multipart_http_query_name_overrides_only_valid_metadata_names() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    for name in [Value::Null, json!("ignored")] {
        let metadata = json!({"name":name});
        assert_eq!(
            server
                .upload(
                    "name=existing",
                    &wire(metadata.to_string().as_bytes(), None, b"old", None)
                )
                .json()["name"],
            "existing"
        );
    }
    let original = server.get("existing").json();
    for name in [json!(7), json!(false), json!([]), json!({})] {
        let metadata = json!({"name":name});
        server
            .upload(
                "name=existing",
                &wire(metadata.to_string().as_bytes(), None, b"new", None),
            )
            .error(400, "invalidArgument");
        assert_eq!(server.get("existing").json(), original);
        assert_eq!(server.get("existing?alt=media").body, b"old");
    }
    server
        .upload("", &wire(br#"{"name":null}"#, None, b"new", None))
        .error(400, "required");
    no_leaks(&dir, 1);
}

#[test]
fn obj_multipart_http_media_json_and_custom_metadata_caps() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 5, 30_000);
    server.bucket();
    for (name, payload) in [("empty", b"".as_slice()), ("exact", b"12345")] {
        let value = server
            .upload(
                &format!("name={name}"),
                &wire(b"{}", Some("text/plain"), payload, None),
            )
            .json();
        assert_eq!(value["contentType"], "text/plain");
        assert_eq!(server.get(&format!("{name}?alt=media")).body, payload);
    }
    server
        .upload("name=over", &wire(b"{}", None, b"123456", None))
        .error(413, "uploadTooLarge");
    server.get("over").error(404, "notFound");
    for length in [65536, 65537] {
        let original = server.get("json").body;
        let mut metadata = b"{}".to_vec();
        metadata.resize(length, b' ');
        let reply = server.upload("name=json", &wire(&metadata, None, b"", None));
        if length == 65536 {
            reply.json();
        } else {
            reply.error(413, "uploadTooLarge");
            assert_eq!(server.get("json").body, original);
        }
    }
    for length in [8191, 8192] {
        let original = server.get("custom").body;
        let metadata = json!({"metadata":{"k":"v".repeat(length)}});
        let reply = server.upload(
            "name=custom",
            &wire(metadata.to_string().as_bytes(), None, b"", None),
        );
        if length == 8191 {
            reply.json();
        } else {
            reply.error(400, "invalidArgument");
            assert_eq!(server.get("custom").body, original);
        }
    }
    no_leaks(&dir, 4);
}

#[test]
fn obj_multipart_http_rejects_bad_metadata_and_unsupported_fields_without_mutation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let original = server
        .upload("name=existing", &wire(b"{}", None, b"old", None))
        .json();
    for field in [
        "acl",
        "temporaryHold",
        "retention",
        "contexts",
        "customTime",
        "kmsKeyName",
        "customerEncryption",
        "unknown",
        "generation",
        "metageneration",
        "size",
        "storageClass",
        "etag",
        "timeCreated",
        "id",
        "selfLink",
        "owner",
    ] {
        let value = json!({field:null});
        server
            .upload(
                "name=existing",
                &wire(value.to_string().as_bytes(), None, b"new", None),
            )
            .error(501, "notImplemented");
    }
    server
        .upload(
            "name=existing",
            &wire(br#"{"eventBasedHold":null}"#, None, b"new", None),
        )
        .error(400, "invalidArgument");
    for (metadata, reason) in [
        (b"{".as_slice(), "parseError"),
        (b"[]", "invalidArgument"),
        (br#"{"contentType":7}"#, "invalidArgument"),
        (br#"{"bucket":null}"#, "invalidArgument"),
        (br#"{"metadata":{"k":false}}"#, "invalidArgument"),
        (br#"{"contentLanguage":""}"#, "invalidArgument"),
    ] {
        server
            .upload("name=existing", &wire(metadata, None, b"new", None))
            .error(400, reason);
    }
    server
        .upload("", &wire(b"{}", None, b"new", None))
        .error(400, "required");
    server
        .upload("", &wire(br#"{"name":".."}"#, None, b"new", None))
        .error(400, "invalidParameter");
    assert_eq!(server.get("existing").json(), original);
    assert_eq!(server.get("existing?alt=media").body, b"old");
    no_leaks(&dir, 1);
}

#[test]
fn obj_multipart_http_checks_metadata_header_and_final_part_hashes_before_publication() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let good = json!({"md5Hash":"XUFAKrxLKna5cZ2REBfFkg==", "crc32c":"mnG7TA=="});
    for key in ["md5Hash", "crc32c"] {
        let value = json!({key:good[key]});
        server
            .upload(
                "name=hash",
                &wire(b"{}", None, b"hello", Some(value.to_string().as_bytes())),
            )
            .json();
        let original = server
            .upload(
                "name=hash",
                &wire(
                    value.to_string().as_bytes(),
                    None,
                    b"hello",
                    Some(value.to_string().as_bytes()),
                ),
            )
            .json();
        let wrong = if key == "md5Hash" {
            "AAAAAAAAAAAAAAAAAAAAAA=="
        } else {
            "AAAAAA=="
        };
        let value = json!({key:wrong});
        server
            .upload(
                "name=hash",
                &wire(b"{}", None, b"hello", Some(value.to_string().as_bytes())),
            )
            .error(400, "invalid");
        assert_eq!(server.get("hash").json(), original);
        server
            .upload(
                "name=hash",
                &wire(value.to_string().as_bytes(), None, b"hello", None),
            )
            .error(400, "invalid");
        assert_eq!(server.get("hash").json(), original);
        server
            .upload(
                "name=hash",
                &wire(
                    good.to_string().as_bytes(),
                    None,
                    b"hello",
                    Some(value.to_string().as_bytes()),
                ),
            )
            .error(400, "invalidParameter");
        assert_eq!(server.get("hash").json(), original);
        for bad in ["not base64", "AA==", "", "AAAAAAAAAAAAAAAAAAAAAB=="] {
            let value = json!({key:bad});
            server
                .upload(
                    "name=hash",
                    &wire(value.to_string().as_bytes(), None, b"hello", None),
                )
                .error(400, "invalidParameter");
            assert_eq!(server.get("hash").json(), original);
        }
    }
    for (hash, status, reason) in [
        ("md5=XUFAKrxLKna5cZ2REBfFkg==", 200, ""),
        ("md5=AAAAAAAAAAAAAAAAAAAAAA==", 400, "invalidParameter"),
        ("crc32c=AAAAAA==", 400, "invalidParameter"),
    ] {
        let original = server.get("hash").json();
        let body = wire(good.to_string().as_bytes(), None, b"hello", None);
        let reply = server.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=multipart&name=hash",
            &format!(
                "Content-Type: multipart/related; boundary=boundary\r\nX-Goog-Hash: {hash}\r\n"
            ),
            &body,
        );
        if status == 200 {
            reply.json();
        } else {
            reply.error(status, reason);
            assert_eq!(server.get("hash").json(), original);
        }
    }
    assert_eq!(server.get("hash?alt=media").body, b"hello");
    no_leaks(&dir, 1);
}

#[test]
fn obj_multipart_http_malformed_checksum_json_preserves_existing_generation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let original = server
        .upload(
            "name=existing",
            &wire(br#"{"metadata":{"key":"old"}}"#, None, b"old", None),
        )
        .json();
    for checksums in [b"{".as_slice(), br#"{"crc32c":"unterminated}"#] {
        server
            .upload(
                "name=existing",
                &wire(
                    br#"{"metadata":{"key":"new"}}"#,
                    None,
                    b"new",
                    Some(checksums),
                ),
            )
            .error(400, "parseError");
        assert_eq!(server.get("existing").json(), original);
        assert_eq!(server.get("existing?alt=media").body, b"old");
        no_leaks(&dir, 1);
    }
}

#[test]
fn obj_multipart_http_truncated_checksum_part_preserves_existing_generation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let original = server
        .upload(
            "name=existing",
            &wire(br#"{"metadata":{"key":"old"}}"#, None, b"old", None),
        )
        .json();
    let body = wire(br#"{"metadata":{"key":"new"}}"#, None, b"new", Some(b"{}"));
    let close = b"\r\n--boundary--\r\n";
    assert!(body.ends_with(close));
    for ending in [b"".as_slice(), b"\r\n--boundary\r\n"] {
        let mut truncated = body[..body.len() - close.len()].to_vec();
        truncated.extend_from_slice(ending);
        server
            .upload("name=existing", &truncated)
            .error(400, "invalidParameter");
        assert_eq!(server.get("existing").json(), original);
        assert_eq!(server.get("existing?alt=media").body, b"old");
        no_leaks(&dir, 1);
    }
}

#[test]
fn obj_multipart_http_rejects_malformed_framing_and_bounds_final_json() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let valid = wire(b"{}", None, b"hello", None);
    let missing_close = valid[..valid.len() - 16].to_vec();
    let four_parts = wire(
        b"{}",
        None,
        b"hello",
        Some(b"{}\r\n--boundary\r\nContent-Type: application/json\r\n\r\n{}"),
    );
    for body in [
        Vec::new(),
        b"--boundary--\r\n".to_vec(),
        b"--boundary\r\nContent-Type: application/json\r\n\r\n{}\r\n--boundary--".to_vec(),
        b"--boundary\r\nContent-Type: text/plain\r\n\r\n{}\r\n--boundary--".to_vec(),
        b"--boundary\r\ninvalid header\r\n\r\n{}\r\n--boundary--".to_vec(),
        missing_close,
        four_parts,
    ] {
        server
            .upload("name=bad", &body)
            .error(400, "invalidParameter");
        server.get("bad").error(404, "notFound");
        no_leaks(&dir, 0);
    }
    for headers in [
        "",
        "Content-Type: application/json\r\n",
        "Content-Type: multipart/related\r\n",
        "Content-Type: multipart/related; boundary=\"\"\r\n",
        "Content-Type: multipart/related; boundary=\"bad \"\r\n",
    ] {
        server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=multipart&name=bad",
                headers,
                &valid,
            )
            .error(400, "invalidParameter");
    }
    let mut oversized = socket(&server.address);
    oversized.write_all(b"POST /upload/storage/v1/b/missing-bucket/o?uploadType=multipart HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=boundary\r\nContent-Length: 999999\r\n\r\n").unwrap();
    Reply::read(oversized).error(413, "uploadTooLarge");
    for value in [br#"{"unknown":true}"#.as_slice(), br#"{"name":"ignored"}"#] {
        server
            .upload("name=bad", &wire(b"{}", None, b"hello", Some(value)))
            .error(400, "invalid");
    }
    let wrong_type = String::from_utf8(wire(b"{}", None, b"hello", Some(b"{}")))
        .unwrap()
        .replacen(
            "Content-Type: application/json\r\n",
            "Content-Type: text/plain\r\n",
            1,
        );
    server
        .upload("name=bad", wrong_type.as_bytes())
        .error(400, "invalidParameter");
    for length in [65536, 65537] {
        let original = server.get("final").body;
        let mut value = b"{}".to_vec();
        value.resize(length, b' ');
        let reply = server.upload("name=final", &wire(b"{}", None, b"hello", Some(&value)));
        if length == 65536 {
            reply.json();
        } else {
            reply.error(413, "uploadTooLarge");
            assert_eq!(server.get("final").body, original);
        }
    }
    for length in [4096, 4097] {
        let original = server.get("preamble").body;
        let mut body = vec![b'p'; length];
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(&valid);
        let reply = server.upload("name=preamble", &body);
        if length == 4096 {
            reply.json();
        } else {
            reply.error(400, "invalidParameter");
            assert_eq!(server.get("preamble").body, original);
        }
        let original = server.get("epilogue").body;
        let mut body = valid.clone();
        body.extend(std::iter::repeat_n(b'e', length));
        let reply = server.upload("name=epilogue", &body);
        if length == 4096 {
            reply.json();
        } else {
            reply.error(400, "invalidParameter");
            assert_eq!(server.get("epilogue").body, original);
        }
    }
    no_leaks(&dir, 3);
}

#[test]
fn obj_multipart_http_upload_tokens_with_generation_match_allow_one_create_and_overwrite() {
    for query_name in [false, true] {
        let dir = TestDir::new();
        let server = Server::start(&dir, 1000, 30_000);
        server.bucket();
        let headers = "Content-Type: multipart/related; boundary=boundary\r\nX-Goog-Gcs-Idempotency-Token: opaque, retry=token\r\n";
        let path = format!(
            "/upload/storage/v1/b/test-bucket/o?uploadType=multipart&{}ifGenerationMatch=",
            if query_name { "name=token&" } else { "" }
        );
        let body = wire(br#"{"name":"token"}"#, None, b"original", None);
        let original = server
            .request("POST", &format!("{path}0"), headers, &body)
            .json();
        server
            .request("POST", &format!("{path}0"), headers, &body)
            .error(412, "conditionNotMet");
        assert_eq!(server.get("token").json(), original);
        assert_eq!(server.get("token?alt=media").body, b"original");
        no_leaks(&dir, 1);
        let overwrite = format!("{path}{}", original["generation"].as_str().unwrap());
        let body = wire(br#"{"name":"token"}"#, None, b"replacement", None);
        let replaced = server.request("POST", &overwrite, headers, &body).json();
        assert_ne!(replaced["generation"], original["generation"]);
        server
            .request("POST", &overwrite, headers, &body)
            .error(412, "conditionNotMet");
        assert_eq!(server.get("token").json(), replaced);
        assert_eq!(server.get("token?alt=media").body, b"replacement");
        no_leaks(&dir, 1);
    }
}

#[test]
fn obj_multipart_http_upload_tokens_reject_unsafe_or_invalid_headers_before_body() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let original = server
        .upload("name=existing", &wire(b"{}", None, b"original", None))
        .json();
    let token = "X-Goog-Gcs-Idempotency-Token: retry-token\r\n";
    for name in ["", "&name=absent", "&name=existing"] {
        for query in [
            "",
            "&ifGenerationNotMatch=0",
            "&ifMetagenerationMatch=1",
            "&ifMetagenerationNotMatch=2",
        ] {
            let mut stream = socket(&server.address);
            write!(stream,"POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart{name}{query} HTTP/1.1\r\nHost: local\r\nContent-Type: multipart/related; boundary=boundary\r\nContent-Length: 9999\r\n{token}\r\n").unwrap();
            Reply::read(stream).error(501, "notImplemented");
        }
        for headers in [
            "X-Goog-Gcs-Idempotency-Token:\r\n".to_owned(),
            format!("{token}{token}"),
        ] {
            for query in ["", "&ifGenerationMatch=0"] {
                let mut stream = socket(&server.address);
                write!(stream,"POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart{name}{query} HTTP/1.1\r\nHost: local\r\nContent-Type: multipart/related; boundary=boundary\r\nContent-Length: 9999\r\n{headers}\r\n").unwrap();
                Reply::read(stream).error(400, "invalidParameter");
            }
        }
    }
    server.get("absent").error(404, "notFound");
    assert_eq!(server.get("existing").json(), original);
    assert_eq!(server.get("existing?alt=media").body, b"original");
    no_leaks(&dir, 1);
}

#[test]
fn obj_multipart_http_preconditions_reject_before_body_and_preserve_previous_generation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let original = server
        .upload("name=existing", &wire(b"{}", None, b"old", None))
        .json();
    let generation = original["generation"].as_str().unwrap();
    for query in [
        "ifGenerationMatch=0".to_owned(),
        "ifGenerationMatch=1".to_owned(),
        format!("ifGenerationNotMatch={generation}"),
        "ifMetagenerationMatch=2".to_owned(),
        "ifMetagenerationNotMatch=1".to_owned(),
    ] {
        let mut stream = socket(&server.address);
        write!(stream,"POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart&name=existing&{query} HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=boundary\r\nContent-Length: 9999\r\n\r\n").unwrap();
        Reply::read(stream).error(412, "conditionNotMet");
        server
            .upload(&query, &wire(br#"{"name":"existing"}"#, None, b"new", None))
            .error(412, "conditionNotMet");
    }
    let mut stream = socket(&server.address);
    stream.write_all(b"POST /upload/storage/v1/b/missing-bucket/o?uploadType=multipart HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=boundary\r\nContent-Length: 9999\r\n\r\n").unwrap();
    Reply::read(stream).error(404, "notFound");
    assert_eq!(server.get("existing").json(), original);
    let value = server.upload(&format!("name=existing&ifGenerationMatch={generation}&ifGenerationNotMatch=0&ifMetagenerationMatch=1&ifMetagenerationNotMatch=2"), &wire(b"{}",None,b"new",None)).json();
    assert_ne!(value["generation"], original["generation"]);
    assert_eq!(value["metageneration"], "1");
    no_leaks(&dir, 1);
    let session = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=new",
        "",
        b"",
    );
    assert_eq!(session.status, 200);
    assert!(session.body.is_empty());
    server.get("new").error(404, "notFound");
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 1);
}

#[test]
fn obj_multipart_http_concurrent_create_if_absent_has_one_winner_and_no_leaks() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let barrier = Arc::new(Barrier::new(5));
    let handles: Vec<_> = (0..4).map(|_| {
        let address = server.address.clone(); let barrier = barrier.clone();
        thread::spawn(move || {
            let body = wire(br#"{"name":"winner","metadata":{"key":"value"}}"#,None,b"hello",None);
            let mut stream = socket(&address);
            write!(stream,"POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart&ifGenerationMatch=0 HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=boundary\r\nContent-Length: {}\r\n\r\n",body.len()).unwrap();
            stream.write_all(&body[..body.len()-16]).unwrap();
            barrier.wait();
            stream.write_all(&body[body.len()-16..]).unwrap();
            Reply::read(stream).status
        })
    }).collect();
    let deadline = Instant::now() + Duration::from_secs(3);
    while std::fs::read_dir(dir.0.join("staging")).unwrap().count() != 4 {
        assert!(
            Instant::now() < deadline,
            "all uploads must pass advisory checks before any commit"
        );
        thread::sleep(Duration::from_millis(5));
    }
    barrier.wait();
    let mut statuses: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 412, 412, 412]);
    assert_eq!(server.get("winner?alt=media").body, b"hello");
    no_leaks(&dir, 1);
}

#[test]
fn obj_multipart_http_deadline_cancels_admitted_bodies_and_releases_all_slots() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 1200);
    server.bucket();
    let original = server
        .upload("name=existing", &wire(b"{}", None, b"old", None))
        .json();
    let body = wire(b"{}", None, b"hello", None);
    let prefix = &body[..body.len() - 16];
    let mut streams: Vec<_> = (0..8).map(|_| {
        let mut stream = socket(&server.address);
        write!(stream,"POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart&name=existing HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=boundary\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",prefix.len()).unwrap();
        stream.write_all(prefix).unwrap(); stream.write_all(b"\r\n").unwrap(); stream
    }).collect();
    let ready = Instant::now() + Duration::from_secs(3);
    while std::fs::read_dir(dir.0.join("staging")).unwrap().count() != 8 {
        assert!(Instant::now() < ready, "all uploads must enter staging");
        thread::sleep(Duration::from_millis(5));
    }
    server
        .request(
            "POST",
            "/upload/storage/v1/b/missing-bucket/o?uploadType=multipart&name=missing",
            "Content-Type: multipart/related; boundary=boundary\r\n",
            &wire(b"{}", None, b"additional", None),
        )
        .error(404, "notFound");
    let deadline = Instant::now() + Duration::from_millis(1300);
    while Instant::now() < deadline {
        for stream in &mut streams {
            let _ = stream.write_all(b"1\r\nx\r\n");
        }
        thread::sleep(Duration::from_millis(40));
    }
    for mut stream in streams {
        let mut response = Vec::new();
        match stream.read_to_end(&mut response) {
            Ok(_) => {}
            // Closing an incomplete request can reset TCP if a trickle arrives after FIN;
            // Windows reports the same close as an aborted connection.
            Err(error) => assert!(common::closed_by_server(&error), "{error}"),
        }
        assert!(response.is_empty());
    }
    no_leaks(&dir, 1);
    assert_eq!(server.get("existing").json(), original);
    server
        .upload("name=existing", &wire(b"{}", None, b"new", None))
        .json();
    no_leaks(&dir, 1);
}
