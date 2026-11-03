use crate::common::server::declare_server;
mod common;

use common::TestDir;
use common::server::{Reply, http_command, no_leaks};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

impl Server {
    fn start(dir: &TestDir, limit: u64, deadline: u64) -> Self {
        Self::launch(
            http_command(&dir.0)
                .args(["--api-base-url", "http://127.0.0.1:4443"])
                .args([
                    "--max-object-bytes",
                    &limit.to_string(),
                    "--object-request-timeout-ms",
                    &deadline.to_string(),
                ]),
        )
    }

    fn bucket(&self) {
        self.request(
            "POST",
            "/storage/v1/b?project=local",
            "",
            br#"{"name":"test-bucket"}"#,
        )
        .expect(200);
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        request(
            &self.address,
            method,
            path,
            "localhost",
            headers,
            body,
            self.client_timeout,
        )
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
        self.0.stop();
    }
}

fn socket(address: &str) -> TcpStream {
    socket_with_timeout(address, Duration::from_secs(5))
}

fn socket_with_timeout(address: &str, timeout: Duration) -> TcpStream {
    let stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(timeout)).unwrap();
    stream.set_write_timeout(Some(timeout)).unwrap();
    stream
}

fn request(
    address: &str,
    method: &str,
    path: &str,
    host: &str,
    headers: &str,
    body: &[u8],
    timeout: Duration,
) -> Reply {
    common::http::Client {
        address,
        host,
        headers: "",
        timeout,
    }
    .request(method, path, headers, body)
    .into_last()
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
    reply.expect(200);
    assert_eq!(
        reply.json_ok(),
        json!({"name":"fields","metadata":{"team":"selected"},"size":"4"})
    );
    assert_eq!(
        server.get("fields").json_ok()["metadata"]["other"],
        "stored"
    );
    let media = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/fields?alt=media",
        "",
        b"",
    );
    media.expect(200);
    assert_eq!(media.body, b"data");
    server.stop();
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
        .json_ok();
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
                format!("Invalid long value: '{}'.", value.replace("%2B", "+"))
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
                reply.json_reason(400, "invalidParameter");
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
                    .json_reason(400, "invalidParameter");
            }
            server
                .get(&format!("existing?alt=media&{query}"))
                .json_reason(400, "invalidParameter");
            assert_eq!(server.get("existing").json_ok(), original);
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
                .json_ok();
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
            while common::server::staging_count(&dir) != 1 {
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
                    .json_ok()
            } else {
                server
                    .upload(
                        "name=existing",
                        &wire(br#"{"metadata":{"key":"changed"}}"#, None, b"changed", None),
                    )
                    .json_ok()
            };
            stream.write_all(&body[body.len() - 1..]).unwrap();
            Reply::framed(stream).json_reason(412, "conditionNotMet");
            assert_eq!(server.get("existing").json_ok(), current);
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
                let resource = Reply::framed(stream).json_ok();
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
        .json_ok();
    for upload in ["media", "multipart"] {
        let path = format!("/upload/storage/v1/b/test-bucket/o?uploadType={upload}&name=existing");
        // A declared X-Goog-Hash is not examined on JSON uploads (recorded), so there is no
        // header-driven rejection to test here.
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
            .json_reason(413, "uploadTooLarge");
        assert_eq!(server.get("existing").json_ok(), original);
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
        Reply::framed(stream).json_reason(
            400,
            if upload == "media" {
                "badRequest"
            } else {
                "invalidParameter"
            },
        );
        assert_eq!(server.get("existing").json_ok(), original);
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
                Some("text/custom"),
                b"hello",
                None,
            ),
        )
        .json_ok();
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
    assert_eq!(object["acl"].as_array().unwrap().len(), 4);
    let object = common::without_acl(object);
    assert_eq!(server.get("dir%2Ffile").json_ok(), object);
    server.stop();
    let server = Server::start(&dir, 1_000_000, 30_000);
    assert_eq!(server.get("dir%2Ffile").json_ok(), object);
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
        .json_ok();
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
    let object = Reply::framed(stream).json_ok();
    // A media part without a type stores none (recorded); downloads default to octet-stream.
    assert!(object.get("contentType").is_none());
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
            let stored = part_type;
            let object = server
                .upload(
                    "name=fallback",
                    &wire(metadata.to_string().as_bytes(), part_type, b"hello", None),
                )
                .json_ok();
            assert_eq!(object.get("contentType").and_then(Value::as_str), stored);
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
        .json_ok();
    // Null entries are absent from the fresh metadata set; Unicode counts by UTF-8 bytes.
    let accepted = json!({"metadata":{"k":"猫".repeat(2730) + "x", "ignored":null}});
    let object = server
        .upload(
            "name=exact",
            &wire(accepted.to_string().as_bytes(), None, b"", None),
        )
        .json_ok();
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
        Reply::framed(stream).json_reason(400, "invalidArgument");
        assert_eq!(server.get("existing").json_ok(), original);
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
                .json_ok()["name"],
            "existing"
        );
    }
    let original = server.get("existing").json_ok();
    for name in [json!(7), json!(false), json!([]), json!({})] {
        let metadata = json!({"name":name});
        server
            .upload(
                "name=existing",
                &wire(metadata.to_string().as_bytes(), None, b"new", None),
            )
            .json_reason(400, "invalidArgument");
        assert_eq!(server.get("existing").json_ok(), original);
        assert_eq!(server.get("existing?alt=media").body, b"old");
    }
    server
        .upload("", &wire(br#"{"name":null}"#, None, b"new", None))
        .json_reason(400, "required");
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
            .json_ok();
        assert_eq!(value["contentType"], "text/plain");
        assert_eq!(server.get(&format!("{name}?alt=media")).body, payload);
    }
    server
        .upload("name=over", &wire(b"{}", None, b"123456", None))
        .json_reason(413, "uploadTooLarge");
    server.get("over").json_reason(404, "notFound");
    for length in [65536, 65537] {
        let original = server.get("json").body;
        let mut metadata = b"{}".to_vec();
        metadata.resize(length, b' ');
        let reply = server.upload("name=json", &wire(&metadata, None, b"", None));
        if length == 65536 {
            reply.json_ok();
        } else {
            reply.json_reason(413, "uploadTooLarge");
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
            reply.json_ok();
        } else {
            reply.json_reason(400, "invalidArgument");
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
        .json_ok();
    for field in [
        "acl",
        "contexts",
        "kmsKeyName",
        "customerEncryption",
        "generation",
        "metageneration",
        "size",
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
            .json_reason(400, "badRequest");
    }
    server
        .upload(
            "name=existing",
            &wire(br#"{"storageClass":"REGIONAL"}"#, None, b"new", None),
        )
        .json_reason(400, "badRequest");
    for (value, reason) in [
        (json!({"customTime":"invalid"}), "invalid"),
        (json!({"storageClass":"GLACIER"}), "invalid"),
        (json!({"storageClass":"coldline"}), "invalid"),
        (json!({"storageClass":""}), "invalid"),
        (json!({"storageClass":7}), "invalidArgument"),
    ] {
        server
            .upload(
                "name=existing",
                &wire(value.to_string().as_bytes(), None, b"new", None),
            )
            .json_reason(400, reason);
        assert_eq!(server.get("existing?alt=media").body, b"old");
    }
    for (metadata, reason) in [
        (b"{".as_slice(), "invalid"),
        (b"[]", "invalidArgument"),
        (br#"{"contentType":7}"#, "invalidArgument"),
        (br#"{"bucket":null}"#, "invalidArgument"),
        (br#"{"metadata":{"k":false}}"#, "invalidArgument"),
        (br#"{"contentLanguage":""}"#, "invalidArgument"),
    ] {
        server
            .upload("name=existing", &wire(metadata, None, b"new", None))
            .json_reason(400, reason);
    }
    server
        .upload("", &wire(b"{}", None, b"new", None))
        .json_reason(400, "required");
    server
        .upload("", &wire(br#"{"name":".."}"#, None, b"new", None))
        .json_reason(400, "invalidParameter");
    assert_eq!(server.get("existing").json_ok(), original);
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
            .json_ok();
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
            .json_ok();
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
            .json_reason(400, "invalid");
        assert_eq!(server.get("hash").json_ok(), original);
        server
            .upload(
                "name=hash",
                &wire(value.to_string().as_bytes(), None, b"hello", None),
            )
            .json_reason(400, "invalid");
        assert_eq!(server.get("hash").json_ok(), original);
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
            .json_reason(400, "invalidParameter");
        assert_eq!(server.get("hash").json_ok(), original);
        for bad in ["not base64", "AA==", "", "AAAAAAAAAAAAAAAAAAAAAB=="] {
            let value = json!({key:bad});
            server
                .upload(
                    "name=hash",
                    &wire(value.to_string().as_bytes(), None, b"hello", None),
                )
                .json_reason(400, "invalidParameter");
            assert_eq!(server.get("hash").json_ok(), original);
        }
    }
    for (hash, status, reason) in [
        ("md5=XUFAKrxLKna5cZ2REBfFkg==", 200, ""),
        // The header is not examined on a JSON upload (recorded).
        ("md5=AAAAAAAAAAAAAAAAAAAAAA==", 200, ""),
        ("crc32c=AAAAAA==", 200, ""),
    ] {
        let original = server.get("hash").json_ok();
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
            reply.json_ok();
        } else {
            reply.json_reason(status, reason);
            assert_eq!(server.get("hash").json_ok(), original);
        }
    }
    assert_eq!(server.get("hash?alt=media").body, b"hello");
    no_leaks(&dir, 1);
}

fn assert_preserved(server: &Server, dir: &TestDir, original: &Value) {
    assert_eq!(server.get("existing").json_ok(), *original);
    assert_eq!(server.get("existing?alt=media").body, b"old");
    no_leaks(dir, 1);
}

fn checksum_failures_preserve_existing_generation(
    cases: impl IntoIterator<Item = (Vec<u8>, &'static str)>,
) {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let original = server
        .upload(
            "name=existing",
            &wire(br#"{"metadata":{"key":"old"}}"#, None, b"old", None),
        )
        .json_ok();
    for (body, reason) in cases {
        server
            .upload("name=existing", &body)
            .json_reason(400, reason);
        assert_preserved(&server, &dir, &original);
    }
}

#[test]
fn obj_multipart_http_malformed_checksum_json_preserves_existing_generation() {
    checksum_failures_preserve_existing_generation(
        [b"{".as_slice(), br#"{"crc32c":"unterminated}"#].map(|checksums| {
            (
                wire(
                    br#"{"metadata":{"key":"new"}}"#,
                    None,
                    b"new",
                    Some(checksums),
                ),
                "parseError",
            )
        }),
    );
}

#[test]
fn obj_multipart_http_truncated_checksum_part_preserves_existing_generation() {
    let body = wire(br#"{"metadata":{"key":"new"}}"#, None, b"new", Some(b"{}"));
    let close = b"\r\n--boundary--\r\n";
    assert!(body.ends_with(close));
    checksum_failures_preserve_existing_generation([b"".as_slice(), b"\r\n--boundary\r\n"].map(
        |ending| {
            let mut truncated = body[..body.len() - close.len()].to_vec();
            truncated.extend_from_slice(ending);
            (truncated, "invalidParameter")
        },
    ));
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
        let reply = server.upload("name=bad", &body);
        // A request with only the metadata part is counted as one payload (recorded).
        if body.starts_with(b"--boundary\r\nContent-Type: application/json\r\n\r\n{}\r\n") {
            reply.json_reason(400, "invalidPayloadSize");
        } else {
            reply.json_reason(400, "invalidParameter");
        }
        server.get("bad").json_reason(404, "notFound");
        no_leaks(&dir, 0);
    }
    // A body that is not declared multipart is stored as a media upload (recorded), so only
    // multipart types with a broken boundary are refused here.
    for headers in [
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
            .plain_error(
                400,
                "text/plain; charset=utf-8",
                "Bad content type.  Please use multipart.",
            );
    }
    let mut oversized = socket(&server.address);
    oversized.write_all(b"POST /upload/storage/v1/b/missing-bucket/o?uploadType=multipart HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=boundary\r\nContent-Length: 999999\r\n\r\n").unwrap();
    Reply::framed(oversized).json_reason(413, "uploadTooLarge");
    for value in [br#"{"unknown":true}"#.as_slice(), br#"{"name":"ignored"}"#] {
        server
            .upload("name=bad", &wire(b"{}", None, b"hello", Some(value)))
            .json_reason(400, "invalid");
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
        .json_reason(400, "invalidParameter");
    for length in [65536, 65537] {
        let original = server.get("final").body;
        let mut value = b"{}".to_vec();
        value.resize(length, b' ');
        let reply = server.upload("name=final", &wire(b"{}", None, b"hello", Some(&value)));
        if length == 65536 {
            reply.json_ok();
        } else {
            reply.json_reason(413, "uploadTooLarge");
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
            reply.json_ok();
        } else {
            reply.json_reason(400, "invalidParameter");
            assert_eq!(server.get("preamble").body, original);
        }
        let original = server.get("epilogue").body;
        let mut body = valid.clone();
        body.extend(std::iter::repeat_n(b'e', length));
        let reply = server.upload("name=epilogue", &body);
        if length == 4096 {
            reply.json_ok();
        } else {
            reply.json_reason(400, "invalidParameter");
            assert_eq!(server.get("epilogue").body, original);
        }
    }
    no_leaks(&dir, 3);
}

#[test]
fn obj_multipart_http_unconditional_replay_and_python_advisory_mime() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let path = "/upload/storage/v1/b/test-bucket/o?uploadType=multipart";
    let headers = "Content-Type: multipart/related; boundary=boundary\r\nX-Goog-Gcs-Idempotency-Token: unconditioned\r\nX-Upload-Content-Type: text/advisory\r\n";
    let body = wire(
        br#"{"name":"python","contentType":"text/metadata"}"#,
        Some("text/metadata"),
        b"original",
        None,
    );
    let barrier = Arc::new(Barrier::new(2));
    let replies = thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let barrier = barrier.clone();
                let address = server.address.clone();
                let body = &body;
                scope.spawn(move || {
                    barrier.wait();
                    request(
                        &address,
                        "POST",
                        path,
                        "localhost",
                        headers,
                        body,
                        Duration::from_secs(5),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    let original = &replies[0];
    replies[1].expect(200);
    assert_eq!(replies[1].body, original.body);
    original.expect(200);
    assert_eq!(original.json_ok()["contentType"], "text/metadata");
    assert_eq!(
        server.request("POST", path, headers, &body).body,
        original.body
    );
    let changed = wire(
        br#"{"name":"python","contentType":"text/metadata"}"#,
        Some("text/metadata"),
        b"different",
        None,
    );
    server
        .request("POST", path, headers, &changed)
        .json_reason(400, "invalid");
    server.stop();
    let server = Server::start(&dir, 1000, 30_000);
    assert_eq!(
        server.request("POST", path, headers, &body).body,
        original.body
    );
    assert_eq!(server.get("python?alt=media").body, b"original");
    no_leaks(&dir, 1);
}

#[test]
fn obj_http_upload_tokens_with_generation_match_allow_one_create_and_overwrite() {
    for (upload, query_name) in [("media", true), ("multipart", false), ("multipart", true)] {
        let dir = TestDir::new();
        let server = token_server(&dir, upload);
        let request = |method: &str, path: &str, headers: &str, body: &[u8]| {
            let (host, forwarded) = if upload == "media" {
                ("untrusted.invalid", "X-Forwarded-Host: evil.invalid\r\n")
            } else {
                ("localhost", "")
            };
            request(
                &server.address,
                method,
                path,
                host,
                &format!("{forwarded}{headers}"),
                body,
                server.client_timeout,
            )
        };
        let content_type = if upload == "multipart" {
            "Content-Type: multipart/related; boundary=boundary\r\n"
        } else {
            ""
        };
        let headers =
            format!("{content_type}X-Goog-Gcs-Idempotency-Token: opaque, retry=token\r\n");
        let path = format!(
            "/upload/storage/v1/b/test-bucket/o?uploadType={upload}&{}ifGenerationMatch=",
            if query_name { "name=token&" } else { "" }
        );
        let body = |bytes: &[u8]| {
            if upload == "multipart" {
                wire(br#"{"name":"token"}"#, None, bytes, None)
            } else {
                bytes.to_vec()
            }
        };
        let original_body = body(b"original");
        let original = request("POST", &format!("{path}0"), &headers, &original_body).json_ok();
        assert_eq!(
            request("POST", &format!("{path}0"), &headers, &original_body).json_ok(),
            original
        );
        let object_path = "/storage/v1/b/test-bucket/o/token";
        assert_eq!(request("GET", object_path, "", b"").json_ok(), original);
        assert_eq!(
            request("GET", &format!("{object_path}?alt=media"), "", b"").body,
            b"original"
        );
        if upload == "multipart" {
            no_leaks(&dir, 1);
        }
        let overwrite = format!("{path}{}", original["generation"].as_str().unwrap());
        let body = body(b"replacement");
        let headers = format!("{content_type}X-Goog-Gcs-Idempotency-Token: overwrite-token\r\n");
        let replaced = request("POST", &overwrite, &headers, &body).json_ok();
        assert_ne!(replaced["generation"], original["generation"]);
        assert_eq!(
            request("POST", &overwrite, &headers, &body).json_ok(),
            replaced
        );
        assert_eq!(request("GET", object_path, "", b"").json_ok(), replaced);
        assert_eq!(
            request("GET", &format!("{object_path}?alt=media"), "", b"").body,
            b"replacement"
        );
        if upload == "media" {
            assert_eq!(common::server::staging_count(&dir), 0);
            assert_eq!(common::server::blob_count(&dir), 1);
        } else {
            no_leaks(&dir, 1);
        }
    }
}

#[test]
fn obj_http_upload_tokens_reject_empty_or_duplicate_headers_before_body() {
    for (upload, names, content_type, length) in [
        (
            "media",
            ["&name=absent", "&name=existing"].as_slice(),
            "",
            900,
        ),
        (
            "multipart",
            ["", "&name=absent", "&name=existing"].as_slice(),
            "Content-Type: multipart/related; boundary=boundary\r\n",
            9999,
        ),
    ] {
        let dir = TestDir::new();
        let server = token_server(&dir, upload);
        let body = if upload == "multipart" {
            wire(b"{}", None, b"original", None)
        } else {
            b"original".to_vec()
        };
        let original = server
            .request(
                "POST",
                &format!("/upload/storage/v1/b/test-bucket/o?uploadType={upload}&name=existing"),
                content_type,
                &body,
            )
            .json_ok();
        let token = "X-Goog-Gcs-Idempotency-Token: retry-token\r\n";
        for name in names {
            for headers in [
                "X-Goog-Gcs-Idempotency-Token:\r\n".to_owned(),
                format!("{token}{token}"),
            ] {
                for query in ["", "&ifGenerationMatch=0"] {
                    let mut stream = socket_with_timeout(&server.address, server.client_timeout);
                    write!(stream,"POST /upload/storage/v1/b/test-bucket/o?uploadType={upload}{name}{query} HTTP/1.1\r\nHost: local\r\n{content_type}Content-Length: {length}\r\n{headers}\r\n").unwrap();
                    Reply::framed(stream).json_error(400, "invalidParameter");
                }
            }
        }
        server.get("absent").json_error(404, "notFound");
        assert_eq!(server.get("existing").json_ok(), original);
        assert_eq!(server.get("existing?alt=media").body, b"original");
        if upload == "media" {
            assert_eq!(common::server::staging_count(&dir), 0);
        }
        no_leaks(&dir, 1);
    }
}

fn token_server(dir: &TestDir, upload: &str) -> Server {
    if upload == "multipart" {
        let server = Server::start(dir, 1000, 30_000);
        server.bucket();
        return server;
    }
    // These flags mirror object_http's server start for the media variant.
    let mut server = Server::launch(http_command(&dir.0).args([
        "--api-base-url",
        "https://advertised.invalid/api",
        "--max-object-bytes",
        "1000",
        "--connection-timeout-ms",
        "5000",
        "--object-request-timeout-ms",
        "900000",
        "--catalog-wait-timeout-ms",
        "30000",
        "--max-connections",
        "32",
    ]));
    server.client_timeout = Duration::from_secs(30);
    assert_eq!(server.ready["max_object_bytes"], 1000);
    assert_eq!(server.ready["object_request_timeout_ms"], 900_000);
    assert_eq!(server.ready["catalog_wait_timeout_ms"], 30_000);
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            "Content-Type: application/json\r\n",
            br#"{"name":"test-bucket","storageClass":"NEARLINE"}"#,
        )
        .expect(200);
    server
}

#[test]
fn obj_multipart_http_preconditions_reject_before_body_and_preserve_previous_generation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 30_000);
    server.bucket();
    let original = server
        .upload("name=existing", &wire(b"{}", None, b"old", None))
        .json_ok();
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
        Reply::framed(stream).json_reason(412, "conditionNotMet");
        server
            .upload(&query, &wire(br#"{"name":"existing"}"#, None, b"new", None))
            .json_reason(412, "conditionNotMet");
    }
    let mut stream = socket(&server.address);
    stream.write_all(b"POST /upload/storage/v1/b/missing-bucket/o?uploadType=multipart HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=boundary\r\nContent-Length: 9999\r\n\r\n").unwrap();
    Reply::framed(stream).json_reason(404, "notFound");
    assert_eq!(server.get("existing").json_ok(), original);
    let value = server.upload(&format!("name=existing&ifGenerationMatch={generation}&ifGenerationNotMatch=0&ifMetagenerationMatch=1&ifMetagenerationNotMatch=2"), &wire(b"{}",None,b"new",None)).json_ok();
    assert_ne!(value["generation"], original["generation"]);
    assert_eq!(value["metageneration"], "1");
    no_leaks(&dir, 1);
    let session = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=new",
        "",
        b"",
    );
    session.expect(200);
    assert!(session.body.is_empty());
    server.get("new").json_reason(404, "notFound");
    assert_eq!(common::server::staging_count(&dir), 1);
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
            Reply::framed(stream).status
        })
    }).collect();
    let deadline = Instant::now() + Duration::from_secs(3);
    while common::server::staging_count(&dir) != 4 {
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
        .json_ok();
    let body = wire(b"{}", None, b"hello", None);
    let prefix = &body[..body.len() - 16];
    let mut streams: Vec<_> = (0..8).map(|_| {
        let mut stream = socket(&server.address);
        write!(stream,"POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart&name=existing HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=boundary\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",prefix.len()).unwrap();
        stream.write_all(prefix).unwrap(); stream.write_all(b"\r\n").unwrap(); stream
    }).collect();
    let ready = Instant::now() + Duration::from_secs(3);
    while common::server::staging_count(&dir) != 8 {
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
        .json_reason(404, "notFound");
    let deadline = Instant::now() + Duration::from_millis(1300);
    while Instant::now() < deadline {
        for stream in &mut streams {
            let _ = stream.write_all(b"1\r\nx\r\n");
        }
        thread::sleep(Duration::from_millis(40));
    }
    for mut stream in streams {
        // Lingering close keeps the timeout response from being reset by the trickle.
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 408 "), "{text}");
        assert!(
            text.contains("\"reason\": \"uploadBrokenConnection\""),
            "{text}"
        );
    }
    no_leaks(&dir, 1);
    assert_eq!(server.get("existing").json_ok(), original);
    server
        .upload("name=existing", &wire(b"{}", None, b"new", None))
        .json_ok();
    no_leaks(&dir, 1);
}

/// gcloud storage 591.0.0 (apitools) frames small uploads with bare LFs and a single-quoted
/// boundary; this is its request byte for byte, apart from the bucket name.
#[test]
fn obj_multipart_http_accepts_the_gcloud_request_shape() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1_000_000, 30_000);
    server.bucket();
    let boundary = "===============5708193607524502531==";
    let body = format!(
        "--{boundary}\nContent-Type: application/json\nMIME-Version: 1.0\n\n{{\"bucket\": \"test-bucket\", \"contentType\": \"text/plain\", \"name\": \"hello.txt\"}}\n--{boundary}\nContent-Type: text/plain\nMIME-Version: 1.0\nContent-Transfer-Encoding: binary\n\nHello from gcloud!\n\n--{boundary}--\n"
    );
    let object = server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?alt=json&uploadType=multipart",
            &format!("Content-Type: multipart/related; boundary='{boundary}'\r\n"),
            body.as_bytes(),
        )
        .json_ok();
    assert_eq!(object["name"], "hello.txt");
    assert_eq!(object["contentType"], "text/plain");
    assert_eq!(object["size"], "19");
    assert_eq!(
        server.get("hello.txt?alt=media").body,
        b"Hello from gcloud!\n"
    );
    // A mismatched quote or a boundary the body does not use still fails before publishing.
    for content_type in [
        format!("multipart/related; boundary='{boundary}"),
        "multipart/related; boundary='other'".to_owned(),
    ] {
        let reply = server.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=multipart&name=rejected",
            &format!("Content-Type: {content_type}\r\n"),
            body.as_bytes(),
        );
        reply.expect(400);
    }
    server.get("rejected").expect(404);
    no_leaks(&dir, 1);
}

/// A CRLF-framed upload keeps CRLF delimiters throughout, so media that contains an LF
/// followed by boundary text is stored intact rather than cut at a bare-LF delimiter.
#[test]
fn obj_multipart_http_crlf_media_keeps_lf_boundary_text() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1_000_000, 30_000);
    server.bucket();
    for (name, media) in [
        ("closing", b"AAA\n--b--\nBBB".as_slice()),
        ("prefix", b"AAA\n--bX"),
    ] {
        let mut body = b"--b\r\nContent-Type: application/json\r\n\r\n{}\r\n--b\r\nContent-Type: application/octet-stream\r\n\r\n".to_vec();
        body.extend_from_slice(media);
        body.extend_from_slice(b"\r\n--b--\r\n");
        let object = server
            .request(
                "POST",
                &format!("/upload/storage/v1/b/test-bucket/o?uploadType=multipart&name={name}"),
                "Content-Type: multipart/related; boundary=b\r\n",
                &body,
            )
            .json_ok();
        assert_eq!(object["size"], media.len().to_string(), "{name}");
        assert_eq!(
            server.get(&format!("{name}?alt=media")).body,
            media,
            "{name}"
        );
    }
    no_leaks(&dir, 2);
}

declare_server!();
