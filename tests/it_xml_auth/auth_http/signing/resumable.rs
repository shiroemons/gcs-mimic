use super::*;
use std::time::Duration;

const START: (&str, &str) = ("x-goog-resumable", "start");
const CHUNK: usize = 256 * 1024;

fn signed_headers(values: &[(&str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        headers.insert(
            hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    headers
}

fn wire(values: &[(&str, &str)]) -> String {
    values
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect()
}

fn sign_start(server: &Server, path: &str, signer: &str, values: &[(&str, &str)]) -> String {
    sign(
        server,
        &Method::POST,
        path,
        signer,
        SystemTime::now(),
        signed_headers(values),
    )
}

/// The session URI points at the advertised API origin; the test listener serves it.
fn session_path(reply: &Reply, object_path: &str) -> String {
    reply.expect(201);
    assert!(reply.body.is_empty());
    let location: Uri = reply.headers["location"].parse().unwrap();
    assert_eq!(location.scheme_str(), Some("http"));
    assert_eq!(location.authority().unwrap().as_str(), "127.0.0.1:4443");
    assert_eq!(location.path(), object_path);
    let id = location
        .query()
        .and_then(|query| query.strip_prefix("upload_id="))
        .unwrap();
    assert_eq!(id.len(), 64);
    assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    location.path_and_query().unwrap().as_str().to_owned()
}

fn chunk(server: &Server, path: &str, range: &str, body: &[u8]) -> Reply {
    server.request("PUT", path, &format!("Content-Range: {range}\r\n"), body)
}

fn absent(server: &Server, path: &str) {
    error(&server.get(ADMIN, path), 404, "NoSuchKey");
}

#[test]
fn signed_resumable_initiation_opens_a_session_that_the_uri_alone_completes() {
    let (_config, _data, server) = fixture();
    let values = [
        START,
        ("content-type", "text/plain"),
        ("x-goog-meta-purpose", "signed resumable"),
        ("x-goog-if-generation-match", "0"),
    ];
    let url = sign_start(
        &server,
        "/iam-bucket/resumed",
        "creator@example.invalid",
        &values,
    );
    let reply = put::request(&server, "POST", &url, &wire(&values), b"");
    assert_eq!(reply.headers["cache-control"], "no-store");
    let path = session_path(&reply, "/iam-bucket/resumed");
    absent(&server, "/iam-bucket/resumed");

    // Later requests carry no credentials: the session URI is the capability.
    let first = vec![b'a'; CHUNK];
    let partial = chunk(&server, &path, "bytes 0-262143/*", &first);
    partial.expect(308);
    assert_eq!(partial.headers["range"], "bytes=0-262143");
    let status = chunk(&server, &path, "bytes */*", b"");
    status.expect(308);
    assert_eq!(status.headers["range"], "bytes=0-262143");
    absent(&server, "/iam-bucket/resumed");

    let done = chunk(&server, &path, "bytes 262144-262148/262149", b"tail!");
    done.expect(200);
    assert!(done.body.is_empty());
    assert!(done.headers["x-goog-hash"].contains("crc32c="));
    assert!(done.headers["x-goog-hash"].contains("md5="));
    assert_eq!(done.headers["x-goog-stored-content-length"], "262149");
    assert_eq!(done.headers["x-goog-metageneration"], "1");
    let generation = done.headers["x-goog-generation"].clone();

    let get = server.get(ADMIN, "/iam-bucket/resumed");
    let mut expected = first;
    expected.extend_from_slice(b"tail!");
    assert_eq!(get.success().body, expected);
    assert_eq!(get.headers["content-type"], "text/plain");
    assert_eq!(get.headers["x-goog-meta-purpose"], "signed resumable");
    assert_eq!(get.headers["x-goog-generation"], generation);
    assert_eq!(get.headers["etag"], done.headers["etag"]);

    // A retried final request replays the completion without a second generation.
    let replay = chunk(&server, &path, "bytes */262149", b"");
    replay.expect(200);
    assert_eq!(replay.headers["x-goog-generation"], generation);

    // The capability names one object and accepts no other query parameters.
    let id = path.split_once('?').unwrap().1;
    error(
        &chunk(
            &server,
            &format!("/iam-bucket/other?{id}"),
            "bytes */*",
            b"",
        ),
        404,
        "NoSuchUpload",
    );
    error(
        &chunk(&server, &format!("{path}&generation=1"), "bytes */*", b""),
        400,
        "InvalidArgument",
    );
    error(
        &chunk(
            &server,
            "/iam-bucket/resumed?upload_id=unknown",
            "bytes */*",
            b"",
        ),
        404,
        "NoSuchUpload",
    );
}

#[test]
fn signed_resumable_initiation_rejects_unsigned_tampered_expired_and_unauthorized_requests() {
    let (_config, _data, server) = fixture();
    let start = wire(&[START]);
    let url = sign_start(
        &server,
        "/iam-bucket/a",
        "creator@example.invalid",
        &[START],
    );

    // Strict mode never opens a session for an anonymous caller.
    error(
        &server.request("POST", "/iam-bucket/a", &start, b""),
        401,
        "AuthenticationRequired",
    );
    // x-goog-resumable is an x-goog-* header, so the signature must cover it. The signer
    // refuses such a POST URL, so a URL signed without the header stands in for one.
    let unsigned = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/a",
        "creator@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    error(
        &put::request(&server, "POST", &unsigned, &start, b""),
        400,
        "InvalidAuthentication",
    );
    // A signed URL POST only starts a resumable upload.
    error(
        &put::request(&server, "POST", &url, "", b""),
        403,
        "SignatureDoesNotMatch",
    );
    error(
        &put::request(&server, "POST", &url, "x-goog-resumable: stop\r\n", b""),
        400,
        "InvalidAuthentication",
    );
    error(
        &put::request(
            &server,
            "POST",
            &url,
            "x-goog-resumable: start\r\nx-goog-resumable: start\r\n",
            b"",
        ),
        400,
        "InvalidAuthentication",
    );
    // Method, resource and host are bound by the signature.
    let signed_put = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/a",
        "creator@example.invalid",
        SystemTime::now(),
        signed_headers(&[START]),
    );
    error(
        &put::request(&server, "POST", &signed_put, &start, b""),
        403,
        "SignatureDoesNotMatch",
    );
    error(
        &put::request(&server, "POST", &url.replace("/a?", "/b?"), &start, b""),
        403,
        "SignatureDoesNotMatch",
    );
    let target: Uri = url.parse().unwrap();
    let port = server.address.rsplit_once(':').unwrap().1;
    let mut stream = server.stream();
    stream
        .write_all(
            format!(
                "POST {} HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Length: 0\r\n{start}\r\n",
                target.path_and_query().unwrap()
            )
            .as_bytes(),
        )
        .unwrap();
    error(&Reply::read_joined(stream), 403, "SignatureDoesNotMatch");
    // Expiry and the signer's own permissions still apply.
    let expired = sign(
        &server,
        &Method::POST,
        "/iam-bucket/a",
        "creator@example.invalid",
        SystemTime::now() - Duration::from_secs(120),
        signed_headers(&[START]),
    );
    error(
        &put::request(&server, "POST", &expired, &start, b""),
        400,
        "ExpiredToken",
    );
    let viewer = sign_start(&server, "/iam-bucket/a", "viewer@example.invalid", &[START]);
    error(
        &put::request(&server, "POST", &viewer, &start, b""),
        403,
        "AccessDenied",
    );
    // Initiation carries no object data.
    error(
        &put::request(&server, "POST", &url, &start, b"bytes"),
        400,
        "InvalidArgument",
    );
    absent(&server, "/iam-bucket/a");
    absent(&server, "/iam-bucket/b");
}

#[test]
fn signed_resumable_sessions_cancel_with_no_content() {
    let (_config, _data, server) = fixture();
    let url = sign_start(
        &server,
        "/iam-bucket/cancelled",
        "creator@example.invalid",
        &[START],
    );
    let reply = put::request(&server, "POST", &url, &wire(&[START]), b"");
    let path = session_path(&reply, "/iam-bucket/cancelled");
    let small = chunk(&server, &path, "bytes 0-2/*", b"abc");
    small.expect(400);
    assert!(
        String::from_utf8_lossy(&small.body)
            .contains("Nonfinal chunk must be a multiple of 256 KiB")
    );
    chunk(&server, &path, "invalid", b"").expect(400);
    chunk(&server, &path, "bytes */*", b"").expect(308);
    let cancelled = server.request("DELETE", &path, "", b"");
    cancelled.expect(204);
    // Google answers later requests to a cancelled XML session with 204 as well.
    chunk(&server, &path, "bytes */*", b"").expect(204);
    server.request("DELETE", &path, "", b"").expect(204);
    absent(&server, "/iam-bucket/cancelled");
}

#[test]
fn permissive_xml_resumable_initiation_needs_no_credentials() {
    let data = TestDir::new();
    let (server, ready) = Server::start(&data, &[]);
    assert_eq!(ready["auth_mode"], "permissive");
    server
        .create_bucket("", "project-a", "open-bucket")
        .success();
    let reply = server.request(
        "POST",
        "/open-bucket/dir%2Fobject",
        &wire(&[START, ("content-type", "text/plain")]),
        b"",
    );
    let path = session_path(&reply, "/open-bucket/dir%2Fobject");
    let done = server.request("PUT", &path, "", b"whole body");
    done.expect(200);
    let get = server.get("", "/open-bucket/dir%2Fobject");
    assert_eq!(get.success().body, b"whole body");
    assert_eq!(get.headers["content-type"], "text/plain");
    // A signed URL is accepted the same way; permissive mode does not verify it.
    let url = sign_start(
        &server,
        "/open-bucket/signed",
        "anyone@example.invalid",
        &[START],
    );
    let reply = put::request(&server, "POST", &url, &wire(&[START]), b"");
    session_path(&reply, "/open-bucket/signed");
}
