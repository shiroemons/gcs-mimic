//! Open-ended resumable writes (`Content-Range: bytes start-*/total`), the form the Node.js
//! client sends for single-request resumable uploads. The body carries the rest of the
//! object, so the request always completes the upload: at its EOF when the total is unknown,
//! or only when it reaches a declared total.
use super::*;

const CHUNK: usize = 256 * 1024;

fn session(server: &Server, name: &str, headers: &str) -> String {
    let reply = server.request(
        "POST",
        &format!("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name={name}"),
        &format!("Content-Type: application/json\r\n{headers}"),
        br#"{"contentType":"text/plain"}"#,
    );
    completed(&reply);
    reply.headers["location"]
        .strip_prefix("https://advertised.invalid/api")
        .unwrap()
        .to_owned()
}

/// Asserts a 200 response and returns its body as JSON (empty for initiation).
fn completed(reply: &Reply) -> Value {
    let body = String::from_utf8_lossy(&reply.body);
    assert_eq!(reply.status, 200, "{body}");
    if reply.body.is_empty() {
        Value::Null
    } else {
        reply.json()
    }
}

/// A PUT whose body uses chunked transfer coding, as streaming clients send it, so the
/// server cannot learn the body length before EOF.
fn put_chunked(server: &Server, path: &str, range: &str, body: &[u8]) -> Reply {
    let mut request = format!(
        "PUT {path} HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\nConnection: close\r\nContent-Range: {range}\r\n\r\n"
    )
    .into_bytes();
    for part in body.chunks(64 * 1024) {
        request.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
        request.extend_from_slice(part);
        request.extend_from_slice(b"\r\n");
    }
    request.extend_from_slice(b"0\r\n\r\n");
    let mut stream = server.stream();
    // A rejected body may be answered before it is fully sent; the response still arrives.
    if let Err(error) = stream.write_all(&request) {
        assert!(closed_by_server(&error), "{error}");
    }
    Reply::read(stream)
}

fn put(server: &Server, path: &str, range: &str, body: &[u8]) -> Reply {
    server.request("PUT", path, &format!("Content-Range: {range}\r\n"), body)
}

/// Sends the first 256 KiB chunk with an unknown total and checks it was acknowledged.
fn put_prefix(server: &Server, path: &str, object: &[u8]) {
    let reply = put(
        server,
        path,
        &format!("bytes 0-{}/*", CHUNK - 1),
        &object[..CHUNK],
    );
    assert_eq!(reply.status, 308);
    assert_eq!(reply.headers["range"], format!("bytes=0-{}", CHUNK - 1));
}

fn status(server: &Server, path: &str) -> Reply {
    let reply = put(server, path, "bytes */*", b"");
    assert_eq!(reply.status, 308);
    reply
}

fn media(server: &Server, name: &str) -> Vec<u8> {
    let reply = server.get(&format!("{name}?alt=media"));
    assert_eq!(reply.status, 200);
    reply.body
}

fn pattern(length: usize) -> Vec<u8> {
    (0..length).map(|i| (i % 251) as u8).collect()
}

#[test]
fn open_range_completes_single_request_uploads_at_body_eof() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1_000_000, 5000);
    server.bucket();

    // Node.js File#save default: unknown end and total, chunked body.
    let path = session(&server, "chunked", "");
    let resource = completed(&put_chunked(
        &server,
        &path,
        "bytes 0-*/*",
        b"Node default bytes\n",
    ));
    assert_eq!(resource["name"], "chunked");
    assert_eq!(resource["size"], "19");
    assert_eq!(resource["contentType"], "text/plain");
    assert_eq!(media(&server, "chunked"), b"Node default bytes\n");
    // A retry after completion returns the stored resource without another generation.
    let retry = put_chunked(&server, &path, "bytes 0-*/*", b"Node default bytes\n");
    assert_eq!(completed(&retry), resource);

    // A declared Content-Length fixes the end without a total.
    let path = session(&server, "declared", "");
    let resource = completed(&put(&server, &path, "bytes 0-*/*", b"declared length"));
    assert_eq!(resource["size"], "15");
    assert_eq!(media(&server, "declared"), b"declared length");

    // A known total with a chunked body finalizes when the body reaches it.
    let path = session(&server, "known-total", "");
    completed(&put_chunked(&server, &path, "bytes 0-*/5", b"exact"));
    assert_eq!(media(&server, "known-total"), b"exact");

    // The total declared at initiation applies to an open request with an unknown total.
    let path = session(
        &server,
        "initiation-total",
        "X-Upload-Content-Length: 4\r\n",
    );
    completed(&put_chunked(&server, &path, "bytes 0-*/*", b"four"));
    assert_eq!(media(&server, "initiation-total"), b"four");

    // An empty body completes an empty object.
    for (name, chunked) in [("empty-chunked", true), ("empty-declared", false)] {
        let path = session(&server, name, "");
        let reply = if chunked {
            put_chunked(&server, &path, "bytes 0-*/*", b"")
        } else {
            put(&server, &path, "bytes 0-*/*", b"")
        };
        assert_eq!(completed(&reply)["size"], "0");
        assert_eq!(media(&server, name), b"");
    }
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn open_range_resumes_after_a_persisted_prefix_without_rewriting_it() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1_000_000, 5000);
    server.bucket();
    let object = pattern(CHUNK + 1000);

    // Resume from the acknowledged offset, as the client does after a status query.
    let path = session(&server, "resumed", "");
    put_prefix(&server, &path, &object);
    let resource = completed(&put_chunked(
        &server,
        &path,
        &format!("bytes {CHUNK}-*/*"),
        &object[CHUNK..],
    ));
    assert_eq!(resource["size"], object.len().to_string());
    assert_eq!(media(&server, "resumed"), object);

    // Resending the whole object from zero appends only the unseen tail; the persisted
    // prefix keeps its original bytes even when the resent bytes differ.
    let path = session(&server, "overlap", "");
    put_prefix(&server, &path, &object);
    let mut resent = vec![b'x'; CHUNK];
    resent.extend_from_slice(&object[CHUNK..]);
    completed(&put_chunked(&server, &path, "bytes 0-*/*", &resent));
    assert_eq!(media(&server, "overlap"), object);
}

#[test]
fn open_range_rejects_malformed_and_inconsistent_requests_without_advancing() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1_000_000, 5000);
    server.bucket();
    let path = session(&server, "rejected", "");

    for range in [
        "bytes *-*/*",
        "bytes -*/*",
        "bytes 0-*",
        "bytes 0-*/x",
        "bytes 0-*/-1",
        "bytes x-*/*",
        "bytes 6-*/5",
        "bytes 0-**/*",
    ] {
        put_chunked(&server, &path, range, b"abc").error(400, "invalidParameter");
    }
    // A start past the persisted offset would leave a gap.
    put_chunked(&server, &path, "bytes 1-*/*", b"abc").error(400, "invalidParameter");
    // A declared Content-Length must reach the declared total.
    put(&server, &path, "bytes 0-*/5", b"abc").error(400, "invalidParameter");
    // A chunked body that ends before, or runs past, the declared total completes nothing.
    put_chunked(&server, &path, "bytes 0-*/5", b"abc").error(400, "badRequest");
    put_chunked(&server, &path, "bytes 0-*/5", b"abcdefg").error(400, "badRequest");
    let probe = status(&server, &path);
    assert!(!probe.headers.contains_key("range"), "{:?}", probe.headers);
    assert_eq!(server.get("rejected").status, 404);
    // The session stays usable after rejected requests.
    completed(&put_chunked(&server, &path, "bytes 0-*/5", b"abcde"));
    assert_eq!(media(&server, "rejected"), b"abcde");

    // A total that conflicts with the initiation total is rejected before the body is read,
    // and an unknown-total body must still reach the initiation total.
    let path = session(&server, "conflict", "X-Upload-Content-Length: 5\r\n");
    put_chunked(&server, &path, "bytes 0-*/6", b"abcdef").error(400, "invalidParameter");
    put_chunked(&server, &path, "bytes 0-*/*", b"abc").error(400, "badRequest");
    assert!(!status(&server, &path).headers.contains_key("range"));

    // A body that ends inside the persisted prefix cannot shrink the object.
    let object = pattern(CHUNK);
    let path = session(&server, "shrink", "");
    put_prefix(&server, &path, &object);
    put_chunked(&server, &path, "bytes 0-*/*", &object[..100]).error(400, "invalidParameter");
    let probe = status(&server, &path);
    assert_eq!(probe.headers["range"], format!("bytes=0-{}", CHUNK - 1));
    // An empty open request at the persisted offset completes the prefix as the object.
    completed(&put(&server, &path, &format!("bytes {CHUNK}-*/*"), b""));
    assert_eq!(media(&server, "shrink"), object);
}

#[test]
fn open_range_bodies_are_bounded_by_the_object_size_limit() {
    let dir = TestDir::new();
    let limit = CHUNK + 1000;
    let server = Server::start(&dir, limit as u64, 5000);
    server.bucket();
    let object = pattern(limit + 1);

    let path = session(&server, "capped", "");
    put_chunked(&server, &path, "bytes 0-*/*", &object).error(413, "uploadTooLarge");
    put_chunked(&server, &path, &format!("bytes 0-*/{}", limit + 1), b"x")
        .error(413, "uploadTooLarge");
    // After a persisted prefix, the remaining allowance is the limit minus the offset.
    put_prefix(&server, &path, &object);
    put_chunked(
        &server,
        &path,
        &format!("bytes {CHUNK}-*/*"),
        &object[CHUNK..],
    )
    .error(413, "uploadTooLarge");
    let probe = status(&server, &path);
    assert_eq!(probe.headers["range"], format!("bytes=0-{}", CHUNK - 1));
    // Exactly the limit succeeds.
    completed(&put_chunked(
        &server,
        &path,
        &format!("bytes {CHUNK}-*/*"),
        &object[CHUNK..limit],
    ));
    assert_eq!(media(&server, "capped"), &object[..limit]);
}
