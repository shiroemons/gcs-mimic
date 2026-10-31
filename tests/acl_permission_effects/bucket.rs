use super::*;

pub(super) fn grant(server: &Server, entity: &str, role: &str) {
    server
        .admin(
            "POST",
            &format!("{BUCKET}/acl"),
            json!({"entity":entity,"role":role}),
        )
        .json();
}

#[test]
fn bucket_acl_roles_grant_bucket_and_write_actions_without_object_reads() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        let original = server.put(b"private existing bytes");
        let generation = original["generation"].as_str().unwrap();
        grant(&server, "user-reader@example.test", "READER");
        let bucket = server
            .request("GET", &format!("{BUCKET}?projection=full"), READER, b"")
            .json();
        assert!(bucket.get("acl").is_none());
        assert!(bucket.get("defaultObjectAcl").is_none());
        let list = server
            .request("GET", &format!("{BUCKET}/o"), READER, b"")
            .json();
        assert_eq!(list["items"][0]["generation"], generation);
        for path in [
            format!("{BUCKET}/acl"),
            format!("{BUCKET}/defaultObjectAcl"),
            OBJECT.into(),
            format!("{OBJECT}?alt=media"),
            "/storage/v1/b?project=local".into(),
        ] {
            assert_eq!(
                server.request("GET", &path, READER, b"").status,
                403,
                "{path}"
            );
        }
        assert_eq!(server.request("PATCH", BUCKET, READER, b"{}").status, 403);
        assert_eq!(server.request("DELETE", OBJECT, READER, b"").status, 403);
        let upload = "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=new";
        assert_eq!(server.request("POST", upload, READER, b"new").status, 403);

        grant(&server, "user-reader@example.test", "WRITER");
        server.request("POST", upload, READER, b"new").json();
        server
            .request("POST", upload, READER, b"replacement")
            .json();
        assert_eq!(server.request("GET", OBJECT, READER, b"").status, 403);
        assert_eq!(server.request("PATCH", OBJECT, READER, b"{}").status, 403);
        assert_eq!(
            server
                .request("GET", &format!("{BUCKET}/acl"), READER, b"")
                .status,
            403
        );
        assert_eq!(server.request("PATCH", BUCKET, READER, b"{}").status, 403);
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{OBJECT}?generation={generation}"),
                    READER,
                    b""
                )
                .status,
            204
        );
        assert_eq!(server.request("DELETE", BUCKET, READER, b"").status, 403);
    }
}

#[test]
fn bucket_acl_owner_changes_metadata_and_acls_without_other_resource_authority() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let server = Server::start(&data, &inputs, "disk");
    server.bucket();
    server.put(b"private object");
    server
        .admin(
            "POST",
            "/storage/v1/b?project=local",
            json!({"name":"other-bucket"}),
        )
        .json();
    grant(&server, "user-reader@example.test", "OWNER");
    let bucket = server
        .request("GET", &format!("{BUCKET}?projection=full"), READER, b"")
        .json();
    assert!(bucket["acl"].is_array());
    assert!(bucket["defaultObjectAcl"].is_array());
    server
        .request(
            "PATCH",
            BUCKET,
            READER,
            br#"{"labels":{"operator":"reader"}}"#,
        )
        .json();
    server
        .request(
            "POST",
            &format!("{BUCKET}/acl"),
            READER,
            br#"{"entity":"user-other@example.test","role":"READER"}"#,
        )
        .json();
    server
        .request(
            "POST",
            &format!("{BUCKET}/defaultObjectAcl"),
            READER,
            br#"{"entity":"user-other@example.test","role":"READER"}"#,
        )
        .json();
    server.request("GET", BUCKET, OTHER, b"").json();
    for path in [
        OBJECT,
        "/storage/v1/b/other-bucket",
        "/storage/v1/b/other-bucket/o",
    ] {
        assert_eq!(
            server.request("GET", path, READER, b"").status,
            403,
            "{path}"
        );
    }
    assert_eq!(server.request("PATCH", OBJECT, READER, b"{}").status, 403);
    assert_eq!(server.request("DELETE", BUCKET, READER, b"").status, 403);
}

#[test]
fn bucket_acl_public_grants_obey_pap_ubla_and_restart_without_suppressing_iam() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let mut server = Server::start(&data, &inputs, "disk");
    server.bucket();
    server.put(b"private");
    grant(&server, "allAuthenticatedUsers", "READER");
    assert_eq!(server.request("GET", BUCKET, "", b"").status, 401);
    server.request("GET", BUCKET, OTHER, b"").json();
    grant(&server, "allUsers", "READER");
    server
        .request("GET", &format!("{BUCKET}/o"), "", b"")
        .json();
    assert_eq!(server.request("GET", OBJECT, "", b"").status, 401);
    grant(&server, "user-reader@example.test", "READER");
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}}),
        )
        .json();
    assert_eq!(server.request("GET", BUCKET, "", b"").status, 401);
    assert_eq!(server.request("GET", BUCKET, OTHER, b"").status, 403);
    server.request("GET", BUCKET, READER, b"").json();
    server.stop();
    let server = Server::start(&data, &inputs, "disk");
    server.request("GET", BUCKET, READER, b"").json();
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
        )
        .json();
    server.request("GET", BUCKET, READER, b"").json();
    server.admin("PUT", &format!("{BUCKET}/iam"), json!({"bindings":[{"role":"roles/storage.bucketViewer","members":["user:reader@example.test"]}]})).json();
    server.request("GET", BUCKET, READER, b"").json();
    assert_eq!(
        server
            .request("GET", &format!("{BUCKET}/o"), READER, b"")
            .status,
        403
    );
    assert_eq!(
        server
            .request("GET", &format!("{BUCKET}/acl"), ADMIN, b"")
            .status,
        400
    );
}
