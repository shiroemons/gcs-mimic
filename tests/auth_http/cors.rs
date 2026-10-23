use super::*;

const ORIGIN: &str = "https://browser.example.invalid";

fn cors_request(server: &Server, method: &str, path: &str, auth: &str, body: &[u8]) -> Reply {
    server.request(
        method,
        path,
        &format!("{auth}Origin: {ORIGIN}\r\nContent-Type: application/json\r\n"),
        body,
    )
}

fn assert_cors(reply: &Reply) {
    assert_eq!(reply.headers["access-control-allow-origin"], ORIGIN);
    assert_eq!(reply.headers["access-control-allow-credentials"], "true");
    let exposed = &reply.headers["access-control-expose-headers"];
    for header in ["ETag", "Location", "Range", "Content-Range"] {
        assert!(exposed.split(", ").any(|value| value == header));
    }
    assert!(reply.headers["vary"].contains("Origin"));
}

#[test]
fn json_cors_preflight_defaults_never_authenticate_or_grant_access() {
    let (_config, _data, server) = iam::fixture();
    for path in [
        "/storage/v1/b?project=project-a",
        "/storage/v1/b/iam-bucket",
        "/storage/v1/b/missing/o/object?alt=media",
        "/upload/storage/v1/b/iam-bucket/o?uploadType=media&name=item",
        "/upload/storage/v1/b/iam-bucket/o?uploadType=resumable&upload_id=missing",
    ] {
        for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "TRACE"] {
            let reply = server.request("OPTIONS", path, &format!("Origin: {ORIGIN}\r\nAccess-Control-Request-Method: {method}\r\nAccess-Control-Request-Headers: Authorization, Content-Type, X-Custom\r\nAuthorization: Bearer invalid\r\n"), b"");
            assert_eq!(reply.status, 200);
            assert!(reply.body.is_empty());
            assert_eq!(reply.headers["access-control-allow-origin"], ORIGIN);
            assert_eq!(
                reply.headers["access-control-allow-methods"],
                "DELETE, GET, HEAD, PATCH, POST, PUT"
            );
            assert_eq!(
                reply.headers["access-control-allow-headers"],
                "authorization, content-type, x-custom"
            );
            assert_eq!(reply.headers["access-control-max-age"], "3600");
            assert!(
                !reply
                    .headers
                    .contains_key("access-control-allow-credentials")
            );
        }
    }
    for method in ["", "Access-Control-Request-Method: \r\n"] {
        let reply = server.request(
            "OPTIONS",
            "/storage/v1/b/iam-bucket",
            &format!("Origin: {ORIGIN}\r\n{method}"),
            b"",
        );
        assert_eq!(reply.status, 404);
        assert_eq!(reply.headers["access-control-allow-credentials"], "true");
    }
    let invalid = server.request("OPTIONS", "/storage/v1/b/iam-bucket", &format!("Origin: {ORIGIN}\r\nAccess-Control-Request-Method: GET\r\nAccess-Control-Request-Headers: valid, invalid header\r\n"), b"");
    invalid.error(400, "invalidArgument");
    assert!(!invalid.headers.contains_key("access-control-allow-origin"));
    let denied = cors_request(
        &server,
        "POST",
        "/upload/storage/v1/b/iam-bucket/o?uploadType=media&name=denied",
        NO_ROLES,
        b"denied",
    );
    denied.error(403, "forbidden");
    assert_cors(&denied);
    server
        .get(ADMIN, "/storage/v1/b/iam-bucket/o/denied")
        .error(404, "notFound");
}

#[test]
fn json_cors_wraps_bucket_object_streaming_errors_and_validators() {
    let (_config, _data, server) = iam::fixture();
    let bucket = "/storage/v1/b/browser-bucket";
    let object = "/storage/v1/b/browser-bucket/o/item";
    let created = cors_request(
        &server,
        "POST",
        "/storage/v1/b?project=project-a",
        ADMIN,
        br#"{"name":"browser-bucket"}"#,
    );
    assert_eq!(created.status, 200);
    assert_cors(&created);
    for (method, path, body) in [
        ("GET", "/storage/v1/b?project=project-a", &b""[..]),
        ("GET", bucket, &b""[..]),
        ("PATCH", bucket, &br#"{"labels":{"browser":"yes"}}"#[..]),
        ("PUT", bucket, &br#"{}"#[..]),
        (
            "POST",
            "/upload/storage/v1/b/browser-bucket/o?uploadType=media&name=item",
            &b"hello"[..],
        ),
        ("GET", object, &b""[..]),
        (
            "GET",
            "/storage/v1/b/browser-bucket/o/item?alt=media",
            &b""[..],
        ),
        ("GET", "/storage/v1/b/browser-bucket/o", &b""[..]),
        ("PATCH", object, &br#"{"metadata":{"browser":"yes"}}"#[..]),
        ("PUT", object, &br#"{}"#[..]),
    ] {
        let reply = cors_request(&server, method, path, ADMIN, body);
        assert_eq!(reply.status, 200, "{method} {path}: {:?}", reply.body);
        assert_cors(&reply);
    }
    let metadata = cors_request(&server, "GET", object, ADMIN, b"");
    let cached = cors_request(
        &server,
        "GET",
        object,
        &format!("{ADMIN}If-None-Match: {}\r\n", metadata.headers["etag"]),
        b"",
    );
    assert_eq!(cached.status, 304);
    assert!(cached.body.is_empty());
    assert_cors(&cached);
    for (auth, status) in [
        ("", 401),
        (NO_ROLES, 403),
        ("Authorization: Bearer unknown\r\n", 401),
    ] {
        let denied = cors_request(&server, "GET", object, auth, b"");
        assert_eq!(denied.status, status);
        assert_cors(&denied);
    }
    for (path, status) in [(object, 204), (bucket, 204)] {
        let deleted = cors_request(&server, "DELETE", path, ADMIN, b"");
        assert_eq!(deleted.status, status);
        assert_cors(&deleted);
    }
    for path in ["/_internal/health", "/not-an-api", "/storage/v10/b"] {
        let reply = cors_request(&server, "GET", path, "", b"");
        assert!(!reply.headers.contains_key("access-control-allow-origin"));
    }
    let plain = server.get(ADMIN, "/storage/v1/b/iam-bucket");
    assert!(!plain.headers.contains_key("access-control-allow-origin"));
    for origin in [
        "Origin: one\r\nOrigin: two\r\n",
        "Origin: https://one.invalid https://two.invalid\r\n",
        "Origin: \r\n",
        "Origin: malformed\r\n",
        "Origin: https://user@app.example.invalid\r\n",
        "Origin: https://app.example.invalid/path\r\n",
        "Origin: https://app.example.invalid?query\r\n",
    ] {
        let reply = server.request(
            "GET",
            "/storage/v1/b/iam-bucket",
            &format!("{ADMIN}{origin}"),
            b"",
        );
        assert!(!reply.headers.contains_key("access-control-allow-origin"));
    }
    let null = server.request(
        "GET",
        "/storage/v1/b/iam-bucket",
        &format!("{ADMIN}Origin: null\r\n"),
        b"",
    );
    assert_eq!(null.headers["access-control-allow-origin"], "null");
    assert_eq!(plain.headers["vary"], "Origin");
}
