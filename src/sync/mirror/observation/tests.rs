use super::*;

async fn tracked(root: &RootLease, expected: u64) -> Result<(), RootError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if root.observation().await?.tracked_objects == expected
            && root.deletion_set().await?.is_some()
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "tracked count did not reach {expected}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

async fn cleared_history(root: &RootLease) -> Result<(), RootError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while root.automatic_deletion_count().await? != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "quiet automatic scan did not clear history"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[tokio::test]
async fn native_quiet_scans_keep_repeated_small_deletions_automatic_across_restart() {
    use std::fs;
    let (data, tree, mut catalog, mut root, config) =
        crate::sync::test_support::fixture(None).await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for index in 0..50 {
        fs::write(tree.0.join(format!("test-bucket/{index:03}")), b"body").unwrap();
    }
    let mut task = crate::sync::start(catalog.clone(), root.clone(), &config).unwrap();
    tracked(&root, 50).await.unwrap();
    for round in 0..6 {
        for index in 0..20 {
            fs::remove_file(tree.0.join(format!("test-bucket/{index:03}"))).unwrap();
        }
        tracked(&root, 30).await.unwrap();
        fs::write(tree.0.join("test-bucket/.DS_Store"), [round]).unwrap();
        cleared_history(&root).await.unwrap();
        for index in 0..20 {
            fs::write(tree.0.join(format!("test-bucket/{index:03}")), b"body").unwrap();
        }
        tracked(&root, 50).await.unwrap();
        if round == 2 {
            task.finish().await.unwrap();
            drop(root);
            catalog.close().await.unwrap();
            catalog = Catalog::open(&data.0).await.unwrap();
            root = crate::sync::preflight(&catalog, &config)
                .await
                .unwrap()
                .unwrap();
            task = crate::sync::start(catalog.clone(), root.clone(), &config).unwrap();
            tracked(&root, 50).await.unwrap();
        }
    }
    task.finish().await.unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn quiet_waits_hold_their_original_deadline_without_blocking_other_controls() {
    let (_data, _tree, catalog, root, config) = crate::sync::test_support::fixture(None).await;
    let mut watcher = DirectoryWatch::new(&root).await.unwrap();
    let start = tokio::time::Instant::now();
    watcher.pending = Some(Pending {
        change: Change::Structure,
        quiet: start + Duration::from_secs(10),
        limit: start + Duration::from_secs(10),
    });
    let mut controls = Controls::default();
    let (reply, mut result) = tokio::sync::oneshot::channel();
    assert!(
        controls
            .admit(
                Command::Wait {
                    deadline: start + Duration::from_millis(50),
                    settling: Default::default(),
                    reply,
                    identity: None
                },
                &watcher
            )
            .is_none()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(5), &mut result)
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(5), controls.expired())
            .await
            .is_err()
    );
    let (reply, _receive) = tokio::sync::oneshot::channel();
    assert!(matches!(
        controls.admit(
            Command::Approve {
                revision: "stale".into(),
                reply,
                identity: None
            },
            &watcher
        ),
        Some(Command::Approve { .. })
    ));
    controls.expired().await;
    controls.expire(&root, &config).await.unwrap();
    assert_eq!(
        result.await.unwrap().unwrap().reason,
        crate::sync::control::WaitReason::SourceChanging
    );
    assert!(start.elapsed() >= Duration::from_millis(50));
    assert!(controls.0.is_empty());
    drop(watcher);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn quiet_zero_candidate_scans_reset_small_repeated_deletions_across_restart() {
    use std::fs;
    let (data, tree, mut catalog, mut root, config) =
        crate::sync::test_support::fixture(None).await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for index in 0..50 {
        fs::write(tree.0.join(format!("test-bucket/{index:03}")), b"body").unwrap();
    }
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let mut writeback = root.default_writeback().await.unwrap();
    let mut gate = BaselineGate::Deferred;
    let mut baseline = false;
    catalog.enable_change_journal().await.unwrap();
    ingest_external(
        &catalog,
        &root,
        &config,
        &writeback,
        &stopping,
        &mut gate,
        &mut baseline,
    )
    .await
    .unwrap();
    for round in 0..6 {
        for index in 0..20 {
            fs::remove_file(tree.0.join(format!("test-bucket/{index:03}"))).unwrap();
        }
        let summary = ingest_external(
            &catalog,
            &root,
            &config,
            &writeback,
            &stopping,
            &mut gate,
            &mut baseline,
        )
        .await
        .unwrap();
        reset_history(&root, Quiet::Established, &summary)
            .await
            .unwrap();
        assert_eq!(root.automatic_deletion_count().await.unwrap(), 20);
        assert!(!root.deletion_set().await.unwrap().unwrap().guarded);
        let summary = ingest_external(
            &catalog,
            &root,
            &config,
            &writeback,
            &stopping,
            &mut gate,
            &mut baseline,
        )
        .await
        .unwrap();
        assert_eq!(
            root.deletion_set().await.unwrap().unwrap().candidate_count,
            0
        );
        reset_history(&root, Quiet::Unverified, &summary)
            .await
            .unwrap();
        assert_eq!(root.automatic_deletion_count().await.unwrap(), 20);
        reset_history(&root, Quiet::Established, &summary)
            .await
            .unwrap();
        assert_eq!(root.automatic_deletion_count().await.unwrap(), 0);
        for index in 0..20 {
            fs::write(tree.0.join(format!("test-bucket/{index:03}")), b"body").unwrap();
        }
        ingest_external(
            &catalog,
            &root,
            &config,
            &writeback,
            &stopping,
            &mut gate,
            &mut baseline,
        )
        .await
        .unwrap();
        if round == 2 {
            drop(writeback);
            drop(root);
            catalog.close().await.unwrap();
            catalog = Catalog::open(&data.0).await.unwrap();
            catalog.enable_change_journal().await.unwrap();
            root = catalog
                .sync_state()
                .activate_root(&tree.0, crate::storage::sync_state::root::Activation::Open)
                .await
                .unwrap();
            writeback = root.default_writeback().await.unwrap();
        }
    }
    drop(writeback);
    drop(root);
    catalog.close().await.unwrap();
}

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
    assert!(matches!(
        result,
        Observed::Settled(Change::Structure, Quiet::Unverified)
    ));
    assert!(watcher.pending.is_none());
    drop(watcher);
    drop(root);
    catalog.close().await.unwrap();
}
