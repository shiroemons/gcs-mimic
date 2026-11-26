use super::admin::{BucketDefinition, Origin, Publication, PublishMode};
use super::{Catalog, ObjectReader, ObjectUpload, RestoreAccess};
use crate::bucket::{BucketAccess, BucketRecord};
use crate::object::{ObjectMetadataUpdate, ObjectRecord, ObjectTarget, Patch, UploadChunk};
use crate::test_support::TestDir;
use bytes::Bytes;
use rusqlite::Connection;
use std::time::UNIX_EPOCH;

pub(crate) fn pause_read(
    catalog: &Catalog,
    step: super::fault::Step,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    use std::sync::{Arc, Mutex, mpsc};
    let (entered, reached) = tokio::sync::oneshot::channel();
    let (release, blocked) = mpsc::channel();
    catalog.inject(
        step,
        0,
        super::fault::Fault::Block {
            entered: Arc::new(Mutex::new(Some(entered))),
            release: Arc::new(Mutex::new(blocked)),
        },
    );
    (reached, release)
}

pub(in crate::storage) fn guidance_source_failures() -> Vec<(
    crate::object::ObjectError,
    crate::storage::failure::BackendFailure,
)> {
    use super::{ConnectionUnavailable, executor, unavailable};
    use crate::{
        object::ObjectError,
        storage::failure::{BackendFailure, object_unavailable},
    };

    vec![
        (
            ObjectError::from(unavailable(executor::ExecutorError::Closed)),
            BackendFailure::CatalogClosed,
        ),
        (
            ObjectError::from(unavailable(ConnectionUnavailable::Closed)),
            BackendFailure::CatalogClosed,
        ),
        (
            object_unavailable(executor::ExecutorError::Closed),
            BackendFailure::CatalogClosed,
        ),
        (
            object_unavailable(ConnectionUnavailable::Closed),
            BackendFailure::CatalogClosed,
        ),
    ]
}

/// Spelled out at every call site so a test that skips authorization says so.
pub(crate) const PERMISSIVE: &BucketAccess = &BucketAccess::Permissive;

/// Names both permissive authorities so tests explicitly skip restore authorization.
pub(crate) const PERMISSIVE_RESTORE: RestoreAccess<'static> = RestoreAccess {
    restore: PERMISSIVE,
    create: PERMISSIVE,
};

/// The live version of bucket/name.
pub(crate) fn live(bucket: &str, name: &str) -> ObjectTarget {
    ObjectTarget::new(bucket, name, None)
}

/// A specific generation of bucket/name.
pub(crate) fn version(bucket: &str, name: &str, generation: i64) -> ObjectTarget {
    ObjectTarget::new(bucket, name, Some(generation))
}

/// A text/plain upload with no declared hashes or preconditions and permissive access.
pub(crate) fn permissive_upload<'a>(
    bucket: &'a str,
    name: &'a str,
    max_bytes: u64,
) -> ObjectUpload<'a> {
    ObjectUpload {
        bucket,
        name,
        content_type: "text/plain".into(),
        expected: Default::default(),
        conditions: Default::default(),
        max_bytes,
        metadata: Default::default(),
        access: BucketAccess::Permissive,
    }
}

pub(crate) fn bucket(name: &str, project: &str) -> BucketRecord {
    BucketRecord {
        name: name.into(),
        project: project.into(),
        location: "US".parse().unwrap(),
        storage_class: crate::bucket::StorageClass::Standard,
        autoclass: None,
        labels: Default::default(),
        versioning: crate::bucket::Versioning::Unset,
        default_event_based_hold: false,
        public_access_prevention: crate::bucket::PublicAccessPrevention::Inherited,
        uniform_bucket_level_access_locked_time: None,
        lifecycle: Default::default(),
        retention_policy: None,
        soft_delete_policy: None,
        object_retention_enabled: false,
        cors: Vec::new(),
        metageneration: 1,
        generation: 0,
        time_created: UNIX_EPOCH,
        updated: UNIX_EPOCH,
    }
}

/// Adds `count` CANCELLED sessions cloned from the newest session, each newer than the last, so
/// retention tests reach the window without paying for that many real uploads. The `n`th row's
/// ID is `n` as 64 hexadecimal digits. Needs at least one session to clone.
pub(crate) async fn fill_cancelled_sessions(catalog: &Catalog, count: i64) {
    catalog
        .run::<_, crate::bucket::CatalogError>(move |connection| {
            super::sql::exec(
                connection,
                "WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM ids WHERE n < ?1) INSERT INTO resumable_sessions SELECT printf('%064x', n), printf('%032x', n), bucket, name, incarnation, options, total, 0, created + n, deadline + n, 'CANCELLED', NULL, NULL FROM ids CROSS JOIN (SELECT * FROM resumable_sessions ORDER BY deadline DESC, id LIMIT 1)",
                [count],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

pub(crate) async fn seed_bucket(catalog: &Catalog) {
    catalog
        .admin(Origin::Seed)
        .ensure_bucket(BucketDefinition::new(bucket("bucket", "project")).unwrap())
        .await
        .unwrap();
}

pub(crate) fn publication(max_bytes: u64) -> Publication {
    Publication {
        content_type: "text/plain".into(),
        metadata: Default::default(),
        expected: Default::default(),
        condition: None,
        max_bytes,
        mode: PublishMode::Replace,
    }
}

pub(crate) fn body(
    parts: Vec<Bytes>,
) -> tokio::sync::mpsc::Receiver<Result<UploadChunk, std::io::Error>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    tokio::spawn(async move {
        for part in parts {
            if sender.send(Ok(UploadChunk::Data(part))).await.is_err() {
                return;
            }
        }
        let _ = sender.send(Ok(UploadChunk::End)).await;
    });
    receiver
}

pub(crate) async fn read_all(reader: &mut ObjectReader) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

pub(crate) async fn downgraded<T>(
    dir: &TestDir,
    catalog: &Catalog,
    downgrade: impl FnOnce(&Connection) -> T,
) -> T {
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    downgrade(&connection)
}

pub(crate) async fn migrated(
    dir: &TestDir,
    record: BucketRecord,
    downgrade: impl FnOnce(&Connection),
) -> (Catalog, BucketRecord) {
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let original = catalog.insert_bucket(record).await.unwrap();
    downgraded(dir, &catalog, downgrade).await;
    (Catalog::open(&dir.0).await.unwrap(), original)
}

/// Caps the catalog at the pages it uses plus `spare`, so a write that needs more fails as on a
/// full disk. SQLite reuses free pages before it grows the file, and a new catalog has some from
/// the schema steps that drop tables, so the catalog gives them up first.
pub(crate) fn limit_pages(connection: &Connection, spare: i64) -> rusqlite::Result<()> {
    connection.execute_batch("VACUUM")?;
    let pages: i64 = connection.pragma_query_value(None, "page_count", |row| row.get(0))?;
    connection.pragma_update(None, "max_page_count", pages + spare)
}

#[track_caller]
pub(crate) fn assert_current_schema(connection: &Connection) {
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        super::CATALOG_SCHEMA_VERSION
    );
}

/// Protocol outputs must read a real transition whose class time differs from both other times.
pub(crate) async fn lifecycle_transitioned_object() -> (ObjectRecord, ObjectRecord) {
    use crate::clock::{Clock, ManualClock};
    use std::{sync::Arc, time::Duration};

    let clock = Arc::new(ManualClock::new(
        UNIX_EPOCH + Duration::from_micros(1_234_567),
    ));
    let catalog = Catalog::open_memory_for_server(
        64 * 1024 * 1024,
        16 * 1024 * 1024,
        clock.clone(),
        super::admin::LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    catalog.configure_lifecycle(&Default::default());
    let mut record = bucket("bucket", "project");
    record.lifecycle = crate::bucket::lifecycle::LifecycleRules::for_test(vec![
        serde_json::json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":1}}),
    ]).into();
    catalog.insert_bucket(record).await.unwrap();
    let original = catalog
        .put_object(
            permissive_upload("bucket", "object", 100),
            body(vec![Bytes::from_static(b"data")]),
        )
        .await
        .unwrap();
    clock.advance(Duration::from_secs(86_400));
    let super::lifecycle::LifecycleOutcome::Executed(outcome) =
        catalog.run_lifecycle_now().await.unwrap()
    else {
        panic!("execution disabled")
    };
    assert_eq!(
        (
            outcome.scanned,
            outcome.transitioned,
            outcome.deleted,
            outcome.archived,
            outcome.blocked,
            outcome.skipped,
            outcome.next
        ),
        (1, 1, 0, 0, 0, 0, Some(clock.now() + Duration::from_secs(1)))
    );
    let transitioned = catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(transitioned.storage_class_updated(), clock.now());
    clock.advance(Duration::from_secs(1));
    let changed = catalog
        .update_object_metadata(
            live("bucket", "object"),
            Default::default(),
            ObjectMetadataUpdate {
                content_type: Patch::Set("text/html".into()),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(
        changed.storage_class_updated(),
        transitioned.storage_class_updated()
    );
    let stored = catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(stored, changed);
    catalog.close().await.unwrap();
    (original, stored)
}

/// An object with no optional field set, at fixed instants. Persisted-format tests pin the
/// bytes it serializes to, so change none of its values.
pub(crate) fn bare_object_record() -> ObjectRecord {
    let at = UNIX_EPOCH + std::time::Duration::from_micros(1_700_000_000_123_456);
    ObjectRecord {
        bucket: "bucket".into(),
        name: "bare.txt".into(),
        generation: 1_700_000_000_123_456,
        metageneration: 1,
        owner: None,
        content_type: "application/octet-stream".into(),
        content_encoding: None,
        content_disposition: None,
        content_language: None,
        cache_control: None,
        metadata: Default::default(),
        size: 3,
        md5: [1; 16],
        md5_availability: Default::default(),
        crc32c: 7,
        component_count: None,
        storage_class: crate::bucket::StorageClass::Standard,
        time_created: at,
        time_storage_class_updated: None,
        updated: at,
        time_deleted: None,
        soft_delete_time: None,
        hard_delete_time: None,
        event_based_hold: Default::default(),
        temporary_hold: Default::default(),
        event_based_hold_release_time: None,
        custom_time: None,
        retention_expiration_time: None,
        retention: None,
        customer_key_sha256: None,
    }
}

/// An object with every optional field set, at fixed instants. Persisted-format tests pin the
/// bytes it serializes to, so change none of its values.
pub(crate) fn populated_object_record() -> ObjectRecord {
    use crate::object::{Hold, Md5Availability, owner::ObjectOwner};
    use std::time::Duration;
    let at = |micros| UNIX_EPOCH + Duration::from_micros(micros);
    ObjectRecord {
        bucket: "bucket".into(),
        name: "folder/full object.txt".into(),
        generation: 1_700_000_000_123_456,
        metageneration: 3,
        owner: Some(ObjectOwner::Principal {
            id: "uploader".into(),
            entity: Some("user-uploader@example.test".parse().unwrap()),
        }),
        content_type: "text/plain".into(),
        content_encoding: Some("gzip".into()),
        content_disposition: Some("inline".into()),
        content_language: Some("en".into()),
        cache_control: Some("private".into()),
        metadata: [
            ("alpha".to_owned(), "one".to_owned()),
            ("beta".into(), "two".into()),
        ]
        .into(),
        size: 11,
        md5: [3; 16],
        md5_availability: Md5Availability::Unavailable,
        crc32c: 123,
        component_count: Some(2),
        storage_class: crate::bucket::StorageClass::Nearline,
        time_created: at(1_700_000_000_123_456),
        time_storage_class_updated: Some(at(1_700_000_100_000_001)),
        updated: at(1_700_000_200_000_002),
        time_deleted: Some(at(1_700_000_300_000_003)),
        soft_delete_time: Some(at(1_700_000_300_000_003)),
        hard_delete_time: Some(at(1_700_900_300_000_003)),
        event_based_hold: Hold::Held,
        temporary_hold: Hold::Released,
        event_based_hold_release_time: Some(at(1_700_000_250_000_004)),
        custom_time: Some("2026-01-02T03:04:05.123456789Z".into()),
        retention_expiration_time: Some(at(1_800_000_000_000_005)),
        retention: Some(crate::object::retention::ObjectRetention {
            mode: crate::object::retention::RetentionMode::Locked,
            retain_until: crate::object::unix_nanos::UnixNanos::new(1_900_000_000, 123_456_789)
                .unwrap(),
        }),
        customer_key_sha256: Some([2; 32]),
    }
}
