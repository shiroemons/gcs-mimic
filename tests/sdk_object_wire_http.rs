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
    for extra in [
        "",
        "Accept-Encoding: gzip;q=0\r\n",
        "Range: bytes=999999-\r\n",
    ] {
        let expanded = server.request("GET", path, &format!("{ADMIN}{extra}"), b"");
        assert_eq!(expanded.status, 200);
        assert_eq!(expanded.body, b"firstsecond");
        for name in [
            "content-encoding",
            "content-length",
            "content-range",
            "accept-ranges",
        ] {
            assert!(!expanded.headers.contains_key(name), "{name}");
        }
        for name in [
            "etag",
            "x-goog-hash",
            "x-goog-stored-content-encoding",
            "x-goog-stored-content-length",
        ] {
            assert_eq!(expanded.headers[name], raw.headers[name], "{name}");
        }
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
    assert_eq!(server.request("GET", path, ADMIN, b"").body, compressed);
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
        b"plain".to_vec(),
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
