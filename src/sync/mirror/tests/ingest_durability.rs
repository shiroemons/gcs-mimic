//! Power losses around ingestion, recovered through the startup path. A tracking row tells
//! deletion detection that its file was in the tree, so a power loss must never keep the row
//! and lose the file's directory entry: the restart would delete the API object.
#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::*;
use crate::storage::fault::{Fault, Step};

const BODY: &[u8] = b"external bytes";
const DIRECTORY: &str = "test-bucket/fresh";
const FILE: &str = "test-bucket/fresh/object";

/// A mirror started the way the server starts it: write-back recovery, ingestion,
/// reconciliation and deletion observation.
struct Started {
    catalog: Catalog,
    root: RootLease,
    _live: Initialized,
}

impl Started {
    async fn start(data: &Path, tree: &Path) -> Self {
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
                status: &MirrorStatusSender::default(),
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

    async fn close(self) {
        drop(self._live);
        drop(self.root);
        self.catalog.close().await.unwrap();
    }
}

/// Creates the file in a new directory the way a user does, without syncing either, and tells
/// the power-loss model so: both entries stay volatile until a directory sync makes them durable.
fn create_unsynced(tree: &Path) {
    let directory = tree.join(DIRECTORY);
    fs::create_dir(&directory).unwrap();
    fault::dir_created(&directory);
    let path = tree.join(FILE);
    fs::write(&path, BODY).unwrap();
    fault::file_created(&path);
}

/// Replaces the new directory in `tree` with what the snapshot kept of it. The root and bucket
/// directories stay, because the catalog binds the root's identity.
fn restore_tree(snapshot: &Path, tree: &Path) {
    fs::remove_dir_all(tree.join(DIRECTORY)).unwrap();
    if snapshot.join(DIRECTORY).is_dir() {
        fs::create_dir(tree.join(DIRECTORY)).unwrap();
    }
    if snapshot.join(FILE).exists() {
        fs::copy(snapshot.join(FILE), tree.join(FILE)).unwrap();
    }
}

async fn api_body(catalog: &Catalog, bucket: &BucketName) -> Option<u64> {
    match catalog
        .admin(Origin::Api)
        .get_object(bucket, &ObjectName::new("fresh/object").unwrap(), None)
        .await
    {
        Ok(record) => Some(record.size),
        Err(AdminError::NotFound) => None,
        Err(error) => panic!("{error}"),
    }
}

async fn tracked(catalog: &Catalog, root: &RootLease, bucket: &BucketName) -> bool {
    let binding = root.sync_binding(bucket).await.unwrap();
    catalog
        .sync_state()
        .get_path(binding, FILE.into())
        .await
        .unwrap()
        .is_some()
}

/// A power loss while ingestion takes in a file the user never synced, followed by a restart.
/// Before ingestion syncs anything, the file and its new directory vanish and nothing tracks
/// them, so the restart has nothing to delete. Once ingestion recorded the file, the tracking row
/// and the file both survive, so the restart neither deletes the API object nor loses the file;
/// the file sits in a new directory, so that directory's entry must be made durable too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_power_loss_around_ingestion_never_keeps_a_tracking_row_without_its_file() {
    for (step, kept) in [
        (Step::SyncSourceSync, false),
        (Step::SyncSourceRecorded, true),
    ] {
        let case = format!("{step:?}");
        let (data, tree, catalog, bucket, root) = live_fixture().await;
        drop(root);
        catalog.close().await.unwrap();
        Started::start(&data.0, &tree.0).await.close().await;
        assert!(tree.0.join("test-bucket").is_dir(), "{case}");

        let snapshots = TestDir::new();
        let (snapshot_data, snapshot_tree) = (snapshots.0.join("data"), snapshots.0.join("tree"));
        fault::arm(&tree.0, step, 0, Fault::PowerLoss(snapshot_tree.clone()));
        fault::arm(&data.0, step, 0, Fault::Snapshot(snapshot_data.clone()));
        create_unsynced(&tree.0);
        let started = Started::start(&data.0, &tree.0).await;
        assert_eq!(fault::pending(&tree.0), 0, "{case}");
        assert_eq!(fault::pending(&data.0), 0, "{case}");
        started.close().await;

        restore_tree(&snapshot_tree, &tree.0);
        let catalog = Catalog::open(&snapshot_data).await.unwrap();
        crate::storage::sync_state::writeback::retrack_restored(&catalog, &tree.0)
            .await
            .unwrap();
        catalog.close().await.unwrap();
        let started = Started::start(&snapshot_data, &tree.0).await;
        assert_eq!(
            api_body(&started.catalog, &bucket).await,
            kept.then_some(BODY.len() as u64),
            "{case}"
        );
        assert_eq!(
            tracked(&started.catalog, &started.root, &bucket).await,
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

/// A failed sync of the file, of its new directory or of the bucket directory above that
/// records nothing for the file: no API object, no tracking row, and the file is counted as
/// deferred, which holds deletion detection back. The next scan syncs and ingests it.
#[tokio::test]
async fn a_failed_sync_before_ingestion_records_nothing_and_the_next_scan_retries() {
    for skip in 0..3 {
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
        assert_eq!(api_body(&catalog, &bucket).await, None, "{skip}");
        assert!(!tracked(&catalog, &root, &bucket).await, "{skip}");

        let retried = crate::sync::ingest::rescan(&catalog, &root, &config, &stopping)
            .await
            .unwrap();
        assert_eq!((retried.ingested, retried.deferred), (1, 0), "{skip}");
        assert_eq!(
            api_body(&catalog, &bucket).await,
            Some(BODY.len() as u64),
            "{skip}"
        );
        assert!(tracked(&catalog, &root, &bucket).await, "{skip}");
        drop(root);
        catalog.close().await.unwrap();
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
