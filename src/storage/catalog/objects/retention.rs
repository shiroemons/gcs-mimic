//! Catalog persistence for bucket object-retention enablement and per-object retention.

use super::{object_unavailable, system_time_micros, unix_micros};
use crate::storage::catalog::resumable::InvalidOptions;
use crate::{
    bucket::CatalogError,
    object::{
        ObjectError, ObjectRecord,
        retention::{BucketRetention, ObjectRetention, RetentionMode},
    },
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, types::Type};

/// Schema 29. Both object columns are NULL when the object has no configuration.
pub(in crate::storage) const SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN object_retention_enabled INTEGER NOT NULL DEFAULT 0 CHECK(object_retention_enabled IN (0, 1));
ALTER TABLE objects ADD COLUMN retention_mode TEXT CHECK(retention_mode IN ('Unlocked', 'Locked'));
ALTER TABLE objects ADD COLUMN retain_until_time INTEGER CHECK((retain_until_time IS NULL) = (retention_mode IS NULL));
PRAGMA user_version = 29;";

pub(in crate::storage) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let unavailable = |error: rusqlite::Error| {
        CatalogError::Unavailable(crate::storage::failure::BackendCause::from_error(error))
    };
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

/// Reads the retention pair starting at `index` (mode, then retain-until micros).
pub(super) fn read(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<ObjectRetention>> {
    let mode: Option<String> = row.get(index)?;
    let until: Option<i64> = row.get(index + 1)?;
    let (Some(mode), Some(until)) = (mode, until) else {
        return Ok(None);
    };
    let mode = RetentionMode::parse(&mode).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Text,
            "invalid retention mode".into(),
        )
    })?;
    Ok(Some(ObjectRetention {
        mode,
        retain_until: system_time_micros(until)?,
    }))
}

/// Values for the (`retention_mode`, `retain_until_time`) columns.
pub(super) fn columns(
    retention: Option<&ObjectRetention>,
) -> Result<(Option<&'static str>, Option<i64>), ObjectError> {
    retention
        .map(|retention| {
            Ok((
                Some(retention.mode.as_str()),
                Some(unix_micros(retention.retain_until).map_err(object_unavailable)?),
            ))
        })
        .unwrap_or(Ok((None, None)))
}

/// Encodes upload metadata retention for a persisted resumable session.
pub(in crate::storage) fn session_value(
    retention: Option<&Option<ObjectRetention>>,
) -> serde_json::Value {
    let Some(retention) = retention else {
        return serde_json::Value::Null;
    };
    let value = retention.map(|retention| {
        // Parsed RFC 3339 times end at year 9999, far inside the i64 microsecond range.
        let micros = unix_micros(retention.retain_until).unwrap_or(i64::MAX);
        serde_json::json!({"mode": retention.mode.as_str(), "retainUntilMicros": micros})
    });
    serde_json::json!({ "value": value })
}

/// Decodes [`session_value`]; sessions written before retention support have no key.
pub(in crate::storage) fn from_session(
    value: &serde_json::Value,
) -> Result<Option<Option<ObjectRetention>>, InvalidOptions> {
    let invalid = || InvalidOptions::new("invalid resumable retention");
    if value.is_null() {
        return Ok(None);
    }
    let inner = value.get("value").ok_or_else(invalid)?;
    if inner.is_null() {
        return Ok(Some(None));
    }
    let mode = inner["mode"]
        .as_str()
        .and_then(RetentionMode::parse)
        .ok_or_else(invalid)?;
    let micros = inner["retainUntilMicros"].as_i64().ok_or_else(invalid)?;
    Ok(Some(Some(ObjectRetention {
        mode,
        retain_until: system_time_micros(micros).map_err(|_| invalid())?,
    })))
}

/// Reads the bucket flag inside the caller's mutation transaction.
pub(super) fn bucket_enabled(
    transaction: &Transaction<'_>,
    bucket: &str,
) -> Result<bool, ObjectError> {
    transaction
        .query_row(
            "SELECT object_retention_enabled FROM buckets WHERE name = ?1",
            [bucket],
            |row| row.get(0),
        )
        .optional()
        .map_err(object_unavailable)?
        .ok_or(ObjectError::BucketNotFound)
}

/// Validates a stored configuration on a new generation inside its publication transaction.
pub(super) fn validate_publication(
    transaction: &Transaction<'_>,
    record: &ObjectRecord,
    now: std::time::SystemTime,
) -> Result<(), ObjectError> {
    if record.retention.is_none() {
        return Ok(());
    }
    let bucket = if bucket_enabled(transaction, &record.bucket)? {
        BucketRetention::Enabled
    } else {
        BucketRetention::Disabled
    };
    crate::object::retention::validate_stored(record, bucket, now)
}
