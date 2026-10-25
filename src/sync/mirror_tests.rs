#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::{
    clock::SystemClock,
    object::UploadChunk,
    storage::admin::{BucketDefinition, GenerationCondition, Publication, PublishMode},
    test_support::TestDir,
};
use bytes::Bytes;

async fn publish(catalog: &Catalog, bucket: &BucketName, body: &[u8]) -> ObjectRecord {
    publish_origin(catalog, bucket, body, Origin::SyncIngest).await
}

async fn publish_origin(
    catalog: &Catalog,
    bucket: &BucketName,
    body: &[u8],
    origin: Origin,
) -> ObjectRecord {
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    tx.try_send(Ok(UploadChunk::Data(Bytes::copy_from_slice(body))))
        .unwrap();
    tx.try_send(Ok(UploadChunk::End)).unwrap();
    catalog
        .admin(origin)
        .publish_object(
            bucket,
            &ObjectName::new("object").unwrap(),
            Publication {
                content_type: "application/octet-stream".into(),
                metadata: Default::default(),
                expected: Default::default(),
                condition: None,
                max_bytes: 4096,
                mode: PublishMode::Replace,
            },
            rx,
        )
        .await
        .unwrap()
}

async fn live_fixture() -> (TestDir, TestDir, Catalog, BucketName, RootLease) {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    catalog
        .admin(Origin::Api)
        .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
        .await
        .unwrap();
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    let root = crate::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    (data, tree, catalog, bucket, root)
}

#[tokio::test]
async fn live_export_retries_identical_observation_at_prepare_arm_durable_and_ack() {
    use std::{sync::Arc, time::Duration};
    for phase in ["prepare", "arm", "durable", "ack"] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        publish_origin(&catalog, &bucket, b"same", Origin::Api).await;
        let other = BucketName::new("zz-other").unwrap();
        catalog
            .admin(Origin::Api)
            .ensure_bucket(BucketDefinition::default_for(&other, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let hook = Arc::new(ExportHook {
            phase: "idle",
            entered: Default::default(),
            release: Default::default(),
        });
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let worker = tokio::spawn(EXPORT_HOOK.scope(hook.clone(), async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        }));
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        crate::storage::sync_state::writeback::inject_live_export_failure(
            root.status().root_id,
            phase,
            1,
        );
        let record = publish_origin(&catalog, &bucket, b"same", Origin::Api).await;
        publish_origin(&catalog, &other, b"other", Origin::Api).await;
        let started = tokio::time::Instant::now();
        hook.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "{phase} must retry"
        );
        let writeback = root
            .writeback(Limits::new(4096, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(writeback.status().await.unwrap().stop, Stop::Ready);
        assert!(writeback.page(0, 256).await.unwrap().is_empty());
        assert_eq!(
            std::fs::read(tree.0.join("zz-other/object")).unwrap(),
            b"other"
        );
        let binding = catalog.sync_state().binding(&bucket).await.unwrap();
        assert_eq!(
            catalog
                .sync_state()
                .get_path(binding, "test-bucket/object".into())
                .await
                .unwrap()
                .unwrap()
                .generation
                .get(),
            record.generation
        );
        crate::storage::sync_state::writeback::inject_live_export_failure(
            root.status().root_id,
            phase,
            1,
        );
        let patched = catalog
            .update_object_metadata(
                bucket.as_str(),
                "object",
                None,
                Default::default(),
                crate::object::ObjectMetadataUpdate {
                    content_type: Some(Some("text/plain".into())),
                    ..Default::default()
                },
                |_| Ok(()),
            )
            .await
            .unwrap();
        let before = std::fs::metadata(tree.0.join("test-bucket/object")).unwrap();
        let started = tokio::time::Instant::now();
        hook.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "metadata {phase} must retry"
        );
        let binding = catalog.sync_state().binding(&bucket).await.unwrap();
        let tracked = catalog
            .sync_state()
            .get_path(binding, "test-bucket/object".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tracked.generation.get(), patched.generation);
        assert_eq!(
            catalog
                .admin(Origin::Api)
                .get_object(&bucket, &ObjectName::new("object").unwrap(), None)
                .await
                .unwrap(),
            patched
        );
        assert_eq!(
            std::fs::metadata(tree.0.join("test-bucket/object"))
                .unwrap()
                .modified()
                .unwrap(),
            before.modified().unwrap()
        );
        assert_eq!(writeback.status().await.unwrap().stop, Stop::Ready);
        publish_origin(&catalog, &bucket, b"later", Origin::Api).await;
        hook.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"later"
        );
        stop.send_replace(true);
        hook.release.notify_one();
        worker.await.unwrap().unwrap();
        drop(writeback);
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn live_export_identical_observation_preserves_edits_before_durable_and_ack() {
    for durable in [false, true] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        let record = publish_origin(&catalog, &bucket, b"same", Origin::Api).await;
        catalog.set_change_journal_enabled(true).await.unwrap();
        let fence = catalog.read_changes(None, None, 1).await.unwrap().fence;
        let binding = catalog.sync_state().binding(&bucket).await.unwrap();
        let writeback = root
            .writeback(Limits::new(4096, 10).unwrap())
            .await
            .unwrap();
        std::fs::create_dir(tree.0.join("test-bucket")).unwrap();
        let path = tree.0.join("test-bucket/object");
        std::fs::write(&path, b"same").unwrap();
        let operation = writeback
            .begin_live_export(
                binding,
                ObjectName::new("object").unwrap(),
                Generation::new(record.generation).unwrap(),
                None,
                fence,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            writeback
                .advance_live_export(operation.clone())
                .await
                .unwrap(),
            LiveResult::Pending
        );
        if durable {
            assert_eq!(
                writeback
                    .advance_live_export(operation.clone())
                    .await
                    .unwrap(),
                LiveResult::Pending
            );
        }
        std::fs::write(&path, b"external").unwrap();
        assert_eq!(
            writeback.advance_live_export(operation).await.unwrap(),
            LiveResult::Conflict
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
        assert_eq!(writeback.status().await.unwrap().stop, Stop::Ready);
        assert!(writeback.page(0, 256).await.unwrap().is_empty());
        drop(writeback);
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn live_export_identical_parent_disappearance_cancels_and_keeps_other_buckets_progressing() {
    use std::{sync::Arc, time::Duration};
    for moved in [false, true] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        let other = BucketName::new("zz-other").unwrap();
        catalog
            .admin(Origin::Api)
            .ensure_bucket(BucketDefinition::default_for(&other, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let hook = Arc::new(ExportHook {
            phase: "idle,after-prepare",
            entered: Default::default(),
            release: Default::default(),
        });
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let worker = tokio::spawn(EXPORT_HOOK.scope(hook.clone(), async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        }));
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        let path = tree.0.join("test-bucket/object");
        std::fs::write(&path, b"same").unwrap();
        publish_origin(&catalog, &bucket, b"same", Origin::Api).await;
        publish_origin(&catalog, &other, b"other", Origin::Api).await;
        hook.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        if moved {
            std::fs::rename(tree.0.join("test-bucket"), tree.0.join("moved")).unwrap();
        } else {
            std::fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
        }
        hook.release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), hook.entered.notified())
            .await
            .unwrap();
        hook.release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), hook.entered.notified())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(tree.0.join("zz-other/object")).unwrap(),
            b"other"
        );
        assert!(!path.exists());
        if moved {
            assert_eq!(std::fs::read(tree.0.join("moved/object")).unwrap(), b"same");
        }
        let writeback = root
            .writeback(Limits::new(4096, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(writeback.status().await.unwrap().stop, Stop::Ready);
        assert!(writeback.page(0, 256).await.unwrap().is_empty());
        stop.send_replace(true);
        hook.release.notify_one();
        worker.await.unwrap().unwrap();
        drop(writeback);
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn live_export_retries_file_and_parent_permissions_without_advancing_the_cursor() {
    use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
    for parent in [false, true] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        publish_origin(&catalog, &bucket, b"before", Origin::Api).await;
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let hook = Arc::new(ExportHook {
            phase: "idle",
            entered: Default::default(),
            release: Default::default(),
        });
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let worker = tokio::spawn(EXPORT_HOOK.scope(hook.clone(), async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        }));
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        let path = tree.0.join(if parent {
            "test-bucket"
        } else {
            "test-bucket/object"
        });
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
        publish_origin(&catalog, &bucket, b"after", Origin::Api).await;
        hook.release.notify_one();
        let advanced =
            tokio::time::timeout(Duration::from_millis(100), hook.entered.notified()).await;
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(if parent { 0o700 } else { 0o600 }),
        )
        .unwrap();
        assert!(
            advanced.is_err(),
            "permission failure must not become a durable finding"
        );
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"after"
        );
        stop.send_replace(true);
        hook.release.notify_one();
        worker.await.unwrap().unwrap();
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn live_export_bucket_incarnation_races_keep_other_buckets_progressing() {
    use std::{sync::Arc, time::Duration};
    for phase in ["before-binding", "after-binding", "after-get-path"] {
        for recreate in [false, true] {
            let (_data, tree, catalog, bucket, root) = live_fixture().await;
            let other = BucketName::new("zz-other").unwrap();
            catalog
                .admin(Origin::Api)
                .ensure_bucket(
                    BucketDefinition::default_for(&other, "owner", &SystemClock).unwrap(),
                )
                .await
                .unwrap();
            let (stop, mut stopping) = tokio::sync::watch::channel(false);
            let phases = match phase {
                "before-binding" => "idle,before-binding",
                "after-binding" => "idle,after-binding",
                _ => "idle,after-get-path",
            };
            let hook = Arc::new(ExportHook {
                phase: phases,
                entered: Default::default(),
                release: Default::default(),
            });
            let worker_catalog = catalog.clone();
            let worker_root = root.clone();
            let worker = tokio::spawn(EXPORT_HOOK.scope(hook.clone(), async move {
                run_live(&worker_catalog, &worker_root, &mut stopping).await
            }));
            tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
                .await
                .unwrap();
            let record = publish_origin(&catalog, &bucket, b"obsolete", Origin::Api).await;
            publish_origin(&catalog, &other, b"other", Origin::Api).await;
            hook.release.notify_one();
            tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
                .await
                .unwrap();
            catalog
                .admin(Origin::Api)
                .delete_object(
                    &bucket,
                    &ObjectName::new("object").unwrap(),
                    GenerationCondition::Is(Generation::new(record.generation).unwrap()),
                )
                .await
                .unwrap();
            catalog
                .delete_bucket(bucket.as_str(), None, None)
                .await
                .unwrap();
            if recreate {
                catalog
                    .admin(Origin::Api)
                    .ensure_bucket(
                        BucketDefinition::default_for(&bucket, "new-owner", &SystemClock).unwrap(),
                    )
                    .await
                    .unwrap();
            }
            hook.release.notify_one();
            // The next key belongs to another bucket in the same committed page.
            tokio::time::timeout(Duration::from_secs(3), hook.entered.notified())
                .await
                .unwrap();
            hook.release.notify_one();
            tokio::time::timeout(Duration::from_secs(3), hook.entered.notified())
                .await
                .unwrap();
            assert_eq!(
                std::fs::read(tree.0.join("zz-other/object")).unwrap(),
                b"other",
                "{phase} recreate={recreate}"
            );
            stop.send_replace(true);
            hook.release.notify_one();
            worker.await.unwrap().unwrap();
            assert!(!tree.0.join("test-bucket/object").exists());
            drop(root);
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn live_export_coalesces_pages_recovers_retention_gaps_and_sleeps_without_polling() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    for gap in [false, true] {
        let data = TestDir::new();
        let tree = TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let bucket = BucketName::new("test-bucket").unwrap();
        catalog
            .admin(Origin::Api)
            .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        let config = Config {
            data_dir: data.0.clone(),
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        };
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let hook = Arc::new(ExportHook {
            phase: "idle",
            entered: Default::default(),
            release: Default::default(),
        });
        let count = Arc::new(AtomicUsize::new(0));
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let worker = tokio::spawn(EXPORT_HOOK.scope(
            hook.clone(),
            LIVE_EXPORTS.scope(count.clone(), async move {
                run_live(&worker_catalog, &worker_root, &mut stopping).await
            }),
        ));
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        for n in 0..300 {
            publish_origin(&catalog, &bucket, n.to_string().as_bytes(), Origin::Api).await;
        }
        if gap {
            let mut connection =
                rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
            let tx = connection.transaction().unwrap();
            tx.execute("DELETE FROM change_journal", []).unwrap();
            tx.execute("UPDATE change_journal_state SET floor=sequence", [])
                .unwrap();
            tx.commit().unwrap();
        }
        hook.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"299"
        );
        hook.release.notify_one();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), hook.entered.notified())
                .await
                .is_err()
        );
        stop.send_replace(true);
        worker.await.unwrap().unwrap();
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn live_export_retries_write_failures_at_copy_install_sync_and_catalog_ack() {
    use std::{sync::Arc, time::Duration};
    for (phase, remove) in [
        ("copy", false),
        ("install", false),
        ("installed", false),
        ("ack", false),
        ("install", true),
        ("installed", true),
        ("ack", true),
        ("cancel", false),
    ] {
        let data = TestDir::new();
        let tree = TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let bucket = BucketName::new("test-bucket").unwrap();
        catalog
            .admin(Origin::Api)
            .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        publish_origin(&catalog, &bucket, b"before", Origin::Api).await;
        let config = Config {
            data_dir: data.0.clone(),
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        };
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let hook = Arc::new(ExportHook {
            phase: "idle",
            entered: Default::default(),
            release: Default::default(),
        });
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let worker = tokio::spawn(EXPORT_HOOK.scope(hook.clone(), async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        }));
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        crate::storage::sync_state::writeback::inject_live_export_failure(
            root.status().root_id,
            phase,
            1,
        );
        if phase == "cancel" {
            std::fs::write(tree.0.join("test-bucket/object"), b"external").unwrap();
        }
        let record = if remove {
            let record = catalog
                .admin(Origin::Api)
                .get_object(&bucket, &ObjectName::new("object").unwrap(), None)
                .await
                .unwrap();
            catalog
                .admin(Origin::Api)
                .delete_object(
                    &bucket,
                    &ObjectName::new("object").unwrap(),
                    GenerationCondition::Is(Generation::new(record.generation).unwrap()),
                )
                .await
                .unwrap();
            record
        } else {
            publish_origin(&catalog, &bucket, b"after\0\xff", Origin::Api).await
        };
        hook.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        if remove {
            assert!(!tree.0.join("test-bucket/object").exists());
        } else if phase == "cancel" {
            assert_eq!(
                std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
                b"external"
            );
        } else {
            assert_eq!(
                std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
                b"after\0\xff",
                "{phase}"
            );
        }
        let binding = catalog.sync_state().binding(&bucket).await.unwrap();
        let tracked = catalog
            .sync_state()
            .get_path(binding, "test-bucket/object".into())
            .await
            .unwrap();
        if remove {
            assert!(tracked.is_none());
        } else if phase == "cancel" {
            assert_ne!(tracked.unwrap().generation.get(), record.generation);
        } else {
            assert_eq!(tracked.unwrap().generation.get(), record.generation);
        }
        let writeback = root
            .writeback(
                Limits::new(
                    super::super::CONFLICT_MAX_BYTES,
                    super::super::CONFLICT_MAX_ENTRIES,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(writeback.status().await.unwrap().stop, Stop::Ready);
        assert_eq!(
            std::fs::read_dir(tree.0.join(mapping::WORKSPACE))
                .unwrap()
                .count(),
            0
        );
        let opened = catalog
            .admin(Origin::Api)
            .open_object(&bucket, &ObjectName::new("object").unwrap(), None)
            .await;
        if remove {
            assert!(matches!(opened, Err(AdminError::NotFound)));
        } else {
            let (_, reader) = opened.unwrap();
            let file = reader.into_file().await;
            let mut body = [0; 8];
            assert_eq!(fsutil::read_at(&file, &mut body, 0).unwrap(), 7);
            assert_eq!(&body[..7], b"after\0\xff");
        }
        stop.send_replace(true);
        hook.release.notify_one();
        worker.await.unwrap().unwrap();
        drop(writeback);
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn export_concurrent_api_and_external_changes_never_strand_unpublished_operations() {
    for (phase, action) in [
        ("before-open", "overwrite"),
        ("before-open", "delete"),
        ("before-arm", "overwrite"),
        ("before-arm", "delete"),
        ("before-publish", "external-create"),
        ("before-durable", "external-edit"),
        ("before-durable", "overwrite"),
        ("before-durable", "delete"),
    ] {
        let data = TestDir::new();
        let tree = TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let bucket = BucketName::new("test-bucket").unwrap();
        let admin = catalog.admin(Origin::SyncIngest);
        admin
            .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        let record = publish(&catalog, &bucket, b"api").await;
        let config = Config {
            data_dir: data.0.clone(),
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        };
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        catalog.set_change_journal_enabled(true).await.unwrap();
        let fence = catalog.read_changes(None, None, 1).await.unwrap().fence;
        let writeback = root
            .writeback(Limits::new(4096, 10).unwrap())
            .await
            .unwrap();
        let path = tree.0.join("test-bucket/object");
        std::fs::create_dir(tree.0.join("test-bucket")).unwrap();
        let identical = action == "external-edit";
        if identical {
            std::fs::write(&path, b"api").unwrap();
        }
        let hook = std::sync::Arc::new(ExportHook {
            phase,
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let name = ObjectName::new("object").unwrap();
        let output = (!identical).then(|| PathBuf::from("test-bucket/object"));
        let (result, ()) = tokio::join!(
            EXPORT_HOOK.scope(
                hook.clone(),
                export(&admin, &writeback, &bucket, &name, output, &record, fence)
            ),
            async {
                hook.entered.notified().await;
                match action {
                    "overwrite" => {
                        publish(&catalog, &bucket, b"new-api").await;
                    }
                    "delete" => {
                        admin
                            .delete_object(
                                &bucket,
                                &name,
                                GenerationCondition::Is(
                                    Generation::new(record.generation).unwrap(),
                                ),
                            )
                            .await
                            .unwrap();
                    }
                    _ => std::fs::write(&path, b"external").unwrap(),
                }
                hook.release.notify_one();
            }
        );
        assert!(!result.unwrap(), "{phase} {action}");
        assert_eq!(
            writeback.status().await.unwrap().stop,
            Stop::Ready,
            "{phase} {action}"
        );
        assert!(writeback.page(0, 256).await.unwrap().is_empty());
        if action.starts_with("external") {
            assert_eq!(std::fs::read(&path).unwrap(), b"external");
        }
        if action == "overwrite" {
            let (_, reader) = admin.open_object(&bucket, &name, None).await.unwrap();
            let mut file = reader.into_file().await;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
            assert_eq!(bytes, b"new-api");
        }
        drop(writeback);
        drop(root);
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        reconcile(&catalog, &root, &config).await.unwrap();
        drop(root);
        catalog.close().await.unwrap();
    }
}
