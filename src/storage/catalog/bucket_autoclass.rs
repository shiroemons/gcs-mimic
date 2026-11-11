//! Catalog persistence for bucket Autoclass configuration.

use super::unavailable;
use crate::bucket::{
    BucketMetadataUpdate, BucketRecord, CatalogError, StorageClass,
    autoclass::{self, AUTOCLASS_STORAGE_CLASS, Autoclass},
    lifecycle::{LifecycleRule, validate_location},
};
use crate::storage::catalog::sql;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, types::Type};
use std::time::SystemTime;

pub(super) const SCHEMA: &str =
    "ALTER TABLE buckets ADD COLUMN autoclass TEXT; PRAGMA user_version = 31;";

/// Read by column name so the bucket column list can grow independently.
pub(super) fn read(row: &Row<'_>) -> rusqlite::Result<Option<Autoclass>> {
    let index = row.as_ref().column_index("autoclass")?;
    row.get::<_, Option<String>>(index)?
        .map(|value| {
            serde_json::from_str(&value)
                .map_err(|error| sql::conversion_error(index, Type::Text, error))
        })
        .transpose()
}

fn store(
    transaction: &Transaction<'_>,
    bucket: &str,
    config: Option<&Autoclass>,
) -> Result<(), CatalogError> {
    let value = config
        .map(serde_json::to_string)
        .transpose()
        .map_err(unavailable)?;
    sql::exec(
        transaction,
        "UPDATE buckets SET autoclass = ?2 WHERE name = ?1",
        rusqlite::params![bucket, value],
    )?;
    Ok(())
}

/// Validate and persist the configuration of a bucket inserted in this transaction. The
/// catalog inserts the column as NULL so this module owns its encoding.
pub(super) fn insert(
    transaction: &Transaction<'_>,
    record: &mut BucketRecord,
    lifecycle: &[LifecycleRule],
) -> Result<(), CatalogError> {
    if let Some(config) = &mut record.autoclass {
        // Creation-time Autoclass reports the bucket create time as its toggle time.
        config.toggle_time = record.time_created;
        if config.terminal_storage_class_update_time.is_some() {
            config.terminal_storage_class_update_time = Some(record.time_created);
        }
        if let Some(class) = &config.terminal_storage_class {
            autoclass::validate_terminal_storage_class(class)?;
        }
    }
    autoclass::validate_bucket(record.autoclass.as_ref(), record.storage_class, lifecycle)?;
    store(transaction, &record.name, record.autoclass.as_ref())
}

/// Apply the requested Autoclass change and enforce the final-state restrictions before the
/// caller applies the remaining metadata fields in the same transaction.
pub(super) fn update(
    transaction: &Transaction<'_>,
    record: &mut BucketRecord,
    update: &BucketMetadataUpdate,
    now: SystemTime,
) -> Result<(), CatalogError> {
    let lifecycle = if let Some(rules) = &update.lifecycle {
        let rules = LifecycleRule::from_input(rules)?;
        validate_location(&rules, &record.location_type)?;
        rules
    } else {
        LifecycleRule::from_stored(&record.lifecycle)?
    };
    if let Some(requested) = &update.autoclass {
        let (config, newly_enabled) = autoclass::apply(record.autoclass.as_ref(), requested, now)?;
        if newly_enabled {
            if update.storage_class.is_none() {
                record.storage_class = AUTOCLASS_STORAGE_CLASS;
            }
            // Enabling moves every object except soft-deleted ones to STANDARD storage.
            sql::exec(
                transaction,
                "UPDATE objects SET storage_class = ?2, time_storage_class_updated = max(?3, updated, time_created) WHERE bucket = ?1 AND soft_delete_time IS NULL AND storage_class != ?2",
                rusqlite::params![
                    record.name,
                    AUTOCLASS_STORAGE_CLASS,
                    super::unix_micros(now).map_err(unavailable)?
                ],
            )?;
        }
        if config != record.autoclass {
            store(transaction, &record.name, config.as_ref())?;
        }
        record.autoclass = config;
    }
    autoclass::validate_bucket(
        record.autoclass.as_ref(),
        update.storage_class.unwrap_or(record.storage_class),
        &lifecycle,
    )
}

/// The storage class a new object generation receives in `bucket`.
pub(crate) fn object_storage_class(
    connection: &Connection,
    bucket: &str,
    requested: Option<StorageClass>,
    bucket_default: StorageClass,
) -> Result<StorageClass, CatalogError> {
    let config = sql::row(
        connection,
        "SELECT autoclass FROM buckets WHERE name = ?1",
        [bucket],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()?
    .flatten()
    .map(|value| serde_json::from_str::<Autoclass>(&value))
    .transpose()
    .map_err(|error| CatalogError::Corrupt(format!("bucket autoclass: {error}").into()))?;
    Ok(autoclass::object_storage_class(
        config.as_ref(),
        requested,
        bucket_default,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bucket::BucketConditions,
        json::JsonError,
        storage::{
            Catalog,
            test_fixtures::{PERMISSIVE, bucket},
        },
        test_support::TestDir,
    };
    use hyper::StatusCode;
    use serde_json::json;

    #[tokio::test]
    async fn lifecycle_catalog_writes_distinguish_invalid_input_from_corrupt_stored_rules() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let invalid = vec![json!({"action":{"type":"Delete"},"condition":{"age":-1}})];
        let mut record = bucket("lifecycle-bucket", "project");
        record.lifecycle = invalid.clone();
        let error = catalog.insert_bucket(record.clone()).await.unwrap_err();
        assert!(matches!(error, CatalogError::Metadata(_)));
        assert_eq!(JsonError::from(error).status, StatusCode::BAD_REQUEST);
        assert!(
            catalog
                .get_bucket(&record.name, PERMISSIVE)
                .await
                .unwrap()
                .is_none()
        );
        record.lifecycle.clear();
        let before = catalog.insert_bucket(record).await.unwrap();
        let error = catalog
            .patch_bucket(
                &before.name,
                BucketMetadataUpdate {
                    lifecycle: Some(invalid.clone()),
                    default_event_based_hold: Some(true),
                    ..Default::default()
                },
                BucketConditions::default(),
                PERMISSIVE,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, CatalogError::Metadata(_)));
        assert_eq!(JsonError::from(error).status, StatusCode::BAD_REQUEST);
        assert_eq!(
            catalog.get_bucket(&before.name, PERMISSIVE).await.unwrap(),
            Some(before.clone())
        );
        let encoded = serde_json::to_string(&invalid).unwrap();
        catalog
            .run::<_, CatalogError>(move |connection| {
                sql::exec(
                    connection,
                    "UPDATE buckets SET lifecycle = ?1 WHERE name = 'lifecycle-bucket'",
                    [encoded],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let error = catalog
            .patch_bucket(
                &before.name,
                BucketMetadataUpdate {
                    default_event_based_hold: Some(true),
                    ..Default::default()
                },
                BucketConditions::default(),
                PERMISSIVE,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, CatalogError::Corrupt(_)));
        assert_eq!(
            JsonError::from(error).status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let mut expected = before;
        expected.lifecycle = invalid;
        assert_eq!(
            catalog
                .get_bucket(&expected.name, PERMISSIVE)
                .await
                .unwrap(),
            Some(expected)
        );
        catalog.close().await.unwrap();
    }
}
