use super::*;

#[tokio::test]
async fn live_cli_restores_and_discards_retained_conflicts_once_across_restart() {
    let data = TestDir::new();
    let tree = TestDir::new();
    api_fixture(&data, &tree, &["restore", "discard"]).await;
    let mut server = Server::start(&data, Some(&tree));
    scan_with_conflicts(&server, None);
    let page = server.cli("conflicts", &["--limit", "1"], true);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let next = server.cli(
        "conflicts",
        &[
            "--revision",
            page["revision"].as_str().unwrap(),
            "--cursor",
            page["next_cursor"].as_str().unwrap(),
            "--limit",
            "1",
        ],
        true,
    );
    assert!(next["next_cursor"].is_null());
    let mut items = [page["items"][0].clone(), next["items"][0].clone()];
    items.sort_by(|a, b| a["object"].as_str().cmp(&b["object"].as_str()));
    let discarded = items[0].clone();
    let restored = items[1].clone();
    assert_eq!(discarded["object"], "discard");
    assert_eq!(discarded["size"], 0);
    assert_eq!(
        fs::read(
            data.0
                .join("sync-conflicts")
                .join(discarded["conflict_id"].as_str().unwrap())
        )
        .unwrap(),
        b""
    );
    assert_eq!(restored["object"], "restore");
    let generation = server
        .request("GET", "/storage/v1/b/test-bucket/o/restore", None, b"")
        .json()["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let wrong = server.cli(
        "resolve",
        &conflict_options(&restored, "restore", "0", "stale-generation"),
        false,
    );
    assert_eq!(wrong["error"]["code"], 409);
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/restore?alt=media",
                None,
                b""
            )
            .body,
        b"API winner"
    );
    let options = conflict_options(&restored, "restore", &generation, "restore-once");
    let mut stale_revision = options.clone();
    stale_revision[3] = "00000000000000000000000000000000";
    assert_eq!(
        server.cli("resolve", &stale_revision, false)["error"]["code"],
        409
    );
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/restore",
            None,
            br#"{"temporaryHold":true}"#,
        )
        .json();
    let protected = server
        .request("GET", "/storage/v1/b/test-bucket/o/restore", None, b"")
        .json();
    assert_eq!(
        server.cli(
            "resolve",
            &conflict_options(&restored, "restore", &generation, "held-restore"),
            false
        )["error"]["code"],
        409
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/restore", None, b"")
            .json(),
        protected
    );
    assert_eq!(
        fs::read(
            data.0
                .join("sync-conflicts")
                .join(restored["conflict_id"].as_str().unwrap())
        )
        .unwrap(),
        b"external\0\xff"
    );
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/restore",
            None,
            br#"{"temporaryHold":false}"#,
        )
        .json();
    let outcome = server.cli("resolve", &options, true);
    assert_eq!(outcome["completed"], true);
    assert_eq!(server.cli("resolve", &options, true), outcome);
    assert_eq!(
        server.cli(
            "resolve",
            &conflict_options(&restored, "discard", &generation, "restore-once"),
            false
        )["error"]["errors"][0]["reason"],
        "operation_id_reused"
    );
    let discarded_generation = server
        .request("GET", "/storage/v1/b/test-bucket/o/discard", None, b"")
        .json()["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let discard_options =
        conflict_options(&discarded, "discard", &discarded_generation, "discard-once");
    let discard = server.cli("resolve", &discard_options, true);
    assert_eq!(discard["completed"], true);
    settle(&server, None);
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/restore?alt=media",
                None,
                b""
            )
            .body,
        b"external\0\xff"
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/discard?alt=media",
                None,
                b""
            )
            .body,
        b"API winner"
    );
    assert_eq!(
        fs::read(tree.0.join("test-bucket/restore")).unwrap(),
        b"external\0\xff"
    );
    assert!(
        server.cli("conflicts", &[], true)["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    server.stop();
    let server = Server::start(&data, Some(&tree));
    settle(&server, None);
    assert_eq!(server.cli("resolve", &options, true), outcome);
    assert_eq!(server.cli("resolve", &discard_options, true), discard);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/restore", None, b"")
            .json()["generation"],
        outcome["result_generation"].to_string()
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/restore?alt=media",
                None,
                b""
            )
            .body,
        b"external\0\xff"
    );
    assert_eq!(
        fs::read(tree.0.join("test-bucket/discard")).unwrap(),
        b"API winner"
    );
}
