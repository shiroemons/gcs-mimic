use super::*;

fn full(server: &Server, generation: &str) -> Value {
    server
        .request(
            "GET",
            &format!("{OBJECT}?generation={generation}&projection=full"),
            ADMIN,
            b"",
        )
        .json()
}

#[test]
fn immutable_upload_owner_survives_acl_edits_overwrite_and_restart() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let mut server = Server::start(&data, &inputs, backend);
        server.bucket();
        super::bucket::grant(&server, "user-reader@example.test", "WRITER");
        let original = server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
                READER,
                b"reader owns this generation",
            )
            .json();
        let generation = original["generation"].as_str().unwrap();
        assert!(original.get("owner").is_none());
        assert_eq!(
            full(&server, generation)["owner"],
            json!({"entity":"user-reader@example.test"})
        );
        for owner in [
            json!({"entity":"user-other@example.test"}),
            json!({"entity":"user-reader@example.test","entityId":"invented"}),
            json!(null),
            json!("user-reader@example.test"),
        ] {
            assert_eq!(
                server
                    .admin(
                        "PATCH",
                        &format!("{OBJECT}?generation={generation}"),
                        json!({"owner":owner})
                    )
                    .status,
                400
            );
            assert_eq!(
                full(&server, generation)["owner"],
                json!({"entity":"user-reader@example.test"})
            );
        }
        let entity = format!("{OBJECT}/acl/user-reader%40example.test?generation={generation}");
        let changed = server
            .admin(
                "PUT",
                &entity,
                json!({"entity":"user-reader@example.test","role":"READER"}),
            )
            .json();
        assert_eq!(changed["role"], "OWNER");
        assert_eq!(server.request("DELETE", &entity, ADMIN, b"").status, 204);
        assert_eq!(
            server.request("GET", &entity, ADMIN, b"").json()["role"],
            "OWNER"
        );
        server
            .admin(
                "PATCH",
                &format!("{OBJECT}?generation={generation}"),
                json!({"acl":[]}),
            )
            .json();
        assert_eq!(
            server.request("GET", &entity, ADMIN, b"").json()["role"],
            "OWNER"
        );
        server.grant("user-other@example.test", "OWNER", generation);
        assert_eq!(
            full(&server, generation)["owner"],
            json!({"entity":"user-reader@example.test"})
        );
        let replacement = server.put(b"admin replacement");
        assert_eq!(
            full(&server, replacement["generation"].as_str().unwrap())["owner"],
            json!({"entity":"user-admin@example.test"})
        );
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
        let path = format!("{OBJECT}?generation={generation}&alt=media");
        assert_eq!(
            server.request("GET", &path, READER, b"").body,
            b"reader owns this generation"
        );
        assert_eq!(server.request("GET", OBJECT, READER, b"").status, 403);
        assert_eq!(
            server
                .request("GET", &format!("{BUCKET}/o"), READER, b"")
                .status,
            403
        );
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{OBJECT}?generation={generation}"),
                    READER,
                    b""
                )
                .status,
            403
        );
        server
            .admin(
                "PATCH",
                BUCKET,
                json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}}),
            )
            .json();
        assert_eq!(
            server.request("GET", &path, READER, b"").body,
            b"reader owns this generation"
        );
        server
            .admin(
                "PATCH",
                BUCKET,
                json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
            )
            .json();
        assert_eq!(server.request("GET", &path, READER, b"").status, 403);
        assert!(full(&server, generation).get("owner").is_none());
        server
            .admin(
                "PATCH",
                BUCKET,
                json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false}}}),
            )
            .json();
        if backend == "disk" {
            server.stop();
            server = Server::start(&data, &inputs, backend);
            assert_eq!(
                full(&server, generation)["owner"],
                json!({"entity":"user-reader@example.test"})
            );
            assert_eq!(
                server.request("GET", &path, READER, b"").body,
                b"reader owns this generation"
            );
        }
    }
}

#[test]
fn owner_upgrade_does_not_guess_legacy_uploaders_and_aliasless_uploads_keep_actual_ids() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let registry = json!({"identities":[
        {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
        {"id":"local:aliasless-uploader","tokens":["reader"]},
        {"id":"other@example.test","tokens":["other"]}
    ]});
    let mut server = Server::start_with_registry(&data, &inputs, "disk", &registry);
    server.bucket();
    let legacy = server.put(b"legacy bytes");
    server.stop();
    {
        let db = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
        db.execute_batch("ALTER TABLE objects DROP COLUMN owner; PRAGMA user_version=43;")
            .unwrap();
    }
    server = Server::start_with_registry(&data, &inputs, "disk", &registry);
    let old = full(&server, legacy["generation"].as_str().unwrap());
    assert!(old.get("owner").is_none());
    assert_eq!(server.request("GET", OBJECT, READER, b"").status, 403);
    server.admin("PUT", &format!("{BUCKET}/iam"), json!({"bindings":[{"role":"roles/storage.legacyBucketWriter","members":["user:local:aliasless-uploader"]}]})).json();
    let mut defaults: Vec<Value> = (0..100)
        .map(|index| json!({"entity":format!("user-member{index}@example.test"),"role":"READER"}))
        .collect();
    server
        .admin("PATCH", BUCKET, json!({"defaultObjectAcl":defaults}))
        .json();
    assert_eq!(
        server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
                READER,
                b"exceeds logical grant limit"
            )
            .status,
        400
    );
    assert_eq!(full(&server, legacy["generation"].as_str().unwrap()), old);
    defaults.pop();
    server
        .admin("PATCH", BUCKET, json!({"defaultObjectAcl":defaults}))
        .json();
    let uploaded = server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            READER,
            b"known local identity",
        )
        .json();
    let view = full(&server, uploaded["generation"].as_str().unwrap());
    assert_eq!(view["acl"].as_array().unwrap().len(), 99);
    assert_eq!(
        server
            .request(
                "POST",
                &format!("{OBJECT}/acl"),
                READER,
                br#"{"entity":"user-extra@example.test","role":"READER"}"#
            )
            .status,
        400
    );
    assert!(
        full(&server, uploaded["generation"].as_str().unwrap())
            .get("owner")
            .is_none()
    );
    server
        .admin("PUT", &format!("{BUCKET}/iam"), json!({"bindings":[]}))
        .json();
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), READER, b"")
            .body,
        b"known local identity"
    );
    assert_eq!(server.request("GET", OBJECT, OTHER, b"").status, 403);
    let owner: String = rusqlite::Connection::open(data.0.join("catalog.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT owner FROM objects WHERE time_deleted IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&owner).unwrap(),
        json!({"kind":"Principal","id":"local:aliasless-uploader","entity":null})
    );
    server.stop();
    server = Server::start_with_registry(&data, &inputs, "disk", &registry);
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), READER, b"")
            .body,
        b"known local identity"
    );
}

#[test]
fn actual_owner_id_cannot_be_replaced_by_reusing_its_canonical_alias() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let mut registry = json!({"identities":[
        {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
        {"id":"local:original-uploader","acl_entity":"user-old@example.test","tokens":["reader"]},
        {"id":"local:other-principal","tokens":["other"]}
    ]});
    let mut server = Server::start_with_registry(&data, &inputs, "disk", &registry);
    server.bucket();
    super::bucket::grant(&server, "user-old@example.test", "WRITER");
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            READER,
            b"original identity owns bytes",
        )
        .json();
    let initial = server
        .request("GET", &format!("{OBJECT}?projection=full"), ADMIN, b"")
        .json();
    server.admin("PUT", OBJECT, initial).json();
    server
        .admin(
            "PUT",
            &format!("{OBJECT}/acl/user-old%40example.test"),
            json!({"entity":"user-old@example.test","role":"READER"}),
        )
        .json();
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("{OBJECT}/acl/user-old%40example.test"),
                ADMIN,
                b""
            )
            .status,
        204
    );
    server.grant(
        "user-explicit@example.test",
        "OWNER",
        server.request("GET", OBJECT, ADMIN, b"").json()["generation"]
            .as_str()
            .unwrap(),
    );
    server.request(
        "DELETE",
        &format!("{BUCKET}/acl/user-old%40example.test"),
        ADMIN,
        b"",
    );
    server.stop();
    registry["identities"][1]["acl_entity"] = json!("user-new@example.test");
    registry["identities"][2]["acl_entity"] = json!("user-old@example.test");
    server = Server::start_with_registry(&data, &inputs, "disk", &registry);
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), READER, b"")
            .body,
        b"original identity owns bytes"
    );
    assert_eq!(server.request("GET", OBJECT, OTHER, b"").status, 403);
    assert_eq!(server.request("GET", BUCKET, OTHER, b"").status, 403);
    let view = server
        .request("GET", &format!("{OBJECT}?projection=full"), ADMIN, b"")
        .json();
    assert_eq!(view["owner"], json!({"entity":"user-new@example.test"}));
    server.admin("PUT", OBJECT, view).json();
    assert_eq!(server.request("GET", OBJECT, OTHER, b"").status, 403);
    for role in ["READER", "OWNER"] {
        let granted = server
            .admin(
                "POST",
                &format!("{OBJECT}/acl"),
                json!({"entity":"user-old@example.test","role":role}),
            )
            .json();
        assert_eq!(granted["role"], role);
        server.request("GET", OBJECT, OTHER, b"").json();
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{OBJECT}/acl/user-old%40example.test"),
                    ADMIN,
                    b""
                )
                .status,
            204
        );
        assert_eq!(server.request("GET", OBJECT, OTHER, b"").status, 403);
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?alt=media"), READER, b"")
                .body,
            b"original identity owns bytes"
        );
    }
    let stored: String = rusqlite::Connection::open(data.0.join("catalog.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT entries FROM acl_object_entries WHERE name='value'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let entries: Value = serde_json::from_str(&stored).unwrap();
    assert!(
        !entries
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["entity"] == "user-old@example.test"
                || entry["entity"] == "user-new@example.test")
    );
    assert!(
        entries
            .as_array()
            .unwrap()
            .iter()
            .any(
                |entry| entry["entity"] == "user-explicit@example.test" && entry["role"] == "OWNER"
            )
    );
}
