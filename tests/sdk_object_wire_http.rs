mod object_effects_support;

use flate2::{Compression, write::GzEncoder};
use object_effects_support::{Server, common::TestDir};
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

const ADMIN: &str = "Authorization: Bearer admin\r\n";

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn gzip_object(server: &Server, name: &str, data: &[u8]) {
    server.put(name, data).json();
    server
        .request(
            "PATCH",
            &format!("/storage/v1/b/test-bucket/o/{name}"),
            ADMIN,
            br#"{"contentEncoding":"gzip"}"#,
        )
        .json();
}

#[test]
fn json_gzip_negotiation_range_and_conditions_preserve_stored_representation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    let compressed = [gzip(b"first"), gzip(b""), gzip(b"second")].concat();
    gzip_object(&server, "gzip", &compressed);
    let path = "/storage/v1/b/test-bucket/o/gzip?alt=media";
    let raw = server.request(
        "GET",
        path,
        &format!("{ADMIN}Accept-Encoding: gzip\r\n"),
        b"",
    );
    assert_eq!(raw.status, 200);
    assert_eq!(raw.body, compressed);
    assert_eq!(raw.headers["content-encoding"], "gzip");
    assert!(!raw.headers.contains_key("content-length"));
    assert_eq!(raw.headers["transfer-encoding"], "chunked");
    assert_eq!(raw.headers["vary"], "Origin, X-Origin, Accept-Encoding");
    assert!(
        !raw.headers
            .contains_key("x-guploader-response-body-transformations")
    );
    for extra in [
        "",
        "Accept-Encoding: identity\r\n",
        "Accept-Encoding: gzip;q=0\r\n",
        "Range: bytes=0-4\r\n",
        "Range: bytes=999999-\r\n",
    ] {
        let expanded = server.request("GET", path, &format!("{ADMIN}{extra}"), b"");
        assert_eq!(expanded.status, 200, "{extra}");
        assert_eq!(expanded.body, b"firstsecond");
        assert_eq!(
            expanded.headers["x-guploader-response-body-transformations"],
            "gunzipped"
        );
        assert_eq!(expanded.headers["warning"], "214 UploadServer gunzipped");
        assert_eq!(expanded.headers["vary"], "Origin, X-Origin");
        for name in [
            "content-encoding",
            "content-length",
            "content-range",
            "accept-ranges",
        ] {
            assert!(!expanded.headers.contains_key(name), "{name}");
        }
        for name in [
            "x-goog-hash",
            "x-goog-stored-content-encoding",
            "x-goog-stored-content-length",
        ] {
            assert_eq!(expanded.headers[name], raw.headers[name], "{name}");
        }
        // Expanded bytes differ from the stored ones, so the tag is weak (observed 2026-10-31).
        assert_eq!(
            expanded.headers["etag"],
            format!("W/{}", raw.headers["etag"]),
            "{extra}"
        );
    }
    let not_modified = server.request(
        "GET",
        path,
        &format!("{ADMIN}If-None-Match: {}\r\n", raw.headers["etag"]),
        b"",
    );
    assert_eq!(not_modified.status, 304);
    assert!(not_modified.body.is_empty());
    server
        .request("GET", path, "Authorization: Bearer creator\r\n", b"")
        .error(403, "forbidden");
    server
        .request(
            "GET",
            path,
            &format!("{ADMIN}Accept-Encoding: gzip;q=2\r\n"),
            b"",
        )
        .error(400, "invalidParameter");
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/gzip",
            ADMIN,
            br#"{"cacheControl":"no-transform"}"#,
        )
        .json();
    let fixed = server.request("GET", path, ADMIN, b"");
    assert_eq!(fixed.body, compressed);
    assert!(!fixed.headers.contains_key("content-length"));
    assert_eq!(fixed.headers["transfer-encoding"], "chunked");
    assert_eq!(
        fixed.headers["cache-control"],
        "no-cache, no-store, max-age=0, must-revalidate, no-transform"
    );
    // no-transform fixes the representation, so it no longer varies by Accept-Encoding.
    assert_eq!(fixed.headers["vary"], "Origin, X-Origin");
    let head = server.request("HEAD", path, ADMIN, b"");
    assert_eq!(head.headers["content-length"], compressed.len().to_string());
    assert_eq!(
        head.headers["cache-control"],
        "no-cache, no-store, max-age=0, must-revalidate"
    );
}

#[test]
fn xml_gzip_gunzips_whole_object_and_keeps_stored_hashes() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    let compressed = gzip(b"0123456789");
    gzip_object(&server, "gzip", &compressed);
    let path = "/test-bucket/gzip";
    let raw = server.request(
        "GET",
        path,
        &format!("{ADMIN}Accept-Encoding: gzip\r\nRange: bytes=2-5\r\n"),
        b"",
    );
    assert_eq!(raw.status, 206);
    assert_eq!(raw.body, compressed[2..6]);
    assert_eq!(raw.headers["content-encoding"], "gzip");
    assert_eq!(raw.headers["vary"], "Accept-Encoding");
    for method in ["GET", "HEAD"] {
        for extra in ["", "Accept-Encoding: identity\r\n", "Range: bytes=0-4\r\n"] {
            let expanded = server.request(method, path, &format!("{ADMIN}{extra}"), b"");
            assert_eq!(expanded.status, 200, "{method} {extra}");
            if method == "GET" {
                assert_eq!(expanded.body, b"0123456789");
            }
            assert_eq!(
                expanded.headers["x-guploader-response-body-transformations"],
                "gunzipped"
            );
            assert_eq!(expanded.headers["warning"], "214 UploadServer gunzipped");
            assert_eq!(expanded.headers["vary"], "Accept-Encoding");
            // The XML API keeps one field per stored-object hash.
            assert_eq!(expanded.headers["x-goog-hash"], raw.headers["x-goog-hash"]);
            assert_eq!(expanded.headers["x-goog-stored-content-encoding"], "gzip");
            for name in [
                "content-encoding",
                "content-length",
                "content-range",
                "accept-ranges",
            ] {
                assert!(!expanded.headers.contains_key(name), "{method} {name}");
            }
        }
    }
    // Authorization precedes transcoding: a denied caller learns nothing about the encoding.
    let denied = server.request("GET", path, "Authorization: Bearer creator\r\n", b"");
    assert_eq!(denied.status, 403);
    assert!(
        !denied
            .headers
            .contains_key("x-guploader-response-body-transformations")
    );
}

/// On a bucket without CORS configuration Cloud Storage's XML API sends no `vary: Origin`,
/// even for a request with an Origin header, and sends `vary: Accept-Encoding` on every
/// GET/HEAD of a stored-gzip object. A compressed HEAD keeps `accept-ranges` but states no
/// length (observed 2026-10-31). A bucket with CORS rules keeps `vary: Origin` locally.
#[test]
fn xml_vary_follows_bucket_cors_and_stored_gzip() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("plain", b"0123456789abcdef").json();
    gzip_object(&server, "gzip", &gzip(b"hello gzip transcoding world\n"));
    let origin = "Origin: https://example.test\r\n";
    let check = |expected_plain: &[&str], expected_gzip: &[&str]| {
        for method in ["GET", "HEAD"] {
            for extra in ["", origin] {
                let headers = format!("{ADMIN}{extra}");
                assert_eq!(
                    vary_fields(&server, method, "/test-bucket/plain", &headers),
                    expected_plain,
                    "{method} plain {extra}"
                );
                for encoding in [
                    "",
                    "Accept-Encoding: identity\r\n",
                    "Accept-Encoding: gzip\r\n",
                ] {
                    let headers = format!("{headers}{encoding}");
                    assert_eq!(
                        vary_fields(&server, method, "/test-bucket/gzip", &headers),
                        expected_gzip,
                        "{method} gzip {extra}{encoding}"
                    );
                }
            }
        }
    };
    check(&[], &["Accept-Encoding"]);
    let compressed = server.request(
        "HEAD",
        "/test-bucket/gzip",
        &format!("{ADMIN}Accept-Encoding: gzip\r\n"),
        b"",
    );
    assert_eq!(compressed.headers["content-encoding"], "gzip");
    assert_eq!(compressed.headers["accept-ranges"], "bytes");
    assert!(!compressed.headers.contains_key("content-length"));
    let plain = server.request("HEAD", "/test-bucket/plain", ADMIN, b"");
    assert_eq!(plain.headers["content-length"], "16");
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            ADMIN,
            br#"{"cors":[{"origin":["https://other.test"],"method":["GET"]}]}"#,
        )
        .json();
    check(&["Origin"], &["Origin", "Accept-Encoding"]);
}

/// The raw `vary` field lines of one response, in order.
fn vary_fields(server: &Server, method: &str, path: &str, headers: &str) -> Vec<String> {
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{headers}\r\n"
    )
    .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let response = String::from_utf8_lossy(&response);
    let head = response.split("\r\n\r\n").next().unwrap();
    head.lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("vary"))
        .map(|(_, value)| value.trim().to_owned())
        .collect()
}

/// Every Cloud Storage JSON API response sends `vary: Origin` and `vary: X-Origin` as two
/// fields, with or without an Origin request header and on errors (observed 2026-10-31).
#[test]
fn json_responses_vary_by_origin_and_x_origin() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("plain", b"0123456789abcdef").json();
    gzip_object(&server, "gzip", &gzip(b"compressed"));
    let both = ["Origin", "X-Origin"];
    for origin in ["", "Origin: https://example.test\r\n"] {
        for (method, path, extra, expected) in [
            ("GET", "/storage/v1/b/test-bucket/o/plain", "", &both[..]),
            (
                "GET",
                "/storage/v1/b/test-bucket/o/plain?alt=media",
                "",
                &both,
            ),
            ("HEAD", "/storage/v1/b/test-bucket/o/plain", "", &both),
            ("GET", "/storage/v1/b/test-bucket/o?maxResults=1", "", &both),
            ("GET", "/storage/v1/b/test-bucket/o/missing", "", &both),
            ("GET", "/storage/v1/b/test-bucket", "", &both),
            (
                "GET",
                "/download/storage/v1/b/test-bucket/o/gzip?alt=media",
                "",
                &both,
            ),
            (
                "GET",
                "/storage/v1/b/test-bucket/o/gzip?alt=media",
                "Accept-Encoding: gzip\r\n",
                &["Origin", "X-Origin", "Accept-Encoding"],
            ),
        ] {
            let headers = format!("{ADMIN}{origin}{extra}");
            assert_eq!(
                vary_fields(&server, method, path, &headers),
                expected,
                "{method} {path} {origin}"
            );
        }
    }
}

/// Cloud Storage answers a JSON media HEAD with the GET headers and no body; only the
/// identity representation states its Content-Length (observed 2026-10-31).
#[test]
fn json_media_head_sends_get_headers_without_a_body() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("plain", b"0123456789abcdef").json();
    gzip_object(&server, "gzip", &gzip(b"hello gzip transcoding world\n"));
    // Framing and date fields differ between a streamed GET and its HEAD by design.
    let framed = |reply: &object_effects_support::Reply| {
        let mut headers = reply.headers.clone();
        for name in ["date", "content-length", "transfer-encoding"] {
            headers.remove(name);
        }
        headers
    };
    for prefix in ["/storage/v1", "/download/storage/v1"] {
        for (object, extra, length) in [
            ("plain", "", Some("16")),
            ("gzip", "", None),
            ("gzip", "Accept-Encoding: identity\r\n", None),
            ("gzip", "Accept-Encoding: gzip\r\n", None),
        ] {
            let path = format!("{prefix}/b/test-bucket/o/{object}?alt=media");
            let headers = format!("{ADMIN}{extra}");
            let get = server.request("GET", &path, &headers, b"");
            let head = server.request("HEAD", &path, &headers, b"");
            assert_eq!(head.status, 200, "{path} {extra}");
            assert_eq!(
                head.headers.get("content-length").map(String::as_str),
                length,
                "{path} {extra}"
            );
            assert_eq!(framed(&head), framed(&get), "{path} {extra}");
            assert!(!head.headers.contains_key("transfer-encoding"));
        }
    }
    let gunzipped = server.request(
        "HEAD",
        "/storage/v1/b/test-bucket/o/gzip?alt=media",
        ADMIN,
        b"",
    );
    assert_eq!(
        gunzipped.headers["x-guploader-response-body-transformations"],
        "gunzipped"
    );
    let compressed = server.request(
        "HEAD",
        "/download/storage/v1/b/test-bucket/o/gzip?alt=media",
        &format!("{ADMIN}Accept-Encoding: gzip\r\n"),
        b"",
    );
    assert_eq!(compressed.headers["content-encoding"], "gzip");
    assert!(compressed.headers["vary"].ends_with("Accept-Encoding"));
    for (path, headers, status) in [
        ("/download/storage/v1/b/test-bucket/o/plain", ADMIN, 400),
        ("/storage/v1/b/test-bucket/o/missing?alt=media", ADMIN, 404),
        (
            "/storage/v1/b/test-bucket/o/plain?alt=media",
            "Authorization: Bearer creator\r\n",
            403,
        ),
        (
            "/storage/v1/b/test-bucket/o/plain?alt=media&ifGenerationMatch=1",
            ADMIN,
            412,
        ),
    ] {
        let response = server.request("HEAD", path, headers, b"");
        assert_eq!(response.status, status, "{path}");
    }
}

#[test]
fn json_media_upload_content_encoding_query_sets_stored_encoding() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    let plain = b"hello gzip transcoding world\n";
    let compressed = gzip(plain);
    let upload = "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=gz.txt";
    let created = server.request(
        "POST",
        &format!("{upload}&contentEncoding=gzip"),
        ADMIN,
        &compressed,
    );
    assert_eq!(created.status, 200);
    let created = created.json();
    assert_eq!(created["contentEncoding"], "gzip");
    assert_eq!(created["size"], compressed.len().to_string());
    assert_eq!(server.get("gz.txt").json()["contentEncoding"], "gzip");
    let path = "/storage/v1/b/test-bucket/o/gz.txt?alt=media";
    let raw = server.request(
        "GET",
        path,
        &format!("{ADMIN}Accept-Encoding: gzip\r\n"),
        b"",
    );
    assert_eq!(raw.body, compressed);
    let expanded = server.request("GET", path, ADMIN, b"");
    assert_eq!(expanded.body, plain);
    // The query value is validated like the metadata property and rejected before the body.
    let invalid = server.request(
        "POST",
        &format!("{upload}&contentEncoding=a%0Ab"),
        ADMIN,
        b"replacement",
    );
    assert_eq!(invalid.status, 400);
    assert_eq!(
        server.get("gz.txt").json()["generation"],
        created["generation"]
    );
    let kms = server.request("POST", &format!("{upload}&kmsKeyName=key"), ADMIN, b"x");
    assert_eq!(kms.status, 400);
}

#[test]
fn json_gzip_corruption_and_expansion_limit_abort_instead_of_clean_eof() {
    let dir = TestDir::new();
    let server = Server::start_with_limits(&dir, false, 30000, 900000, 1024);
    server.bucket();
    let valid = gzip(b"decoded");
    let mut bad_crc = valid.clone();
    bad_crc[valid.len() - 8] ^= 1;
    let mut bad_size = valid.clone();
    bad_size[valid.len() - 4] ^= 1;
    for (index, bytes) in [
        valid[..12].to_vec(),
        valid[..valid.len() - 1].to_vec(),
        bad_crc,
        bad_size,
        [valid.as_slice(), b"garbage"].concat(),
        gzip(&[0; 1025]),
    ]
    .iter()
    .enumerate()
    {
        let name = format!("bad{index}");
        gzip_object(&server, &name, bytes);
        let mut stream = TcpStream::connect(&server.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(stream, "GET /storage/v1/b/test-bucket/o/{name}?alt=media HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = Vec::new();
        if let Err(error) = stream.read_to_end(&mut response) {
            assert!(matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::UnexpectedEof
            ));
        }
        assert!(
            response.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&response)
        );
        assert!(
            !response.ends_with(b"\r\n0\r\n\r\n"),
            "invalid gzip completed cleanly: {index}"
        );
        assert_eq!(
            server.get(&name).json()["size"],
            json!(bytes.len().to_string())
        );
    }
}

#[test]
fn json_object_head_preserves_selection_conditions_and_authorization() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    let object = server.put("head", b"payload").json();
    let path = format!(
        "/storage/v1/b/test-bucket/o/head?generation={}",
        object["generation"].as_str().unwrap()
    );
    let metadata = server.request("GET", &path, "Authorization: Bearer admin\r\n", b"");
    let head = server.request("HEAD", &path, "Authorization: Bearer admin\r\n", b"");
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(head.headers["etag"], metadata.headers["etag"]);
    assert_eq!(
        head.headers["content-length"],
        metadata.body.len().to_string()
    );
    for (path, headers, status) in [
        (path.as_str(), "Authorization: Bearer creator\r\n", 403),
        (
            "/storage/v1/b/test-bucket/o/missing",
            "Authorization: Bearer admin\r\n",
            404,
        ),
        (
            "/storage/v1/b/test-bucket/o/head?ifGenerationMatch=0",
            "Authorization: Bearer admin\r\n",
            412,
        ),
    ] {
        let response = server.request("HEAD", path, headers, b"");
        assert_eq!(response.status, status);
        assert!(response.body.is_empty());
    }
    assert_eq!(server.get("head").json(), object);
}

/// Cloud Storage accepts metadata that declares gzip over a plain body, but a read that
/// would gunzip it fails before any body byte: 400, `text/plain`, `Bad Request`, with the
/// headers of the gunzipped response (observed 2026-10-31). A client accepting gzip still
/// receives the stored bytes.
#[test]
fn gunzipping_a_plain_body_fails_400_before_the_headers_are_sent() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    gzip_object(&server, "plain", b"hello gzip transcoding world\n");
    for (path, extra) in [
        ("/storage/v1/b/test-bucket/o/plain?alt=media", ""),
        (
            "/storage/v1/b/test-bucket/o/plain?alt=media",
            "Accept-Encoding: identity\r\n",
        ),
        ("/test-bucket/plain", ""),
        ("/test-bucket/plain", "Accept-Encoding: identity\r\n"),
    ] {
        let reply = server.request("GET", path, &format!("{ADMIN}{extra}"), b"");
        assert_eq!(reply.status, 400, "{path} {extra}");
        assert_eq!(reply.body, b"Bad Request", "{path} {extra}");
        assert_eq!(reply.headers["content-type"], "text/plain; charset=utf-8");
        assert_eq!(reply.headers["content-length"], "11");
        assert_eq!(
            reply.headers["x-guploader-response-body-transformations"],
            "gunzipped"
        );
        assert_eq!(reply.headers["warning"], "214 UploadServer gunzipped");
        assert_eq!(reply.headers["x-goog-stored-content-encoding"], "gzip");
        assert_eq!(reply.headers["x-goog-stored-content-length"], "29");
        assert!(reply.headers.contains_key("x-goog-hash"), "{path}");
        assert!(!reply.headers.contains_key("content-encoding"), "{path}");
        assert!(!reply.headers.contains_key("accept-ranges"), "{path}");
        assert!(reply.headers["etag"].starts_with("W/"), "{path}");
        // The two APIs differ in cache, pragma and vary fields (observed 2026-10-31).
        if path.starts_with("/storage/") {
            assert_eq!(
                reply.headers["cache-control"],
                "no-cache, no-store, max-age=0, must-revalidate"
            );
            assert_eq!(reply.headers["pragma"], "no-cache");
            assert_eq!(reply.headers["vary"], "Origin, X-Origin");
        } else {
            assert_eq!(reply.headers["cache-control"], "private, max-age=0");
            assert_eq!(reply.headers["vary"], "Accept-Encoding");
            assert!(!reply.headers.contains_key("pragma"), "{path}");
        }
    }
    // HEAD reads no bytes, so it cannot see the mismatch: 200 with the gunzipped headers and
    // a weak tag (observed 2026-10-31).
    for path in [
        "/storage/v1/b/test-bucket/o/plain?alt=media",
        "/test-bucket/plain",
    ] {
        let reply = server.request(
            "HEAD",
            path,
            &format!("{ADMIN}Accept-Encoding: identity\r\n"),
            b"",
        );
        assert_eq!(reply.status, 200, "{path}");
        assert!(reply.body.is_empty(), "{path}");
        assert_eq!(
            reply.headers["x-guploader-response-body-transformations"], "gunzipped",
            "{path}"
        );
        assert!(reply.headers["etag"].starts_with("W/"), "{path}");
        assert!(!reply.headers.contains_key("content-length"), "{path}");
    }
    for path in [
        "/storage/v1/b/test-bucket/o/plain?alt=media",
        "/test-bucket/plain",
    ] {
        let reply = server.request(
            "GET",
            path,
            &format!("{ADMIN}Accept-Encoding: gzip\r\n"),
            b"",
        );
        assert_eq!(reply.status, 200, "{path}");
        assert_eq!(reply.body, b"hello gzip transcoding world\n");
        assert_eq!(reply.headers["content-encoding"], "gzip");
        assert_eq!(reply.headers["content-length"], "29");
    }
}

/// Cloud Storage answers a HEAD with a satisfiable Range on both APIs with 206 and
/// `content-range` (observed 2026-10-31). An identity representation states the range
/// length; a stored-gzip object sent compressed states no length; an unsatisfiable range is 416.
#[test]
fn head_with_range_answers_206_and_content_range_on_json_and_xml() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("plain", b"0123456789abcdef").json();
    let compressed = gzip(b"hello gzip transcoding world\n");
    gzip_object(&server, "gzip", &compressed);
    let stored = compressed.len();
    let json = |object: &str| format!("/storage/v1/b/test-bucket/o/{object}?alt=media");
    let xml = |object: &str| format!("/test-bucket/{object}");
    let gzip_ok = "Accept-Encoding: gzip\r\nRange: bytes=0-4\r\n";
    for (path, extra, content_range, length) in [
        (
            json("plain"),
            "Range: bytes=0-4\r\n",
            "bytes 0-4/16",
            Some("5"),
        ),
        (
            xml("plain"),
            "Range: bytes=0-4\r\n",
            "bytes 0-4/16",
            Some("5"),
        ),
        (
            json("plain"),
            "Range: bytes=-4\r\n",
            "bytes 12-15/16",
            Some("4"),
        ),
        (
            xml("plain"),
            "Range: bytes=12-\r\n",
            "bytes 12-15/16",
            Some("4"),
        ),
        (json("gzip"), gzip_ok, &format!("bytes 0-4/{stored}"), None),
        (xml("gzip"), gzip_ok, &format!("bytes 0-4/{stored}"), None),
    ] {
        let headers = format!("{ADMIN}{extra}");
        let head = server.request("HEAD", &path, &headers, b"");
        assert_eq!(head.status, 206, "{path} {extra}");
        assert!(head.body.is_empty(), "{path}");
        assert_eq!(
            head.headers["content-range"], content_range,
            "{path} {extra}"
        );
        assert_eq!(
            head.headers.get("content-length").map(String::as_str),
            length,
            "{path} {extra}"
        );
        // JSON stored-gzip GET streams chunked; other representations keep their length.
        let get = server.request("GET", &path, &headers, b"");
        assert_eq!(get.status, 206, "{path} {extra}");
        assert_eq!(
            get.headers["content-range"], content_range,
            "{path} {extra}"
        );
        if path.starts_with("/storage/") && path.contains("/gzip?") {
            assert!(!get.headers.contains_key("content-length"));
            assert_eq!(get.headers["transfer-encoding"], "chunked");
        } else {
            assert_eq!(get.headers["content-length"], get.body.len().to_string());
        }
    }
    for path in [json("plain"), xml("plain"), json("gzip"), xml("gzip")] {
        let head = server.request(
            "HEAD",
            &path,
            &format!("{ADMIN}Accept-Encoding: gzip\r\nRange: bytes=100-200\r\n"),
            b"",
        );
        assert_eq!(head.status, 416, "{path}");
        assert!(head.body.is_empty(), "{path}");
        if path.starts_with("/storage/") {
            assert!(
                head.headers["content-range"].starts_with("bytes */"),
                "{path}"
            );
            // The JSON HEAD 416 states no length (observed 2026-10-31).
            assert!(!head.headers.contains_key("content-length"), "{path}");
        } else {
            assert!(!head.headers.contains_key("content-range"), "{path}");
        }
    }
}

/// The XML API answers an unsatisfiable Range with the object's own headers (tag,
/// generation, hashes, `accept-ranges`) and an `InvalidRange` document whose `Details`
/// echoes the request's Range value; HEAD states that document's length (observed
/// 2026-10-31). Unlike the JSON API it sends no `content-range`.
#[test]
fn xml_unsatisfiable_range_carries_object_headers_and_echoes_the_range() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("plain", b"0123456789abcdef").json();
    let extra = format!("{ADMIN}Range: bytes=100-200\r\n");
    let body = "<?xml version='1.0' encoding='UTF-8'?><Error><Code>InvalidRange</Code>\
<Message>The requested range cannot be satisfied.</Message>\
<Details>bytes=100-200</Details></Error>";
    let get = server.request("GET", "/test-bucket/plain", &extra, b"");
    assert_eq!(get.status, 416);
    assert_eq!(get.body, body.as_bytes());
    let head = server.request("HEAD", "/test-bucket/plain", &extra, b"");
    assert_eq!(head.status, 416);
    assert!(head.body.is_empty());
    for reply in [&get, &head] {
        assert_eq!(
            reply.headers["content-type"],
            "application/xml; charset=UTF-8"
        );
        assert_eq!(reply.headers["content-length"], body.len().to_string());
        assert_eq!(reply.headers["accept-ranges"], "bytes");
        assert!(!reply.headers.contains_key("content-range"));
        for name in [
            "etag",
            "x-goog-generation",
            "x-goog-metageneration",
            "x-goog-hash",
            "x-goog-stored-content-length",
        ] {
            assert!(reply.headers.contains_key(name), "{name}");
        }
    }
}

/// Cloud Storage's JSON media responses carry `last-modified`, the storage class,
/// `pragma: no-cache` and `content-disposition: attachment` unless the object sets its own
/// disposition; the XML API has the first two but neither of the others (observed 2026-10-31).
#[test]
fn media_responses_carry_the_provider_object_headers() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("plain", b"0123456789abcdef").json();
    gzip_object(&server, "gzip", &gzip(b"hello gzip transcoding world\n"));
    server.put("named", b"named").json();
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/named",
            ADMIN,
            br#"{"contentDisposition":"inline; filename=\"n.txt\""}"#,
        )
        .json();
    let http_date =
        |value: &str| httpdate::parse_http_date(value).is_ok() && value.ends_with("GMT");
    for method in ["GET", "HEAD"] {
        for (object, extra) in [
            ("plain", ""),
            ("plain", "Range: bytes=0-4\r\n"),
            ("gzip", ""),
            ("gzip", "Accept-Encoding: gzip\r\n"),
        ] {
            // gzip_object patches the encoding, which advances the metageneration.
            let metageneration = if object == "gzip" { "2" } else { "1" };
            let path = format!("/storage/v1/b/test-bucket/o/{object}?alt=media");
            let reply = server.request(method, &path, &format!("{ADMIN}{extra}"), b"");
            let label = format!("JSON {method} {object} {extra}");
            assert!(http_date(&reply.headers["last-modified"]), "{label}");
            assert_eq!(
                reply.headers["x-goog-metageneration"], metageneration,
                "{label}"
            );
            assert_eq!(reply.headers["x-goog-storage-class"], "STANDARD", "{label}");
            assert_eq!(reply.headers["pragma"], "no-cache", "{label}");
            assert_eq!(
                reply.headers["content-disposition"], "attachment",
                "{label}"
            );
            let xml = server.request(
                method,
                &format!("/test-bucket/{object}"),
                &format!("{ADMIN}{extra}"),
                b"",
            );
            let label = format!("XML {method} {object} {extra}");
            assert!(http_date(&xml.headers["last-modified"]), "{label}");
            assert_eq!(
                xml.headers["x-goog-metageneration"], metageneration,
                "{label}"
            );
            assert_eq!(xml.headers["x-goog-storage-class"], "STANDARD", "{label}");
            assert!(!xml.headers.contains_key("pragma"), "{label}");
            assert!(!xml.headers.contains_key("content-disposition"), "{label}");
            assert_eq!(
                reply.headers["last-modified"], xml.headers["last-modified"],
                "{label}"
            );
        }
        // The object's own disposition wins over the JSON default.
        let named = server.request(
            method,
            "/storage/v1/b/test-bucket/o/named?alt=media",
            ADMIN,
            b"",
        );
        assert_eq!(
            named.headers["content-disposition"],
            "inline; filename=\"n.txt\""
        );
        assert_eq!(named.headers["pragma"], "no-cache");
    }
}
