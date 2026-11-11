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

mod boundaries;
mod transitions;

const DAY: Duration = Duration::from_secs(86_400);

async fn disk(data: &TestDir) -> (Catalog, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH + DAY));
    let catalog = Catalog::open_with_clock(&data.0, clock.clone())
        .await
        .unwrap();
    catalog.configure_lifecycle(&Default::default());
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
    catalog.configure_lifecycle(&Default::default());
    (catalog, clock)
}

async fn execute(catalog: &Catalog) -> LifecycleCounts {
    match catalog.run_lifecycle_now().await.unwrap() {
        LifecycleOutcome::Executed(counts) => counts,
        LifecycleOutcome::Disabled => panic!("lifecycle execution is disabled"),
    }
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
    assert_eq!(execute(&catalog).await.deleted, 1);
    let deleted = catalog
        .get_soft_deleted_object(
            ObjectTarget::new("bucket", "object", object.generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(deleted.soft_delete_time, Some(clock.now()));
    assert_eq!(execute(&catalog).await.scanned, 0);
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
    let first = execute(&catalog).await;
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
    assert_eq!(execute(&catalog).await.deleted, 1);
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
    assert_eq!(execute(&catalog).await.deleted, 1);
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
        assert_eq!(execute(&catalog).await.blocked, 1);
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
        assert_eq!(execute(&catalog).await.deleted, 1);
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
    let result = execute(&catalog).await;
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
    assert_eq!(execute(&catalog).await.next, Some(next));
    clock.set(next);
    assert_eq!(execute(&catalog).await.deleted, 1);
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
    let first = execute(&catalog).await;
    assert_eq!(first.deleted, 1024);
    assert_eq!(first.next, Some(clock.now()));
    let LifecycleOutcome::Executed(second) = catalog.run_lifecycle(Pass::Scheduled).await.unwrap()
    else {
        panic!("lifecycle execution is disabled");
    };
    assert_eq!(second.deleted, 1);
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_selected_storage_class_action_updates_metadata_once() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":1}})]).await;
    let object = put(&catalog, "object").await;
    clock.advance(DAY);
    let outcome = execute(&catalog).await;
    assert_eq!(
        (
            outcome.deleted,
            outcome.archived,
            outcome.transitioned,
            outcome.next
        ),
        (0, 0, 1, Some(clock.now() + COALESCE))
    );
    assert_eq!(
        catalog
            .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap()
            .storage_class,
        StorageClass::Coldline
    );
    let current = catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(current.generation, object.generation);
    assert_eq!(current.metageneration, object.metageneration + 1);
    assert_eq!(current.time_created, object.time_created);
    assert_eq!(current.updated, clock.now());
    assert_eq!(current.storage_class_updated(), clock.now());
    assert_ne!(
        crate::json::object_etag(&current),
        crate::json::object_etag(&object)
    );
    assert_eq!(execute(&catalog).await.transitioned, 0);
    let scans = catalog.0.lifecycle.state().scans;
    catalog.maintain_resumable_sessions().await.unwrap();
    assert_eq!(catalog.0.lifecycle.state().scans, scans);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn applied_transitions_prevent_later_old_class_deletion_but_keep_independent_deletes() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![
        json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"age":30}}),
        delete(json!({"age":60,"matchesStorageClass":["STANDARD"]})),
        delete(json!({"age":90})),
    ]).await;
    put(&catalog, "object").await;
    let due = clock.now() + DAY * 90;
    clock.advance(DAY * 30);
    let transition = execute(&catalog).await;
    assert_eq!(transition.transitioned, 1);
    assert_eq!(transition.next, Some(clock.now() + COALESCE));
    clock.advance(DAY * 30);
    let deferred = execute(&catalog).await;
    assert_eq!(deferred.deleted, 0);
    assert_eq!(deferred.next, Some(due));
    assert!(exists(&catalog, live("bucket", "object")).await);
    clock.set(due);
    assert_eq!(execute(&catalog).await.deleted, 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn replacing_applied_transition_rules_preserves_the_new_class_until_a_matching_delete() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![json!({"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"age":30}})]).await;
    let object = put(&catalog, "object").await;
    clock.advance(DAY * 30);
    assert_eq!(execute(&catalog).await.transitioned, 1);
    rules(
        &catalog,
        vec![delete(json!({"age":60,"matchesStorageClass":["STANDARD"]}))],
    )
    .await;
    assert_eq!(execute(&catalog).await.next, None);
    clock.advance(DAY * 60);
    let outcome = execute(&catalog).await;
    assert_eq!(
        (outcome.deleted, outcome.archived, outcome.next),
        (0, 0, None)
    );
    let current = catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(
        (current.generation, current.storage_class),
        (object.generation, StorageClass::Nearline)
    );
    rules(
        &catalog,
        vec![delete(json!({"age":60,"matchesStorageClass":["NEARLINE"]}))],
    )
    .await;
    assert_eq!(execute(&catalog).await.deleted, 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn creation_age_zero_and_strict_creation_dates_drive_exact_delete_deadlines() {
    for age in [0, 1] {
        let (catalog, clock) = memory().await;
        clock.advance(Duration::from_micros(123));
        create(
            &catalog,
            vec![delete(json!({"age":age,"createdBefore":"1970-01-03"}))],
        )
        .await;
        put(&catalog, "before-date").await;
        let due = if age == 0 {
            UNIX_EPOCH + DAY * 2
        } else {
            clock.now() + DAY
        };
        assert_eq!(execute(&catalog).await.next, Some(due));
        clock.set(due - Duration::from_micros(1));
        let before = execute(&catalog).await;
        assert_eq!((before.deleted, before.next), (0, Some(due)));
        clock.set(due);
        assert_eq!(execute(&catalog).await.deleted, 1);
        clock.set(UNIX_EPOCH + DAY * 2);
        put(&catalog, "exact-date").await;
        clock.advance(DAY * 10);
        let excluded = execute(&catalog).await;
        assert_eq!((excluded.deleted, excluded.next), (0, None));
        assert!(exists(&catalog, live("bucket", "exact-date")).await);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn custom_time_conditions_require_metadata_and_delete_at_its_exact_deadline() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![delete(
            json!({"daysSinceCustomTime":1,"customTimeBefore":"1970-01-04"}),
        )],
    )
    .await;
    put(&catalog, "unset").await;
    put(&catalog, "dated").await;
    catalog
        .update_object_metadata(
            live("bucket", "dated"),
            Default::default(),
            ObjectMetadataUpdate {
                custom_time: Some(Some("1970-01-02T12:00:00.000123Z".into())),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let due = UNIX_EPOCH + DAY * 2 + Duration::from_secs(43_200) + Duration::from_micros(123);
    assert_eq!(execute(&catalog).await.next, Some(due));
    clock.set(due - Duration::from_micros(1));
    assert_eq!(execute(&catalog).await.deleted, 0);
    clock.set(due);
    assert_eq!(execute(&catalog).await.deleted, 1);
    assert!(exists(&catalog, live("bucket", "unset")).await);
    put(&catalog, "exact-date").await;
    catalog
        .update_object_metadata(
            live("bucket", "exact-date"),
            Default::default(),
            ObjectMetadataUpdate {
                custom_time: Some(Some("1970-01-04T00:00:00Z".into())),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    clock.advance(DAY * 10);
    let excluded = execute(&catalog).await;
    assert_eq!((excluded.deleted, excluded.next), (0, None));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn noncurrent_time_conditions_use_the_archive_instant_and_preserve_live_versions() {
    let (catalog, clock) = memory().await;
    clock.advance(Duration::from_secs(43_200));
    let mut record = bucket("bucket", "project");
    record.versioning_enabled = true;
    catalog.insert_bucket(record).await.unwrap();
    let original = put(&catalog, "object").await;
    clock.advance(DAY);
    let current = put(&catalog, "object").await;
    rules(&catalog, vec![delete(json!({"isLive":false,"numNewerVersions":1,"daysSinceNoncurrentTime":1,"noncurrentTimeBefore":"1970-01-04"}))]).await;
    let due = clock.now() + DAY;
    assert_eq!(execute(&catalog).await.next, Some(due));
    clock.set(due - Duration::from_micros(1));
    assert_eq!(execute(&catalog).await.deleted, 0);
    clock.set(due);
    assert_eq!(execute(&catalog).await.deleted, 1);
    assert!(!exists(&catalog, version("bucket", "object", original.generation)).await);
    assert!(exists(&catalog, version("bucket", "object", current.generation)).await);
    assert_eq!(execute(&catalog).await.next, None);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn name_lists_and_strict_size_conditions_use_published_names_and_data_bytes() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"matchesPrefix":["logs/","archive/"],"matchesSuffix":[".bin",".txt"],"sizeAboveBytes":"3","sizeBelowBytes":"5"}))]).await;
    for (name, size) in [
        ("logs/match.bin", 4),
        ("archive/match.txt", 4),
        ("logs/equal-low.bin", 3),
        ("logs/equal-high.bin", 5),
        ("logs/empty.bin", 0),
        ("Logs/case.bin", 4),
        ("logs/case.BIN", 4),
        ("other/path.bin", 4),
    ] {
        catalog
            .put_object(
                permissive_upload("bucket", name, 100),
                body(vec![Bytes::from(vec![b'x'; size])]),
            )
            .await
            .unwrap();
    }
    catalog
        .update_object_metadata(
            live("bucket", "logs/match.bin"),
            Default::default(),
            ObjectMetadataUpdate {
                metadata: Some(Some(std::collections::BTreeMap::from([(
                    "large".into(),
                    Some("x".repeat(1024)),
                )]))),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let outcome = execute(&catalog).await;
    assert_eq!(
        (outcome.scanned, outcome.deleted, outcome.next),
        (8, 2, clock.now().checked_add(COALESCE))
    );
    assert!(!exists(&catalog, live("bucket", "logs/match.bin")).await);
    assert!(!exists(&catalog, live("bucket", "archive/match.txt")).await);
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 6);
    clock.advance(COALESCE);
    assert_eq!(execute(&catalog).await.next, None);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn an_unconfigured_catalog_never_executes_stored_rules() {
    let data = TestDir::new();
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH + DAY));
    let catalog = Catalog::open_with_clock(&data.0, clock.clone())
        .await
        .unwrap();
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    put(&catalog, "object").await;
    clock.advance(DAY);
    assert!(matches!(
        catalog.run_lifecycle_now().await.unwrap(),
        LifecycleOutcome::Disabled
    ));
    catalog.maintain_resumable_sessions().await.unwrap();
    assert!(exists(&catalog, live("bucket", "object")).await);
    assert_eq!(catalog.0.lifecycle.state().scans, 0);
    catalog.configure_lifecycle(&Default::default());
    assert_eq!(execute(&catalog).await.deleted, 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn parsed_rules_are_reused_across_pages_and_replaced_atomically_when_rules_change() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"age":10}))]).await;
    for index in 0..260 {
        put(&catalog, &format!("object-{index:03}")).await;
    }
    let mut cached = BucketRules::Absent;
    let mut cursor = None;
    let mut allocation = None;
    let incarnation = incarnation(&catalog).await;
    for index in 0..3 {
        if index == 2 {
            rules(&catalog, vec![delete(json!({"age":20}))]).await;
        }
        let now = clock.now();
        let incarnation = incarnation.clone();
        let page = catalog
            .run::<_, CatalogError>(move |connection| {
                sweep_page(
                    connection,
                    "bucket",
                    &incarnation,
                    cursor,
                    now,
                    ACTION_LIMIT,
                    cached,
                )
                .map_err(unavailable)
            })
            .await
            .unwrap();
        let BucketRules::Ready { rules, .. } = &page.rules else {
            panic!("valid stored rules");
        };
        let pointer = rules.as_ptr() as usize;
        if index == 1 {
            assert_eq!(allocation, Some(pointer));
        }
        allocation = Some(pointer);
        assert_eq!(page.outcome.scanned, if index == 2 { 4 } else { 128 });
        assert_eq!(
            page.outcome.next,
            Some(now + DAY * if index == 2 { 20 } else { 10 })
        );
        cursor = page.after;
        cached = page.rules;
    }
    assert!(cursor.is_none());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_mutation_failure_rolls_back_the_page_and_retries_without_quarantining_versions() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    put(&catalog, "z-good").await;
    put(&catalog, "a-fails").await;
    catalog.run::<_, CatalogError>(|connection| {
        sql::exec(connection, "CREATE TEMP TRIGGER fail_lifecycle_delete BEFORE DELETE ON objects WHEN old.name = 'a-fails' BEGIN SELECT RAISE(ABORT, 'injected mutation failure'); END", [])?;
        Ok(())
    }).await.unwrap();
    clock.advance(DAY);
    assert!(catalog.maintain_resumable_sessions().await.is_err());
    assert!(catalog.resumable_maintenance_error().is_some());
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 2);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM lifecycle_quarantine").await,
        0
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM pending_blob_deletions").await,
        0
    );
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec(connection, "DROP TRIGGER fail_lifecycle_delete", [])?;
            Ok(())
        })
        .await
        .unwrap();
    catalog.maintain_resumable_sessions().await.unwrap();
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 0);
    assert!(catalog.resumable_maintenance_error().is_none());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn failed_lifecycle_commit_rolls_back_rows_events_and_journal_then_retries() {
    let data = TestDir::new();
    let (catalog, clock) = disk(&data).await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    notifications(&catalog).await;
    put(&catalog, "object").await;
    put(&catalog, "second").await;
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
    assert!(exists(&catalog, live("bucket", "second")).await);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM pending_blob_deletions").await,
        0
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM notification_outbox").await,
        0
    );
    assert_eq!(execute(&catalog).await.deleted, 2);
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
    catalog.configure_lifecycle(&Default::default());
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
        lifecycle_execution: Some(crate::config::LifecycleExecution::Disabled),
        ..Default::default()
    });
    clock.advance(DAY);
    assert!(matches!(
        catalog.run_lifecycle_now().await.unwrap(),
        LifecycleOutcome::Disabled
    ));
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
    for query in [VERSION_SCAN, VERSION_FIRST] {
        let plans = catalog
            .run::<_, CatalogError>(move |connection| {
                let mut statement =
                    connection.prepare_cached(&format!("EXPLAIN QUERY PLAN {query}"))?;
                let read = |row: &rusqlite::Row<'_>| row.get::<_, String>(3);
                let plans = if query == VERSION_SCAN {
                    statement
                        .query_map(params!["bucket", "object", 1, VERSION_PAGE], read)?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                } else {
                    statement
                        .query_map(params!["bucket", VERSION_PAGE], read)?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                Ok(plans)
            })
            .await
            .unwrap();
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
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_broken_stored_rule_is_quarantined_once_and_api_repair_resumes_execution() {
    if diagnostic_parent(
        "a_broken_stored_rule_is_quarantined_once_and_api_repair_resumes_execution",
        &[("lifecycle_rules_quarantined", 1)],
    ) {
        return;
    }
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
    catalog.start_resumable_maintenance();
    crate::test_support::eventually_async(
        "the healthy bucket to finish lifecycle maintenance",
        || async { !exists(&catalog, live("bucket", "good")).await },
    )
    .await;
    for _ in 0..3 {
        clock.advance(Duration::from_secs(1));
        execute(&catalog).await;
        catalog.maintain_resumable_sessions().await.unwrap();
        assert!(catalog.resumable_maintenance_error().is_none());
        assert!(catalog.background_tasks_running());
    }
    assert!(!exists(&catalog, live("bucket", "good")).await);
    assert!(exists(&catalog, live("aaa-broken", "object")).await);
    catalog
        .patch_bucket(
            "aaa-broken",
            BucketMetadataUpdate {
                lifecycle: Some(vec![delete(json!({"age":1}))]),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(execute(&catalog).await.deleted, 1);
    assert!(!exists(&catalog, live("aaa-broken", "object")).await);
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
            assert_eq!(execute(&catalog).await.deleted, 1);
            assert_eq!(fault::pending(&data.0), 0);
            catalog.close().await.unwrap();
            let recovered = Catalog::open_with_clock(&snapshot.0, clock).await.unwrap();
            recovered.configure_lifecycle(&Default::default());
            assert_eq!(
                execute(&recovered).await.deleted,
                u64::from(step == Step::LifecycleCommit)
            );
            assert_eq!(execute(&recovered).await.deleted, 0);
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
                event_types: vec![
                    "OBJECT_DELETE".into(),
                    "OBJECT_ARCHIVE".into(),
                    "OBJECT_METADATA_UPDATE".into(),
                ],
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
) -> LifecycleCounts {
    let now = catalog.0.clock.now();
    catalog
        .run::<_, CatalogError>(move |connection| {
            let transaction = sql::immediate(connection)?;
            let mut diagnostics = Vec::new();
            let BucketRules::Ready { rules, .. } = bucket_rules(
                &transaction,
                &target.bucket,
                &incarnation,
                BucketRules::Absent,
                &mut diagnostics,
            )
            .map_err(unavailable)?
            else {
                return Ok(LifecycleCounts::default());
            };
            let outcome = evaluate_version(
                &transaction,
                &target,
                &rules,
                now,
                &incarnation,
                &mut diagnostics,
                ACTION_LIMIT,
            )
            .map_err(unavailable)?;
            transaction.commit()?;
            emit_quarantines(diagnostics);
            Ok(outcome)
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
    assert_eq!(execute(&catalog).await.deleted, 1);
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
        assert_eq!(execute(&catalog).await.deleted, 1, "{path}");
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
    assert_eq!(execute(&catalog).await.next, Some(until));
    clock.advance(DAY);
    assert_eq!(execute(&catalog).await.blocked, 1);
    assert!(exists(&catalog, live("bucket", "object")).await);
    clock.set(until);
    assert_eq!(execute(&catalog).await.archived, 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn malformed_versions_are_quarantined_once_while_healthy_pages_finish() {
    if diagnostic_parent(
        "malformed_versions_are_quarantined_once_while_healthy_pages_finish",
        &[("lifecycle_version_quarantined", 2)],
    ) {
        return;
    }
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    for name in ["a-corrupt", "z-corrupt"] {
        put(&catalog, name).await;
    }
    for index in 0..130 {
        put(&catalog, &format!("good-{index:03}")).await;
    }
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec(
                connection,
                "UPDATE objects SET custom_time = 'invalid' WHERE name = 'a-corrupt'",
                [],
            )?;
            sql::exec(
                connection,
                "UPDATE objects SET metadata = '[]' WHERE name = 'z-corrupt'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    clock.advance(DAY);
    catalog.start_resumable_maintenance();
    crate::test_support::eventually_async(
        "healthy versions beyond a corrupt page to be deleted",
        || async { count(&catalog, "SELECT count(*) FROM objects").await == 2 },
    )
    .await;
    for _ in 0..3 {
        let outcome = execute(&catalog).await;
        assert_eq!(
            (outcome.deleted, outcome.skipped, outcome.scanned),
            (0, 2, 2)
        );
        assert!(catalog.resumable_maintenance_error().is_none());
        assert!(catalog.background_tasks_running());
    }
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec(
                connection,
                "UPDATE objects SET custom_time = NULL, metadata = '{}' WHERE name IN ('a-corrupt', 'z-corrupt')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(execute(&catalog).await.deleted, 2);
    catalog.close().await.unwrap();
}

fn diagnostic_parent(test: &str, events: &[(&str, usize)]) -> bool {
    if std::env::var_os("GCSM_TEST_LIFECYCLE_DIAGNOSTIC").is_some() {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("storage::catalog::lifecycle::tests::{test}"),
            "--nocapture",
        ])
        .env("GCSM_TEST_LIFECYCLE_DIAGNOSTIC", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{stderr}\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    let logged: Vec<Value> = stderr
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    for (event, count) in events {
        assert_eq!(
            logged
                .iter()
                .filter(|entry| entry["event"] == *event)
                .count(),
            *count,
            "{stderr}"
        );
    }
    true
}

#[tokio::test]
async fn synchronized_defaults_report_stored_or_new_rules_only_once() {
    if diagnostic_parent(
        "synchronized_defaults_report_stored_or_new_rules_only_once",
        &[("lifecycle_sync_disabled", 2)],
    ) {
        return;
    }
    for stored in [true, false] {
        let tree = TestDir::new();
        let (catalog, _) = memory().await;
        if stored {
            create(&catalog, vec![delete(json!({"age":1}))]).await;
        }
        catalog.configure_lifecycle(&crate::config::Config {
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        });
        if !stored {
            create(&catalog, vec![delete(json!({"age":1}))]).await;
        }
        for age in 2..5 {
            rules(&catalog, vec![delete(json!({"age":age}))]).await;
        }
        assert!(matches!(
            catalog.run_lifecycle_now().await.unwrap(),
            LifecycleOutcome::Disabled
        ));
        catalog.close().await.unwrap();
    }
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "lifecycle rules must be written inside with_connection")]
fn marking_rules_outside_a_catalog_write_scope_fails_the_invariant() {
    rules_written();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn synchronization_keeps_host_files_by_default_and_explicit_execution_deletes_them() {
    use crate::config::{Config, LifecycleExecution};
    for choice in [
        None,
        Some(LifecycleExecution::Enabled),
        Some(LifecycleExecution::Disabled),
    ] {
        let data = TestDir::new();
        let tree = TestDir::new();
        let (catalog, clock) = disk(&data).await;
        create(&catalog, vec![delete(json!({"age":1}))]).await;
        put(&catalog, "object").await;
        let config = Config {
            data_dir: data.0.clone(),
            sync_dir: Some(tree.0.clone()),
            lifecycle_execution: choice,
            ..Default::default()
        };
        catalog.configure_lifecycle(&config);
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        crate::sync::reconcile(&catalog, &root).await.unwrap();
        let host_file = tree.0.join("bucket/object");
        assert_eq!(std::fs::read(&host_file).unwrap(), b"data");
        let task = crate::sync::start(catalog.clone(), root.clone(), &config).unwrap();
        task.rescan().await.unwrap();
        clock.advance(DAY);
        let outcome = catalog.run_lifecycle_now().await.unwrap();
        let enabled = choice == Some(LifecycleExecution::Enabled);
        assert_eq!(matches!(outcome, LifecycleOutcome::Executed(_)), enabled);
        let deleted = match outcome {
            LifecycleOutcome::Disabled => 0,
            LifecycleOutcome::Executed(counts) => counts.deleted,
        };
        assert_eq!(deleted, u64::from(enabled));
        if enabled {
            crate::test_support::eventually_async(
                "lifecycle deletion to reach the synchronized file",
                || async { !host_file.exists() },
            )
            .await;
            assert!(!exists(&catalog, live("bucket", "object")).await);
        } else {
            task.rescan().await.unwrap();
            assert!(exists(&catalog, live("bucket", "object")).await);
            assert_eq!(std::fs::read(&host_file).unwrap(), b"data");
        }
        task.finish().await.unwrap();
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn the_running_maintenance_loop_wakes_on_a_write_after_the_injected_deadline() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"age":1}))]).await;
    put(&catalog, "expired").await;
    let due = clock.now() + DAY;
    catalog.start_resumable_maintenance();
    crate::test_support::eventually_async("the lifecycle deadline to be scheduled", || async {
        catalog.0.lifecycle.state().next == Some(due)
    })
    .await;
    clock.advance(DAY);
    put(&catalog, "fresh").await;
    crate::test_support::eventually_async(
        "the running maintenance loop to delete the expired object",
        || async { !exists(&catalog, live("bucket", "expired")).await },
    )
    .await;
    assert!(exists(&catalog, live("bucket", "fresh")).await);
    catalog.close().await.unwrap();
}
