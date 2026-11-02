use super::*;
use crate::{bucket::BucketMetadataUpdate, clock::Clock, test_support::TestDir};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};

const DAY: u64 = 86_400;
const START: Duration = Duration::from_secs(1_000_000);

struct TestClock(Mutex<SystemTime>);

impl Clock for TestClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().unwrap()
    }
}

impl TestClock {
    fn set(&self, time: SystemTime) {
        *self.0.lock().unwrap() = time;
    }
}

fn record(name: &str) -> BucketRecord {
    BucketRecord {
        name: name.into(),
        project: "project".into(),
        location: "US".into(),
        location_type: "multi-region".into(),
        storage_class: "STANDARD".into(),
        autoclass: None,
        labels: [("team".to_owned(), "storage".to_owned())].into(),
        versioning_enabled: false,
        versioning_configured: false,
        default_event_based_hold: false,
        public_access_prevention_enforced: false,
        uniform_bucket_level_access_locked_time: None,
        lifecycle: Vec::new(),
        retention_policy: None,
        soft_delete_policy: None,
        object_retention_enabled: false,
        cors: Vec::new(),
        metageneration: 1,
        generation: 0,
        time_created: UNIX_EPOCH + START,
        updated: UNIX_EPOCH + START,
    }
}

async fn open(dir: &TestDir) -> (Catalog, Arc<TestClock>) {
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH + START)));
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    (catalog, clock)
}

async fn set_retention(catalog: &Catalog, name: &str, seconds: u64) -> BucketRecord {
    catalog
        .patch_bucket(
            name,
            BucketMetadataUpdate {
                soft_delete_retention: Some(seconds),
                ..Default::default()
            },
            None,
            None,
            &BucketAccess::Permissive,
        )
        .await
        .unwrap()
}

/// Adds one soft-deleted object row. Its blob file does not exist: these tests check catalog
/// state, and reclamation tolerates an already missing file.
async fn retained_object(catalog: &Catalog, bucket: &str, hard_delete: SystemTime) -> i64 {
    let (bucket, hard) = (bucket.to_owned(), unix_micros(hard_delete).unwrap());
    catalog
        .run_admin(move |connection| {
            let generation: i64 = connection
                .query_row(
                    "UPDATE object_generation SET last_generation = last_generation + 1 RETURNING last_generation",
                    [],
                    |row| row.get(0),
                )
                .map_err(unavailable)?;
            connection
                .execute(
                    "INSERT INTO objects (bucket, name, generation, metageneration, content_type, size, md5, crc32c, storage_class, time_created, updated, blob_id, soft_delete_time, hard_delete_time) VALUES (?1, 'object', ?2, 1, 'text/plain', 0, zeroblob(16), 0, 'STANDARD', 0, 0, printf('%032x', ?2), 1, ?3)",
                    params![bucket, generation, hard],
                )
                .map_err(unavailable)?;
            Ok(generation)
        })
        .await
        .unwrap()
}

async fn count(catalog: &Catalog, sql: &'static str) -> i64 {
    catalog
        .run_admin(move |connection| {
            connection
                .query_row(sql, [], |row| row.get(0))
                .map_err(unavailable)
        })
        .await
        .unwrap()
}

async fn delete(catalog: &Catalog, name: &str) -> Result<(), CatalogError> {
    catalog
        .delete_bucket_authorized(name, None, None, &BucketAccess::Permissive)
        .await
}

#[tokio::test]
async fn soft_deleted_bucket_outlives_its_objects_and_expires_through_maintenance() {
    let dir = TestDir::new();
    let (catalog, clock) = open(&dir).await;
    catalog.insert_bucket(record("expiring")).await.unwrap();
    set_retention(&catalog, "expiring", 90 * DAY).await;
    let object_hard = UNIX_EPOCH + START + Duration::from_secs(90 * DAY);
    retained_object(&catalog, "expiring", object_hard).await;
    // A shorter bucket policy cannot end the version before its retained object.
    let live = set_retention(&catalog, "expiring", 7 * DAY).await;
    let deleted_at = UNIX_EPOCH + START + Duration::from_secs(3600);
    clock.set(deleted_at);
    delete(&catalog, "expiring").await.unwrap();
    let access = BucketAccess::Permissive;
    let version = catalog
        .get_soft_deleted_bucket("expiring", live.generation, &access)
        .await
        .unwrap();
    assert_eq!(version.record, live);
    assert_eq!(version.soft_delete_time, deleted_at);
    assert_eq!(version.hard_delete_time, object_hard);
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(object_hard)
    );

    clock.set(object_hard - Duration::from_secs(1));
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(object_hard)
    );
    assert_eq!(
        catalog
            .list_soft_deleted_buckets("project", "", None, 10)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    clock.set(object_hard);
    // Reads hide the expired version before maintenance runs.
    assert_eq!(
        catalog
            .get_soft_deleted_bucket("expiring", live.generation, &access)
            .await,
        Err(CatalogError::NotFound)
    );
    assert!(
        catalog
            .list_soft_deleted_buckets("project", "", None, 10)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        catalog
            .restore_bucket("expiring", live.generation, &access)
            .await,
        Err(CatalogError::NotFound)
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM soft_deleted_buckets").await,
        1
    );
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM soft_deleted_buckets").await,
        0
    );
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 0);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM pending_blob_deletions").await,
        0
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn maintenance_purges_expired_versions_in_bounded_batches() {
    let dir = TestDir::new();
    let (catalog, clock) = open(&dir).await;
    let total = PURGE_BATCH + 3;
    for index in 0..total {
        let name = format!("batch-{index:02}");
        catalog.insert_bucket(record(&name)).await.unwrap();
        set_retention(&catalog, &name, 7 * DAY).await;
        retained_object(&catalog, &name, UNIX_EPOCH + START).await;
        delete(&catalog, &name).await.unwrap();
    }
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, total);
    clock.set(UNIX_EPOCH + START + Duration::from_secs(7 * DAY));
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM soft_deleted_buckets").await,
        0
    );
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn failed_transitions_leave_the_catalog_unchanged() {
    let dir = TestDir::new();
    let (catalog, _clock) = open(&dir).await;
    catalog.insert_bucket(record("atomic")).await.unwrap();
    let live = set_retention(&catalog, "atomic", 7 * DAY).await;
    retained_object(
        &catalog,
        "atomic",
        UNIX_EPOCH + START + Duration::from_secs(DAY),
    )
    .await;
    let inject = |sql: &'static str| {
        let catalog = catalog.clone();
        async move {
            catalog
                .run_admin(move |connection| connection.execute_batch(sql).map_err(unavailable))
                .await
                .unwrap();
        }
    };
    // The objects re-key is the last deletion step, so a failure there must undo the copy.
    inject("CREATE TEMP TRIGGER fail_rekey BEFORE UPDATE OF bucket ON objects BEGIN SELECT RAISE(ABORT, 'injected'); END;").await;
    assert!(matches!(
        delete(&catalog, "atomic").await,
        Err(CatalogError::Unavailable(_))
    ));
    assert_eq!(
        catalog.get_bucket("atomic").await.unwrap(),
        Some(live.clone())
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM soft_deleted_buckets").await,
        0
    );
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM objects WHERE bucket = 'atomic'"
        )
        .await,
        1
    );
    inject("DROP TRIGGER fail_rekey;").await;
    delete(&catalog, "atomic").await.unwrap();

    inject("CREATE TEMP TRIGGER fail_restore BEFORE DELETE ON soft_deleted_buckets BEGIN SELECT RAISE(ABORT, 'injected'); END;").await;
    let access = BucketAccess::Permissive;
    assert!(matches!(
        catalog
            .restore_bucket("atomic", live.generation, &access)
            .await,
        Err(CatalogError::Unavailable(_))
    ));
    assert_eq!(catalog.get_bucket("atomic").await.unwrap(), None);
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM objects WHERE bucket = 'atomic'"
        )
        .await,
        0
    );
    assert!(
        catalog
            .get_soft_deleted_bucket("atomic", live.generation, &access)
            .await
            .is_ok()
    );
    inject("DROP TRIGGER fail_restore;").await;
    let restored = catalog
        .restore_bucket("atomic", live.generation, &access)
        .await
        .unwrap();
    assert_eq!(restored, live);
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM objects WHERE bucket = 'atomic'"
        )
        .await,
        1
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn restore_brings_back_iam_and_acl_with_a_new_policy_revision() {
    use crate::auth::{Binding, Policy};
    let dir = TestDir::new();
    let (catalog, _clock) = open(&dir).await;
    catalog.insert_bucket(record("policy")).await.unwrap();
    let live = set_retention(&catalog, "policy", 7 * DAY).await;
    let access = BucketAccess::Permissive;
    let stored = catalog
        .set_policy(
            "policy",
            Policy {
                bindings: vec![Binding {
                    role: "roles/storage.objectViewer".into(),
                    members: vec!["user:reader@example.test".into()],
                }],
                etag: None,
            },
            &access,
        )
        .await
        .unwrap();
    let live = catalog.get_bucket("policy").await.unwrap().unwrap_or(live);
    let acl = count(
        &catalog,
        "SELECT count(*) FROM acl_bucket_entries WHERE bucket = 'policy'",
    )
    .await;
    assert!(acl > 0, "the bucket must have stored ACL rows to restore");
    delete(&catalog, "policy").await.unwrap();
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM acl_bucket_entries WHERE bucket = 'policy'"
        )
        .await,
        0
    );
    let restored = catalog
        .restore_bucket("policy", live.generation, &access)
        .await
        .unwrap();
    assert_eq!(restored, live);
    let policy = catalog.get_policy("policy", &access).await.unwrap();
    assert_eq!(policy.bindings, stored.bindings);
    assert_ne!(policy.etag, stored.etag);
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM acl_bucket_entries WHERE bucket = 'policy'"
        )
        .await,
        acl
    );
    catalog.close().await.unwrap();
}

/// Startup reconciliation only visits live buckets, so a version deleted under one set of
/// trusted principals must be re-bound to the current set when it comes back.
#[tokio::test]
async fn restore_rebinds_policy_acl_entities_to_the_current_principals() {
    use crate::{
        auth::{Auth, Binding, Policy, Registry},
        clock::SystemClock,
        storage::admin::LocalAdmin,
    };
    let strict = |entity: &str| {
        Auth::strict(
            Registry::parse(
                serde_json::json!({"identities":[{"id":"local:reader","tokens":[],"acl_entity":entity}]})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap(),
        )
    };
    let bucket_entities = |catalog: &Catalog| {
        let catalog = catalog.clone();
        async move {
            catalog
                .run_admin(|connection| {
                    connection
                        .query_row(
                            "SELECT entries FROM acl_bucket_entries WHERE bucket = 'rebound' AND scope = 'bucket'",
                            [],
                            |row| row.get::<_, String>(0),
                        )
                        .map_err(unavailable)
                })
                .await
                .unwrap()
        }
    };
    let dir = TestDir::new();
    let catalog = Catalog::open_for_server(&dir.0, Arc::new(SystemClock), LocalAdmin::Disabled)
        .await
        .unwrap();
    catalog
        .configure_authentication(&strict("user-old@example.test"))
        .await
        .unwrap();
    catalog.insert_bucket(record("rebound")).await.unwrap();
    set_retention(&catalog, "rebound", 7 * DAY).await;
    catalog
        .set_policy(
            "rebound",
            Policy {
                bindings: vec![Binding {
                    role: "roles/storage.legacyBucketReader".into(),
                    members: vec!["user:local:reader".into()],
                }],
                etag: None,
            },
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert!(
        bucket_entities(&catalog)
            .await
            .contains("user-old@example.test")
    );
    let generation = catalog
        .get_bucket("rebound")
        .await
        .unwrap()
        .unwrap()
        .generation;
    delete(&catalog, "rebound").await.unwrap();
    catalog.close().await.unwrap();

    let reopened = Catalog::open_for_server(&dir.0, Arc::new(SystemClock), LocalAdmin::Disabled)
        .await
        .unwrap();
    reopened
        .configure_authentication(&strict("user-new@example.test"))
        .await
        .unwrap();
    reopened
        .restore_bucket("rebound", generation, &BucketAccess::Permissive)
        .await
        .unwrap();
    let entries = bucket_entities(&reopened).await;
    assert!(entries.contains("user-new@example.test"), "{entries}");
    assert!(!entries.contains("user-old@example.test"), "{entries}");
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn schema_45_backfills_generations_and_keeps_new_ones_unique() {
    let dir = TestDir::new();
    let (catalog, _clock) = open(&dir).await;
    let original = catalog.insert_bucket(record("legacy")).await.unwrap();
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    super::super::test_support::downgrade_bucket_generation_schema(&connection);
    connection
        .execute_batch(
            "UPDATE object_generation SET last_generation = 0; PRAGMA user_version = 44;",
        )
        .unwrap();
    drop(connection);
    let (catalog, _clock) = open(&dir).await;
    let migrated = catalog.get_bucket("legacy").await.unwrap().unwrap();
    assert_eq!(migrated, original);
    // Schema 45 backfills microseconds and schema 46 scales them to the nanosecond scale.
    assert_eq!(
        migrated.generation,
        unix_micros(UNIX_EPOCH + START).unwrap() * 1000
    );
    // A same-name bucket created at the same instant still gets a new generation.
    set_retention(&catalog, "legacy", 7 * DAY).await;
    delete(&catalog, "legacy").await.unwrap();
    let recreated = catalog.insert_bucket(record("legacy")).await.unwrap();
    assert!(recreated.generation > migrated.generation);
    catalog.close().await.unwrap();
}

/// Bucket generations moved from the shared microsecond allocator to their own
/// nanosecond-scale one (schema 46). Existing live and soft-deleted generations are scaled by
/// 1000 together with the retained-object keys derived from them, so order, uniqueness and
/// `restore?generation=` keep working.
#[tokio::test]
async fn schema_46_scales_bucket_generations_and_retained_keys_to_nanoseconds() {
    let dir = TestDir::new();
    let (catalog, _clock) = open(&dir).await;
    catalog.insert_bucket(record("kept")).await.unwrap();
    catalog.insert_bucket(record("gone")).await.unwrap();
    set_retention(&catalog, "gone", 7 * DAY).await;
    let hard_delete = UNIX_EPOCH + START + Duration::from_secs(DAY);
    retained_object(&catalog, "gone", hard_delete).await;
    delete(&catalog, "gone").await.unwrap();
    catalog.close().await.unwrap();

    // Rebuild the schema 45 state: microsecond-scale generations, no bucket allocator.
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    // Reopening verifies that every referenced blob exists, so give the retained row its file.
    let blob: String = connection
        .query_row("SELECT blob_id FROM objects", [], |row| row.get(0))
        .unwrap();
    let blob_path = crate::storage::blob_id::BlobId::parse(&blob)
        .unwrap()
        .blob_path(&dir.0);
    std::fs::create_dir_all(blob_path.parent().unwrap()).unwrap();
    std::fs::write(&blob_path, b"").unwrap();
    connection
        .execute_batch(
            "DROP TABLE bucket_generation;
             ALTER TABLE buckets DROP COLUMN versioning_configured;
             ALTER TABLE soft_deleted_buckets DROP COLUMN versioning_configured;
             ALTER TABLE objects DROP COLUMN temporary_hold_released;
             ALTER TABLE objects DROP COLUMN event_based_hold_released;
             ALTER TABLE objects DROP COLUMN retain_until_sub_micro_nanos;
             UPDATE buckets SET generation = 1000000100 WHERE name = 'kept';
             UPDATE soft_deleted_buckets SET generation = 1000000200 WHERE name = 'gone';
             UPDATE objects SET bucket = 'gone#1000000200' WHERE bucket LIKE 'gone#%';
             DELETE FROM acl_object_entries;
             INSERT INTO acl_object_entries (bucket, name, generation, entries)
                 SELECT bucket, name, generation, '[{\"entity\":\"allUsers\",\"role\":\"READER\"}]'
                 FROM objects WHERE bucket = 'gone#1000000200';
             PRAGMA user_version = 45;",
        )
        .unwrap();
    drop(connection);

    let (catalog, _clock) = open(&dir).await;
    let kept = catalog.get_bucket("kept").await.unwrap().unwrap();
    assert_eq!(kept.generation, 1_000_000_100_000);
    let page = catalog
        .list_soft_deleted_buckets("project", "", None, 10)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].record.generation, 1_000_000_200_000);
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM objects WHERE bucket = 'gone#1000000200000'"
        )
        .await,
        1
    );
    // The retained object's ACL rows move with its key, or the foreign key would reject them
    // (or leave them orphaned under the old key).
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM acl_object_entries WHERE bucket = 'gone#1000000200000'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM acl_object_entries WHERE bucket = 'gone#1000000200'"
        )
        .await,
        0
    );
    // A bucket created after the migration is above every migrated generation.
    let fresh = catalog.insert_bucket(record("fresh")).await.unwrap();
    assert!(fresh.generation > kept.generation && fresh.generation > 1_000_000_200_000);
    let restored = catalog
        .restore_bucket("gone", 1_000_000_200_000, &BucketAccess::Permissive)
        .await
        .unwrap();
    assert_eq!(restored.generation, 1_000_000_200_000);
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM objects WHERE bucket = 'gone'"
        )
        .await,
        1
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn new_bucket_generations_are_nanosecond_scale_unique_and_monotonic() {
    let dir = TestDir::new();
    let (catalog, clock) = open(&dir).await;
    let start_nanos = unix_micros(UNIX_EPOCH + START).unwrap() * 1000;
    // A frozen clock still yields strictly increasing values: max(now, previous + 1).
    let first = catalog.insert_bucket(record("first")).await.unwrap();
    let second = catalog.insert_bucket(record("second")).await.unwrap();
    assert_eq!(first.generation, start_nanos);
    assert_eq!(second.generation, start_nanos + 1);
    // Deleting and recreating a name never repeats the soft-deleted version's value.
    set_retention(&catalog, "first", 7 * DAY).await;
    delete(&catalog, "first").await.unwrap();
    let again = catalog.insert_bucket(record("first")).await.unwrap();
    assert_eq!(again.generation, start_nanos + 2);
    // A clock that jumps ahead takes over; one that steps back cannot lower the sequence.
    let later = UNIX_EPOCH + START + Duration::from_secs(60);
    clock.set(later);
    let mut ahead = record("ahead");
    ahead.time_created = later;
    ahead.updated = later;
    let ahead = catalog.insert_bucket(ahead).await.unwrap();
    assert_eq!(ahead.generation, unix_micros(later).unwrap() * 1000);
    let behind = catalog.insert_bucket(record("behind")).await.unwrap();
    assert_eq!(behind.generation, ahead.generation + 1);
    catalog.close().await.unwrap();
}
