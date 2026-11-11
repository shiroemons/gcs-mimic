use super::admin::{BucketDefinition, Origin, Publication, PublishMode};
use super::{Catalog, ObjectReader, ObjectUpload, RestoreAccess};
use crate::bucket::{BucketAccess, BucketRecord};
use crate::object::{ObjectMetadataUpdate, ObjectRecord, ObjectTarget, UploadChunk};
use crate::test_support::TestDir;
use bytes::Bytes;
use rusqlite::Connection;
use std::time::UNIX_EPOCH;

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
        location: "US".into(),
        location_type: "multi-region".into(),
        storage_class: crate::bucket::StorageClass::Standard,
        autoclass: None,
        labels: Default::default(),
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
                "WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM ids WHERE n < ?1) INSERT INTO resumable_sessions SELECT printf('%064x', n), printf('%032x', n), bucket, name, incarnation, options, total, 0, created + n, deadline + n, 'CANCELLED', NULL FROM ids CROSS JOIN (SELECT * FROM resumable_sessions ORDER BY deadline DESC, id LIMIT 1)",
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
    record.lifecycle = vec![
        serde_json::json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":1}}),
    ];
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
                content_type: Some(Some("text/html".into())),
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
