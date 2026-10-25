use super::*;
use std::{sync::Arc, time::Duration};

async fn setup() -> (TestDir, Catalog) {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .admin(Origin::Seed)
        .ensure_bucket(bucket("test-bucket"))
        .await
        .unwrap();
    (dir, catalog)
}

async fn paused_wait(
    catalog: &Catalog,
    after: JournalFence,
    phase: &'static str,
) -> (
    Arc<WaitHook>,
    tokio::task::JoinHandle<Result<ChangeObservation, AdminError>>,
) {
    let hook = Arc::new(WaitHook {
        phase,
        entered: Default::default(),
        release: Default::default(),
        used: Default::default(),
        checks: Default::default(),
    });
    let worker = catalog.clone();
    let context = hook.clone();
    let wait =
        tokio::spawn(WAIT_HOOK.scope(context, async move { worker.wait_for_changes(after).await }));
    tokio::time::timeout(Duration::from_secs(5), hook.entered.notified())
        .await
        .unwrap();
    (hook, wait)
}

async fn finish(
    hook: Arc<WaitHook>,
    wait: tokio::task::JoinHandle<Result<ChangeObservation, AdminError>>,
) -> ChangeObservation {
    hook.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn sync_observation_all_owner_pages_include_empty_buckets_and_legacy_cursors() {
    let (_dir, catalog) = setup().await;
    let admin = catalog.admin(Origin::SyncIngest);
    let mut definitions = Vec::new();
    for index in 0..300 {
        definitions.push(
            BucketDefinition::default_for(
                &BucketName::new(format!("bucket-{index:03}")).unwrap(),
                &format!("owner-{}", index % 3),
                &SystemClock,
            )
            .unwrap(),
        );
    }
    admin.ensure_buckets(definitions).await.unwrap();
    let mut legacy = admin
        .get_bucket(&BucketName::new("test-bucket").unwrap())
        .await
        .unwrap()
        .unwrap();
    legacy.name = "legacy_bucket".into();
    catalog.insert_bucket(legacy).await.unwrap();
    let first = admin.list_all_buckets(None, 256).await.unwrap();
    assert!(!first.items.is_empty() && first.items.len() <= 256);
    assert!(first.has_more);
    assert_eq!(first.items[0].bucket.name, "bucket-000");
    for (index, item) in first.items.iter().enumerate() {
        assert_eq!(item.bucket.project, format!("owner-{}", index % 3));
        assert_eq!(item.incarnation.len(), 32);
    }
    let mut cursor = first.items.last().unwrap().bucket.name.clone();
    let mut count = first.items.len();
    loop {
        let page = admin.list_all_buckets(Some(&cursor), 256).await.unwrap();
        assert!(!page.items.is_empty() && page.items.len() <= 256);
        for item in &page.items {
            assert!(item.bucket.name > cursor);
            cursor = item.bucket.name.clone();
        }
        count += page.items.len();
        if !page.has_more {
            break;
        }
    }
    assert_eq!(count, 302);
    let legacy = admin.list_all_buckets(Some("bucket-299"), 1).await.unwrap();
    assert_eq!(legacy.items[0].bucket.name, "legacy_bucket");
    assert!(legacy.has_more);
    let last = admin
        .list_all_buckets(Some("legacy_bucket"), 1)
        .await
        .unwrap();
    assert_eq!(last.items[0].bucket.name, "test-bucket");
    assert!(!last.has_more);
    assert!(
        admin
            .list_all_buckets(Some("test-bucket"), 256)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    for limit in [0, 257] {
        assert!(matches!(
            admin.list_all_buckets(None, limit).await,
            Err(AdminError::Invalid(_))
        ));
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_observation_owner_inventory_recreation_preserves_keyset_and_incarnation() {
    let (_dir, catalog) = setup().await;
    let admin = catalog.admin(Origin::SyncIngest);
    for name in ["page-a", "page-m", "page-z"] {
        admin.ensure_bucket(bucket(name)).await.unwrap();
    }
    let baseline = catalog.set_change_journal_enabled(true).await.unwrap();
    let first = admin.list_all_buckets(None, 1).await.unwrap();
    let old = &first.items[0];
    assert_eq!(old.bucket.name, "page-a");
    catalog.delete_bucket("page-a", None, None).await.unwrap();
    admin
        .ensure_bucket(
            BucketDefinition::default_for(
                &BucketName::new("page-a").unwrap(),
                "other-owner",
                &SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let next = admin
        .list_all_buckets(Some(&old.bucket.name), 1)
        .await
        .unwrap();
    assert_eq!(next.items[0].bucket.name, "page-m");
    let replay = catalog
        .read_changes(Some(baseline), None, 256)
        .await
        .unwrap();
    assert_eq!(
        replay.items.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [ChangeKind::BucketDelete, ChangeKind::BucketCreate]
    );
    let current = admin.list_all_buckets(None, 1).await.unwrap();
    assert_eq!(current.items[0].bucket.project, "other-owner");
    assert_ne!(current.items[0].incarnation, old.incarnation);
    assert_eq!(replay.items[1].incarnation, current.items[0].incarnation);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_observation_owner_pages_share_metadata_budget_and_bounded_lookahead() {
    let (_dir, catalog) = setup().await;
    let admin = catalog.admin(Origin::SyncIngest);
    let template = admin
        .get_bucket(&BucketName::new("test-bucket").unwrap())
        .await
        .unwrap()
        .unwrap();
    for name in ["page-a", "page-b", "page-c"] {
        let mut record = template.clone();
        record.name = name.into();
        record.project = format!("owner-{name}");
        if name == "page-b" {
            record.cors = vec![crate::bucket::CorsRule::default(); 20_000];
        }
        catalog.insert_bucket(record).await.unwrap();
    }
    let first = admin.list_all_buckets(None, 256).await.unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].bucket.name, "page-a");
    assert!(first.has_more);
    let large = admin.list_all_buckets(Some("page-a"), 256).await.unwrap();
    assert_eq!(large.items.len(), 1);
    assert_eq!(large.items[0].bucket.cors.len(), 20_000);
    assert!(large.has_more);
    catalog
        .run_admin(|connection| {
            connection
                .execute(
                    "UPDATE buckets SET labels = 'invalid' WHERE name = 'page-c'",
                    [],
                )
                .map_err(unavailable)?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        admin
            .list_all_buckets(Some("page-a"), 1)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(admin.list_all_buckets(Some("page-b"), 1).await.is_err());
    catalog
        .run_admin(|connection| {
            connection
                .execute("UPDATE buckets SET labels = '{}' WHERE name = 'page-c'", [])
                .map_err(unavailable)?;
            connection
                .execute(
                    "DELETE FROM bucket_incarnations WHERE bucket = 'page-c'",
                    [],
                )
                .map_err(unavailable)?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(admin.list_all_buckets(Some("page-b"), 1).await.is_err());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_observation_commits_before_during_and_after_registration_are_not_lost() {
    for phase in ["before-register", "registered", "checked"] {
        let (_dir, catalog) = setup().await;
        let baseline = catalog.set_change_journal_enabled(true).await.unwrap();
        let (hook, wait) = paused_wait(&catalog, baseline, phase).await;
        publish(&catalog, Origin::SyncIngest, "object").await;
        let observed = finish(hook, wait).await;
        assert!(!observed.rescan_required);
        assert!(observed.fence.sequence > baseline.sequence);
        let page = catalog.read_changes(Some(baseline), None, 1).await.unwrap();
        let captured = page.fence;
        publish(&catalog, Origin::Api, "later").await;
        assert_eq!(
            catalog
                .read_changes(Some(page.next), Some(captured), 256)
                .await
                .unwrap()
                .fence,
            captured
        );
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn sync_observation_every_committed_mutation_wakes_all_waiters() {
    let (_dir, catalog) = setup().await;
    let mut cursor = catalog.set_change_journal_enabled(true).await.unwrap();
    for kind in [
        ChangeKind::Publish,
        ChangeKind::Metadata,
        ChangeKind::Delete,
        ChangeKind::BucketCreate,
        ChangeKind::BucketUpdate,
        ChangeKind::Iam,
        ChangeKind::BucketDelete,
    ] {
        let mut waits = Vec::new();
        for _ in 0..3 {
            waits.push(paused_wait(&catalog, cursor, "checked").await);
        }
        match kind {
            ChangeKind::Publish => publish(&catalog, Origin::SyncIngest, "object").await,
            ChangeKind::Metadata => {
                catalog
                    .update_object_metadata(
                        "test-bucket",
                        "object",
                        None,
                        ObjectConditions::default(),
                        ObjectMetadataUpdate::default(),
                        |_| Ok(()),
                    )
                    .await
                    .unwrap();
            }
            ChangeKind::Delete => {
                catalog
                    .admin(Origin::SyncIngest)
                    .delete_object(
                        &BucketName::new("test-bucket").unwrap(),
                        &ObjectName::new("object").unwrap(),
                        crate::storage::admin::GenerationCondition::Is(
                            crate::storage::admin::Generation::new(
                                catalog
                                    .get_object(
                                        "test-bucket",
                                        "object",
                                        None,
                                        ObjectConditions::default(),
                                    )
                                    .await
                                    .unwrap()
                                    .generation,
                            )
                            .unwrap(),
                        ),
                    )
                    .await
                    .unwrap();
            }
            ChangeKind::BucketCreate => {
                catalog
                    .admin(Origin::BucketInit)
                    .ensure_bucket(bucket("other-bucket"))
                    .await
                    .unwrap();
            }
            ChangeKind::BucketUpdate => {
                catalog
                    .patch_bucket(
                        "other-bucket",
                        crate::bucket::BucketMetadataUpdate::default(),
                        None,
                        None,
                        &BucketAccess::Permissive,
                    )
                    .await
                    .unwrap();
            }
            ChangeKind::Iam => {
                catalog
                    .set_policy(
                        "other-bucket",
                        crate::auth::Policy::default_for("project"),
                        &BucketAccess::Permissive,
                    )
                    .await
                    .unwrap();
            }
            ChangeKind::BucketDelete => {
                catalog
                    .delete_bucket("other-bucket", None, None)
                    .await
                    .unwrap();
            }
        }
        let page = catalog.read_changes(Some(cursor), None, 256).await.unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].kind, kind);
        for (hook, wait) in waits {
            assert_eq!(finish(hook, wait).await.fence, page.fence);
        }
        cursor = page.fence;
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_observation_invalid_gap_disabled_epoch_and_restart_cursors_require_rescan() {
    let (dir, catalog) = setup().await;
    let disabled = catalog.read_changes(None, None, 1).await.unwrap().fence;
    assert!(
        catalog
            .wait_for_changes(disabled)
            .await
            .unwrap()
            .rescan_required
    );
    let baseline = catalog.set_change_journal_enabled(true).await.unwrap();
    for invalid in [
        disabled,
        JournalFence {
            sequence: JournalSeq(u64::MAX),
            ..baseline
        },
        JournalFence {
            revision: -1,
            ..baseline
        },
    ] {
        assert!(
            catalog
                .wait_for_changes(invalid)
                .await
                .unwrap()
                .rescan_required
        );
    }
    for change in ["disable", "enable", "parent-pap"] {
        let current = catalog
            .set_change_journal_enabled(change != "enable")
            .await
            .unwrap();
        let phase = if change == "enable" {
            "registered"
        } else {
            "checked"
        };
        let (hook, wait) = paused_wait(&catalog, current, phase).await;
        if change == "parent-pap" {
            catalog
                .configure_public_access_prevention(&["project".into()])
                .await
                .unwrap();
        } else {
            catalog
                .set_change_journal_enabled(change == "enable")
                .await
                .unwrap();
        }
        let observed = finish(hook, wait).await;
        assert!(observed.rescan_required);
        assert!(observed.fence.revision > current.revision);
    }
    let current = catalog.set_change_journal_enabled(true).await.unwrap();
    catalog
        .run_admin(|connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            transaction
                .execute_batch(
                    "UPDATE change_journal_state SET sequence = 1, floor = 1 WHERE singleton = 1",
                )
                .map_err(unavailable)?;
            transaction.commit().map_err(unavailable)
        })
        .await
        .unwrap();
    assert!(
        catalog
            .wait_for_changes(current)
            .await
            .unwrap()
            .rescan_required
    );
    let before_restart = catalog.read_changes(None, None, 1).await.unwrap().fence;
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert!(
        reopened
            .wait_for_changes(before_restart)
            .await
            .unwrap()
            .rescan_required
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn sync_observation_cancel_rollback_false_wake_and_shutdown_hold_no_permits() {
    let (_dir, catalog) = setup().await;
    let baseline = catalog.set_change_journal_enabled(true).await.unwrap();
    let (hook, mut wait) = paused_wait(&catalog, baseline, "checked").await;
    assert_eq!(catalog.0.admin_reads.available_permits(), 32);
    assert_eq!(catalog.0.execution.available_permits(), 1);
    let api = catalog
        .0
        .admission
        .clone()
        .acquire_many_owned(32)
        .await
        .unwrap();
    let uploads = catalog
        .0
        .uploads
        .clone()
        .acquire_many_owned(8)
        .await
        .unwrap();
    assert!(catalog.get_bucket("test-bucket").await.is_err());
    catalog
        .run_admin(|connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            transaction
                .execute(
                    "UPDATE change_journal_state SET sequence = sequence + 1",
                    [],
                )
                .map_err(unavailable)?;
            transaction.rollback().map_err(unavailable)
        })
        .await
        .unwrap();
    catalog.0.change_wake.notify_waiters();
    hook.release.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut wait)
            .await
            .is_err()
    );
    assert!(hook.checks.load(std::sync::atomic::Ordering::SeqCst) <= 2);
    assert_eq!(
        catalog
            .read_changes(Some(baseline), None, 1)
            .await
            .unwrap()
            .fence,
        baseline
    );
    wait.abort();
    assert!(wait.await.unwrap_err().is_cancelled());
    let (hook, wait) = paused_wait(&catalog, baseline, "checked").await;
    publish(&catalog, Origin::SyncIngest, "independent").await;
    assert!(!finish(hook, wait).await.rescan_required);
    drop(api);
    drop(uploads);
    let current = catalog.read_changes(None, None, 1).await.unwrap().fence;
    let (hook, wait) = paused_wait(&catalog, current, "checked").await;
    catalog.close().await.unwrap();
    hook.release.notify_one();
    assert_eq!(wait.await.unwrap(), Err(AdminError::Closed));
}
