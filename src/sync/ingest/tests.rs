#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::{
    object::{ObjectMetadataUpdate, UploadChunk},
    test_support::TestDir,
};
use std::{collections::BTreeMap, fs, sync::Arc, time::Duration};

async fn fixture() -> (TestDir, TestDir, Catalog, RootLease, Config) {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    let root = crate::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    (data, tree, catalog, root, config)
}

async fn scan(catalog: &Catalog, root: &RootLease, config: &Config) -> Reconciliation {
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    rescan(catalog, root, config, &stopping).await.unwrap()
}

async fn object(catalog: &Catalog, path: &str) -> crate::object::ObjectRecord {
    let (bucket, name) = mapping::path_to_name(std::path::Path::new(path)).unwrap();
    catalog
        .admin(Origin::Api)
        .get_object(&bucket, &name, None)
        .await
        .unwrap()
}

async fn api_put(catalog: &Catalog, path: &str, bytes: &'static [u8]) {
    let (bucket, name) = mapping::path_to_name(std::path::Path::new(path)).unwrap();
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(bytes::Bytes::from_static(bytes))))
        .await
        .unwrap();
    sender.send(Ok(UploadChunk::End)).await.unwrap();
    catalog
        .admin(Origin::Api)
        .publish_object(
            &bucket,
            &name,
            Publication {
                content_type: "application/api".into(),
                metadata: Default::default(),
                expected: Default::default(),
                condition: None,
                max_bytes: 4096,
                mode: PublishMode::Replace,
            },
            receiver,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn ingest_initial_baseline_runtime_rescan_and_restart_preserve_generations() {
    let (data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("first-bucket")).unwrap();
    fs::create_dir(tree.0.join("second-bucket")).unwrap();
    fs::write(tree.0.join("first-bucket/zero"), b"").unwrap();
    fs::write(tree.0.join("second-bucket/body"), [0, 255, 13, 10]).unwrap();
    let task = crate::sync::start(catalog.clone(), root.clone(), &config).unwrap();
    let unchanged = task.rescan().await.unwrap();
    assert_eq!(unchanged.ingested, 0);
    let original = object(&catalog, "second-bucket/body").await;
    let unchanged = task.rescan().await.unwrap();
    assert_eq!((unchanged.ingested, unchanged.identical), (0, 0));
    assert_eq!(original.size, 4);
    assert_eq!(object(&catalog, "first-bucket/zero").await.size, 0);
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_bucket(&BucketName::new("first-bucket").unwrap())
            .await
            .unwrap()
            .unwrap()
            .project,
        config.seed_project
    );
    fs::rename(
        tree.0.join("second-bucket/body"),
        tree.0.join("second-bucket/renamed"),
    )
    .unwrap();
    assert_eq!(task.rescan().await.unwrap().ingested, 1);
    assert_eq!(
        object(&catalog, "second-bucket/body").await.generation,
        original.generation
    );
    assert_eq!(
        object(&catalog, "second-bucket/renamed").await.md5,
        original.md5
    );
    task.finish().await.unwrap();
    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = crate::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    assert!(root.status().permits_external_deletions());
    assert_eq!(scan(&catalog, &root, &config).await.ingested, 0);
    assert_eq!(
        object(&catalog, "second-bucket/body").await.generation,
        original.generation
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_external_only_conflicts_equal_hash_and_unsafe_entries() {
    let (_data, tree, catalog, root, config) = fixture().await;
    catalog
        .admin(Origin::Api)
        .ensure_bucket(
            crate::storage::admin::BucketDefinition::default_for(
                &BucketName::new("test-bucket").unwrap(),
                "existing-owner",
                &crate::clock::SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    api_put(&catalog, "test-bucket/shared", b"api").await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/shared"), b"external").unwrap();
    fs::write(tree.0.join("test-bucket/only"), b"new").unwrap();
    for name in ["4913", "file.swp", "file~", ".DS_Store", "._file"] {
        fs::write(tree.0.join("test-bucket").join(name), b"ignore").unwrap();
    }
    std::os::unix::fs::symlink("only", tree.0.join("test-bucket/link")).unwrap();
    let summary = scan(&catalog, &root, &config).await;
    assert_eq!(
        (
            summary.ingested,
            summary.conflicting,
            summary.ignored,
            summary.unrepresentable
        ),
        (1, 1, 5, 1)
    );
    assert_eq!(
        fs::read(tree.0.join("test-bucket/shared")).unwrap(),
        b"external"
    );
    assert_eq!(root.findings_page(0, 256).await.unwrap().len(), 1);
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_bucket(&BucketName::new("test-bucket").unwrap())
            .await
            .unwrap()
            .unwrap()
            .project,
        "existing-owner"
    );
    let generation = object(&catalog, "test-bucket/only").await.generation;
    fs::write(tree.0.join("test-bucket/only"), b"new").unwrap();
    assert_eq!(scan(&catalog, &root, &config).await.ingested, 0);
    assert_eq!(
        object(&catalog, "test-bucket/only").await.generation,
        generation
    );
    fs::remove_file(tree.0.join("test-bucket/only")).unwrap();
    scan(&catalog, &root, &config).await;
    assert_eq!(
        object(&catalog, "test-bucket/only").await.generation,
        generation
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_preserves_metadata_patch_and_api_body_racing_publication() {
    for body_race in [false, true] {
        let (_data, tree, catalog, root, config) = fixture().await;
        fs::create_dir(tree.0.join("test-bucket")).unwrap();
        fs::write(tree.0.join("test-bucket/object"), b"old").unwrap();
        scan(&catalog, &root, &config).await;
        fs::write(tree.0.join("test-bucket/object"), b"external-edit").unwrap();
        let hook = Arc::new(Hook {
            phase: "publish",
            entered: Default::default(),
            release: Default::default(),
        });
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let worker_config = config.clone();
        let worker = tokio::spawn(HOOK.scope(hook.clone(), async move {
            scan(&worker_catalog, &worker_root, &worker_config).await
        }));
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        if body_race {
            api_put(&catalog, "test-bucket/object", b"api-winner").await;
        } else {
            catalog
                .update_object_metadata(
                    "test-bucket",
                    "object",
                    None,
                    Default::default(),
                    ObjectMetadataUpdate {
                        content_type: Some(Some("application/patched".into())),
                        metadata: Some(Some(BTreeMap::from([(
                            "key".into(),
                            Some("patched".into()),
                        )]))),
                        ..Default::default()
                    },
                    |_| Ok(()),
                )
                .await
                .unwrap();
        }
        hook.release.notify_one();
        let summary = worker.await.unwrap();
        let result = object(&catalog, "test-bucket/object").await;
        if body_race {
            assert_eq!(summary.ingested, 0);
            assert_eq!(result.size, 10);
            assert_eq!(scan(&catalog, &root, &config).await.conflicting, 1);
        } else {
            assert_eq!(summary.ingested, 1);
            assert_eq!(result.content_type, "application/patched");
            assert_eq!(result.metadata["key"], "patched");
        }
        assert_eq!(
            fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"external-edit"
        );
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn ingest_defers_partial_and_replacement_saves_then_resumes_activation() {
    let (data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/object"), b"partial").unwrap();
    let worker_catalog = catalog.clone();
    let worker_root = root.clone();
    let worker_config = config.clone();
    let hook = Arc::new(Hook {
        phase: "settle",
        entered: Default::default(),
        release: Default::default(),
    });
    let worker = tokio::spawn(HOOK.scope(hook.clone(), async move {
        scan(&worker_catalog, &worker_root, &worker_config).await
    }));
    tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
        .await
        .unwrap();
    fs::write(tree.0.join("test-bucket/object~"), b"completed").unwrap();
    fs::rename(
        tree.0.join("test-bucket/object~"),
        tree.0.join("test-bucket/object"),
    )
    .unwrap();
    hook.release.notify_one();
    assert_eq!(worker.await.unwrap().deferred, 1);
    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = crate::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan(&catalog, &root, &config).await.ingested, 1);
    assert_eq!(object(&catalog, "test-bucket/object").await.size, 9);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_new_files_use_seed_mime_and_bucket_default_hold() {
    let (_data, tree, catalog, root, mut config) = fixture().await;
    config.seed_content_type = crate::mime::SeedContentType::Extension;
    let record = crate::json::parse_insert(
        br#"{"name":"test-bucket","defaultEventBasedHold":true}"#,
        "owner",
        std::time::SystemTime::UNIX_EPOCH,
    )
    .unwrap();
    catalog
        .admin(Origin::Api)
        .ensure_bucket(crate::storage::admin::BucketDefinition::new(record).unwrap())
        .await
        .unwrap();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/file.txt"), b"held").unwrap();
    assert_eq!(scan(&catalog, &root, &config).await.ingested, 1);
    let record = object(&catalog, "test-bucket/file.txt").await;
    assert_eq!(record.content_type, "text/plain; charset=utf-8");
    assert!(record.event_based_hold);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_shutdown_finishes_the_active_publication_before_restart() {
    let (data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/first"), b"first").unwrap();
    fs::write(tree.0.join("test-bucket/second"), b"second").unwrap();
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let hook = Arc::new(Hook {
        phase: "publish",
        entered: Default::default(),
        release: Default::default(),
    });
    let worker_catalog = catalog.clone();
    let worker_root = root.clone();
    let worker_config = config.clone();
    let worker = tokio::spawn(HOOK.scope(hook.clone(), async move {
        rescan(&worker_catalog, &worker_root, &worker_config, &stopping)
            .await
            .unwrap()
    }));
    tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
        .await
        .unwrap();
    stop.send_replace(true);
    hook.release.notify_one();
    assert_eq!(worker.await.unwrap().ingested, 1);
    let first = object(&catalog, "test-bucket/first").await;
    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = crate::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    assert!(!root.status().permits_external_deletions());
    let task = crate::sync::start(catalog.clone(), root.clone(), &config).unwrap();
    assert!(task.rescan().await.unwrap().baseline_complete);
    task.finish().await.unwrap();
    assert_eq!(
        object(&catalog, "test-bucket/first").await.generation,
        first.generation
    );
    assert_eq!(fs::read_dir(data.0.join("staging")).unwrap().count(), 0);
    drop(root);
    catalog.close().await.unwrap();
}

#[test]
fn ingest_strict_configuration_has_actionable_remedy() {
    let tree = TestDir::new();
    let data = TestDir::new();
    let errors = Config::load(
        &[
            "--sync-dir".into(),
            tree.0.to_str().unwrap().into(),
            "--data-dir".into(),
            data.0.to_str().unwrap().into(),
            "--auth-mode".into(),
            "strict".into(),
            "--identity-file".into(),
            "identities.json".into(),
        ],
        &BTreeMap::new(),
    )
    .unwrap_err();
    assert!(
        errors
            .errors
            .iter()
            .any(|error| error.fix.contains("permissive") && error.fix.contains("remove sync_dir"))
    );
}

#[tokio::test]
async fn ingest_deleted_bucket_stays_deleted_after_restart_until_directory_recreated() {
    let (data, tree, catalog, root, config) = fixture().await;
    let bucket = BucketName::new("deleted-bucket").unwrap();
    catalog
        .admin(Origin::Api)
        .ensure_bucket(
            crate::storage::admin::BucketDefinition::default_for(
                &bucket,
                "original-owner",
                &crate::clock::SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    crate::sync::reconcile(&catalog, &root, &config)
        .await
        .unwrap();
    catalog
        .delete_bucket(bucket.as_str(), None, None)
        .await
        .unwrap();
    fs::write(tree.0.join("deleted-bucket/retained"), b"do not import").unwrap();
    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = crate::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    let task = crate::sync::start(catalog.clone(), root.clone(), &config).unwrap();
    let summary = task.rescan().await.unwrap();
    assert_eq!(summary.blocked_buckets, 1);
    assert!(
        catalog
            .admin(Origin::Api)
            .get_bucket(&bucket)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        root.sync_buckets_page(String::new(), 256)
            .await
            .unwrap()
            .len(),
        1
    );
    fs::create_dir(tree.0.join("new-bucket")).unwrap();
    fs::write(tree.0.join("new-bucket/body"), b"new").unwrap();
    assert_eq!(task.rescan().await.unwrap().ingested, 1);
    fs::remove_file(tree.0.join("deleted-bucket/retained")).unwrap();
    fs::remove_dir(tree.0.join("deleted-bucket")).unwrap();
    assert_eq!(task.rescan().await.unwrap().blocked_buckets, 0);
    fs::create_dir(tree.0.join("deleted-bucket")).unwrap();
    assert_eq!(task.rescan().await.unwrap().blocked_buckets, 0);
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_bucket(&bucket)
            .await
            .unwrap()
            .unwrap()
            .project,
        config.seed_project
    );
    task.finish().await.unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_unreadable_file_is_deferred_while_other_files_progress() {
    use std::os::unix::fs::PermissionsExt;
    let (_data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    let blocked = tree.0.join("test-bucket/blocked");
    fs::write(&blocked, b"protected").unwrap();
    fs::write(tree.0.join("test-bucket/working"), b"working").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let summary = scan(&catalog, &root, &config).await;
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!((summary.ingested, summary.deferred), (1, 1));
    assert_eq!(object(&catalog, "test-bucket/working").await.size, 7);
    assert_eq!(scan(&catalog, &root, &config).await.ingested, 1);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_unchanged_files_skip_settle_and_shutdown_interrupts_changed_settle() {
    let (_data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/object"), b"stable").unwrap();
    scan(&catalog, &root, &config).await;
    let hook = Arc::new(Hook {
        phase: "settle",
        entered: Default::default(),
        release: Default::default(),
    });
    let summary = tokio::time::timeout(
        Duration::from_secs(5),
        HOOK.scope(hook, scan(&catalog, &root, &config)),
    )
    .await
    .unwrap();
    assert_eq!((summary.ingested, summary.identical), (0, 0));
    fs::write(tree.0.join("test-bucket/object"), b"changed").unwrap();
    let hook = Arc::new(Hook {
        phase: "settle",
        entered: Default::default(),
        release: Default::default(),
    });
    let (stop, stopping) = tokio::sync::watch::channel(false);
    {
        let work = HOOK.scope(hook.clone(), rescan(&catalog, &root, &config, &stopping));
        tokio::pin!(work);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut work => panic!("expected a pending quiet checkpoint: {result:?}"),
                _ = hook.entered.notified() => {},
            }
        })
        .await
        .unwrap();
        hook.release.notify_one();
        // Drive the released checkpoint into the quiet wait before signalling shutdown.
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(work.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        stop.send_replace(true);
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(500), &mut work)
                .await
                .unwrap()
                .unwrap()
                .ingested,
            0
        );
    }
    assert_eq!(object(&catalog, "test-bucket/object").await.size, 6);
    drop(root);
    catalog.close().await.unwrap();
}
