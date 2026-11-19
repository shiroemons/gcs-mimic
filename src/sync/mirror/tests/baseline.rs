//! Which side of a pass holds back the baseline and deletion detection, and how single paths
//! that tell nothing about their file stay out of a deletion without stopping it.
#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::*;
use crate::storage::sync_state::observation::ObservationCursor;
use crate::sync::entries::{EntryProblem, UnrepresentableEntry};

/// A name the API accepts and no file system holds: one component longer than any name limit.
fn unexportable_name() -> String {
    "x".repeat(300)
}

async fn exists(catalog: &Catalog, bucket: &BucketName, name: &str) -> bool {
    match catalog
        .admin(Origin::Api)
        .get_object(bucket, &ObjectName::new(name).unwrap(), None)
        .await
    {
        Ok(_) => true,
        Err(AdminError::NotFound) => false,
        Err(error) => panic!("{error}"),
    }
}

async fn ingest(
    catalog: &Catalog,
    root: &RootLease,
    status: &MirrorStatusSender,
    baseline: &mut BaselineProgress,
) -> Reconciliation {
    let writeback = root.default_writeback().await.unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    ingest_external(
        LiveScope {
            catalog,
            root,
            config: &Config::default(),
            writeback: &writeback,
            stopping: &stopping,
            entries: status.entries(),
        },
        baseline,
        &mut SkippedExports::default(),
    )
    .await
    .unwrap()
}

const COMPLETE: BaselineProgress = BaselineProgress {
    gate: BaselineGate::Allowed,
    complete: true,
};

/// The path stop reasons the findings list, in order.
async fn path_stops(root: &RootLease) -> Vec<String> {
    let observation = root.observation().await.unwrap();
    root.observation_page(
        observation.revision,
        ObservationCursor::START,
        crate::storage::sync_state::PageLimit::LARGEST,
    )
    .await
    .unwrap()
    .into_iter()
    .filter(|item| {
        item.category == crate::storage::sync_state::observation::FindingCategory::PathStop
    })
    .map(|item| item.reason.as_str().to_owned())
    .collect()
}

/// An object the tree cannot hold is reported, but it has no file that could look deleted, so
/// it neither keeps the baseline open nor stops the deletion of a file removed from the tree. A
/// complete baseline also spares later passes the full reconciliation they ran while it stayed
/// open.
#[tokio::test]
async fn an_object_the_tree_cannot_hold_neither_holds_back_the_baseline_nor_external_deletions() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    publish_named(&catalog, &bucket, "kept", b"kept", Origin::Api).await;
    publish_named(&catalog, &bucket, "deleted", b"deleted", Origin::Api).await;
    publish_named(&catalog, &bucket, &unexportable_name(), b"api", Origin::Api).await;
    let mut baseline = BaselineProgress {
        gate: BaselineGate::Allowed,
        complete: false,
    };
    let status = MirrorStatusSender::default();
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert!(baseline.complete);
    assert!(summary.baseline_complete);
    let reconciled = reconcile(&catalog, &root).await.unwrap();
    assert_eq!(
        (
            reconciled.unrepresentable,
            reconciled.unmapped_objects,
            reconciled.unrepresentable_entries().get()
        ),
        (1, 1, 0)
    );
    assert_eq!(root.observation().await.unwrap().mapping_findings, 1);

    fs::remove_file(tree.0.join("test-bucket/deleted")).unwrap();
    // A pass after the baseline completed ingests and inventories without reconciling again:
    // a reconciliation would plan each object and block at this hook.
    let hook = std::sync::Arc::new(Hook {
        phase: "before-binding",
        entered: Default::default(),
        release: Default::default(),
    });
    let summary = tokio::time::timeout(
        HANG_GUARD,
        EXPORT_HOOK.scope(hook, ingest(&catalog, &root, &status, &mut baseline)),
    )
    .await
    .unwrap();
    assert!(summary.baseline_complete);
    assert!(!exists(&catalog, &bucket, "deleted").await);
    assert!(exists(&catalog, &bucket, "kept").await);
    assert!(exists(&catalog, &bucket, &unexportable_name()).await);
    assert_eq!(root.observation().await.unwrap().mapping_findings, 1);
    drop(root);
    catalog.close().await.unwrap();
}

/// A tree entry no object can take may be where a missing file went, so it stops every inferred
/// deletion until it is gone. The mirror's status names it by path meanwhile.
#[tokio::test]
async fn an_entry_no_object_can_take_stops_deletions_and_is_named_until_it_is_gone() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    publish_named(&catalog, &bucket, "kept", b"kept", Origin::Api).await;
    publish_named(&catalog, &bucket, "deleted", b"deleted", Origin::Api).await;
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let outside = TestDir::new();
    fs::write(outside.0.join("target"), b"outside").unwrap();
    std::os::unix::fs::symlink(outside.0.join("target"), tree.0.join("test-bucket/link")).unwrap();
    fs::remove_file(tree.0.join("test-bucket/deleted")).unwrap();
    let status = MirrorStatusSender::default();
    let mut baseline = COMPLETE;
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert_eq!(summary.unrepresentable_entries().get(), 1);
    assert!(root.deletion_set().await.unwrap().is_none());
    assert!(exists(&catalog, &bucket, "deleted").await);
    assert_eq!(
        status.subscribe().unrepresentable_entries().entries(),
        [UnrepresentableEntry {
            path: "test-bucket/link".into(),
            problem: EntryProblem::UnsafeEntry,
        }]
    );

    fs::remove_file(tree.0.join("test-bucket/link")).unwrap();
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert_eq!(summary.unrepresentable, 0);
    assert!(
        status
            .subscribe()
            .unrepresentable_entries()
            .entries()
            .is_empty()
    );
    assert!(!exists(&catalog, &bucket, "deleted").await);
    assert!(exists(&catalog, &bucket, "kept").await);
    assert_eq!(fs::read(outside.0.join("target")).unwrap(), b"outside");
    drop(root);
    catalog.close().await.unwrap();
}

/// A directory where a tracked file was tells nothing about that file. It stops that one path,
/// in the inventory and in exports alike, while the other deletion goes ahead and the pass ends
/// without a failure that would pause or retry the root.
#[tokio::test]
async fn a_tracked_file_replaced_by_a_directory_stops_only_its_own_path() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    for name in ["replaced", "deleted", "kept"] {
        publish_named(&catalog, &bucket, name, name.as_bytes(), Origin::Api).await;
    }
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let replaced = tree.0.join("test-bucket/replaced");
    fs::remove_file(&replaced).unwrap();
    fs::create_dir(&replaced).unwrap();
    fs::remove_file(tree.0.join("test-bucket/deleted")).unwrap();
    let status = MirrorStatusSender::default();
    let mut baseline = COMPLETE;
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert_eq!(summary.unrepresentable, 0);
    assert!(!exists(&catalog, &bucket, "deleted").await);
    assert!(exists(&catalog, &bucket, "replaced").await);
    assert_eq!(path_stops(&root).await, ["EntryReplaced"]);

    // Neither a new API generation nor the API deletion reaches through the directory.
    publish_named(&catalog, &bucket, "replaced", b"newer", Origin::Api).await;
    let summary = reconcile(&catalog, &root).await.unwrap();
    assert_eq!((summary.exported, summary.unrepresentable), (0, 0));
    assert!(replaced.is_dir());
    assert_eq!(root.observation().await.unwrap().mapping_findings, 0);
    assert_eq!(path_stops(&root).await, ["EntryReplaced"]);
    let current = catalog
        .admin(Origin::Api)
        .get_object(&bucket, &ObjectName::new("replaced").unwrap(), None)
        .await
        .unwrap();
    catalog
        .admin(Origin::Api)
        .delete_object(
            &bucket,
            &ObjectName::new("replaced").unwrap(),
            GenerationCondition::Is(Generation::new(current.generation).unwrap()),
        )
        .await
        .unwrap();
    reconcile(&catalog, &root).await.unwrap();
    assert!(replaced.is_dir());

    // Once the directory is gone the path holds nothing, and its stop clears.
    fs::remove_dir(&replaced).unwrap();
    ingest(&catalog, &root, &status, &mut baseline).await;
    assert!(path_stops(&root).await.is_empty());
    assert!(exists(&catalog, &bucket, "kept").await);
    drop(root);
    catalog.close().await.unwrap();
}

/// With the baseline complete, a tracked file missing while its API generation stayed the same
/// was deleted from the tree, wherever the mirror was meanwhile. Reconciliation leaves it to the
/// deletion inventory instead of writing it back.
#[tokio::test]
async fn a_file_deleted_while_the_mirror_was_away_is_left_to_the_deletion_inventory() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    publish_named(&catalog, &bucket, "kept", b"kept", Origin::Api).await;
    publish_named(&catalog, &bucket, "deleted", b"deleted", Origin::Api).await;
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let deleted = tree.0.join("test-bucket/deleted");
    fs::remove_file(&deleted).unwrap();
    let summary = reconcile(&catalog, &root).await.unwrap();
    assert_eq!(summary.exported, 0);
    assert!(summary.baseline_complete);
    assert!(!deleted.exists());
    assert!(exists(&catalog, &bucket, "deleted").await);
    let status = MirrorStatusSender::default();
    let mut baseline = COMPLETE;
    ingest(&catalog, &root, &status, &mut baseline).await;
    assert!(!exists(&catalog, &bucket, "deleted").await);
    assert!(exists(&catalog, &bucket, "kept").await);
    assert!(!deleted.exists());
    drop(root);
    catalog.close().await.unwrap();
}

/// After `max_sync_depth` is lowered below an exported object, that object stays out of the
/// tree and out of every deletion, and the deletion of another file still goes ahead.
#[tokio::test]
async fn a_lowered_depth_limit_keeps_the_deeper_object_while_other_deletions_proceed() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    for name in ["one/shallow", "one/gone", "one/two/deep"] {
        publish_named(&catalog, &bucket, name, name.as_bytes(), Origin::Api).await;
    }
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    assert!(tree.0.join("test-bucket/one/two/deep").is_file());
    let mut config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    drop(root);
    // The deeper directory has to go before the restart: a tree deeper than the limit stops
    // every scan with a remedy that names the setting.
    fs::remove_dir_all(tree.0.join("test-bucket/one/two")).unwrap();
    config.max_sync_depth = std::num::NonZeroUsize::new(2);
    let root = crate::sync::test_support::probed(
        crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap(),
    )
    .await;
    let summary = reconcile(&catalog, &root).await.unwrap();
    assert_eq!((summary.unmapped_objects, summary.unrepresentable), (1, 1));
    assert!(summary.baseline_complete);
    fs::remove_file(tree.0.join("test-bucket/one/gone")).unwrap();
    let status = MirrorStatusSender::default();
    let mut baseline = COMPLETE;
    ingest(&catalog, &root, &status, &mut baseline).await;
    assert!(!exists(&catalog, &bucket, "one/gone").await);
    assert!(exists(&catalog, &bucket, "one/two/deep").await);
    assert!(exists(&catalog, &bucket, "one/shallow").await);
    assert!(!tree.0.join("test-bucket/one/two").exists());
    drop(root);
    catalog.close().await.unwrap();
}
