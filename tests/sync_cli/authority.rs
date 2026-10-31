use super::*;

#[tokio::test]
async fn live_cli_requires_retained_original_owner_after_bucket_recreation_and_restart() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let inputs = TestDir::new();
    api_fixture(&data, &tree, &["retained"]).await;
    let registry = inputs.0.join("identities.json");
    fs::write(&registry, br#"{"identities":[{"id":"all","tokens":["all"],"project_roles":{"local":["roles/storage.admin"],"original":["roles/storage.admin"],"replacement":["roles/storage.admin"]}},{"id":"current","tokens":["current"],"project_roles":{"local":["roles/storage.admin"],"replacement":["roles/storage.admin"]}}]}"#).unwrap();
    let options = [
        "--auth-mode",
        "strict",
        "--identity-file",
        registry.to_str().unwrap(),
    ];
    let mut server = Server::start_with(&data, Some(&tree), &options);
    scan_with_conflicts(&server, Some("all"));
    assert_eq!(
        server.cli_with_token("conflicts", &[], Some("current"), false)["error"]["code"],
        403
    );
    assert_eq!(
        server
            .request(
                "DELETE",
                "/storage/v1/b/test-bucket/o/retained",
                Some("all"),
                b""
            )
            .status,
        204
    );
    // Deletion export is asynchronous, and an explicit same-name recreation ingests whatever
    // the directory still holds; let the mirrored file go before deleting its bucket.
    scan_with_conflicts(&server, Some("all"));
    assert!(!tree.0.join("test-bucket/retained").exists());
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket", Some("all"), b"")
            .status,
        204
    );
    server
        .request(
            "POST",
            "/storage/v1/b?project=replacement",
            Some("all"),
            br#"{"name":"test-bucket"}"#,
        )
        .json();
    scan_with_conflicts(&server, Some("all"));
    server.stop();
    let server = Server::start_with(&data, Some(&tree), &options);
    for command in ["status", "conflicts", "wait", "rescan"] {
        assert_eq!(
            server.cli_with_token(command, &[], Some("current"), false)["error"]["code"],
            403
        );
    }
    let page = server.cli_with_token("conflicts", &[], Some("all"), true);
    let item = &page["items"][0];
    assert_eq!(item["owner_project"], "original");
    let restore = conflict_options(item, "restore", "0", "old-incarnation");
    assert_eq!(
        server.cli_with_token("resolve", &restore, Some("current"), false)["error"]["code"],
        403
    );
    assert_eq!(
        server.cli_with_token("resolve", &restore, Some("all"), false)["error"]["code"],
        409
    );
    let discard = conflict_options(item, "discard", "0", "discard-original");
    assert_eq!(
        server.cli_with_token("resolve", &discard, Some("current"), false)["error"]["code"],
        403
    );
    assert_eq!(
        server.cli_with_token("resolve", &discard, Some("all"), true)["completed"],
        true
    );
    settle(&server, Some("all"));
    assert!(
        server.cli_with_token("conflicts", &[], Some("current"), true)["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        server.cli_with_token("resolve", &discard, Some("current"), false)["error"]["code"],
        403
    );
    assert_eq!(
        server.cli_with_token("resolve", &discard, Some("all"), true)["completed"],
        true
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/retained",
                Some("all"),
                b""
            )
            .status,
        404
    );
}
