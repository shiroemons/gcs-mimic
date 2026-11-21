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

pub(super) async fn exists(catalog: &Catalog, bucket: &BucketName, name: &str) -> bool {
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

/// One live ingestion pass, as a native event or a rescan request starts it.
pub(super) async fn ingest(
    catalog: &Catalog,
    root: &RootLease,
    status: &Lifecycle,
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

pub(super) const COMPLETE: BaselineProgress = BaselineProgress {
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
    let status = Lifecycle::default();
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert!(baseline.complete);
    assert!(summary.baseline_complete);
    let reconciled = reconcile(&catalog, &root).await.unwrap();
    assert_eq!(
        (
            reconciled.unrepresentable(),
            reconciled.unmapped_objects(),
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
    let status = Lifecycle::default();
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
    assert_eq!(summary.unrepresentable(), 0);
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

/// A file renamed to a name that keeps failing to open while it holds the file is deferred as a
/// read failure, which holds the deletion inventory back: the object of the old name stays in
/// that pass, though its file is gone. Once the new name opens, a pass takes it in and the old
/// name's object goes.
#[tokio::test]
async fn a_rename_whose_new_name_keeps_failing_to_open_deletes_nothing_meanwhile() {
    let bucket = BucketName::new("test-bucket").unwrap();
    let (_data, tree, catalog, root, _config, clock) =
        crate::sync::test_support::clocked_fixture(Some(&bucket)).await;
    publish_named(&catalog, &bucket, "a", b"renamed", Origin::Api).await;
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let directory = tree.0.join("test-bucket");
    fs::rename(directory.join("a"), directory.join("b")).unwrap();
    clock.set(std::time::SystemTime::now() + std::time::Duration::from_secs(3));
    let failing = fsutil::FailingOpens::vanishing(&directory, "b", usize::MAX);
    let status = Lifecycle::default();
    let mut baseline = COMPLETE;
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert!(failing.left() < usize::MAX);
    assert_eq!(
        summary.gate().hold(),
        crate::sync::mirror::gate::Hold::Deferred
    );
    assert!(root.deletion_set().await.unwrap().is_none());
    assert!(exists(&catalog, &bucket, "a").await);
    assert!(!exists(&catalog, &bucket, "b").await);
    drop(failing);
    clock.set(std::time::SystemTime::now() + std::time::Duration::from_secs(6));
    ingest(&catalog, &root, &status, &mut baseline).await;
    assert!(exists(&catalog, &bucket, "b").await);
    assert!(!exists(&catalog, &bucket, "a").await);
    assert_eq!(fs::read(directory.join("b")).unwrap(), b"renamed");
    drop(root);
    catalog.close().await.unwrap();
}

/// The log names each entry no object can take once while it stays: a later scan that still
/// finds it warns about nothing, a new entry is warned about on its own, and one that comes
/// back after it was gone is warned about again.
#[tokio::test]
async fn each_entry_no_object_can_take_is_warned_about_once_while_it_stays() {
    let (_data, tree, catalog, _bucket, root) = live_fixture().await;
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let link = tree.0.join("test-bucket/link");
    std::os::unix::fs::symlink("nowhere", &link).unwrap();
    let status = Lifecycle::default();
    let mut baseline = COMPLETE;
    ingest(&catalog, &root, &status, &mut baseline).await;
    ingest(&catalog, &root, &status, &mut baseline).await;
    let unsafe_link = UnrepresentableEntry {
        path: "test-bucket/link".into(),
        problem: EntryProblem::UnsafeEntry,
    };
    assert_eq!(
        status.entries().warned_entries(),
        std::slice::from_ref(&unsafe_link)
    );

    fs::create_dir(tree.0.join("Not_A_Bucket")).unwrap();
    ingest(&catalog, &root, &status, &mut baseline).await;
    let unmappable = UnrepresentableEntry {
        path: "Not_A_Bucket".into(),
        problem: EntryProblem::UnmappableName,
    };
    assert_eq!(
        status.entries().warned_entries(),
        [unsafe_link.clone(), unmappable.clone()]
    );

    fs::remove_file(&link).unwrap();
    ingest(&catalog, &root, &status, &mut baseline).await;
    std::os::unix::fs::symlink("nowhere", &link).unwrap();
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert_eq!(summary.unrepresentable_entries().get(), 2);
    assert_eq!(
        status.entries().warned_entries(),
        [unsafe_link.clone(), unmappable, unsafe_link]
    );
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
    let status = Lifecycle::default();
    let mut baseline = COMPLETE;
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert_eq!(summary.unrepresentable(), 0);
    assert!(!exists(&catalog, &bucket, "deleted").await);
    assert!(exists(&catalog, &bucket, "replaced").await);
    assert_eq!(path_stops(&root).await, ["EntryReplaced"]);

    // Neither a new API generation nor the API deletion reaches through the directory.
    publish_named(&catalog, &bucket, "replaced", b"newer", Origin::Api).await;
    let summary = reconcile(&catalog, &root).await.unwrap();
    assert_eq!((summary.exported, summary.unrepresentable()), (0, 0));
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

/// A directory with a file in it where a tracked file was stops more than its own path: the file
/// inside needs that path as a directory while the tracked object keeps it a file, so it is an
/// entry no object can take, which holds back every deletion. Moving the directory away without
/// putting a file back then deletes the object it replaced, as deleting the file does.
#[tokio::test]
async fn a_tracked_file_replaced_by_a_directory_with_files_holds_back_every_deletion() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    for name in ["replaced", "deleted", "kept"] {
        publish_named(&catalog, &bucket, name, name.as_bytes(), Origin::Api).await;
    }
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let replaced = tree.0.join("test-bucket/replaced");
    fs::remove_file(&replaced).unwrap();
    fs::create_dir(&replaced).unwrap();
    fs::write(replaced.join("inside"), b"inside").unwrap();
    fs::remove_file(tree.0.join("test-bucket/deleted")).unwrap();
    let status = Lifecycle::default();
    let mut baseline = COMPLETE;
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert_eq!(summary.unrepresentable_entries().get(), 1);
    assert!(exists(&catalog, &bucket, "deleted").await);
    assert!(exists(&catalog, &bucket, "replaced").await);
    assert!(!exists(&catalog, &bucket, "replaced/inside").await);
    // No inventory ran to look at the path, so the entry inside is what names it.
    assert!(path_stops(&root).await.is_empty());

    fs::remove_dir_all(&replaced).unwrap();
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert_eq!(summary.unrepresentable_entries().get(), 0);
    assert!(!exists(&catalog, &bucket, "replaced").await);
    assert!(!exists(&catalog, &bucket, "deleted").await);
    assert!(exists(&catalog, &bucket, "kept").await);
    assert!(path_stops(&root).await.is_empty());
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
    let status = Lifecycle::default();
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
    assert_eq!(
        (summary.unmapped_objects(), summary.unrepresentable()),
        (1, 1)
    );
    assert!(summary.baseline_complete);
    fs::remove_file(tree.0.join("test-bucket/one/gone")).unwrap();
    let status = Lifecycle::default();
    let mut baseline = COMPLETE;
    ingest(&catalog, &root, &status, &mut baseline).await;
    assert!(!exists(&catalog, &bucket, "one/gone").await);
    assert!(exists(&catalog, &bucket, "one/two/deep").await);
    assert!(exists(&catalog, &bucket, "one/shallow").await);
    assert!(!tree.0.join("test-bucket/one/two").exists());
    drop(root);
    catalog.close().await.unwrap();
}

/// Starts the mirror on `tree` with `max_sync_depth` as the server does: ingestion,
/// reconciliation and the deletion inventory.
async fn start_with_depth(
    catalog: &Catalog,
    data: &TestDir,
    tree: &TestDir,
    depth: Option<std::num::NonZeroUsize>,
) -> (RootLease, Initialized) {
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        max_sync_depth: depth,
        ..Default::default()
    };
    let root = crate::sync::preflight(catalog, &config)
        .await
        .unwrap()
        .unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let live = initialize_live(
        LiveRun {
            catalog,
            root: &root,
            config: &config,
            status: &Lifecycle::default(),
            resume: &mut None,
        },
        &stopping,
    )
    .await
    .unwrap();
    (root, live)
}

/// An object with a finding is no deletion candidate, so once the finding's cause is gone, its
/// missing tracked file is written back and the finding clears, instead of the reconciliation
/// leaving the file to an inventory that skips it.
#[tokio::test]
async fn a_missing_file_of_an_object_with_a_finding_is_written_back_once_the_cause_is_gone() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    for name in ["one/shallow", "one/two/deep"] {
        publish_named(&catalog, &bucket, name, name.as_bytes(), Origin::Api).await;
    }
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    drop(root);
    fs::remove_dir_all(tree.0.join("test-bucket/one/two")).unwrap();
    let (root, live) =
        start_with_depth(&catalog, &data, &tree, std::num::NonZeroUsize::new(2)).await;
    assert_eq!(root.observation().await.unwrap().mapping_findings, 1);
    drop(live);
    drop(root);

    let (root, live) = start_with_depth(&catalog, &data, &tree, None).await;
    assert_eq!(
        fs::read(tree.0.join("test-bucket/one/two/deep")).unwrap(),
        b"one/two/deep"
    );
    assert_eq!(root.observation().await.unwrap().mapping_findings, 0);
    assert!(exists(&catalog, &bucket, "one/two/deep").await);
    assert!(exists(&catalog, &bucket, "one/shallow").await);
    drop(live);
    drop(root);
    catalog.close().await.unwrap();
}
