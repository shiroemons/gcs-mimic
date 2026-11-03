#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]

use super::{Hook, RootLease};
use crate::{
    clock::SystemClock,
    config::Config,
    storage::{
        Catalog,
        admin::{BucketDefinition, BucketName, Origin},
    },
    test_support::TestDir,
};
use std::{future::Future, sync::Arc, time::Duration};

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
