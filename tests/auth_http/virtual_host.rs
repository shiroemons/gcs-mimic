use super::*;
use gcs_mimic::{auth::signature::sign_url, url::encode_object_name};
use hyper::{HeaderMap, Method, Uri};
use std::time::SystemTime;

const DOMAIN: &str = "storage.example.invalid";
const BUCKET: &str = "dotted.bucket";

#[test]
fn virtual_host_xml_objects_keep_known_google_route_names() {
    let (_config, _data, server) = configured_fixture(&["--xml-virtual-host-domains", DOMAIN]);
    server
        .upload(ADMIN, BUCKET, "storage/v1/b/x/acl", b"explicit XML object")
        .ok();
    assert_eq!(
        request(
            &server,
            "GET",
            &format!("{BUCKET}.{DOMAIN}"),
            "/storage/v1/b/x/acl",
            VIEWER,
            b""
        )
        .ok()
        .body,
        b"explicit XML object"
    );
}

#[test]
fn public_origin_ready_reports_normalized_dns_ip_and_unset_values() {
    for (base, expected, enabled) in [
        (None, None, false),
        (
            Some("https://Storage.Invalid:443/"),
            Some("https://storage.invalid"),
            true,
        ),
        (
            Some("http://Storage.Invalid:4443/"),
            Some("http://storage.invalid:4443"),
            true,
        ),
        (
            Some("https://storage.invalid:80"),
            Some("https://storage.invalid:80"),
            true,
        ),
        (
            Some("http://127.0.0.1:80/"),
            Some("http://127.0.0.1"),
            false,
        ),
        (
            Some("https://[0:0:0:0:0:0:0:1]:443/"),
            Some("https://[::1]"),
            false,
        ),
    ] {
        let data = TestDir::new();
        let mut options = vec!["--api-base-url", "http://advertised.invalid/prefix"];
        if let Some(base) = base {
            options.extend(["--public-base-url", base]);
        }
        let (mut server, ready) = Server::start_with_stderr(&data, &options, Stdio::piped());
        assert_eq!(
            ready.get("public_base_url"),
            Some(&expected.map(Value::from).unwrap_or(Value::Null))
        );
        assert_eq!(ready["public_virtual_host_enabled"], enabled);
        server.create_bucket("", "project-a", "probe-bucket").ok();
        server.upload("", "probe-bucket", "object", b"hello").ok();
        let metadata = server
            .get("", "/storage/v1/b/probe-bucket/o/object")
            .ok()
            .json();
        assert!(
            metadata["selfLink"]
                .as_str()
                .unwrap()
                .starts_with("http://advertised.invalid/prefix/")
        );
        server.stop();
        let mut stderr = String::new();
        server
            .child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        if base.is_some() && !enabled {
            let reports: Vec<Value> = stderr
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert!(reports.iter().any(|report| {
                report["level"] == "warn"
                    && report["diagnostics"].as_array().is_some_and(|items| {
                        items.iter().any(|item| {
                            item["key"] == "--public-base-url"
                                && item["fix"]
                                    .as_str()
                                    .unwrap()
                                    .contains("virtual-host routing is disabled")
                        })
                    })
            }));
        }
    }
}

#[test]
fn invalid_public_origin_prevents_catalog_creation_and_ready() {
    let dir = TestDir::new();
    let domains = (0..32)
        .map(|index| format!("host{index}.invalid"))
        .collect::<Vec<_>>()
        .join(",");
    for (index, (options, expected_key)) in [
        (
            vec!["--public-base-url", "http://storage.invalid/prefix"],
            "--public-base-url",
        ),
        (
            vec!["--public-base-url", "http://storage.invalid:0"],
            "--public-base-url",
        ),
        (
            vec!["--public-base-url", "http://127.1"],
            "--public-base-url",
        ),
        (
            vec![
                "--public-base-url",
                "http://storage.invalid",
                "--xml-virtual-host-domains",
                &domains,
            ],
            "--public-base-url",
        ),
        (
            vec![
                "--public-base-url",
                "http://storage.invalid",
                "--api-base-url",
                "http://api.storage.invalid",
            ],
            "--api-base-url",
        ),
    ]
    .iter()
    .enumerate()
    {
        let data = dir.0.join(format!("data-{index}"));
        let output = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&data)
            .args(options)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!data.exists());
        let reports: Vec<Value> = String::from_utf8_lossy(&output.stderr)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(reports.iter().any(|report| {
            report["diagnostics"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["key"] == *expected_key))
        }));
    }
}

fn fixture(enabled: bool) -> (TestDir, TestDir, Server) {
    configured_fixture(&[
        "--xml-virtual-host-domains",
        if enabled {
            "example.invalid,storage.example.invalid"
        } else {
            ""
        },
    ])
}

fn configured_fixture(options: &[&str]) -> (TestDir, TestDir, Server) {
    let config = TestDir::new();
    let data = TestDir::new();
    let mut identities = registry();
    for identity in identities["identities"].as_array_mut().unwrap() {
        identity["public_keys"] =
            json!([include_str!("../../fixtures/signing/test-only-public.pem")]);
    }
    let file = config.0.join("identities.json");
    std::fs::write(&file, identities.to_string()).unwrap();
    let mut args = vec![
        "--auth-mode",
        "strict",
        "--identity-file",
        file.to_str().unwrap(),
    ];
    if !options.contains(&"--api-base-url") {
        args.extend(["--api-base-url", "http://advertised.invalid/prefix"]);
    }
    args.extend_from_slice(options);
    let (server, _) = Server::start(&data, &args);
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
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\n{headers}\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    if method == "HEAD" {
        request = close_after_head(&request);
    }
    stream.write_all(&request).unwrap();
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
fn public_origin_native_and_unset_legacy_keep_path_style_host_acceptance() {
    for options in [
        vec!["--public-base-url", "http://storage.example.invalid"],
        vec!["--compat-profile", "legacy-emulator"],
        vec![],
    ] {
        let (_config, _data, server) = configured_fixture(&options);
        for authority in ["arbitrary.invalid:1234", "127.0.0.1", "[::1]:4443"] {
            assert_eq!(
                request(
                    &server,
                    "GET",
                    authority,
                    &format!("/{BUCKET}/object"),
                    VIEWER,
                    b""
                )
                .ok()
                .body,
                b"hello world"
            );
        }
        if options.first() == Some(&"--public-base-url") {
            assert_eq!(
                request(
                    &server,
                    "GET",
                    &format!("{BUCKET}.{DOMAIN}:1234"),
                    "/object",
                    VIEWER,
                    b""
                )
                .ok()
                .body,
                b"hello world"
            );
        }
    }
}

#[test]
fn public_origin_legacy_host_matrix_selects_resource_and_preserves_reserved_routes() {
    for (origin, ports) in [
        (
            "http://storage.example.invalid",
            vec!["", ":80", ":443", ":8443"],
        ),
        (
            "http://storage.example.invalid:80",
            vec!["", ":80", ":8443"],
        ),
        ("http://storage.example.invalid:8443", vec![":8443"]),
    ] {
        let (_config, _data, server) = configured_fixture(&[
            "--compat-profile",
            "legacy-emulator",
            "--public-base-url",
            origin,
            "--xml-virtual-host-domains",
            "example.invalid,explicit.storage.example.invalid",
        ]);
        for (bucket, object) in [
            ("probe-bucket", "object"),
            (DOMAIN, "probe-bucket/object"),
            ("dotted.bucket.storage.example.invalid", "object"),
            ("probe-bucket.storage.example.invalid", "object"),
            ("lookalike.invalid", "probe-bucket/object"),
            ("advertised.invalid", &format!("{BUCKET}/object")),
        ] {
            server.create_bucket(ADMIN, "project-a", bucket).ok();
            server
                .upload(
                    ADMIN,
                    bucket,
                    object,
                    format!("bucket={bucket};object={object}").as_bytes(),
                )
                .ok();
        }
        for port in &ports {
            for (authority, path) in [
                (format!("{DOMAIN}{port}"), "/probe-bucket/object"),
                (format!("probe-bucket.{DOMAIN}{port}"), "/object"),
            ] {
                let reply = request(&server, "GET", &authority, path, VIEWER, b"");
                assert_eq!(
                    reply.ok().body,
                    b"bucket=probe-bucket;object=object",
                    "{origin}: {authority}"
                );
                let head = request(&server, "HEAD", &authority, path, VIEWER, b"");
                head.ok();
                assert!(head.body.is_empty());
            }
        }
        for authority in ["dotted.bucket.storage.example.invalid", "lookalike.invalid"] {
            let path = if authority.starts_with("dotted") {
                "/object"
            } else {
                "/probe-bucket/object"
            };
            assert!(
                String::from_utf8_lossy(
                    &request(&server, "GET", authority, path, VIEWER, b"")
                        .ok()
                        .body
                )
                .starts_with(&format!("bucket={authority};"))
            );
        }
        for authority in [
            "Storage.Example.Invalid",
            "probe-bucket.Storage.Example.Invalid",
            "storage.example.invalid.",
        ] {
            error(
                &request(&server, "GET", authority, "/object", VIEWER, b""),
                400,
                "InvalidArgument",
            );
        }
        if origin.ends_with(":8443") {
            assert_eq!(
                request(&server, "GET", DOMAIN, "/probe-bucket/object", VIEWER, b"")
                    .ok()
                    .body,
                b"bucket=storage.example.invalid;object=probe-bucket/object"
            );
            // The public candidate must not retry the shorter explicit example.invalid suffix.
            assert_eq!(
                request(
                    &server,
                    "GET",
                    "probe-bucket.storage.example.invalid:80",
                    "/object",
                    VIEWER,
                    b""
                )
                .ok()
                .body,
                b"bucket=probe-bucket.storage.example.invalid;object=object"
            );
            assert_eq!(
                request(
                    &server,
                    "GET",
                    "probe-bucket.storage.example.invalid:08443",
                    "/object",
                    VIEWER,
                    b""
                )
                .ok()
                .body,
                b"bucket=probe-bucket.storage.example.invalid;object=object"
            );
        }
        assert_eq!(server.get(VIEWER, &format!("/{BUCKET}/object")).status, 404);
        assert_eq!(
            request(
                &server,
                "GET",
                "advertised.invalid",
                &format!("/{BUCKET}/object"),
                VIEWER,
                b""
            )
            .ok()
            .body,
            format!("bucket=advertised.invalid;object={BUCKET}/object").as_bytes()
        );
        for authority in [
            DOMAIN,
            "probe-bucket.storage.example.invalid",
            "unrelated.invalid",
        ] {
            request(
                &server,
                "GET",
                authority,
                "/storage/v1/b?project=project-a",
                ADMIN,
                b"",
            )
            .ok();
            request(&server, "GET", authority, "/_internal/ready", "", b"").ok();
        }
        server
            .upload(ADMIN, BUCKET, "storage/v1/b", b"explicit XML object")
            .ok();
        assert_eq!(
            request(
                &server,
                "GET",
                "dotted.bucket.explicit.storage.example.invalid",
                "/storage/v1/b",
                VIEWER,
                b""
            )
            .ok()
            .body,
            b"explicit XML object"
        );
        let authority = format!("probe-bucket.{DOMAIN}{}", ports[0]);
        request(
            &server,
            "PUT",
            &authority,
            "/new",
            ADMIN,
            b"selected upload",
        )
        .ok();
        assert_eq!(
            request(&server, "GET", &authority, "/new", VIEWER, b"")
                .ok()
                .body,
            b"selected upload"
        );
        error(
            &request(&server, "DELETE", &authority, "/new", VIEWER, b""),
            403,
            "AccessDenied",
        );
        assert_eq!(
            request(&server, "DELETE", &authority, "/new", ADMIN, b"").status,
            204
        );
        error(
            &request(&server, "GET", &authority, "/new", VIEWER, b""),
            404,
            "NoSuchKey",
        );
        error(
            &request(&server, "GET", &authority, "/", ADMIN, b""),
            400,
            "InvalidArgument",
        );
        error(
            &request(&server, "GET", &authority, "/missing", VIEWER, b""),
            404,
            "NoSuchKey",
        );
        error(
            &request(
                &server,
                "GET",
                &format!("missing.{DOMAIN}{}", ports[0]),
                "/object",
                VIEWER,
                b"",
            ),
            404,
            "NoSuchBucket",
        );
    }
}

#[test]
fn public_origin_ip_legacy_matching_disables_only_derived_virtual_hosts() {
    for (base, matches, misses) in [
        (
            "http://127.0.0.1",
            vec!["127.0.0.1", "127.0.0.1:8443"],
            vec!["127.0.0.2"],
        ),
        ("http://[::1]", vec!["[::1]"], vec!["[::1]:80"]),
        (
            "http://[::1]:8443",
            vec!["[::1]:8443"],
            vec!["[::1]", "[::1]:80"],
        ),
    ] {
        let (_config, _data, server) = configured_fixture(&[
            "--compat-profile",
            "legacy-emulator",
            "--public-base-url",
            base,
            "--xml-virtual-host-domains",
            DOMAIN,
        ]);
        for authority in matches {
            assert_eq!(
                request(
                    &server,
                    "GET",
                    authority,
                    &format!("/{BUCKET}/object"),
                    VIEWER,
                    b""
                )
                .ok()
                .body,
                b"hello world"
            );
        }
        for authority in misses {
            let reply = request(
                &server,
                "GET",
                authority,
                &format!("/{BUCKET}/object"),
                VIEWER,
                b"",
            );
            if authority.starts_with('[') {
                error(&reply, 400, "InvalidArgument");
                assert!(String::from_utf8_lossy(&reply.body).contains("invalid bucket name"));
                request(
                    &server,
                    "GET",
                    authority,
                    "/storage/v1/b?project=project-a",
                    ADMIN,
                    b"",
                )
                .ok();
            } else {
                error(&reply, 404, "NoSuchBucket");
            }
        }
        assert_eq!(
            request(
                &server,
                "GET",
                &format!("{BUCKET}.{DOMAIN}"),
                "/object",
                VIEWER,
                b""
            )
            .ok()
            .body,
            b"hello world"
        );
        error(
            &request(
                &server,
                "GET",
                "probe-bucket.127.0.0.1",
                "/object",
                VIEWER,
                b"",
            ),
            404,
            "NoSuchBucket",
        );
    }
}

#[test]
fn public_origin_legacy_accepts_host_syntax_before_namespace_and_bucket_validation() {
    let (_config, _data, server) = configured_fixture(&[
        "--compat-profile",
        "legacy-emulator",
        "--public-base-url",
        "http://storage.example.invalid",
    ]);
    for authority in [
        "fake_gcs:4443",
        "gcs_emulator:4443",
        &format!("{}:4443", "a".repeat(64)),
    ] {
        request(
            &server,
            "GET",
            authority,
            "/storage/v1/b?project=project-a",
            ADMIN,
            b"",
        )
        .ok();
        request(&server, "GET", authority, "/_internal/ready", "", b"").ok();
        error(
            &request(&server, "GET", authority, "/object", VIEWER, b""),
            404,
            "NoSuchBucket",
        );
    }
    server.create_bucket(ADMIN, "project-a", "fake_gcs").ok();
    server
        .upload(ADMIN, "fake_gcs", "object", b"legacy underscore bucket")
        .ok();
    assert_eq!(
        request(&server, "GET", "fake_gcs:4443", "/object", VIEWER, b"")
            .ok()
            .body,
        b"legacy underscore bucket"
    );
}

#[test]
fn public_origin_legacy_cors_uses_the_selected_public_or_fallback_bucket() {
    let (_config, _data, server) = configured_fixture(&[
        "--compat-profile",
        "legacy-emulator",
        "--public-base-url",
        "http://storage.example.invalid",
    ]);
    for (bucket, origin, object) in [
        ("probe-bucket", "https://public-app.invalid", "object"),
        (
            "fallback.invalid",
            "https://fallback-app.invalid",
            "nested/object",
        ),
    ] {
        server.create_bucket(ADMIN, "project-a", bucket).ok();
        server.upload(ADMIN, bucket, object, bucket.as_bytes()).ok();
        let cors = json!({"cors":[{"origin":[origin], "method":["GET","HEAD","PUT"],
            "responseHeader":["Content-Type"], "maxAgeSeconds":120}]});
        server
            .request(
                "PATCH",
                &format!("/storage/v1/b/{bucket}"),
                ADMIN,
                cors.to_string().as_bytes(),
            )
            .ok();
    }
    for (host, path, bucket, origin) in [
        (
            DOMAIN,
            "/probe-bucket/object",
            "probe-bucket",
            "https://public-app.invalid",
        ),
        (
            "probe-bucket.storage.example.invalid",
            "/object",
            "probe-bucket",
            "https://public-app.invalid",
        ),
        (
            "fallback.invalid",
            "/nested/object",
            "fallback.invalid",
            "https://fallback-app.invalid",
        ),
    ] {
        let headers = format!("Origin: {origin}\r\n");
        let preflight = request(
            &server,
            "OPTIONS",
            host,
            path,
            &format!(
                "{headers}Access-Control-Request-Method: PUT\r\nAccess-Control-Request-Headers: Content-Type\r\n"
            ),
            b"",
        );
        preflight.ok();
        assert_eq!(preflight.headers["access-control-allow-origin"], origin);
        assert!(preflight.body.is_empty());
        let read = request(
            &server,
            "GET",
            host,
            path,
            &format!("{VIEWER}{headers}"),
            b"",
        );
        assert_eq!(read.ok().body, bucket.as_bytes());
        assert_eq!(read.headers["access-control-allow-origin"], origin);
        let denied = request(&server, "GET", host, path, &headers, b"");
        error(&denied, 401, "AuthenticationRequired");
        assert_eq!(denied.headers["access-control-allow-origin"], origin);
        let denied = request(
            &server,
            "PUT",
            host,
            path,
            &format!("{VIEWER}{headers}"),
            b"denied",
        );
        error(&denied, 403, "AccessDenied");
        assert_eq!(denied.headers["access-control-allow-origin"], origin);
        let wrong_origin = request(
            &server,
            "GET",
            host,
            path,
            &format!("{VIEWER}Origin: https://other.invalid\r\n"),
            b"",
        );
        wrong_origin.ok();
        assert!(
            !wrong_origin
                .headers
                .contains_key("access-control-allow-origin")
        );
    }
}

#[test]
fn public_origin_helpers_reach_equal_api_public_authorities_for_each_receiver() {
    for base in [
        "http://localhost:4443",
        "http://gcs:4443",
        "http://browser.example.invalid:4443",
    ] {
        let (config, _data, server) = configured_fixture(&[
            "--compat-profile",
            "legacy-emulator",
            "--public-base-url",
            base,
            "--api-base-url",
            base,
        ]);
        let file = config.0.join("helper.json");
        std::fs::write(&file, json!({"public_base_url":base}).to_string()).unwrap();
        for kind in ["public", "sign"] {
            let mut command = common::server_command();
            command.args([
                "url",
                kind,
                "--config",
                file.to_str().unwrap(),
                "--bucket",
                BUCKET,
                "--object",
                "object",
            ]);
            if kind == "sign" {
                command.args([
                    "--method",
                    "GET",
                    "--expires",
                    "60",
                    "--signer",
                    "viewer@example.invalid",
                    "--private-key",
                    "fixtures/signing/test-only-private.pem",
                ]);
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let url = String::from_utf8(output.stdout).unwrap();
            assert_eq!(
                send(
                    &server,
                    "GET",
                    url.trim(),
                    if kind == "public" { VIEWER } else { "" },
                    b""
                )
                .ok()
                .body,
                b"hello world"
            );
        }
        let authority = base.strip_prefix("http://").unwrap();
        let metadata = request(
            &server,
            "GET",
            authority,
            &format!("/storage/v1/b/{BUCKET}/o/object"),
            VIEWER,
            b"",
        );
        assert!(
            metadata.ok().json()["selfLink"]
                .as_str()
                .unwrap()
                .starts_with(base)
        );
        // The SDK's default XML read path must select the path bucket at the API authority.
        assert_eq!(
            request(
                &server,
                "GET",
                authority,
                &format!("/{BUCKET}/object"),
                VIEWER,
                b""
            )
            .ok()
            .body,
            b"hello world"
        );
    }
}

#[test]
fn public_origin_legacy_preserves_strict_signatures_permissions_pap_and_authority_errors() {
    let (_config, _data, server) = configured_fixture(&[
        "--compat-profile",
        "legacy-emulator",
        "--public-base-url",
        "http://storage.example.invalid",
        "--xml-virtual-host-domains",
        DOMAIN,
    ]);
    let authority = format!("{BUCKET}.{DOMAIN}");
    // Dotted public bucket prefixes use the measured fallback hostname bucket.
    let path = format!("/{BUCKET}/object");
    error(
        &request(&server, "GET", DOMAIN, &path, "", b""),
        401,
        "AuthenticationRequired",
    );
    error(
        &request(&server, "GET", DOMAIN, &path, NO_ROLES, b""),
        403,
        "AccessDenied",
    );
    let url = signed(&Method::GET, DOMAIN, &path, "admin@example.invalid");
    assert_eq!(
        send(&server, "GET", &url, "", b"").ok().body,
        b"hello world"
    );
    error(
        &send(
            &server,
            "GET",
            &url.replace(DOMAIN, "changed.invalid"),
            "",
            b"",
        ),
        403,
        "SignatureDoesNotMatch",
    );
    server
        .create_bucket(ADMIN, "project-a", "probe-bucket")
        .ok();
    let virtual_authority = format!("probe-bucket.{DOMAIN}:4443");
    let put = signed(
        &Method::PUT,
        &virtual_authority,
        "/signed",
        "admin@example.invalid",
    );
    send(&server, "PUT", &put, "", b"signed virtual upload").ok();
    let get = signed(
        &Method::GET,
        &virtual_authority,
        "/signed",
        "viewer@example.invalid",
    );
    assert_eq!(
        send(&server, "GET", &get, "", b"").ok().body,
        b"signed virtual upload"
    );
    error(
        &send(&server, "GET", &get.replace(":4443", ":4444"), "", b""),
        403,
        "SignatureDoesNotMatch",
    );
    error(
        &request(&server, "GET", &authority, "/object", VIEWER, b""),
        404,
        "NoSuchBucket",
    );
    let policy = json!({"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]});
    server
        .request(
            "PUT",
            &format!("/storage/v1/b/{BUCKET}/iam"),
            ADMIN,
            policy.to_string().as_bytes(),
        )
        .ok();
    request(&server, "GET", DOMAIN, &path, "", b"").ok();
    server
        .request(
            "PATCH",
            &format!("/storage/v1/b/{BUCKET}"),
            ADMIN,
            br#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
        )
        .ok();
    error(
        &request(&server, "GET", DOMAIN, &path, "", b""),
        401,
        "AuthenticationRequired",
    );
    send(&server, "GET", &url, "", b"").ok();
    for method in ["GET", "HEAD"] {
        let reply = request(
            &server,
            method,
            DOMAIN,
            &format!("http://other.invalid{path}"),
            ADMIN,
            b"",
        );
        assert_eq!(reply.status, 400);
        if method == "HEAD" {
            assert!(reply.body.is_empty());
        }
    }
    let target = url.replace(DOMAIN, "other.invalid");
    error(
        &request(&server, "GET", DOMAIN, &target, "", b""),
        403,
        "SignatureDoesNotMatch",
    );
    for headers in [
        "",
        "Host: bad host\r\n",
        "Host: storage.example.invalid\r\nHost: duplicate.invalid\r\n",
        "Host: storage.example.invalid:invalid\r\n",
    ] {
        assert_eq!(
            server
                .raw(
                    format!("GET /{BUCKET}/object HTTP/1.1\r\n{headers}Connection: close\r\n\r\n")
                        .as_bytes()
                )
                .status,
            400
        );
    }
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
        400,
        "InvalidArgument",
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
        400
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
