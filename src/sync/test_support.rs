#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]

/// The rescan faults a mirror test arms, which a test of the control route arms as well.
pub(crate) use super::mirror::{RescanFault, arm_rescan_fault};
use super::{Hook, Missing, RootLease};
use crate::{
    clock::{Clock, ManualClock, SystemClock},
    config::Config,
    fsutil,
    storage::{
        Catalog,
        admin::{BucketDefinition, BucketName, Origin},
        fault::copy_with_times,
        sync_state::{SyncFingerprint, SyncPath, SyncUpdate, deletions::Candidate},
    },
    test_support::TestDir,
};
use std::{
    fs::{self, File},
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

/// How [`retrack`] records a file's tracked fingerprint.
#[derive(Clone, Copy, Debug)]
pub(super) enum Retrack {
    /// Stable, under the file's own inode number: a scan skips the file without reading it.
    Stable,
    /// Stable, under another inode number: what a later scan sees when Docker Desktop's shared
    /// folders report a new number for the file.
    Renumbered,
    /// Racy, under the file's own inode number: an observation too close to its last change.
    Racy,
}

/// Records the current size and times of the tracked file at `path`, relative to the tree, as
/// its tracked fingerprint, keeping the tracked hashes and generation.
pub(super) async fn retrack(
    catalog: &Catalog,
    root: &RootLease,
    tree: &Path,
    path: &str,
    record: Retrack,
) {
    let (bucket, name) =
        super::mapping::path_to_name(Path::new(path), root.naming().unwrap()).unwrap();
    let binding = root.sync_binding(&bucket).await.unwrap();
    let state = catalog.sync_state();
    let tracked = state
        .get_path(binding.clone(), path.to_owned())
        .await
        .unwrap()
        .unwrap();
    let identity = fsutil::stat_identity(&File::open(tree.join(path)).unwrap()).unwrap();
    let (inode, racy) = match record {
        Retrack::Stable => (identity.inode, false),
        Retrack::Renumbered => (!identity.inode, false),
        Retrack::Racy => (identity.inode, true),
    };
    let fingerprint = SyncFingerprint {
        size: identity.size,
        mtime_ns: identity.modified_ns,
        ctime_ns: identity.changed_ns,
        inode,
        racy,
        ..tracked.fingerprint
    };
    state
        .acknowledge(
            &name,
            tracked.generation,
            SyncUpdate::new(
                binding,
                crate::storage::sync_state::durable::Durable::assumed(
                    SyncPath::try_from(path.to_owned()).unwrap(),
                ),
                fingerprint,
            )
            .unwrap(),
        )
        .await
        .unwrap();
}

pub(super) async fn fixture(
    bucket: Option<&BucketName>,
) -> (TestDir, TestDir, Catalog, RootLease, Config) {
    let (data, tree, catalog, root, config) = unprobed_fixture(bucket).await;
    (data, tree, catalog, probed(root).await, config)
}

/// Like [`fixture`], on a catalog whose clock the test sets. A lookup that waits for a name to
/// open again waits on this clock, so its waits pass without sleeping.
pub(super) async fn clocked_fixture(
    bucket: Option<&BucketName>,
) -> (
    TestDir,
    TestDir,
    Catalog,
    RootLease,
    Config,
    Arc<ManualClock>,
) {
    let clock = Arc::new(ManualClock::new(SystemTime::now()));
    let (data, tree, catalog, root, config) = unprobed_fixture_on(bucket, clock.clone()).await;
    (data, tree, catalog, probed(root).await, config, clock)
}

/// An activated root whose naming rules nobody has probed yet, as the mirror receives it.
pub(super) async fn unprobed_fixture(
    bucket: Option<&BucketName>,
) -> (TestDir, TestDir, Catalog, RootLease, Config) {
    unprobed_fixture_on(bucket, Arc::new(SystemClock)).await
}

async fn unprobed_fixture_on(
    bucket: Option<&BucketName>,
    clock: Arc<dyn Clock>,
) -> (TestDir, TestDir, Catalog, RootLease, Config) {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open_with_clock(&data.0, clock).await.unwrap();
    if let Some(bucket) = bucket {
        catalog
            .admin(Origin::Api)
            .ensure_bucket(BucketDefinition::default_for(bucket, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
    }
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    let root = super::preflight(&catalog, &config).await.unwrap().unwrap();
    (data, tree, catalog, root, config)
}

/// Probes and records the root's naming rules as the mirror does when it activates the root, for
/// tests that drive a pass without starting the mirror.
pub(super) async fn probed(root: RootLease) -> RootLease {
    let writeback = root.default_writeback().await.unwrap();
    super::naming::activate(&writeback).await.unwrap();
    root
}

pub(super) async fn hooked_worker<F>(
    key: &'static tokio::task::LocalKey<Arc<Hook>>,
    phase: &'static str,
    work: F,
) -> (Arc<Hook>, tokio::task::JoinHandle<F::Output>)
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let hook = Arc::new(Hook {
        phase,
        entered: Default::default(),
        release: Default::default(),
    });
    let worker = tokio::spawn(key.scope(hook.clone(), work));
    tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
        .await
        .unwrap();
    (hook, worker)
}

/// The entries of `workspace` that write-back operations own, without the directory the root's
/// naming probe keeps there.
pub(crate) fn operation_entries(workspace: &Path) -> Vec<PathBuf> {
    fs::read_dir(workspace)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_name() != super::naming::PROBE_DIRECTORY)
        .map(|entry| entry.path())
        .collect()
}

/// Puts back into `tree` what a crash or power-loss `snapshot` of it holds of the workspace's
/// operation entries and of each path in `targets`: a file, an empty directory or nothing. The
/// root and workspace directories stay, because the catalog binds their identities. Files keep
/// their times, which tracked fingerprints and recovery's ownership rule compare, so only their
/// inode numbers and change times differ from the snapshot's.
pub(crate) fn restore_tree(snapshot: &Path, tree: &Path, targets: &[&str]) {
    let workspace = tree.join(super::mapping::WORKSPACE);
    for entry in operation_entries(&workspace) {
        fs::remove_file(entry).unwrap();
    }
    for entry in operation_entries(&snapshot.join(super::mapping::WORKSPACE)) {
        copy_with_times(&entry, &workspace.join(entry.file_name().unwrap())).unwrap();
    }
    for target in targets {
        let current = tree.join(target);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir(&current).unwrap(),
            Ok(_) => fs::remove_file(&current).unwrap(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("{}: {error}", current.display()),
        }
        let kept = snapshot.join(target);
        match fs::symlink_metadata(&kept) {
            Ok(metadata) if metadata.is_dir() => fs::create_dir(&current).unwrap(),
            Ok(_) => {
                fs::create_dir_all(current.parent().unwrap()).unwrap();
                copy_with_times(&kept, &current).unwrap();
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("{}: {error}", kept.display()),
        }
    }
}

/// `candidate`, whose path the test emptied, as the inventory sees it missing.
pub(crate) async fn seen_missing(root: &RootLease, candidate: Candidate) -> Missing {
    let observed = super::deletion::observe(root, &root.lookup().unwrap(), candidate)
        .await
        .unwrap();
    let super::deletion::Observed::Absent(missing) = observed else {
        panic!("the path is not seen empty: {observed:?}");
    };
    missing
}
