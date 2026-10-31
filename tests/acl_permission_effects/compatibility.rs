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
        .json();
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{
                "publicAccessPrevention":"enforced","uniformBucketLevelAccess":{"enabled":true}
            }}),
        )
        .json();
    server.stop();
    {
        let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
        // Older catalogs stored ACLs independently of their legacy IAM view.
        connection.execute("UPDATE acl_bucket_entries SET entries=?1 WHERE bucket='test-bucket' AND scope='bucket'", [json!([
            {"entity":"user-reader@example.test","role":"OWNER"},
            {"entity":"allUsers","role":"READER"}
        ]).to_string()]).unwrap();
        connection.execute_batch("ALTER TABLE objects DROP COLUMN owner; ALTER TABLE buckets DROP COLUMN acl_policy_bound; PRAGMA user_version=42;").unwrap();
    }
    let mut server = Server::start(&data, &inputs, "disk");
    let policy = server.request("GET", &iam, ADMIN, b"").json();
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
    let bucket = server.request("GET", BUCKET, ADMIN, b"").json();
    server.request("GET", BUCKET, READER, b"").json();
    server
        .request("GET", &format!("{BUCKET}/o"), READER, b"")
        .json();
    assert_eq!(server.request("GET", BUCKET, "", b"").status, 401);
    assert_eq!(server.request("GET", OBJECT, READER, b"").status, 403);
    assert_eq!(
        server
            .request("GET", &format!("{BUCKET}/acl"), ADMIN, b"")
            .status,
        400
    );
    server.stop();
    let server = Server::start(&data, &inputs, "disk");
    assert_eq!(server.request("GET", &iam, ADMIN, b"").json(), policy);
    assert_eq!(server.request("GET", BUCKET, ADMIN, b"").json(), bucket);
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false}}}),
        )
        .json();
    assert_eq!(
        server
            .request("GET", &format!("{BUCKET}/acl/allUsers"), ADMIN, b"")
            .json()["role"],
        "READER"
    );
    assert_eq!(server.request("GET", BUCKET, "", b"").status, 401);
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
    server.request("GET", BUCKET, OTHER, b"").json();
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), ADMIN, b"")
            .body,
        b"legacy private bytes"
    );
}
