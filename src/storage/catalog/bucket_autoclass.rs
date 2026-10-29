//! Catalog persistence for bucket Autoclass configuration.

use super::unavailable;
use crate::bucket::{
    BucketMetadataUpdate, BucketRecord, CatalogError,
    autoclass::{self, AUTOCLASS_STORAGE_CLASS, Autoclass},
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, types::Type};
use std::time::SystemTime;

pub(super) const SCHEMA: &str =
    "ALTER TABLE buckets ADD COLUMN autoclass TEXT; PRAGMA user_version = 31;";

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

/// Read by column name so the bucket column list can grow independently.
pub(super) fn read(row: &Row<'_>) -> rusqlite::Result<Option<Autoclass>> {
    let index = row.as_ref().column_index("autoclass")?;
    row.get::<_, Option<String>>(index)?
        .map(|value| {
            serde_json::from_str(&value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
            })
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
    transaction
        .execute(
            "UPDATE buckets SET autoclass = ?2 WHERE name = ?1",
            rusqlite::params![bucket, value],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Validate and persist the configuration of a bucket inserted in this transaction. The
/// catalog inserts the column as NULL so this module owns its encoding.
pub(super) fn insert(
    transaction: &Transaction<'_>,
    record: &mut BucketRecord,
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
    autoclass::validate_bucket(
        record.autoclass.as_ref(),
        &record.storage_class,
        &record.lifecycle,
    )?;
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
    if let Some(requested) = &update.autoclass {
        let (config, newly_enabled) = autoclass::apply(record.autoclass.as_ref(), requested, now)?;
        if newly_enabled {
            if update.storage_class.is_none() {
                record.storage_class = AUTOCLASS_STORAGE_CLASS.to_owned();
            }
            // Enabling moves every object except soft-deleted ones to STANDARD storage.
            transaction
                .execute(
                    "UPDATE objects SET storage_class = ?2 WHERE bucket = ?1 AND soft_delete_time IS NULL AND storage_class != ?2",
                    [record.name.as_str(), AUTOCLASS_STORAGE_CLASS],
                )
                .map_err(unavailable)?;
        }
        if config != record.autoclass {
            store(transaction, &record.name, config.as_ref())?;
        }
        record.autoclass = config;
    }
    autoclass::validate_bucket(
        record.autoclass.as_ref(),
        update
            .storage_class
            .as_deref()
            .unwrap_or(&record.storage_class),
        update.lifecycle.as_deref().unwrap_or(&record.lifecycle),
    )
}

/// The storage class a new object generation receives in `bucket`.
pub(crate) fn object_storage_class(
    connection: &Connection,
    bucket: &str,
    requested: Option<String>,
    bucket_default: String,
) -> Result<String, CatalogError> {
    let config = connection
        .query_row(
            "SELECT autoclass FROM buckets WHERE name = ?1",
            [bucket],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(unavailable)?
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
