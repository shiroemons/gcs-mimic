#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]

use super::{Hook, RootLease};
use crate::{
    clock::SystemClock,
    config::Config,
    fsutil,
    storage::{
        Catalog,
        admin::{BucketDefinition, BucketName, Origin},
        sync_state::{SyncFingerprint, SyncPath, SyncUpdate},
    },
    test_support::TestDir,
};
use std::{fs::File, future::Future, path::Path, sync::Arc, time::Duration};

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
    let (bucket, name) = super::mapping::path_to_name(Path::new(path)).unwrap();
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
                SyncPath::try_from(path.to_owned()).unwrap(),
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
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
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
