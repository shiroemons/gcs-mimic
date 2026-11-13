use super::*;
use gcs_mimic::url::{encode_object_name, public_object_url};

const OBJECT: &str = "/iam-bucket/object";
const JSON_OBJECT: &str = "/storage/v1/b/iam-bucket/o/object";

fn fixture() -> (TestDir, TestDir, Server) {
    let (config, data, server) = iam::fixture();
    server
        .upload(ADMIN, "iam-bucket", "object", b"hello world")
        .success();
    (config, data, server)
}

fn grant(server: &Server, member: &str, role: &str) -> Value {
    iam::set(
        server,
        ADMIN,
        &json!({"bindings":[iam::binding(role, member)]}),
    )
    .success()
    .json()
}

#[test]
fn xml_cache_validators_authorize_before_comparing_one_selected_generation() {
    let (_config, _data, server) = fixture();
    xml_error(
        &server.get("If-None-Match: *\r\n", OBJECT),
        401,
        "AuthenticationRequired",
    );
    grant(&server, "allUsers", "roles/storage.objectViewer");
    let selected = server.get("", OBJECT);
    selected.success();
    let etag = &selected.headers["etag"];
    let modified = &selected.headers["last-modified"];
    for method in ["GET", "HEAD"] {
        for condition in [
            format!("If-None-Match: W/{etag}\r\n"),
            format!("If-Modified-Since: {modified}\r\n"),
        ] {
            let response = server.request(method, OBJECT, &condition, b"");
            response.expect(304);
            assert!(response.body.is_empty());
            assert_eq!(&response.headers["etag"], etag);
            assert_eq!(response.headers["cache-control"], "public, max-age=3600");
            assert_eq!(&response.headers["last-modified"], modified);
        }
        server
            .request(
                method,
                OBJECT,
                &format!("If-Match: \"other\"\r\nIf-None-Match: {etag}\r\n"),
                b"",
            )
            .expect(412);
    }
    server
        .get(
            "If-None-Match: \"other\"\r\nIf-Modified-Since: Sun, 06 Nov 2094 08:49:37 GMT\r\n",
            OBJECT,
        )
        .success();
    let generation = server.get(ADMIN, JSON_OBJECT).success().json()["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    server
        .request(
            "PATCH",
            "/storage/v1/b/iam-bucket",
            ADMIN,
            br#"{"versioning":{"enabled":true}}"#,
        )
        .success();
    server
        .upload(ADMIN, "iam-bucket", "object", b"new generation")
        .success();
    server
        .get(&format!("If-None-Match: {etag}\r\n"), OBJECT)
        .success();
    server
        .get(
            &format!("If-None-Match: {etag}\r\n"),
            &format!("{OBJECT}?generation={generation}"),
        )
        .expect(304);
}

use crate::common::errors::xml_error_with_content_type as xml_error;

#[test]
fn public_members_authorize_anonymous_and_registered_callers_without_token_fallback() {
    let (_config, _data, server) = fixture();
    for (auth, status, code) in [
        ("", 401, "AuthenticationRequired"),
        (NO_ROLES, 403, "AccessDenied"),
    ] {
        xml_error(&server.get(auth, OBJECT), status, code);
        let reply = server.get(auth, JSON_OBJECT);
        reply.auth_error(
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
        assert_eq!(server.get(auth, OBJECT).success().body, b"hello world");
        server.get(auth, JSON_OBJECT).success();
        assert_eq!(
            server
                .get(auth, &format!("{JSON_OBJECT}?alt=media"))
                .success()
                .body,
            b"hello world"
        );
        assert_eq!(
            server
                .get(auth, "/storage/v1/b/iam-bucket/o")
                .success()
                .json()["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    server
        .get("", "/storage/v1/b/iam-bucket")
        .anonymous_denied("storage.buckets.get", "bucket");
    grant(&server, "allUsers", "roles/storage.admin");
    server.get("", "/storage/v1/b/iam-bucket").success();
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
    assert_eq!(server.get(NO_ROLES, OBJECT).success().body, b"hello world");
    server.get(NO_ROLES, JSON_OBJECT).success();
    xml_error(&server.get("", OBJECT), 401, "AuthenticationRequired");
    server
        .get("", JSON_OBJECT)
        .anonymous_denied("storage.objects.get", "object");
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
    let mutation = |method: &str| {
        let (permission, body): (_, &[u8]) = if method == "DELETE" {
            ("storage.objects.delete", b"")
        } else {
            ("storage.objects.update", b"{}")
        };
        (server.request(method, JSON_OBJECT, "", body), permission)
    };
    server
        .upload("", "iam-bucket", "new", b"new")
        .anonymous_denied("storage.objects.create", "object");
    server
        .multipart("", "iam-bucket", "multipart", b"new")
        .anonymous_denied("storage.objects.create", "object");
    for method in ["PATCH", "PUT", "DELETE"] {
        let (reply, permission) = mutation(method);
        reply.anonymous_denied(permission, "object");
    }
    server
        .get("", "/storage/v1/b/iam-bucket/o")
        .anonymous_denied("storage.objects.list", "bucket");

    grant(&server, "allUsers", "roles/storage.objectCreator");
    server.upload("", "iam-bucket", "new", b"new").success();
    server
        .multipart("", "iam-bucket", "multipart", b"new")
        .success();
    let before = server.get(ADMIN, JSON_OBJECT).success().json();
    // Replacing a live object also needs storage.objects.delete, which the denial names.
    server
        .upload("", "iam-bucket", "object", b"overwrite")
        .anonymous_denied("storage.objects.delete", "object");
    server
        .multipart("", "iam-bucket", "object", b"overwrite")
        .anonymous_denied("storage.objects.delete", "object");
    for method in ["PATCH", "PUT", "DELETE"] {
        let (reply, permission) = mutation(method);
        reply.anonymous_denied(permission, "object");
    }
    assert_eq!(server.get(ADMIN, JSON_OBJECT).success().json(), before);
    assert_eq!(server.get(ADMIN, OBJECT).success().body, b"hello world");

    grant(&server, "allUsers", "roles/storage.objectAdmin");
    server
        .upload("", "iam-bucket", "object", b"media")
        .success();
    server
        .multipart("", "iam-bucket", "object", b"multipart")
        .success();
    server
        .request(
            "PATCH",
            JSON_OBJECT,
            "",
            br#"{"metadata":{"key":"patched"}}"#,
        )
        .success();
    assert_eq!(
        server.get("", JSON_OBJECT).success().json()["metadata"]["key"],
        "patched"
    );
    server
        .request(
            "PUT",
            JSON_OBJECT,
            "",
            br#"{"metadata":{"key":"replaced"}}"#,
        )
        .success();
    assert_eq!(
        server.get("", JSON_OBJECT).success().json()["metadata"],
        json!({"key":"replaced"})
    );
    assert_eq!(
        server
            .get("", "/storage/v1/b/iam-bucket/o")
            .success()
            .json()["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    server.request("DELETE", JSON_OBJECT, "", b"").success();
    server.get("", JSON_OBJECT).auth_error(404, "notFound");
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
            server.get("", bucket).success();
        } else {
            server
                .get("", bucket)
                .anonymous_denied("storage.buckets.get", "bucket");
        }
        if role == Some("roles/storage.objectAdmin") {
            server
                .upload("", "iam-bucket", "object", b"hello")
                .success();
            assert_eq!(server.get("", OBJECT).success().body, b"hello");
            server.request("DELETE", JSON_OBJECT, "", b"").success();
        }
        server
            .get("", iam::IAM)
            .anonymous_denied("storage.buckets.getIamPolicy", "bucket");
        iam::set(&server, "", &json!({"bindings":[]}))
            .anonymous_denied("storage.buckets.setIamPolicy", "bucket");
        server
            .request("DELETE", bucket, "", b"")
            .anonymous_denied("storage.buckets.delete", "bucket");
    }
    grant(&server, "allUsers", "roles/storage.admin");
    server.get("", bucket).success();
    let policy = server.get("", iam::IAM).success().json();
    iam::set(&server, "", &policy).success();
    iam::set(&server, "", &json!({"bindings":[]})).success();
    iam::set(&server, "", &policy).anonymous_denied("storage.buckets.setIamPolicy", "bucket");
    server
        .get("", iam::IAM)
        .anonymous_denied("storage.buckets.getIamPolicy", "bucket");
    grant(&server, "allUsers", "roles/storage.admin");
    server.request("DELETE", bucket, "", b"").success();
    server.get(ADMIN, bucket).auth_error(404, "notFound");
}

#[test]
fn public_grants_survive_restart_and_revocation_preserves_etag_checks() {
    let (config, data, mut server) = fixture();
    let identities = identity_file(&config);
    let grant = grant(&server, "allUsers", "roles/storage.objectViewer");
    server.stop();
    let server = Server::strict(&data, &identities);
    assert_eq!(server.get("", OBJECT).success().body, b"hello world");
    assert_eq!(server.get(ADMIN, iam::IAM).success().json(), grant);
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[], "etag":"AAAAAAAAAAA="}),
    )
    .auth_error(412, "conditionNotMet");
    assert_eq!(server.get("", OBJECT).success().body, b"hello world");
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[], "etag":grant["etag"]}),
    )
    .success();
    xml_error(&server.get("", OBJECT), 401, "AuthenticationRequired");
    server
        .get("", JSON_OBJECT)
        .anonymous_denied("storage.objects.get", "object");
    xml_error(&server.get(NO_ROLES, OBJECT), 403, "AccessDenied");
    assert_eq!(server.get(ADMIN, OBJECT).success().body, b"hello world");
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
                iam::set(&server, ADMIN, &json!({"bindings":[]})).success();
                read.join().unwrap();
            });
            xml_error(&server.get("", OBJECT), 401, "AuthenticationRequired");
            server
                .get("", JSON_OBJECT)
                .anonymous_denied("storage.objects.get", "object");
        }
    }
}

#[test]
fn xml_open_reader_keeps_generation_metadata_and_bytes_during_overwrite_and_revocation() {
    let (_config, _data, server) = fixture();
    let old = server
        .upload(ADMIN, "iam-bucket", "object", &vec![b'a'; 8 * 1024 * 1024])
        .success()
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
        .success()
        .json();
    assert_ne!(replacement["generation"], old["generation"]);
    iam::set(&server, ADMIN, &json!({"bindings":[]})).success();
    // The connection stays open after the body, so read exactly the declared length.
    let length: u64 = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .parse()
        .unwrap();
    let mut body = Vec::new();
    reader.take(length).read_to_end(&mut body).unwrap();
    assert_eq!(body.len(), 8 * 1024 * 1024 - 1);
    assert!(body.iter().all(|byte| *byte == b'a'));
    xml_error(&server.get("", OBJECT), 401, "AuthenticationRequired");
    assert_eq!(server.get(ADMIN, OBJECT).success().body, b"replacement");
}

#[test]
fn xml_head_matches_get_headers_and_selected_metadata() {
    let (_config, _data, server) = fixture();
    grant(&server, "allUsers", "roles/storage.objectViewer");
    server.request("PATCH", JSON_OBJECT, ADMIN, br#"{"contentType":"text/plain","contentDisposition":"attachment; filename=hello.txt","contentLanguage":"en","metadata":{"color":"blue"}}"#).success();
    let get = server.get("", OBJECT);
    let head = server.request("HEAD", OBJECT, "", b"");
    head.expect(200);
    assert!(head.body.is_empty());
    let mut get_headers = get.headers.clone();
    let mut head_headers = head.headers;
    get_headers.remove("date");
    head_headers.remove("date");
    // The request helper asks the server to close the connection, which hyper echoes.
    assert_eq!(get_headers.remove("connection").as_deref(), Some("close"));
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
    // Cloud Storage's XML API sends one x-goog-hash line per algorithm.
    assert_eq!(
        get.headers["x-goog-hash"],
        "crc32c=yZRlqg==, md5=XrY7u+Ae7tCTyyK7j1rNww=="
    );
    assert!(get.headers["last-modified"].ends_with(" GMT"));
    assert_eq!(get.headers["last-modified"].len(), 29);
    let partial = server.get(
        "",
        &format!("{OBJECT}?generation={}", get.headers["x-goog-generation"]),
    );
    assert_eq!(partial.body, get.body);
    let failed = server.request("HEAD", "/iam-bucket/missing", "", b"");
    failed.expect(404);
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
        reply.expect(206);
        assert_eq!(reply.body, expected);
        assert_eq!(reply.headers["content-range"], content_range);
        assert_eq!(reply.headers["x-goog-generation"], generation);
        assert_eq!(reply.headers["x-goog-stored-content-length"], "11");
        // Unlike JSON, XML ranges keep the full-object hashes.
        assert_eq!(
            reply.headers["x-goog-hash"],
            "crc32c=yZRlqg==, md5=XrY7u+Ae7tCTyyK7j1rNww=="
        );
        let head = server.request("HEAD", OBJECT, &format!("Range: {range}\r\n"), b"");
        head.expect(206);
        assert_eq!(head.headers["content-range"], content_range);
        assert_eq!(head.headers["content-length"], expected.len().to_string());
    }
    let reply = server.get("Range: bytes=11-\r\n", OBJECT);
    xml_error(&reply, 416, "InvalidRange");
    // The XML 416 carries the object headers but no content-range (observed 2026-10-31).
    assert!(!reply.headers.contains_key("content-range"));
    let head = server.request("HEAD", OBJECT, "Range: bytes=11-\r\n", b"");
    head.expect(416);
    assert!(head.body.is_empty());
    assert!(!head.headers.contains_key("content-range"));
    assert_eq!(
        server.get("Range: bytes=1-0\r\n", OBJECT).success().body,
        b"hello world"
    );
    server
        .get(
            &format!(
                "x-goog-if-generation-match: {generation}\r\nx-goog-if-metageneration-match: 1\r\n"
            ),
            OBJECT,
        )
        .success();
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
        .success();
    xml_error(
        &server.get("", &format!("{OBJECT}?generation={generation}")),
        404,
        "NoSuchKey",
    );
    assert_eq!(server.get("", OBJECT).success().body, b"replacement");
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
        .anonymous_denied("storage.objects.get", "object");
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
            .success();
        let url = public_object_url("http://local", "iam-bucket", name).unwrap();
        assert_eq!(
            server
                .get("", url.strip_prefix("http://local").unwrap())
                .success()
                .body,
            name.as_bytes(),
            "{name}"
        );
    }
    assert_eq!(server.get("", "/iam-bucket/a+b").success().body, b"a+b");
    assert_eq!(
        server.get("", "/iam-bucket/a/../b").success().body,
        b"a/../b"
    );
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
            .success()
            .json()["name"],
        "object"
    );
    assert_eq!(
        server
            .get("", "/download/storage/v1/b/iam-bucket/o/object?alt=media")
            .success()
            .body,
        b"hello world"
    );
    server.get("", "/storage/v1/invalid").expect(404);
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
        reply.expect_with(404, format_args!("{path}"));
        assert_eq!(
            reply.headers["content-type"],
            "application/json; charset=UTF-8"
        );
        assert_eq!(
            reply.json(),
            json!({"error":{"code":404,"message":"Route not found","errors":[{"message":"Route not found","domain":"global","reason":"notFound"}]}})
        );
    }
}

#[test]
fn xml_bucket_paths_reserve_only_api_namespaces() {
    let (_config, _data, server) = fixture();
    for bucket in ["batch", "storage", "upload", "download"] {
        server.create_bucket(ADMIN, "project-a", bucket).success();
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
            .success();
        let url = public_object_url("http://local", bucket, name).unwrap();
        assert_eq!(
            server
                .get(ADMIN, url.strip_prefix("http://local").unwrap())
                .success()
                .body,
            b"bytes"
        );
    }
    let batch = server.get(ADMIN, "/batch/storage/v1");
    batch.auth_error(400, "badRequest");
    assert!(
        batch.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("POST /batch/storage/v1")
    );
    server
        .request(
            "POST",
            "/batch/storage/v1",
            &format!("{ADMIN}Content-Type: multipart/mixed; boundary=batch\r\n"),
            b"not a multipart envelope",
        )
        .auth_error(400, "badRequest");
    assert_eq!(server.get(ADMIN, "/batch/x").success().body, b"bytes");
    for path in [
        "/batch/storage/v1/x",
        "/storage/v1/x",
        "/upload/storage/v1/x",
        "/download/storage/v1/x",
        "/_internal/x",
    ] {
        let reply = server.get(ADMIN, path);
        reply.expect_with(404, format_args!("{path}"));
        assert_eq!(
            reply.headers["content-type"],
            "application/json; charset=UTF-8"
        );
        assert_eq!(
            reply.json(),
            json!({"error":{"code":404,"message":"Route not found","errors":[{"message":"Route not found","domain":"global","reason":"notFound"}]}})
        );
    }
}

#[test]
fn xml_unsupported_operations_fail_before_reading_a_body() {
    let (_config, _data, server) = fixture();
    grant(&server, "allUsers", "roles/storage.objectViewer");
    let before = server.get(ADMIN, JSON_OBJECT).success().json();
    for (method, path) in [
        ("GET", "/iam-bucket?acl"),
        ("GET", "/iam-bucket/?uploads=unexpected"),
        ("PUT", "/iam-bucket/object?acl"),
        ("POST", OBJECT),
        ("OPTIONS", OBJECT),
        ("GET", "/iam-bucket/object?acl"),
        ("GET", "/iam-bucket/object?uploads"),
        ("GET", "/iam-bucket/object?response-cache-control=no-cache"),
    ] {
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: local\r\n{ADMIN}Content-Length: 999999\r\n\r\n"
        );
        xml_error(&server.raw(request.as_bytes()), 400, "InvalidArgument");
    }
    for (auth, status, code) in [
        ("", 401, "AuthenticationRequired"),
        (NO_ROLES, 403, "AccessDenied"),
    ] {
        let request = format!(
            "GET /iam-bucket/?uploads HTTP/1.1\r\nHost: local\r\n{auth}Content-Length: 999999\r\n\r\n"
        );
        xml_error(&server.raw(request.as_bytes()), status, code);
    }
    let uploads = server.get(ADMIN, "/iam-bucket/?uploads");
    uploads.expect(200);
    assert!(String::from_utf8_lossy(&uploads.body).contains("<ListMultipartUploadsResult"));
    assert_eq!(server.get(ADMIN, JSON_OBJECT).success().json(), before);
    assert_eq!(server.get("", OBJECT).success().body, b"hello world");
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
    assert_eq!(server.get(ADMIN, OBJECT).success().body, b"hello world");
    server.request("DELETE", OBJECT, ADMIN, b"").expect(204);
    xml_error(&server.get(ADMIN, OBJECT), 404, "NoSuchKey");
}

#[test]
fn xml_go_sdk_read_headers_accept_telemetry_without_replaying_reads() {
    let (_config, _data, server) = fixture();
    grant(&server, "allUsers", "roles/storage.objectViewer");
    server
        .upload(ADMIN, "iam-bucket", "dir%2Ffixture.bin", b"hello world")
        .success();
    let headers = concat!(
        "User-Agent: Go-http-client/1.1\r\n",
        "Accept-Encoding: gzip\r\n",
        "X-Goog-Api-Client: gccl-invocation-id/8bf3a9fb-418c-4a43-867c-b463a65f1c85 gccl-attempt-count/1 gl-go/1.27.3 gccl/1.70.0\r\n",
        "X-Goog-Gcs-Idempotency-Token: 8bf3a9fb-418c-4a43-867c-b463a65f1c85\r\n",
    );
    let path = "/iam-bucket/dir%2Ffixture.bin";
    let reply = server.get(headers, path);
    assert_eq!(reply.success().body, b"hello world");
    let head = server.request("HEAD", path, headers, b"");
    head.expect(200);
    assert_eq!(head.headers["content-length"], "11");
    let conditions = format!(
        "{headers}Range: bytes=6-10\r\nx-goog-if-generation-match: {}\r\nx-goog-if-metageneration-match: 1\r\n",
        reply.headers["x-goog-generation"]
    );
    let range = server.get(&conditions, path);
    range.expect(206);
    assert_eq!(range.body, b"world");
    server
        .upload(ADMIN, "iam-bucket", "dir%2Ffixture.bin", b"replacement")
        .success();
    assert_eq!(server.get(headers, path).success().body, b"replacement");
    iam::set(&server, ADMIN, &json!({"bindings":[]})).success();
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
        head.expect(400);
        assert_eq!(
            head.headers["content-type"],
            "application/xml; charset=UTF-8"
        );
        assert!(head.body.is_empty());
    }
    assert_eq!(server.get("", OBJECT).success().body, b"hello world");
    assert_eq!(
        server
            .get("x-goog-gcs-idempotency-token: valid\r\n", OBJECT)
            .success()
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
    // Incomplete customer-supplied keys fail with their documented XML codes.
    for (header, code) in [
        (
            "x-goog-encryption-key: key",
            "CustomerEncryptionAlgorithmIsInvalid",
        ),
        (
            "x-goog-encryption-key-sha256: hash",
            "CustomerEncryptionAlgorithmIsInvalid",
        ),
        (
            "x-goog-encryption-algorithm: AES256",
            "CustomerEncryptionKeyFormatIsInvalid",
        ),
    ] {
        xml_error(&server.get(&format!("{header}\r\n"), OBJECT), 400, code);
    }
    for header in [
        "x-goog-encryption-kms-key-name: key",
        "x-goog-user-project: p",
        "If-Range: *",
        "x-goog-if-generation-not-match: 1",
    ] {
        xml_error(
            &server.get(&format!("{header}\r\n"), OBJECT),
            400,
            "InvalidArgument",
        );
    }
    for header in [
        "If-Match: *",
        "If-None-Match: *",
        "If-Modified-Since: Fri, 19 Feb 2010 22:04:23 GMT",
        "If-Unmodified-Since: Fri, 19 Feb 2010 22:04:23 GMT",
    ] {
        for (method, path) in [
            ("DELETE", OBJECT),
            ("PUT", OBJECT),
            ("GET", "/iam-bucket?prefix=object"),
        ] {
            xml_error(
                &server.request(method, path, &format!("{ADMIN}{header}\r\n"), b""),
                400,
                "InvalidArgument",
            );
        }
    }
    server.get("Unknown-Header: ignored\r\n", OBJECT).success();
    server
        .get("x-goog-api-client: gl-go/1.27.3 gccl/1.70.0\r\n", OBJECT)
        .success();
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
            .success();
        xml_error(&server.get("", OBJECT), 400, "InvalidArgument");
        assert_eq!(
            server.get("", JSON_OBJECT).success().json()["metadata"],
            metadata
        );
        let head = server.request("HEAD", OBJECT, "", b"");
        head.expect(400);
        assert!(head.body.is_empty());
    }
}

#[test]
fn xml_cache_control_respects_metadata_and_distinguishes_public_from_authenticated() {
    let (_config, _data, server) = fixture();
    assert_eq!(
        server.get(ADMIN, OBJECT).success().headers["cache-control"],
        "private, max-age=0"
    );
    assert_eq!(
        server
            .get(ADMIN, &format!("{JSON_OBJECT}?alt=media"))
            .success()
            .headers["cache-control"],
        "no-cache, no-store, max-age=0, must-revalidate"
    );
    grant(
        &server,
        "allAuthenticatedUsers",
        "roles/storage.objectViewer",
    );
    assert_eq!(
        server.get(NO_ROLES, OBJECT).success().headers["cache-control"],
        "private, max-age=0"
    );
    grant(&server, "allUsers", "roles/storage.objectViewer");
    for (path, expected) in [
        (OBJECT.to_owned(), "public, max-age=3600"),
        (
            format!("{JSON_OBJECT}?alt=media"),
            "public, max-age=3600, must-revalidate",
        ),
    ] {
        assert_eq!(
            server.get("", &path).success().headers["cache-control"],
            expected
        );
    }
    server
        .request(
            "PATCH",
            JSON_OBJECT,
            ADMIN,
            br#"{"cacheControl":"public, max-age=60"}"#,
        )
        .success();
    assert_eq!(
        server
            .get("Cache-Control: no-store\r\n", OBJECT)
            .success()
            .headers["cache-control"],
        "public, max-age=60"
    );
    iam::set(&server, ADMIN, &json!({"bindings":[]})).success();
    assert_eq!(
        server.get(ADMIN, OBJECT).success().headers["cache-control"],
        "public, max-age=60"
    );
    assert_eq!(
        server
            .get(ADMIN, &format!("{JSON_OBJECT}?alt=media"))
            .success()
            .headers["cache-control"],
        "no-cache, no-store, max-age=0, must-revalidate"
    );
}

#[test]
fn xml_permissive_reads_ignore_credentials_without_granting_public_policy() {
    let data = TestDir::new();
    let (server, _) = Server::start(&data, &[]);
    server
        .create_bucket("", "project-a", "iam-bucket")
        .success();
    server
        .upload("", "iam-bucket", "object", b"hello world")
        .success();
    assert_eq!(
        server
            .get("Authorization: nonsense\r\n", OBJECT)
            .success()
            .body,
        b"hello world"
    );
    assert_eq!(
        server.get("", OBJECT).success().headers["cache-control"],
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
            .success();
        for method in ["PATCH", "PUT"] {
            server
                .request(
                    method,
                    "/storage/v1/b/iam-bucket",
                    ADMIN,
                    json!({"iamConfiguration":setting}).to_string().as_bytes(),
                )
                .success();
        }
    }
    assert_eq!(server.get(ADMIN, OBJECT).success().body, b"hello world");
    xml_error(&server.get("", OBJECT), 401, "AuthenticationRequired");
}

#[test]
fn uniform_access_metadata_and_predefined_acl_rejections() {
    let (_config, _data, server) = fixture();
    let path = "/storage/v1/b/iam-bucket";
    // Conflicting aliases: the one that changes the stored setting wins. On a disabled bucket
    // that enables access, as Cloud Storage did (recorded 2026-11); on an enabled bucket the
    // same body disables it, as Ruby's setter sends when it resends the alias from GET.
    let conflict = br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false},"bucketPolicyOnly":{"enabled":true}}}"#;
    for expected in [true, false] {
        let conflicting = server
            .request("PATCH", path, ADMIN, conflict)
            .success()
            .json();
        let config = &conflicting["iamConfiguration"];
        assert_eq!(config["uniformBucketLevelAccess"]["enabled"], expected);
        assert_eq!(
            config["bucketPolicyOnly"],
            config["uniformBucketLevelAccess"]
        );
    }
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
            .success()
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
        .success()
        .json();
    let config = &enabled["iamConfiguration"];
    assert_eq!(config["uniformBucketLevelAccess"]["enabled"], true);
    assert!(config["uniformBucketLevelAccess"]["lockedTime"].is_string());
    assert_eq!(
        config["bucketPolicyOnly"],
        config["uniformBucketLevelAccess"]
    );
    assert_eq!(server.get(ADMIN, path).success().json(), enabled);
    let listed = server
        .get(ADMIN, "/storage/v1/b?project=project-a")
        .success()
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
            .auth_error(403, "forbidden");
        server
            .request(
                method,
                &format!("{path}?ifMetagenerationMatch=0"),
                ADMIN,
                br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false}}}"#,
            )
            .auth_error(412, "conditionNotMet");
        server.request(method, path, ADMIN, br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":[]},"bucketPolicyOnly":{"enabled":false}}}"#).auth_error(400, "invalid");
        for key in ["predefinedAcl", "predefinedDefaultObjectAcl"] {
            let reply = server.request(method, &format!("{path}?{key}=publicRead"), ADMIN, b"{}");
            reply.auth_error(400, "invalid");
            assert!(
                reply.json()["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("uniform bucket-level access")
            );
        }
    }
    for key in ["predefinedAcl", "predefinedDefaultObjectAcl"] {
        server.request("POST", &format!("/storage/v1/b?project=project-a&{key}=private"), ADMIN, br#"{"name":"rejected-create","iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}"#).auth_error(400, "invalid");
        server
            .get(ADMIN, "/storage/v1/b/rejected-create")
            .auth_error(404, "notFound");
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
        reply.auth_error(400, "invalidArgument");
        assert!(
            reply.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("remove ")
        );
    }
    server
        .get(ADMIN, "/storage/v1/b/iam-bucket/o/rejected")
        .auth_error(404, "notFound");
    assert_eq!(server.get(ADMIN, path).success().json(), enabled);
    server
        .request(
            "PATCH",
            path,
            ADMIN,
            br#"{"iamConfiguration":{"bucketPolicyOnly":{"enabled":false}}}"#,
        )
        .success();
    // Without uniform access uploads apply predefinedAcl; copy, rewrite and compose still
    // reject destinationPredefinedAcl explicitly.
    for route in &routes[3..6] {
        let reply = server.request("POST", route, ADMIN, b"{}");
        assert_eq!(
            reply.json()["error"]["errors"][0]["reason"],
            gcs_mimic::json::JsonError::unsupported_object("predefinedAcl").reason
        );
    }
    let terraform = server.request("POST", "/storage/v1/b?project=project-a", ADMIN, br#"{"name":"terraform-ubla","location":"US","iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true},"publicAccessPrevention":"enforced"}}"#).success().json();
    assert_eq!(
        terraform["iamConfiguration"]["publicAccessPrevention"],
        "enforced"
    );
    assert_eq!(
        terraform["iamConfiguration"]["uniformBucketLevelAccess"]["enabled"],
        true
    );
}
