use super::*;

#[tokio::test]
async fn native_quiet_deadline_is_capped_and_survives_cancelled_waits() {
    let (_data, _tree, catalog, root, _config) = crate::sync::test_support::fixture(None).await;
    let mut watcher = DirectoryWatch::new(&root).await.unwrap();
    let now = tokio::time::Instant::now();
    watcher.pending = Some(Pending {
        change: Change::Structure,
        quiet: now + Duration::from_secs(60),
        limit: now + Duration::from_millis(50),
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(5), watcher.next_observation())
            .await
            .is_err()
    );
    assert!(watcher.pending.is_some());
    let result = tokio::time::timeout(Duration::from_millis(500), watcher.next_observation())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Observed::Settled(Change::Structure)));
    assert!(watcher.pending.is_none());
    drop(watcher);
    drop(root);
    catalog.close().await.unwrap();
}
