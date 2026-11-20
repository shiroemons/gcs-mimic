//! Findings whose cause can go without a journal change of their own object: each ingestion
//! observes those objects again, exports the ones whose cause went, and only reads for the rest.
#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::baseline::{COMPLETE, ingest};
use super::*;
use crate::storage::sync_state::findings::FindingKind;
use crate::sync::control::WaitReason;

/// The findings of `root` by object name, in the order they were recorded.
async fn findings(root: &RootLease) -> Vec<(String, FindingKind)> {
    root.findings_page(0, 256)
        .await
        .unwrap()
        .into_iter()
        .map(|finding| (finding.name, finding.reason))
        .collect()
}

/// Records the findings of the objects published after `cursor` by replaying them, as the
/// running mirror does, with whatever the tree holds in their way now.
async fn replay_from(catalog: &Catalog, root: &RootLease, cursor: JournalFence) {
    wait_from(catalog, root, cursor).await;
}

/// A name collision between two API objects holds both out of the tree, and the API deleting
/// one of them settles the other, whose own journal change was replayed long ago: the next pass
/// observes the remaining object again and exports it, and no finding is left.
#[tokio::test]
async fn an_api_deletion_settles_the_collision_of_the_object_left() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    catalog.enable_change_journal().await.unwrap();
    publish_named(&catalog, &bucket, "a", b"file", Origin::Api).await;
    let child = publish_named(&catalog, &bucket, "a/b", b"child", Origin::Api).await;
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let collision = FindingKind::Unmapped(Reason::Collision);
    assert_eq!(
        findings(&root).await,
        [("a".into(), collision), ("a/b".into(), collision)]
    );
    let cursor = catalog.current_fence().await.unwrap();
    catalog
        .admin(Origin::Api)
        .delete_object(
            &bucket,
            &ObjectName::new("a/b").unwrap(),
            GenerationCondition::Is(Generation::new(child.generation).unwrap()),
        )
        .await
        .unwrap();
    let result = wait_from(&catalog, &root, cursor).await;
    assert_eq!(
        (
            result.reason,
            result.findings.map(|counted| counted.unmapped)
        ),
        (WaitReason::Ready, Some(0))
    );
    assert!(findings(&root).await.is_empty());
    assert_eq!(fs::read(tree.0.join("test-bucket/a")).unwrap(), b"file");
    drop(root);
    catalog.close().await.unwrap();
}

/// Replays the journal from `cursor` on its own, with no ingestion before it. The write-back
/// handle goes with the call, since the catalog closes only once nothing holds it.
async fn replay_alone(catalog: &Catalog, root: &RootLease, cursor: JournalFence) {
    let fence = catalog.current_fence().await.unwrap();
    let ChangeRead::Page(page) = catalog
        .read_changes(cursor, Some(fence), 256)
        .await
        .unwrap()
    else {
        panic!("the journal kept every change since the cursor");
    };
    let writeback = root.default_writeback().await.unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    replay_page(
        catalog,
        root,
        &writeback,
        page,
        &stopping,
        &mut SkippedExports::default(),
    )
    .await
    .unwrap();
}

/// The body of `name` that the API holds now.
async fn api_body(catalog: &Catalog, bucket: &BucketName, name: &str) -> Vec<u8> {
    let (_, reader) = catalog
        .admin(Origin::Api)
        .open_object(bucket, &ObjectName::new(name).unwrap(), None)
        .await
        .unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader.into_file().unwrap(), &mut bytes).unwrap();
    bytes
}

/// Gives the tracked object `a` a collision at the generation the tree tracks: `a/b` arrives
/// through the API, and a metadata change of `a` is replayed while `a/b` holds it out. Answers
/// the generation of `a/b`.
async fn collide_with_a_tracked_file(
    catalog: &Catalog,
    bucket: &BucketName,
    root: &RootLease,
) -> Generation {
    publish_named(catalog, bucket, "a", b"file", Origin::Api).await;
    assert!(reconcile(catalog, root).await.unwrap().baseline_complete);
    let cursor = catalog.current_fence().await.unwrap();
    let child = publish_named(catalog, bucket, "a/b", b"child", Origin::Api).await;
    catalog
        .update_object_metadata(
            crate::storage::test_fixtures::live(bucket.as_str(), "a"),
            Default::default(),
            crate::object::ObjectMetadataUpdate {
                content_type: crate::object::Patch::Set("text/plain".into()),
                ..Default::default()
            },
            |_| Ok(()),
            crate::storage::test_fixtures::PERMISSIVE,
        )
        .await
        .unwrap();
    replay_from(catalog, root, cursor).await;
    let collision = FindingKind::Unmapped(Reason::Collision);
    let mut found = findings(root).await;
    found.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(found, [("a".into(), collision), ("a/b".into(), collision)]);
    Generation::new(child.generation).unwrap()
}

/// Another look at a finding whose object the tree tracks at its current generation finds the
/// file edited meanwhile, and leaves the edit to ingestion, which uploads it, rather than
/// exporting the object over it.
#[tokio::test]
async fn another_look_at_a_finding_leaves_an_edit_of_its_tracked_file_to_ingestion() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    catalog.enable_change_journal().await.unwrap();
    let child = collide_with_a_tracked_file(&catalog, &bucket, &root).await;
    let file = tree.0.join("test-bucket/a");
    fs::write(&file, b"edited file").unwrap();
    let cursor = catalog.current_fence().await.unwrap();
    catalog
        .admin(Origin::Api)
        .delete_object(
            &bucket,
            &ObjectName::new("a/b").unwrap(),
            GenerationCondition::Is(child),
        )
        .await
        .unwrap();
    let result = wait_from(&catalog, &root, cursor).await;
    assert_eq!(
        (
            result.reason,
            result.findings.map(|counted| counted.unmapped)
        ),
        (WaitReason::Ready, Some(0))
    );
    assert!(findings(&root).await.is_empty());
    assert_eq!(fs::read(&file).unwrap(), b"edited file");
    assert_eq!(api_body(&catalog, &bucket, "a").await, b"edited file");
    drop(root);
    catalog.close().await.unwrap();
}

/// The replay of an API deletion observes the objects whose names collided with the deleted
/// one again, so the object left is exported, and its finding goes, before the next ingestion.
#[tokio::test]
async fn the_replay_of_a_nested_deletion_settles_the_collision_it_caused() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    catalog.enable_change_journal().await.unwrap();
    let child = collide_with_a_tracked_file(&catalog, &bucket, &root).await;
    let cursor = catalog.current_fence().await.unwrap();
    catalog
        .admin(Origin::Api)
        .delete_object(
            &bucket,
            &ObjectName::new("a/b").unwrap(),
            GenerationCondition::Is(child),
        )
        .await
        .unwrap();
    replay_alone(&catalog, &root, cursor).await;
    assert!(findings(&root).await.is_empty());
    assert_eq!(fs::read(tree.0.join("test-bucket/a")).unwrap(), b"file");
    drop(root);
    catalog.close().await.unwrap();
}

/// What keeps an object out of the tree may be an entry in its way, which can go without any
/// API change: a symlink where a directory of the object's path belongs, or a file that keeps
/// a directory in place of the object's name. Once the entry is removed, the rescan that follows
/// exports the object without a restart, and its finding goes.
#[tokio::test]
async fn removing_the_entry_in_the_way_lets_a_rescan_export_the_object() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    catalog.enable_change_journal().await.unwrap();
    let directory = tree.0.join("test-bucket");
    fs::create_dir(&directory).unwrap();
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let cursor = catalog.current_fence().await.unwrap();
    let outside = TestDir::new();
    std::os::unix::fs::symlink(&outside.0, directory.join("link")).unwrap();
    fs::create_dir(directory.join("occupied")).unwrap();
    fs::write(directory.join("occupied/kept.swp"), b"editor").unwrap();
    publish_named(&catalog, &bucket, "link/object", b"linked", Origin::Api).await;
    publish_named(&catalog, &bucket, "occupied", b"occupied", Origin::Api).await;
    replay_from(&catalog, &root, cursor).await;
    assert_eq!(
        findings(&root).await,
        [
            (
                "link/object".into(),
                FindingKind::Unmapped(Reason::UnsafeEntry)
            ),
            (
                "occupied".into(),
                FindingKind::Unmapped(Reason::DirectoryOccupied)
            ),
        ]
    );

    fs::remove_file(directory.join("link")).unwrap();
    fs::remove_file(directory.join("occupied/kept.swp")).unwrap();
    let mut baseline = COMPLETE;
    ingest(&catalog, &root, &Lifecycle::default(), &mut baseline).await;
    assert!(findings(&root).await.is_empty());
    assert_eq!(fs::read(directory.join("link/object")).unwrap(), b"linked");
    assert_eq!(fs::read(directory.join("occupied")).unwrap(), b"occupied");
    assert!(fs::read_dir(&outside.0).unwrap().next().is_none());
    drop(root);
    catalog.close().await.unwrap();
}

/// A regular file directly in the root under a bucket's name keeps every object of that bucket
/// out of the tree as a collision, and the bucket's directory with them. Once the file is moved
/// away, the next ingestion exports the bucket without a restart and leaves the file as it was.
#[tokio::test]
async fn moving_a_root_file_named_like_a_bucket_away_lets_an_ingestion_export_the_bucket() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    catalog.enable_change_journal().await.unwrap();
    let file = tree.0.join("test-bucket");
    fs::write(&file, b"mine").unwrap();
    for name in ["x", "y/z"] {
        publish_named(&catalog, &bucket, name, name.as_bytes(), Origin::Api).await;
    }
    reconcile(&catalog, &root).await.unwrap();
    let collision = FindingKind::Unmapped(Reason::Collision);
    assert_eq!(
        findings(&root).await,
        [("x".into(), collision), ("y/z".into(), collision)]
    );
    assert_eq!(fs::read(&file).unwrap(), b"mine");

    let aside = TestDir::new();
    fs::rename(&file, aside.0.join("test-bucket")).unwrap();
    let mut baseline = COMPLETE;
    ingest(&catalog, &root, &Lifecycle::default(), &mut baseline).await;
    assert!(findings(&root).await.is_empty());
    assert_eq!(fs::read(file.join("x")).unwrap(), b"x");
    assert_eq!(fs::read(file.join("y/z")).unwrap(), b"y/z");
    assert_eq!(fs::read(aside.0.join("test-bucket")).unwrap(), b"mine");
    drop(root);
    catalog.close().await.unwrap();
}

/// A finding the pass cannot observe again, here behind a directory it may not open, keeps its
/// row for a later pass and stops neither the pass nor the other findings: the object whose
/// entry in the way went is still exported.
#[tokio::test]
async fn a_finding_that_cannot_be_observed_again_holds_back_only_itself() {
    use std::os::unix::fs::PermissionsExt;
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    catalog.enable_change_journal().await.unwrap();
    let directory = tree.0.join("test-bucket");
    fs::create_dir(&directory).unwrap();
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let cursor = catalog.current_fence().await.unwrap();
    for name in ["held", "link"] {
        std::os::unix::fs::symlink("nowhere", directory.join(name)).unwrap();
        publish_named(
            &catalog,
            &bucket,
            &format!("{name}/object"),
            name.as_bytes(),
            Origin::Api,
        )
        .await;
    }
    replay_from(&catalog, &root, cursor).await;
    let unsafe_entry = FindingKind::Unmapped(Reason::UnsafeEntry);
    assert_eq!(
        findings(&root).await,
        [
            ("held/object".into(), unsafe_entry),
            ("link/object".into(), unsafe_entry)
        ]
    );

    for name in ["held", "link"] {
        fs::remove_file(directory.join(name)).unwrap();
    }
    let held = directory.join("held");
    fs::create_dir(&held).unwrap();
    fs::set_permissions(&held, fs::Permissions::from_mode(0o000)).unwrap();
    let mut baseline = COMPLETE;
    let summary = ingest(&catalog, &root, &Lifecycle::default(), &mut baseline).await;
    fs::set_permissions(&held, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(summary.unreadable(), 1);
    assert_eq!(
        findings(&root).await,
        [("held/object".into(), unsafe_entry)]
    );
    assert_eq!(fs::read(directory.join("link/object")).unwrap(), b"link");
    assert!(!held.join("object").exists());
    drop(root);
    catalog.close().await.unwrap();
}
