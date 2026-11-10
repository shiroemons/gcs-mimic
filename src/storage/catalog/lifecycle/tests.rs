use super::*;
use crate::{
    bucket::{BucketMetadataUpdate, SoftDeletePolicy},
    clock::{Clock, ManualClock},
    object::{
        ObjectMetadataUpdate,
        retention::{ObjectRetention, RetentionMode},
    },
    storage::{
        admin::LocalAdmin,
        catalog::{
            fault::{self, Crash, Fault, Step},
            test_fixtures::{
                PERMISSIVE, PERMISSIVE_RESTORE, body, bucket, live, permissive_upload, version,
            },
        },
    },
    test_support::TestDir,
};
use bytes::Bytes;
use serde_json::{Value, json};
use std::{io, sync::Arc, time::UNIX_EPOCH};

const DAY: Duration = Duration::from_secs(86_400);

async fn disk(data: &TestDir) -> (Catalog, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH + DAY));
    let catalog = Catalog::open_with_clock(&data.0, clock.clone())
        .await
        .unwrap();
    (catalog, clock)
}

async fn memory() -> (Catalog, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH + DAY));
    let catalog = Catalog::open_memory_for_server(
        64 * 1024 * 1024,
        16 * 1024 * 1024,
        clock.clone(),
        LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    (catalog, clock)
}

fn delete(condition: Value) -> Value {
    json!({"action":{"type":"Delete"},"condition":condition})
}

async fn rules(catalog: &Catalog, rules: Vec<Value>) {
    catalog
        .patch_bucket(
            "bucket",
            BucketMetadataUpdate {
                lifecycle: Some(rules),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
}

async fn create(catalog: &Catalog, rules: Vec<Value>) {
    let mut record = bucket("bucket", "project");
    record.lifecycle = rules;
    catalog.insert_bucket(record).await.unwrap();
}

async fn put(catalog: &Catalog, name: &str) -> ObjectRecord {
    catalog
        .put_object(
            permissive_upload("bucket", name, 100),
            body(vec![Bytes::from_static(b"data")]),
        )
        .await
        .unwrap()
}

async fn count(catalog: &Catalog, query: &'static str) -> i64 {
    catalog
        .run::<_, CatalogError>(move |connection| Ok(sql::scalar(connection, query, [])?))
        .await
        .unwrap()
}

async fn exists(catalog: &Catalog, target: ObjectTarget) -> bool {
    match catalog
        .get_object(target, Default::default(), PERMISSIVE)
        .await
    {
        Ok(_) => true,
        Err(ObjectError::ObjectNotFound) => false,
        Err(error) => panic!("unexpected read failure: {error}"),
    }
}

#[tokio::test]
async fn unversioned_delete_reclaims_the_body_on_the_shared_maintenance_pass() {
    let data = TestDir::new();
    let (catalog, clock) = disk(&data).await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    put(&catalog, "object").await;
    clock.advance(DAY);
    catalog.maintain_resumable_sessions().await.unwrap();
    assert!(!exists(&catalog, live("bucket", "object")).await);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM pending_blob_deletions").await,
        0
    );
    assert_eq!(
        std::fs::read_dir(data.0.join("blobs"))
            .unwrap()
            .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
            .sum::<usize>(),
        0
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn lifecycle_soft_delete_preserves_bytes_and_does_not_touch_retired_versions() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.lifecycle = vec![delete(json!({"age":1}))];
    record.soft_delete_policy = Some(SoftDeletePolicy {
        retention_duration_seconds: 604800,
        effective_time: clock.now(),
    });
    catalog.insert_bucket(record).await.unwrap();
    let object = put(&catalog, "object").await;
    clock.advance(DAY);
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().deleted, 1);
    let deleted = catalog
        .get_soft_deleted_object(
            ObjectTarget::new("bucket", "object", object.generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(deleted.soft_delete_time, Some(clock.now()));
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().scanned, 0);
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn versioned_live_archives_and_the_next_pass_removes_the_noncurrent_generation() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.versioning_enabled = true;
    record.lifecycle = vec![delete(json!({"age":1}))];
    catalog.insert_bucket(record).await.unwrap();
    let object = put(&catalog, "object").await;
    clock.advance(DAY);
    let first = catalog.run_lifecycle_now().await.unwrap();
    assert_eq!((first.archived, first.deleted), (1, 0));
    assert!(!exists(&catalog, live("bucket", "object")).await);
    let archived = catalog
        .get_object(
            version("bucket", "object", object.generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(archived.time_deleted, Some(clock.now()));
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().deleted, 1);
    assert!(!exists(&catalog, version("bucket", "object", object.generation)).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn only_matching_noncurrent_generations_are_removed_with_newer_versions_across_pages() {
    let (catalog, _clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.versioning_enabled = true;
    catalog.insert_bucket(record).await.unwrap();
    let oldest = put(&catalog, "object").await;
    for _ in 0..129 {
        put(&catalog, "object").await;
    }
    rules(
        &catalog,
        vec![delete(json!({"isLive":false,"numNewerVersions":129}))],
    )
    .await;
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().deleted, 1);
    assert!(!exists(&catalog, version("bucket", "object", oldest.generation)).await);
    assert!(exists(&catalog, live("bucket", "object")).await);
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 129);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn releasing_each_hold_allows_a_matching_delete() {
    for metadata in [
        ObjectMetadataUpdate {
            temporary_hold: Some(true),
            ..Default::default()
        },
        ObjectMetadataUpdate {
            event_based_hold: Some(true),
            ..Default::default()
        },
    ] {
        let (catalog, clock) = memory().await;
        create(&catalog, vec![delete(json!({"age":1}))]).await;
        catalog
            .put_object(
                crate::storage::ObjectUpload {
                    metadata,
                    ..permissive_upload("bucket", "object", 100)
                },
                body(vec![Bytes::from_static(b"held")]),
            )
            .await
            .unwrap();
        clock.advance(DAY);
        assert_eq!(catalog.run_lifecycle_now().await.unwrap().blocked, 1);
        assert!(exists(&catalog, live("bucket", "object")).await);
        catalog
            .update_object_metadata(
                live("bucket", "object"),
                Default::default(),
                ObjectMetadataUpdate {
                    event_based_hold: Some(false),
                    temporary_hold: Some(false),
                    ..Default::default()
                },
                |_| Ok(()),
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert_eq!(catalog.run_lifecycle_now().await.unwrap().deleted, 1);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn bucket_and_object_retention_delay_delete_until_the_later_exact_expiry() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.lifecycle = vec![delete(json!({"age":1}))];
    record.object_retention_enabled = true;
    record.retention_policy = Some(crate::bucket::RetentionPolicy {
        retention_period: 86_410,
        effective_time: clock.now(),
        is_locked: false,
    });
    catalog.insert_bucket(record).await.unwrap();
    let until = clock.now() + DAY + Duration::from_secs(20);
    catalog
        .put_object(
            crate::storage::ObjectUpload {
                metadata: ObjectMetadataUpdate {
                    retention: Some(Some(ObjectRetention {
                        mode: RetentionMode::Locked,
                        retain_until: until.into(),
                    })),
                    ..Default::default()
                },
                ..permissive_upload("bucket", "object", 100)
            },
            body(vec![Bytes::from_static(b"retained")]),
        )
        .await
        .unwrap();
    clock.advance(DAY);
    let result = catalog.run_lifecycle_now().await.unwrap();
    assert_eq!(result.blocked, 1);
    assert_eq!(result.next, Some(until));
    clock.set(until - Duration::from_micros(1));
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(until)
    );
    clock.set(until);
    catalog.maintain_resumable_sessions().await.unwrap();
    assert!(!exists(&catalog, live("bucket", "object")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn restored_objects_restart_lifecycle_age_at_their_new_creation_time() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.lifecycle = vec![delete(json!({"age":1}))];
    record.soft_delete_policy = Some(SoftDeletePolicy {
        retention_duration_seconds: 604800,
        effective_time: clock.now(),
    });
    catalog.insert_bucket(record).await.unwrap();
    let object = put(&catalog, "object").await;
    clock.advance(DAY);
    catalog.run_lifecycle_now().await.unwrap();
    let restored = catalog
        .restore_object(
            ObjectTarget::new("bucket", "object", object.generation),
            Default::default(),
            PERMISSIVE_RESTORE,
        )
        .await
        .unwrap();
    assert_eq!(restored.time_created, clock.now());
    let next = clock.now() + DAY;
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().next, Some(next));
    clock.set(next);
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().deleted, 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn no_rules_stays_disarmed_and_early_wakes_do_not_scan() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![]).await;
    put(&catalog, "object").await;
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert_eq!(catalog.0.lifecycle.state().scans, 0);
    rules(&catalog, vec![delete(json!({"age":1}))]).await;
    let due = clock.now() + DAY;
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(due)
    );
    let scans = catalog.0.lifecycle.state().scans;
    clock.advance(Duration::from_secs(10));
    for _ in 0..3 {
        assert_eq!(
            catalog.maintain_resumable_sessions().await.unwrap(),
            Some(due)
        );
    }
    assert_eq!(catalog.0.lifecycle.state().scans, scans);
    clock.set(due - Duration::from_micros(1));
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(due)
    );
    clock.set(due);
    catalog.maintain_resumable_sessions().await.unwrap();
    assert!(!exists(&catalog, live("bucket", "object")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writes_coalesce_for_one_second_and_removing_rules_disarms_the_schedule() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    catalog.maintain_resumable_sessions().await.unwrap();
    put(&catalog, "first").await;
    let gate = clock.now() + Duration::from_secs(1);
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(gate)
    );
    assert_eq!(catalog.0.lifecycle.state().scans, 1);
    clock.set(gate);
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(UNIX_EPOCH + DAY + DAY)
    );
    rules(&catalog, vec![]).await;
    clock.advance(Duration::from_secs(1));
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    let scans = catalog.0.lifecycle.state().scans;
    put(&catalog, "second").await;
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert_eq!(catalog.0.lifecycle.state().scans, scans);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn the_action_cap_returns_now_and_the_following_pass_finishes_the_backlog() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    for index in 0..1025 {
        put(&catalog, &format!("object-{index:04}")).await;
    }
    clock.advance(DAY);
    let first = catalog.run_lifecycle_now().await.unwrap();
    assert_eq!(first.deleted, 1024);
    assert_eq!(first.next, Some(clock.now()));
    let second = catalog.run_lifecycle(Pass::Scheduled).await.unwrap();
    assert_eq!(second.deleted, 1);
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_selected_storage_class_action_is_unchanged_and_has_no_immediate_wakeup() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":1}})]).await;
    let object = put(&catalog, "object").await;
    clock.advance(DAY);
    let outcome = catalog.run_lifecycle_now().await.unwrap();
    assert_eq!(
        (outcome.deleted, outcome.archived, outcome.next),
        (0, 0, None)
    );
    assert_eq!(
        catalog
            .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap()
            .storage_class,
        object.storage_class
    );
    let scans = catalog.0.lifecycle.state().scans;
    catalog.maintain_resumable_sessions().await.unwrap();
    assert_eq!(catalog.0.lifecycle.state().scans, scans);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn failed_lifecycle_commit_rolls_back_rows_events_and_journal_then_retries() {
    let data = TestDir::new();
    let (catalog, clock) = disk(&data).await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    notifications(&catalog).await;
    put(&catalog, "object").await;
    clock.advance(DAY);
    fault::arm(
        &data.0,
        Step::LifecycleCommit,
        0,
        Fault::Io(io::ErrorKind::Other),
    );
    let error = catalog.maintain_resumable_sessions().await.unwrap_err();
    assert!(std::error::Error::source(&error).is_some());
    assert!(catalog.resumable_maintenance_error().is_some());
    assert!(exists(&catalog, live("bucket", "object")).await);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM pending_blob_deletions").await,
        0
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM notification_outbox").await,
        0
    );
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().deleted, 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn restart_rebuilds_the_schedule_from_stored_rules_and_recomputes_the_deadline() {
    let data = TestDir::new();
    let (catalog, clock) = disk(&data).await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    put(&catalog, "object").await;
    catalog.run_lifecycle_now().await.unwrap();
    catalog.close().await.unwrap();
    let catalog = Catalog::open_with_clock(&data.0, clock.clone())
        .await
        .unwrap();
    assert!(catalog.0.lifecycle.armed.load(Ordering::Acquire));
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(clock.now() + DAY)
    );
    clock.advance(DAY);
    catalog.maintain_resumable_sessions().await.unwrap();
    assert!(!exists(&catalog, live("bucket", "object")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn disabled_execution_preserves_rules_and_objects_even_on_a_forced_pass() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    put(&catalog, "object").await;
    catalog.configure_lifecycle(&crate::config::Config {
        disable_lifecycle: true,
        ..Default::default()
    });
    clock.advance(DAY);
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().scanned, 0);
    assert!(exists(&catalog, live("bucket", "object")).await);
    assert!(
        !catalog
            .get_bucket("bucket", PERMISSIVE)
            .await
            .unwrap()
            .unwrap()
            .lifecycle
            .is_empty()
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn the_version_scan_seeks_the_primary_key_without_a_temporary_sort() {
    let (catalog, _clock) = memory().await;
    let plans = catalog.run::<_, CatalogError>(|connection| {
        Ok(connection.prepare_cached("EXPLAIN QUERY PLAN SELECT name, generation FROM objects WHERE bucket = ?1 AND soft_delete_time IS NULL AND (name, generation) < (?2, ?3) ORDER BY name DESC, generation DESC LIMIT 128")?.query_map(params!["bucket", "object", 1], |row| row.get::<_, String>(3))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await.unwrap();
    println!("lifecycle version scan: {plans:?}");
    assert!(
        plans
            .iter()
            .any(|plan| plan.contains("SEARCH objects USING PRIMARY KEY")),
        "{plans:?}"
    );
    assert!(
        !plans
            .iter()
            .any(|plan| plan.contains("TEMP B-TREE") || plan.starts_with("SCAN objects")),
        "{plans:?}"
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_broken_stored_rule_reports_failure_without_stopping_other_buckets() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    put(&catalog, "good").await;
    let mut broken = bucket("aaa-broken", "project");
    broken.lifecycle = vec![delete(json!({"age":1}))];
    catalog.insert_bucket(broken).await.unwrap();
    catalog
        .put_object(
            permissive_upload("aaa-broken", "object", 100),
            body(vec![Bytes::from_static(b"data")]),
        )
        .await
        .unwrap();
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec(
                connection,
                "UPDATE buckets SET lifecycle = '[{}]' WHERE name = 'aaa-broken'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    clock.advance(DAY);
    assert!(catalog.maintain_resumable_sessions().await.is_err());
    assert!(!exists(&catalog, live("bucket", "good")).await);
    assert!(exists(&catalog, live("aaa-broken", "object")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn lifecycle_crash_and_power_loss_snapshots_restart_idempotently() {
    for crash in Crash::ALL {
        for step in [Step::LifecycleCommit, Step::LifecycleCommitted] {
            let data = TestDir::new();
            let snapshot = TestDir::new();
            let (catalog, clock) = disk(&data).await;
            create(&catalog, vec![delete(json!({"age":1}))]).await;
            notifications(&catalog).await;
            put(&catalog, "object").await;
            clock.advance(DAY);
            fault::arm(&data.0, step, 0, crash.fault(snapshot.0.clone()));
            assert_eq!(catalog.run_lifecycle_now().await.unwrap().deleted, 1);
            assert_eq!(fault::pending(&data.0), 0);
            catalog.close().await.unwrap();
            let recovered = Catalog::open_with_clock(&snapshot.0, clock).await.unwrap();
            assert_eq!(
                recovered.run_lifecycle_now().await.unwrap().deleted,
                u64::from(step == Step::LifecycleCommit)
            );
            assert_eq!(recovered.run_lifecycle_now().await.unwrap().deleted, 0);
            assert_eq!(count(&recovered, "SELECT count(*) FROM objects").await, 0);
            assert_eq!(
                count(&recovered, "SELECT count(*) FROM notification_outbox").await,
                1
            );
            recovered.maintain_resumable_sessions().await.unwrap();
            assert_eq!(
                count(&recovered, "SELECT count(*) FROM pending_blob_deletions").await,
                0
            );
            recovered.close().await.unwrap();
        }
    }
}

async fn notifications(catalog: &Catalog) {
    catalog
        .insert_notification(
            "bucket",
            crate::notification::NotificationConfig {
                topic: "//pubsub.googleapis.com/projects/project/topics/topic".into(),
                payload_format: "JSON_API_V1".into(),
                event_types: vec!["OBJECT_DELETE".into(), "OBJECT_ARCHIVE".into()],
                custom_attributes: Default::default(),
                object_name_prefix: String::new(),
            },
            PERMISSIVE,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn lifecycle_emits_one_archive_then_one_delete_in_the_existing_outbox() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.versioning_enabled = true;
    record.lifecycle = vec![delete(json!({"age":1}))];
    catalog.insert_bucket(record).await.unwrap();
    notifications(&catalog).await;
    put(&catalog, "object").await;
    clock.advance(DAY);
    catalog.run_lifecycle_now().await.unwrap();
    catalog.run_lifecycle_now().await.unwrap();
    for event in ["OBJECT_ARCHIVE", "OBJECT_DELETE"] {
        let pending = catalog.next_notification().await.unwrap().unwrap();
        let message: Value = serde_json::from_str(&pending.message).unwrap();
        assert_eq!(message["messages"][0]["attributes"]["eventType"], event);
        catalog.acknowledge_notification(pending.id).await.unwrap();
    }
    assert!(catalog.next_notification().await.unwrap().is_none());
    catalog.close().await.unwrap();
}

async fn candidate(
    catalog: &Catalog,
    target: ObjectTarget,
    incarnation: String,
) -> LifecycleOutcome {
    let now = catalog.0.clock.now();
    catalog
        .run::<_, CatalogError>(move |connection| {
            apply_version(connection, &target, &incarnation, now).map_err(unavailable)
        })
        .await
        .unwrap()
}

async fn incarnation(catalog: &Catalog) -> String {
    catalog
        .run::<_, CatalogError>(|connection| {
            Ok(super::super::bucket_incarnation(connection, "bucket")?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn stale_candidates_recheck_overwrites_rule_removal_and_hold_changes_before_deletion() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    let old = put(&catalog, "object").await;
    let incarnation = incarnation(&catalog).await;
    clock.advance(DAY);
    let fresh = put(&catalog, "object").await;
    assert_eq!(
        candidate(
            &catalog,
            version("bucket", "object", old.generation),
            incarnation.clone()
        )
        .await
        .deleted,
        0
    );
    clock.advance(DAY);
    rules(&catalog, vec![]).await;
    assert_eq!(
        candidate(
            &catalog,
            version("bucket", "object", fresh.generation),
            incarnation.clone()
        )
        .await
        .deleted,
        0
    );
    rules(&catalog, vec![delete(json!({"age":1}))]).await;
    catalog
        .update_object_metadata(
            live("bucket", "object"),
            Default::default(),
            ObjectMetadataUpdate {
                temporary_hold: Some(true),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(
        candidate(
            &catalog,
            version("bucket", "object", fresh.generation),
            incarnation
        )
        .await
        .blocked,
        1
    );
    assert!(exists(&catalog, live("bucket", "object")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_bucket_recreated_between_selection_and_application_rejects_the_old_incarnation() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    let previous = incarnation(&catalog).await;
    catalog
        .delete_bucket("bucket", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    let object = put(&catalog, "object").await;
    clock.advance(DAY);
    assert_eq!(
        candidate(
            &catalog,
            version("bucket", "object", object.generation),
            previous
        )
        .await
        .deleted,
        0
    );
    assert!(exists(&catalog, live("bucket", "object")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_low_level_bucket_publication_rearms_without_any_caller_specific_wakeup() {
    let (catalog, clock) = memory().await;
    assert!(!catalog.0.lifecycle.armed.load(Ordering::Acquire));
    catalog
        .run::<_, CatalogError>(|connection| {
            let transaction = sql::immediate(connection)?;
            let mut record = bucket("bucket", "project");
            record.lifecycle = vec![delete(json!({"age":1}))];
            super::super::insert_bucket_in_transaction(&transaction, &mut record, None)?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(catalog.0.lifecycle.armed.load(Ordering::Acquire));
    put(&catalog, "object").await;
    clock.advance(DAY);
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().deleted, 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn administrative_ensure_import_seed_and_initialization_all_arm_the_shared_schedule() {
    use super::super::{
        admin::{
            BucketDefinition, Origin, SeedMode,
            initialization::{
                Creation, EntryDigest, InitTarget, MemberBits, Planned, RecordedEntry,
            },
        },
        import_journal::{ImportBinding, ImportElement},
    };
    for path in ["ensure", "import", "seed", "initialize"] {
        let (catalog, clock) = memory().await;
        let mut record = bucket("bucket", "project");
        record.lifecycle = vec![delete(json!({"age":1}))];
        let definition = BucketDefinition::new(record).unwrap();
        match path {
            "ensure" => {
                catalog
                    .admin(Origin::Seed)
                    .ensure_bucket(definition)
                    .await
                    .unwrap();
            }
            "import" => {
                let binding = ImportBinding {
                    source: [1; 32],
                    fingerprint: [2; 32],
                    expected_elements: 1,
                };
                catalog
                    .import_journal()
                    .begin(binding.clone())
                    .await
                    .unwrap();
                catalog
                    .admin(Origin::Import)
                    .publish_import_bucket(
                        binding,
                        0,
                        ImportElement {
                            ordinal: 1,
                            fingerprint: [3; 32],
                            inode: None,
                            ctime_ns: None,
                        },
                        definition,
                    )
                    .await
                    .unwrap();
            }
            "seed" => {
                let mut batch = catalog.admin(Origin::Seed).seed_batch().await.unwrap();
                batch.bucket(definition).unwrap();
                batch.commit(SeedMode::Merge).await.unwrap();
            }
            "initialize" => {
                catalog
                    .admin(Origin::BucketInit)
                    .initialize_buckets(
                        vec![InitTarget {
                            name: "bucket".into(),
                            project: "project".into(),
                        }],
                        move |_| {
                            Ok::<_, super::super::admin::AdminError>((
                                vec![Planned::Create(Box::new(Creation {
                                    definition,
                                    policy: None,
                                    entry: RecordedEntry {
                                        digest: EntryDigest::new([0; 32]),
                                        members: MemberBits::new(0),
                                    },
                                }))],
                                (),
                            ))
                        },
                    )
                    .await
                    .unwrap();
            }
            _ => panic!("unknown publication path"),
        }
        assert!(catalog.0.lifecycle.armed.load(Ordering::Acquire), "{path}");
        put(&catalog, "object").await;
        clock.advance(DAY);
        assert_eq!(
            catalog.run_lifecycle_now().await.unwrap().deleted,
            1,
            "{path}"
        );
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn retention_deadlines_replace_an_earlier_future_age_deadline_and_block_live_archiving() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.versioning_enabled = true;
    record.object_retention_enabled = true;
    record.lifecycle = vec![delete(json!({"age":1}))];
    catalog.insert_bucket(record).await.unwrap();
    let until = clock.now() + DAY + DAY;
    catalog
        .put_object(
            crate::storage::ObjectUpload {
                metadata: ObjectMetadataUpdate {
                    retention: Some(Some(ObjectRetention {
                        mode: RetentionMode::Locked,
                        retain_until: until.into(),
                    })),
                    ..Default::default()
                },
                ..permissive_upload("bucket", "object", 100)
            },
            body(vec![Bytes::from_static(b"retained")]),
        )
        .await
        .unwrap();
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().next, Some(until));
    clock.advance(DAY);
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().blocked, 1);
    assert!(exists(&catalog, live("bucket", "object")).await);
    clock.set(until);
    assert_eq!(catalog.run_lifecycle_now().await.unwrap().archived, 1);
    catalog.close().await.unwrap();
}
