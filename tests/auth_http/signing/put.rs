use super::*;

fn headers(values: &[(&str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        headers.insert(
            hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    headers
}

pub(super) fn request(
    server: &Server,
    method: &str,
    url: &str,
    headers: &str,
    body: &[u8],
) -> Reply {
    let uri: Uri = url.parse().unwrap();
    let mut raw = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\n{headers}\r\n",
        uri.path_and_query().unwrap(),
        uri.authority().unwrap(),
        body.len(),
    )
    .into_bytes();
    raw.extend_from_slice(body);
    if method == "HEAD" {
        raw = close_after_response(&raw);
    }
    let mut stream = server.stream();
    stream.write_all(&raw).unwrap();
    Reply::read_body(stream, method == "HEAD")
}

fn early(server: &Server, url: &str, headers: &str) -> Reply {
    let uri: Uri = url.parse().unwrap();
    let mut stream = server.stream();
    stream.write_all(format!(
        "PUT {} HTTP/1.1\r\nHost: {}\r\nContent-Length: 999999\r\nExpect: 100-continue\r\n{headers}\r\n",
        uri.path_and_query().unwrap(), uri.authority().unwrap(),
    ).as_bytes()).unwrap();
    Reply::read(stream)
}

fn metadata(server: &Server) -> Value {
    server
        .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
        .ok()
        .json()
}

#[test]
fn signed_put_streams_private_bytes_metadata_and_signed_headers() {
    let (_config, _data, server) = fixture();
    let values = [
        ("content-type", "text/plain"),
        ("content-md5", "XrY7u+Ae7tCTyyK7j1rNww=="),
        ("x-goog-meta-purpose", "signed upload"),
        ("x-goog-if-generation-match", "0"),
    ];
    let url = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/new",
        "creator@example.invalid",
        SystemTime::now(),
        headers(&values),
    );
    let wire = values
        .iter()
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect::<String>();
    request(&server, "PUT", &url, &wire, b"hello world").ok();
    error(
        &server.get("", "/iam-bucket/new"),
        401,
        "AuthenticationRequired",
    );
    let get = server.get(ADMIN, "/iam-bucket/new");
    assert_eq!(get.ok().body, b"hello world");
    assert_eq!(get.headers["content-type"], "text/plain");
    assert_eq!(get.headers["x-goog-meta-purpose"], "signed upload");
    for (name, value) in values {
        error(
            &early(
                &server,
                &url,
                &wire.replace(&format!("{name}: {value}\r\n"), ""),
            ),
            403,
            "SignatureDoesNotMatch",
        );
        error(
            &early(
                &server,
                &url,
                &wire.replace(
                    &format!("{name}: {value}\r\n"),
                    &format!("{name}: changed\r\n"),
                ),
            ),
            403,
            "SignatureDoesNotMatch",
        );
    }
    error(
        &early(
            &server,
            &url,
            &format!("{wire}x-goog-meta-extra: unsigned\r\n"),
        ),
        400,
        "InvalidAuthentication",
    );
    let unsigned = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/unsigned",
        "creator@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    request(
        &server,
        "PUT",
        &unsigned,
        "Content-Type: text/plain\r\nCache-Control: private\r\nx-extra: allowed\r\n",
        b"extra headers",
    )
    .ok();
}

#[test]
fn signed_put_rejects_credentials_permissions_and_resource_tampering_before_body() {
    let (_config, _data, server) = fixture();
    let before = metadata(&server);
    for (signer, path) in [
        ("viewer@example.invalid", "/iam-bucket/absent"),
        ("creator@example.invalid", "/iam-bucket/object"),
        ("no-roles@example.invalid", "/iam-bucket/absent"),
    ] {
        let url = sign(
            &server,
            &Method::PUT,
            path,
            signer,
            SystemTime::now(),
            HeaderMap::new(),
        );
        error(&early(&server, &url, ""), 403, "AccessDenied");
    }
    let url = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/object",
        "admin@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    for changed in [
        url.replace("/object?", "/other?"),
        url.replace("/iam-bucket/", "/other-bucket/"),
        format!("{url}&generation=1"),
    ] {
        error(&early(&server, &changed, ""), 403, "SignatureDoesNotMatch");
    }
    error(
        &early(
            &server,
            &url,
            "Authorization: GOOG4-RSA-SHA256 unsupported\r\n",
        ),
        400,
        "InvalidAuthentication",
    );
    for method in ["GET", "HEAD", "DELETE"] {
        let reply = request(&server, method, &url, "", b"");
        assert_eq!(reply.status, 403);
        if method == "HEAD" {
            assert!(reply.body.is_empty());
        } else {
            error(&reply, 403, "SignatureDoesNotMatch");
        }
    }
    let get = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "admin@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    error(&early(&server, &get, ""), 403, "SignatureDoesNotMatch");
    let expired = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/object",
        "admin@example.invalid",
        SystemTime::now() - Duration::from_secs(120),
        HeaderMap::new(),
    );
    error(&early(&server, &expired, ""), 400, "ExpiredToken");
    let uri: Uri = url.parse().unwrap();
    let reply = server.raw(format!("PUT {} HTTP/1.1\r\nHost: changed.invalid\r\nContent-Length: 999999\r\nExpect: 100-continue\r\n\r\n", uri.path_and_query().unwrap()).as_bytes());
    error(&reply, 403, "SignatureDoesNotMatch");
    assert_eq!(metadata(&server), before);
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/object").ok().body,
        b"hello world"
    );
    iam::set(&server, ADMIN, &json!({"bindings":[iam::binding("roles/storage.objectCreator", "user:no-roles@example.invalid")]})).ok();
    let bucket_creator = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/by-grant",
        "no-roles@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    request(&server, "PUT", &bucket_creator, "", b"bucket grant").ok();
    error(&early(&server, &bucket_creator, ""), 403, "AccessDenied");
    iam::set(&server, ADMIN, &json!({"bindings":[]})).ok();
    error(&early(&server, &bucket_creator, ""), 403, "AccessDenied");
}

#[test]
fn signed_put_sha256_and_md5_failures_preserve_the_full_previous_resource() {
    let (_config, data, mut server) = fixture();
    let body = vec![b'x'; 512 * 1024 + 3];
    let hash = ring::digest::digest(&ring::digest::SHA256, &body)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    for signed_hash in [false, true] {
        let required = if signed_hash {
            headers(&[("x-goog-content-sha256", &hash)])
        } else {
            HeaderMap::new()
        };
        let url = sign(
            &server,
            &Method::PUT,
            "/iam-bucket/object",
            "admin@example.invalid",
            SystemTime::now(),
            required,
        );
        let wire = format!("x-goog-content-sha256: {hash}\r\n");
        request(&server, "PUT", &url, &wire, &body).ok();
        let before = metadata(&server);
        error(
            &request(&server, "PUT", &url, &wire, b"bad body"),
            400,
            "BadDigest",
        );
        assert_eq!(metadata(&server), before);
        assert_eq!(server.get(ADMIN, "/iam-bucket/object").ok().body, body);
    }
    let url = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/object",
        "admin@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    let before = metadata(&server);
    error(
        &request(
            &server,
            "PUT",
            &url,
            "Content-MD5: XrY7u+Ae7tCTyyK7j1rNww==\r\n",
            b"bad body",
        ),
        400,
        "BadDigest",
    );
    assert_eq!(metadata(&server), before);
    request(
        &server,
        "PUT",
        &url,
        "x-goog-content-sha256: UNSIGNED-PAYLOAD\r\n",
        b"arbitrary",
    )
    .ok();
    server.stop();
    let server = Server::strict(&data, &_config.0.join("identities.json"));
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/object").ok().body,
        b"arbitrary"
    );
}

#[test]
fn signed_put_conditions_are_atomic_and_cors_wraps_success_and_failure() {
    let (_config, _data, server) = fixture();
    let generation = metadata(&server)["generation"].as_str().unwrap().to_owned();
    let url = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/object",
        "admin@example.invalid",
        SystemTime::now(),
        headers(&[("x-goog-if-generation-match", &generation)]),
    );
    let uri: Uri = url.parse().unwrap();
    let mut streams = Vec::new();
    for _ in 0..2 {
        let mut stream = server.stream();
        stream.write_all(format!("PUT {} HTTP/1.1\r\nHost: {}\r\nContent-Length: 3\r\nExpect: 100-continue\r\nx-goog-if-generation-match: {generation}\r\n\r\n", uri.path_and_query().unwrap(), uri.authority().unwrap()).as_bytes()).unwrap();
        let mut interim = [0; 25];
        stream.read_exact(&mut interim).unwrap();
        assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
        streams.push(stream);
    }
    let mut statuses = thread::scope(|scope| {
        streams
            .into_iter()
            .map(|mut stream| {
                scope.spawn(move || {
                    stream.write_all(b"new").unwrap();
                    Reply::read(stream).status
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>()
    });
    statuses.sort();
    assert_eq!(statuses, [200, 412]);
    let before = metadata(&server);
    error(
        &early(
            &server,
            &url,
            &format!("x-goog-if-generation-match: {generation}\r\n"),
        ),
        412,
        "PreconditionFailed",
    );
    assert_eq!(metadata(&server), before);
    server.request("PATCH", "/storage/v1/b/iam-bucket", &format!("{ADMIN}Content-Type: application/json\r\n"), br#"{"cors":[{"origin":["https://app.example.invalid"],"method":["PUT","GET"],"responseHeader":["Content-Type","x-goog-generation"],"maxAgeSeconds":60}]}"#).ok();
    let url = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/object",
        "admin@example.invalid",
        SystemTime::now(),
        headers(&[("content-type", "text/plain")]),
    );
    let preflight = request(
        &server,
        "OPTIONS",
        &url,
        "Origin: https://app.example.invalid\r\nAccess-Control-Request-Method: PUT\r\nAccess-Control-Request-Headers: content-type\r\n",
        b"",
    );
    preflight.ok();
    assert_eq!(
        preflight.headers["access-control-allow-origin"],
        "https://app.example.invalid"
    );
    for (content_type, status) in [("text/plain", 200), ("changed", 403)] {
        let reply = request(
            &server,
            "PUT",
            &url,
            &format!("Origin: https://app.example.invalid\r\nContent-Type: {content_type}\r\n"),
            b"cors",
        );
        assert_eq!(reply.status, status);
        assert_eq!(
            reply.headers["access-control-allow-origin"],
            "https://app.example.invalid"
        );
        assert!(reply.headers["access-control-expose-headers"].contains("x-goog-generation"));
    }
}
