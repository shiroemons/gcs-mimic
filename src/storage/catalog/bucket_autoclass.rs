//! Catalog persistence for bucket Autoclass configuration.

use super::unavailable;
use crate::bucket::{
    BucketMetadataUpdate, BucketRecord, CatalogError, StorageClass,
    autoclass::{self, AUTOCLASS_STORAGE_CLASS, Autoclass},
    lifecycle::{LifecycleRules, validate_location},
};
use crate::storage::catalog::sql;
use rusqlite::{Connection, OptionalExtension, Transaction};
use std::time::SystemTime;

pub(super) const SCHEMA: &str =
    "ALTER TABLE buckets ADD COLUMN autoclass TEXT; PRAGMA user_version = 31;";

fn store(
    transaction: &Transaction<'_>,
    bucket: &str,
    config: Option<&Autoclass>,
) -> Result<(), CatalogError> {
    sql::exec(
        transaction,
        "UPDATE buckets SET autoclass = ?2 WHERE name = ?1",
        rusqlite::params![bucket, config.map(sql::Json)],
    )?;
    Ok(())
}

/// Validate and persist the configuration of a bucket inserted in this transaction. The
/// catalog inserts the column as NULL so this module owns its encoding.
pub(super) fn insert(
    transaction: &Transaction<'_>,
    record: &mut BucketRecord,
    lifecycle: &LifecycleRules,
) -> Result<(), CatalogError> {
    // Creation-time Autoclass reports the bucket create time as its toggle time.
    record.autoclass = record
        .autoclass
        .map(|config| config.created_at(record.time_created));
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
    // Replacing the rules is how a bucket whose stored rules no longer decode is repaired, so
    // only the stored rules that stay need to be readable.
    let lifecycle = if let Some(rules) = &update.lifecycle {
        validate_location(rules, record.location.kind())?;
        rules
    } else {
        record.lifecycle.rules()?
    };
    if let Some(requested) = &update.autoclass {
        let was_enabled = record.autoclass.is_some_and(|config| config.is_enabled());
        let config = autoclass::apply(record.autoclass.as_ref(), requested, now)?;
        if !was_enabled && config.is_some_and(|config| config.is_enabled()) {
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
        lifecycle,
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
        |row| row.get::<_, Option<sql::Json<Autoclass>>>(0),
    )
    .optional()?
    .flatten()
    .map(|json| json.0);
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
        bucket::{BucketConditions, lifecycle::BucketLifecycle},
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
        // Requested rules are validated where they are parsed.
        let error = LifecycleRules::from_values(&invalid).unwrap_err();
        assert!(matches!(error, CatalogError::Metadata(_)));
        assert_eq!(JsonError::from(error).status, StatusCode::BAD_REQUEST);
        let error = crate::json::parse_bucket_patch(
            json!({"lifecycle":{"rule":invalid}, "defaultEventBasedHold":true})
                .to_string()
                .as_bytes(),
            crate::config::CompatProfile::Native,
        )
        .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        // A library caller passing back unreadable stored rules gets the same input error.
        let mut record = bucket("lifecycle-bucket", "project");
        record.lifecycle = BucketLifecycle::from_stored(invalid.clone());
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
        record.lifecycle = BucketLifecycle::default();
        let before = catalog.insert_bucket(record).await.unwrap();
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
        // The bucket stays readable, with its rules as stored.
        let mut expected = before;
        expected.lifecycle = BucketLifecycle::from_stored(invalid);
        assert!(matches!(
            expected.lifecycle,
            BucketLifecycle::Unreadable { .. }
        ));
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
