use super::*;
use gcs_mimic::auth::signature::sign_url;
use hyper::{HeaderMap, Method, Uri};
use std::time::SystemTime;

const PRIVATE: &[u8] = include_bytes!("../../fixtures/signing/test-only-private.pem");
const PUBLIC: &str = include_str!("../../fixtures/signing/test-only-public.pem");

fn fixture() -> (TestDir, TestDir, Server) {
    let config = TestDir::new();
    let data = TestDir::new();
    let mut value = registry();
    for index in 0..6 {
        value["identities"][index]["public_keys"] = json!([PUBLIC]);
    }
    let path = config.0.join("identities.json");
    std::fs::write(&path, value.to_string()).unwrap();
    let server = Server::strict(&data, &path);
    server.create_bucket(ADMIN, "project-a", "iam-bucket").ok();
    server
        .upload(ADMIN, "iam-bucket", "object", b"hello world")
        .ok();
    (config, data, server)
}

#[path = "signing/put.rs"]
mod put;

#[path = "signing/overrides.rs"]
mod overrides;

#[path = "signing/pap.rs"]
mod pap;

fn sign(
    server: &Server,
    method: &Method,
    path: &str,
    signer: &str,
    time: SystemTime,
    headers: HeaderMap,
) -> String {
    sign_url(
        &format!("http://{}{path}", server.address),
        method,
        signer,
        PRIVATE,
        60,
        time,
        headers,
    )
    .unwrap()
}

fn send(server: &Server, method: &str, url: &str, headers: &str) -> Reply {
    let uri: Uri = url.parse().unwrap();
    let raw = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\n{headers}\r\n",
        uri.path_and_query().unwrap(),
        uri.authority().unwrap()
    );
    let mut stream = server.stream();
    stream.write_all(raw.as_bytes()).unwrap();
    Reply::read_body(stream, method == "HEAD")
}

fn error(reply: &Reply, status: u16, code: &str) {
    assert_eq!(
        reply.status,
        status,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert!(
        String::from_utf8_lossy(&reply.body).contains(&format!("<Code>{code}</Code>")),
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
}

#[test]
fn signed_private_get_head_range_and_bucket_grants_use_signer_permissions() {
    let (_config, _data, server) = fixture();
    let now = SystemTime::now();
    error(
        &server.get("", "/iam-bucket/object"),
        401,
        "AuthenticationRequired",
    );
    let url = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "viewer@example.invalid",
        now,
        HeaderMap::new(),
    );
    let whole = send(&server, "GET", &url, "");
    whole.ok();
    assert_eq!(whole.body, b"hello world");
    let range = send(&server, "GET", &url, "Range: bytes=1-4\r\n");
    assert_eq!(range.status, 206);
    assert_eq!(range.body, b"ello");
    let selected = sign(
        &server,
        &Method::GET,
        &format!(
            "/iam-bucket/object?generation={}",
            whole.headers["x-goog-generation"]
        ),
        "viewer@example.invalid",
        now,
        HeaderMap::new(),
    );
    assert_eq!(send(&server, "GET", &selected, "").ok().body, whole.body);
    let head = sign(
        &server,
        &Method::HEAD,
        "/iam-bucket/object",
        "viewer@example.invalid",
        now,
        HeaderMap::new(),
    );
    let head = send(&server, "HEAD", &head, "");
    head.ok();
    assert!(head.body.is_empty());
    assert_eq!(head.headers["content-length"], "11");
    let no_roles = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "no-roles@example.invalid",
        now,
        HeaderMap::new(),
    );
    error(&send(&server, "GET", &no_roles, ""), 403, "AccessDenied");
    iam::set(&server,ADMIN,&json!({"bindings":[iam::binding("roles/storage.objectViewer","user:no-roles@example.invalid")]})).ok();
    assert_eq!(
        send(&server, "GET", &no_roles, "").ok().body,
        b"hello world"
    );
    server
        .create_bucket(ADMIN, "project-a", "other-bucket")
        .ok();
    server
        .upload(ADMIN, "other-bucket", "object", b"other")
        .ok();
    let other = sign(
        &server,
        &Method::GET,
        "/other-bucket/object",
        "no-roles@example.invalid",
        now,
        HeaderMap::new(),
    );
    error(&send(&server, "GET", &other, ""), 403, "AccessDenied");
    iam::set(&server, ADMIN, &json!({"bindings":[]})).ok();
    error(&send(&server, "GET", &no_roles, ""), 403, "AccessDenied");
}

#[test]
fn signed_tampering_never_falls_back_to_public_access_and_has_xml_errors() {
    let (_config, _data, server) = fixture();
    let now = SystemTime::now();
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectViewer","allUsers")]}),
    )
    .ok();
    let mut required = HeaderMap::new();
    required.insert("x-required", "one".parse().unwrap());
    let url = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object?generation=123",
        "viewer@example.invalid",
        now,
        required.clone(),
    );
    for changed in [
        url.replace("/iam-bucket/", "/other-bucket/"),
        url.replace("/object?", "/other?"),
        url.replace("generation=123", "generation=124"),
        url.replace("&generation=123", ""),
        format!("{url}&extra=1"),
    ] {
        error(
            &send(&server, "GET", &changed, "x-required: one\r\n"),
            403,
            "SignatureDoesNotMatch",
        );
    }
    for method in ["HEAD", "PUT", "DELETE"] {
        let reply = send(&server, method, &url, "x-required: one\r\n");
        assert_eq!(reply.status, 403);
        if method != "HEAD" {
            error(&reply, 403, "SignatureDoesNotMatch");
        }
    }
    for header in ["", "x-required: two\r\n"] {
        error(
            &send(&server, "GET", &url, header),
            403,
            "SignatureDoesNotMatch",
        );
    }
    for host in [
        "different.invalid".to_owned(),
        format!("{}0", server.address),
    ] {
        error(
            &send(
                &server,
                "GET",
                &url.replace(&server.address, &host),
                "x-required: one\r\n",
            ),
            403,
            "SignatureDoesNotMatch",
        );
    }
    let unknown = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "unknown@example.invalid",
        now,
        HeaderMap::new(),
    );
    error(
        &send(&server, "GET", &unknown, ""),
        403,
        "InvalidAccessKeyId",
    );
    let expired = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "viewer@example.invalid",
        now - Duration::from_secs(120),
        HeaderMap::new(),
    );
    error(&send(&server, "GET", &expired, ""), 400, "ExpiredToken");
    let future = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "viewer@example.invalid",
        now + Duration::from_secs(1200),
        HeaderMap::new(),
    );
    error(
        &send(&server, "GET", &future, ""),
        403,
        "RequestTimeTooSkewed",
    );
    let badhex = url.rsplit_once('=').unwrap().0.to_owned() + "=nothex";
    error(
        &send(&server, "GET", &badhex, "x-required: one\r\n"),
        400,
        "InvalidAuthentication",
    );
    let override_url = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object?response-cache-control=no-cache",
        "viewer@example.invalid",
        now,
        HeaderMap::new(),
    );
    error(
        &send(&server, "GET", &override_url, ""),
        501,
        "NotImplemented",
    );
    let valid = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "viewer@example.invalid",
        now,
        HeaderMap::new(),
    );
    error(
        &send(&server, "GET", &valid, ADMIN),
        400,
        "InvalidAuthentication",
    );
    let query = valid.split_once('?').unwrap().1;
    server
        .get("", &format!("/storage/v1/b/iam-bucket/o/object?{query}"))
        .invalid_credentials();
}

#[test]
fn signed_paths_and_repeated_headers_round_trip_without_normalizing_object_names() {
    let (_config, _data, server) = fixture();
    let now = SystemTime::now();
    for name in [
        "é日/a b+%?#",
        "a//b/",
        "a/../b",
        "./x",
        "a/.../b",
        "/leading",
    ] {
        let encoded = gcs_mimic::url::encode_object_name(name);
        server
            .upload(ADMIN, "iam-bucket", &encoded, name.as_bytes())
            .ok();
        let url = sign(
            &server,
            &Method::GET,
            &format!("/iam-bucket/{encoded}"),
            "viewer@example.invalid",
            now,
            HeaderMap::new(),
        );
        assert_eq!(send(&server, "GET", &url, "").ok().body, name.as_bytes());
    }
    let mut headers = HeaderMap::new();
    headers.append("x-reviewer", "jane doe".parse().unwrap());
    headers.append("x-reviewer", "john".parse().unwrap());
    let url = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "viewer@example.invalid",
        now,
        headers,
    );
    assert_eq!(
        send(
            &server,
            "GET",
            &url,
            "X-Reviewer:  jane\t  doe \r\nx-reviewer: john\r\n"
        )
        .ok()
        .body,
        b"hello world"
    );
    error(
        &send(
            &server,
            "GET",
            &url,
            "x-reviewer: john\r\nx-reviewer: jane doe\r\n",
        ),
        403,
        "SignatureDoesNotMatch",
    );
    let encoded = sign(
        &server,
        &Method::GET,
        "/iam-bucket/a%2F..%2Fb",
        "viewer@example.invalid",
        now,
        HeaderMap::new(),
    );
    error(
        &send(&server, "GET", &encoded.replace("%2F", "/"), ""),
        403,
        "SignatureDoesNotMatch",
    );
}

#[test]
fn invalid_signing_keys_fail_before_readiness_and_permissive_remains_explicit() {
    let config = TestDir::new();
    let data = TestDir::new();
    for key in ["bad PEM", std::str::from_utf8(PRIVATE).unwrap()] {
        let mut value = registry();
        value["identities"][1]["public_keys"] = json!([key]);
        let path = config.0.join("identities.json");
        std::fs::write(&path, value.to_string()).unwrap();
        let output = common::server_command()
            .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
            .arg(&data.0)
            .args(["--auth-mode", "strict", "--identity-file"])
            .arg(path)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("public_keys"));
    }
    let (server, _) = Server::start(&data, &[]);
    server.create_bucket("", "project-a", "iam-bucket").ok();
    server.upload("", "iam-bucket", "object", b"hello").ok();
    let url = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "unknown@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    assert_eq!(send(&server, "GET", &(url + "00"), "").ok().body, b"hello");
}

#[test]
fn partial_signing_fields_never_become_anonymous_under_public_grants() {
    let (_config, _data, server) = fixture();
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectViewer","allUsers")]}),
    )
    .ok();
    let url = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "viewer@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    let (base, query) = url.split_once('?').unwrap();
    let fields = query.split('&').collect::<Vec<_>>();
    assert_eq!(fields.len(), 6);
    for mask in 1..63 {
        let query = fields
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, value)| *value)
            .collect::<Vec<_>>()
            .join("&");
        error(
            &send(&server, "GET", &format!("{base}?{query}"), ""),
            400,
            "InvalidAuthentication",
        );
    }
    for query in [
        "%58-Goog-Date=20261023T120000Z",
        "x-goog-expires=60",
        "X-Goog-SignedHeaders=host",
    ] {
        error(
            &send(&server, "GET", &format!("{base}?{query}"), ""),
            400,
            "InvalidAuthentication",
        );
    }
}
