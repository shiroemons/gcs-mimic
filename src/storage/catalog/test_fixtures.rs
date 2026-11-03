use super::admin::{BucketDefinition, Origin, Publication, PublishMode};
use super::{Catalog, ObjectReader, ObjectUpload, RestoreAccess};
use crate::bucket::{BucketAccess, BucketRecord};
use crate::object::{ObjectTarget, UploadChunk};
use crate::test_support::TestDir;
use bytes::Bytes;
use rusqlite::Connection;
use std::time::UNIX_EPOCH;

pub(in crate::storage) fn guidance_source_failures() -> Vec<(
    crate::object::ObjectError,
    crate::storage::failure::BackendFailure,
)> {
    use super::{ConnectionUnavailable, admission_error, executor, unavailable};
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
        (
            ObjectError::from(admission_error(&tokio::sync::TryAcquireError::Closed)),
            BackendFailure::CatalogClosed,
        ),
        (
            ObjectError::from(admission_error(&tokio::sync::TryAcquireError::NoPermits)),
            BackendFailure::OperationLimit,
        ),
        (
            ObjectError::from(unavailable(BackendFailure::OutboxLimit)),
            BackendFailure::OutboxLimit,
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
        storage_class: "STANDARD".into(),
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
