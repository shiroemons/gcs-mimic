use super::*;
use serde_json::json;
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
    // A wait names the first files the set would delete and the bucket that went whole, and its
    // remedy says first that the bucket's directory is missing.
    let waited = server.cli("wait", &["--timeout", "10s"], false);
    assert_eq!(waited["reason"], "DeletionApproval", "{waited}");
    assert_eq!(
        waited["deletions"],
        json!({
            "paths":(0..10).map(|index| format!("test-bucket/{index:03}")).collect::<Vec<_>>(),
            "omitted":90,"removed_buckets":["test-bucket"],"removed_buckets_omitted":0,
        }),
        "{waited}"
    );
    let remedy = waited["remedy"].as_str().unwrap();
    assert!(
        remedy.starts_with(
            "The directory of bucket test-bucket is missing, as it is when a mount did not come up"
        ),
        "{remedy}"
    );
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
    // Listing the set the status names deletes nothing, so a bucket update leaves its pages
    // readable, with or without the revision; the decision on it inventories the tree again,
    // which replaces the set, and is refused.
    let rest = server.cli("deletions", &["--revision", old, "--cursor", cursor], true);
    assert_eq!(rest["revision"], old);
    assert_eq!(rest["items"].as_array().unwrap().len(), 63);
    assert_eq!(server.cli("deletions", &[], true)["revision"], old);
    assert_eq!(
        server.cli("approve", &["--deletion-set", old], false)["error"]["code"],
        409
    );
    let replaced = server.cli("status", &[], true);
    let new = replaced["pending_deletion_revision"].as_str().unwrap();
    assert_ne!(new, old);
    let gone = server.cli("deletions", &["--revision", old], false);
    assert_eq!(gone["error"]["code"], 409, "{gone}");
    let message = gone["error"]["message"].as_str().unwrap();
    for part in [
        format!("Deletion set {new} waits instead, and 100 of the 100 files"),
        format!("list it: gcs-mimic sync status --view deletions --revision {new}"),
    ] {
        assert!(message.contains(&part), "{message}");
    }
    assert!(!message.contains("gcs-mimic sync approve"), "{message}");
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
    let approved = server.cli_run("approve", &["--deletion-set", revision]);
    assert!(approved.success, "{:?}", approved.stderr);
    assert!(approved.answer["operation_id"].is_string());
    assert_eq!(approved.answer["decision"], "approve");
    assert_eq!(
        approved.diagnostics("sync-approve"),
        [format!(
            "The server deletes 100 objects of the approved deletion set; their buckets stay, and synchronization cannot bring the objects back; gcs-mimic sync wait --endpoint {} answers once the deletions are done",
            server.endpoint
        )]
    );
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
/// makes a decision on that set fail with 409, which counts what the set that waits now holds.
/// A refused approval names no approval to run, only the view to check that set in; a refused
/// rejection names the rejection of that set, never an approval. That rejection is accepted,
/// and writes the file back beside the new object.
/// A script that finds a set waiting can run `sync reject && sync wait` without copying a
/// revision: the rejection reads the set the status reports as waiting, names it with its files
/// and writes them back, and the wait answers once they are. An approval without a revision is
/// refused before anything is sent, naming the set and the view that lists it, and a rejection
/// with no set waiting fails.
#[test]
fn live_cli_rejects_the_waiting_set_without_a_revision() {
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
    let revision = status["pending_deletion_revision"].as_str().unwrap();
    let approve = server.cli_run("approve", &[]);
    assert!(!approve.success);
    assert_eq!(
        approve.diagnostics("sync"),
        [format!(
            "sync approve requires --deletion-set REVISION from an inspected deletion set. Deletion set {revision} waits for a decision, with 2 files; list it before you approve it: gcs-mimic sync status --view deletions --revision {revision} --endpoint {}",
            server.endpoint
        )]
    );
    assert_eq!(server.cli("status", &[], true)["state"], "DeletionApproval");
    let rejected = server.cli_run("reject", &[]);
    assert!(rejected.success, "{:?}", rejected.stderr);
    assert_eq!(rejected.answer["operation_id"], revision);
    assert_eq!(
        rejected.diagnostics("sync-reject"),
        [
            format!("Rejecting deletion set {revision} (2 files)"),
            format!(
                "The server writes 2 files of the rejected deletion set back from the objects; gcs-mimic sync wait --endpoint {} answers once the write-back is done",
                server.endpoint
            ),
        ]
    );
    assert_eq!(
        server.cli("wait", &["--timeout", "10s"], true)["completed"],
        true
    );
    for name in ["first", "second"] {
        assert_eq!(fs::read(bucket.join(name)).unwrap(), b"body", "{name}");
    }
    let again = server.cli_run("reject", &[]);
    assert!(!again.success);
    assert_eq!(
        again.diagnostics("sync"),
        [format!(
            "No deletion set waits for a decision, so there is nothing to reject; the status says what synchronization waits for: gcs-mimic sync status --endpoint {}",
            server.endpoint
        )]
    );
}

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
    let refused_approval = server.cli("approve", &["--deletion-set", &old], false);
    assert_eq!(refused_approval["error"]["code"], 409, "{refused_approval}");
    let refused = server.cli("reject", &["--deletion-set", &old], false);
    assert_eq!(refused["error"]["code"], 409, "{refused}");
    let status = server.cli("status", &[], true);
    assert_eq!(status["pending_deletion_count"], 1, "{status}");
    let new = status["pending_deletion_revision"].as_str().unwrap();
    assert_ne!(new, old);
    let tracked = &status["deletion_set"]["baseline_count"];
    let files = if tracked == 1 { "file" } else { "files" };
    let held = format!(
        "Deletion set {new} waits instead, and 1 of the {tracked} {files} that synchronization tracked is missing"
    );
    let view = format!("gcs-mimic sync status --view deletions --revision {new}");
    let message = refused_approval["error"]["message"].as_str().unwrap();
    for part in [
        &format!("Deletion set {old} was not approved"),
        &held,
        &view,
    ] {
        assert!(message.contains(part.as_str()), "{message}");
    }
    assert!(!message.contains("gcs-mimic sync approve"), "{message}");
    assert!(!message.contains("gcs-mimic sync reject"), "{message}");
    let message = refused["error"]["message"].as_str().unwrap();
    for part in [
        format!("Deletion set {old} was not rejected"),
        held,
        view,
        format!("gcs-mimic sync reject --deletion-set {new}"),
    ] {
        assert!(message.contains(&part), "{message}");
    }
    assert!(!message.contains("gcs-mimic sync approve"), "{message}");
    let accepted = server.cli_run("reject", &["--deletion-set", new]);
    assert!(accepted.success, "{:?}", accepted.stderr);
    assert_eq!(accepted.answer["operation_id"], new);
    assert_eq!(accepted.answer["decision"], "reject");
    assert_eq!(
        accepted.diagnostics("sync-reject"),
        [format!(
            "The server writes 1 file of the rejected deletion set back from the objects; gcs-mimic sync wait --endpoint {} answers once the write-back is done",
            server.endpoint
        )]
    );
    assert_eq!(
        server.cli("wait", &["--timeout", "10s"], true)["completed"],
        true
    );
    assert_eq!(fs::read(bucket.join("second")).unwrap(), b"body");
    assert_eq!(fs::read(bucket.join("first")).unwrap(), b"written");
    let replay = server.cli("reject", &["--deletion-set", new], false);
    assert_eq!(replay["error"]["code"], 409, "{replay}");
}
