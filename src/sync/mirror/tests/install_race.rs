//! A user's change to the tree that races an export's installation. The attempt finds the tree
//! other than it read it and cancels, the export of that object waits for the next pass, and
//! the root keeps synchronizing: such a change is no failure an operator must clear with a
//! rescan. The next pass plans the export again through every check, so what the user put in
//! place stays as it is.

use super::*;
use crate::storage::fault::{Fault, Step};
use crate::sync::control::WaitReason;
use std::sync::Arc;

const BODY: &[u8] = b"api\0body";

/// A step of the mirror's file work held until the test changed the tree.
struct Held {
    entered: tokio::sync::oneshot::Receiver<()>,
    release: std::sync::mpsc::Sender<()>,
}

impl Held {
    /// Holds the next file work on `tree` at `step`.
    fn arm(tree: &Path, step: Step) -> Self {
        let (entered, held) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        fault::arm(
            tree,
            step,
            0,
            Fault::Block {
                entered: Arc::new(std::sync::Mutex::new(Some(entered))),
                release: Arc::new(std::sync::Mutex::new(released)),
            },
        );
        Self {
            entered: held,
            release,
        }
    }

    /// Waits until the work is held, runs `change`, then lets the work go on.
    async fn change(self, change: impl FnOnce()) {
        tokio::time::timeout(HANG_GUARD, self.entered)
            .await
            .unwrap()
            .unwrap();
        change();
        self.release.send(()).unwrap();
    }
}

/// Runs the wait `sync wait` sends while `held` lets `change` alter the tree, and returns the
/// wait's answer.
async fn wait_racing(
    task: &Task,
    held: Held,
    change: impl FnOnce(),
) -> crate::sync::control::WaitResult {
    let waited = queue_wait(task, HANG_GUARD).await;
    held.change(change).await;
    tokio::time::timeout(HANG_GUARD, waited)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

/// Checks that the mirror kept running after a race, with nothing recorded as a pause.
async fn still_running(task: &Task, root: &RootLease) {
    assert_eq!(task.status().state(), MirrorState::Running);
    assert_eq!(root.observation().await.unwrap().paused_reason, None);
}

/// The directory at an export's target, replaced after the export proved it empty and before it
/// removed it, is not the directory the proof read. The export cancels rather than remove it,
/// and the next pass proves the new directory: it holds an AppleDouble file, which may hold a
/// user's data, so it stays as a collision with that file. The other object of the same change
/// is exported, the wait ends ready, and the mirror never pauses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_replaced_while_an_export_removes_it_pauses_nothing() {
    let (_data, tree, catalog, task) = started_mirror(None).await;
    task.rescan().await.unwrap();
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    let directory = tree.0.join("test-bucket/a");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(".DS_Store"), b"finder").unwrap();
    let held = Held::arm(&tree.0, Step::SyncDirectoryMetadataRemoved);
    let bucket = BucketName::new("test-bucket").unwrap();
    publish_named(&catalog, &bucket, "a", BODY, Origin::Api).await;
    publish_named(&catalog, &bucket, "b", BODY, Origin::Api).await;

    let waited = wait_racing(&task, held, || {
        fs::remove_dir_all(&directory).unwrap();
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("._a"), b"apple double").unwrap();
    })
    .await;
    assert_eq!(waited.reason, WaitReason::Ready);
    assert_eq!(waited.findings.map(|found| found.unmapped), Some(1));
    still_running(&task, &root).await;
    assert_eq!(fs::read(directory.join("._a")).unwrap(), b"apple double");
    assert_eq!(fs::read(tree.0.join("test-bucket/b")).unwrap(), BODY);
    let findings = root.findings_page(0, 256).await.unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].name.as_str(), "a");
    assert_eq!(
        findings[0].reason,
        FindingKind::Unmapped(Reason::DirectoryOccupied)
    );
    assert!(install_faults::workspace_bodies(&tree.0).is_empty());
    assert_eq!(fault::pending(&tree.0), 0);
    drop(root);
    task.finish().await.unwrap();
    catalog.close().await.unwrap();
}

/// A directory on the path to an export's target, replaced by a user's file after the export
/// copied the object and before it installed it, leaves the path unresolved. The export cancels
/// and unlinks its private copy; the next pass finds the file in the place of the directory, and
/// the object stays out of the tree with that file untouched. The mirror never pauses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_on_the_path_replaced_by_a_file_after_the_copy_pauses_nothing() {
    let (_data, tree, catalog, task) = started_mirror(None).await;
    task.rescan().await.unwrap();
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    let directory = tree.0.join("test-bucket/sub");
    fs::create_dir_all(&directory).unwrap();
    let held = Held::arm(&tree.0, Step::SyncCopyDurable);
    let bucket = BucketName::new("test-bucket").unwrap();
    publish_named(&catalog, &bucket, "sub/b", BODY, Origin::Api).await;

    let waited = wait_racing(&task, held, || {
        fs::remove_dir(&directory).unwrap();
        fs::write(&directory, b"mine").unwrap();
    })
    .await;
    // The file is a tree entry that no object can take while `sub/b` exists, which the wait
    // names; the race itself holds nothing back.
    assert_eq!(waited.reason, WaitReason::SourceUnrepresentable);
    let reconciliation = waited.reconciliation.unwrap();
    assert_eq!(reconciliation.deferred, 0);
    assert_eq!(reconciliation.unrepresentable_entries().get(), 1);
    still_running(&task, &root).await;
    assert_eq!(fs::read(&directory).unwrap(), b"mine");
    let findings = root.findings_page(0, 256).await.unwrap();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].name.as_str(), "sub/b");
    assert_eq!(findings[0].reason, FindingKind::Unmapped(Reason::Collision));
    assert!(
        install_faults::workspace_bodies(&tree.0).is_empty(),
        "the cancelled export left its private copy"
    );
    assert_eq!(fault::pending(&tree.0), 0);
    drop(root);
    task.finish().await.unwrap();
    catalog.close().await.unwrap();
}

/// A restore whose installation the same change races, as after a protected or rejected
/// deletion, plans again from the tree instead of failing: it finds the user's file in the
/// place of the directory, leaves it untouched and writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_raced_by_a_change_on_its_path_plans_again() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    let directory = tree.0.join("test-bucket/sub");
    publish_named(&catalog, &bucket, "sub/b", BODY, Origin::Api).await;
    reconcile(&catalog, &root).await.unwrap();
    // The file went, as when a deletion that a hold protects or the user rejected is restored.
    fs::remove_file(directory.join("b")).unwrap();
    let held = Held::arm(&tree.0, Step::SyncCopyDurable);
    let restoring = {
        let (catalog, root, bucket) = (catalog.clone(), root.clone(), bucket.clone());
        crate::task::spawn_supervised("restore-race-test", async move {
            let name = ObjectName::new("sub/b").unwrap();
            restore_from_catalog(&catalog, &root, &bucket, &name).await
        })
    };
    held.change(|| {
        fs::remove_dir(&directory).unwrap();
        fs::write(&directory, b"mine").unwrap();
    })
    .await;
    let restored = tokio::time::timeout(HANG_GUARD, restoring)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(restored, Restored::Skipped);
    assert_eq!(fs::read(&directory).unwrap(), b"mine");
    assert!(install_faults::workspace_bodies(&tree.0).is_empty());
    assert_eq!(
        root.default_writeback()
            .await
            .unwrap()
            .status()
            .await
            .unwrap()
            .entries,
        0
    );
    let findings = root.findings_page(0, 256).await.unwrap();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].reason, FindingKind::Unmapped(Reason::Collision));
    assert_eq!(fault::pending(&tree.0), 0);
    drop(root);
    catalog.close().await.unwrap();
}
