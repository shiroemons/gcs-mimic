//! A live mirror exporting an object whose target name an empty directory holds, including
//! crashes and power losses around the directory's removal.

use super::install_faults::{Started, conflict_bodies, workspace_bodies};
use super::*;
use crate::storage::fault::{self, Crash, Fault, Step};
use crate::sync::WaitReason;
use crate::sync::test_support::restore_tree;

const BODY: &[u8] = b"api\0body";
/// The target of the object `a`.
const TARGET: &str = "test-bucket/a";

/// Deletes `record` through the API.
async fn delete(catalog: &Catalog, bucket: &BucketName, record: &ObjectRecord) {
    catalog
        .admin(Origin::Api)
        .delete_object(
            bucket,
            &ObjectName::new(record.name.clone()).unwrap(),
            GenerationCondition::Is(Generation::new(record.generation).unwrap()),
        )
        .await
        .unwrap();
}

/// The generation the tree tracks for `a`, if it tracks one.
async fn tracked(started: &Started, bucket: &BucketName) -> Option<i64> {
    let binding = started.root.sync_binding(bucket).await.unwrap();
    started
        .catalog
        .sync_state()
        .get_path(binding, TARGET.into())
        .await
        .unwrap()
        .map(|record| record.generation.get())
}

/// After an API delete leaves `a/` empty, creating `a` through the API exports it in place of
/// that directory in the same run, with no finding and no restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_api_object_replaces_the_directory_an_api_delete_emptied() {
    let (data, tree, catalog, bucket, _) = live_fixture().await;
    let nested = publish_named(&catalog, &bucket, "a/x.txt", BODY, Origin::Api).await;
    let mut started = Started::start(catalog, &data.0, &tree.0).await;
    assert_eq!(started.wait().await.unwrap().reason, WaitReason::Ready);
    assert_eq!(fs::read(tree.0.join("test-bucket/a/x.txt")).unwrap(), BODY);
    delete(&started.catalog, &bucket, &nested).await;
    assert_eq!(started.wait().await.unwrap().reason, WaitReason::Ready);
    assert!(fs::read_dir(tree.0.join(TARGET)).unwrap().next().is_none());
    let record = publish_named(&started.catalog, &bucket, "a", BODY, Origin::Api).await;
    assert_eq!(started.wait().await.unwrap().reason, WaitReason::Ready);
    assert_eq!(fs::read(tree.0.join(TARGET)).unwrap(), BODY);
    assert_eq!(tracked(&started, &bucket).await, Some(record.generation));
    assert!(started.root.findings_page(0, 256).await.unwrap().is_empty());
    started.close().await;
}

/// A removal that the filesystem refuses, because the directory holds an entry or may not be
/// removed, records an occupied directory for that object alone: the root keeps running and the
/// other object of the same pass is exported.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_removal_is_one_collision_and_other_exports_continue() {
    for kind in [
        io::ErrorKind::DirectoryNotEmpty,
        io::ErrorKind::PermissionDenied,
    ] {
        let (data, tree, catalog, bucket, _) = live_fixture().await;
        fs::create_dir_all(tree.0.join(TARGET)).unwrap();
        let mut started = Started::start(catalog, &data.0, &tree.0).await;
        fault::arm(&tree.0, Step::SyncEmptyDirectoryRemove, 0, Fault::Io(kind));
        publish_named(&started.catalog, &bucket, "a", BODY, Origin::Api).await;
        publish_named(&started.catalog, &bucket, "b", BODY, Origin::Api).await;
        // A name the tree cannot hold is reported and does not stop the wait.
        let waited = started.wait().await.unwrap();
        assert_eq!(waited.reason, WaitReason::Ready, "{kind:?}");
        assert_eq!(
            waited.findings.map(|found| found.unmapped),
            Some(1),
            "{kind:?}"
        );
        assert_eq!(fault::pending(&tree.0), 0, "{kind:?}");
        assert!(tree.0.join(TARGET).is_dir(), "{kind:?}");
        assert_eq!(fs::read(tree.0.join("test-bucket/b")).unwrap(), BODY);
        let findings = started.root.findings_page(0, 256).await.unwrap();
        assert_eq!(findings.len(), 1, "{kind:?}");
        assert_eq!(findings[0].name.as_str(), "a");
        assert_eq!(
            findings[0].reason,
            FindingKind::Unmapped(Reason::DirectoryOccupied)
        );
        assert!(workspace_bodies(&tree.0).is_empty());
        started.close().await;
    }
}

/// A collision an export meets at installation is an object the tree cannot hold, as one a
/// target check finds: the pass counts the object, completes the baseline and leaves deletion
/// detection to the tree's own entries.
#[tokio::test]
async fn a_collision_at_installation_counts_the_object_and_holds_back_nothing() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    fs::create_dir_all(tree.0.join(TARGET)).unwrap();
    publish_named(&catalog, &bucket, "a", BODY, Origin::Api).await;
    fault::arm(
        &tree.0,
        Step::SyncEmptyDirectoryRemove,
        0,
        Fault::Io(io::ErrorKind::DirectoryNotEmpty),
    );
    let summary = reconcile(&catalog, &root).await.unwrap();
    assert_eq!(fault::pending(&tree.0), 0);
    assert_eq!(
        (
            summary.unmapped_objects(),
            summary.unrepresentable_entries().get(),
            summary.unrepresentable()
        ),
        (1, 0, 1)
    );
    assert!(summary.baseline_complete);
    assert!(tree.0.join(TARGET).is_dir());
    drop(root);
    catalog.close().await.unwrap();
}

/// A removal that fails for a passing reason, such as an interrupted call, keeps that cause: the
/// export retries it and installs the object in the directory's place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transient_removal_failure_is_retried() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    drop(root);
    fs::create_dir_all(tree.0.join(TARGET)).unwrap();
    let mut started = Started::start(catalog, &data.0, &tree.0).await;
    fault::arm(
        &tree.0,
        Step::SyncEmptyDirectoryRemove,
        0,
        Fault::Io(io::ErrorKind::Interrupted),
    );
    let record = publish_named(&started.catalog, &bucket, "a", BODY, Origin::Api).await;
    assert_eq!(started.wait().await.unwrap().reason, WaitReason::Ready);
    assert_eq!(fault::pending(&tree.0), 0);
    assert_eq!(fs::read(tree.0.join(TARGET)).unwrap(), BODY);
    assert_eq!(tracked(&started, &bucket).await, Some(record.generation));
    assert!(started.root.findings_page(0, 256).await.unwrap().is_empty());
    started.close().await;
}

/// A directory that holds only a file scans ignore, but that a removal does not take out, keeps
/// the object out of the tree as one the tree cannot hold: the wait is ready, the baseline
/// completes and a file deleted from the tree still deletes its object.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_holding_an_ignored_file_holds_back_neither_the_wait_nor_deletions() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    drop(root);
    publish_named(&catalog, &bucket, "a", BODY, Origin::Api).await;
    publish_named(&catalog, &bucket, "gone", BODY, Origin::Api).await;
    fs::create_dir_all(tree.0.join(TARGET)).unwrap();
    fs::write(tree.0.join(TARGET).join(".x.swp"), b"editor").unwrap();
    let mut started = Started::start(catalog, &data.0, &tree.0).await;
    let waited = started.wait().await.unwrap();
    assert_eq!(waited.reason, WaitReason::Ready);
    assert_eq!(waited.findings.map(|found| found.unmapped), Some(1));
    // Only a complete baseline and a pass that holds nothing back turn a missing file into a
    // deletion.
    fs::remove_file(tree.0.join("test-bucket/gone")).unwrap();
    assert_eq!(started.wait().await.unwrap().reason, WaitReason::Ready);
    let gone = started
        .catalog
        .admin(Origin::Api)
        .get_object(&bucket, &ObjectName::new("gone").unwrap(), None)
        .await;
    assert!(matches!(gone, Err(AdminError::NotFound)), "{gone:?}");
    assert_eq!(
        fs::read(tree.0.join(TARGET).join(".x.swp")).unwrap(),
        b"editor"
    );
    started.close().await;
}

/// A crash or power loss after the export unlinked the `.DS_Store` of the directory at its
/// target, and before it removed that directory, recovers to the API state on restart: the
/// unlink was synced, the snapshot holds the empty directory, and the restart's reconciliation
/// removes it and installs the object. A user's file elsewhere in the bucket stays in the
/// snapshot and in the tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crashes_and_power_losses_after_the_metadata_unlink_recover_to_the_api_state() {
    const KEPT: &str = "test-bucket/notes/keep.txt";
    for &crash in Crash::ALL {
        let case = format!("{crash:?}");
        let (data, tree, catalog, bucket, _) = live_fixture().await;
        fs::create_dir_all(tree.0.join(TARGET)).unwrap();
        fs::write(tree.0.join(TARGET).join(".DS_Store"), b"finder").unwrap();
        fs::create_dir_all(tree.0.join("test-bucket/notes")).unwrap();
        fs::write(tree.0.join(KEPT), b"user").unwrap();
        let mut started = Started::start(catalog, &data.0, &tree.0).await;
        assert_eq!(started.wait().await.unwrap().reason, WaitReason::Ready);
        let snapshots = TestDir::new();
        let (snapshot_data, snapshot_tree) = (snapshots.0.join("data"), snapshots.0.join("tree"));
        let step = Step::SyncDirectoryMetadataRemoved;
        fault::arm(&tree.0, step, 0, crash.fault(snapshot_tree.clone()));
        started
            .catalog
            .inject(step, 0, Fault::Snapshot(snapshot_data.clone()));
        let record = publish_named(&started.catalog, &bucket, "a", BODY, Origin::Api).await;
        assert_eq!(
            started.wait().await.unwrap().reason,
            WaitReason::Ready,
            "{case}"
        );
        assert_eq!(fault::pending(&tree.0), 0, "{case}");
        assert_eq!(started.catalog.pending_faults(), 0, "{case}");
        started.close().await;

        // The snapshot holds the directory without its `.DS_Store` and the user's file as it was.
        assert!(snapshot_tree.join(TARGET).is_dir(), "{case}");
        assert!(
            !snapshot_tree.join(TARGET).join(".DS_Store").exists(),
            "{case}"
        );
        assert_eq!(
            fs::read(snapshot_tree.join(KEPT)).unwrap(),
            b"user",
            "{case}"
        );
        restore_tree(&snapshot_tree, &tree.0, &[TARGET]);
        assert_eq!(workspace_bodies(&tree.0).len(), 1, "{case}");
        assert_eq!(Held::at(&tree.0.join(TARGET)), Held::Directory, "{case}");

        let catalog = Catalog::open(&snapshot_data).await.unwrap();
        crate::storage::sync_state::writeback::retrack_restored(&catalog, &tree.0)
            .await
            .unwrap();
        let mut started = Started::start(catalog, &snapshot_data, &tree.0).await;
        assert_eq!(
            started.wait().await.unwrap().reason,
            WaitReason::Ready,
            "{case}"
        );
        assert!(conflict_bodies(&snapshot_data).is_empty(), "{case}");
        assert_eq!(Held::at(&tree.0.join(TARGET)), Held::Object, "{case}");
        assert_eq!(
            tracked(&started, &bucket).await,
            Some(record.generation),
            "{case}"
        );
        assert_eq!(fs::read(tree.0.join(KEPT)).unwrap(), b"user", "{case}");
        assert!(workspace_bodies(&tree.0).is_empty(), "{case}");
        assert!(
            started.root.findings_page(0, 256).await.unwrap().is_empty(),
            "{case}"
        );
        started.close().await;
        drop((data, snapshots));
    }
}

/// A symlink at the target, to an empty directory or to nothing, is never followed or removed:
/// the export records it as an unsafe entry, as before empty directories gave way, and the
/// directory it points to stays. Only the export runs, because a scan stops at the symlinks.
#[tokio::test]
async fn a_symlink_at_the_target_is_neither_followed_nor_removed() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    let outside = TestDir::new();
    fs::create_dir(outside.0.join("empty")).unwrap();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    std::os::unix::fs::symlink(outside.0.join("empty"), tree.0.join("test-bucket/a")).unwrap();
    std::os::unix::fs::symlink(outside.0.join("gone"), tree.0.join("test-bucket/b")).unwrap();
    publish_named(&catalog, &bucket, "a", BODY, Origin::Api).await;
    publish_named(&catalog, &bucket, "b", BODY, Origin::Api).await;
    assert_eq!(reconcile(&catalog, &root).await.unwrap().exported, 0);
    let mut findings = root
        .findings_page(0, 256)
        .await
        .unwrap()
        .into_iter()
        .map(|finding| (finding.name, finding.reason))
        .collect::<Vec<_>>();
    findings.sort_by(|left, right| left.0.cmp(&right.0));
    let unsafe_entry = FindingKind::Unmapped(Reason::UnsafeEntry);
    assert_eq!(
        findings,
        [
            ("a".to_owned(), unsafe_entry),
            ("b".to_owned(), unsafe_entry)
        ]
    );
    for link in ["a", "b"] {
        assert!(tree.0.join("test-bucket").join(link).is_symlink(), "{link}");
    }
    assert!(outside.0.join("empty").is_dir());
    drop(root);
    catalog.close().await.unwrap();
}

/// What a snapshot holds at the target.
#[derive(Debug, PartialEq, Eq)]
enum Held {
    /// The empty directory.
    Directory,
    /// Nothing.
    Missing,
    /// The exported object.
    Object,
}

impl Held {
    fn at(path: &Path) -> Self {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                assert!(fs::read_dir(path).unwrap().next().is_none());
                Self::Directory
            }
            Ok(_) => {
                assert_eq!(fs::read(path).unwrap(), BODY);
                Self::Object
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::Missing,
            Err(error) => panic!("{error}"),
        }
    }

    /// What the snapshot at `step` holds: the private API copy until installation moves it,
    /// and at the target the directory until a sync of the target's parent makes its removal
    /// durable. The rename's own parent syncs make both durable before its checkpoint.
    fn expected(step: Step, crash: Crash) -> (usize, Self) {
        match (step, crash) {
            (Step::SyncEmptyDirectoryRemove, _)
            | (Step::SyncEmptyDirectoryRemoved, Crash::PowerLoss) => (1, Self::Directory),
            (Step::SyncEmptyDirectoryRemoved, Crash::Abort) => (1, Self::Missing),
            (Step::SyncCopyRenamed | Step::SyncCopyInstalled, _) => (0, Self::Object),
            (step, _) => panic!("no removal checkpoint: {step:?}"),
        }
    }
}

/// A crash or power loss before the removal, between the removal and its parent's sync, after
/// the rename that installs the object and after installation recovers to the API state on
/// restart: no conflict, `sync wait` succeeds and the object stands where the directory was. A
/// power loss that undoes the removal leaves the empty directory, which the restart's
/// reconciliation removes the same way. The snapshot holds exactly what each checkpoint
/// promises. The removal's own sync of the parent is not observable here: the rename's call syncs
/// the same directory before any later checkpoint, so removing that sync passes this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crashes_and_power_losses_around_the_removal_recover_to_the_api_state() {
    for step in [
        Step::SyncEmptyDirectoryRemove,
        Step::SyncEmptyDirectoryRemoved,
        Step::SyncCopyRenamed,
        Step::SyncCopyInstalled,
    ] {
        for &crash in Crash::ALL {
            let case = format!("{step:?} {crash:?}");
            let (data, tree, catalog, bucket, _) = live_fixture().await;
            fs::create_dir_all(tree.0.join(TARGET)).unwrap();
            let mut started = Started::start(catalog, &data.0, &tree.0).await;
            let snapshots = TestDir::new();
            let (snapshot_data, snapshot_tree) =
                (snapshots.0.join("data"), snapshots.0.join("tree"));
            fault::arm(&tree.0, step, 0, crash.fault(snapshot_tree.clone()));
            started
                .catalog
                .inject(step, 0, Fault::Snapshot(snapshot_data.clone()));
            let record = publish_named(&started.catalog, &bucket, "a", BODY, Origin::Api).await;
            assert_eq!(
                started.wait().await.unwrap().reason,
                WaitReason::Ready,
                "{case}"
            );
            assert_eq!(fault::pending(&tree.0), 0, "{case}");
            assert_eq!(started.catalog.pending_faults(), 0, "{case}");
            started.close().await;

            restore_tree(&snapshot_tree, &tree.0, &[TARGET]);
            let (copies, held) = Held::expected(step, crash);
            assert_eq!(workspace_bodies(&tree.0).len(), copies, "{case}");
            assert_eq!(Held::at(&tree.0.join(TARGET)), held, "{case}");

            let catalog = Catalog::open(&snapshot_data).await.unwrap();
            crate::storage::sync_state::writeback::retrack_restored(&catalog, &tree.0)
                .await
                .unwrap();
            let mut started = Started::start(catalog, &snapshot_data, &tree.0).await;
            assert_eq!(
                started.wait().await.unwrap().reason,
                WaitReason::Ready,
                "{case}"
            );
            assert!(conflict_bodies(&snapshot_data).is_empty(), "{case}");
            assert_eq!(Held::at(&tree.0.join(TARGET)), Held::Object, "{case}");
            assert_eq!(
                tracked(&started, &bucket).await,
                Some(record.generation),
                "{case}"
            );
            assert!(workspace_bodies(&tree.0).is_empty(), "{case}");
            assert!(
                started.root.findings_page(0, 256).await.unwrap().is_empty(),
                "{case}"
            );
            started.close().await;
            drop((data, snapshots));
        }
    }
}
