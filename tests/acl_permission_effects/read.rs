use super::*;

#[test]
fn object_acl_reads_are_generation_scoped_on_disk_and_memory() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        let first = server.put(b"first\0\xff");
        let generation = first["generation"].as_str().unwrap();
        server.grant("user-reader@example.test", "READER", generation);
        let path = format!("{OBJECT}?generation={generation}");
        let record = server.request("GET", &path, READER, b"").json();
        assert_eq!(record["generation"], generation);
        assert!(
            server
                .request("GET", &format!("{path}&projection=full"), READER, b"")
                .json()
                .get("acl")
                .is_none()
        );
        assert_eq!(
            server
                .request("GET", &format!("{path}&alt=media"), READER, b"")
                .body,
            b"first\0\xff"
        );
        assert_eq!(
            server
                .request("HEAD", "/test-bucket/value", READER, b"")
                .status,
            200
        );
        assert_eq!(
            server
                .request("GET", "/test-bucket/value", READER, b"")
                .body,
            b"first\0\xff"
        );
        for target in [
            BUCKET,
            &format!("{BUCKET}/o"),
            &format!("{OBJECT}/acl"),
            &format!("{BUCKET}/o/other"),
            &format!("{OBJECT}?generation=1"),
        ] {
            assert_eq!(
                server.request("GET", target, READER, b"").status,
                403,
                "{target}"
            );
        }
        for method in ["PATCH", "DELETE"] {
            assert_eq!(server.request(method, OBJECT, READER, b"{}").status, 403);
        }
        assert_eq!(
            server
                .request("GET", &format!("{path}&ifGenerationMatch=1"), READER, b"")
                .status,
            412
        );
        assert_eq!(
            server
                .request("GET", &format!("{path}&ifGenerationMatch=1"), OTHER, b"")
                .status,
            403
        );
        assert_eq!(
            server
                .request("GET", &path, "Authorization: Bearer invalid\r\n", b"")
                .status,
            401
        );
        server.put(b"second");
        assert_eq!(server.request("GET", OBJECT, READER, b"").status, 403);
        assert_eq!(
            server
                .request("GET", &format!("{path}&alt=media"), READER, b"")
                .body,
            b"first\0\xff"
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
        assert_eq!(server.request("GET", &path, READER, b"").status, 403);
    }
}

#[test]
fn public_acl_pap_uniform_access_and_restart_recheck_the_selected_generation() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let mut server = Server::start(&data, &inputs, "disk");
    server.bucket();
    let object = server.put(b"public body");
    let generation = object["generation"].as_str().unwrap();
    server.grant("allAuthenticatedUsers", "READER", generation);
    assert_eq!(server.request("GET", OBJECT, "", b"").status, 401);
    server.request("GET", OBJECT, OTHER, b"").json();
    server.grant("allUsers", "READER", generation);
    let public = server.request("GET", &format!("{OBJECT}?alt=media"), "", b"");
    assert_eq!(public.body, b"public body");
    assert_eq!(public.headers["cache-control"], "public, max-age=3600");
    server.grant("user-reader@example.test", "OWNER", generation);
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}}),
        )
        .json();
    assert_eq!(server.request("GET", OBJECT, "", b"").status, 401);
    assert_eq!(server.request("GET", OBJECT, OTHER, b"").status, 403);
    server.request("GET", OBJECT, READER, b"").json();
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
        )
        .json();
    assert_eq!(server.request("GET", OBJECT, READER, b"").status, 403);
    server.request("GET", OBJECT, ADMIN, b"").json();
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false}}}),
        )
        .json();
    server.request("GET", OBJECT, READER, b"").json();
    server.stop();
    let server = Server::start(&data, &inputs, "disk");
    server.request("GET", OBJECT, READER, b"").json();
    assert_eq!(server.request("GET", OBJECT, "", b"").status, 401);
    assert_eq!(server.request("GET", OBJECT, OTHER, b"").status, 403);
}

#[test]
fn object_acl_read_does_not_bypass_customer_key_validation() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let server = Server::start(&data, &inputs, "disk");
    server.bucket();
    let key = "x-goog-encryption-algorithm: AES256\r\nx-goog-encryption-key: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\r\nx-goog-encryption-key-sha256: Zmh6rfhivXdsj8GLjp+OIAiXFIVu4jOzkCpZHQ1fKSU=\r\n";
    let object = server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            &format!("{ADMIN}{key}"),
            b"keyed body",
        )
        .json();
    server.grant(
        "user-reader@example.test",
        "READER",
        object["generation"].as_str().unwrap(),
    );
    let media = format!("{OBJECT}?alt=media");
    assert_eq!(server.request("GET", &media, READER, b"").status, 400);
    assert_eq!(
        server
            .request("GET", &media, &format!("{READER}{key}"), b"")
            .body,
        b"keyed body"
    );
    assert_eq!(server.request("GET", &media, OTHER, b"").status, 403);
}
