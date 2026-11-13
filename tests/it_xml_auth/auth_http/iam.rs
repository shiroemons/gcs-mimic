use super::*;
use std::sync::{Arc, Barrier};

pub(super) const IAM: &str = "/storage/v1/b/iam-bucket/iam";

pub(super) fn fixture() -> (TestDir, TestDir, Server) {
    fixture_with(&[])
}

pub(super) fn fixture_with(options: &[&str]) -> (TestDir, TestDir, Server) {
    let config = TestDir::new();
    let identities = identity_file(&config);
    let data = TestDir::new();
    let server = Server::strict_with(&data, &identities, options);
    server
        .create_bucket(ADMIN, "project-a", "iam-bucket")
        .success();
    (config, data, server)
}

pub(super) fn set(server: &Server, auth: &str, policy: &Value) -> Reply {
    server.request("PUT", IAM, auth, policy.to_string().as_bytes())
}

pub(super) fn binding(role: &str, member: &str) -> Value {
    json!({"role": role, "members": [member]})
}

fn grants(roles: &[&str]) -> Value {
    json!({"bindings": roles.iter().map(|role| binding(role, "user:no-roles@example.invalid")).collect::<Vec<_>>()})
}

#[test]
fn test_permissions_matches_operations_and_policy_changes() {
    let (_config, _data, server) = fixture();
    let path = format!(
        "{IAM}/testPermissions?permissions=storage.objects.get&permissions=storage.objects.create&permissions=storage.buckets.getIamPolicy"
    );
    assert_eq!(
        server.get(NO_ROLES, &path).success().json(),
        json!({
            "kind":"storage#testIamPermissionsResponse", "permissions":[]
        })
    );
    assert_eq!(
        server.get("", &path).success().json()["permissions"],
        json!([])
    );
    assert_eq!(
        server.get(VIEWER, &path).success().json()["permissions"],
        json!(["storage.objects.get"])
    );
    server
        .upload(ADMIN, "iam-bucket", "test-permissions", b"hello")
        .success();
    let object = "/storage/v1/b/iam-bucket/o/test-permissions";
    server.get(NO_ROLES, object).forbidden();
    set(&server, ADMIN, &grants(&["roles/storage.objectViewer"])).success();
    assert_eq!(
        server.get(NO_ROLES, &path).success().json()["permissions"],
        json!(["storage.objects.get"])
    );
    server.get(NO_ROLES, object).success();
    server
        .upload(NO_ROLES, "iam-bucket", "denied", b"no")
        .forbidden();
    server.get(NO_ROLES, IAM).forbidden();
    set(&server, ADMIN, &json!({"bindings":[]})).success();
    assert_eq!(
        server.get(NO_ROLES, &path).success().json()["permissions"],
        json!([])
    );
    server.get(NO_ROLES, object).forbidden();
    server
        .get("Authorization: Bearer invalid\r\n", &path)
        .auth_error(401, "authError");
}

#[test]
fn test_permissions_validates_route_queries_and_response_fields() {
    let data = TestDir::new();
    let mut server = Server::start(&data, &[]).0;
    server
        .create_bucket("", "project-a", "iam-bucket")
        .success();
    let path = format!("{IAM}/testPermissions");
    server.get("", &path).auth_error(400, "required");
    for name in [
        "",
        "storage.*",
        "storage.objects.unknown",
        "storage.buckets.create",
        "storage.buckets.list",
    ] {
        server
            .get("", &format!("{path}?permissions={name}"))
            .auth_error(400, "invalid");
    }
    server
        .get(
            "",
            &format!("{path}?permissions=storage.managedFolders.getIamPolicy"),
        )
        .auth_error(400, "badRequest");
    let query = "permissions=storage.objects.get&permissions=storage.objects.create&permissions=storage.objects.get";
    assert_eq!(
        server
            .get("", &format!("{path}?{query}&userProject="))
            .success()
            .json(),
        json!({
            "kind":"storage#testIamPermissionsResponse", "permissions":["storage.objects.get", "storage.objects.create"]
        })
    );
    assert_eq!(
        server
            .get("", &format!("{path}?{query}&fields=permissions"))
            .success()
            .json(),
        json!({
            "permissions":["storage.objects.get", "storage.objects.create"]
        })
    );
    server
        .get("", &format!("{path}?{query}&userProject=project-a"))
        .auth_error(400, "badRequest");
    server
        .get("", &format!("{path}?{query}&unknown=true"))
        .auth_error(400, "invalidParameter");
    server
        .get("", &format!("{path}?{query}&fields=missing"))
        .auth_error(400, "invalidParameter");
    server
        .get(
            "",
            "/storage/v1/b/missing-bucket/iam/testPermissions?permissions=storage.objects.get",
        )
        .auth_error(404, "notFound");
    let response = server.request("POST", &format!("{path}?{query}"), "", b"");
    response.auth_error(405, "methodNotAllowed");
    assert_eq!(response.headers["allow"], "GET");
    server
        .request("GET", &format!("{path}?{query}"), "", b"body")
        .auth_error(400, "invalidArgument");
    server.stop();
}

#[test]
fn iam_default_policy_and_versions() {
    let (_config, _data, server) = fixture();
    let policy = server.get(ADMIN, IAM).success().json();
    assert_eq!(policy["kind"], "storage#policy");
    assert_eq!(policy["resourceId"], "projects/_/buckets/iam-bucket");
    assert_eq!(policy["version"], 1);
    assert_eq!(policy["etag"].as_str().unwrap().len(), 12);
    assert_eq!(
        policy["bindings"],
        json!([
            {"role":"roles/storage.legacyBucketOwner", "members":["projectEditor:project-a", "projectOwner:project-a"]},
            {"role":"roles/storage.legacyBucketReader", "members":["projectViewer:project-a"]}
        ])
    );
    for version in [1, 3] {
        assert_eq!(
            server
                .get(
                    ADMIN,
                    &format!("{IAM}?optionsRequestedPolicyVersion={version}")
                )
                .success()
                .json(),
            policy
        );
    }
    server.get(NO_ROLES, "/storage/v1/b/iam-bucket").forbidden();
}

#[test]
fn iam_policy_round_trip_and_unconditional_versions() {
    let (_config, _data, server) = fixture();
    let mut previous = server.get(ADMIN, IAM).success().json()["etag"].clone();
    for version in [0, 1, 3] {
        let policy = json!({"version":version, "kind":false, "resourceId":null, "unknown":true,
            "bindings":[{"role":"roles/storage.objectViewer", "members":["user:viewer@example.invalid", "serviceAccount:no-roles@example.invalid", "projectOwner:another-project", "projectEditor:project-a", "projectViewer:project-a"], "unknown":true}]});
        let result = set(&server, ADMIN, &policy).success().json();
        assert_eq!(result["version"], 1);
        // Cloud Storage returns the members sorted (recorded 2026-11).
        assert_eq!(
            result["bindings"][0]["members"],
            json!([
                "projectEditor:project-a",
                "projectOwner:another-project",
                "projectViewer:project-a",
                "serviceAccount:no-roles@example.invalid",
                "user:viewer@example.invalid"
            ])
        );
        assert_ne!(result["etag"], previous);
        previous = result["etag"].clone();
        assert_eq!(server.get(ADMIN, IAM).success().json(), result);
    }
    server
        .upload(ADMIN, "iam-bucket", "object", b"hello")
        .success();
    assert_eq!(
        server
            .get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object?alt=media")
            .success()
            .body,
        b"hello"
    );
    let bucket_before = server
        .get(ADMIN, "/storage/v1/b/iam-bucket")
        .success()
        .json();
    assert_eq!(
        set(&server, ADMIN, &json!({"bindings":[]}))
            .success()
            .json()["bindings"],
        json!([])
    );
    // A policy write advances the bucket metageneration, replay or not (recorded 2026-10-31);
    // nothing else about the bucket changes.
    let bucket_after = server
        .get(ADMIN, "/storage/v1/b/iam-bucket")
        .success()
        .json();
    let metageneration = |bucket: &Value| {
        bucket["metageneration"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap()
    };
    assert_eq!(
        metageneration(&bucket_after),
        metageneration(&bucket_before) + 1
    );
    let stable = |bucket: &Value| {
        let mut bucket = bucket.clone();
        for field in ["metageneration", "etag", "updated"] {
            bucket.as_object_mut().unwrap().remove(field);
        }
        bucket
    };
    assert_eq!(stable(&bucket_after), stable(&bucket_before));
    server
        .get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object")
        .forbidden();
}

#[test]
fn iam_merges_roles_and_deduplicates_members() {
    let (_config, _data, server) = fixture();
    let policy = json!({"bindings":[
        {"role":"roles/storage.bucketViewer", "members":["user:local", "user:local"]},
        {"role":"roles/storage.bucketViewer", "members":["user:another"]}
    ]});
    // Cloud Storage merged a repeated role, de-duplicated and sorted its members, and kept the
    // first binding's position (case bkt-iam-policy-shape, recorded 2026-11).
    let result = set(&server, ADMIN, &policy).success().json();
    assert_eq!(
        result["bindings"],
        json!([{"role":"roles/storage.bucketViewer", "members":["user:another", "user:local"]}])
    );
    assert_eq!(server.get(ADMIN, IAM).success().json(), result);
}

#[test]
fn iam_policy_survives_restart() {
    let (config, data, mut server) = fixture();
    let policy = set(&server, ADMIN, &grants(&["roles/storage.objectViewer"]))
        .success()
        .json();
    server
        .upload(ADMIN, "iam-bucket", "object", b"persistent")
        .success();
    server.stop();
    let server = Server::strict(&data, &config.0.join("identities.json"));
    assert_eq!(server.get(ADMIN, IAM).success().json(), policy);
    assert_eq!(
        server
            .get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object?alt=media")
            .success()
            .body,
        b"persistent"
    );
}

#[test]
fn iam_migrates_v5_catalog() {
    let (config, data, mut server) = fixture();
    server
        .upload(ADMIN, "iam-bucket", "object", b"before migration")
        .success();
    server.stop();
    let path = data.0.join("catalog.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    let current_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    common::downgrade_object_schema(&connection);
    connection
        .execute_batch(
            "ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold; DROP TABLE resumable_sessions; DROP TABLE bucket_incarnations; DROP TABLE bucket_policies; DROP TABLE policy_revision; PRAGMA user_version = 5;",
        )
        .unwrap();
    drop(connection);
    let mut server = Server::strict(&data, &config.0.join("identities.json"));
    assert_eq!(
        server.get(ADMIN, IAM).success().json()["bindings"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/iam-bucket/o/object?alt=media")
            .success()
            .body,
        b"before migration"
    );
    let policy = set(&server, ADMIN, &grants(&["roles/storage.objectViewer"]))
        .success()
        .json();
    server.stop();
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        current_version
    );
    drop(connection);
    let server = Server::strict(&data, &config.0.join("identities.json"));
    assert_eq!(server.get(ADMIN, IAM).success().json(), policy);
}

#[test]
fn iam_permissive_policy_and_cas() {
    let data = TestDir::new();
    let (mut server, _) = Server::start(&data, &[]);
    server
        .create_bucket("", "project-a", "iam-bucket")
        .success();
    let initial = server.get("", IAM).success().json();
    let policy = set(
        &server,
        "Authorization: nonsense\r\n",
        &grants(&["roles/storage.objectViewer"]),
    )
    .success()
    .json();
    set(&server, "", &initial).auth_error(412, "conditionNotMet");
    set(
        &server,
        "",
        &json!({"bindings":[binding("roles/storage.admin", "group:local@example.invalid")]}),
    )
    .auth_error(400, "badRequest");
    assert_eq!(server.get("", IAM).success().json(), policy);
    server.stop();
    let (server, _) = Server::start(&data, &[]);
    assert_eq!(server.get("", IAM).success().json(), policy);
}

#[test]
fn iam_requires_effective_get_and_set_permissions() {
    let (_config, _data, server) = fixture();
    server
        .get("", IAM)
        .anonymous_denied("storage.buckets.getIamPolicy", "bucket");
    set(&server, "", &json!({})).anonymous_denied("storage.buckets.setIamPolicy", "bucket");
    server
        .get("Authorization: Bearer unknown\r\n", IAM)
        .invalid_credentials();
    for auth in [VIEWER, CREATOR, EDITOR, NO_ROLES] {
        server.get(auth, IAM).forbidden();
        set(&server, auth, &json!({})).forbidden();
    }
    set(&server, ADMIN, &grants(&["roles/storage.objectAdmin"])).success();
    server.get(NO_ROLES, IAM).forbidden();
    set(&server, NO_ROLES, &json!({})).forbidden();
    set(&server, ADMIN, &grants(&["roles/storage.admin"])).success();
    server.get(NO_ROLES, IAM).success();
    set(&server, NO_ROLES, &grants(&["roles/storage.admin"])).success();
    // Project grants remain effective after every bucket binding is removed.
    set(&server, NO_ROLES, &json!({"bindings":[]})).success();
    server.get(NO_ROLES, IAM).forbidden();
    server.get(ADMIN, IAM).success();
}

#[test]
fn iam_bucket_grants_authorize_only_the_target_bucket() {
    let (_config, _data, server) = fixture();
    server
        .create_bucket(ADMIN, "project-a", "other-bucket")
        .success();
    server
        .create_bucket(ADMIN, "project-b", "foreign-bucket")
        .success();
    server
        .upload(ADMIN, "iam-bucket", "object", b"hello")
        .success();
    set(
        &server,
        ADMIN,
        &grants(&["roles/storage.objectViewer", "roles/storage.objectCreator"]),
    )
    .success();
    server.get(NO_ROLES, "/storage/v1/b/iam-bucket/o").success();
    server
        .get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object")
        .success();
    server
        .upload(NO_ROLES, "iam-bucket", "fresh", b"created")
        .success();
    server
        .upload(NO_ROLES, "iam-bucket", "fresh", b"overwrite")
        .forbidden();
    for bucket in ["other-bucket", "foreign-bucket", "missing-bucket"] {
        server
            .get(NO_ROLES, &format!("/storage/v1/b/{bucket}/o"))
            .forbidden();
        server
            .upload(NO_ROLES, bucket, "object", b"denied")
            .forbidden();
    }
    set(&server, ADMIN, &grants(&["roles/storage.admin"])).success();
    server
        .get(NO_ROLES, "/storage/v1/b?project=project-a")
        .forbidden();
    server
        .create_bucket(NO_ROLES, "project-a", "never-created")
        .forbidden();
    set(&server, ADMIN, &json!({"bindings":[]})).success();
    for path in [
        "/storage/v1/b/iam-bucket/o",
        "/storage/v1/b/iam-bucket/o/object",
    ] {
        server.get(NO_ROLES, path).forbidden();
    }
    server
        .upload(NO_ROLES, "iam-bucket", "revoked", b"denied")
        .forbidden();
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/iam-bucket/o/fresh?alt=media")
            .success()
            .body,
        b"created"
    );
}

#[test]
fn iam_bucket_grants_cover_existing_routes() {
    let (_config, _data, server) = fixture();
    set(&server, ADMIN, &grants(&["roles/storage.admin"])).success();
    server.get(NO_ROLES, "/storage/v1/b/iam-bucket").success();
    server
        .upload(NO_ROLES, "iam-bucket", "object", b"old")
        .success();
    server
        .upload(NO_ROLES, "iam-bucket", "object", b"new")
        .success();
    server
        .multipart(NO_ROLES, "iam-bucket", "multipart", b"multipart")
        .success();
    server.get(NO_ROLES, "/storage/v1/b/iam-bucket/o").success();
    server
        .get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object")
        .success();
    for path in [
        "/storage/v1/b/iam-bucket/o/object?alt=media",
        "/download/storage/v1/b/iam-bucket/o/object?alt=media",
    ] {
        assert_eq!(server.get(NO_ROLES, path).success().body, b"new");
    }
    for method in ["PATCH", "PUT"] {
        server
            .request(
                method,
                "/storage/v1/b/iam-bucket/o/object",
                NO_ROLES,
                br#"{"metadata":{"key":"value"}}"#,
            )
            .success();
    }
    for name in ["object", "multipart"] {
        server
            .request(
                "DELETE",
                &format!("/storage/v1/b/iam-bucket/o/{name}"),
                NO_ROLES,
                b"",
            )
            .success();
    }
    server
        .request("DELETE", "/storage/v1/b/iam-bucket", NO_ROLES, b"")
        .success();
}

#[test]
fn auth_http_revoked_bucket_grants_deny_every_basic_method_and_preserve_objects() {
    let (_config, data, server) = fixture();
    server
        .upload(ADMIN, "iam-bucket", "object", b"old")
        .success();
    let original = server
        .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
        .success()
        .json();
    set(&server, ADMIN, &grants(&["roles/storage.admin"])).success();
    server.get(NO_ROLES, "/storage/v1/b/iam-bucket").success();
    // Bucket roles cannot supply project-level create/list permissions.
    server
        .create_bucket(NO_ROLES, "project-a", "unauthorized")
        .forbidden();
    server
        .get(NO_ROLES, "/storage/v1/b?project=project-a")
        .forbidden();
    set(&server, ADMIN, &json!({"bindings":[]})).success();
    let bucket = server
        .get(ADMIN, "/storage/v1/b/iam-bucket")
        .success()
        .json();
    server
        .get(ADMIN, "/storage/v1/b/unauthorized")
        .auth_error(404, "notFound");
    for (auth, status, reason) in [("", 401, "required"), (NO_ROLES, 403, "forbidden")] {
        for (method, path, body) in [
            ("GET", "/storage/v1/b/iam-bucket", b"".as_slice()),
            ("PATCH", "/storage/v1/b/iam-bucket", b"{}"),
            ("PUT", "/storage/v1/b/iam-bucket", b"{}"),
            ("DELETE", "/storage/v1/b/iam-bucket", b""),
            ("GET", "/storage/v1/b/iam-bucket/o", b""),
            ("GET", "/storage/v1/b/iam-bucket/o/object", b""),
            ("GET", "/storage/v1/b/iam-bucket/o/object?alt=media", b""),
            (
                "PATCH",
                "/storage/v1/b/iam-bucket/o/object",
                br#"{"metadata":{"key":"new"}}"#,
            ),
            ("PUT", "/storage/v1/b/iam-bucket/o/object", b"{}"),
            ("DELETE", "/storage/v1/b/iam-bucket/o/object", b""),
            (
                "POST",
                "/upload/storage/v1/b/iam-bucket/o?uploadType=resumable&name=object",
                b"{}",
            ),
        ] {
            server
                .request(method, path, auth, body)
                .auth_error(status, reason);
        }
        server
            .request(
                "GET",
                "/storage/v1/b/iam-bucket/o/object?alt=media",
                &format!("{auth}Range: bytes=0-1\r\n"),
                b"",
            )
            .auth_error(status, reason);
        server
            .upload(auth, "iam-bucket", "object", b"new")
            .auth_error(status, reason);
        server
            .multipart(auth, "iam-bucket", "object", b"new")
            .auth_error(status, reason);
        assert_eq!(
            server
                .get(ADMIN, "/storage/v1/b/iam-bucket")
                .success()
                .json(),
            bucket
        );
        server
            .get(ADMIN, "/storage/v1/b/unauthorized")
            .auth_error(404, "notFound");
        let sessions: i64 = common::catalog::count(
            &rusqlite::Connection::open_with_flags(
                data.0.join("catalog.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap(),
            "resumable_sessions",
        );
        assert_eq!(sessions, 0);
        assert_eq!(
            server
                .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
                .success()
                .json(),
            original
        );
        assert_eq!(
            server
                .get(ADMIN, "/storage/v1/b/iam-bucket/o/object?alt=media")
                .success()
                .body,
            b"old"
        );
        assert_eq!(common::process::staging_count(&data), 0);
        assert_eq!(common::process::blob_count(&data), 1);
    }
}

#[test]
fn auth_http_media_and_multipart_recheck_revoked_overwrite_grants_after_streaming() {
    let (_config, data, server) = fixture();
    server
        .upload(ADMIN, "iam-bucket", "object", b"old")
        .success();
    let original = server
        .request(
            "PATCH",
            "/storage/v1/b/iam-bucket/o/object",
            ADMIN,
            br#"{"contentType":"text/old","metadata":{"key":"old"}}"#,
        )
        .success()
        .json();
    for upload in ["media", "multipart"] {
        set(&server, ADMIN, &grants(&["roles/storage.admin"])).success();
        let body = if upload == "media" {
            b"new".to_vec()
        } else {
            b"--boundary\r\nContent-Type: application/json\r\n\r\n{\"metadata\":{\"key\":\"new\"}}\r\n--boundary\r\n\r\nnew\r\n--boundary--\r\n".to_vec()
        };
        let mut stream = server.stream();
        write!(stream, "POST /upload/storage/v1/b/iam-bucket/o?uploadType={upload}&name=object HTTP/1.1\r\nHost: local\r\n{NO_ROLES}Content-Type: multipart/related; boundary=boundary\r\nContent-Length: {}\r\n\r\n", body.len()).unwrap();
        stream.write_all(&body[..body.len() - 1]).unwrap();
        common::eventually("upload must pass advisory authorization", || {
            common::process::staging_count(&data) == 1
        });
        set(&server, ADMIN, &json!({"bindings":[]})).success();
        stream.write_all(&body[body.len() - 1..]).unwrap();
        Reply::read_joined(stream).forbidden();
        assert_eq!(
            server
                .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
                .success()
                .json(),
            original
        );
        assert_eq!(
            server
                .get(ADMIN, "/storage/v1/b/iam-bucket/o/object?alt=media")
                .success()
                .body,
            b"old"
        );
        assert_eq!(common::process::staging_count(&data), 0);
        assert_eq!(common::process::blob_count(&data), 1);
    }
}

#[test]
fn iam_stale_and_concurrent_etags() {
    let (_config, _data, server) = fixture();
    let initial = server.get(ADMIN, IAM).success().json();
    let updated = set(&server, ADMIN, &initial).success().json();
    set(&server, ADMIN, &initial).auth_error(412, "conditionNotMet");
    assert_eq!(server.get(ADMIN, IAM).success().json(), updated);
    let updated = set(&server, ADMIN, &json!({"etag":"", "bindings":[]}))
        .success()
        .json();
    assert_eq!(updated["bindings"], json!([]));
    let mut unpadded = updated.clone();
    unpadded["etag"] = json!(updated["etag"].as_str().unwrap().trim_end_matches('='));
    let updated = set(&server, ADMIN, &unpadded).success().json();
    set(&server, ADMIN, &json!({"etag":"c3RhbGU=", "bindings":[]}))
        .auth_error(412, "conditionNotMet");
    let barrier = Arc::new(Barrier::new(8));
    let statuses = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let barrier = barrier.clone();
                let policy = &updated;
                let server = &server;
                scope.spawn(move || {
                    barrier.wait();
                    set(server, ADMIN, policy).status
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(statuses.iter().filter(|&&status| status == 200).count(), 1);
    assert_eq!(statuses.iter().filter(|&&status| status == 412).count(), 7);
    assert_ne!(
        server.get(ADMIN, IAM).success().json()["etag"],
        updated["etag"]
    );
}

#[test]
fn iam_revocation_races_object_reads() {
    let (_config, _data, server) = fixture();
    server
        .upload(ADMIN, "iam-bucket", "object", b"selected")
        .success();
    for _ in 0..12 {
        set(&server, ADMIN, &grants(&["roles/storage.objectViewer"])).success();
        let barrier = Barrier::new(2);
        thread::scope(|scope| {
            let read = scope.spawn(|| {
                barrier.wait();
                let result = server.get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object?alt=media");
                assert!(matches!(result.status, 200 | 403));
                if result.status == 200 {
                    assert_eq!(result.body, b"selected");
                }
            });
            barrier.wait();
            set(&server, ADMIN, &json!({"bindings":[]})).success();
            read.join().unwrap();
        });
        server
            .get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object?alt=media")
            .forbidden();
    }
}

#[test]
fn iam_recreated_bucket_has_fresh_policy() {
    let (_config, _data, server) = fixture();
    let original = server.get(ADMIN, IAM).success().json();
    let edited = set(&server, ADMIN, &grants(&["roles/storage.admin"]))
        .success()
        .json();
    server
        .request("DELETE", "/storage/v1/b/iam-bucket", ADMIN, b"")
        .success();
    server
        .create_bucket(ADMIN, "project-a", "iam-bucket")
        .success();
    let recreated = server.get(ADMIN, IAM).success().json();
    assert_eq!(recreated["bindings"], original["bindings"]);
    assert_ne!(recreated["etag"], original["etag"]);
    assert_ne!(recreated["etag"], edited["etag"]);
    set(&server, ADMIN, &edited).auth_error(412, "conditionNotMet");
    server.get(NO_ROLES, IAM).forbidden();
}

#[test]
fn iam_rejects_invalid_policy_shapes() {
    let (_config, _data, server) = fixture();
    let before = server.get(ADMIN, IAM).success().json();
    for policy in [
        json!({}),
        json!(null),
        json!([]),
        json!(true),
        json!({"version":2}),
        json!({"version":-1}),
        json!({"version":"1"}),
        json!({"version":null}),
        json!({"version":1.5}),
        json!({"etag":null}),
        json!({"etag":1}),
        json!({"etag":"not base64!", "bindings":[]}),
        json!({"etag":"A", "bindings":[]}),
        json!({"etag":"AA=", "bindings":[]}),
        json!({"etag":"AA===", "bindings":[]}),
        json!({"etag":"AA=A", "bindings":[]}),
        json!({"bindings":null}),
        json!({"bindings":{}}),
        json!({"bindings":[null]}),
        json!({"bindings":[{}]}),
        json!({"bindings":[{"role":1}]}),
        json!({"bindings":[{"role":"roles/storage.objectViewer"}]}),
        json!({"bindings":[{"role":"roles/storage.objectViewer", "members":null}]}),
        json!({"bindings":[{"role":"roles/storage.objectViewer", "members":[1]}]}),
    ] {
        set(&server, ADMIN, &policy).auth_error(400, "invalid");
    }
    // Nonzero unused bits still decode, so the tag simply does not match (recorded 2026-11).
    for etag in ["AB==", "CAF="] {
        set(&server, ADMIN, &json!({"etag":etag, "bindings":[]}))
            .auth_error(412, "conditionNotMet");
    }
    for role in [
        "",
        "storage.admin",
        "roles/storage.unknown",
        "roles/storage.hmacKeyAdmin",
        "roles/storage.expressModeUserAccess",
        "roles/owner/extra",
        "projects//roles/name",
        "projects/project/roles/",
        "organizations/a/roles/b/extra",
    ] {
        set(
            &server,
            ADMIN,
            &json!({"bindings":[binding(role, "user:local")]}),
        )
        .auth_error(400, "invalid");
    }
    for member in [
        "",
        "user",
        "user:",
        "user:a b",
        "user:a\nb",
        "user:日本語",
        "unknown:local",
        "group:",
        "domain:",
        "principal:",
        "principal://",
        "principalSet://",
        "projectOwner:",
    ] {
        set(
            &server,
            ADMIN,
            &json!({"bindings":[binding("roles/storage.objectViewer", member)]}),
        )
        .auth_error(400, "invalid");
    }
    let too_long = format!("user:{}", "x".repeat(255));
    set(
        &server,
        ADMIN,
        &json!({"bindings":[binding("roles/storage.objectViewer", &too_long)]}),
    )
    .auth_error(400, "invalid");
    assert_eq!(server.get(ADMIN, IAM).success().json(), before);
    // A binding without members grants nothing; Cloud Storage dropped it from the stored policy
    // instead of rejecting it (recorded 2026-10-31).
    let kept = set(
        &server,
        ADMIN,
        &json!({"bindings":[binding("roles/storage.objectViewer", "user:local"), {"role":"roles/storage.objectViewer", "members":[]}]}),
    )
    .success()
    .json();
    assert_eq!(
        kept["bindings"],
        json!([binding("roles/storage.objectViewer", "user:local")])
    );
}

#[test]
fn iam_rejects_unsupported_policy_semantics() {
    let (_config, _data, server) = fixture();
    let before = server.get(ADMIN, IAM).success().json();
    for role in [
        "roles/owner",
        "roles/editor",
        "roles/viewer",
        "projects/project-a/roles/custom",
        "organizations/123/roles/custom",
    ] {
        set(
            &server,
            ADMIN,
            &json!({"bindings":[binding(role, "user:local")]}),
        )
        .auth_error(400, "badRequest");
    }
    for member in [
        "group:local@example.invalid",
        "domain:example.invalid",
        "principal://iam.googleapis.com/locations/global/workforcePools/local/subject/local",
        "principalSet://iam.googleapis.com/projects/123/locations/global/workloadIdentityPools/local/*",
        "deleted:user:local@example.invalid?uid=123",
    ] {
        set(
            &server,
            ADMIN,
            &json!({"bindings":[binding("roles/storage.objectViewer", member)]}),
        )
        .auth_error(400, "badRequest");
    }
    for condition in [
        json!(null),
        json!({}),
        json!({"expression":"true"}),
        json!(false),
    ] {
        let mut entry = binding("roles/storage.objectViewer", "user:local");
        entry["condition"] = condition;
        set(&server, ADMIN, &json!({"bindings":[entry]})).auth_error(400, "badRequest");
    }
    set(&server, ADMIN, &json!({"auditConfigs":[]})).auth_error(400, "badRequest");
    assert_eq!(server.get(ADMIN, IAM).success().json(), before);
}

#[test]
fn iam_rejects_queries_paths_and_bodies() {
    let (_config, _data, server) = fixture();
    for query in [
        "optionsRequestedPolicyVersion=",
        "optionsRequestedPolicyVersion=true",
        "optionsRequestedPolicyVersion=1&optionsRequestedPolicyVersion=3",
        "unknown=value",
        "unknown=%ff",
    ] {
        server
            .get(ADMIN, &format!("{IAM}?{query}"))
            .auth_error(400, "invalidParameter");
    }
    // Cloud Storage answered an out-of-range version with its version 1 policy (recorded
    // 2026-10-31), so any integer is accepted.
    for version in ["0", "2", "4", "-1"] {
        let policy = server
            .get(
                ADMIN,
                &format!("{IAM}?optionsRequestedPolicyVersion={version}"),
            )
            .success()
            .json();
        assert_eq!(policy["version"], 1, "{version}");
    }
    for method in ["GET", "PUT"] {
        for query in ["userProject=project-a", "userProject=", "fields="] {
            server
                .request(method, &format!("{IAM}?{query}"), ADMIN, b"")
                .auth_error(400, "badRequest");
        }
    }
    server
        .request(
            "PUT",
            &format!("{IAM}?optionsRequestedPolicyVersion=1"),
            ADMIN,
            b"{}",
        )
        .auth_error(400, "invalidParameter");
    server
        .get(ADMIN, "/storage/v1/b/BAD/iam")
        .auth_error(400, "invalidParameter");
    server
        .get(ADMIN, "/storage/v1/b/%ff/iam")
        .auth_error(400, "invalidParameter");
    server
        .request("POST", IAM, ADMIN, b"")
        .auth_error(405, "methodNotAllowed");
    server
        .request("GET", IAM, ADMIN, b"{}")
        .auth_error(400, "invalidArgument");
    for bytes in [b"{".as_slice(), b"", b"\xff"] {
        server
            .request("PUT", IAM, ADMIN, bytes)
            .auth_error(400, "invalid");
    }
    server
        .get(ADMIN, "/storage/v1/b/iam-bucket/iam/testPermissions")
        .auth_error(400, "required");
    server
        .get(ADMIN, "/storage/v1/b/missing-bucket/iam")
        .auth_error(404, "notFound");
    server
        .get(NO_ROLES, "/storage/v1/b/missing-bucket/iam")
        .forbidden();
}

#[test]
fn iam_principal_and_body_limits() {
    let (_config, _data, server) = fixture();
    let member = "user:local";
    // The limit counts occurrences as sent; the stored policy then keeps each member once.
    let max =
        json!({"bindings":[{"role":"roles/storage.objectViewer", "members":vec![member;1500]}]});
    assert_eq!(
        set(&server, ADMIN, &max).success().json()["bindings"][0]["members"],
        json!([member])
    );
    let mut excess = max;
    excess["bindings"]
        .as_array_mut()
        .unwrap()
        .push(binding("roles/storage.objectCreator", member));
    set(&server, ADMIN, &excess).auth_error(400, "invalid");
    let longest = format!("user:{}", "x".repeat(254));
    set(
        &server,
        ADMIN,
        &json!({"bindings":[binding("roles/storage.objectViewer", &longest)]}),
    )
    .success();
    // The documented principal count with long names takes far more than 64 KiB, and has to
    // survive storage as well as the request limit.
    let long_members: Vec<_> = (0..1500)
        .map(|index| format!("user:{index:04}{}", "x".repeat(250)))
        .collect();
    let long = json!({"bindings":[{"role":"roles/storage.objectViewer","members":long_members}]});
    assert!(long.to_string().len() > 256 * 1024);
    set(&server, ADMIN, &long).success();
    assert_eq!(
        server.get(ADMIN, IAM).success().json()["bindings"][0]["members"],
        long["bindings"][0]["members"]
    );
    const BODY_LIMIT: usize = 16 * 1024 * 1024;
    let exact = format!(
        "{{\"bindings\":[],\"padding\":\"{}\"}}",
        "x".repeat(BODY_LIMIT - 28)
    );
    assert_eq!(exact.len(), BODY_LIMIT);
    server
        .request("PUT", IAM, ADMIN, exact.as_bytes())
        .success();
    let too_large = format!("{exact} ");
    server
        .request("PUT", IAM, ADMIN, too_large.as_bytes())
        .auth_error(413, "uploadTooLarge");
    let mut stream = server.stream();
    write!(stream, "PUT {IAM} HTTP/1.1\r\nHost: local\r\n{ADMIN}Transfer-Encoding: chunked\r\n\r\n{:x}\r\n{too_large}\r\n0\r\n\r\n", too_large.len()).unwrap();
    Reply::read_joined(stream).auth_error(413, "uploadTooLarge");
}
