//! A live mirror exporting an object whose target name an empty directory holds, including
//! crashes and power losses around the directory's removal.

use super::install_faults::{Started, conflict_bodies, workspace_bodies};
use super::*;
use crate::storage::fault::{self, Crash, Fault, Step};
use crate::sync::control::WaitReason;
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
/// removed, records a collision for that object alone: the root keeps running and the other
/// object of the same pass is exported.
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
        assert_eq!(waited.findings.map(|found| found.unmapped), Some(1), "{kind:?}");
        assert_eq!(fault::pending(&tree.0), 0, "{kind:?}");
        assert!(tree.0.join(TARGET).is_dir(), "{kind:?}");
        assert_eq!(fs::read(tree.0.join("test-bucket/b")).unwrap(), BODY);
        let findings = started.root.findings_page(0, 256).await.unwrap();
        assert_eq!(findings.len(), 1, "{kind:?}");
        assert_eq!(findings[0].name.as_str(), "a");
        assert_eq!(findings[0].reason, FindingKind::Unmapped(Reason::Collision));
        assert!(workspace_bodies(&tree.0).is_empty());
        started.close().await;
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
