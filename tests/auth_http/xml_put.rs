use super::*;
use std::{
    net::Shutdown,
    sync::{Arc, Barrier},
};

const OBJECT: &str = "/iam-bucket/object";
const JSON_OBJECT: &str = "/storage/v1/b/iam-bucket/o/object";
const MD5: &str = "XrY7u+Ae7tCTyyK7j1rNww==";
const CRC: &str = "yZRlqg==";

fn xml_error(reply: &Reply, status: u16, code: &str) {
    assert_eq!(
        reply.status,
        status,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(
        reply.headers["content-type"],
        "application/xml; charset=UTF-8"
    );
    assert!(
        String::from_utf8_lossy(&reply.body).contains(&format!("<Code>{code}</Code>")),
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
}

fn fixture() -> (TestDir, TestDir, Server) {
    let (config, data, server) = iam::fixture();
    server
        .request(
            "PUT",
            OBJECT,
            &format!("{ADMIN}Content-Type: text/plain\r\nx-goog-meta-original: keep\r\n"),
            b"hello world",
        )
        .ok();
    (config, data, server)
}

fn snapshot(server: &Server, data: &TestDir) -> (Value, Vec<u8>, i64, i64) {
    let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    (
        server.get(ADMIN, JSON_OBJECT).ok().json(),
        server.get(ADMIN, OBJECT).ok().body.clone(),
        connection
            .query_row("SELECT last_generation FROM object_generation", [], |row| {
                row.get(0)
            })
            .unwrap(),
        connection
            .query_row("SELECT count(*) FROM objects", [], |row| row.get(0))
            .unwrap(),
    )
}

fn files(data: &TestDir, directory: &str) -> usize {
    std::fs::read_dir(data.0.join(directory))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                std::fs::read_dir(path).unwrap().count()
            } else {
                1
            }
        })
        .sum()
}

fn clean(data: &TestDir) {
    let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    let count: i64 = connection
        .query_row("SELECT count(*) FROM objects", [], |row| row.get(0))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while files(data, "staging") != 0 || files(data, "blobs") as i64 != count {
        assert!(
            Instant::now() < deadline,
            "upload artifacts were not reclaimed"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let pending: i64 = connection
        .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(pending, 0);
}

fn expect_continue(server: &Server, path: &str, headers: &str, length: usize) -> TcpStream {
    let mut stream = server.stream();
    stream.write_all(format!("PUT {path} HTTP/1.1\r\nHost: local\r\nContent-Length: {length}\r\nExpect: 100-continue\r\n{headers}\r\n").as_bytes()).unwrap();
    let mut interim = [0; 25];
    stream.read_exact(&mut interim).unwrap();
    assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
    stream
}

#[test]
fn xml_put_metadata_hashes_empty_and_encoded_paths_round_trip() {
    let (_config, data, mut server) = iam::fixture();
    let headers = format!(
        "{ADMIN}Content-Type: text/plain; charset=UTF-8\r\nContent-MD5: {MD5}\r\nx-goog-hash: crc32c={CRC}\r\nx-goog-hash: md5={MD5}\r\nCache-Control: private, max-age=73\r\nContent-Disposition: attachment; filename=hello.txt\r\nContent-Encoding: gzip\r\nContent-Language: en\r\nX-Goog-Meta-Project: example\r\nx-goog-meta-empty:\r\n"
    );
    let put = server.request("PUT", OBJECT, &headers, b"hello world");
    assert_eq!(put.status, 200);
    assert!(put.body.is_empty());
    assert_eq!(put.headers["etag"], "\"5eb63bbbe01eeed093cb22bb8f5acdc3\"");
    assert_eq!(put.headers["x-goog-metageneration"], "1");
    assert_eq!(put.headers["x-goog-stored-content-length"], "11");
    assert_eq!(put.headers["x-goog-stored-content-encoding"], "gzip");
    assert_eq!(
        put.headers["x-goog-hash"],
        format!("crc32c={CRC},md5={MD5}")
    );
    let get = server.get(&format!("{ADMIN}Accept-Encoding: gzip\r\n"), OBJECT);
    assert_eq!(get.ok().body, b"hello world");
    for name in [
        "etag",
        "x-goog-generation",
        "x-goog-metageneration",
        "x-goog-hash",
        "x-goog-stored-content-length",
        "x-goog-stored-content-encoding",
    ] {
        assert_eq!(get.headers[name], put.headers[name]);
    }
    assert_eq!(get.headers["cache-control"], "private, max-age=73");
    assert_eq!(
        get.headers["content-disposition"],
        "attachment; filename=hello.txt"
    );
    assert_eq!(get.headers["content-language"], "en");
    assert_eq!(get.headers["x-goog-meta-project"], "example");
    assert_eq!(get.headers["x-goog-meta-empty"], "");
    let metadata = server.get(ADMIN, JSON_OBJECT).ok().json();
    assert_eq!(metadata["contentType"], "text/plain; charset=UTF-8");
    assert_eq!(metadata["contentEncoding"], "gzip");
    assert_eq!(
        metadata["contentDisposition"],
        "attachment; filename=hello.txt"
    );
    assert_eq!(metadata["contentLanguage"], "en");
    assert_eq!(metadata["cacheControl"], "private, max-age=73");
    assert_eq!(
        metadata["metadata"],
        json!({"project":"example", "empty":""})
    );
    assert_eq!(metadata["md5Hash"], MD5);
    assert_eq!(metadata["crc32c"], CRC);
    server.stop();
    let identities = identity_file(&_config);
    let server = Server::strict(&data, &identities);
    assert_eq!(server.get(ADMIN, JSON_OBJECT).ok().json(), metadata);
    assert_eq!(
        server
            .get(&format!("{ADMIN}Accept-Encoding: gzip\r\n"), OBJECT)
            .ok()
            .body,
        b"hello world"
    );
    let empty = server.request("PUT", OBJECT, ADMIN, b"");
    assert_eq!(empty.status, 200);
    assert_eq!(
        empty.headers["etag"],
        "\"d41d8cd98f00b204e9800998ecf8427e\""
    );
    assert_eq!(empty.headers["x-goog-stored-content-encoding"], "identity");
    let get = server.get(ADMIN, OBJECT);
    assert!(get.ok().body.is_empty());
    assert_eq!(get.headers["content-type"], "application/octet-stream");
    for name in [
        "content-encoding",
        "content-language",
        "content-disposition",
        "x-goog-meta-project",
    ] {
        assert!(!get.headers.contains_key(name));
    }
    for path in ["dir%2F%E7%8C%AB+%252F", "a//b%3F%23", "%2E%2E/child"] {
        let path = format!("/iam-bucket/{path}");
        server.request("PUT", &path, ADMIN, b"encoded").ok();
        assert_eq!(server.get(ADMIN, &path).ok().body, b"encoded");
    }
    clean(&data);
}

#[test]
fn xml_put_chunked_multibuffer_and_configured_size_boundaries() {
    let data = TestDir::new();
    let (server, _) = Server::start(&data, &["--max-object-bytes", "262281"]);
    server.create_bucket("", "project-a", "iam-bucket").ok();
    let payload: Vec<u8> = (0..262281).map(|n| (n % 251) as u8).collect();
    let mut stream = server.stream();
    stream
        .write_all(
            b"PUT /iam-bucket/object HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .unwrap();
    for part in payload.chunks(17003) {
        write!(stream, "{:x}\r\n", part.len()).unwrap();
        stream.write_all(part).unwrap();
        stream.write_all(b"\r\n").unwrap();
    }
    stream.write_all(b"0\r\n\r\n").unwrap();
    let reply = Reply::read(stream);
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.headers["x-goog-stored-content-length"],
        payload.len().to_string()
    );
    assert_eq!(server.get("", OBJECT).ok().body, payload);
    assert_eq!(server.request("PUT", OBJECT, "", &payload).status, 200);
    let before = snapshot(&server, &data);
    xml_error(&server.raw(b"PUT /iam-bucket/object HTTP/1.1\r\nHost: local\r\nContent-Length: 262282\r\nExpect: 100-continue\r\n\r\n"), 400, "EntityTooLarge");
    let mut request =
        b"PUT /iam-bucket/object HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n"
            .to_vec();
    write!(request, "{:x}\r\n", payload.len()).unwrap();
    request.extend_from_slice(&payload);
    request.extend_from_slice(b"\r\n1\r\nx\r\n0\r\n\r\n");
    xml_error(&server.raw(&request), 400, "EntityTooLarge");
    assert_eq!(snapshot(&server, &data), before);
    clean(&data);
}

#[test]
fn xml_put_individual_and_combined_hash_headers_accept_matching_bytes() {
    let (_config, data, server) = iam::fixture();
    for hashes in [
        format!("Content-MD5: {MD5}\r\n"),
        format!("x-goog-hash: md5={MD5}\r\n"),
        format!("x-goog-hash: crc32c={CRC}\r\n"),
        format!("x-goog-hash: crc32c={CRC}, md5={MD5}\r\n"),
    ] {
        let mut stream = expect_continue(&server, OBJECT, &format!("{ADMIN}{hashes}"), 11);
        stream.write_all(b"hello world").unwrap();
        let reply = Reply::read(stream);
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.headers["x-goog-hash"],
            format!("crc32c={CRC},md5={MD5}")
        );
        assert_eq!(server.get(ADMIN, OBJECT).ok().body, b"hello world");
    }
    clean(&data);
}

#[test]
fn xml_put_unsupported_inputs_fail_before_continue_and_preserve_state() {
    let (_config, data, server) = fixture();
    let before = snapshot(&server, &data);
    for header in [
        "x-goog-acl: public-read",
        "x-goog-encryption-key: secret",
        "x-goog-encryption-key-sha256: value",
        "x-goog-encryption-algorithm: AES256",
        "x-goog-encryption-kms-key-name: key",
        "x-goog-storage-class: STANDARD",
        "x-goog-user-project: billing",
        "x-goog-object-lock-mode: GOVERNANCE",
        "x-goog-object-lock-retain-until-date: 2030-01-01T00:00:00Z",
        "x-goog-event-based-hold: false",
        "x-goog-temporary-hold: true",
        "x-goog-resumable: start",
        "x-goog-content-sha256: UNSIGNED-PAYLOAD",
        "x-goog-copy-source: /iam-bucket/object",
        "x-goog-allowed-resources: value",
        "x-goog-custom-time: 2026-10-23T00:00:00Z",
        "x-goog-unknown: value",
        "x-amz-acl: private",
        "Range: bytes=0-1",
        "Content-Range: bytes 0-1/2",
        "If-Match: *",
        "If-None-Match: *",
        "If-Modified-Since: Thu, 22 Oct 2026 00:00:00 GMT",
        "If-Unmodified-Since: Thu, 22 Oct 2026 00:00:00 GMT",
        "If-Range: *",
        "Trailer: x-goog-hash",
        "x-goog-gcs-idempotency-token: retry",
    ] {
        let request = format!(
            "PUT {OBJECT} HTTP/1.1\r\nHost: local\r\nContent-Length: 999999\r\nExpect: 100-continue\r\n{ADMIN}{header}\r\n\r\n"
        );
        xml_error(&server.raw(request.as_bytes()), 501, "NotImplemented");
        assert_eq!(snapshot(&server, &data), before, "{header}");
    }
    for query in [
        "acl",
        "predefinedAcl=private",
        "userProject=billing",
        "generation=1",
        "generation=abc",
        "generation=0",
        "generation=",
        "generation=9223372036854775808",
        "uploads",
        "unknown=value",
    ] {
        let request = format!(
            "PUT {OBJECT}?{query} HTTP/1.1\r\nHost: local\r\nContent-Length: 999999\r\nExpect: 100-continue\r\n{ADMIN}\r\n"
        );
        xml_error(&server.raw(request.as_bytes()), 501, "NotImplemented");
    }
    assert_eq!(snapshot(&server, &data), before);
    clean(&data);
}

#[test]
fn xml_put_invalid_headers_and_checksums_preserve_entire_object() {
    let (_config, data, server) = fixture();
    let before = snapshot(&server, &data);
    for (header, code) in [
        ("Content-Type: invalid\r\n", "InvalidArgument"),
        (
            "Content-Type: text/plain\r\nContent-Type: text/html\r\n",
            "ExcessHeaderValues",
        ),
        ("Content-MD5: invalid\r\n", "InvalidDigest"),
        (
            "Content-MD5: XrY7u+Ae7tCTyyK7j1rNww==\r\nContent-MD5: XrY7u+Ae7tCTyyK7j1rNww==\r\n",
            "ExcessHeaderValues",
        ),
        ("x-goog-hash: md5=invalid\r\n", "InvalidDigest"),
        ("x-goog-hash: crc32c=AAAA\r\n", "InvalidDigest"),
        (
            "x-goog-hash: md5=XrY7u+Ae7tCTyyK7j1rNww==,md5=XrY7u+Ae7tCTyyK7j1rNww==\r\n",
            "InvalidDigest",
        ),
        (
            "Content-MD5: AAAAAAAAAAAAAAAAAAAAAA==\r\nx-goog-hash: md5=XrY7u+Ae7tCTyyK7j1rNww==\r\n",
            "InvalidDigest",
        ),
        ("x-goog-if-generation-match: -1\r\n", "InvalidArgument"),
        ("x-goog-if-generation-match: +1\r\n", "InvalidArgument"),
        (
            "x-goog-if-generation-match: 9223372036854775808\r\n",
            "InvalidArgument",
        ),
        (
            "x-goog-if-generation-match: 0\r\nx-goog-if-generation-match: 0\r\n",
            "ExcessHeaderValues",
        ),
        ("x-goog-if-metageneration-match: 0\r\n", "InvalidArgument"),
        (
            "x-goog-if-metageneration-match: invalid\r\n",
            "InvalidArgument",
        ),
        ("x-goog-gcs-idempotency-token: \r\n", "InvalidArgument"),
        ("x-goog-meta-: value\r\n", "InvalidArgument"),
        (
            "X-Goog-Meta-Name: one\r\nx-goog-meta-name: two\r\n",
            "ExcessHeaderValues",
        ),
        (
            "Cache-Control: private\r\nCache-Control: public\r\n",
            "ExcessHeaderValues",
        ),
    ] {
        let request = format!(
            "PUT {OBJECT} HTTP/1.1\r\nHost: local\r\nContent-Length: 999999\r\nExpect: 100-continue\r\n{ADMIN}{header}\r\n"
        );
        xml_error(&server.raw(request.as_bytes()), 400, code);
        assert_eq!(snapshot(&server, &data), before, "{header}");
    }
    xml_error(
        &server.raw(format!("PUT {OBJECT} HTTP/1.1\r\nHost: local\r\n{ADMIN}\r\n").as_bytes()),
        411,
        "MissingContentLength",
    );
    for md5 in [
        format!("{MD5},crc32c=AAAAAA=="),
        format!("{MD5},md5={MD5}"),
        format!("md5={MD5}"),
        "XrY7u+Ae7tCTyyK7j1rNwx==".into(),
    ] {
        xml_error(
            &server.request(
                "PUT",
                OBJECT,
                &format!("{ADMIN}Content-MD5: {md5}\r\n"),
                b"hello world",
            ),
            400,
            "InvalidDigest",
        );
        assert_eq!(snapshot(&server, &data), before);
    }
    for header in [
        "Content-MD5: AAAAAAAAAAAAAAAAAAAAAA==\r\n",
        "x-goog-hash: crc32c=AAAAAA==\r\n",
        "x-goog-hash: md5=AAAAAAAAAAAAAAAAAAAAAA==\r\n",
    ] {
        xml_error(
            &server.request("PUT", OBJECT, &format!("{ADMIN}{header}"), b"replacement"),
            400,
            "BadDigest",
        );
        assert_eq!(snapshot(&server, &data), before);
    }
    xml_error(
        &server.request(
            "PUT",
            OBJECT,
            &format!("{ADMIN}x-goog-hash: sha256=abcd\r\n"),
            b"replacement",
        ),
        501,
        "NotImplemented",
    );
    let header = format!("{ADMIN}x-goog-meta-k: {}\r\n", "a".repeat(8192));
    xml_error(
        &server.request("PUT", OBJECT, &header, b"replacement"),
        400,
        "MetadataTooLarge",
    );
    let mut request = format!(
        "PUT {OBJECT} HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n{ADMIN}x-goog-meta-key: "
    )
    .into_bytes();
    request.extend_from_slice(b"\xff\r\n\r\nnew");
    xml_error(&server.raw(&request), 400, "InvalidArgument");
    assert_eq!(snapshot(&server, &data), before);
    clean(&data);
    server
        .request(
            "PUT",
            OBJECT,
            &format!("{ADMIN}x-goog-meta-k: {}\r\n", "a".repeat(8191)),
            b"boundary",
        )
        .ok();
    assert_eq!(
        server.get(ADMIN, JSON_OBJECT).ok().json()["metadata"]["k"]
            .as_str()
            .unwrap()
            .len(),
        8191
    );
}

#[test]
fn xml_put_strict_permissions_and_public_grants_never_fallback_on_invalid_tokens() {
    let (_config, data, server) = fixture();
    let before = snapshot(&server, &data);
    for (auth, status, code) in [
        (CREATOR, 403, "AccessDenied"),
        (VIEWER, 403, "AccessDenied"),
        (NO_ROLES, 403, "AccessDenied"),
        ("", 401, "AuthenticationRequired"),
        (
            "Authorization: Bearer invalid\r\n",
            400,
            "InvalidAuthentication",
        ),
    ] {
        let request = format!(
            "PUT {OBJECT} HTTP/1.1\r\nHost: local\r\nContent-Length: 999999\r\nExpect: 100-continue\r\n{auth}\r\n"
        );
        xml_error(&server.raw(request.as_bytes()), status, code);
        assert_eq!(snapshot(&server, &data), before);
    }
    server
        .request("PUT", "/iam-bucket/created", CREATOR, b"created")
        .ok();
    for auth in [VIEWER, ""] {
        xml_error(
            &server.request("PUT", "/iam-bucket/fresh", auth, b"denied"),
            if auth.is_empty() { 401 } else { 403 },
            if auth.is_empty() {
                "AuthenticationRequired"
            } else {
                "AccessDenied"
            },
        );
    }
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectCreator", "allUsers")]}),
    )
    .ok();
    server
        .request("PUT", "/iam-bucket/anonymous", "", b"public creator")
        .ok();
    xml_error(
        &server.request("PUT", OBJECT, "", b"denied"),
        401,
        "AuthenticationRequired",
    );
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectAdmin", "allUsers")]}),
    )
    .ok();
    for auth in ["", NO_ROLES, CREATOR] {
        server.request("PUT", OBJECT, auth, b"public admin").ok();
    }
    let public_before = snapshot(&server, &data);
    for auth in [
        "Authorization: Bearer invalid\r\n",
        "Authorization: Basic invalid\r\n",
    ] {
        xml_error(
            &server.request("PUT", OBJECT, auth, b"bad credentials"),
            400,
            "InvalidAuthentication",
        );
    }
    assert_eq!(snapshot(&server, &data), public_before);
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectCreator", "allAuthenticatedUsers")]}),
    )
    .ok();
    server
        .request("PUT", "/iam-bucket/registered", NO_ROLES, b"registered")
        .ok();
    xml_error(
        &server.request("PUT", "/iam-bucket/not-anonymous", "", b"denied"),
        401,
        "AuthenticationRequired",
    );
    xml_error(
        &server.request("PUT", "/missing-bucket/object", ADMIN, b"missing"),
        404,
        "NoSuchBucket",
    );
    clean(&data);
}

#[test]
fn xml_put_conditions_and_concurrent_overwrites_are_atomic() {
    let (_config, data, server) = fixture();
    let before = snapshot(&server, &data);
    let generation = before.0["generation"].as_str().unwrap();
    for condition in [
        "x-goog-if-generation-match: 0\r\n".to_owned(),
        "x-goog-if-generation-match: 1\r\n".to_owned(),
        "x-goog-if-metageneration-match: 2\r\n".to_owned(),
    ] {
        xml_error(
            &server.request("PUT", OBJECT, &format!("{ADMIN}{condition}"), b"stale"),
            412,
            "PreconditionFailed",
        );
        assert_eq!(snapshot(&server, &data), before);
    }
    let headers = format!(
        "{ADMIN}x-goog-if-generation-match: {generation}\r\nx-goog-if-metageneration-match: 1\r\nx-goog-gcs-idempotency-token: retry\r\n"
    );
    let mut streams = [
        expect_continue(&server, OBJECT, &headers, 6),
        expect_continue(&server, OBJECT, &headers, 6),
    ];
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = streams
        .iter_mut()
        .map(|stream| {
            let mut stream = stream.try_clone().unwrap();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                stream.write_all(b"winner").unwrap();
                Reply::read(stream)
            })
        })
        .collect();
    let responses: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let mut statuses: Vec<_> = responses.iter().map(|reply| reply.status).collect();
    statuses.sort();
    assert_eq!(statuses, [200, 412]);
    xml_error(
        responses.iter().find(|reply| reply.status == 412).unwrap(),
        412,
        "PreconditionFailed",
    );
    assert_eq!(server.get(ADMIN, OBJECT).ok().body, b"winner");
    let counter: i64 = rusqlite::Connection::open(data.0.join("catalog.sqlite3"))
        .unwrap()
        .query_row("SELECT last_generation FROM object_generation", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        counter.to_string(),
        server.get(ADMIN, JSON_OBJECT).ok().json()["generation"]
            .as_str()
            .unwrap()
    );
    let headers = format!("{ADMIN}x-goog-if-generation-match: 0\r\n");
    let mut a = expect_continue(&server, "/iam-bucket/absent", &headers, 1);
    let mut b = expect_continue(&server, "/iam-bucket/absent", &headers, 1);
    a.write_all(b"a").unwrap();
    b.write_all(b"b").unwrap();
    let mut statuses = [Reply::read(a).status, Reply::read(b).status];
    statuses.sort();
    assert_eq!(statuses, [200, 412]);
    clean(&data);
}

#[test]
fn xml_put_rechecks_create_delete_and_bucket_grants_at_publication() {
    let (_config, data, server) = iam::fixture();
    let mut late = expect_continue(&server, OBJECT, CREATOR, 4);
    server.request("PUT", OBJECT, ADMIN, b"winner").ok();
    let before = snapshot(&server, &data);
    late.write_all(b"late").unwrap();
    xml_error(&Reply::read(late), 403, "AccessDenied");
    assert_eq!(snapshot(&server, &data), before);
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectAdmin", "allUsers")]}),
    )
    .ok();
    let mut revoked = expect_continue(&server, OBJECT, "", 4);
    iam::set(&server, ADMIN, &json!({"bindings":[]})).ok();
    revoked.write_all(b"late").unwrap();
    xml_error(&Reply::read(revoked), 401, "AuthenticationRequired");
    assert_eq!(snapshot(&server, &data), before);
    clean(&data);
}

#[test]
fn xml_put_incomplete_disconnect_shutdown_and_restart_reclaim_artifacts() {
    let (_config, data, mut server) = fixture();
    let before = snapshot(&server, &data);
    let mut stream = expect_continue(&server, OBJECT, ADMIN, 99999);
    stream.write_all(b"partial").unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut discarded = Vec::new();
    match stream.read_to_end(&mut discarded) {
        Ok(_) => {
            if !discarded.is_empty() {
                assert!(discarded.starts_with(b"HTTP/1.1 400 "));
                assert!(
                    String::from_utf8_lossy(&discarded).contains("<Code>IncompleteBody</Code>")
                );
            }
        }
        Err(error) => {
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
            assert!(discarded.is_empty());
        }
    }
    clean(&data);
    assert_eq!(snapshot(&server, &data), before);
    let mut stream = expect_continue(&server, OBJECT, ADMIN, 99999);
    stream.write_all(b"partial").unwrap();
    server.stop();
    drop(stream);
    let identities = identity_file(&_config);
    let mut server = Server::strict(&data, &identities);
    assert_eq!(snapshot(&server, &data), before);
    clean(&data);
    let mut stream = expect_continue(&server, OBJECT, ADMIN, 99999);
    stream.write_all(b"partial").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while files(&data, "staging") == 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    drop(stream);
    drop(server);
    let server = Server::strict(&data, &identities);
    assert_eq!(snapshot(&server, &data), before);
    clean(&data);
}

#[test]
fn xml_put_chunked_trailers_and_body_deadlines_cannot_publish() {
    let data = TestDir::new();
    let (mut server, _) = Server::start(&data, &[]);
    server.create_bucket("", "project-a", "iam-bucket").ok();
    server.request("PUT", OBJECT, "", b"previous").ok();
    let before = snapshot(&server, &data);
    xml_error(&server.raw(b"PUT /iam-bucket/object HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nnew\r\n0\r\nx-goog-hash: crc32c=AAAAAA==\r\n\r\n"), 400, "IncompleteBody");
    clean(&data);
    assert_eq!(snapshot(&server, &data), before);
    let mut stream = expect_continue(&server, OBJECT, "", 99999);
    stream.write_all(b"partial").unwrap();
    let mut received = Vec::new();
    assert_eq!(stream.read_to_end(&mut received).unwrap(), 0);
    clean(&data);
    assert_eq!(snapshot(&server, &data), before);
    server.stop();
    let (server, _) = Server::start(&data, &["--object-request-timeout-ms", "500"]);
    let mut stream = expect_continue(&server, OBJECT, "", 99999);
    for _ in 0..10 {
        if stream.write_all(b"x").is_err() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let mut received = Vec::new();
    match stream.read_to_end(&mut received) {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        result => panic!("Expected a closed timed-out upload, got {result:?}"),
    }
    assert!(received.is_empty());
    clean(&data);
    assert_eq!(snapshot(&server, &data), before);
}

#[test]
fn xml_put_versioning_and_default_event_holds_match_shared_uploads() {
    let (_config, data, mut server) = fixture();
    server
        .request(
            "PATCH",
            "/storage/v1/b/iam-bucket",
            ADMIN,
            br#"{"versioning":{"enabled":true}}"#,
        )
        .ok();
    let first = server.get(ADMIN, JSON_OBJECT).ok().json();
    server
        .request(
            "PUT",
            OBJECT,
            &format!("{ADMIN}x-goog-if-metageneration-match: 1\r\n"),
            b"second",
        )
        .ok();
    let second = server.get(ADMIN, JSON_OBJECT).ok().json();
    assert_ne!(first["generation"], second["generation"]);
    assert_eq!(
        server
            .get(
                ADMIN,
                &format!(
                    "{JSON_OBJECT}?generation={}&alt=media",
                    first["generation"].as_str().unwrap()
                )
            )
            .ok()
            .body,
        b"hello world"
    );
    let versions = server
        .get(ADMIN, "/storage/v1/b/iam-bucket/o?versions=true")
        .ok()
        .json();
    assert_eq!(versions["items"].as_array().unwrap().len(), 2);
    server
        .request(
            "PATCH",
            "/storage/v1/b/iam-bucket",
            ADMIN,
            br#"{"defaultEventBasedHold":true}"#,
        )
        .ok();
    server.request("PUT", OBJECT, ADMIN, b"held").ok();
    let before = snapshot(&server, &data);
    assert_eq!(before.0["eventBasedHold"], true);
    xml_error(
        &server.request("PUT", OBJECT, ADMIN, b"denied"),
        403,
        "AccessDenied",
    );
    assert_eq!(snapshot(&server, &data), before);
    server.stop();
    let identities = identity_file(&_config);
    let server = Server::strict(&data, &identities);
    assert_eq!(snapshot(&server, &data), before);
    assert_eq!(
        server
            .get(
                ADMIN,
                &format!(
                    "{JSON_OBJECT}?generation={}&alt=media",
                    second["generation"].as_str().unwrap()
                )
            )
            .ok()
            .body,
        b"second"
    );
    clean(&data);
}

#[test]
fn xml_put_bucket_cors_covers_preflight_success_and_denials() {
    let (_config, _data, server) = fixture();
    server.request("PATCH", "/storage/v1/b/iam-bucket", ADMIN, br#"{"cors":[{"origin":["https://browser.example"],"method":["PUT"],"responseHeader":["Content-Type","Content-MD5","ETag","x-goog-generation"],"maxAgeSeconds":73}]}"#).ok();
    let origin = "Origin: https://browser.example\r\n";
    let preflight = server.request("OPTIONS", OBJECT, &format!("{origin}Access-Control-Request-Method: PUT\r\nAccess-Control-Request-Headers: content-type, content-md5\r\n"), b"");
    assert_eq!(preflight.status, 200);
    assert_eq!(
        preflight.headers["access-control-allow-origin"],
        "https://browser.example"
    );
    assert_eq!(preflight.headers["access-control-allow-methods"], "PUT");
    assert_eq!(preflight.headers["access-control-max-age"], "73");
    for (auth, extra, status, code) in [
        (ADMIN, "", 200, ""),
        (VIEWER, "", 403, "AccessDenied"),
        ("", "", 401, "AuthenticationRequired"),
        (ADMIN, "x-goog-acl: private\r\n", 501, "NotImplemented"),
    ] {
        let reply = server.request("PUT", OBJECT, &format!("{auth}{origin}{extra}"), b"new");
        if status == 200 {
            reply.ok();
        } else {
            xml_error(&reply, status, code);
        }
        assert_eq!(
            reply.headers["access-control-allow-origin"],
            "https://browser.example"
        );
        assert!(reply.headers["access-control-expose-headers"].contains("ETag"));
        assert!(
            !reply
                .headers
                .contains_key("access-control-allow-credentials")
        );
    }
    let denied = server.request(
        "PUT",
        OBJECT,
        &format!("{ADMIN}Origin: https://denied.example\r\n"),
        b"allowed by IAM",
    );
    denied.ok();
    assert!(!denied.headers.contains_key("access-control-allow-origin"));
    let denied = server.request("OPTIONS", OBJECT, &format!("{origin}Access-Control-Request-Method: PUT\r\nAccess-Control-Request-Headers: x-unsupported\r\n"), b"");
    assert!(!denied.headers.contains_key("access-control-allow-origin"));
}

#[test]
fn xml_put_upload_admission_is_bounded_and_releases_aborted_slots() {
    let (_config, data, server) = fixture();
    let before = snapshot(&server, &data);
    let mut streams: Vec<_> = (0..8)
        .map(|n| expect_continue(&server, &format!("/iam-bucket/held-{n}"), ADMIN, 99999))
        .collect();
    streams.push(expect_continue(&server, OBJECT, ADMIN, 999999));
    assert_eq!(snapshot(&server, &data), before);
    drop(streams);
    clean(&data);
    server
        .request("PUT", "/iam-bucket/after-release", ADMIN, b"slot available")
        .ok();
}
