#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::*;
use crate::storage::sync_state::conflicts::ConflictTarget;
use crate::storage::test_fixtures::{PERMISSIVE, live};
use crate::sync::test_support::hooked_worker;
use crate::{
    clock::SystemClock,
    object::UploadChunk,
    storage::admin::{BucketDefinition, GenerationCondition, Publication, PublishMode},
    test_support::{HANG_GUARD, TestDir, eventually_async},
};
use bytes::Bytes;
use std::fs;

async fn run_live(
    catalog: &Catalog,
    root: &RootLease,
    stopping: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(), SyncError> {
    let (_sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let health = crate::task::TaskHealth::default();
    let config = Config::default();
    run_live_config(
        LiveRun {
            catalog,
            root,
            config: &config,
            health: &health,
            resume: None,
        },
        stopping,
        &mut receiver,
        &mut observation::Controls::default(),
    )
    .await
}

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
    live_fixture_with("test-bucket").await
}

async fn live_fixture_with(name: &str) -> (TestDir, TestDir, Catalog, BucketName, RootLease) {
    let bucket = BucketName::new(name).unwrap();
    let (data, tree, catalog, root, _) = crate::sync::test_support::fixture(Some(&bucket)).await;
    (data, tree, catalog, bucket, root)
}

#[tokio::test]
async fn external_inventory_defers_unreadable_children_without_stopping_other_files() {
    use std::os::unix::fs::PermissionsExt;
    let (_data, tree, catalog, _bucket, root) = live_fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    let blocked = tree.0.join("test-bucket/blocked");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("hidden"), b"hidden").unwrap();
    fs::write(tree.0.join("test-bucket/working"), b"working").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let counts = external_files(&root, &root.sync_admin(), Interrupt::new(&stopping)).await;
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(counts.unwrap(), (1, 0, 1));
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn quiet_native_observation_keeps_split_bulk_deletions_pending_and_answers_controls() {
    let (_data, tree, catalog, _bucket, root) = live_fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for index in 0..100 {
        fs::write(tree.0.join(format!("test-bucket/{index:03}")), b"body").unwrap();
    }
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let (commands, mut requests) = tokio::sync::mpsc::channel(2);
    let worker_root = root.clone();
    let worker_catalog = catalog.clone();
    let hook = std::sync::Arc::new(Hook {
        phase: "idle,observation-invalidated",
        entered: Default::default(),
        release: Default::default(),
    });
    let worker = tokio::spawn(EXPORT_HOOK.scope(hook.clone(), async move {
        let mut controls = observation::Controls::default();
        let result = run_live_config(
            LiveRun {
                catalog: &worker_catalog,
                root: &worker_root,
                config: &Config::default(),
                health: &crate::task::TaskHealth::default(),
                resume: None,
            },
            &mut stopping,
            &mut requests,
            &mut controls,
        )
        .await;
        controls.stop();
        result
    }));
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    for index in 0..12 {
        fs::remove_file(tree.0.join(format!("test-bucket/{index:03}"))).unwrap();
    }
    hook.release.notify_one();
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    let observed = tokio::time::timeout(Duration::from_millis(500), root.observation())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.tracked_objects, 100);
    let (reply, mut waiting) = tokio::sync::oneshot::channel();
    commands
        .send(Command::Wait {
            deadline: tokio::time::Instant::now() + Duration::from_secs(30),
            settling: Default::default(),
            reply,
            identity: None,
        })
        .await
        .unwrap();
    let (reply, approved) = tokio::sync::oneshot::channel();
    commands
        .send(Command::Approve {
            revision: "00000000000000000000000000000000".into(),
            reply,
            identity: None,
        })
        .await
        .unwrap();
    hook.release.notify_one();
    let error = tokio::time::timeout(Duration::from_millis(500), approved)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(crate::error::causes(&error).any(|cause| matches!(
        cause.downcast_ref::<AdminError>(),
        Some(AdminError::Precondition)
    )));
    assert!(matches!(
        waiting.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    assert_eq!(root.observation().await.unwrap().tracked_objects, 100);
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 0);
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    hook.release.notify_one();
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    let set = root.deletion_set().await.unwrap().unwrap();
    assert!(set.guarded);
    assert_eq!(set.candidate_count, 100);
    assert_eq!(root.observation().await.unwrap().tracked_objects, 100);
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 0);
    stop.send(true).unwrap();
    hook.release.notify_one();
    worker.await.unwrap().unwrap();
    assert!(matches!(
        waiting.await.unwrap().unwrap().reason,
        crate::sync::control::WaitReason::Stopped
            | crate::sync::control::WaitReason::DeletionApproval
    ));
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn delayed_native_event_retains_the_completed_deletion_observation_until_its_scan_finishes() {
    let (_data, tree, catalog, _bucket, root) = live_fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for index in 0..100 {
        fs::write(tree.0.join(format!("test-bucket/{index:03}")), b"body").unwrap();
    }
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let worker_root = root.clone();
    let worker_catalog = catalog.clone();
    let hook = std::sync::Arc::new(Hook {
        phase: "observation-ready,observation-invalidated",
        entered: Default::default(),
        release: Default::default(),
    });
    let worker = tokio::spawn(EXPORT_HOOK.scope(hook.clone(), async move {
        run_live(&worker_catalog, &worker_root, &mut stopping).await
    }));
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    let writeback = root.default_writeback().await.unwrap();
    let mut gate = BaselineGate::Allowed;
    let mut complete = true;
    ingest_external(
        &catalog,
        &root,
        &Config::default(),
        &writeback,
        &stop.subscribe(),
        &mut gate,
        &mut complete,
    )
    .await
    .unwrap();
    let before = root.deletion_set().await.unwrap().unwrap();
    assert!(before.guarded);
    assert_eq!(before.candidate_count, 100);

    // The native deletion event was queued while the explicit scan completed.
    hook.release.notify_one();
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    assert!(root.deletion_set().await.unwrap().is_none());
    let (summary, during) = root
        .published_observation_for(
            crate::storage::sync_state::authority::ControlAuthority::Permissive,
        )
        .await
        .unwrap();
    assert!(summary.baseline_complete);
    let during = during.unwrap();
    assert_eq!(during.revision, before.revision);
    assert_eq!(during.candidate_count, 100);
    stop.send(true).unwrap();
    hook.release.notify_one();
    worker.await.unwrap().unwrap();
    drop(writeback);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn api_priority_preserves_external_conflicts_and_keeps_protected_api_metadata_unchanged() {
    for (held, tracked) in [(false, false), (false, true), (true, false), (true, true)] {
        let (data, tree, catalog, bucket, root) = live_fixture().await;
        publish_origin(&catalog, &bucket, b"old API", Origin::Api).await;
        if tracked {
            assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
        } else {
            fs::create_dir(tree.0.join("test-bucket")).unwrap();
        }
        fs::write(tree.0.join("test-bucket/object"), b"external saved body").unwrap();
        publish_origin(&catalog, &bucket, b"API winner", Origin::Api).await;
        if held {
            catalog
                .update_object_metadata(
                    live(bucket.as_str(), "object"),
                    Default::default(),
                    crate::object::ObjectMetadataUpdate {
                        temporary_hold: Some(true),
                        ..Default::default()
                    },
                    |_| Ok(()),
                    PERMISSIVE,
                )
                .await
                .unwrap();
        }
        let name = ObjectName::new("object").unwrap();
        let incarnation = catalog
            .admin(Origin::Api)
            .list_all_buckets(None, 1)
            .await
            .unwrap()
            .items[0]
            .incarnation;
        let winner = catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await
            .unwrap();
        root.record_export_conflict(
            root.sync_binding(&bucket).await.unwrap(),
            name.clone(),
            Generation::new(winner.generation).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(root.observation().await.unwrap().mapping_findings, 1);
        let summary = reconcile(&catalog, &root).await.unwrap();
        assert_eq!(summary.conflicting, 1);
        assert_eq!(summary.exported, 1);
        assert_eq!(root.observation().await.unwrap().mapping_findings, 0);
        assert_eq!(
            fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"API winner"
        );
        assert_eq!(
            catalog
                .admin(Origin::Api)
                .get_object(&bucket, &name, None)
                .await
                .unwrap(),
            winner
        );
        let page = root
            .conflict_page(
                None,
                0,
                256,
                crate::storage::sync_state::authority::ControlAuthority::Permissive,
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].owner_project, "owner");
        assert_eq!(page.items[0].generation, Some(winner.generation));
        assert_eq!(page.items[0].bucket, bucket.as_str());
        assert_eq!(page.items[0].bucket_incarnation, incarnation);
        assert_eq!(page.usage.entries, 1);
        assert_eq!(
            fs::read(
                data.0
                    .join("sync-conflicts")
                    .join(page.items[0].conflict_id.to_string())
            )
            .unwrap(),
            b"external saved body"
        );
        assert_eq!(reconcile(&catalog, &root).await.unwrap().identical, 1);
        assert_eq!(
            root.conflict_page(
                None,
                0,
                256,
                crate::storage::sync_state::authority::ControlAuthority::Permissive
            )
            .await
            .unwrap()
            .items
            .len(),
            1
        );
        root.resolve_conflict(
            crate::storage::sync_state::conflicts::ResolveRequest {
                operation_id: "discard-preserved-revision".into(),
                action: crate::storage::sync_state::conflicts::Action::Discard,
                target: crate::storage::sync_state::conflicts::ResolveTarget::Conflict(
                    ConflictTarget {
                        conflict_id: page.items[0].conflict_id.to_string(),
                        revision: page.items[0].revision.to_string(),
                        expected_generation: winner.generation,
                    },
                ),
            },
            crate::storage::sync_state::authority::ControlAuthority::Permissive,
            4096,
        )
        .await
        .unwrap();
        assert_eq!(
            catalog
                .admin(Origin::Api)
                .get_object(&bucket, &name, None)
                .await
                .unwrap(),
            winner
        );
        assert_eq!(root.observation().await.unwrap().unresolved_conflicts, 0);
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn api_deletion_preserves_divergent_external_bytes_for_an_explicit_restore() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    let original = publish_origin(&catalog, &bucket, b"old API", Origin::Api).await;
    reconcile(&catalog, &root).await.unwrap();
    fs::write(
        tree.0.join("test-bucket/object"),
        b"retained external bytes",
    )
    .unwrap();
    let name = ObjectName::new("object").unwrap();
    root.record_export_conflict(
        root.sync_binding(&bucket).await.unwrap(),
        name.clone(),
        Generation::new(original.generation).unwrap(),
    )
    .await
    .unwrap();
    catalog
        .admin(Origin::Api)
        .delete_object(
            &bucket,
            &name,
            GenerationCondition::Is(Generation::new(original.generation).unwrap()),
        )
        .await
        .unwrap();
    let summary = reconcile(&catalog, &root).await.unwrap();
    assert_eq!(summary.conflicting, 1);
    assert_eq!(root.observation().await.unwrap().mapping_findings, 0);
    assert!(!tree.0.join("test-bucket/object").exists());
    assert!(matches!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await,
        Err(AdminError::NotFound)
    ));
    let page = root
        .conflict_page(
            None,
            0,
            256,
            crate::storage::sync_state::authority::ControlAuthority::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        fs::read(
            data.0
                .join("sync-conflicts")
                .join(page.items[0].conflict_id.to_string())
        )
        .unwrap(),
        b"retained external bytes"
    );
    let restored = root
        .resolve_conflict(
            crate::storage::sync_state::conflicts::ResolveRequest {
                operation_id: "restore-after-api-deletion".into(),
                action: crate::storage::sync_state::conflicts::Action::Restore,
                target: crate::storage::sync_state::conflicts::ResolveTarget::Conflict(
                    ConflictTarget {
                        conflict_id: page.items[0].conflict_id.to_string(),
                        revision: page.items[0].revision.to_string(),
                        expected_generation: 0,
                    },
                ),
            },
            crate::storage::sync_state::authority::ControlAuthority::Permissive,
            4096,
        )
        .await
        .unwrap();
    assert!(restored.result_generation.unwrap() > original.generation);
    let record = catalog
        .admin(Origin::Api)
        .get_object(&bucket, &name, None)
        .await
        .unwrap();
    assert_eq!(record.size, b"retained external bytes".len() as u64);
    assert_eq!(record.crc32c, crc32c::crc32c(b"retained external bytes"));
    assert_eq!(root.observation().await.unwrap().unresolved_conflicts, 0);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn live_export_retries_identical_observation_at_prepare_arm_durable_and_ack() {
    use std::time::Duration;
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
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let (hook, worker) = hooked_worker(&EXPORT_HOOK, "idle", async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        })
        .await;
        crate::storage::sync_state::writeback::inject_live_export_failure(
            root.status().root_id,
            phase,
            1,
        );
        let record = publish_origin(&catalog, &bucket, b"same", Origin::Api).await;
        publish_origin(&catalog, &other, b"other", Origin::Api).await;
        let started = tokio::time::Instant::now();
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
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
                live(bucket.as_str(), "object"),
                Default::default(),
                crate::object::ObjectMetadataUpdate {
                    content_type: Some(Some("text/plain".into())),
                    ..Default::default()
                },
                |_| Ok(()),
                PERMISSIVE,
            )
            .await
            .unwrap();
        let before = std::fs::metadata(tree.0.join("test-bucket/object")).unwrap();
        let started = tokio::time::Instant::now();
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
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
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
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
        catalog.enable_change_journal().await.unwrap();
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
                LiveSource::Observe,
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
    for moved in [false, true] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        let other = BucketName::new("zz-other").unwrap();
        catalog
            .admin(Origin::Api)
            .ensure_bucket(BucketDefinition::default_for(&other, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let (hook, worker) = hooked_worker(&EXPORT_HOOK, "idle,after-prepare", async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        })
        .await;
        let path = tree.0.join("test-bucket/object");
        std::fs::write(&path, b"same").unwrap();
        publish_origin(&catalog, &bucket, b"same", Origin::Api).await;
        publish_origin(&catalog, &other, b"other", Origin::Api).await;
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        if moved {
            std::fs::rename(tree.0.join("test-bucket"), tree.0.join("moved")).unwrap();
        } else {
            std::fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
        }
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
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
    use std::os::unix::fs::PermissionsExt;
    for parent in [false, true] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        publish_origin(&catalog, &bucket, b"before", Origin::Api).await;
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let (hook, worker) = hooked_worker(&EXPORT_HOOK, "idle,retry", async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        })
        .await;
        let path = tree.0.join(if parent {
            "test-bucket"
        } else {
            "test-bucket/object"
        });
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
        publish_origin(&catalog, &bucket, b"after", Origin::Api).await;
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(if parent { 0o700 } else { 0o600 }),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"before"
        );
        assert_eq!(
            root.writeback(Limits::new(4096, 10).unwrap())
                .await
                .unwrap()
                .status()
                .await
                .unwrap()
                .protected_entries,
            0
        );
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
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
            let worker_catalog = catalog.clone();
            let worker_root = root.clone();
            let (hook, worker) = hooked_worker(&EXPORT_HOOK, phases, async move {
                run_live(&worker_catalog, &worker_root, &mut stopping).await
            })
            .await;
            let record = publish_origin(&catalog, &bucket, b"obsolete", Origin::Api).await;
            publish_origin(&catalog, &other, b"other", Origin::Api).await;
            hook.release.notify_one();
            tokio::time::timeout(HANG_GUARD, hook.entered.notified())
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
                .delete_bucket(bucket.as_str(), Default::default(), PERMISSIVE)
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
            tokio::time::timeout(HANG_GUARD, hook.entered.notified())
                .await
                .unwrap();
            hook.release.notify_one();
            tokio::time::timeout(HANG_GUARD, hook.entered.notified())
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
        let (data, tree, catalog, bucket, root) = live_fixture().await;
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let count = Arc::new(AtomicUsize::new(0));
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let (hook, worker) = hooked_worker(
            &EXPORT_HOOK,
            "idle",
            LIVE_EXPORTS.scope(count.clone(), async move {
                run_live(&worker_catalog, &worker_root, &mut stopping).await
            }),
        )
        .await;
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
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"299"
        );
        // Native observation can enqueue our export's own event before becoming quiet.
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                hook.release.notify_one();
                if tokio::time::timeout(Duration::from_millis(100), hook.entered.notified())
                    .await
                    .is_err()
                {
                    break;
                }
                assert_eq!(count.load(Ordering::SeqCst), 1);
            }
        })
        .await
        .unwrap();
        stop.send_replace(true);
        hook.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn live_export_retries_write_failures_at_copy_install_sync_and_catalog_ack() {
    use std::time::Duration;
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
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let (hook, mut worker) = hooked_worker(&EXPORT_HOOK, "idle", async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        })
        .await;
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
        if phase == "installed" {
            assert!(
                tokio::time::timeout(Duration::from_secs(3), worker)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
            let recovering = root
                .writeback(
                    Limits::new(
                        super::super::CONFLICT_MAX_BYTES,
                        super::super::CONFLICT_MAX_ENTRIES,
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            let usage = recovering.status().await.unwrap();
            assert_eq!(usage.stop, Stop::IoFailure);
            assert_eq!(usage.protected_entries, 1);
            assert_eq!(usage.retained_bytes, b"before".len() as u128);
            let pending = recovering.page(0, 256).await.unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(
                fs::read(
                    tree.0
                        .join(mapping::WORKSPACE)
                        .join(pending[0].token.id().to_string())
                )
                .unwrap(),
                b"before"
            );
            recovering.recover_pending().await.unwrap();
            assert_eq!(
                recovering.preserve_protected().await.unwrap().stop,
                Stop::Ready
            );
            let copies = root
                .conflict_page(
                    None,
                    0,
                    256,
                    crate::storage::sync_state::authority::ControlAuthority::Permissive,
                )
                .await
                .unwrap();
            assert_eq!(copies.items.len(), 1);
            assert_eq!(copies.items[0].owner_project, "owner");
            assert_eq!(
                copies.items[0].generation,
                Some(pending[0].generation.get())
            );
            assert_eq!(copies.items[0].bucket_incarnation, pending[0].incarnation);
            assert_eq!(
                fs::read(
                    data.0
                        .join("sync-conflicts")
                        .join(copies.items[0].conflict_id.to_string())
                )
                .unwrap(),
                b"before"
            );
            let worker_catalog = catalog.clone();
            let worker_root = root.clone();
            let mut stopping = stop.subscribe();
            worker = tokio::spawn(EXPORT_HOOK.scope(hook.clone(), async move {
                run_live(&worker_catalog, &worker_root, &mut stopping).await
            }));
        }
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap_or_else(|error| panic!("phase {phase}, remove={remove}: {error}"));
        if remove {
            assert!(!tree.0.join("test-bucket/object").exists());
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
            .get_path(binding.clone(), "test-bucket/object".into())
            .await
            .unwrap();
        if remove {
            assert!(tracked.is_none());
        } else {
            assert_eq!(tracked.unwrap().generation.get(), record.generation);
        }
        if phase == "cancel" {
            let page = root
                .conflict_page(
                    None,
                    0,
                    256,
                    crate::storage::sync_state::authority::ControlAuthority::Permissive,
                )
                .await
                .unwrap();
            assert_eq!(page.items.len(), 1);
            let conflict = &page.items[0];
            assert_eq!(conflict.owner_project, "owner");
            assert_eq!(conflict.generation, Some(record.generation));
            assert_eq!(conflict.bucket, bucket.as_str());
            assert_eq!(conflict.bucket_incarnation.to_hex().len(), 32);
            assert_eq!(
                fs::read(
                    data.0
                        .join("sync-conflicts")
                        .join(conflict.conflict_id.to_string())
                )
                .unwrap(),
                b"external"
            );
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
            let file = reader.into_file().await.unwrap();
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
        catalog.enable_change_journal().await.unwrap();
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
        let hook = std::sync::Arc::new(Hook {
            phase,
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let name = ObjectName::new("object").unwrap();
        let mut summary = Reconciliation::default();
        let (result, ()) = tokio::join!(
            EXPORT_HOOK.scope(
                hook.clone(),
                reconcile_live_object(
                    &catalog,
                    &root,
                    &writeback,
                    &bucket,
                    &name,
                    fence,
                    &mut summary
                )
            ),
            async {
                tokio::time::timeout(HANG_GUARD, hook.entered.notified())
                    .await
                    .unwrap();
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
        result.unwrap();
        assert!(
            summary.deferred > 0 || summary.conflicting > 0,
            "{phase} {action}"
        );
        assert_eq!(
            writeback.status().await.unwrap().stop,
            Stop::Ready,
            "{phase} {action}"
        );
        assert!(writeback.page(0, 256).await.unwrap().is_empty());
        if action.starts_with("external") {
            let conflicts = root
                .conflict_page(
                    None,
                    0,
                    256,
                    crate::storage::sync_state::authority::ControlAuthority::Permissive,
                )
                .await
                .unwrap();
            assert_eq!(conflicts.items.len(), 1, "{phase} {action}");
            assert_eq!(
                std::fs::read(
                    data.0
                        .join("sync-conflicts")
                        .join(conflicts.items[0].conflict_id.to_string())
                )
                .unwrap(),
                b"external"
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"api");
        }
        if action == "overwrite" {
            let (_, reader) = admin.open_object(&bucket, &name, None).await.unwrap();
            let mut file = reader.into_file().await.unwrap();
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
        reconcile(&catalog, &root).await.unwrap();
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[derive(Clone, Copy)]
enum LiveFailure {
    Error,
    Panic,
    /// Catalog admission timed out, as under contention.
    Busy,
}

/// Failures armed per working tree, since tests run in parallel and the mirror runs on its
/// own thread, out of reach of a task-local hook.
static LIVE_FAILURES: std::sync::Mutex<Vec<(PathBuf, LiveFailure)>> =
    std::sync::Mutex::new(Vec::new());

pub(super) fn injected_live_failure(config: &Config) -> Result<(), SyncError> {
    let mut armed = LIVE_FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(index) = armed
        .iter()
        .position(|(tree, _)| config.sync_dir.as_ref() == Some(tree))
    else {
        return Ok(());
    };
    let (_, failure) = armed.remove(index);
    drop(armed);
    match failure {
        LiveFailure::Error => Err(SyncError::Io(io::Error::other(
            "injected live sync failure",
        ))),
        LiveFailure::Panic => panic!("injected live sync panic"),
        LiveFailure::Busy => Err(SyncError::Admin(AdminError::Busy {
            retry_after: std::time::Duration::from_secs(1),
        })),
    }
}

async fn started_mirror(failure: Option<LiveFailure>) -> (TestDir, TestDir, Catalog, Task) {
    started_mirror_failing(failure.as_slice()).await
}

/// Starts the mirror with `failures` armed for its first runs, in order.
async fn started_mirror_failing(failures: &[LiveFailure]) -> (TestDir, TestDir, Catalog, Task) {
    let (data, tree, catalog, _, root) = live_fixture().await;
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    LIVE_FAILURES
        .lock()
        .unwrap()
        .extend(failures.iter().map(|failure| (tree.0.clone(), *failure)));
    let task = start(catalog.clone(), root, &config).unwrap();
    (data, tree, catalog, task)
}

/// An error or panic must keep readiness off until an explicit rescan succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_mirror_turns_readiness_off() {
    for failure in [LiveFailure::Error, LiveFailure::Panic] {
        let (_data, _tree, catalog, task) = started_mirror(Some(failure)).await;
        eventually_async("the mirror to stop", || async {
            !catalog.background_tasks_running()
        })
        .await;
        assert!(
            catalog.is_healthy(),
            "a stopped mirror does not poison the catalog"
        );
        let (control, root) = catalog.sync_controls().unwrap().unwrap();
        await_paused(&root).await;
        assert!(
            !catalog.background_tasks_running(),
            "inspection cannot resume a paused observer"
        );
        drop((control, root));
        task.finish().await.unwrap();
        assert!(!catalog.background_tasks_running(), "the stop is permanent");
        catalog.close().await.unwrap();
    }
    assert_eq!(
        crate::task::tests::reported_panics("sync-mirror"),
        ["injected live sync panic"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_rescan_recovers_a_paused_observer_and_shutdown_releases_ownership() {
    let (_data, tree, catalog, task) = started_mirror(Some(LiveFailure::Error)).await;
    eventually_async("the mirror to stop", || async {
        !catalog.background_tasks_running()
    })
    .await;
    assert!(matches!(
        catalog
            .sync_state()
            .activate_root(&tree.0, crate::storage::sync_state::root::Activation::Open)
            .await,
        Err(RootError::InUse)
    ));
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    await_paused(&root).await;
    fs::create_dir_all(tree.0.join("test-bucket")).unwrap();
    fs::write(
        tree.0.join("test-bucket/operator-note"),
        b"preserve while paused",
    )
    .unwrap();
    assert!(!catalog.background_tasks_running());
    assert!(
        tokio::time::timeout(Duration::from_secs(10), task.rescan())
            .await
            .unwrap()
            .unwrap()
            .baseline_complete
    );
    assert!(root.observation().await.unwrap().paused_reason.is_none());
    assert!(catalog.background_tasks_running());
    drop(root);
    task.finish().await.unwrap();
    assert!(catalog.sync_controls().unwrap().is_none());
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, crate::storage::sync_state::root::Activation::Open)
        .await
        .unwrap();
    assert_eq!(
        fs::read(tree.0.join("test-bucket/operator-note")).unwrap(),
        b"preserve while paused"
    );
    drop(root);
    catalog.close().await.unwrap();
}

async fn await_paused(root: &RootLease) {
    eventually_async("the observer to pause", || async {
        root.observation().await.unwrap().paused_reason.is_some()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_disk_full_pauses_for_controls_and_shutdown_releases_the_root() {
    let (_data, tree, catalog, task) = started_mirror(None).await;
    task.rescan().await.unwrap();
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    crate::storage::sync_state::writeback::inject_live_export_failure(
        root.status().root_id,
        "copy",
        100,
    );
    let bucket = BucketName::new("test-bucket").unwrap();
    let record = publish_origin(&catalog, &bucket, b"API winner survives", Origin::Api).await;
    await_paused(&root).await;
    assert!(!catalog.background_tasks_running());
    assert!(catalog.sync_controls().unwrap().is_some());
    let page = root
        .conflict_page(
            None,
            0,
            256,
            crate::storage::sync_state::authority::ControlAuthority::Permissive,
        )
        .await
        .unwrap();
    assert!(page.items.is_empty());
    let (reply, receive) = tokio::sync::oneshot::channel();
    let (control, _) = catalog.sync_controls().unwrap().unwrap();
    control
        .sender
        .try_send(Command::Resolve {
            request: crate::storage::sync_state::conflicts::ResolveRequest {
                operation_id: "missing-but-observed".into(),
                action: crate::storage::sync_state::conflicts::Action::Discard,
                target: crate::storage::sync_state::conflicts::ResolveTarget::Conflict(
                    ConflictTarget {
                        conflict_id: "0".repeat(32),
                        revision: "0".repeat(32),
                        expected_generation: record.generation,
                    },
                ),
            },
            reply,
            identity: None,
        })
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), receive)
            .await
            .unwrap()
            .unwrap(),
        Err(crate::storage::sync_state::conflicts::Error::RevisionMismatch)
    ));
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &ObjectName::new("object").unwrap(), None)
            .await
            .unwrap(),
        record
    );
    drop((control, root));
    tokio::time::timeout(Duration::from_secs(2), task.finish())
        .await
        .unwrap()
        .unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, crate::storage::sync_state::root::Activation::Open)
        .await
        .unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_interrupts_a_persistent_io_retry_after_its_owned_transition() {
    let (_data, tree, catalog, task) = started_mirror(None).await;
    task.rescan().await.unwrap();
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    crate::storage::sync_state::writeback::inject_live_export_failure(
        root.status().root_id,
        "copy",
        100,
    );
    publish_origin(
        &catalog,
        &BucketName::new("test-bucket").unwrap(),
        b"API body",
        Origin::Api,
    )
    .await;
    eventually_async("a pending writeback", || async {
        root.observation().await.unwrap().pending_writeback != 0
    })
    .await;
    drop(root);
    tokio::time::timeout(Duration::from_secs(2), task.finish())
        .await
        .unwrap()
        .unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, crate::storage::sync_state::root::Activation::Open)
        .await
        .unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn failed_explicit_rescan_records_the_pause_before_replying() {
    use crate::storage::sync_state::observation::PauseReason;
    let (_data, tree, catalog, _bucket, root) = live_fixture().await;
    reconcile(&catalog, &root).await.unwrap();
    let writeback = root.default_writeback().await.unwrap();
    let workspace = tree.0.join(mapping::WORKSPACE);
    fs::rename(&workspace, tree.0.join("retained-workspace")).unwrap();
    fs::create_dir(&workspace).unwrap();
    let config = Config::default();
    let mut cursor = catalog.read_changes(None, None, 1).await.unwrap().fence;
    let mut gate = BaselineGate::Allowed;
    let mut baseline = true;
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    let mut control = LiveControl {
        catalog: &catalog,
        root: &root,
        config: &config,
        writeback: &writeback,
        stopping: &stopping,
        ingest_complete: &mut gate,
        baseline_complete: &mut baseline,
        cursor: &mut cursor,
    };
    let (reply, result) = tokio::sync::oneshot::channel();
    let error = control
        .handle_authorized(Command::Rescan {
            reply,
            identity: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error.pause_reason(), PauseReason::RecoveryRequired);
    assert!(result.await.unwrap().is_err());
    assert_eq!(
        root.observation().await.unwrap().paused_reason,
        Some(PauseReason::RecoveryRequired)
    );
    drop(writeback);
    drop(root);
    catalog.close().await.unwrap();
}

#[test]
fn only_typed_transient_io_failures_are_retried() {
    assert!(!retryable(&writeback::Error::Admin(AdminError::Io(
        io::Error::other("permanent corruption").into()
    ))));
    for errno in [libc::EMFILE, libc::ENFILE, libc::EAGAIN] {
        let error = writeback::Error::Admin(io::Error::from_raw_os_error(errno).into());
        assert!(retryable(&error));
    }
}

#[tokio::test]
async fn wait_deadline_interrupts_settling_without_publishing_an_incomplete_scan() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    reconcile(&catalog, &root).await.unwrap();
    fs::write(tree.0.join("test-bucket/new-source"), b"unsettled").unwrap();
    let config = Config::default();
    let mut cursor = catalog.read_changes(None, None, 1).await.unwrap().fence;
    let mut gate = BaselineGate::Allowed;
    let mut baseline = true;
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    let writeback = root.default_writeback().await.unwrap();
    let mut control = LiveControl {
        catalog: &catalog,
        root: &root,
        config: &config,
        writeback: &writeback,
        stopping: &stopping,
        ingest_complete: &mut gate,
        baseline_complete: &mut baseline,
        cursor: &mut cursor,
    };
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        control.wait(tokio::time::Instant::now() + Duration::from_millis(50)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.reason, crate::sync::control::WaitReason::Timeout);
    assert!(result.reconciliation.unwrap().interrupted);
    assert!(matches!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &ObjectName::new("new-source").unwrap(), None)
            .await,
        Err(AdminError::NotFound)
    ));
    assert_eq!(gate, BaselineGate::Deferred);
    drop(writeback);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn wait_captures_a_fixed_post_scan_fence_and_does_not_chase_later_writes() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    catalog.enable_change_journal().await.unwrap();
    let config = Config::default();
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let mut cursor = catalog.read_changes(None, None, 1).await.unwrap().fence;
    publish_origin(&catalog, &bucket, b"captured", Origin::Api).await;
    let mut gate = BaselineGate::Allowed;
    let mut baseline = true;
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    let writeback = root.default_writeback().await.unwrap();
    let mut control = LiveControl {
        writeback: &writeback,
        catalog: &catalog,
        root: &root,
        config: &config,
        stopping: &stopping,
        ingest_complete: &mut gate,
        baseline_complete: &mut baseline,
        cursor: &mut cursor,
    };
    assert!(
        control
            .wait(tokio::time::Instant::now())
            .await
            .unwrap()
            .timed_out
    );
    let hook = std::sync::Arc::new(Hook {
        phase: "wait-fence",
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let result = EXPORT_HOOK
        .scope(hook.clone(), async {
            let work=control.wait(tokio::time::Instant::now() + Duration::from_secs(10));
            tokio::pin!(work);
            tokio::select! {
                result=&mut work=>panic!("wait finished before its fence checkpoint: {:?}",result.map(|value|value.reason)),
                result=tokio::time::timeout(Duration::from_secs(5),hook.entered.notified())=>{result.unwrap();}
            }
                    let other = BucketName::new("later-bucket").unwrap();
                    catalog
                        .admin(Origin::Api)
                        .ensure_bucket(
                            BucketDefinition::default_for(&other, "owner", &SystemClock).unwrap(),
                        )
                        .await
                        .unwrap();
                    publish_origin(&catalog, &other, b"after capture", Origin::Api).await;
            hook.release.notify_one();
            work.await
        })
        .await;
    let result = result.unwrap();
    assert!(result.completed, "{:?}", result.reason);
    assert!(!result.timed_out);
    assert!(
        result.journal_fence.unwrap().sequence
            < catalog
                .read_changes(None, None, 1)
                .await
                .unwrap()
                .fence
                .sequence
                .get()
    );
    assert_eq!(
        fs::read(tree.0.join("test-bucket/object")).unwrap(),
        b"captured"
    );
    assert!(!tree.0.join("later-bucket/object").exists());
    drop(writeback);
    drop(root);
    catalog.close().await.unwrap();
}

/// Contention that outlasts the catalog wait must not take the server out of readiness until
/// restart: the mirror retries, and its next successful pass clears the failed ones, even after
/// enough consecutive failures to report it down.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transient_mirror_failure_is_retried_until_it_clears() {
    let (_data, _tree, catalog, task) = started_mirror_failing(&[LiveFailure::Busy; 4]).await;
    // A served rescan shows a retried run got past its start.
    tokio::time::timeout(std::time::Duration::from_secs(10), task.rescan())
        .await
        .unwrap()
        .unwrap();
    assert!(
        catalog.background_tasks_running(),
        "a successful pass restores readiness"
    );
    task.finish().await.unwrap();
    assert!(catalog.background_tasks_running());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn retry_repeats_transient_errors_and_keeps_permanent_causes() {
    let (_data, _tree, catalog, _bucket, root) = live_fixture().await;
    let mut attempts = 0;
    let result = with_retry(&root, || {
        attempts += 1;
        std::future::ready(if attempts == 1 {
            Err(writeback::Error::Admin(AdminError::Io(
                io::ErrorKind::Interrupted.into(),
            )))
        } else {
            Ok(7)
        })
    })
    .await
    .unwrap();
    assert_eq!(result, 7);
    assert_eq!(attempts, 2);
    attempts = 0;
    let error = with_retry(&root, || {
        attempts += 1;
        std::future::ready(Err::<(), _>(writeback::Error::Admin(AdminError::Forbidden)))
    })
    .await
    .unwrap_err();
    assert_eq!(attempts, 1);
    assert!(matches!(
        error,
        SyncError::Writeback(writeback::Error::Admin(AdminError::Forbidden))
    ));
    drop(root);
    catalog.close().await.unwrap();
}

#[test]
fn writeback_retries_resource_pressure_without_retrying_unclassified_failures() {
    for error in [
        io::Error::from_raw_os_error(libc::EMFILE),
        io::Error::from_raw_os_error(libc::ENFILE),
        io::ErrorKind::TimedOut.into(),
    ] {
        assert!(retryable(&writeback::Error::Admin(error.into())));
    }
    assert!(!retryable(&writeback::Error::Admin(AdminError::Io(
        io::Error::other("invalid state").into()
    ))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mirror_stopped_by_shutdown_stays_ready() {
    let (_data, _tree, catalog, task) = started_mirror(None).await;
    // A served rescan shows the mirror is past its start and running.
    tokio::time::timeout(std::time::Duration::from_secs(10), task.rescan())
        .await
        .unwrap()
        .unwrap();
    assert!(catalog.background_tasks_running());
    task.finish().await.unwrap();
    assert!(catalog.background_tasks_running());
    catalog.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn native_watch_permission_retry_lowers_readiness_and_shutdown_releases_the_root() {
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    let (_data, tree, catalog, task) = started_mirror(None).await;
    // Drain startup exports before attributing an unhealthy observer to the permission change.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let (reply, response) = tokio::sync::oneshot::channel();
    task.rescan
        .send(Command::Wait {
            deadline,
            settling: Default::default(),
            reply,
            identity: None,
        })
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout_at(deadline, response)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .reason,
        crate::sync::control::WaitReason::Ready
    );
    let bucket = tree.0.join("test-bucket");
    std::fs::set_permissions(&bucket, std::fs::Permissions::from_mode(0o0)).unwrap();
    eventually_async("the observer to stop", || async {
        !catalog.background_tasks_running()
    })
    .await;
    tokio::time::timeout(Duration::from_secs(5), task.finish())
        .await
        .unwrap()
        .unwrap();
    std::fs::set_permissions(&bucket, std::fs::Permissions::from_mode(0o700)).unwrap();
    catalog.close().await.unwrap();
}
