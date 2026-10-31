use super::*;

pub(super) fn role(policy: &Value, name: &str) -> Vec<String> {
    policy["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|binding| binding["role"] == name)
        .flat_map(|binding| binding["members"].as_array().unwrap())
        .map(|member| member.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn bucket_acl_and_legacy_iam_share_revocation_without_changing_other_bindings() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        server.put(b"unchanged private bytes");
        let iam = format!("{BUCKET}/iam");
        let initial = server.admin("PUT", &iam, json!({"bindings":[
            {"role":"roles/storage.bucketViewer","members":["user:other@example.test"]},
            {"role":"roles/storage.legacyBucketReader","members":["user:reader@example.test"]}
        ]})).json();
        let acl = server
            .request(
                "GET",
                &format!("{BUCKET}/acl/user-reader%40example.test"),
                ADMIN,
                b"",
            )
            .json();
        assert_eq!(acl["role"], "READER");
        super::bucket::grant(&server, "user-reader@example.test", "WRITER");
        let changed = server.request("GET", &iam, ADMIN, b"").json();
        assert_ne!(changed["etag"], initial["etag"]);
        assert!(role(&changed, "roles/storage.legacyBucketReader").is_empty());
        assert_eq!(
            role(&changed, "roles/storage.legacyBucketWriter"),
            ["user:reader@example.test"]
        );
        assert_eq!(
            role(&changed, "roles/storage.bucketViewer"),
            ["user:other@example.test"]
        );
        assert_eq!(
            server
                .admin("PUT", &iam, json!({"etag":initial["etag"],"bindings":[]}))
                .status,
            412
        );
        assert_eq!(server.request("GET", &iam, ADMIN, b"").json(), changed);
        assert_eq!(server.request("GET", OBJECT, READER, b"").status, 403);
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{BUCKET}/acl/user-reader%40example.test"),
                    ADMIN,
                    b""
                )
                .status,
            204
        );
        let revoked = server.request("GET", &iam, ADMIN, b"").json();
        assert!(role(&revoked, "roles/storage.legacyBucketWriter").is_empty());
        assert_eq!(
            role(&revoked, "roles/storage.bucketViewer"),
            ["user:other@example.test"]
        );
        assert_eq!(server.request("GET", BUCKET, READER, b"").status, 403);
        server.request("GET", BUCKET, OTHER, b"").json();
        let before = server.request("GET", BUCKET, ADMIN, b"").json();
        server
            .admin(
                "PUT",
                &iam,
                json!({"etag":revoked["etag"],"bindings":revoked["bindings"]}),
            )
            .json();
        assert_eq!(server.request("GET", BUCKET, ADMIN, b"").json(), before);
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?alt=media"), ADMIN, b"")
                .body,
            b"unchanged private bytes"
        );
    }
}

#[test]
fn trusted_user_alias_matches_iam_and_acl_without_changing_the_local_principal_id() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let registry = json!({"identities":[
        {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
        {"id":"local:reader-user","acl_entity":"user-reader@example.test","tokens":["reader"]},
        {"id":"other@example.test","tokens":["other"]}
    ]});
    let mut server = Server::start_with_registry(&data, &inputs, "disk", &registry);
    server.bucket();
    let object = server.put(b"aliased private object");
    server.grant(
        "user-reader@example.test",
        "READER",
        object["generation"].as_str().unwrap(),
    );
    server.request("GET", OBJECT, READER, b"").json();
    assert_eq!(server.request("GET", OBJECT, OTHER, b"").status, 403);
    let iam = format!("{BUCKET}/iam");
    server.admin("PUT", &iam, json!({"bindings":[{"role":"roles/storage.legacyBucketReader","members":["user:local:reader-user"]}]})).json();
    assert_eq!(
        server
            .request(
                "GET",
                &format!("{BUCKET}/acl/user-reader%40example.test"),
                ADMIN,
                b""
            )
            .json()["role"],
        "READER"
    );
    server.request("GET", BUCKET, READER, b"").json();
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("{BUCKET}/acl/user-reader%40example.test"),
                ADMIN,
                b""
            )
            .status,
        204
    );
    assert_eq!(server.request("GET", BUCKET, READER, b"").status, 403);
    server.stop();
    let mut changed = registry;
    changed["identities"][1]["acl_entity"] = json!("user-new@example.test");
    let server = Server::start_with_registry(&data, &inputs, "disk", &changed);
    assert_eq!(server.request("GET", OBJECT, READER, b"").status, 403);
    assert_eq!(server.request("GET", OBJECT, OTHER, b"").status, 403);
    assert_eq!(
        server
            .request(
                "GET",
                OBJECT,
                "Authorization: Bearer local:reader-user\r\n",
                b""
            )
            .status,
        401
    );
    server.request("GET", OBJECT, ADMIN, b"").json();
}

#[test]
fn unrelated_acl_edits_preserve_stable_iam_ids_across_alias_changes() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let registry = json!({"identities":[
        {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
        {"id":"local:reader-user","acl_entity":"user-reader@example.test","tokens":["reader"]},
        {"id":"other@example.test","tokens":["other"]}
    ]});
    let mut server = Server::start_with_registry(&data, &inputs, "disk", &registry);
    server.bucket();
    let object = server.put(b"ACL-only alias bytes");
    server.grant(
        "user-reader@example.test",
        "READER",
        object["generation"].as_str().unwrap(),
    );
    let iam = format!("{BUCKET}/iam");
    server.admin("PUT", &iam, json!({"bindings":[{"role":"roles/storage.legacyBucketReader","members":["user:local:reader-user"]}]})).json();
    super::bucket::grant(&server, "user-unregistered@example.test", "READER");
    let policy = server.request("GET", &iam, ADMIN, b"").json();
    assert!(
        role(&policy, "roles/storage.legacyBucketReader")
            .contains(&"user:local:reader-user".into())
    );
    assert!(
        !role(&policy, "roles/storage.legacyBucketReader")
            .contains(&"user:reader@example.test".into())
    );
    let metadata = server
        .request("GET", &format!("{BUCKET}?projection=full"), ADMIN, b"")
        .json();
    server
        .admin("PATCH", BUCKET, json!({"acl":metadata["acl"]}))
        .json();
    assert_eq!(server.request("GET", &iam, ADMIN, b"").json(), policy);
    server.stop();
    let mut changed = registry;
    changed["identities"][1]["acl_entity"] = json!("user-new@example.test");
    changed["identities"][2]["acl_entity"] = json!("user-reader@example.test");
    let mut server = Server::start_with_registry(&data, &inputs, "disk", &changed);
    server.request("GET", BUCKET, READER, b"").json();
    server
        .request("GET", &format!("{BUCKET}/o"), READER, b"")
        .json();
    assert_eq!(server.request("GET", OBJECT, READER, b"").status, 403);
    assert_eq!(server.request("GET", BUCKET, OTHER, b"").status, 403);
    assert!(
        role(
            &server.request("GET", &iam, ADMIN, b"").json(),
            "roles/storage.legacyBucketReader"
        )
        .contains(&"user:local:reader-user".into())
    );
    server.stop();
    let server = Server::start_with_registry(&data, &inputs, "disk", &changed);
    server.request("GET", BUCKET, READER, b"").json();
    assert_eq!(server.request("GET", BUCKET, OTHER, b"").status, 403);
}
