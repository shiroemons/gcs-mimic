//! Power losses and failed syncs around tracking a file, recovered through the startup path. A
//! tracking row tells deletion detection that its file was in the tree, so a power loss must
//! never keep the row and lose the file's directory entry: the restart would delete the API
//! object. Ingestion and the write-back that observes an identical file both record rows.
#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::*;
use crate::storage::fault::{Fault, Step};

const BODY: &[u8] = b"external bytes";
const DIRECTORY: &str = "test-bucket/fresh";
const FILE: &str = "test-bucket/fresh/object";
const OBJECT: &str = "fresh/object";
/// A file two new directories deep, below the bucket directory.
const NESTED_TOP: &str = "test-bucket/a";
const NESTED_FILE: &str = "test-bucket/a/b/x";
const NESTED_OBJECT: &str = "a/b/x";

/// A mirror started the way the server starts it: write-back recovery, ingestion,
/// reconciliation and deletion observation.
pub(super) struct Started {
    pub(super) catalog: Catalog,
    pub(super) root: RootLease,
    _live: Initialized,
}

impl Started {
    pub(super) async fn start(data: &Path, tree: &Path) -> Self {
        let catalog = Catalog::open(data).await.unwrap();
        let config = Config {
            data_dir: data.to_owned(),
            sync_dir: Some(tree.to_owned()),
            ..Default::default()
        };
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        let (_stop, stopping) = tokio::sync::watch::channel(false);
        let live = initialize_live(
            LiveRun {
                catalog: &catalog,
                root: &root,
                config: &config,
                status: &Lifecycle::default(),
                resume: &mut None,
            },
            &stopping,
        )
        .await
        .unwrap();
        Self {
            catalog,
            root,
            _live: live,
        }
    }

    pub(super) async fn close(self) {
        drop(self._live);
        drop(self.root);
        self.catalog.close().await.unwrap();
    }
}

/// Creates the file in a new directory the way a user does, without syncing either, and tells
/// the power-loss model so: both entries stay volatile until a directory sync makes them durable.
fn create_unsynced(tree: &Path) {
    let directory = tree.join(DIRECTORY);
    fs::create_dir_all(directory.parent().unwrap()).unwrap();
    fs::create_dir(&directory).unwrap();
    fault::dir_created(&directory);
    let path = tree.join(FILE);
    fs::write(&path, BODY).unwrap();
    fault::file_created(&path);
}

/// Like [`create_unsynced`], for the file two new directories below the bucket directory.
fn create_nested_unsynced(tree: &Path) {
    let mut directory = tree.join("test-bucket");
    for component in ["a", "b"] {
        directory.push(component);
        fs::create_dir(&directory).unwrap();
        fault::dir_created(&directory);
    }
    let path = tree.join(NESTED_FILE);
    fs::write(&path, BODY).unwrap();
    fault::file_created(&path);
}

/// Replaces the new directory `top` in `tree` with what the snapshot kept of it. The root and
/// bucket directories stay, because the catalog binds the root's identity.
fn restore_tree(snapshot: &Path, tree: &Path, top: &str) {
    fs::remove_dir_all(tree.join(top)).unwrap();
    if snapshot.join(top).is_dir() {
        copy_tree(&snapshot.join(top), &tree.join(top));
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Restarts from the snapshots after a power loss: the tree below `top` as the power loss left
/// it, and the catalog as it was then.
async fn restart_after_power_loss(snapshots: &TestDir, tree: &Path, top: &str) -> Started {
    let (snapshot_data, snapshot_tree) = (snapshots.0.join("data"), snapshots.0.join("tree"));
    restore_tree(&snapshot_tree, tree, top);
    let catalog = Catalog::open(&snapshot_data).await.unwrap();
    crate::storage::sync_state::writeback::retrack_restored(&catalog, tree)
        .await
        .unwrap();
    catalog.close().await.unwrap();
    Started::start(&snapshot_data, tree).await
}

async fn api_body(catalog: &Catalog, bucket: &BucketName, name: &str) -> Option<u64> {
    match catalog
        .admin(Origin::Api)
        .get_object(bucket, &ObjectName::new(name).unwrap(), None)
        .await
    {
        Ok(record) => Some(record.size),
        Err(AdminError::NotFound) => None,
        Err(error) => panic!("{error}"),
    }
}

async fn tracked(catalog: &Catalog, root: &RootLease, bucket: &BucketName, path: &str) -> bool {
    let binding = root.sync_binding(bucket).await.unwrap();
    catalog
        .sync_state()
        .get_path(binding, path.into())
        .await
        .unwrap()
        .is_some()
}

/// A root whose baseline an earlier start completed, closed again.
async fn started_once() -> (TestDir, TestDir, BucketName) {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    drop(root);
    catalog.close().await.unwrap();
    Started::start(&data.0, &tree.0).await.close().await;
    assert!(tree.0.join("test-bucket").is_dir());
    (data, tree, bucket)
}

/// Publishes `name` through the API while the server is stopped.
async fn publish_stopped(data: &Path, bucket: &BucketName, name: &str) {
    let catalog = Catalog::open(data).await.unwrap();
    publish_named(&catalog, bucket, name, BODY, Origin::Api).await;
    catalog.close().await.unwrap();
}

/// A power loss while ingestion takes in a file the user never synced, followed by a restart.
/// Before ingestion syncs the bucket directory, the file's new directory vanishes and nothing
/// tracks it, so the restart has nothing to delete; the file survives once that sync made its
/// directory's entry durable, untracked, and the restart ingests it. Once ingestion recorded
/// the file, the tracking row and the file both survive, so the restart neither deletes the API
/// object nor loses the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_power_loss_around_ingestion_never_keeps_a_tracking_row_without_its_file() {
    // Ingestion syncs the file, its directory, the bucket directory and the root, in that order.
    for (step, skip, kept) in [
        (Step::SyncSourceSync, 0, false),
        (Step::SyncSourceSync, 1, false),
        (Step::SyncSourceSync, 2, false),
        (Step::SyncSourceSync, 3, true),
        (Step::SyncSourceRecorded, 0, true),
    ] {
        let case = format!("{step:?} {skip}");
        let (data, tree, bucket) = started_once().await;

        let snapshots = TestDir::new();
        let (snapshot_data, snapshot_tree) = (snapshots.0.join("data"), snapshots.0.join("tree"));
        fault::arm(&tree.0, step, skip, Fault::PowerLoss(snapshot_tree));
        fault::arm(&data.0, step, 0, Fault::Snapshot(snapshot_data));
        create_unsynced(&tree.0);
        let started = Started::start(&data.0, &tree.0).await;
        assert_eq!(fault::pending(&tree.0), 0, "{case}");
        assert_eq!(fault::pending(&data.0), 0, "{case}");
        started.close().await;

        let started = restart_after_power_loss(&snapshots, &tree.0, DIRECTORY).await;
        assert_eq!(
            api_body(&started.catalog, &bucket, OBJECT).await,
            kept.then_some(BODY.len() as u64),
            "{case}"
        );
        assert_eq!(
            tracked(&started.catalog, &started.root, &bucket, FILE).await,
            kept,
            "{case}"
        );
        assert_eq!(
            fs::read(tree.0.join(FILE)).ok().as_deref(),
            kept.then_some(BODY),
            "{case}"
        );
        let set = started.root.deletion_set().await.unwrap();
        assert_eq!(set.map_or(0, |set| set.candidate_count), 0, "{case}");
        started.close().await;
    }
}

/// The same power losses while ingestion tracks a file that already holds its object's bytes,
/// which it acknowledges instead of publishing. The object exists throughout, so the restart
/// either finds the file tracked or writes it back; it never deletes the object.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_power_loss_while_an_identical_file_is_tracked_never_deletes_its_object() {
    for (step, skip) in [
        (Step::SyncSourceSync, 0),
        (Step::SyncSourceSync, 1),
        (Step::SyncSourceSync, 2),
        (Step::SyncSourceSync, 3),
        (Step::SyncSourceRecorded, 0),
    ] {
        let case = format!("{step:?} {skip}");
        let (data, tree, bucket) = started_once().await;
        publish_stopped(&data.0, &bucket, OBJECT).await;

        let snapshots = TestDir::new();
        let (snapshot_data, snapshot_tree) = (snapshots.0.join("data"), snapshots.0.join("tree"));
        fault::arm(&tree.0, step, skip, Fault::PowerLoss(snapshot_tree));
        fault::arm(&data.0, step, 0, Fault::Snapshot(snapshot_data));
        create_unsynced(&tree.0);
        let started = Started::start(&data.0, &tree.0).await;
        assert_eq!(fault::pending(&tree.0), 0, "{case}");
        assert_eq!(fault::pending(&data.0), 0, "{case}");
        assert!(
            tracked(&started.catalog, &started.root, &bucket, FILE).await,
            "{case}"
        );
        started.close().await;

        let started = restart_after_power_loss(&snapshots, &tree.0, DIRECTORY).await;
        assert_eq!(
            api_body(&started.catalog, &bucket, OBJECT).await,
            Some(BODY.len() as u64),
            "{case}"
        );
        assert!(
            tracked(&started.catalog, &started.root, &bucket, FILE).await,
            "{case}"
        );
        assert_eq!(fs::read(tree.0.join(FILE)).unwrap(), BODY, "{case}");
        let set = started.root.deletion_set().await.unwrap();
        assert_eq!(set.map_or(0, |set| set.candidate_count), 0, "{case}");
        started.close().await;
    }
}

/// A file identical to its object, which the user wrote two new directories deep without
/// syncing anything. A failed sync at any level stops ingestion from tracking it, and the
/// reconciliation that follows at startup observes the same file through write-back instead,
/// which records the row. Write-back must make every directory up to the root durable before
/// that row, as ingestion would have: after a power loss right after the row, the restart finds
/// the file, and the object is not deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_observed_after_a_failed_ingestion_sync_survives_a_power_loss() {
    // Ingestion syncs the file, `b`, `a`, the bucket directory and the root, in that order.
    for skip in 0..5 {
        let (data, tree, bucket) = started_once().await;
        publish_stopped(&data.0, &bucket, NESTED_OBJECT).await;

        let snapshots = TestDir::new();
        let (snapshot_data, snapshot_tree) = (snapshots.0.join("data"), snapshots.0.join("tree"));
        fault::arm(
            &tree.0,
            Step::SyncCopyCommit,
            0,
            Fault::PowerLoss(snapshot_tree),
        );
        fault::arm(
            &data.0,
            Step::SyncCopyCommit,
            0,
            Fault::Snapshot(snapshot_data),
        );
        fault::arm(
            &tree.0,
            Step::SyncSourceSync,
            skip,
            Fault::Io(io::ErrorKind::PermissionDenied),
        );
        create_nested_unsynced(&tree.0);
        let started = Started::start(&data.0, &tree.0).await;
        assert_eq!(fault::pending(&tree.0), 0, "{skip}");
        assert_eq!(fault::pending(&data.0), 0, "{skip}");
        assert!(
            tracked(&started.catalog, &started.root, &bucket, NESTED_FILE).await,
            "{skip}"
        );
        started.close().await;

        let started = restart_after_power_loss(&snapshots, &tree.0, NESTED_TOP).await;
        assert_eq!(
            api_body(&started.catalog, &bucket, NESTED_OBJECT).await,
            Some(BODY.len() as u64),
            "{skip}"
        );
        assert_eq!(fs::read(tree.0.join(NESTED_FILE)).unwrap(), BODY, "{skip}");
        assert!(
            tracked(&started.catalog, &started.root, &bucket, NESTED_FILE).await,
            "{skip}"
        );
        let set = started.root.deletion_set().await.unwrap();
        assert_eq!(set.map_or(0, |set| set.candidate_count), 0, "{skip}");
        started.close().await;
    }
}

/// A failed sync of the file, of its new directory, of the bucket directory above that or of
/// the root records nothing for the file: no API object, no tracking row, and the file is
/// counted as deferred, which holds deletion detection back. The next scan syncs and ingests it.
#[tokio::test]
async fn a_failed_sync_before_ingestion_records_nothing_and_the_next_scan_retries() {
    for skip in 0..4 {
        let (data, tree, catalog, bucket, root) = live_fixture().await;
        let config = Config {
            data_dir: data.0.clone(),
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        };
        let (_stop, stopping) = tokio::sync::watch::channel(false);
        fs::create_dir_all(tree.0.join(DIRECTORY)).unwrap();
        fs::write(tree.0.join(FILE), BODY).unwrap();
        fault::arm(
            &tree.0,
            Step::SyncSourceSync,
            skip,
            Fault::Io(io::ErrorKind::PermissionDenied),
        );
        let failed = crate::sync::ingest::rescan(&catalog, &root, &config, &stopping)
            .await
            .unwrap();
        assert_eq!(fault::pending(&tree.0), 0, "{skip}");
        assert_eq!((failed.ingested, failed.deferred), (0, 1), "{skip}");
        assert!(!failed.gate().hold().infers_deletions(), "{skip}");
        assert_eq!(api_body(&catalog, &bucket, OBJECT).await, None, "{skip}");
        assert!(!tracked(&catalog, &root, &bucket, FILE).await, "{skip}");

        let retried = crate::sync::ingest::rescan(&catalog, &root, &config, &stopping)
            .await
            .unwrap();
        assert_eq!((retried.ingested, retried.deferred), (1, 0), "{skip}");
        assert_eq!(
            api_body(&catalog, &bucket, OBJECT).await,
            Some(BODY.len() as u64),
            "{skip}"
        );
        assert!(tracked(&catalog, &root, &bucket, FILE).await, "{skip}");
        drop(root);
        catalog.close().await.unwrap();
    }
}

/// The same failures for a file that already holds its object's bytes, which ingestion
/// acknowledges instead of publishing: nothing is tracked and the object stays as it was until
/// the next scan tracks the file.
#[tokio::test]
async fn a_failed_sync_before_an_identical_file_is_tracked_records_nothing() {
    for skip in 0..4 {
        let (data, tree, catalog, bucket, root) = live_fixture().await;
        let config = Config {
            data_dir: data.0.clone(),
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        };
        let (_stop, stopping) = tokio::sync::watch::channel(false);
        let api = publish_named(&catalog, &bucket, OBJECT, BODY, Origin::Api).await;
        fs::create_dir_all(tree.0.join(DIRECTORY)).unwrap();
        fs::write(tree.0.join(FILE), BODY).unwrap();
        fault::arm(
            &tree.0,
            Step::SyncSourceSync,
            skip,
            Fault::Io(io::ErrorKind::PermissionDenied),
        );
        let failed = crate::sync::ingest::rescan(&catalog, &root, &config, &stopping)
            .await
            .unwrap();
        assert_eq!(fault::pending(&tree.0), 0, "{skip}");
        assert_eq!((failed.identical, failed.deferred), (0, 1), "{skip}");
        assert!(!failed.gate().hold().infers_deletions(), "{skip}");
        assert!(!tracked(&catalog, &root, &bucket, FILE).await, "{skip}");

        let retried = crate::sync::ingest::rescan(&catalog, &root, &config, &stopping)
            .await
            .unwrap();
        assert_eq!((retried.identical, retried.deferred), (1, 0), "{skip}");
        assert!(tracked(&catalog, &root, &bucket, FILE).await, "{skip}");
        let current = catalog
            .admin(Origin::Api)
            .get_object(&bucket, &ObjectName::new(OBJECT).unwrap(), None)
            .await
            .unwrap();
        assert_eq!(current.generation, api.generation, "{skip}");
        drop(root);
        catalog.close().await.unwrap();
    }
}

/// A failed sync during the startup ingestion of a new file, followed by the startup
/// reconciliation, which has no object to compare the file with: nothing tracks the file, so
/// nothing can delete it, and the next start ingests it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_sync_at_startup_leaves_a_new_file_for_the_next_start() {
    for skip in 0..4 {
        let (data, tree, bucket) = started_once().await;
        fs::create_dir(tree.0.join(DIRECTORY)).unwrap();
        fs::write(tree.0.join(FILE), BODY).unwrap();
        fault::arm(
            &tree.0,
            Step::SyncSourceSync,
            skip,
            Fault::Io(io::ErrorKind::PermissionDenied),
        );
        let started = Started::start(&data.0, &tree.0).await;
        assert_eq!(fault::pending(&tree.0), 0, "{skip}");
        assert_eq!(
            api_body(&started.catalog, &bucket, OBJECT).await,
            None,
            "{skip}"
        );
        assert!(
            !tracked(&started.catalog, &started.root, &bucket, FILE).await,
            "{skip}"
        );
        assert_eq!(fs::read(tree.0.join(FILE)).unwrap(), BODY, "{skip}");
        started.close().await;

        let started = Started::start(&data.0, &tree.0).await;
        assert_eq!(
            api_body(&started.catalog, &bucket, OBJECT).await,
            Some(BODY.len() as u64),
            "{skip}"
        );
        assert!(
            tracked(&started.catalog, &started.root, &bucket, FILE).await,
            "{skip}"
        );
        started.close().await;
    }
}

/// A bucket directory missing at startup, as a mount that did not come up leaves it, is not
/// recreated, and its tracked files wait for approval however few they are; the objects stay.
/// While the server runs, removing the directory reaches the same inventory without a full
/// reconciliation, so it behaves the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bucket_directory_missing_at_startup_stays_missing_and_waits_for_approval() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    drop(root);
    catalog.close().await.unwrap();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for name in ["first", "second"] {
        fs::write(tree.0.join("test-bucket").join(name), BODY).unwrap();
    }
    Started::start(&data.0, &tree.0).await.close().await;
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();

    let started = Started::start(&data.0, &tree.0).await;
    assert!(!tree.0.join("test-bucket").exists());
    let set = started.root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));
    for name in ["first", "second"] {
        assert!(
            started
                .catalog
                .admin(Origin::Api)
                .get_object(&bucket, &ObjectName::new(name).unwrap(), None)
                .await
                .is_ok()
        );
    }
    started.close().await;
}
