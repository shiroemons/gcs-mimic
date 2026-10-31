//! Catalog persistence for bucket Pub/Sub notification configurations.
//!
//! Rows reference their bucket with `ON DELETE CASCADE`, so deleting a bucket removes its
//! configurations and ID allocator in the same transaction and a recreated bucket starts empty.

use super::{BucketAccess, Catalog, CatalogError, check_access, unavailable};
use crate::notification::{NotificationConfig, NotificationRecord, check_limits};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// A configuration over a bucket or event-type limit is invalid metadata whose message names
/// the limit; the validation error has no cause to keep.
impl From<crate::notification::NotificationError> for CatalogError {
    fn from(error: crate::notification::NotificationError) -> Self {
        Self::InvalidMetadata(error.to_string())
    }
}

pub(super) const SCHEMA: &str = "CREATE TABLE bucket_notification_ids (
    bucket TEXT PRIMARY KEY COLLATE BINARY REFERENCES buckets(name) ON DELETE CASCADE,
    last_id INTEGER NOT NULL CHECK(last_id > 0)
) WITHOUT ROWID;
CREATE TABLE bucket_notifications (
    bucket TEXT NOT NULL COLLATE BINARY REFERENCES buckets(name) ON DELETE CASCADE,
    id INTEGER NOT NULL CHECK(id > 0),
    config TEXT NOT NULL CHECK(length(CAST(config AS BLOB)) <= 65536),
    PRIMARY KEY (bucket, id)
) WITHOUT ROWID;
PRAGMA user_version = 28;";

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

#[expect(
    clippy::disallowed_methods,
    reason = "validates catalog state once at open"
)]
pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    // IDs are never reused within a bucket incarnation, so every row must be covered by its allocator.
    let valid: bool = connection
        .query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM bucket_notifications n LEFT JOIN bucket_notification_ids i ON i.bucket = n.bucket WHERE i.last_id IS NULL OR n.id > i.last_id)",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !valid {
        return Err(unavailable("invalid bucket notification catalog state"));
    }
    Ok(())
}

fn authorize(
    connection: &Connection,
    bucket: &str,
    access: &BucketAccess,
) -> Result<(), CatalogError> {
    let project: String = connection
        .prepare_cached("SELECT project FROM buckets WHERE name = ?1")
        .and_then(|mut statement| statement.query_row([bucket], |row| row.get(0)))
        .optional()
        .map_err(unavailable)?
        .ok_or(CatalogError::NotFound)?;
    check_access(connection, bucket, &project, access)?;
    Ok(())
}

fn record(id: i64, stored: &str) -> Result<NotificationRecord, CatalogError> {
    let config = NotificationConfig::from_stored(stored)
        .map_err(|reason| CatalogError::Corrupt(reason.into()))?;
    Ok(NotificationRecord { id, config })
}

pub(super) fn read_all(
    connection: &Connection,
    bucket: &str,
) -> Result<Vec<NotificationRecord>, CatalogError> {
    // The documented 100-configuration limit bounds this read.
    let mut statement = connection
        .prepare_cached(
            "SELECT id, config FROM bucket_notifications WHERE bucket = ?1 ORDER BY id LIMIT 101",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map([bucket], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unavailable)?;
    let mut records = Vec::new();
    for row in rows {
        let (id, stored) = row.map_err(unavailable)?;
        records.push(record(id, &stored)?);
    }
    Ok(records)
}

/// The Pub/Sub notification documentation states that creating and deleting configurations
/// increments the bucket metageneration, so both mutations update bucket metadata atomically.
fn touch_bucket(
    transaction: &Connection,
    bucket: &str,
    access: &BucketAccess,
    now_micros: i64,
) -> Result<(), CatalogError> {
    let updated = transaction
        .prepare_cached(
            "UPDATE buckets SET metageneration = metageneration + 1, updated = ?2, updated_nanos = 0
             WHERE name = ?1 AND metageneration < 9223372036854775807",
        )
        .and_then(|mut statement| statement.execute(params![bucket, now_micros]))
        .map_err(unavailable)?;
    if updated != 1 {
        return Err(unavailable("bucket metageneration exhausted"));
    }
    super::change_journal::append(
        transaction,
        super::change_journal::origin(access),
        super::change_journal::ChangeKind::BucketUpdate,
        bucket,
        None,
    )
}

/// Only canonical positive decimal IDs can name a stored configuration.
fn parse_id(id: &str) -> Option<i64> {
    if id.starts_with('0') || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    id.parse().ok().filter(|id| *id > 0)
}

impl Catalog {
    pub(crate) async fn insert_notification(
        &self,
        bucket: &str,
        config: NotificationConfig,
        access: &BucketAccess,
    ) -> Result<NotificationRecord, CatalogError> {
        let bucket = bucket.to_owned();
        let access = access.clone();
        let clock = self.0.clock.clone();
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            authorize(&transaction, &bucket, &access)?;
            let existing: Vec<_> = read_all(&transaction, &bucket)?
                .into_iter()
                .map(|record| record.config)
                .collect();
            check_limits(&existing, &config).map_err(CatalogError::from)?;
            let id: i64 = transaction
                .prepare_cached(
                    "INSERT INTO bucket_notification_ids VALUES (?1, 1)
                     ON CONFLICT(bucket) DO UPDATE SET last_id = last_id + 1
                     WHERE last_id < 9223372036854775807 RETURNING last_id",
                )
                .and_then(|mut statement| statement.query_row([&bucket], |row| row.get(0)))
                .optional()
                .map_err(unavailable)?
                .ok_or_else(|| unavailable("notification ID exhausted"))?;
            transaction
                .prepare_cached("INSERT INTO bucket_notifications VALUES (?1, ?2, ?3)")
                .and_then(|mut statement| {
                    statement.execute(params![bucket, id, config.to_stored()])
                })
                .map_err(unavailable)?;
            touch_bucket(
                &transaction,
                &bucket,
                &access,
                super::unix_micros(clock.now())?,
            )?;
            transaction.commit().map_err(unavailable)?;
            Ok(NotificationRecord { id, config })
        })
        .await
    }

    pub(crate) async fn get_notification(
        &self,
        bucket: &str,
        id: &str,
        access: &BucketAccess,
    ) -> Result<Option<NotificationRecord>, CatalogError> {
        let bucket = bucket.to_owned();
        let id = parse_id(id);
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            authorize(&transaction, &bucket, &access)?;
            let Some(id) = id else { return Ok(None) };
            let stored: Option<String> = transaction
                .prepare_cached(
                    "SELECT config FROM bucket_notifications WHERE bucket = ?1 AND id = ?2",
                )
                .and_then(|mut statement| {
                    statement.query_row(params![bucket, id], |row| row.get(0))
                })
                .optional()
                .map_err(unavailable)?;
            stored.map(|stored| record(id, &stored)).transpose()
        })
        .await
    }

    pub(crate) async fn list_notifications(
        &self,
        bucket: &str,
        access: &BucketAccess,
    ) -> Result<Vec<NotificationRecord>, CatalogError> {
        let bucket = bucket.to_owned();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            authorize(&transaction, &bucket, &access)?;
            read_all(&transaction, &bucket)
        })
        .await
    }

    /// Returns false when the bucket exists but has no configuration with this ID.
    pub(crate) async fn delete_notification(
        &self,
        bucket: &str,
        id: &str,
        access: &BucketAccess,
    ) -> Result<bool, CatalogError> {
        let bucket = bucket.to_owned();
        let id = parse_id(id);
        let access = access.clone();
        let clock = self.0.clock.clone();
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            authorize(&transaction, &bucket, &access)?;
            let Some(id) = id else { return Ok(false) };
            let deleted = transaction
                .prepare_cached("DELETE FROM bucket_notifications WHERE bucket = ?1 AND id = ?2")
                .and_then(|mut statement| statement.execute(params![bucket, id]))
                .map_err(unavailable)?;
            if deleted == 0 {
                return Ok(false);
            }
            touch_bucket(
                &transaction,
                &bucket,
                &access,
                super::unix_micros(clock.now())?,
            )?;
            transaction.commit().map_err(unavailable)?;
            Ok(true)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bucket::BucketRecord;
    use std::{collections::BTreeMap, time::UNIX_EPOCH};

    fn bucket(name: &str) -> BucketRecord {
        BucketRecord {
            name: name.into(),
            project: "project".into(),
            location: "US".into(),
            location_type: "multi-region".into(),
            storage_class: "STANDARD".into(),
            autoclass: None,
            labels: BTreeMap::new(),
            versioning_enabled: false,
            default_event_based_hold: false,
            public_access_prevention_enforced: false,
            uniform_bucket_level_access_locked_time: None,
            lifecycle: Vec::new(),
            retention_policy: None,
            soft_delete_policy: None,
            object_retention_enabled: false,
            cors: Vec::new(),
            metageneration: 1,
            time_created: UNIX_EPOCH,
            updated: UNIX_EPOCH,
        }
    }

    fn config(prefix: &str) -> NotificationConfig {
        NotificationConfig {
            topic: "//pubsub.googleapis.com/projects/p/topics/topic".into(),
            payload_format: "NONE".into(),
            event_types: vec!["OBJECT_FINALIZE".into()],
            custom_attributes: BTreeMap::from([("k".into(), "v".into())]),
            object_name_prefix: prefix.into(),
        }
    }

    #[tokio::test]
    async fn notifications_persist_allocate_unique_ids_and_cascade_with_bucket() {
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let access = BucketAccess::Permissive;
        catalog.insert_bucket(bucket("bucket")).await.unwrap();
        catalog.insert_bucket(bucket("other")).await.unwrap();
        let first = catalog
            .insert_notification("bucket", config("a/"), &access)
            .await
            .unwrap();
        let second = catalog
            .insert_notification("bucket", config("b/"), &access)
            .await
            .unwrap();
        let other = catalog
            .insert_notification("other", config("c/"), &access)
            .await
            .unwrap();
        assert_eq!((first.id, second.id, other.id), (1, 2, 1));
        assert!(
            catalog
                .delete_notification("bucket", "2", &access)
                .await
                .unwrap()
        );
        assert!(
            !catalog
                .delete_notification("bucket", "2", &access)
                .await
                .unwrap()
        );
        // A deleted highest ID is not reused within the same bucket incarnation.
        let third = catalog
            .insert_notification("bucket", config("d/"), &access)
            .await
            .unwrap();
        assert_eq!(third.id, 3);
        catalog.close().await.unwrap();

        let catalog = Catalog::open(&dir.0).await.unwrap();
        let listed = catalog.list_notifications("bucket", &access).await.unwrap();
        assert_eq!(listed, [first.clone(), third]);
        for id in ["01", "+1", "", "x", "0", "99999999999999999999"] {
            assert_eq!(
                catalog
                    .get_notification("bucket", id, &access)
                    .await
                    .unwrap(),
                None,
                "{id}"
            );
        }
        assert_eq!(
            catalog
                .get_notification("bucket", "1", &access)
                .await
                .unwrap(),
            Some(first)
        );
        assert!(matches!(
            catalog.list_notifications("missing", &access).await,
            Err(CatalogError::NotFound)
        ));
        assert!(matches!(
            catalog
                .insert_notification("missing", config(""), &access)
                .await,
            Err(CatalogError::NotFound)
        ));

        catalog
            .delete_bucket_authorized("bucket", None, None, &access)
            .await
            .unwrap();
        catalog.insert_bucket(bucket("bucket")).await.unwrap();
        assert!(
            catalog
                .list_notifications("bucket", &access)
                .await
                .unwrap()
                .is_empty()
        );
        let fresh = catalog
            .insert_notification("bucket", config(""), &access)
            .await
            .unwrap();
        assert_eq!(fresh.id, 1);
        assert_eq!(
            catalog.list_notifications("other", &access).await.unwrap(),
            [other]
        );
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn notification_limits_are_enforced_inside_the_transaction() {
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let access = BucketAccess::Permissive;
        catalog.insert_bucket(bucket("bucket")).await.unwrap();
        for _ in 0..10 {
            catalog
                .insert_notification("bucket", config(""), &access)
                .await
                .unwrap();
        }
        assert!(matches!(
            catalog
                .insert_notification("bucket", config(""), &access)
                .await,
            Err(CatalogError::InvalidMetadata(_))
        ));
        assert_eq!(
            catalog
                .list_notifications("bucket", &access)
                .await
                .unwrap()
                .len(),
            10
        );
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn corrupt_notification_rows_are_rejected() {
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog.insert_bucket(bucket("bucket")).await.unwrap();
        catalog
            .insert_notification("bucket", config(""), &BucketAccess::Permissive)
            .await
            .unwrap();
        catalog
            .run(|connection| {
                connection
                    .execute("UPDATE bucket_notifications SET config = '{}'", [])
                    .map_err(unavailable)?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            catalog
                .list_notifications("bucket", &BucketAccess::Permissive)
                .await,
            Err(CatalogError::Corrupt(_))
        ));
        catalog
            .run(|connection| {
                connection
                    .execute_batch(
                        "UPDATE bucket_notifications SET config = (SELECT config FROM bucket_notifications), id = 2",
                    )
                    .map_err(unavailable)?;
                Ok(())
            })
            .await
            .unwrap();
        catalog.close().await.unwrap();
        // A row beyond its allocator would let a later insert reuse a live ID.
        assert!(matches!(
            Catalog::open(&dir.0).await,
            Err(CatalogError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn notification_schema_migration_preserves_existing_buckets() {
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let original = catalog.insert_bucket(bucket("bucket")).await.unwrap();
        catalog.close().await.unwrap();
        let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        super::super::test_support::downgrade_object_retention_schema(&connection);
        connection
            .execute_batch(
                "DROP TABLE bucket_notifications; DROP TABLE bucket_notification_ids; PRAGMA user_version = 27;",
            )
            .unwrap();
        drop(connection);
        let catalog = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(catalog.get_bucket("bucket").await.unwrap(), Some(original));
        let access = BucketAccess::Permissive;
        assert!(
            catalog
                .list_notifications("bucket", &access)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            catalog
                .insert_notification("bucket", config(""), &access)
                .await
                .unwrap()
                .id,
            1
        );
        catalog.close().await.unwrap();
    }
}
