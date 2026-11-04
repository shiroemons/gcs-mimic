use super::put::request;
use super::*;

const RANGE: &str = "x-goog-content-length-range";

fn signed_url(server: &Server, path: &str, range: &str) -> String {
    let mut headers = HeaderMap::new();
    headers.insert(RANGE, range.parse().unwrap());
    sign(
        server,
        &Method::PUT,
        path,
        "admin@example.invalid",
        SystemTime::now(),
        headers,
    )
}

/// A PUT whose body only chunked transfer encoding delimits, so no size is declared up front.
fn chunked(server: &Server, url: &str, headers: &str, body: &[u8]) -> Reply {
    let uri: Uri = url.parse().unwrap();
    let mut raw = format!(
        "PUT {} HTTP/1.1\r\nHost: {}\r\nTransfer-Encoding: chunked\r\n{headers}\r\n",
        uri.path_and_query().unwrap(),
        uri.authority().unwrap(),
    )
    .into_bytes();
    for part in body.chunks(3) {
        raw.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
        raw.extend_from_slice(part);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"0\r\n\r\n");
    let mut stream = server.stream();
    stream.write_all(&raw).unwrap();
    Reply::read_joined(stream)
}

fn absent(server: &Server, name: &str) {
    error(
        &server.get(ADMIN, &format!("/iam-bucket/{name}")),
        404,
        "NoSuchKey",
    );
}

#[test]
fn signed_range_admits_inclusive_sizes_and_rejects_others_before_publication() {
    let (_config, _data, server) = fixture();
    let url = signed_url(&server, "/iam-bucket/direct", "5,5");
    let wire = format!("{RANGE}: 5,5\r\n");
    request(&server, "PUT", &url, &wire, b"hello").success();
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/direct").success().body,
        b"hello"
    );
    // The same signed URL cannot replace the object with a body of another size.
    for (body, code) in [
        (&b"hello!"[..], "EntityTooLarge"),
        (&b"hell"[..], "EntityTooSmall"),
    ] {
        error(&request(&server, "PUT", &url, &wire, body), 400, code);
        error(&chunked(&server, &url, &wire, body), 400, code);
    }
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/direct").success().body,
        b"hello"
    );

    let url = signed_url(&server, "/iam-bucket/bounds", "3,5");
    let wire = format!("{RANGE}: 3,5\r\n");
    for body in [&b"abc"[..], b"abcd", b"abcde"] {
        request(&server, "PUT", &url, &wire, body).success();
        chunked(&server, &url, &wire, body).success();
    }
    let url = signed_url(&server, "/iam-bucket/outside", "3,5");
    for (body, code) in [
        (&b"ab"[..], "EntityTooSmall"),
        (&b"abcdef"[..], "EntityTooLarge"),
        (&[b'x'; 256 * 1024][..], "EntityTooLarge"),
    ] {
        error(&request(&server, "PUT", &url, &wire, body), 400, code);
        error(&chunked(&server, &url, &wire, body), 400, code);
    }
    absent(&server, "outside");
}

#[test]
fn signature_binds_the_range_and_malformed_ranges_fail() {
    let (_config, _data, server) = fixture();
    let url = signed_url(&server, "/iam-bucket/bound", "5,5");
    for wire in ["", "x-goog-content-length-range: 0,100\r\n"] {
        error(
            &request(&server, "PUT", &url, wire, b"hello"),
            403,
            "SignatureDoesNotMatch",
        );
    }
    absent(&server, "bound");
    for range in ["5", "1,", "6,5", "a,b", "-1,5", "1,2,3"] {
        let url = signed_url(&server, "/iam-bucket/malformed", range);
        error(
            &request(
                &server,
                "PUT",
                &url,
                &format!("{RANGE}: {range}\r\n"),
                b"hello",
            ),
            400,
            "ContentLengthRangeInHeaderIsInvalid",
        );
    }
    absent(&server, "malformed");
}
