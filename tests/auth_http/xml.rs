use super::*;
use gcs_mimic::url::{encode_object_name, public_object_url};

const OBJECT: &str = "/iam-bucket/object";
const JSON_OBJECT: &str = "/storage/v1/b/iam-bucket/o/object";

fn fixture() -> (TestDir, TestDir, Server) {
    let (config, data, server) = iam::fixture();
    server
        .upload(ADMIN, "iam-bucket", "object", b"hello world")
        .ok();
    (config, data, server)
}

fn grant(server: &Server, member: &str, role: &str) -> Value {
    iam::set(
        server,
        ADMIN,
        &json!({"bindings":[iam::binding(role, member)]}),
    )
    .ok()
    .json()
}

fn xml_error(reply: &Reply, status: u16, code: &str) {
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
fn public_members_authorize_anonymous_and_registered_callers_without_token_fallback() {
    let (_config, _data, server) = fixture();
    for (auth, status, code) in [
        ("", 401, "AuthenticationRequired"),
        (NO_ROLES, 403, "AccessDenied"),
    ] {
        xml_error(&server.get(auth, OBJECT), status, code);
        let reply = server.get(auth, JSON_OBJECT);
        reply.error(
            status,
            if status == 401 {
                "required"
            } else {
                "forbidden"
            },
        );
    }
    grant(&server, "allUsers", "roles/storage.objectViewer");
    for auth in ["", NO_ROLES, ADMIN] {
        assert_eq!(server.get(auth, OBJECT).ok().body, b"hello world");
        server.get(auth, JSON_OBJECT).ok();
        assert_eq!(
            server
                .get(auth, &format!("{JSON_OBJECT}?alt=media"))
                .ok()
                .body,
            b"hello world"
        );
        assert_eq!(
            server.get(auth, "/storage/v1/b/iam-bucket/o").ok().json()["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    server.get("", "/storage/v1/b/iam-bucket").login_required();
    grant(&server, "allUsers", "roles/storage.admin");
    server.get("", "/storage/v1/b/iam-bucket").ok();
    server
        .get("", "/storage/v1/b?project=project-a")
        .login_required();
    server
        .create_bucket("", "project-a", "anonymous-bucket")
        .login_required();
    grant(
        &server,
        "allAuthenticatedUsers",
        "roles/storage.objectViewer",
    );
    assert_eq!(server.get(NO_ROLES, OBJECT).ok().body, b"hello world");
    server.get(NO_ROLES, JSON_OBJECT).ok();
    xml_error(&server.get("", OBJECT), 401, "AuthenticationRequired");
    server.get("", JSON_OBJECT).login_required();
    for member in ["allUsers", "allAuthenticatedUsers"] {
        grant(&server, member, "roles/storage.objectViewer");
        for auth in [
            "Authorization: Bearer unknown\r\n",
            "Authorization: Bearer\r\n",
            "Authorization: Bearer admin-token extra\r\n",
        ] {
            xml_error(&server.get(auth, OBJECT), 400, "InvalidAuthentication");
            server.get(auth, JSON_OBJECT).invalid_credentials();
        }
    }
}

#[test]
fn anonymous_object_mutations_require_each_granted_permission() {
    let (_config, _data, server) = fixture();
    server
        .upload("", "iam-bucket", "new", b"new")
        .login_required();
    server
        .multipart("", "iam-bucket", "multipart", b"new")
        .login_required();
    for method in ["PATCH", "PUT", "DELETE"] {
        server
            .request(
                method,
                JSON_OBJECT,
                "",
                if method == "DELETE" { b"" } else { b"{}" },
            )
            .login_required();
    }
    server
        .get("", "/storage/v1/b/iam-bucket/o")
        .login_required();

    grant(&server, "allUsers", "roles/storage.objectCreator");
    server.upload("", "iam-bucket", "new", b"new").ok();
    server.multipart("", "iam-bucket", "multipart", b"new").ok();
    let before = server.get(ADMIN, JSON_OBJECT).ok().json();
    server
        .upload("", "iam-bucket", "object", b"overwrite")
        .login_required();
    server
        .multipart("", "iam-bucket", "object", b"overwrite")
        .login_required();
    for method in ["PATCH", "PUT", "DELETE"] {
        server
            .request(
                method,
                JSON_OBJECT,
                "",
                if method == "DELETE" { b"" } else { b"{}" },
            )
            .login_required();
    }
    assert_eq!(server.get(ADMIN, JSON_OBJECT).ok().json(), before);
    assert_eq!(server.get(ADMIN, OBJECT).ok().body, b"hello world");

    grant(&server, "allUsers", "roles/storage.objectAdmin");
    server.upload("", "iam-bucket", "object", b"media").ok();
    server
        .multipart("", "iam-bucket", "object", b"multipart")
        .ok();
    server
        .request(
            "PATCH",
            JSON_OBJECT,
            "",
            br#"{"metadata":{"key":"patched"}}"#,
        )
        .ok();
    assert_eq!(
        server.get("", JSON_OBJECT).ok().json()["metadata"]["key"],
        "patched"
    );
    server
        .request(
            "PUT",
            JSON_OBJECT,
            "",
            br#"{"metadata":{"key":"replaced"}}"#,
        )
        .ok();
    assert_eq!(
        server.get("", JSON_OBJECT).ok().json()["metadata"],
        json!({"key":"replaced"})
    );
    assert_eq!(
        server.get("", "/storage/v1/b/iam-bucket/o").ok().json()["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    server.request("DELETE", JSON_OBJECT, "", b"").ok();
    server.get("", JSON_OBJECT).error(404, "notFound");
}

#[test]
fn anonymous_bucket_and_policy_operations_require_granted_permissions() {
    let (_config, _data, server) = iam::fixture();
    let bucket = "/storage/v1/b/iam-bucket";
    for role in [
        None,
        Some("roles/storage.objectCreator"),
        Some("roles/storage.objectAdmin"),
        Some("roles/storage.legacyBucketReader"),
    ] {
        if let Some(role) = role {
            grant(&server, "allUsers", role);
        }
        if role == Some("roles/storage.legacyBucketReader") {
            server.get("", bucket).ok();
        } else {
            server.get("", bucket).login_required();
        }
        if role == Some("roles/storage.objectAdmin") {
            server.upload("", "iam-bucket", "object", b"hello").ok();
            assert_eq!(server.get("", OBJECT).ok().body, b"hello");
            server.request("DELETE", JSON_OBJECT, "", b"").ok();
        }
        server.get("", iam::IAM).login_required();
        iam::set(&server, "", &json!({"bindings":[]})).login_required();
        server.request("DELETE", bucket, "", b"").login_required();
    }
    grant(&server, "allUsers", "roles/storage.admin");
    server.get("", bucket).ok();
    let policy = server.get("", iam::IAM).ok().json();
    iam::set(&server, "", &policy).ok();
    iam::set(&server, "", &json!({"bindings":[]})).ok();
    iam::set(&server, "", &policy).login_required();
    server.get("", iam::IAM).login_required();
    grant(&server, "allUsers", "roles/storage.admin");
    server.request("DELETE", bucket, "", b"").ok();
    server.get(ADMIN, bucket).error(404, "notFound");
}

#[test]
fn public_grants_survive_restart_and_revocation_preserves_etag_checks() {
    let (config, data, mut server) = fixture();
    let identities = identity_file(&config);
    let grant = grant(&server, "allUsers", "roles/storage.objectViewer");
    server.stop();
    let server = Server::strict(&data, &identities);
    assert_eq!(server.get("", OBJECT).ok().body, b"hello world");
    assert_eq!(server.get(ADMIN, iam::IAM).ok().json(), grant);
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[], "etag":"AAAAAAAAAAA="}),
    )
    .error(412, "conditionNotMet");
    assert_eq!(server.get("", OBJECT).ok().body, b"hello world");
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[], "etag":grant["etag"]}),
    )
    .ok();
    xml_error(&server.get("", OBJECT), 401, "AuthenticationRequired");
    server.get("", JSON_OBJECT).login_required();
    xml_error(&server.get(NO_ROLES, OBJECT), 403, "AccessDenied");
    assert_eq!(server.get(ADMIN, OBJECT).ok().body, b"hello world");
}

#[test]
fn public_revocation_racing_xml_and_json_reads_denies_subsequent_requests() {
    let (_config, _data, server) = fixture();
    for path in [OBJECT, "/storage/v1/b/iam-bucket/o/object?alt=media"] {
        for _ in 0..8 {
            grant(&server, "allUsers", "roles/storage.objectViewer");
            let barrier = std::sync::Barrier::new(2);
            thread::scope(|scope| {
                let read = scope.spawn(|| {
                    barrier.wait();
                    let result = server.get("", path);
                    assert!(matches!(result.status, 200 | 401));
                    if result.status == 200 {
                        assert_eq!(result.body, b"hello world");
                    }
                });
                barrier.wait();
                iam::set(&server, ADMIN, &json!({"bindings":[]})).ok();
                read.join().unwrap();
            });
            xml_error(&server.get("", OBJECT), 401, "AuthenticationRequired");
            server.get("", JSON_OBJECT).login_required();
        }
    }
}

#[test]
fn xml_open_reader_keeps_generation_metadata_and_bytes_during_overwrite_and_revocation() {
    let (_config, _data, server) = fixture();
    let old = server
        .upload(ADMIN, "iam-bucket", "object", &vec![b'a'; 8 * 1024 * 1024])
        .ok()
        .json();
    grant(&server, "allUsers", "roles/storage.objectViewer");
    let mut stream = server.stream();
    stream
        .write_all(b"GET /iam-bucket/object HTTP/1.1\r\nHost: local\r\nRange: bytes=1-\r\n\r\n")
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        assert_ne!(reader.read_until(b'\n', &mut bytes).unwrap(), 0);
    }
    let headers = String::from_utf8(bytes).unwrap();
    assert!(headers.starts_with("HTTP/1.1 206"));
    assert!(headers.contains(&format!(
        "x-goog-generation: {}\r\n",
        old["generation"].as_str().unwrap()
    )));
    assert!(headers.contains("cache-control: public, max-age=3600\r\n"));
    let replacement = server
        .upload(ADMIN, "iam-bucket", "object", b"replacement")
        .ok()
        .json();
    assert_ne!(replacement["generation"], old["generation"]);
    iam::set(&server, ADMIN, &json!({"bindings":[]})).ok();
    let mut body = Vec::new();
    reader.read_to_end(&mut body).unwrap();
    assert_eq!(body.len(), 8 * 1024 * 1024 - 1);
    assert!(body.iter().all(|byte| *byte == b'a'));
    xml_error(&server.get("", OBJECT), 401, "AuthenticationRequired");
    assert_eq!(server.get(ADMIN, OBJECT).ok().body, b"replacement");
}

#[test]
fn xml_head_matches_get_headers_and_selected_metadata() {
    let (_config, _data, server) = fixture();
    grant(&server, "allUsers", "roles/storage.objectViewer");
    server.request("PATCH", JSON_OBJECT, ADMIN, br#"{"contentType":"text/plain","contentDisposition":"attachment; filename=hello.txt","contentLanguage":"en","metadata":{"color":"blue"}}"#).ok();
    let get = server.get("", OBJECT);
    let head = server.request("HEAD", OBJECT, "", b"");
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    let mut get_headers = get.headers.clone();
    let mut head_headers = head.headers.clone();
    get_headers.remove("date");
    head_headers.remove("date");
    // The HEAD helper asks the server to close the connection, which hyper echoes.
    assert_eq!(head_headers.remove("connection").as_deref(), Some("close"));
    assert_eq!(get_headers, head_headers);
    assert_eq!(get.headers["etag"], "\"5eb63bbbe01eeed093cb22bb8f5acdc3\"");
    assert_eq!(get.headers["content-type"], "text/plain");
    assert_eq!(
        get.headers["content-disposition"],
        "attachment; filename=hello.txt"
    );
    assert_eq!(get.headers["content-language"], "en");
    assert_eq!(get.headers["x-goog-meta-color"], "blue");
    assert_eq!(get.headers["x-goog-storage-class"], "STANDARD");
    assert_eq!(get.headers["x-goog-stored-content-length"], "11");
    assert_eq!(get.headers["x-goog-stored-content-encoding"], "identity");
    assert_eq!(get.headers["accept-ranges"], "bytes");
    assert_eq!(get.headers["x-goog-metageneration"], "2");
    assert_eq!(
        get.headers["x-goog-hash"],
        "crc32c=yZRlqg==,md5=XrY7u+Ae7tCTyyK7j1rNww=="
    );
    assert!(get.headers["last-modified"].ends_with(" GMT"));
    assert_eq!(get.headers["last-modified"].len(), 29);
    let partial = server.get(
        "",
        &format!("{OBJECT}?generation={}", get.headers["x-goog-generation"]),
    );
    assert_eq!(partial.body, get.body);
    let failed = server.request("HEAD", "/iam-bucket/missing", "", b"");
    assert_eq!(failed.status, 404);
    assert!(failed.body.is_empty());
}

#[test]
fn xml_ranges_generation_and_preconditions_use_one_live_generation() {
    let (_config, _data, server) = fixture();
    grant(&server, "allUsers", "roles/storage.objectViewer");
    let generation = server.get("", OBJECT).headers["x-goog-generation"].clone();
    for (range, expected, content_range) in [
        ("bytes=0-4", &b"hello"[..], "bytes 0-4/11"),
        ("bytes=6-", &b"world"[..], "bytes 6-10/11"),
        ("bytes=-5", &b"world"[..], "bytes 6-10/11"),
        ("bytes=0-999", &b"hello world"[..], "bytes 0-10/11"),
    ] {
        let reply = server.get(
            &format!("Range: {range}\r\n"),
            &format!("{OBJECT}?generation={generation}"),
        );
        assert_eq!(reply.status, 206);
        assert_eq!(reply.body, expected);
        assert_eq!(reply.headers["content-range"], content_range);
        assert_eq!(reply.headers["x-goog-generation"], generation);
        assert_eq!(reply.headers["x-goog-stored-content-length"], "11");
        let head = server.request("HEAD", OBJECT, &format!("Range: {range}\r\n"), b"");
        assert_eq!(head.status, 200);
        assert_eq!(head.headers["content-range"], content_range);
        assert_eq!(head.headers["content-length"], "11");
    }
    let reply = server.get("Range: bytes=11-\r\n", OBJECT);
    xml_error(&reply, 416, "InvalidRange");
    assert_eq!(reply.headers["content-range"], "bytes */11");
    let head = server.request("HEAD", OBJECT, "Range: bytes=11-\r\n", b"");
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(head.headers["content-range"], "bytes */11");
    assert_eq!(head.headers["content-length"], "11");
    assert_eq!(
        server.get("Range: bytes=1-0\r\n", OBJECT).ok().body,
        b"hello world"
    );
    server
        .get(
            &format!(
                "x-goog-if-generation-match: {generation}\r\nx-goog-if-metageneration-match: 1\r\n"
            ),
            OBJECT,
        )
        .ok();
    for headers in [
        "x-goog-if-generation-match: 0\r\n".to_owned(),
        format!(
            "x-goog-if-generation-match: {generation}\r\nx-goog-if-metageneration-match: 2\r\n"
        ),
    ] {
        xml_error(&server.get(&headers, OBJECT), 412, "PreconditionFailed");
    }
    xml_error(
        &server.get("", &format!("{OBJECT}?generation=1")),
        404,
        "NoSuchKey",
    );
    server
        .upload(ADMIN, "iam-bucket", "object", b"replacement")
        .ok();
    xml_error(
        &server.get("", &format!("{OBJECT}?generation={generation}")),
        404,
        "NoSuchKey",
    );
    assert_eq!(server.get("", OBJECT).ok().body, b"replacement");
}

#[test]
fn xml_missing_resources_follow_effective_permission_disclosure() {
    let (_config, _data, server) = fixture();
    for auth in ["", NO_ROLES] {
        for path in ["/missing-bucket/object", "/iam-bucket/missing"] {
            xml_error(
                &server.get(auth, path),
                if auth.is_empty() { 401 } else { 403 },
                if auth.is_empty() {
                    "AuthenticationRequired"
                } else {
                    "AccessDenied"
                },
            );
        }
    }
    xml_error(
        &server.get(ADMIN, "/missing-bucket/object"),
        404,
        "NoSuchBucket",
    );
    xml_error(&server.get(ADMIN, "/iam-bucket/missing"), 404, "NoSuchKey");
    grant(&server, "allUsers", "roles/storage.objectViewer");
    xml_error(&server.get("", "/iam-bucket/missing"), 404, "NoSuchKey");
    server
        .get("", "/storage/v1/b/missing-bucket/o/object")
        .login_required();
}

#[test]
fn xml_public_urls_round_trip_raw_encoded_names_without_normalization() {
    let (_config, _data, server) = fixture();
    let url = public_object_url("http://local", "iam-bucket", "object").unwrap();
    xml_error(
        &server.get("", url.strip_prefix("http://local").unwrap()),
        401,
        "AuthenticationRequired",
    );
    grant(&server, "allUsers", "roles/storage.objectViewer");
    for name in [
        "a b",
        "a+b",
        "100%",
        "é猫",
        "a/b",
        "literal%2F",
        "a/../b",
        "./x",
        "x/.",
        "a//b/",
        "/a",
        "query?#",
    ] {
        server
            .upload(
                ADMIN,
                "iam-bucket",
                &encode_object_name(name),
                name.as_bytes(),
            )
            .ok();
        let url = public_object_url("http://local", "iam-bucket", name).unwrap();
        assert_eq!(
            server
                .get("", url.strip_prefix("http://local").unwrap())
                .ok()
                .body,
            name.as_bytes(),
            "{name}"
        );
    }
    assert_eq!(server.get("", "/iam-bucket/a+b").ok().body, b"a+b");
    assert_eq!(server.get("", "/iam-bucket/a/../b").ok().body, b"a/../b");
    for name in [
        "%",
        "%GG",
        "%ff",
        "%C0%AF",
        "%0A",
        ".",
        "..",
        ".well-known/acme-challenge/x",
    ] {
        xml_error(
            &server.get("", &format!("/iam-bucket/{name}")),
            400,
            "InvalidArgument",
        );
    }
    // Reserved JSON namespaces retain their original error format and route precedence.
    assert_eq!(
        server
            .get("", "/storage/v1/b/iam-bucket/o/object")
            .ok()
            .json()["name"],
        "object"
    );
    assert_eq!(
        server
            .get("", "/download/storage/v1/b/iam-bucket/o/object?alt=media")
            .ok()
            .body,
        b"hello world"
    );
    assert_eq!(server.get("", "/storage/v1/invalid").status, 404);
}

#[test]
fn xml_invalid_bucket_paths_keep_json_unknown_route_errors() {
    let (_config, _data, server) = fixture();
    for path in [
        "/INVALID/object",
        "/ab/object",
        "/goog-bucket/object",
        "/1.2.3.4/object",
    ] {
        let reply = server.get("", path);
        assert_eq!(reply.status, 404, "{path}");
        assert_eq!(reply.headers["content-type"], "application/json");
        assert_eq!(reply.json(), json!({"error":"route not implemented"}));
    }
}

#[test]
fn xml_bucket_paths_reserve_only_api_namespaces() {
    let (_config, _data, server) = fixture();
    for bucket in ["batch", "storage", "upload", "download"] {
        server.create_bucket(ADMIN, "project-a", bucket).ok();
    }
    for (bucket, name) in [
        ("batch", "x"),
        ("batch", "storage/v1-other/x"),
        ("storage", "v1-other/x"),
        ("upload", "storage/v1-other/x"),
        ("download", "storage/v1-other/x"),
    ] {
        server
            .upload(ADMIN, bucket, &encode_object_name(name), b"bytes")
            .ok();
        let url = public_object_url("http://local", bucket, name).unwrap();
        assert_eq!(
            server
                .get(ADMIN, url.strip_prefix("http://local").unwrap())
                .ok()
                .body,
            b"bytes"
        );
    }
    for path in [
        "/batch/storage/v1",
        "/batch/storage/v1/x",
        "/storage/v1/x",
        "/upload/storage/v1/x",
        "/download/storage/v1/x",
        "/_internal/x",
    ] {
        let reply = server.get(ADMIN, path);
        assert_eq!(reply.status, 404, "{path}");
        assert_eq!(reply.headers["content-type"], "application/json");
        assert_eq!(reply.json(), json!({"error":"route not implemented"}));
    }
}

#[test]
fn xml_unsupported_operations_fail_before_reading_a_body() {
    let (_config, _data, server) = fixture();
    grant(&server, "allUsers", "roles/storage.objectViewer");
    for (method, path) in [
        ("GET", "/iam-bucket"),
        ("GET", "/iam-bucket/"),
        ("PUT", "/iam-bucket/object?acl"),
        ("POST", OBJECT),
        ("OPTIONS", OBJECT),
        ("GET", "/iam-bucket/object?acl"),
        ("GET", "/iam-bucket/object?uploads"),
        ("GET", "/iam-bucket/object?response-cache-control=no-cache"),
    ] {
        let request =
            format!("{method} {path} HTTP/1.1\r\nHost: local\r\nContent-Length: 999999\r\n\r\n");
        xml_error(&server.raw(request.as_bytes()), 400, "InvalidArgument");
    }
    assert_eq!(server.get("", OBJECT).ok().body, b"hello world");
}

#[test]
fn xml_delete_checks_permissions_conditions_and_empty_body() {
    let (_config, _data, server) = fixture();
    for auth in [VIEWER, CREATOR] {
        xml_error(
            &server.request("DELETE", OBJECT, auth, b""),
            403,
            "AccessDenied",
        );
    }
    xml_error(
        &server.request(
            "DELETE",
            OBJECT,
            &format!("{ADMIN}x-goog-if-generation-match: 1\r\n"),
            b"",
        ),
        412,
        "PreconditionFailed",
    );
    let request =
        format!("DELETE {OBJECT} HTTP/1.1\r\nHost: local\r\n{ADMIN}Content-Length: 999999\r\n\r\n");
    xml_error(&server.raw(request.as_bytes()), 400, "UnexpectedContent");
    assert_eq!(server.get(ADMIN, OBJECT).ok().body, b"hello world");
    assert_eq!(server.request("DELETE", OBJECT, ADMIN, b"").status, 204);
    xml_error(&server.get(ADMIN, OBJECT), 404, "NoSuchKey");
}

#[test]
fn xml_go_sdk_read_headers_accept_telemetry_without_replaying_reads() {
    let (_config, _data, server) = fixture();
    grant(&server, "allUsers", "roles/storage.objectViewer");
    server
        .upload(ADMIN, "iam-bucket", "dir%2Ffixture.bin", b"hello world")
        .ok();
    let headers = concat!(
        "User-Agent: Go-http-client/1.1\r\n",
        "Accept-Encoding: gzip\r\n",
        "X-Goog-Api-Client: gccl-invocation-id/8bf3a9fb-418c-4a43-867c-b463a65f1c85 gccl-attempt-count/1 gl-go/1.27.3 gccl/1.70.0\r\n",
        "X-Goog-Gcs-Idempotency-Token: 8bf3a9fb-418c-4a43-867c-b463a65f1c85\r\n",
    );
    let path = "/iam-bucket/dir%2Ffixture.bin";
    let reply = server.get(headers, path);
    assert_eq!(reply.ok().body, b"hello world");
    let head = server.request("HEAD", path, headers, b"");
    assert_eq!(head.status, 200);
    assert_eq!(head.headers["content-length"], "11");
    let conditions = format!(
        "{headers}Range: bytes=6-10\r\nx-goog-if-generation-match: {}\r\nx-goog-if-metageneration-match: 1\r\n",
        reply.headers["x-goog-generation"]
    );
    let range = server.get(&conditions, path);
    assert_eq!(range.status, 206);
    assert_eq!(range.body, b"world");
    server
        .upload(ADMIN, "iam-bucket", "dir%2Ffixture.bin", b"replacement")
        .ok();
    assert_eq!(server.get(headers, path).ok().body, b"replacement");
    iam::set(&server, ADMIN, &json!({"bindings":[]})).ok();
    xml_error(&server.get(headers, path), 401, "AuthenticationRequired");
}

#[test]
fn xml_read_idempotency_tokens_require_one_nonempty_value() {
    let (_config, _data, server) = fixture();
    grant(&server, "allUsers", "roles/storage.objectViewer");
    for (headers, code) in [
        ("x-goog-gcs-idempotency-token:\r\n", "InvalidArgument"),
        ("x-goog-gcs-idempotency-token: \t \r\n", "InvalidArgument"),
        (
            "x-goog-gcs-idempotency-token: first\r\nX-Goog-Gcs-Idempotency-Token: second\r\n",
            "ExcessHeaderValues",
        ),
        (
            "x-goog-gcs-idempotency-token:\r\nx-goog-gcs-idempotency-token: second\r\n",
            "ExcessHeaderValues",
        ),
    ] {
        xml_error(&server.get(headers, OBJECT), 400, code);
        let head = server.request("HEAD", OBJECT, headers, b"");
        assert_eq!(head.status, 400);
        assert_eq!(
            head.headers["content-type"],
            "application/xml; charset=UTF-8"
        );
        assert!(head.body.is_empty());
    }
    assert_eq!(server.get("", OBJECT).ok().body, b"hello world");
    assert_eq!(
        server
            .get("x-goog-gcs-idempotency-token: valid\r\n", OBJECT)
            .ok()
            .body,
        b"hello world"
    );
}

#[test]
fn xml_signatures_and_unsupported_headers_never_fall_back_to_public_access() {
    let (_config, _data, server) = fixture();
    grant(&server, "allUsers", "roles/storage.objectViewer");
    for query in [
        "X-Goog-Signature=x",
        "X-Goog-Algorithm=GOOG4-RSA-SHA256",
        "%58-Goog-Signature=x",
        "Signature=x&GoogleAccessId=local",
    ] {
        xml_error(
            &server.get("", &format!("{OBJECT}?{query}")),
            400,
            "InvalidAuthentication",
        );
    }
    for scheme in ["GOOG4-RSA-SHA256", "GOOG4-HMAC-SHA256", "GOOG1", "AWS"] {
        xml_error(
            &server.get(&format!("Authorization: {scheme} value\r\n"), OBJECT),
            400,
            "InvalidAuthentication",
        );
    }
    for header in [
        "x-goog-encryption-key: key",
        "x-goog-encryption-key-sha256: hash",
        "x-goog-encryption-algorithm: AES256",
        "x-goog-user-project: p",
        "If-Match: *",
        "If-None-Match: *",
        "If-Modified-Since: Fri, 19 Feb 2010 22:04:23 GMT",
        "If-Unmodified-Since: Fri, 19 Feb 2010 22:04:23 GMT",
        "If-Range: *",
        "x-goog-if-generation-not-match: 1",
    ] {
        xml_error(
            &server.get(&format!("{header}\r\n"), OBJECT),
            400,
            "InvalidArgument",
        );
    }
    server.get("Unknown-Header: ignored\r\n", OBJECT).ok();
    server
        .get("x-goog-api-client: gl-go/1.27.3 gccl/1.70.0\r\n", OBJECT)
        .ok();
    for headers in [
        "x-goog-if-generation-match: invalid\r\n",
        "x-goog-if-generation-match: -1\r\n",
        "x-goog-if-metageneration-match: 0\r\n",
    ] {
        xml_error(&server.get(headers, OBJECT), 400, "InvalidArgument");
    }
    xml_error(
        &server.get(
            "x-goog-if-generation-match: 1\r\nx-goog-if-generation-match: 2\r\n",
            OBJECT,
        ),
        400,
        "ExcessHeaderValues",
    );
}

#[test]
fn xml_unrepresentable_or_case_colliding_custom_metadata_fails_explicitly() {
    let (_config, _data, server) = fixture();
    grant(&server, "allUsers", "roles/storage.objectViewer");
    for metadata in [
        json!({"bad key":"value"}),
        json!({"key":"line\r\nbreak"}),
        json!({"Color":"blue", "color":"red"}),
    ] {
        server
            .request(
                "PUT",
                JSON_OBJECT,
                ADMIN,
                json!({"metadata":metadata}).to_string().as_bytes(),
            )
            .ok();
        xml_error(&server.get("", OBJECT), 400, "InvalidArgument");
        assert_eq!(
            server.get("", JSON_OBJECT).ok().json()["metadata"],
            metadata
        );
        let head = server.request("HEAD", OBJECT, "", b"");
        assert_eq!(head.status, 400);
        assert!(head.body.is_empty());
    }
}

#[test]
fn xml_cache_control_respects_metadata_and_distinguishes_public_from_authenticated() {
    let (_config, _data, server) = fixture();
    assert_eq!(
        server.get(ADMIN, OBJECT).ok().headers["cache-control"],
        "private, max-age=0"
    );
    assert_eq!(
        server
            .get(ADMIN, &format!("{JSON_OBJECT}?alt=media"))
            .ok()
            .headers["cache-control"],
        "no-cache"
    );
    grant(
        &server,
        "allAuthenticatedUsers",
        "roles/storage.objectViewer",
    );
    assert_eq!(
        server.get(NO_ROLES, OBJECT).ok().headers["cache-control"],
        "private, max-age=0"
    );
    grant(&server, "allUsers", "roles/storage.objectViewer");
    for path in [OBJECT.to_owned(), format!("{JSON_OBJECT}?alt=media")] {
        assert_eq!(
            server.get("", &path).ok().headers["cache-control"],
            "public, max-age=3600"
        );
    }
    server
        .request(
            "PATCH",
            JSON_OBJECT,
            ADMIN,
            br#"{"cacheControl":"public, max-age=60"}"#,
        )
        .ok();
    assert_eq!(
        server
            .get("Cache-Control: no-store\r\n", OBJECT)
            .ok()
            .headers["cache-control"],
        "public, max-age=60"
    );
    iam::set(&server, ADMIN, &json!({"bindings":[]})).ok();
    assert_eq!(
        server.get(ADMIN, OBJECT).ok().headers["cache-control"],
        "public, max-age=60"
    );
    assert_eq!(
        server
            .get(ADMIN, &format!("{JSON_OBJECT}?alt=media"))
            .ok()
            .headers["cache-control"],
        "no-cache"
    );
}

#[test]
fn xml_permissive_reads_ignore_credentials_without_granting_public_policy() {
    let data = TestDir::new();
    let (server, _) = Server::start(&data, &[]);
    server.create_bucket("", "project-a", "iam-bucket").ok();
    server
        .upload("", "iam-bucket", "object", b"hello world")
        .ok();
    assert_eq!(
        server.get("Authorization: nonsense\r\n", OBJECT).ok().body,
        b"hello world"
    );
    assert_eq!(
        server.get("", OBJECT).ok().headers["cache-control"],
        "private, max-age=0"
    );
    xml_error(
        &server.get("", "/missing-bucket/object"),
        404,
        "NoSuchBucket",
    );
}

#[test]
fn uniform_access_settings_fail_closed() {
    let (_config, _data, server) = fixture();
    for (index, setting) in [
        json!({"bucketPolicyOnly":{"enabled":true}}),
        json!({"uniformBucketLevelAccess":{"enabled":true}}),
    ]
    .into_iter()
    .enumerate()
    {
        server
            .request(
                "POST",
                "/storage/v1/b?project=project-a",
                ADMIN,
                json!({"name":format!("restricted-bucket-{index}"), "iamConfiguration":setting})
                    .to_string()
                    .as_bytes(),
            )
            .ok();
        for method in ["PATCH", "PUT"] {
            server
                .request(
                    method,
                    "/storage/v1/b/iam-bucket",
                    ADMIN,
                    json!({"iamConfiguration":setting}).to_string().as_bytes(),
                )
                .ok();
        }
    }
    assert_eq!(server.get(ADMIN, OBJECT).ok().body, b"hello world");
    xml_error(&server.get("", OBJECT), 401, "AuthenticationRequired");
}

#[test]
fn uniform_access_metadata_and_predefined_acl_rejections() {
    let (_config, _data, server) = fixture();
    let path = "/storage/v1/b/iam-bucket";
    let conflicting = server.request("PATCH", path, ADMIN, br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false},"bucketPolicyOnly":{"enabled":true}}}"#).ok().json();
    assert_eq!(
        conflicting["iamConfiguration"]["uniformBucketLevelAccess"]["enabled"],
        false
    );
    for value in [
        Value::Null,
        json!({}),
        json!({"enabled":false}),
        json!({"enabled":null}),
    ] {
        let result = server
            .request(
                "PATCH",
                path,
                ADMIN,
                json!({"iamConfiguration":{"uniformBucketLevelAccess":value}})
                    .to_string()
                    .as_bytes(),
            )
            .ok()
            .json();
        assert_eq!(
            result["iamConfiguration"]["uniformBucketLevelAccess"],
            json!({"enabled":false})
        );
    }
    let enabled = server
        .request(
            "PATCH",
            path,
            ADMIN,
            br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}"#,
        )
        .ok()
        .json();
    let config = &enabled["iamConfiguration"];
    assert_eq!(config["uniformBucketLevelAccess"]["enabled"], true);
    assert!(config["uniformBucketLevelAccess"]["lockedTime"].is_string());
    assert_eq!(
        config["bucketPolicyOnly"],
        config["uniformBucketLevelAccess"]
    );
    assert_eq!(server.get(ADMIN, path).ok().json(), enabled);
    let listed = server
        .get(ADMIN, "/storage/v1/b?project=project-a")
        .ok()
        .json();
    assert!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == &enabled)
    );
    for method in ["PATCH", "PUT"] {
        server
            .request(
                method,
                path,
                VIEWER,
                br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false}}}"#,
            )
            .error(403, "forbidden");
        server
            .request(
                method,
                &format!("{path}?ifMetagenerationMatch=0"),
                ADMIN,
                br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false}}}"#,
            )
            .error(412, "conditionNotMet");
        server.request(method, path, ADMIN, br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":[]},"bucketPolicyOnly":{"enabled":false}}}"#).error(400, "invalidArgument");
        for key in ["predefinedAcl", "predefinedDefaultObjectAcl"] {
            let reply = server.request(method, &format!("{path}?{key}=publicRead"), ADMIN, b"{}");
            reply.error(400, "invalidArgument");
            assert!(
                reply.json()["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("remove {key}"))
            );
        }
    }
    for key in ["predefinedAcl", "predefinedDefaultObjectAcl"] {
        server.request("POST", &format!("/storage/v1/b?project=project-a&{key}=private"), ADMIN, br#"{"name":"rejected-create","iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}"#).error(400, "invalidArgument");
        server
            .get(ADMIN, "/storage/v1/b/rejected-create")
            .error(404, "notFound");
    }
    let routes = [
        "/upload/storage/v1/b/iam-bucket/o?uploadType=media&name=rejected&predefinedAcl=publicRead",
        "/upload/storage/v1/b/iam-bucket/o?uploadType=multipart&predefinedAcl=publicRead",
        "/upload/storage/v1/b/iam-bucket/o?uploadType=resumable&name=rejected&predefinedAcl=publicRead",
        "/storage/v1/b/iam-bucket/o/object/copyTo/b/iam-bucket/o/rejected?destinationPredefinedAcl=publicRead",
        "/storage/v1/b/iam-bucket/o/object/rewriteTo/b/iam-bucket/o/rejected?destinationPredefinedAcl=publicRead",
        "/storage/v1/b/iam-bucket/o/rejected/compose?destinationPredefinedAcl=publicRead",
        "/upload/storage/v1/b/iam-bucket/o?uploadType=media&name=rejected&%70redefinedAcl=publicRead",
    ];
    for route in routes {
        let reply = server.request("POST", route, ADMIN, b"{}");
        reply.error(400, "invalidArgument");
        assert!(
            reply.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("remove ")
        );
    }
    server
        .get(ADMIN, "/storage/v1/b/iam-bucket/o/rejected")
        .error(404, "notFound");
    assert_eq!(server.get(ADMIN, path).ok().json(), enabled);
    server
        .request(
            "PATCH",
            path,
            ADMIN,
            br#"{"iamConfiguration":{"bucketPolicyOnly":{"enabled":false}}}"#,
        )
        .ok();
    for route in routes {
        let reply = server.request("POST", route, ADMIN, b"{}");
        assert_eq!(
            reply.json()["error"]["errors"][0]["reason"],
            gcs_mimic::json::JsonError::unsupported_object("predefinedAcl").reason
        );
    }
    let terraform = server.request("POST", "/storage/v1/b?project=project-a", ADMIN, br#"{"name":"terraform-ubla","location":"US","iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true},"publicAccessPrevention":"enforced"}}"#).ok().json();
    assert_eq!(
        terraform["iamConfiguration"]["publicAccessPrevention"],
        "enforced"
    );
    assert_eq!(
        terraform["iamConfiguration"]["uniformBucketLevelAccess"]["enabled"],
        true
    );
}
