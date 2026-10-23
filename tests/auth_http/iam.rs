use super::*;
use std::sync::{Arc, Barrier};

pub(super) const IAM: &str = "/storage/v1/b/iam-bucket/iam";

pub(super) fn fixture() -> (TestDir, TestDir, Server) {
    let config = TestDir::new();
    let identities = identity_file(&config);
    let data = TestDir::new();
    let server = Server::strict(&data, &identities);
    server.create_bucket(ADMIN, "project-a", "iam-bucket").ok();
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
fn iam_default_policy_and_versions() {
    let (_config, _data, server) = fixture();
    let policy = server.get(ADMIN, IAM).ok().json();
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
                .ok()
                .json(),
            policy
        );
    }
    server.get(NO_ROLES, "/storage/v1/b/iam-bucket").forbidden();
}

#[test]
fn iam_policy_round_trip_and_unconditional_versions() {
    let (_config, _data, server) = fixture();
    let mut previous = server.get(ADMIN, IAM).ok().json()["etag"].clone();
    for version in [0, 1, 3] {
        let policy = json!({"version":version, "kind":false, "resourceId":null, "unknown":true,
            "bindings":[{"role":"roles/storage.objectViewer", "members":["user:viewer@example.invalid", "serviceAccount:no-roles@example.invalid", "projectOwner:another-project", "projectEditor:project-a", "projectViewer:project-a"], "unknown":true}]});
        let result = set(&server, ADMIN, &policy).ok().json();
        assert_eq!(result["version"], 1);
        assert_eq!(
            result["bindings"][0]["members"],
            policy["bindings"][0]["members"]
        );
        assert_ne!(result["etag"], previous);
        previous = result["etag"].clone();
        assert_eq!(server.get(ADMIN, IAM).ok().json(), result);
    }
    server.upload(ADMIN, "iam-bucket", "object", b"hello").ok();
    assert_eq!(
        server
            .get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object?alt=media")
            .ok()
            .body,
        b"hello"
    );
    let bucket_before = server.get(ADMIN, "/storage/v1/b/iam-bucket").ok().json();
    assert_eq!(
        set(&server, ADMIN, &json!({"bindings":[]})).ok().json()["bindings"],
        json!([])
    );
    assert_eq!(
        server.get(ADMIN, "/storage/v1/b/iam-bucket").ok().json(),
        bucket_before
    );
    server
        .get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object")
        .forbidden();
}

#[test]
fn iam_preserves_duplicate_members_and_role_bindings() {
    let (_config, _data, server) = fixture();
    let policy = json!({"bindings":[
        {"role":"roles/storage.bucketViewer", "members":["user:local", "user:local"]},
        {"role":"roles/storage.bucketViewer", "members":["user:another"]}
    ]});
    let result = set(&server, ADMIN, &policy).ok().json();
    assert_eq!(result["bindings"], policy["bindings"]);
    assert_eq!(server.get(ADMIN, IAM).ok().json(), result);
}

#[test]
fn iam_policy_survives_restart() {
    let (config, data, mut server) = fixture();
    let policy = set(&server, ADMIN, &grants(&["roles/storage.objectViewer"]))
        .ok()
        .json();
    server
        .upload(ADMIN, "iam-bucket", "object", b"persistent")
        .ok();
    server.stop();
    let server = Server::strict(&data, &config.0.join("identities.json"));
    assert_eq!(server.get(ADMIN, IAM).ok().json(), policy);
    assert_eq!(
        server
            .get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object?alt=media")
            .ok()
            .body,
        b"persistent"
    );
}

#[test]
fn iam_migrates_v5_catalog() {
    let (config, data, mut server) = fixture();
    server
        .upload(ADMIN, "iam-bucket", "object", b"before migration")
        .ok();
    server.stop();
    let path = data.0.join("catalog.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    common::downgrade_object_schema(&connection);
    connection
        .execute_batch(
            "ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold; DROP TABLE resumable_sessions; DROP TABLE bucket_incarnations; DROP TABLE bucket_policies; DROP TABLE policy_revision; PRAGMA user_version = 5;",
        )
        .unwrap();
    drop(connection);
    let mut server = Server::strict(&data, &config.0.join("identities.json"));
    assert_eq!(
        server.get(ADMIN, IAM).ok().json()["bindings"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/iam-bucket/o/object?alt=media")
            .ok()
            .body,
        b"before migration"
    );
    let policy = set(&server, ADMIN, &grants(&["roles/storage.objectViewer"]))
        .ok()
        .json();
    server.stop();
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        13
    );
    drop(connection);
    let server = Server::strict(&data, &config.0.join("identities.json"));
    assert_eq!(server.get(ADMIN, IAM).ok().json(), policy);
}

#[test]
fn iam_permissive_policy_and_cas() {
    let data = TestDir::new();
    let (mut server, _) = Server::start(&data, &[]);
    server.create_bucket("", "project-a", "iam-bucket").ok();
    let initial = server.get("", IAM).ok().json();
    let policy = set(
        &server,
        "Authorization: nonsense\r\n",
        &grants(&["roles/storage.objectViewer"]),
    )
    .ok()
    .json();
    set(&server, "", &initial).error(412, "conditionNotMet");
    set(
        &server,
        "",
        &json!({"bindings":[binding("roles/storage.admin", "group:local@example.invalid")]}),
    )
    .error(501, "notImplemented");
    assert_eq!(server.get("", IAM).ok().json(), policy);
    server.stop();
    let (server, _) = Server::start(&data, &[]);
    assert_eq!(server.get("", IAM).ok().json(), policy);
}

#[test]
fn iam_requires_effective_get_and_set_permissions() {
    let (_config, _data, server) = fixture();
    server.get("", IAM).login_required();
    set(&server, "", &json!({})).login_required();
    server
        .get("Authorization: Bearer unknown\r\n", IAM)
        .invalid_credentials();
    for auth in [VIEWER, CREATOR, EDITOR, NO_ROLES] {
        server.get(auth, IAM).forbidden();
        set(&server, auth, &json!({})).forbidden();
    }
    set(&server, ADMIN, &grants(&["roles/storage.objectAdmin"])).ok();
    server.get(NO_ROLES, IAM).forbidden();
    set(&server, NO_ROLES, &json!({})).forbidden();
    set(&server, ADMIN, &grants(&["roles/storage.admin"])).ok();
    server.get(NO_ROLES, IAM).ok();
    set(&server, NO_ROLES, &grants(&["roles/storage.admin"])).ok();
    // Project grants remain effective after every bucket binding is removed.
    set(&server, NO_ROLES, &json!({"bindings":[]})).ok();
    server.get(NO_ROLES, IAM).forbidden();
    server.get(ADMIN, IAM).ok();
}

#[test]
fn iam_bucket_grants_authorize_only_the_target_bucket() {
    let (_config, _data, server) = fixture();
    server
        .create_bucket(ADMIN, "project-a", "other-bucket")
        .ok();
    server
        .create_bucket(ADMIN, "project-b", "foreign-bucket")
        .ok();
    server.upload(ADMIN, "iam-bucket", "object", b"hello").ok();
    set(
        &server,
        ADMIN,
        &grants(&["roles/storage.objectViewer", "roles/storage.objectCreator"]),
    )
    .ok();
    server.get(NO_ROLES, "/storage/v1/b/iam-bucket/o").ok();
    server
        .get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object")
        .ok();
    server
        .upload(NO_ROLES, "iam-bucket", "fresh", b"created")
        .ok();
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
    set(&server, ADMIN, &grants(&["roles/storage.admin"])).ok();
    server
        .get(NO_ROLES, "/storage/v1/b?project=project-a")
        .forbidden();
    server
        .create_bucket(NO_ROLES, "project-a", "never-created")
        .forbidden();
    set(&server, ADMIN, &json!({"bindings":[]})).ok();
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
            .ok()
            .body,
        b"created"
    );
}

#[test]
fn iam_bucket_grants_cover_existing_routes() {
    let (_config, _data, server) = fixture();
    set(&server, ADMIN, &grants(&["roles/storage.admin"])).ok();
    server.get(NO_ROLES, "/storage/v1/b/iam-bucket").ok();
    server.upload(NO_ROLES, "iam-bucket", "object", b"old").ok();
    server.upload(NO_ROLES, "iam-bucket", "object", b"new").ok();
    server
        .multipart(NO_ROLES, "iam-bucket", "multipart", b"multipart")
        .ok();
    server.get(NO_ROLES, "/storage/v1/b/iam-bucket/o").ok();
    server
        .get(NO_ROLES, "/storage/v1/b/iam-bucket/o/object")
        .ok();
    for path in [
        "/storage/v1/b/iam-bucket/o/object?alt=media",
        "/download/storage/v1/b/iam-bucket/o/object?alt=media",
    ] {
        assert_eq!(server.get(NO_ROLES, path).ok().body, b"new");
    }
    for method in ["PATCH", "PUT"] {
        server
            .request(
                method,
                "/storage/v1/b/iam-bucket/o/object",
                NO_ROLES,
                br#"{"metadata":{"key":"value"}}"#,
            )
            .ok();
    }
    for name in ["object", "multipart"] {
        server
            .request(
                "DELETE",
                &format!("/storage/v1/b/iam-bucket/o/{name}"),
                NO_ROLES,
                b"",
            )
            .ok();
    }
    server
        .request("DELETE", "/storage/v1/b/iam-bucket", NO_ROLES, b"")
        .ok();
}

#[test]
fn auth_http_revoked_bucket_grants_deny_every_basic_method_and_preserve_objects() {
    let (_config, data, server) = fixture();
    server.upload(ADMIN, "iam-bucket", "object", b"old").ok();
    let original = server
        .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
        .ok()
        .json();
    set(&server, ADMIN, &grants(&["roles/storage.admin"])).ok();
    server.get(NO_ROLES, "/storage/v1/b/iam-bucket").ok();
    // Bucket roles cannot supply project-level create/list permissions.
    server
        .create_bucket(NO_ROLES, "project-a", "unauthorized")
        .forbidden();
    server
        .get(NO_ROLES, "/storage/v1/b?project=project-a")
        .forbidden();
    set(&server, ADMIN, &json!({"bindings":[]})).ok();
    let bucket = server.get(ADMIN, "/storage/v1/b/iam-bucket").ok().json();
    server
        .get(ADMIN, "/storage/v1/b/unauthorized")
        .error(404, "notFound");
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
                .error(status, reason);
        }
        server
            .request(
                "GET",
                "/storage/v1/b/iam-bucket/o/object?alt=media",
                &format!("{auth}Range: bytes=0-1\r\n"),
                b"",
            )
            .error(status, reason);
        server
            .upload(auth, "iam-bucket", "object", b"new")
            .error(status, reason);
        server
            .multipart(auth, "iam-bucket", "object", b"new")
            .error(status, reason);
        assert_eq!(
            server.get(ADMIN, "/storage/v1/b/iam-bucket").ok().json(),
            bucket
        );
        server
            .get(ADMIN, "/storage/v1/b/unauthorized")
            .error(404, "notFound");
        let sessions: i64 = rusqlite::Connection::open_with_flags(
            data.0.join("catalog.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row("SELECT count(*) FROM resumable_sessions", [], |row| {
            row.get(0)
        })
        .unwrap();
        assert_eq!(sessions, 0);
        assert_eq!(
            server
                .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
                .ok()
                .json(),
            original
        );
        assert_eq!(
            server
                .get(ADMIN, "/storage/v1/b/iam-bucket/o/object?alt=media")
                .ok()
                .body,
            b"old"
        );
        assert_eq!(
            std::fs::read_dir(data.0.join("staging")).unwrap().count(),
            0
        );
        assert_eq!(
            std::fs::read_dir(data.0.join("blobs"))
                .unwrap()
                .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
                .sum::<usize>(),
            1
        );
    }
}

#[test]
fn auth_http_media_and_multipart_recheck_revoked_overwrite_grants_after_streaming() {
    let (_config, data, server) = fixture();
    server.upload(ADMIN, "iam-bucket", "object", b"old").ok();
    let original = server
        .request(
            "PATCH",
            "/storage/v1/b/iam-bucket/o/object",
            ADMIN,
            br#"{"contentType":"text/old","metadata":{"key":"old"}}"#,
        )
        .ok()
        .json();
    for upload in ["media", "multipart"] {
        set(&server, ADMIN, &grants(&["roles/storage.admin"])).ok();
        let body = if upload == "media" {
            b"new".to_vec()
        } else {
            b"--boundary\r\nContent-Type: application/json\r\n\r\n{\"metadata\":{\"key\":\"new\"}}\r\n--boundary\r\n\r\nnew\r\n--boundary--\r\n".to_vec()
        };
        let mut stream = server.stream();
        write!(stream, "POST /upload/storage/v1/b/iam-bucket/o?uploadType={upload}&name=object HTTP/1.1\r\nHost: local\r\n{NO_ROLES}Content-Type: multipart/related; boundary=boundary\r\nContent-Length: {}\r\n\r\n", body.len()).unwrap();
        stream.write_all(&body[..body.len() - 1]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while std::fs::read_dir(data.0.join("staging")).unwrap().count() != 1 {
            assert!(
                Instant::now() < deadline,
                "upload must pass advisory authorization"
            );
            thread::sleep(Duration::from_millis(5));
        }
        set(&server, ADMIN, &json!({"bindings":[]})).ok();
        stream.write_all(&body[body.len() - 1..]).unwrap();
        Reply::read(stream).forbidden();
        assert_eq!(
            server
                .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
                .ok()
                .json(),
            original
        );
        assert_eq!(
            server
                .get(ADMIN, "/storage/v1/b/iam-bucket/o/object?alt=media")
                .ok()
                .body,
            b"old"
        );
        assert_eq!(
            std::fs::read_dir(data.0.join("staging")).unwrap().count(),
            0
        );
        assert_eq!(
            std::fs::read_dir(data.0.join("blobs"))
                .unwrap()
                .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
                .sum::<usize>(),
            1
        );
    }
}

#[test]
fn iam_stale_and_concurrent_etags() {
    let (_config, _data, server) = fixture();
    let initial = server.get(ADMIN, IAM).ok().json();
    let updated = set(&server, ADMIN, &initial).ok().json();
    set(&server, ADMIN, &initial).error(412, "conditionNotMet");
    assert_eq!(server.get(ADMIN, IAM).ok().json(), updated);
    let updated = set(&server, ADMIN, &json!({"etag":"", "bindings":[]}))
        .ok()
        .json();
    assert_eq!(updated["bindings"], json!([]));
    let mut unpadded = updated.clone();
    unpadded["etag"] = json!(updated["etag"].as_str().unwrap().trim_end_matches('='));
    let updated = set(&server, ADMIN, &unpadded).ok().json();
    set(&server, ADMIN, &json!({"etag":"c3RhbGU=", "bindings":[]})).error(412, "conditionNotMet");
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
    assert_ne!(server.get(ADMIN, IAM).ok().json()["etag"], updated["etag"]);
}

#[test]
fn iam_revocation_races_object_reads() {
    let (_config, _data, server) = fixture();
    server
        .upload(ADMIN, "iam-bucket", "object", b"selected")
        .ok();
    for _ in 0..12 {
        set(&server, ADMIN, &grants(&["roles/storage.objectViewer"])).ok();
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
            set(&server, ADMIN, &json!({"bindings":[]})).ok();
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
    let original = server.get(ADMIN, IAM).ok().json();
    let edited = set(&server, ADMIN, &grants(&["roles/storage.admin"]))
        .ok()
        .json();
    server
        .request("DELETE", "/storage/v1/b/iam-bucket", ADMIN, b"")
        .ok();
    server.create_bucket(ADMIN, "project-a", "iam-bucket").ok();
    let recreated = server.get(ADMIN, IAM).ok().json();
    assert_eq!(recreated["bindings"], original["bindings"]);
    assert_ne!(recreated["etag"], original["etag"]);
    assert_ne!(recreated["etag"], edited["etag"]);
    set(&server, ADMIN, &edited).error(412, "conditionNotMet");
    server.get(NO_ROLES, IAM).forbidden();
}

#[test]
fn iam_rejects_invalid_policy_shapes() {
    let (_config, _data, server) = fixture();
    let before = server.get(ADMIN, IAM).ok().json();
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
        json!({"etag":"AB==", "bindings":[]}),
        json!({"bindings":null}),
        json!({"bindings":{}}),
        json!({"bindings":[null]}),
        json!({"bindings":[{}]}),
        json!({"bindings":[{"role":1}]}),
        json!({"bindings":[{"role":"roles/storage.objectViewer"}]}),
        json!({"bindings":[{"role":"roles/storage.objectViewer", "members":null}]}),
        json!({"bindings":[{"role":"roles/storage.objectViewer", "members":[]}]}),
        json!({"bindings":[{"role":"roles/storage.objectViewer", "members":[1]}]}),
    ] {
        set(&server, ADMIN, &policy).error(400, "invalid");
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
        .error(400, "invalid");
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
        .error(400, "invalid");
    }
    let too_long = format!("user:{}", "x".repeat(255));
    set(
        &server,
        ADMIN,
        &json!({"bindings":[binding("roles/storage.objectViewer", &too_long)]}),
    )
    .error(400, "invalid");
    assert_eq!(server.get(ADMIN, IAM).ok().json(), before);
}

#[test]
fn iam_rejects_unsupported_policy_semantics() {
    let (_config, _data, server) = fixture();
    let before = server.get(ADMIN, IAM).ok().json();
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
        .error(501, "notImplemented");
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
        .error(501, "notImplemented");
    }
    for condition in [
        json!(null),
        json!({}),
        json!({"expression":"true"}),
        json!(false),
    ] {
        let mut entry = binding("roles/storage.objectViewer", "user:local");
        entry["condition"] = condition;
        set(&server, ADMIN, &json!({"bindings":[entry]})).error(501, "notImplemented");
    }
    set(&server, ADMIN, &json!({"auditConfigs":[]})).error(501, "notImplemented");
    assert_eq!(server.get(ADMIN, IAM).ok().json(), before);
}

#[test]
fn iam_rejects_queries_paths_and_bodies() {
    let (_config, _data, server) = fixture();
    for query in [
        "optionsRequestedPolicyVersion=0",
        "optionsRequestedPolicyVersion=2",
        "optionsRequestedPolicyVersion=4",
        "optionsRequestedPolicyVersion=-1",
        "optionsRequestedPolicyVersion=",
        "optionsRequestedPolicyVersion=true",
        "optionsRequestedPolicyVersion=1&optionsRequestedPolicyVersion=3",
        "unknown=value",
        "unknown=%ff",
    ] {
        server
            .get(ADMIN, &format!("{IAM}?{query}"))
            .error(400, "invalidParameter");
    }
    for method in ["GET", "PUT"] {
        for query in ["userProject=project-a", "userProject=", "fields="] {
            server
                .request(method, &format!("{IAM}?{query}"), ADMIN, b"")
                .error(501, "notImplemented");
        }
    }
    server
        .request(
            "PUT",
            &format!("{IAM}?optionsRequestedPolicyVersion=1"),
            ADMIN,
            b"{}",
        )
        .error(400, "invalidParameter");
    server
        .get(ADMIN, "/storage/v1/b/BAD/iam")
        .error(400, "invalidParameter");
    server
        .get(ADMIN, "/storage/v1/b/%ff/iam")
        .error(400, "invalidParameter");
    server
        .request("POST", IAM, ADMIN, b"")
        .error(405, "methodNotAllowed");
    server
        .request("GET", IAM, ADMIN, b"{}")
        .error(400, "invalidArgument");
    for bytes in [b"{".as_slice(), b"", b"\xff"] {
        server
            .request("PUT", IAM, ADMIN, bytes)
            .error(400, "parseError");
    }
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/iam-bucket/iam/testPermissions")
            .status,
        404
    );
    server
        .get(ADMIN, "/storage/v1/b/missing-bucket/iam")
        .error(404, "notFound");
    server
        .get(NO_ROLES, "/storage/v1/b/missing-bucket/iam")
        .forbidden();
}

#[test]
fn iam_principal_and_body_limits() {
    let (_config, _data, server) = fixture();
    let member = "user:local";
    let max =
        json!({"bindings":[{"role":"roles/storage.objectViewer", "members":vec![member;1500]}]});
    assert_eq!(
        set(&server, ADMIN, &max).ok().json()["bindings"][0]["members"]
            .as_array()
            .unwrap()
            .len(),
        1500
    );
    let mut excess = max.clone();
    excess["bindings"]
        .as_array_mut()
        .unwrap()
        .push(binding("roles/storage.objectCreator", member));
    set(&server, ADMIN, &excess).error(400, "invalid");
    let longest = format!("user:{}", "x".repeat(254));
    set(
        &server,
        ADMIN,
        &json!({"bindings":[binding("roles/storage.objectViewer", &longest)]}),
    )
    .ok();
    let exact = format!(
        "{{\"bindings\":[],\"padding\":\"{}\"}}",
        "x".repeat(65536 - 28)
    );
    assert_eq!(exact.len(), 65536);
    server.request("PUT", IAM, ADMIN, exact.as_bytes()).ok();
    let too_large = format!("{exact} ");
    server
        .request("PUT", IAM, ADMIN, too_large.as_bytes())
        .error(413, "uploadTooLarge");
    let mut stream = server.stream();
    write!(stream, "PUT {IAM} HTTP/1.1\r\nHost: local\r\n{ADMIN}Transfer-Encoding: chunked\r\n\r\n{:x}\r\n{too_large}\r\n0\r\n\r\n", too_large.len()).unwrap();
    Reply::read(stream).error(413, "uploadTooLarge");
}
