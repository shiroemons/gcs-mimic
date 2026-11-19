use super::*;
use std::collections::BTreeSet;

#[test]
fn live_cli_approves_every_frozen_deletion_page_and_rejects_replay_after_restart() {
    let data = TestDir::new();
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for index in 0..100 {
        fs::write(tree.0.join(format!("test-bucket/{index:03}")), b"body").unwrap();
    }
    let mut server = Server::start(&data, Some(&tree));
    server.cli("rescan", &["--timeout", "60s"], true);
    assert_eq!(
        server.cli("wait", &["--timeout", "10s"], true)["completed"],
        true
    );
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    server.cli("rescan", &[], true);
    let status = server.cli("status", &[], true);
    assert_eq!(status["state"], "DeletionApproval", "{status}");
    assert_eq!(status["pending_deletion_count"], 100);
    let old = status["pending_deletion_revision"].as_str().unwrap();
    let first = server.cli("deletions", &["--revision", old, "--limit", "37"], true);
    let cursor = first["next_cursor"].as_str().unwrap();
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            None,
            br#"{"labels":{"k":"v"}}"#,
        )
        .json_ok();
    assert_eq!(
        server.cli("deletions", &["--revision", old, "--cursor", cursor], false)["error"]["code"],
        409
    );
    assert_eq!(
        server.cli("approve", &["--deletion-set", old], false)["error"]["code"],
        409
    );
    server.cli("rescan", &[], true);
    let status = server.cli("status", &[], true);
    let revision = status["pending_deletion_revision"].as_str().unwrap();
    let mut cursor = None::<String>;
    let mut names = BTreeSet::new();
    let mut pages = 0;
    loop {
        let mut options = vec!["--revision", revision, "--limit", "37"];
        if let Some(cursor) = &cursor {
            options.extend(["--cursor", cursor]);
        }
        let page = server.cli("deletions", &options, true);
        assert_eq!(page["candidate_count"], 100);
        assert_eq!(page["baseline_count"], 100);
        assert_eq!(page["revision"], revision);
        assert_eq!(
            page["journal_fence"],
            status["deletion_set"]["journal_fence"]
        );
        assert_eq!(
            page["scan_revision"],
            status["deletion_set"]["scan_revision"]
        );
        for item in page["items"].as_array().unwrap() {
            assert!(item.get("path").is_none());
            assert!(item["generation"].as_i64().unwrap() > 0);
            assert!(names.insert(item["object"].as_str().unwrap().to_owned()));
        }
        pages += 1;
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(names, (0..100).map(|i| format!("{i:03}")).collect());
    assert_eq!(
        server.cli("approve", &["--deletion-set", &"0".repeat(32)], false)["error"]["code"],
        409
    );
    let approved = server.cli("approve", &["--deletion-set", revision], true);
    assert!(approved["operation_id"].is_string());
    assert_eq!(
        server.cli("wait", &["--timeout", "10s"], true)["completed"],
        true
    );
    assert_eq!(
        server.cli("approve", &["--deletion-set", revision], false)["error"]["code"],
        409
    );
    assert_eq!(server.cli("status", &[], true)["pending_deletion_count"], 0);
    for name in &names {
        server
            .request(
                "GET",
                &format!("/storage/v1/b/test-bucket/o/{name}"),
                None,
                b"",
            )
            .expect(404);
    }
    assert!(tree.0.join("test-bucket").is_dir());
    server.stop();
    let server = Server::start(&data, Some(&tree));
    settle(&server, None);
    assert_eq!(
        server.cli("approve", &["--deletion-set", revision], false)["error"]["code"],
        409
    );
    assert!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o", None, b"")
            .json_ok()["items"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
}

/// An API write to a member's object after the status named the set waiting for a decision
/// makes a decision on that set fail with 409, which names the set that replaced it and the
/// same decision on it, never the other one. That decision is accepted, and the rejection
/// writes the file back beside the new object.
#[test]
fn live_cli_refuses_a_decision_after_an_api_write_to_a_member_naming_the_new_set() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let bucket = tree.0.join("test-bucket");
    fs::create_dir(&bucket).unwrap();
    for name in ["first", "second"] {
        fs::write(bucket.join(name), b"body").unwrap();
    }
    let server = Server::start(&data, Some(&tree));
    server.cli("rescan", &["--timeout", "60s"], true);
    assert_eq!(
        server.cli("wait", &["--timeout", "10s"], true)["completed"],
        true
    );
    fs::remove_dir_all(&bucket).unwrap();
    server.cli("rescan", &[], true);
    let status = server.cli("status", &[], true);
    assert_eq!(status["state"], "DeletionApproval", "{status}");
    let old = status["pending_deletion_revision"]
        .as_str()
        .unwrap()
        .to_owned();
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=first",
            None,
            b"written",
        )
        .expect(200);
    let refused = server.cli("reject", &["--deletion-set", &old], false);
    assert_eq!(refused["error"]["code"], 409, "{refused}");
    let status = server.cli("status", &[], true);
    assert_eq!(status["pending_deletion_count"], 1, "{status}");
    let new = status["pending_deletion_revision"].as_str().unwrap();
    assert_ne!(new, old);
    let message = refused["error"]["message"].as_str().unwrap();
    for part in [
        format!("Deletion set {old} was not rejected"),
        format!("deletion set {new} replaced it"),
        format!("gcs-mimic sync reject --deletion-set {new}"),
    ] {
        assert!(message.contains(&part), "{message}");
    }
    assert!(!message.contains("gcs-mimic sync approve"), "{message}");
    let accepted = server.cli("reject", &["--deletion-set", new], true);
    assert_eq!(accepted["operation_id"], new);
    assert_eq!(
        server.cli("wait", &["--timeout", "10s"], true)["completed"],
        true
    );
    assert_eq!(fs::read(bucket.join("second")).unwrap(), b"body");
    assert_eq!(fs::read(bucket.join("first")).unwrap(), b"written");
    let replay = server.cli("reject", &["--deletion-set", new], false);
    assert_eq!(replay["error"]["code"], 409, "{replay}");
}
