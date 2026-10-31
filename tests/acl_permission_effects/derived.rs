use super::*;

fn writer(server: &Server) {
    server.admin("PUT", &format!("{BUCKET}/iam"), json!({"bindings":[{"role":"roles/storage.legacyBucketWriter","members":["user:reader@example.test"]}]})).json();
}

#[test]
fn source_acl_copy_replay_keeps_original_generation_and_rechecks_current_controls() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let mut server = Server::start(&data, &inputs, backend);
        server.bucket();
        writer(&server);
        let original = server.put(b"original source");
        let generation = original["generation"].as_str().unwrap();
        server.grant("user-reader@example.test", "READER", generation);
        let copy = format!("{OBJECT}/copyTo/b/test-bucket/o/copied");
        let headers = format!("{READER}X-Goog-Gcs-Idempotency-Token: acl-source-copy\r\n");
        let completed = server.request("POST", &copy, &headers, b"{}").json();
        server.put(b"new private source");
        assert_eq!(server.request("GET", OBJECT, READER, b"").status, 403);
        assert_eq!(
            server.request("POST", &copy, &headers, b"{}").json(),
            completed
        );
        server
            .admin(
                "PATCH",
                BUCKET,
                json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
            )
            .json();
        assert_eq!(server.request("POST", &copy, &headers, b"{}").status, 403);
        server.admin("PATCH", BUCKET, json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false},"publicAccessPrevention":"enforced"}})).json();
        assert_eq!(
            server.request("POST", &copy, &headers, b"{}").json(),
            completed
        );
        assert_eq!(
            server
                .request(
                    "POST",
                    &copy,
                    &headers,
                    br#"{"metadata":{"changed":"yes"}}"#
                )
                .status,
            400
        );
        if backend == "disk" {
            server.stop();
            server = Server::start(&data, &inputs, backend);
            assert_eq!(
                server.request("POST", &copy, &headers, b"{}").json(),
                completed
            );
        }
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
        assert_eq!(server.request("POST", &copy, &headers, b"{}").status, 403);
        server.grant("user-reader@example.test", "READER", generation);
        assert_eq!(
            server.request("POST", &copy, &headers, b"{}").json(),
            completed
        );
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
        assert_eq!(server.request("POST", &copy, &headers, b"{}").status, 403);
        assert_eq!(
            server
                .request("GET", &format!("{BUCKET}/o/copied?alt=media"), ADMIN, b"")
                .body,
            b"original source"
        );
    }
}

#[test]
fn source_acl_authorizes_copy_compose_move_and_fixed_generation_rewrite() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        writer(&server);
        let payload = vec![b'x'; 1048576 + 7];
        let original = server.put(&payload);
        let generation = original["generation"].as_str().unwrap();
        server.grant("user-reader@example.test", "READER", generation);
        server
            .request(
                "POST",
                &format!("{OBJECT}/copyTo/b/test-bucket/o/copied"),
                READER,
                b"{}",
            )
            .json();
        server
            .request(
                "POST",
                &format!("{BUCKET}/o/composed/compose"),
                READER,
                json!({"sourceObjects":[{"name":"value","generation":generation}]})
                    .to_string()
                    .as_bytes(),
            )
            .json();
        let rewrite = format!(
            "{OBJECT}/rewriteTo/b/test-bucket/o/rewritten?sourceGeneration={generation}&maxBytesRewrittenPerCall=1048576"
        );
        let first = server
            .request("POST", &rewrite, READER, br#"{"storageClass":"NEARLINE"}"#)
            .json();
        assert_eq!(first["done"], false);
        let implicit =
            format!("{OBJECT}/rewriteTo/b/test-bucket/o/implicit?maxBytesRewrittenPerCall=1048576");
        let started = server
            .request("POST", &implicit, READER, br#"{"storageClass":"NEARLINE"}"#)
            .json();
        server.put(b"different private live");
        assert_eq!(
            server
                .request(
                    "POST",
                    &format!(
                        "{implicit}&rewriteToken={}",
                        started["rewriteToken"].as_str().unwrap()
                    ),
                    READER,
                    br#"{"storageClass":"NEARLINE"}"#
                )
                .status,
            412
        );
        let continuation = format!(
            "{rewrite}&rewriteToken={}",
            first["rewriteToken"].as_str().unwrap()
        );
        let completed = server
            .request(
                "POST",
                &continuation,
                READER,
                br#"{"storageClass":"NEARLINE"}"#,
            )
            .json();
        assert_eq!(completed["done"], true);
        assert_eq!(
            server
                .request(
                    "POST",
                    &continuation,
                    READER,
                    br#"{"storageClass":"NEARLINE"}"#
                )
                .json(),
            completed
        );
        assert_eq!(
            server
                .request(
                    "GET",
                    &format!("{BUCKET}/o/rewritten?alt=media"),
                    ADMIN,
                    b""
                )
                .body,
            payload
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
        assert_eq!(
            server
                .request(
                    "POST",
                    &continuation,
                    READER,
                    br#"{"storageClass":"NEARLINE"}"#
                )
                .status,
            403
        );
        let live = server.request("GET", OBJECT, ADMIN, b"").json();
        server.grant(
            "user-reader@example.test",
            "READER",
            live["generation"].as_str().unwrap(),
        );
        server
            .request("POST", &format!("{OBJECT}/moveTo/o/moved"), READER, b"{}")
            .json();
        assert_eq!(
            server
                .request("GET", &format!("{BUCKET}/o/moved?alt=media"), ADMIN, b"")
                .body,
            b"different private live"
        );
    }
}

#[test]
fn legacy_copy_completion_requires_iam_when_original_source_proof_is_absent() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let mut server = Server::start(&data, &inputs, "disk");
    server.bucket();
    writer(&server);
    let source = server.put(b"original");
    server.grant(
        "user-reader@example.test",
        "READER",
        source["generation"].as_str().unwrap(),
    );
    let copy = format!("{OBJECT}/copyTo/b/test-bucket/o/copied");
    let headers = format!("{READER}X-Goog-Gcs-Idempotency-Token: legacy-source-copy\r\n");
    let result = server.request("POST", &copy, &headers, b"{}").json();
    server.stop();
    let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE upload_replays SET completion=json_remove(completion,'$.source')",
                []
            )
            .unwrap(),
        1
    );
    drop(connection);
    let server = Server::start(&data, &inputs, "disk");
    assert_eq!(server.request("POST", &copy, &headers, b"{}").status, 403);
    server.admin("PUT", &format!("{BUCKET}/iam"), json!({"bindings":[{"role":"roles/storage.objectAdmin","members":["user:reader@example.test"]}]})).json();
    assert_eq!(
        server.request("POST", &copy, &headers, b"{}").json(),
        result
    );
}
