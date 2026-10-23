use super::*;
use gcs_mimic::{auth::signature::sign_url, url::encode_object_name};
use hyper::{HeaderMap, Method, Uri};
use std::time::SystemTime;

const DOMAIN: &str = "storage.example.invalid";
const BUCKET: &str = "dotted.bucket";

fn fixture(enabled: bool) -> (TestDir, TestDir, Server) {
    let config = TestDir::new();
    let data = TestDir::new();
    let mut identities = registry();
    for identity in identities["identities"].as_array_mut().unwrap() {
        identity["public_keys"] =
            json!([include_str!("../../fixtures/signing/test-only-public.pem")]);
    }
    let file = config.0.join("identities.json");
    std::fs::write(&file, identities.to_string()).unwrap();
    let (server, _) = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            file.to_str().unwrap(),
            "--xml-virtual-host-domains",
            if enabled {
                "example.invalid,storage.example.invalid"
            } else {
                ""
            },
            "--api-base-url",
            "http://advertised.invalid/prefix",
        ],
    );
    server.create_bucket(ADMIN, "project-a", BUCKET).ok();
    server.upload(ADMIN, BUCKET, "object", b"hello world").ok();
    (config, data, server)
}

fn request(
    server: &Server,
    method: &str,
    host: &str,
    path: &str,
    headers: &str,
    body: &[u8],
) -> Reply {
    let mut stream = server.stream();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\n{headers}\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    Reply::read_body(stream, method == "HEAD")
}

fn host(server: &Server) -> String {
    format!(
        "{BUCKET}.{DOMAIN}:{}",
        server.address.split_once(':').unwrap().1
    )
}

fn signed(method: &Method, authority: &str, path: &str, signer: &str) -> String {
    sign_url(
        &format!("http://{authority}{path}"),
        method,
        signer,
        include_bytes!("../../fixtures/signing/test-only-private.pem"),
        60,
        SystemTime::now(),
        HeaderMap::new(),
    )
    .unwrap()
}

fn send(server: &Server, method: &str, url: &str, headers: &str, body: &[u8]) -> Reply {
    let uri: Uri = url.parse().unwrap();
    request(
        server,
        method,
        uri.authority().unwrap().as_str(),
        uri.path_and_query().unwrap().as_str(),
        headers,
        body,
    )
}

fn error(reply: &Reply, status: u16, code: &str) {
    assert_eq!(
        reply.status,
        status,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(
        reply.headers["content-type"],
        "application/xml; charset=UTF-8"
    );
    assert!(
        String::from_utf8_lossy(&reply.body).contains(&format!("<Code>{code}</Code>")),
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
}

#[test]
fn virtual_host_reads_puts_raw_paths_and_strict_permissions_match_path_style() {
    let (_config, _data, server) = fixture(true);
    let host = host(&server);
    error(
        &request(&server, "GET", &host, "/object", "", b""),
        401,
        "AuthenticationRequired",
    );
    error(
        &request(&server, "GET", &host, "/object", NO_ROLES, b""),
        403,
        "AccessDenied",
    );
    error(
        &request(&server, "PUT", &host, "/new", VIEWER, b"no"),
        403,
        "AccessDenied",
    );
    request(&server, "PUT", &host, "/new", CREATOR, b"new").ok();
    error(
        &request(&server, "PUT", &host, "/new", CREATOR, b"overwrite"),
        403,
        "AccessDenied",
    );
    assert_eq!(
        request(&server, "GET", &host, "/new", VIEWER, b"")
            .ok()
            .body,
        b"new"
    );
    for name in [
        "object",
        "é日/a b+%?#",
        "a//b/",
        "/leading",
        "a/../b",
        "storage/v1/b",
        "_internal/ready",
    ] {
        let encoded = encode_object_name(name);
        request(
            &server,
            "PUT",
            &host,
            &format!("/{encoded}"),
            ADMIN,
            name.as_bytes(),
        )
        .ok();
        let virtual_read = request(&server, "GET", &host, &format!("/{encoded}"), VIEWER, b"");
        let path_read = server.get(VIEWER, &format!("/{BUCKET}/{encoded}"));
        assert_eq!(virtual_read.ok().body, path_read.ok().body);
        for header in ["etag", "x-goog-generation", "content-length", "x-goog-hash"] {
            assert_eq!(virtual_read.headers[header], path_read.headers[header]);
        }
        let head = request(&server, "HEAD", &host, &format!("/{encoded}"), VIEWER, b"");
        head.ok();
        assert!(head.body.is_empty());
    }
    let range = request(
        &server,
        "GET",
        &host,
        "/new",
        &format!("{VIEWER}Range: bytes=1-2\r\n"),
        b"",
    );
    assert_eq!(range.status, 206);
    assert_eq!(range.body, b"ew");
    let current = request(&server, "GET", &host, "/new", VIEWER, b"");
    let generation: i64 = current.headers["x-goog-generation"].parse().unwrap();
    assert_eq!(
        request(
            &server,
            "GET",
            &host,
            &format!("/new?generation={generation}"),
            VIEWER,
            b""
        )
        .ok()
        .body,
        b"new"
    );
    error(
        &request(
            &server,
            "GET",
            &host,
            &format!("/new?generation={}", generation + 1),
            VIEWER,
            b"",
        ),
        404,
        "NoSuchKey",
    );
    error(
        &request(
            &server,
            "PUT",
            &host,
            "/new",
            &format!("{ADMIN}x-goog-if-generation-match: 0\r\n"),
            b"denied",
        ),
        412,
        "PreconditionFailed",
    );
    assert_eq!(
        request(&server, "GET", &host, "/new", VIEWER, b"").headers["x-goog-generation"],
        current.headers["x-goog-generation"]
    );
    request(&server, "PUT", &host, "/new", &format!("{ADMIN}x-goog-if-generation-match: {generation}\r\nx-goog-if-metageneration-match: 1\r\n"), b"condition bytes").ok();
    error(
        &request(&server, "GET", &host, "/", ADMIN, b""),
        501,
        "NotImplemented",
    );
    error(
        &request(&server, "GET", &host, "/bad%GG", ADMIN, b""),
        400,
        "InvalidArgument",
    );
    error(
        &request(&server, "GET", &host, "/%FF", ADMIN, b""),
        400,
        "InvalidArgument",
    );
    assert_eq!(
        request(&server, "GET", &host, "/storage/v1/b", ADMIN, b"")
            .ok()
            .body,
        b"storage/v1/b"
    );
}

#[test]
fn virtual_host_is_opt_in_and_never_uses_forwarded_hosts_or_json_dispatch() {
    let (_config, _data, server) = fixture(true);
    let host = host(&server);
    for other in [
        "dotted.bucket.unconfigured.invalid",
        "dotted.bucket.evil-example.invalid",
        DOMAIN,
        "dotted.bucket.storage.example.invalid.",
        "[::1]:4443",
    ] {
        assert_eq!(
            request(
                &server,
                "GET",
                other,
                &format!("/{BUCKET}/object"),
                VIEWER,
                b""
            )
            .ok()
            .body,
            b"hello world"
        );
    }
    assert_eq!(
        request(
            &server,
            "GET",
            "local",
            &format!("/{BUCKET}/object"),
            &format!("{VIEWER}Forwarded: host={host}\r\nX-Forwarded-Host: {host}\r\n"),
            b""
        )
        .ok()
        .body,
        b"hello world"
    );
    let metadata = request(
        &server,
        "GET",
        "local",
        &format!("/storage/v1/b/{BUCKET}/o/object"),
        &format!("{ADMIN}X-Forwarded-Host: {host}\r\n"),
        b"",
    )
    .ok()
    .json();
    assert!(
        metadata["selfLink"]
            .as_str()
            .unwrap()
            .starts_with("http://advertised.invalid/prefix/")
    );
    error(
        &request(
            &server,
            "GET",
            &host,
            &format!("/storage/v1/b/{BUCKET}/o/object"),
            ADMIN,
            b"",
        ),
        404,
        "NoSuchKey",
    );
    error(
        &request(&server, "GET", &host, "/_internal/ready", ADMIN, b""),
        404,
        "NoSuchKey",
    );
    assert_eq!(
        request(
            &server,
            "GET",
            &host.to_ascii_uppercase(),
            "/object",
            VIEWER,
            b""
        )
        .ok()
        .body,
        b"hello world"
    );
    let (_config_off, _data_off, disabled) = fixture(false);
    assert_eq!(
        request(
            &disabled,
            "GET",
            &host,
            &format!("/{BUCKET}/object"),
            VIEWER,
            b""
        )
        .ok()
        .body,
        b"hello world"
    );
    assert_eq!(
        request(&disabled, "GET", &host, "/object", VIEWER, b"").status,
        501
    );
}

#[test]
fn virtual_host_absolute_target_authority_must_match_host_before_dispatch() {
    let (_config, _data, server) = fixture(true);
    let host = host(&server);
    assert_eq!(
        request(
            &server,
            "GET",
            &host,
            &format!("http://{host}/object"),
            VIEWER,
            b""
        )
        .ok()
        .body,
        b"hello world"
    );
    for (authority, target) in [
        (host.as_str(), format!("http://local/{BUCKET}/object")),
        ("local", format!("http://{host}/object")),
    ] {
        error(
            &request(&server, "GET", authority, &target, VIEWER, b""),
            400,
            "InvalidArgument",
        );
        let head = request(&server, "HEAD", authority, &target, VIEWER, b"");
        assert_eq!(head.status, 400);
        assert!(head.body.is_empty());
        error(
            &request(
                &server,
                "OPTIONS",
                authority,
                &target,
                "Origin: https://app.invalid\r\nAccess-Control-Request-Method: GET\r\n",
                b"",
            ),
            400,
            "InvalidArgument",
        );
    }
    let signed = signed(&Method::GET, &host, "/object", "viewer@example.invalid");
    error(
        &request(&server, "GET", "local", &signed, "", b""),
        403,
        "SignatureDoesNotMatch",
    );
}

#[test]
fn virtual_host_signatures_bind_host_port_raw_path_and_addressing_style() {
    let (_config, _data, server) = fixture(true);
    let host = host(&server);
    let url = signed(&Method::GET, &host, "/object", "viewer@example.invalid");
    assert_eq!(
        send(&server, "GET", &url, "", b"").ok().body,
        b"hello world"
    );
    let head = signed(&Method::HEAD, &host, "/object", "viewer@example.invalid");
    assert!(send(&server, "HEAD", &head, "", b"").ok().body.is_empty());
    let put = signed(&Method::PUT, &host, "/object", "admin@example.invalid");
    send(
        &server,
        "PUT",
        &put,
        "Content-Type: text/plain\r\n",
        b"signed new bytes",
    )
    .ok();
    error(
        &send(
            &server,
            "PUT",
            &signed(&Method::PUT, &host, "/object", "viewer@example.invalid"),
            "",
            b"no",
        ),
        403,
        "AccessDenied",
    );
    for changed in [
        url.replace(BUCKET, "another.bucket"),
        url.replace(
            &host,
            &format!(
                "{BUCKET}.{DOMAIN}:{}",
                server
                    .address
                    .split_once(':')
                    .unwrap()
                    .1
                    .parse::<u16>()
                    .unwrap()
                    ^ 1
            ),
        ),
        url.replace("/object?", "/different?"),
        url.replace(
            &format!("{host}/object"),
            &format!("{}/{BUCKET}/object", server.address),
        ),
    ] {
        error(
            &send(&server, "GET", &changed, "", b""),
            403,
            "SignatureDoesNotMatch",
        );
    }
    let path_url = signed(
        &Method::GET,
        &server.address,
        &format!("/{BUCKET}/object"),
        "viewer@example.invalid",
    );
    let moved = path_url.replace(
        &format!("{}/{BUCKET}/object", server.address),
        &format!("{host}/object"),
    );
    error(
        &send(&server, "GET", &moved, "", b""),
        403,
        "SignatureDoesNotMatch",
    );
    for name in ["é日/a b+%?#", "a//b/", "/leading", "a/../b"] {
        let path = format!("/{}", encode_object_name(name));
        let put = signed(&Method::PUT, &host, &path, "admin@example.invalid");
        send(&server, "PUT", &put, "", name.as_bytes()).ok();
        let get = signed(&Method::GET, &host, &path, "viewer@example.invalid");
        assert_eq!(
            send(&server, "GET", &get, "", b"").ok().body,
            name.as_bytes()
        );
    }
}

#[test]
fn virtual_host_public_access_and_xml_cors_use_bucket_rules() {
    let (_config, _data, server) = fixture(true);
    let host = host(&server);
    server.request("PATCH", &format!("/storage/v1/b/{BUCKET}"), ADMIN, &serde_json::to_vec(&json!({"cors":[{"origin":["https://app.invalid"],"method":["GET","HEAD","PUT"],"responseHeader":["Content-Type"],"maxAgeSeconds":120}]})).unwrap()).ok();
    let origin = "Origin: https://app.invalid\r\n";
    let preflight = request(
        &server,
        "OPTIONS",
        &host,
        "/object",
        &format!(
            "{origin}Access-Control-Request-Method: PUT\r\nAccess-Control-Request-Headers: Content-Type\r\n"
        ),
        b"",
    );
    assert_eq!(preflight.status, 200);
    assert_eq!(
        preflight.headers["access-control-allow-origin"],
        "https://app.invalid"
    );
    let json_path = request(
        &server,
        "OPTIONS",
        &host,
        "/storage/v1/b/other/o/object",
        &format!(
            "{origin}Access-Control-Request-Method: PUT\r\nAccess-Control-Request-Headers: Content-Type\r\n"
        ),
        b"",
    );
    assert_eq!(json_path.status, 200);
    assert_eq!(
        json_path.headers["access-control-allow-origin"],
        "https://app.invalid"
    );
    assert!(
        !json_path
            .headers
            .contains_key("access-control-allow-credentials")
    );
    let denied = request(&server, "GET", &host, "/object", origin, b"");
    error(&denied, 401, "AuthenticationRequired");
    assert_eq!(
        denied.headers["access-control-allow-origin"],
        "https://app.invalid"
    );
    server
        .request(
            "PUT",
            &format!("/storage/v1/b/{BUCKET}/iam"),
            ADMIN,
            &serde_json::to_vec(
                &json!({"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}),
            )
            .unwrap(),
        )
        .ok();
    let public = request(&server, "GET", &host, "/object", origin, b"");
    assert_eq!(public.ok().body, b"hello world");
    assert_eq!(
        public.headers["access-control-allow-origin"],
        "https://app.invalid"
    );
    let put = signed(&Method::PUT, &host, "/object", "admin@example.invalid");
    let response = send(&server, "PUT", &put, origin, b"cors signed put");
    response.ok();
    assert_eq!(
        response.headers["access-control-allow-origin"],
        "https://app.invalid"
    );
    error(
        &send(
            &server,
            "GET",
            &format!(
                "{}&tampered=1",
                signed(&Method::GET, &host, "/object", "viewer@example.invalid")
            ),
            origin,
            b"",
        ),
        403,
        "SignatureDoesNotMatch",
    );
}

#[test]
fn virtual_host_signed_response_overrides_bind_query_and_preserve_metadata() {
    let (_config, _data, server) = fixture(true);
    let host = host(&server);
    let metadata = format!("/storage/v1/b/{BUCKET}/o/object");
    let before = server.get(ADMIN, &metadata).ok().json();
    let path = "/object?response-content-type=text%2Fcsv&response-content-disposition=attachment%3B%20filename%3Dreport.csv";
    for method in [Method::GET, Method::HEAD] {
        let url = signed(&method, &host, path, "viewer@example.invalid");
        let reply = send(&server, method.as_str(), &url, "", b"");
        reply.ok();
        assert_eq!(reply.headers["content-type"], "text/csv");
        assert_eq!(
            reply.headers["content-disposition"],
            "attachment; filename=report.csv"
        );
        assert_eq!(reply.headers["content-length"], "11");
        assert_eq!(
            reply.headers["x-goog-generation"],
            before["generation"].as_str().unwrap()
        );
        assert_eq!(
            reply.body,
            if method == Method::HEAD {
                b"".as_slice()
            } else {
                b"hello world".as_slice()
            }
        );
        let tampered = send(
            &server,
            method.as_str(),
            &url.replace("text%2Fcsv", "text%2Fhtml"),
            "",
            b"",
        );
        assert_eq!(tampered.status, 403);
        assert_eq!(
            tampered.headers["content-type"],
            "application/xml; charset=UTF-8"
        );
        assert!(!tampered.headers.contains_key("content-disposition"));
    }
    assert_eq!(server.get(ADMIN, &metadata).ok().json(), before);
}

#[test]
fn virtual_host_cli_signs_literal_response_queries_for_the_final_bucket_host() {
    let (_config, _data, server) = fixture(true);
    let port = server.address.split_once(':').unwrap().1;
    for method in ["GET", "HEAD"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
            .args([
                "url",
                "sign",
                "--base-url",
                &format!("http://{DOMAIN}:{port}"),
                "--bucket",
                BUCKET,
                "--object",
                "object",
                "--method",
                method,
                "--expires",
                "5m",
                "--signer",
                "viewer@example.invalid",
                "--private-key",
                "fixtures/signing/test-only-private.pem",
                "--style",
                "virtual-hosted",
                "--query",
                "response-content-type=text/csv",
                "--query",
                "response-content-disposition=attachment; filename=report.csv",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let url = String::from_utf8(output.stdout).unwrap();
        let uri: Uri = url.trim().parse().unwrap();
        assert_eq!(uri.authority().unwrap().as_str(), host(&server));
        assert_eq!(uri.path(), "/object");
        let reply = send(&server, method, url.trim(), "", b"");
        reply.ok();
        assert_eq!(reply.headers["content-type"], "text/csv");
        assert_eq!(
            reply.headers["content-disposition"],
            "attachment; filename=report.csv"
        );
        assert_eq!(
            reply.body,
            if method == "HEAD" {
                b"".as_slice()
            } else {
                b"hello world".as_slice()
            }
        );
    }
}

#[test]
fn virtual_host_pap_preserves_signed_transfers_and_reactivates_stored_public_grants() {
    for member in ["allUsers", "allAuthenticatedUsers"] {
        let (_config, _data, server) = fixture(true);
        let host = host(&server);
        let bucket = format!("/storage/v1/b/{BUCKET}");
        let policy_path = format!("{bucket}/iam");
        let policy = server
            .request(
                "PUT",
                &policy_path,
                ADMIN,
                json!({"bindings":[{"role":"roles/storage.objectViewer","members":[member]}]})
                    .to_string()
                    .as_bytes(),
            )
            .ok()
            .json();
        request(&server, "GET", &host, "/object", NO_ROLES, b"").ok();
        let anonymous_status = if member == "allUsers" { 200 } else { 401 };
        assert_eq!(
            request(&server, "GET", &host, "/object", "", b"").status,
            anonymous_status
        );
        assert_eq!(
            server.get("", &format!("/{BUCKET}/object")).status,
            anonymous_status
        );
        assert_eq!(
            server.get(NO_ROLES, &format!("/{BUCKET}/object")).ok().body,
            b"hello world"
        );
        server
            .request(
                "PATCH",
                &bucket,
                ADMIN,
                br#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
            )
            .ok();
        for method in ["GET", "HEAD"] {
            assert_eq!(
                request(&server, method, &host, "/object", "", b"").status,
                401
            );
            assert_eq!(
                server
                    .request(method, &format!("/{BUCKET}/object"), "", b"")
                    .status,
                401
            );
            assert_eq!(
                request(&server, method, &host, "/object", NO_ROLES, b"").status,
                403
            );
            let public_only = signed(
                &method.parse().unwrap(),
                &host,
                "/object",
                "no-roles@example.invalid",
            );
            assert_eq!(send(&server, method, &public_only, "", b"").status, 403);
        }
        let before = server.get(ADMIN, &format!("{bucket}/o/object")).ok().json();
        let get = signed(
            &Method::GET,
            &host,
            "/object?response-content-type=text%2Fcsv",
            "viewer@example.invalid",
        );
        let reply = send(&server, "GET", &get, "", b"");
        assert_eq!(reply.ok().body, b"hello world");
        assert_eq!(reply.headers["content-type"], "text/csv");
        error(
            &send(
                &server,
                "GET",
                &get.replace("text%2Fcsv", "text%2Fhtml"),
                "",
                b"",
            ),
            403,
            "SignatureDoesNotMatch",
        );
        assert_eq!(
            server.get(ADMIN, &format!("{bucket}/o/object")).ok().json(),
            before
        );
        let put = signed(
            &Method::PUT,
            &host,
            "/signed-under-pap",
            "admin@example.invalid",
        );
        send(&server, "PUT", &put, "", b"signed under PAP").ok();
        let uploaded = signed(
            &Method::GET,
            &host,
            "/signed-under-pap",
            "viewer@example.invalid",
        );
        assert_eq!(
            send(&server, "GET", &uploaded, "", b"").ok().body,
            b"signed under PAP"
        );
        assert_eq!(server.get(ADMIN, &policy_path).ok().json(), policy);
        server
            .request(
                "PATCH",
                &bucket,
                ADMIN,
                br#"{"iamConfiguration":{"publicAccessPrevention":"inherited"}}"#,
            )
            .ok();
        assert_eq!(server.get(ADMIN, &policy_path).ok().json(), policy);
        assert_eq!(
            request(&server, "GET", &host, "/object", NO_ROLES, b"")
                .ok()
                .body,
            b"hello world"
        );
        assert_eq!(
            request(&server, "GET", &host, "/object", "", b"").status,
            anonymous_status
        );
    }
}
