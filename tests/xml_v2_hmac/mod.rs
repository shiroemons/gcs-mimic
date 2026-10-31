use super::*;

fn send(server: &Server, method: &str, signed: &Signed, body: &[u8], host: &str) -> Reply {
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    write!(
        stream,
        "{method} {} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
        signed.path,
        body.len(),
        signed.headers
    )
    .unwrap();
    stream.write_all(body).unwrap();
    read_response(BufReader::new(stream), method)
}

fn read_response(mut reader: BufReader<TcpStream>, method: &str) -> Reply {
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
        let (name, value) = line.trim_end().split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let mut body = Vec::new();
    if method != "HEAD" {
        if headers
            .get("transfer-encoding")
            .is_some_and(|value| value == "chunked")
        {
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                let size = usize::from_str_radix(line.trim(), 16).unwrap();
                if size == 0 {
                    break;
                }
                let start = body.len();
                body.resize(start + size, 0);
                reader.read_exact(&mut body[start..]).unwrap();
                let mut crlf = [0; 2];
                reader.read_exact(&mut crlf).unwrap();
                assert_eq!(&crlf, b"\r\n");
            }
        } else {
            reader.read_to_end(&mut body).unwrap();
        }
    }
    Reply {
        status,
        headers,
        body,
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0u32, |bits, (index, byte)| {
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

fn query(key: &Key, method: &str, path: &str, extras: &[(&str, &str)]) -> Signed {
    query_resource(key, method, path, path, extras)
}

fn query_resource(
    key: &Key,
    method: &str,
    path: &str,
    resource: &str,
    extras: &[(&str, &str)],
) -> Signed {
    let expires = (SystemTime::now() + Duration::from_secs(3600))
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let value = |name: &str| {
        extras
            .iter()
            .find(|(key, _)| *key == name)
            .map_or("", |(_, value)| *value)
    };
    let extensions = extras
        .iter()
        .filter(|(name, _)| name.starts_with("x-goog-"))
        .map(|(name, value)| (*name, *value))
        .collect::<BTreeMap<_, _>>();
    let canonical = format!(
        "{method}\n{}\n{}\n{expires}\n{}{resource}",
        value("content-md5"),
        value("content-type"),
        extensions
            .iter()
            .map(|(name, value)| format!("{name}:{value}\n"))
            .collect::<String>()
    );
    let signature = base64(
        hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, key.secret.as_bytes()),
            canonical.as_bytes(),
        )
        .as_ref(),
    );
    Signed {
        path: format!(
            "{path}{}GoogleAccessId={}&Expires={expires}&Signature={}",
            if path.contains('?') { '&' } else { '?' },
            encode_query_component(&key.id),
            encode_query_component(&signature)
        ),
        headers: extras
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect(),
    }
}

#[derive(Clone, Copy)]
enum Lane {
    Query,
    Goog1,
    Aws,
}

#[test]
fn legacy_headers_sign_response_overrides_while_google_query_retains_documented_unsigned_overrides()
{
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    server.request("PUT", "/test-bucket/object", ADMIN, b"bytes");
    let key = server.key("signer@example.invalid");
    let before = server
        .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
        .json();
    for kind in ["GOOG1", "AWS"] {
        let request = header(
            &key,
            kind,
            "GET",
            "/test-bucket/object?response-content-type=text%2Fplain",
            "/test-bucket/object?response-content-type=text/plain",
            &[],
        );
        let reply = send(&server, "GET", &request, b"", "localhost");
        assert_eq!(
            reply.status,
            200,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        assert_eq!(reply.headers["content-type"], "text/plain");
        let changed = Signed {
            path: request.path.replace("text%2Fplain", "text%2Fhtml"),
            headers: request.headers,
        };
        error(
            &send(&server, "GET", &changed, b"", "localhost"),
            403,
            "SignatureDoesNotMatch",
        );
        for (raw, value) in [
            ("image/svg+xml", "image/svg+xml"),
            ("image%2Fsvg%2Bxml", "image/svg+xml"),
            ("image%2Fsvg%20xml", "image/svg xml"),
        ] {
            let request = header(
                &key,
                kind,
                "GET",
                &format!("/test-bucket/object?response-content-type={raw}"),
                &format!("/test-bucket/object?response-content-type={value}"),
                &[],
            );
            let reply = send(&server, "GET", &request, b"", "localhost");
            assert_eq!(
                reply.status,
                200,
                "{}",
                String::from_utf8_lossy(&reply.body)
            );
            assert_eq!(reply.headers["content-type"], value);
            assert_eq!(reply.body, b"bytes");
            let changed = Signed {
                path: "/test-bucket/object?response-content-type=tampered".into(),
                headers: request.headers,
            };
            error(
                &send(&server, "GET", &changed, b"", "localhost"),
                403,
                "SignatureDoesNotMatch",
            );
        }
    }
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
            .json(),
        before
    );
    let request = query_resource(
        &key,
        "GET",
        "/test-bucket/object?response-content-type=text%2Fhtml",
        "/test-bucket/object",
        &[],
    );
    let reply = send(&server, "GET", &request, b"", "localhost");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.headers["content-type"], "text/html");
}

#[test]
fn legacy_headers_cannot_retarget_the_unsigned_generation_to_read_or_delete_private_history() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            ADMIN,
            br#"{"versioning":{"enabled":true}}"#,
        )
        .json();
    server.request(
        "PUT",
        "/test-bucket/object",
        ADMIN,
        b"private old generation",
    );
    let old = server
        .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
        .json();
    let generation = old["generation"].as_str().unwrap();
    server.request("PUT", "/test-bucket/object", ADMIN, b"latest generation");
    assert_eq!(
        server
            .request(
                "GET",
                &format!("/test-bucket/object?generation={generation}"),
                ADMIN,
                b""
            )
            .body,
        b"private old generation"
    );
    let before = server
        .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
        .json();
    let versions = server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o?versions=true",
            ADMIN,
            b"",
        )
        .json();
    let key = server.key("signer@example.invalid");
    for kind in ["GOOG1", "AWS"] {
        for method in ["GET", "HEAD", "DELETE"] {
            let mut request = header(
                &key,
                kind,
                method,
                "/test-bucket/object",
                "/test-bucket/object",
                &[],
            );
            request.path.push_str(&format!("?generation={generation}"));
            let reply = send(&server, method, &request, b"", "localhost");
            if method == "HEAD" {
                assert_eq!(reply.status, 400);
                assert!(reply.body.is_empty());
            } else {
                error(&reply, 400, "InvalidAuthentication");
            }
            assert_eq!(
                server
                    .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
                    .json(),
                before
            );
            assert_eq!(
                server
                    .request(
                        "GET",
                        "/storage/v1/b/test-bucket/o?versions=true",
                        ADMIN,
                        b""
                    )
                    .json(),
                versions
            );
        }
    }
    assert_eq!(
        server
            .request("GET", "/test-bucket/object", ADMIN, b"")
            .body,
        b"latest generation"
    );
    assert_eq!(
        server
            .request(
                "GET",
                &format!("/test-bucket/object?generation={generation}"),
                ADMIN,
                b""
            )
            .body,
        b"private old generation"
    );
}

fn signed(lane: Lane, key: &Key, method: &str, path: &str, extras: &[(&str, &str)]) -> Signed {
    match lane {
        Lane::Query => query(key, method, path, extras),
        Lane::Goog1 => header(key, "GOOG1", method, path, path, extras),
        Lane::Aws => header(key, "AWS", method, path, path, extras),
    }
}

#[test]
fn rsa_and_hmac_v2_queries_share_the_literal_virtual_bucket_resource() {
    for backend in ["disk", "memory"] {
        let dir = TestDir::new();
        let server = Server::start(&dir, backend);
        server.bucket("test-bucket");
        let key = server.key("signer@example.invalid");
        for method in ["PUT", "GET", "HEAD", "DELETE"] {
            for rsa in [false, true] {
                if method == "DELETE" && !rsa {
                    continue;
                }
                let path = "/sdk%252Fraw";
                let resource = "/test-bucket/sdk%252Fraw";
                let mut request = query_resource(&key, method, path, resource, &[]);
                if rsa {
                    let expires = request
                        .path
                        .split("Expires=")
                        .nth(1)
                        .unwrap()
                        .split('&')
                        .next()
                        .unwrap();
                    let canonical = format!("{method}\n\n\n{expires}\n{resource}");
                    let hex = gcs_mimic::auth::signature::sign_policy(&canonical, PRIVATE).unwrap();
                    let signature = base64(
                        &(0..hex.len())
                            .step_by(2)
                            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
                            .collect::<Vec<_>>(),
                    );
                    request.path = format!(
                        "{path}?GoogleAccessId=signer%40example.invalid&Expires={expires}&Signature={}",
                        encode_query_component(&signature)
                    );
                }
                let body = if method == "PUT" {
                    b"virtual bytes".as_slice()
                } else {
                    b""
                };
                let reply = send(
                    &server,
                    method,
                    &request,
                    body,
                    "test-bucket.storage.example.invalid:9876",
                );
                assert_eq!(
                    reply.status,
                    if method == "DELETE" { 204 } else { 200 },
                    "{}",
                    String::from_utf8_lossy(&reply.body)
                );
                if method == "GET" {
                    assert_eq!(reply.body, b"virtual bytes");
                }
                if method == "GET" {
                    error(
                        &send(
                            &server,
                            method,
                            &request,
                            b"",
                            "other.storage.example.invalid:9876",
                        ),
                        403,
                        "SignatureDoesNotMatch",
                    );
                    assert_eq!(
                        send(
                            &server,
                            method,
                            &request,
                            b"",
                            "test-bucket.storage.example.invalid:1234"
                        )
                        .body,
                        b"virtual bytes"
                    );
                }
            }
        }
    }
}

#[test]
fn legacy_queries_and_headers_recheck_key_state_restart_and_never_borrow_key_project_grants() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    for lane in [Lane::Query, Lane::Goog1, Lane::Aws] {
        let creator = server.key("creator@example.invalid");
        let put = signed(lane, &creator, "PUT", "/test-bucket/creator", &[]);
        assert_eq!(send(&server, "PUT", &put, b"new", "localhost").status, 200);
        error(
            &send(&server, "PUT", &put, b"no overwrite", "localhost"),
            403,
            "AccessDenied",
        );
        let delete = signed(lane, &key, "DELETE", "/test-bucket/creator", &[]);
        assert_eq!(
            send(&server, "DELETE", &delete, b"", "localhost").status,
            204
        );
        for email in [
            "reader@example.invalid",
            "keys@example.invalid",
            "stranger@example.invalid",
            "unknown@example.invalid",
        ] {
            let denied = server.key(email);
            let put = signed(lane, &denied, "PUT", "/test-bucket/denied", &[]);
            error(
                &send(&server, "PUT", &put, b"denied", "localhost"),
                403,
                if email.starts_with("unknown") {
                    "InvalidAccessKeyId"
                } else {
                    "AccessDenied"
                },
            );
        }
    }
    let put = signed(Lane::Query, &key, "PUT", "/test-bucket/state", &[]);
    assert_eq!(
        send(&server, "PUT", &put, b"durable", "localhost").status,
        200
    );
    let path = format!("{KEYS}/{}", key.id);
    let keys = "Authorization: Bearer keys\r\n";
    server
        .request("PUT", &path, keys, br#"{"state":"INACTIVE"}"#)
        .json();
    for lane in [Lane::Query, Lane::Goog1, Lane::Aws] {
        error(
            &send(
                &server,
                "GET",
                &signed(lane, &key, "GET", "/test-bucket/state", &[]),
                b"",
                "localhost",
            ),
            403,
            "InvalidAccessKeyId",
        );
    }
    server
        .request("PUT", &path, keys, br#"{"state":"ACTIVE"}"#)
        .json();
    server.stop();
    drop(server);
    let server = Server::start(&dir, "disk");
    for lane in [Lane::Query, Lane::Goog1, Lane::Aws] {
        assert_eq!(
            send(
                &server,
                "GET",
                &signed(lane, &key, "GET", "/test-bucket/state", &[]),
                b"",
                "localhost"
            )
            .body,
            b"durable"
        );
    }
    server
        .request("PUT", &path, keys, br#"{"state":"INACTIVE"}"#)
        .json();
    assert_eq!(server.request("DELETE", &path, keys, b"").status, 204);
    for lane in [Lane::Query, Lane::Goog1, Lane::Aws] {
        error(
            &send(
                &server,
                "GET",
                &signed(lane, &key, "GET", "/test-bucket/state", &[]),
                b"",
                "localhost",
            ),
            403,
            "InvalidAccessKeyId",
        );
    }
    assert_eq!(
        server.request("GET", "/test-bucket/state", ADMIN, b"").body,
        b"durable"
    );
}

#[test]
fn legacy_signatures_keep_public_failures_closed_and_preserve_complete_old_resources() {
    use md5::{Digest, Md5};
    let dir = TestDir::new();
    let mut server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    assert_eq!(
        server
            .request("PUT", "/test-bucket/object", ADMIN, b"old")
            .status,
        200
    );
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectAdmin","members":["allUsers"]}]}"#,
        )
        .json();
    let before = server
        .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
        .json();
    let list = server
        .request("GET", "/storage/v1/b/test-bucket/o", ADMIN, b"")
        .json();
    for lane in [Lane::Query, Lane::Goog1, Lane::Aws] {
        let json = signed(lane, &key, "GET", "/storage/v1/b/test-bucket/o/object", &[]);
        send(&server, "GET", &json, b"", "localhost").error(401, "authError");
        let mut unknown = signed(lane, &key, "GET", "/test-bucket/object", &[]);
        unknown.path = unknown.path.replace(&key.id, "unknown-access-key");
        unknown.headers = unknown.headers.replace(&key.id, "unknown-access-key");
        error(
            &send(&server, "GET", &unknown, b"", "localhost"),
            403,
            "InvalidAccessKeyId",
        );
        let mut invalid = signed(lane, &key, "PUT", "/test-bucket/object", &[]);
        match lane {
            Lane::Query => {
                let signature = invalid.path.find("Signature=").unwrap() + 10;
                invalid.path.truncate(signature);
                invalid.path.push_str("AAAAAAAAAAAAAAAAAAAAAAAAAAA%3D");
            }
            Lane::Goog1 | Lane::Aws => {
                let signature = invalid.headers.rfind(':').unwrap() + 1;
                invalid.headers.truncate(signature);
                invalid.headers.push_str("AAAAAAAAAAAAAAAAAAAAAAAAAAA=\r\n");
            }
        }
        error(
            &send(
                &server,
                "PUT",
                &invalid,
                b"invalid public overwrite",
                "localhost",
            ),
            403,
            "SignatureDoesNotMatch",
        );
        let md5 = base64(&Md5::digest(b"expected"));
        let put = signed(
            lane,
            &key,
            "PUT",
            "/test-bucket/object",
            &[
                ("content-md5", &md5),
                ("content-type", "application/octet-stream"),
            ],
        );
        error(
            &send(&server, "PUT", &put, b"wrong body", "localhost"),
            400,
            "BadDigest",
        );
        let truncated = signed(lane, &key, "PUT", "/test-bucket/object", &[]);
        let mut stream = TcpStream::connect(&server.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        write!(stream,"PUT {} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 3\r\n{}Connection: close\r\n\r\nno",truncated.path,truncated.headers).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        error(
            &read_response(BufReader::new(stream), "PUT"),
            400,
            "IncompleteBody",
        );
        let mut duplicate = signed(lane, &key, "GET", "/test-bucket/object", &[]);
        match lane {
            Lane::Query => duplicate.path.push_str("&GoogleAccessId=duplicate"),
            _ => {
                duplicate.headers.push_str(&duplicate.headers.clone());
            }
        }
        error(
            &send(&server, "GET", &duplicate, b"", "localhost"),
            400,
            "InvalidAuthentication",
        );
        let mut mixed = signed(lane, &key, "GET", "/test-bucket/object", &[]);
        match lane {
            Lane::Query => mixed.headers.push_str("Authorization: Bearer admin\r\n"),
            _ => mixed.path.push_str("?X-Goog-Signature=00"),
        }
        error(
            &send(&server, "GET", &mixed, b"", "localhost"),
            400,
            "InvalidAuthentication",
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
fn legacy_registered_signers_use_current_acl_iam_pap_and_ubla_without_implicit_grants() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    server.request("PUT", "/test-bucket/object", ADMIN, b"protected");
    let key = server.key("scoped@example.invalid");
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/object/acl",
            ADMIN,
            br#"{"entity":"user-scoped@example.invalid","role":"READER"}"#,
        )
        .json();
    for lane in [Lane::Query, Lane::Goog1, Lane::Aws] {
        assert_eq!(
            send(
                &server,
                "GET",
                &signed(lane, &key, "GET", "/test-bucket/object", &[]),
                b"",
                "localhost"
            )
            .body,
            b"protected"
        );
        error(
            &send(
                &server,
                "PUT",
                &signed(lane, &key, "PUT", "/test-bucket/denied", &[]),
                b"denied",
                "localhost",
            ),
            403,
            "AccessDenied",
        );
    }
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            ADMIN,
            br#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
        )
        .json();
    for lane in [Lane::Query, Lane::Goog1, Lane::Aws] {
        assert_eq!(
            send(
                &server,
                "GET",
                &signed(lane, &key, "GET", "/test-bucket/object", &[]),
                b"",
                "localhost"
            )
            .body,
            b"protected"
        );
    }
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            ADMIN,
            br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}"#,
        )
        .json();
    for lane in [Lane::Query, Lane::Goog1, Lane::Aws] {
        error(
            &send(
                &server,
                "GET",
                &signed(lane, &key, "GET", "/test-bucket/object", &[]),
                b"",
                "localhost",
            ),
            403,
            "AccessDenied",
        );
    }
    server.request("PUT","/storage/v1/b/test-bucket/iam",ADMIN,br#"{"bindings":[{"role":"roles/storage.objectAdmin","members":["serviceAccount:scoped@example.invalid"]}]}"#).json();
    for lane in [Lane::Query, Lane::Goog1, Lane::Aws] {
        assert_eq!(
            send(
                &server,
                "PUT",
                &signed(lane, &key, "PUT", "/test-bucket/object", &[]),
                b"explicit grant",
                "localhost"
            )
            .status,
            200
        );
    }
    let before = server
        .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
        .json();
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            br#"{"bindings":[]}"#,
        )
        .json();
    for lane in [Lane::Query, Lane::Goog1, Lane::Aws] {
        error(
            &send(
                &server,
                "PUT",
                &signed(lane, &key, "PUT", "/test-bucket/object", &[]),
                b"revoked",
                "localhost",
            ),
            403,
            "AccessDenied",
        );
    }
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
        b"explicit grant"
    );
}

#[test]
fn legacy_bucket_service_operations_and_aws_allowlist_reuse_shared_permissions() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    let key = server.key("signer@example.invalid");
    for (lane, name) in [
        (Lane::Query, "query-bucket"),
        (Lane::Goog1, "legacy-bucket"),
        (Lane::Aws, "aws-bucket"),
    ] {
        let path = format!("/{name}");
        let project = if matches!(lane, Lane::Aws) {
            "x-amz-project-id"
        } else {
            "x-goog-project-id"
        };
        let created = send(
            &server,
            "PUT",
            &signed(lane, &key, "PUT", &path, &[(project, "local")]),
            b"",
            "localhost",
        );
        assert_eq!(
            created.status,
            200,
            "{}",
            String::from_utf8_lossy(&created.body)
        );
        assert_eq!(
            send(
                &server,
                "HEAD",
                &signed(lane, &key, "HEAD", &path, &[]),
                b"",
                "localhost"
            )
            .status,
            200
        );
        let project = if matches!(lane, Lane::Aws) {
            "x-amz-project-id"
        } else {
            "x-goog-project-id"
        };
        let list = send(
            &server,
            "GET",
            &signed(lane, &key, "GET", "/", &[(project, "local")]),
            b"",
            "localhost",
        );
        assert_eq!(list.status, 200, "{}", String::from_utf8_lossy(&list.body));
        assert!(String::from_utf8(list.body).unwrap().contains(name));
        assert_eq!(
            send(
                &server,
                "DELETE",
                &signed(lane, &key, "DELETE", &path, &[]),
                b"",
                "localhost"
            )
            .status,
            204
        );
    }
    server.bucket("test-bucket");
    for field in [
        "x-amz-acl",
        "x-amz-server-side-encryption",
        "x-amz-storage-class",
    ] {
        let put = signed(
            Lane::Aws,
            &key,
            "PUT",
            "/test-bucket/no-silent-alias",
            &[(field, "private")],
        );
        error(
            &send(&server, "PUT", &put, b"must not publish", "localhost"),
            400,
            "InvalidArgument",
        );
    }
    let collision = header(
        &key,
        "AWS",
        "PUT",
        "/test-bucket/no-silent-alias",
        "/test-bucket/no-silent-alias",
        &[("x-amz-meta-k", "a"), ("x-goog-meta-k", "b")],
    );
    error(
        &send(&server, "PUT", &collision, b"must not publish", "localhost"),
        400,
        "InvalidAuthentication",
    );
    assert_eq!(
        server
            .request("GET", "/test-bucket/no-silent-alias", ADMIN, b"")
            .status,
        404
    );
}

fn header(
    key: &Key,
    kind: &str,
    method: &str,
    path: &str,
    resource: &str,
    extras: &[(&str, &str)],
) -> Signed {
    let date = httpdate::fmt_http_date(SystemTime::now());
    let value = |name: &str| {
        extras
            .iter()
            .find(|(key, _)| *key == name)
            .map_or("", |(_, value)| *value)
    };
    let prefix = if kind == "GOOG1" { "x-goog-" } else { "x-amz-" };
    let fields = extras
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, value)| (*name, *value))
        .collect::<BTreeMap<_, _>>();
    let canonical = format!(
        "{method}\n{}\n{}\n{}\n{}{resource}",
        value("content-md5"),
        value("content-type"),
        if fields.contains_key(format!("{prefix}date").as_str()) {
            ""
        } else {
            date.as_str()
        },
        fields
            .iter()
            .map(|(name, value)| format!("{name}:{value}\n"))
            .collect::<String>()
    );
    let signature = base64(
        hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, key.secret.as_bytes()),
            canonical.as_bytes(),
        )
        .as_ref(),
    );
    Signed {
        path: path.into(),
        headers: format!(
            "Date: {date}\r\n{}Authorization: {kind} {}:{signature}\r\n",
            extras
                .iter()
                .map(|(name, value)| format!("{name}: {value}\r\n"))
                .collect::<String>(),
            key.id
        ),
    }
}

#[test]
fn goog1_and_aws_legacy_headers_transfer_metadata_and_use_virtual_bucket_resources() {
    for backend in ["disk", "memory"] {
        let dir = TestDir::new();
        let server = Server::start(&dir, backend);
        server.bucket("test-bucket");
        let key = server.key("signer@example.invalid");
        for kind in ["GOOG1", "AWS"] {
            for virtual_host in [false, true] {
                let path = if virtual_host {
                    "/legacy%252Fname"
                } else {
                    "/test-bucket/legacy%252Fname"
                };
                let resource = "/test-bucket/legacy%252Fname";
                let host = if virtual_host {
                    "test-bucket.storage.example.invalid:9876"
                } else {
                    "localhost"
                };
                let meta = if kind == "GOOG1" {
                    "x-goog-meta-kind"
                } else {
                    "x-amz-meta-kind"
                };
                let put = header(
                    &key,
                    kind,
                    "PUT",
                    path,
                    resource,
                    &[
                        ("content-type", "application/octet-stream"),
                        (meta, "legacy"),
                    ],
                );
                let reply = send(&server, "PUT", &put, b"\0header signed\xff", host);
                assert_eq!(
                    reply.status,
                    200,
                    "{}",
                    String::from_utf8_lossy(&reply.body)
                );
                for method in ["GET", "HEAD"] {
                    let signed = header(&key, kind, method, path, resource, &[]);
                    let reply = send(&server, method, &signed, b"", host);
                    assert_eq!(
                        reply.status,
                        200,
                        "{}",
                        String::from_utf8_lossy(&reply.body)
                    );
                    if method == "GET" {
                        assert_eq!(reply.body, b"\0header signed\xff");
                    } else {
                        assert!(reply.body.is_empty());
                    }
                    assert_eq!(reply.headers["x-goog-meta-kind"], "legacy");
                }
                let delete = header(&key, kind, "DELETE", path, resource, &[]);
                assert_eq!(send(&server, "DELETE", &delete, b"", host).status, 204);
            }
        }
    }
}

#[test]
fn v2_hmac_query_transfers_get_head_put_delete_on_disk_and_ram_without_v4_derivation() {
    for backend in ["disk", "memory"] {
        let dir = TestDir::new();
        let server = Server::start(&dir, backend);
        server.bucket("test-bucket");
        let key = server.key("signer@example.invalid");
        let bytes = b"\0legacy signed binary\xff";
        let put = query(
            &key,
            "PUT",
            "/test-bucket/raw%252Fobject",
            &[
                ("content-type", "application/octet-stream"),
                ("x-goog-meta-mode", "legacy"),
            ],
        );
        let reply = send(&server, "PUT", &put, bytes, "localhost");
        assert_eq!(
            reply.status,
            200,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        for method in ["GET", "HEAD"] {
            let signed = query(&key, method, "/test-bucket/raw%252Fobject", &[]);
            let reply = send(&server, method, &signed, b"", "localhost");
            assert_eq!(reply.status, 200);
            if method == "HEAD" {
                assert!(reply.body.is_empty());
            } else {
                assert_eq!(reply.body, bytes);
            }
            assert_eq!(reply.headers["x-goog-meta-mode"], "legacy");
        }
        let delete = query(&key, "DELETE", "/test-bucket/raw%252Fobject", &[]);
        assert_eq!(
            send(&server, "DELETE", &delete, b"", "localhost").status,
            204
        );
        assert_eq!(
            server
                .request("GET", "/test-bucket/raw%252Fobject", ADMIN, b"")
                .status,
            404
        );
    }
}
