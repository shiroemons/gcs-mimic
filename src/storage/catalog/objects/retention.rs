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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        retain_until: decode_time(until)?,
    }))
}

/// The `retain_until_time` column holds microseconds. A time with a sub-microsecond remainder
/// is stored as the negated nanosecond count instead: no retention time precedes the epoch, so
/// a negative value is unambiguous, rows written earlier keep reading as microseconds and no
/// schema migration is needed.
fn encode_time(time: SystemTime) -> Result<i64, ObjectError> {
    let nanos = time
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_nanos()).ok());
    match nanos {
        Some(nanos) if nanos % 1000 != 0 => Ok(-nanos),
        // Past year 2262 nanoseconds overflow an i64; the remainder is dropped there.
        _ => unix_micros(time).map_err(object_unavailable),
    }
}

fn decode_time(value: i64) -> rusqlite::Result<SystemTime> {
    if value < 0 {
        return Ok(UNIX_EPOCH + Duration::from_nanos(value.unsigned_abs()));
    }
    system_time_micros(value)
}

/// The nanoseconds past the last whole microsecond of a retention time.
fn sub_micro_nanos(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| u64::from(since.subsec_nanos() % 1000))
}

/// Values for the (`retention_mode`, `retain_until_time`) columns.
pub(super) fn columns(
    retention: Option<&ObjectRetention>,
) -> Result<(Option<&'static str>, Option<i64>), ObjectError> {
    retention
        .map(|retention| {
            Ok((
                Some(retention.mode.as_str()),
                Some(encode_time(retention.retain_until)?),
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
        let mut value =
            serde_json::json!({"mode": retention.mode.as_str(), "retainUntilMicros": micros});
        // Absent when zero, so sessions written before nanosecond comparison read unchanged.
        let remainder = sub_micro_nanos(retention.retain_until);
        if remainder != 0 {
            value["retainUntilSubMicroNanos"] = remainder.into();
        }
        value
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
    let remainder = match inner.get("retainUntilSubMicroNanos") {
        None => 0,
        Some(value) => value
            .as_u64()
            .filter(|nanos| *nanos < 1000)
            .ok_or_else(invalid)?,
    };
    let base = system_time_micros(micros).map_err(|_| invalid())?;
    Ok(Some(Some(ObjectRetention {
        mode,
        retain_until: base + Duration::from_nanos(remainder),
    })))
}

/// Reads the bucket flag inside the caller's mutation transaction.
pub(super) fn bucket_enabled(
    transaction: &Transaction<'_>,
    bucket: &str,
) -> Result<bool, ObjectError> {
    transaction
        .prepare_cached("SELECT object_retention_enabled FROM buckets WHERE name = ?1")
        .and_then(|mut statement| statement.query_row([bucket], |row| row.get(0)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::retention::ObjectRetention;

    fn at(secs: u64, nanos: u32) -> SystemTime {
        UNIX_EPOCH + Duration::new(secs, nanos)
    }

    #[test]
    fn column_keeps_nanoseconds_and_reads_microsecond_rows_unchanged() {
        for time in [at(4_000_000_000, 123_456_789), at(4_000_000_000, 1)] {
            let stored = encode_time(time).unwrap();
            assert!(
                stored < 0,
                "a sub-microsecond remainder is stored as nanoseconds"
            );
            assert_eq!(decode_time(stored).unwrap(), time);
        }
        // Whole microseconds keep the original encoding, so earlier rows and later readers agree.
        let whole = at(4_000_000_000, 123_456_000);
        let stored = encode_time(whole).unwrap();
        assert_eq!(stored, 4_000_000_000_123_456);
        assert_eq!(decode_time(stored).unwrap(), whole);
        assert_eq!(decode_time(0).unwrap(), UNIX_EPOCH);
    }

    #[test]
    fn session_value_keeps_the_remainder_and_reads_older_sessions() {
        let retention = ObjectRetention {
            mode: RetentionMode::Unlocked,
            retain_until: at(4_000_000_000, 123_456_789),
        };
        let value = session_value(Some(&Some(retention)));
        assert_eq!(value["value"]["retainUntilSubMicroNanos"], 789);
        assert_eq!(from_session(&value).unwrap(), Some(Some(retention)));
        // Without a remainder the key is absent, which is also how older sessions look.
        let whole = ObjectRetention {
            retain_until: at(4_000_000_000, 123_456_000),
            ..retention
        };
        let value = session_value(Some(&Some(whole)));
        assert!(value["value"].get("retainUntilSubMicroNanos").is_none());
        assert_eq!(from_session(&value).unwrap(), Some(Some(whole)));
        let mut invalid = session_value(Some(&Some(retention)));
        invalid["value"]["retainUntilSubMicroNanos"] = 1000.into();
        assert!(from_session(&invalid).is_err());
    }
}
