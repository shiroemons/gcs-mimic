use super::*;

fn replay_headers(token: &str) -> String {
    format!("{READER}X-Goog-Gcs-Idempotency-Token: {token}\r\n")
}

#[test]
fn full_upload_replay_keeps_original_owner_proof_after_generation_deletion() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        super::bucket::grant(&server, "user-reader@example.test", "WRITER");
        let path = "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value&projection=full";
        let headers = replay_headers("owned-upload-response");
        let first = server
            .request("POST", path, &headers, b"original bytes")
            .json();
        assert_eq!(first["owner"], json!({"entity":"user-reader@example.test"}));
        assert!(
            first["acl"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["entity"] == "user-reader@example.test"
                    && entry["role"] == "OWNER")
        );
        let generation = first["generation"].as_str().unwrap();
        server.grant("user-other@example.test", "READER", generation);
        assert_eq!(
            server
                .request("POST", path, &headers, b"original bytes")
                .json(),
            first
        );
        server.put(b"new private bytes");
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{OBJECT}?generation={generation}"),
                    ADMIN,
                    b""
                )
                .status,
            204
        );
        assert_eq!(
            server
                .request("POST", path, &headers, b"original bytes")
                .json(),
            first
        );
        assert_eq!(
            server
                .request("POST", path, &headers, b"changed bytes")
                .status,
            400
        );
        server
            .admin(
                "PATCH",
                BUCKET,
                json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
            )
            .json();
        let hidden = server
            .request("POST", path, &headers, b"original bytes")
            .json();
        assert!(hidden.get("owner").is_none());
        assert!(hidden.get("acl").is_none());
        server.admin("PATCH", BUCKET, json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false},"publicAccessPrevention":"enforced"}})).json();
        assert_eq!(
            server
                .request("POST", path, &headers, b"original bytes")
                .json(),
            first
        );
        let wrong = headers.replace("Bearer reader", "Bearer other");
        super::bucket::grant(&server, "user-other@example.test", "WRITER");
        assert_eq!(
            server
                .request("POST", path, &wrong, b"original bytes")
                .status,
            400
        );
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?alt=media"), ADMIN, b"")
                .body,
            b"new private bytes"
        );
    }
}

#[test]
fn upload_replay_resolves_saved_owner_id_against_current_aliases_without_rebinding_old_alias() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let mut registry = json!({"identities":[
        {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
        {"id":"local:uploader","acl_entity":"user-old@example.test","tokens":["reader"]},
        {"id":"local:other","tokens":["other"]}
    ]});
    let mut server = Server::start_with_registry(&data, &inputs, "disk", &registry);
    server.bucket();
    server.admin("PUT", &format!("{BUCKET}/iam"), json!({"bindings":[{"role":"roles/storage.legacyBucketWriter","members":["user:local:uploader"]}]})).json();
    let path = "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value&projection=full";
    let headers = replay_headers("saved-identity-owner");
    let original = server
        .request("POST", path, &headers, b"saved owner bytes")
        .json();
    assert_eq!(original["owner"], json!({"entity":"user-old@example.test"}));
    server.stop();
    registry["identities"][1]["acl_entity"] = json!("user-current@example.test");
    registry["identities"][2]["acl_entity"] = json!("user-old@example.test");
    server = Server::start_with_registry(&data, &inputs, "disk", &registry);
    let current = server
        .request("POST", path, &headers, b"saved owner bytes")
        .json();
    assert_eq!(current["generation"], original["generation"]);
    assert_eq!(current["metageneration"], original["metageneration"]);
    assert_eq!(
        current["owner"],
        json!({"entity":"user-current@example.test"})
    );
    assert!(
        !current["acl"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["entity"] == "user-old@example.test")
    );
    assert_eq!(server.request("GET", OBJECT, OTHER, b"").status, 403);
    let saved: String = rusqlite::Connection::open(data.0.join("catalog.sqlite3"))
        .unwrap()
        .query_row("SELECT completion FROM upload_replays", [], |row| {
            row.get(0)
        })
        .unwrap();
    let saved: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved["record"]["owner"]["id"], "local:uploader");
    assert_eq!(saved["record"]["owner"]["entity"], "user-old@example.test");
    assert!(
        !saved["acl"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["entity"] == "user-old@example.test")
    );
    assert_eq!(server.request("DELETE", OBJECT, ADMIN, b"").status, 204);
    let replay = server
        .request("POST", path, &headers, b"saved owner bytes")
        .json();
    assert_eq!(
        replay["owner"],
        json!({"entity":"user-current@example.test"})
    );
}

#[test]
fn full_copy_replay_binds_destination_owner_and_current_source_authority() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let server = Server::start(&data, &inputs, "disk");
    server.bucket();
    super::bucket::grant(&server, "user-reader@example.test", "WRITER");
    let source = server.put(b"source bytes");
    let generation = source["generation"].as_str().unwrap();
    server.grant("user-reader@example.test", "READER", generation);
    let path = format!("{OBJECT}/copyTo/b/test-bucket/o/copied?projection=full");
    let headers = replay_headers("owned-copy-response");
    let first = server.request("POST", &path, &headers, b"{}").json();
    assert_eq!(first["owner"], json!({"entity":"user-reader@example.test"}));
    assert_eq!(first["name"], "copied");
    assert_eq!(
        server
            .request("DELETE", &format!("{BUCKET}/o/copied"), ADMIN, b"")
            .status,
        204
    );
    assert_eq!(server.request("POST", &path, &headers, b"{}").json(), first);
    assert_eq!(
        server
            .request(
                "POST",
                &path.replace("/o/copied", "/o/other"),
                &headers,
                b"{}"
            )
            .status,
        400
    );
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("{OBJECT}/acl/user-reader%40example.test?generation={generation}"),
                ADMIN,
                b""
            )
            .status,
        204
    );
    assert_eq!(server.request("POST", &path, &headers, b"{}").status, 403);
    server.grant("user-reader@example.test", "READER", generation);
    assert_eq!(server.request("POST", &path, &headers, b"{}").json(), first);
    for (query, expected_owner, token) in [
        ("projection=noAcl", false, "no-acl-response"),
        ("projection=full&fields=owner", true, "owner-field-response"),
    ] {
        let path =
            format!("/upload/storage/v1/b/test-bucket/o?uploadType=media&name={token}&{query}");
        let value = server
            .request("POST", &path, &replay_headers(token), b"projection")
            .json();
        assert_eq!(value.get("owner").is_some(), expected_owner);
        assert!(value.get("acl").is_none());
        assert_eq!(
            server
                .request("POST", &path, &replay_headers(token), b"projection")
                .json(),
            value
        );
    }
}
