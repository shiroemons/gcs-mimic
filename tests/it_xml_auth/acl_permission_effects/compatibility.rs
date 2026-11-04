use super::*;

#[test]
fn legacy_bucket_views_reconcile_before_ready_and_keep_role_union_and_restart_etags() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let mut server = Server::start(&data, &inputs, "disk");
    server.bucket();
    server.put(b"legacy private bytes");
    let iam = format!("{BUCKET}/iam");
    server
        .admin(
            "PUT",
            &iam,
            json!({"bindings":[
                {"role":"roles/storage.legacyBucketWriter","members":["user:reader@example.test"]},
                {"role":"roles/storage.bucketViewer","members":["user:other@example.test"]}
            ]}),
        )
        .json_ok();
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{
                "publicAccessPrevention":"enforced","uniformBucketLevelAccess":{"enabled":true}
            }}),
        )
        .json_ok();
    server.stop();
    {
        let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
        common::downgrade_deletion_history_schema(&connection);
        // Older catalogs stored ACLs independently of their legacy IAM view.
        connection.execute("UPDATE acl_bucket_entries SET entries=?1 WHERE bucket='test-bucket' AND scope='bucket'", [json!([
            {"entity":"user-reader@example.test","role":"OWNER"},
            {"entity":"allUsers","role":"READER"}
        ]).to_string()]).unwrap();
        connection.execute_batch("ALTER TABLE buckets DROP COLUMN versioning_configured; DROP TABLE soft_deleted_buckets; DROP TABLE bucket_generation; ALTER TABLE buckets DROP COLUMN generation; ALTER TABLE objects DROP COLUMN temporary_hold_released; ALTER TABLE objects DROP COLUMN event_based_hold_released; ALTER TABLE objects DROP COLUMN retain_until_sub_micro_nanos; ALTER TABLE objects DROP COLUMN owner; ALTER TABLE buckets DROP COLUMN acl_policy_bound; PRAGMA user_version=42;").unwrap();
    }
    let mut server = Server::start(&data, &inputs, "disk");
    let policy = server.request("GET", &iam, ADMIN, b"").json_ok();
    for role in [
        "roles/storage.legacyBucketWriter",
        "roles/storage.legacyBucketOwner",
    ] {
        assert_eq!(
            super::mirror::role(&policy, role),
            ["user:reader@example.test"]
        );
    }
    assert_eq!(
        super::mirror::role(&policy, "roles/storage.legacyBucketReader"),
        ["allUsers"]
    );
    assert_eq!(
        super::mirror::role(&policy, "roles/storage.bucketViewer"),
        ["user:other@example.test"]
    );
    let bucket = server.request("GET", BUCKET, ADMIN, b"").json_ok();
    server.request("GET", BUCKET, READER, b"").json_ok();
    server
        .request("GET", &format!("{BUCKET}/o"), READER, b"")
        .json_ok();
    server.request("GET", BUCKET, "", b"").expect(401);
    server.request("GET", OBJECT, READER, b"").expect(403);
    server
        .request("GET", &format!("{BUCKET}/acl"), ADMIN, b"")
        .expect(400);
    server.stop();
    let server = Server::start(&data, &inputs, "disk");
    assert_eq!(server.request("GET", &iam, ADMIN, b"").json_ok(), policy);
    assert_eq!(server.request("GET", BUCKET, ADMIN, b"").json_ok(), bucket);
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false}}}),
        )
        .json_ok();
    assert_eq!(
        server
            .request("GET", &format!("{BUCKET}/acl/allUsers"), ADMIN, b"")
            .json_ok()["role"],
        "READER"
    );
    server.request("GET", BUCKET, "", b"").expect(401);
    server
        .request(
            "DELETE",
            &format!("{BUCKET}/acl/user-reader%40example.test"),
            ADMIN,
            b"",
        )
        .expect(204);
    server.request("GET", BUCKET, READER, b"").expect(403);
    server.request("GET", BUCKET, OTHER, b"").json_ok();
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), ADMIN, b"")
            .body,
        b"legacy private bytes"
    );
}
