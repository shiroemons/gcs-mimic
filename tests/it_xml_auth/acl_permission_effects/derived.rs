use super::*;

fn writer(server: &Server) {
    server.admin("PUT", &format!("{BUCKET}/iam"), json!({"bindings":[{"role":"roles/storage.legacyBucketWriter","members":["user:reader@example.test"]}]})).json_ok();
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
        let completed = server.request("POST", &copy, &headers, b"{}").json_ok();
        server.put(b"new private source");
        server.request("GET", OBJECT, READER, b"").expect(403);
        assert_eq!(
            server.request("POST", &copy, &headers, b"{}").json_ok(),
            completed
        );
        server
            .admin(
                "PATCH",
                BUCKET,
                json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
            )
            .json_ok();
        server.request("POST", &copy, &headers, b"{}").expect(403);
        server.admin("PATCH", BUCKET, json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false},"publicAccessPrevention":"enforced"}})).json_ok();
        assert_eq!(
            server.request("POST", &copy, &headers, b"{}").json_ok(),
            completed
        );
        server
            .request(
                "POST",
                &copy,
                &headers,
                br#"{"metadata":{"changed":"yes"}}"#,
            )
            .expect(400);
        if backend == "disk" {
            server.stop();
            server = Server::start(&data, &inputs, backend);
            assert_eq!(
                server.request("POST", &copy, &headers, b"{}").json_ok(),
                completed
            );
        }
        server
            .request(
                "DELETE",
                &format!("{OBJECT}/acl/user-reader%40example.test?generation={generation}"),
                ADMIN,
                b"",
            )
            .expect(204);
        server.request("POST", &copy, &headers, b"{}").expect(403);
        server.grant("user-reader@example.test", "READER", generation);
        assert_eq!(
            server.request("POST", &copy, &headers, b"{}").json_ok(),
            completed
        );
        server
            .request(
                "DELETE",
                &format!("{OBJECT}?generation={generation}"),
                ADMIN,
                b"",
            )
            .expect(204);
        server.request("POST", &copy, &headers, b"{}").expect(403);
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
            .json_ok();
        server
            .request(
                "POST",
                &format!("{BUCKET}/o/composed/compose"),
                READER,
                json!({"sourceObjects":[{"name":"value","generation":generation}]})
                    .to_string()
                    .as_bytes(),
            )
            .json_ok();
        let rewrite = format!(
            "{OBJECT}/rewriteTo/b/test-bucket/o/rewritten?sourceGeneration={generation}&maxBytesRewrittenPerCall=1048576"
        );
        let first = server
            .request("POST", &rewrite, READER, br#"{"storageClass":"NEARLINE"}"#)
            .json_ok();
        assert_eq!(first["done"], false);
        let implicit =
            format!("{OBJECT}/rewriteTo/b/test-bucket/o/implicit?maxBytesRewrittenPerCall=1048576");
        let started = server
            .request("POST", &implicit, READER, br#"{"storageClass":"NEARLINE"}"#)
            .json_ok();
        server.put(b"different private live");
        server
            .request(
                "POST",
                &format!(
                    "{implicit}&rewriteToken={}",
                    started["rewriteToken"].as_str().unwrap()
                ),
                READER,
                br#"{"storageClass":"NEARLINE"}"#,
            )
            .expect(412);
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
            .json_ok();
        assert_eq!(completed["done"], true);
        assert_eq!(
            server
                .request(
                    "POST",
                    &continuation,
                    READER,
                    br#"{"storageClass":"NEARLINE"}"#
                )
                .json_ok(),
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
        server
            .request(
                "DELETE",
                &format!("{OBJECT}/acl/user-reader%40example.test?generation={generation}"),
                ADMIN,
                b"",
            )
            .expect(204);
        server
            .request(
                "POST",
                &continuation,
                READER,
                br#"{"storageClass":"NEARLINE"}"#,
            )
            .expect(403);
        let live = server.request("GET", OBJECT, ADMIN, b"").json_ok();
        server.grant(
            "user-reader@example.test",
            "READER",
            live["generation"].as_str().unwrap(),
        );
        server
            .request("POST", &format!("{OBJECT}/moveTo/o/moved"), READER, b"{}")
            .json_ok();
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
    let result = server.request("POST", &copy, &headers, b"{}").json_ok();
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
    server.request("POST", &copy, &headers, b"{}").expect(403);
    server.admin("PUT", &format!("{BUCKET}/iam"), json!({"bindings":[{"role":"roles/storage.objectAdmin","members":["user:reader@example.test"]}]})).json_ok();
    assert_eq!(
        server.request("POST", &copy, &headers, b"{}").json_ok(),
        result
    );
}

/// A copy, a rewrite and a compose of the test object to `{name}-copy`, `{name}-rewrite` and
/// `{name}-compose` with `query`: each one's path, body and destination name.
fn derived_writes(name: &str, query: &str) -> [(String, String, String); 3] {
    let sources = json!({"sourceObjects":[{"name":"value"}]}).to_string();
    [
        ("copyTo", String::new()),
        ("rewriteTo", String::new()),
        ("compose", sources),
    ]
    .map(|(method, body)| {
        let destination = format!("{name}-{}", method.trim_end_matches("To"));
        let path = if method == "compose" {
            format!("{BUCKET}/o/{destination}/compose?{query}")
        } else {
            format!("{OBJECT}/{method}/b/test-bucket/o/{destination}?{query}")
        };
        (path, body, destination)
    })
}

/// `destinationPredefinedAcl` replaces the default object ACL on a copy, rewrite or compose as
/// `predefinedAcl` does on an upload, under the same public access prevention check. Cloud
/// Storage applied it on all three (recorded 2026-11-13, case obj-unsupported-derived).
#[test]
fn derived_writes_apply_destination_predefined_acl() {
    use super::predefined_upload::{ADMIN_OWNER, absent, pairs, stored};
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        // A stored default object ACL shows that the canned ACL replaces it.
        server
            .admin(
                "POST",
                &format!("{BUCKET}/defaultObjectAcl"),
                json!({"entity":"user-reader@example.test","role":"READER"}),
            )
            .json_ok();
        server.put(b"source bytes");
        for (value, expected) in [
            ("publicRead", vec![ADMIN_OWNER, ("allUsers", "READER")]),
            (
                "bucketOwnerRead",
                vec![ADMIN_OWNER, ("project-owners-902648055030", "READER")],
            ),
        ] {
            let query = format!("destinationPredefinedAcl={value}");
            for (path, body, destination) in derived_writes(value, &query) {
                server
                    .request("POST", &path, ADMIN, body.as_bytes())
                    .json_ok();
                assert_eq!(
                    stored(&server, &destination),
                    pairs(&expected),
                    "{backend} {destination}"
                );
            }
        }
        // An anonymous caller may write and read every object here, yet cannot name the ACL
        // of an object the project owners will own, as on an upload.
        let iam = format!("{BUCKET}/iam");
        server
            .admin(
                "PUT",
                &iam,
                json!({"bindings":[{"role":"roles/storage.objectAdmin","members":["allUsers"]}]}),
            )
            .json_ok();
        for (path, body, destination) in
            derived_writes("anonymous", "destinationPredefinedAcl=private")
        {
            let refused = server.request("POST", &path, "", body.as_bytes());
            refused.json_reason(400, "invalidArgument");
            assert_eq!(
                serde_json::from_slice::<Value>(&refused.body).unwrap()["error"]["message"],
                "Anonymous callers cannot specify destinationPredefinedAcl; authenticate or omit destinationPredefinedAcl",
                "{path}"
            );
            absent(&server, &destination);
        }
        server.admin("PUT", &iam, json!({"bindings":[]})).json_ok();
        server
            .admin(
                "PATCH",
                BUCKET,
                json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}}),
            )
            .json_ok();
        for (path, body, destination) in
            derived_writes("public", "destinationPredefinedAcl=publicRead")
        {
            server
                .request("POST", &path, ADMIN, body.as_bytes())
                .json_reason(412, "conditionNotMet");
            absent(&server, &destination);
        }
    }
}
