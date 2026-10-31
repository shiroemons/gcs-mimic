//! Local wire fixtures sign independently of the production chunk decoder.

use super::*;

const MARKER: &str = "STREAMING-AWS4-HMAC-SHA256-PAYLOAD";

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0_u32, |bits, (index, byte)| {
            bits | u32::from(*byte) << (16 - index * 8)
        });
        for index in 0..4 {
            output.push(if index <= chunk.len() {
                ALPHABET[(bits >> (18 - index * 6) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    output
}

fn chain(key: &Key, signed: &Signed, chunks: &[&[u8]]) -> Vec<u8> {
    let date = signed
        .headers
        .lines()
        .find_map(|line| line.strip_prefix("x-amz-date: "))
        .unwrap();
    let credential = signed
        .headers
        .split("Credential=")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap();
    let scope = credential.split_once('/').unwrap().1;
    let mut previous = signed
        .headers
        .split("Signature=")
        .nth(1)
        .unwrap()
        .trim()
        .to_owned();
    let empty = hex(digest::digest(&digest::SHA256, b"").as_ref());
    let mut body = Vec::new();
    for bytes in chunks
        .iter()
        .copied()
        .chain(std::iter::once(b"".as_slice()))
    {
        let message = format!(
            "AWS4-HMAC-SHA256-PAYLOAD\n{date}\n{scope}\n{previous}\n{empty}\n{}",
            hex(digest::digest(&digest::SHA256, bytes).as_ref())
        );
        previous = signature(key, true, scope, &message);
        body.extend_from_slice(
            format!("{:x};chunk-signature={previous}\r\n", bytes.len()).as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body
}

fn headers(key: &Key, path: &str, decoded: u64, encoding: Option<&str>) -> Signed {
    let length = decoded.to_string();
    let mut extras = vec![
        ("x-amz-content-sha256", MARKER),
        ("x-amz-decoded-content-length", length.as_str()),
    ];
    if let Some(encoding) = encoding {
        extras.push(("content-encoding", encoding));
    }
    sign(key, "PUT", path, true, true, &extras, SystemTime::now())
}

fn send(
    server: &Server,
    signed: &Signed,
    body: &[u8],
    pieces: Option<usize>,
    trailer: bool,
) -> Reply {
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let framing = if pieces.is_some() {
        "Transfer-Encoding: chunked\r\n".into()
    } else {
        format!("Content-Length: {}\r\n", body.len())
    };
    write!(
        stream,
        "PUT {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{framing}{}\r\n",
        signed.path, signed.headers
    )
    .unwrap();
    if let Some(size) = pieces {
        for part in body.chunks(size) {
            write!(stream, "{:x}\r\n", part.len()).unwrap();
            stream.write_all(part).unwrap();
            stream.write_all(b"\r\n").unwrap();
        }
        stream
            .write_all(if trailer {
                b"0\r\nother: unexpected\r\n\r\n"
            } else {
                b"0\r\n\r\n"
            })
            .unwrap();
    } else {
        stream.write_all(body).unwrap();
    }
    read_reply(stream)
}

fn read_reply(mut stream: TcpStream) -> Reply {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let boundary = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap();
    let head = std::str::from_utf8(&bytes[..boundary]).unwrap();
    Reply {
        status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
        headers: head
            .lines()
            .skip(1)
            .map(|line| {
                let (name, value) = line.split_once(':').unwrap();
                (name.to_ascii_lowercase(), value.trim().into())
            })
            .collect(),
        body: bytes[boundary + 4..].to_vec(),
    }
}

#[test]
fn signed_chunks_transfer_binary_and_empty_objects_over_content_length_and_transfer_encoding() {
    for backend in ["disk", "memory"] {
        let dir = TestDir::new();
        let server = Server::start(&dir, backend);
        server.bucket("test-bucket");
        let key = server.key("signer@example.invalid");
        let bytes = (0..200_000)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        for pieces in [None, Some(1), Some(65536), Some(usize::MAX)] {
            let signed = headers(
                &key,
                "/test-bucket/stream%252Fraw",
                bytes.len() as u64,
                Some("aws-chunked"),
            );
            let body = chain(&key, &signed, &[&bytes[..65536], &bytes[65536..]]);
            assert_eq!(send(&server, &signed, &body, pieces, false).status, 200);
            let reply = server.request("GET", "/test-bucket/stream%252Fraw", ADMIN, b"");
            assert_eq!(reply.body, bytes);
            assert!(!reply.headers.contains_key("content-encoding"));
        }
        for pieces in [None, Some(1)] {
            let signed = headers(&key, "/test-bucket/empty", 0, Some("aws-chunked"));
            assert_eq!(
                send(&server, &signed, &chain(&key, &signed, &[]), pieces, false).status,
                200
            );
            assert!(
                server
                    .request("GET", "/test-bucket/empty", ADMIN, b"")
                    .body
                    .is_empty()
            );
        }
        let signed = headers(&key, "/test-bucket/no-ce", 3, None);
        assert_eq!(
            send(
                &server,
                &signed,
                &chain(&key, &signed, &[b"abc"]),
                Some(1),
                false
            )
            .status,
            200
        );
        assert_eq!(
            server.request("GET", "/test-bucket/no-ce", ADMIN, b"").body,
            b"abc"
        );
    }
}

#[test]
fn google_transfer_encoding_without_aws_content_encoding_uses_full_hash_or_unsigned_decoded_body() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    let bytes = b"HTTP chunk framing alone is decoded by the transport";
    let hash = hex(digest::digest(&digest::SHA256, bytes).as_ref());
    for payload in ["UNSIGNED-PAYLOAD", hash.as_str()] {
        let signed = sign(
            &key,
            "PUT",
            "/test-bucket/plain",
            true,
            true,
            &[
                ("x-amz-decoded-content-length", &bytes.len().to_string()),
                ("x-amz-content-sha256", payload),
            ],
            SystemTime::now(),
        );
        assert_eq!(send(&server, &signed, bytes, Some(3), false).status, 200);
        assert_eq!(
            server.request("GET", "/test-bucket/plain", ADMIN, b"").body,
            bytes
        );
    }
}

#[test]
fn signed_chunk_tampering_lengths_terminal_eof_and_trailers_preserve_the_complete_old_resource() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    server.request("PUT", "/test-bucket/object", ADMIN, b"old");
    let before = server
        .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
        .json();
    let list = server
        .request("GET", "/storage/v1/b/test-bucket/o", ADMIN, b"")
        .json();
    let bytes = vec![b'a'; 9000];
    let signed = headers(&key, "/test-bucket/object", 9000, Some("aws-chunked"));
    let body = chain(&key, &signed, &[&bytes[..8192], &bytes[8192..]]);
    let mut tampered = body.clone();
    let data = body.windows(2).position(|bytes| bytes == b"\r\n").unwrap() + 2;
    tampered[data] = b'b';
    let mut extra = body.clone();
    extra.push(b'x');
    let mut signature = body.clone();
    signature[22] = if signature[22] == b'0' { b'1' } else { b'0' };
    let mut terminal_signature = body.clone();
    let terminal = body
        .windows(18)
        .rposition(|part| part == b"0;chunk-signature=")
        .unwrap()
        + 18;
    terminal_signature[terminal] = if terminal_signature[terminal] == b'0' {
        b'1'
    } else {
        b'0'
    };
    for (wire, trailer) in [
        (tampered, false),
        (signature, false),
        (terminal_signature, false),
        (body[..body.len() - 1].to_vec(), false),
        (body[..body.len() - 86].to_vec(), false),
        (extra, false),
        (body.clone(), true),
    ] {
        let reply = send(&server, &signed, &wire, Some(1024), trailer);
        assert!(
            reply.status == 400 || reply.status == 403,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
                .json(),
            before
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/test-bucket/o", ADMIN, b"")
                .json(),
            list
        );
        assert_eq!(
            server
                .request("GET", "/test-bucket/object", ADMIN, b"")
                .body,
            b"old"
        );
    }
    for length in [8999, 9001] {
        let signed = headers(&key, "/test-bucket/object", length, Some("aws-chunked"));
        assert_eq!(
            send(
                &server,
                &signed,
                &chain(&key, &signed, &[&bytes]),
                None,
                false
            )
            .status,
            400
        );
    }
    let mut truncated = TcpStream::connect(&server.address).unwrap();
    truncated
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    write!(
        truncated,
        "PUT {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n{}\r\n",
        signed.path,
        body.len() + 1,
        signed.headers
    )
    .unwrap();
    truncated.write_all(&body).unwrap();
    truncated.shutdown(std::net::Shutdown::Write).unwrap();
    error(&read_reply(truncated), 400, "IncompleteBody");
    server.stop();
    drop(server);
    let server = Server::start(&dir, "disk");
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
            .json(),
        before
    );
    assert_eq!(
        server
            .request("GET", "/test-bucket/object", ADMIN, b"")
            .body,
        b"old"
    );
    assert_eq!(
        std::fs::read_dir(dir.0.join("data/staging"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn signed_chunks_validate_decoded_md5_and_remove_only_the_aws_encoding_token() {
    use md5::{Digest, Md5};
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(b"object bytes stay compressed").unwrap();
    let bytes = gzip.finish().unwrap();
    let md5 = base64(&Md5::digest(&bytes));
    let length = bytes.len().to_string();
    let signed = sign(
        &key,
        "PUT",
        "/test-bucket/encoding",
        true,
        true,
        &[
            ("x-amz-content-sha256", MARKER),
            ("x-amz-decoded-content-length", &length),
            ("content-encoding", "aws-chunked, gzip"),
            ("content-md5", &md5),
        ],
        SystemTime::now(),
    );
    assert_eq!(
        send(
            &server,
            &signed,
            &chain(&key, &signed, &[&bytes]),
            None,
            false
        )
        .status,
        200
    );
    let reply = server.request(
        "GET",
        "/test-bucket/encoding",
        "Authorization: Bearer admin\r\nAccept-Encoding: gzip\r\n",
        b"",
    );
    assert_eq!(reply.body, bytes);
    assert_eq!(reply.headers["content-encoding"], "gzip");
    let before = server
        .request("GET", "/storage/v1/b/test-bucket/o/encoding", ADMIN, b"")
        .json();
    let wrong = sign(
        &key,
        "PUT",
        "/test-bucket/encoding",
        true,
        true,
        &[
            ("x-amz-content-sha256", MARKER),
            ("x-amz-decoded-content-length", &length),
            ("content-encoding", "aws-chunked"),
            ("content-md5", "AAAAAAAAAAAAAAAAAAAAAA=="),
        ],
        SystemTime::now(),
    );
    error(
        &send(
            &server,
            &wrong,
            &chain(&key, &wrong, &[&bytes]),
            None,
            false,
        ),
        400,
        "BadDigest",
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/encoding", ADMIN, b"")
            .json(),
        before
    );
}

#[test]
fn signed_chunks_enforce_decoded_object_limit_and_reject_malformed_header_and_chunk_resources() {
    let dir = TestDir::new();
    let server = Server::start_with_limit(&dir, "disk", 8192);
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    let bytes = vec![b'x'; 8192];
    let good = headers(&key, "/test-bucket/limit", 8192, Some("aws-chunked"));
    assert_eq!(
        send(&server, &good, &chain(&key, &good, &[&bytes]), None, false).status,
        200
    );
    let before = server
        .request("GET", "/storage/v1/b/test-bucket/o/limit", ADMIN, b"")
        .json();
    let too_large = headers(&key, "/test-bucket/limit", 8193, Some("aws-chunked"));
    error(
        &send(
            &server,
            &too_large,
            &chain(&key, &too_large, &[&vec![b'x'; 8193]]),
            None,
            false,
        ),
        400,
        "EntityTooLarge",
    );
    let missing = Signed {
        path: good.path.clone(),
        headers: good
            .headers
            .lines()
            .filter(|line| !line.starts_with("x-amz-decoded-content-length:"))
            .map(|line| format!("{line}\r\n"))
            .collect(),
    };
    let mut invalid = Vec::new();
    invalid.push(Signed {
        path: good.path.clone(),
        headers: format!("{}x-amz-decoded-content-length: 8192\r\n", good.headers),
    });
    invalid.push(Signed {
        path: good.path.clone(),
        headers: good.headers.replace(
            "x-amz-decoded-content-length: 8192",
            "x-amz-decoded-content-length: 18446744073709551616",
        ),
    });
    invalid.push(Signed {
        path: good.path.clone(),
        headers: format!("{}x-goog-meta-extra: forbidden\r\n", good.headers),
    });
    invalid.push(Signed {
        path: format!("{}?X-Amz-Signature=00", good.path),
        headers: good.headers.clone(),
    });
    invalid.push(missing);
    for signed in invalid {
        error(
            &send(&server, &signed, &[], None, false),
            400,
            "InvalidAuthentication",
        );
    }
    for body in [
        chain(&key, &good, &[&bytes[..1], &bytes[1..]]),
        format!("{}\r\n", "1".repeat(100)).into_bytes(),
        b"10000000000000000;chunk-signature=0000000000000000000000000000000000000000000000000000000000000000\r\n".to_vec(),
    ] {
        error(&send(&server, &good, &body, None, false), 400, "InvalidArgument");
    }
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/limit", ADMIN, b"")
            .json(),
        before
    );
    assert_eq!(
        server.request("GET", "/test-bucket/limit", ADMIN, b"").body,
        bytes
    );
}

#[test]
fn signed_chunks_use_current_keys_and_principal_permissions_with_creator_overwrite_and_pap_ubla() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let creator = server.key("creator@example.invalid");
    let signed = headers(&creator, "/test-bucket/creator", 3, Some("aws-chunked"));
    let body = chain(&creator, &signed, &[b"new"]);
    assert_eq!(send(&server, &signed, &body, None, false).status, 200);
    error(
        &send(&server, &signed, &body, None, false),
        403,
        "AccessDenied",
    );
    for email in [
        "reader@example.invalid",
        "keys@example.invalid",
        "stranger@example.invalid",
        "unknown@example.invalid",
    ] {
        let key = server.key(email);
        let signed = headers(&key, "/test-bucket/creator", 3, Some("aws-chunked"));
        error(
            &send(
                &server,
                &signed,
                &chain(&key, &signed, &[b"bad"]),
                None,
                false,
            ),
            403,
            if email.starts_with("unknown") {
                "InvalidAccessKeyId"
            } else {
                "AccessDenied"
            },
        );
    }
    server.request("PATCH", "/storage/v1/b/test-bucket", ADMIN, br#"{"iamConfiguration":{"publicAccessPrevention":"enforced","uniformBucketLevelAccess":{"enabled":true}}}"#).json();
    let key = server.key("scoped@example.invalid");
    let signed = headers(&key, "/test-bucket/scoped", 3, Some("aws-chunked"));
    let body = chain(&key, &signed, &[b"yes"]);
    error(
        &send(&server, &signed, &body, None, false),
        403,
        "AccessDenied",
    );
    server.request("PUT", "/storage/v1/b/test-bucket/iam", ADMIN, br#"{"bindings":[{"role":"roles/storage.objectAdmin","members":["serviceAccount:scoped@example.invalid"]}]}"#).json();
    assert_eq!(send(&server, &signed, &body, None, false).status, 200);
    let before = server
        .request("GET", "/storage/v1/b/test-bucket/o/scoped", ADMIN, b"")
        .json();
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            br#"{"bindings":[]}"#,
        )
        .json();
    error(
        &send(&server, &signed, &body, None, false),
        403,
        "AccessDenied",
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/scoped", ADMIN, b"")
            .json(),
        before
    );
    server.request("PUT", "/storage/v1/b/test-bucket/iam", ADMIN, br#"{"bindings":[{"role":"roles/storage.objectAdmin","members":["serviceAccount:scoped@example.invalid"]}]}"#).json();
    let path = format!("{KEYS}/{}", key.id);
    let admin = "Authorization: Bearer keys\r\n";
    server
        .request("PUT", &path, admin, br#"{"state":"INACTIVE"}"#)
        .json();
    error(
        &send(&server, &signed, &body, None, false),
        403,
        "InvalidAccessKeyId",
    );
    server
        .request("PUT", &path, admin, br#"{"state":"ACTIVE"}"#)
        .json();
    server.stop();
    drop(server);
    let server = Server::start(&dir, "disk");
    assert_eq!(send(&server, &signed, &body, None, false).status, 200);
    server
        .request("PUT", &path, admin, br#"{"state":"INACTIVE"}"#)
        .json();
    assert_eq!(server.request("DELETE", &path, admin, b"").status, 204);
    error(
        &send(&server, &signed, &body, None, false),
        403,
        "InvalidAccessKeyId",
    );
    assert_eq!(
        server
            .request("GET", "/test-bucket/scoped", ADMIN, b"")
            .body,
        b"yes"
    );
    let after = server
        .request("GET", "/storage/v1/b/test-bucket/o/scoped", ADMIN, b"")
        .json();
    assert_ne!(after["generation"], before["generation"]);
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            br#"{"bindings":[]}"#,
        )
        .json();
    error(
        &send(&server, &signed, &body, None, false),
        403,
        "InvalidAccessKeyId",
    );
}

#[test]
fn invalid_streaming_seed_and_non_object_routes_fail_before_continue_without_public_fallback() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectAdmin","members":["allUsers"]}]}"#,
        )
        .json();
    for path in [
        "/test-bucket/object",
        "/test-bucket/object?uploadId=missing&partNumber=1",
        "/test-bucket/new-bucket",
    ] {
        let good = headers(&key, path, 3, Some("aws-chunked"));
        let mut bad = good.headers.clone();
        let signature = bad.find("Signature=").unwrap() + "Signature=".len();
        bad.replace_range(signature..signature + 64, &"0".repeat(64));
        error(&server.raw(&format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 200\r\nExpect: 100-continue\r\n{bad}\r\n"), "PUT"), if path.contains("uploadId") {400} else {403}, if path.contains("uploadId") {"InvalidAuthentication"} else {"SignatureDoesNotMatch"});
    }
    let good = headers(
        &key,
        "/test-bucket/object?uploadId=missing&partNumber=1",
        3,
        Some("aws-chunked"),
    );
    error(&server.raw(&format!("PUT {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 200\r\nExpect: 100-continue\r\n{}\r\n",good.path,good.headers),"PUT"),400,"InvalidAuthentication");
    server
        .request("GET", "/storage/v1/b/test-bucket/o", ADMIN, b"")
        .json();
    assert_eq!(
        server
            .request("GET", "/test-bucket/object", ADMIN, b"")
            .status,
        404
    );
}
