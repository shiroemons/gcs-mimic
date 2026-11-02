//! Catalog persistence for bucket object-retention enablement and per-object retention.

use crate::storage::catalog::resumable::InvalidOptions;
use crate::storage::catalog::sql;
use crate::{
    bucket::CatalogError,
    object::{
        ObjectError, ObjectRecord,
        retention::{BucketRetention, ObjectRetention, RetentionMode},
        unix_nanos::UnixNanos,
    },
};
use rusqlite::{Connection, Row, Transaction, types::Type};

/// Schema 29. Both object columns are NULL when the object has no configuration.
pub(in crate::storage) const SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN object_retention_enabled INTEGER NOT NULL DEFAULT 0 CHECK(object_retention_enabled IN (0, 1));
ALTER TABLE objects ADD COLUMN retention_mode TEXT CHECK(retention_mode IN ('Unlocked', 'Locked'));
ALTER TABLE objects ADD COLUMN retain_until_time INTEGER CHECK((retain_until_time IS NULL) = (retention_mode IS NULL));
PRAGMA user_version = 29;";

/// Schema 47. The nanoseconds past the whole microsecond of `retain_until_time`; rows written
/// before it keep reading as whole microseconds.
pub(in crate::storage) const SUB_MICRO_SCHEMA: &str = "ALTER TABLE objects ADD COLUMN retain_until_sub_micro_nanos INTEGER NOT NULL DEFAULT 0 CHECK(retain_until_sub_micro_nanos BETWEEN 0 AND 999 AND (retain_until_sub_micro_nanos = 0 OR retain_until_time IS NOT NULL));
PRAGMA user_version = 47;";

pub(in crate::storage) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let unavailable = |error: rusqlite::Error| {
        CatalogError::Unavailable(crate::storage::failure::BackendCause::from_error(error))
    };
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

/// Reads the retention pair starting at `index` (mode, then retain-until micros) and the
/// sub-microsecond remainder at `sub_index`.
pub(super) fn read(
    row: &Row<'_>,
    index: usize,
    sub_index: usize,
) -> rusqlite::Result<Option<ObjectRetention>> {
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
    let invalid = || {
        rusqlite::Error::FromSqlConversionFailure(
            index + 1,
            Type::Integer,
            "invalid retain-until time".into(),
        )
    };
    let remainder = u32::try_from(row.get::<_, i64>(sub_index)?).map_err(|_| invalid())?;
    let retain_until =
        UnixNanos::from_micros_and_remainder(until, remainder).ok_or_else(invalid)?;
    Ok(Some(ObjectRetention { mode, retain_until }))
}

/// Values for the (`retention_mode`, `retain_until_time`, `retain_until_sub_micro_nanos`)
/// columns. The remainder (0 to 999) keeps the nanoseconds that the microsecond column drops.
pub(super) fn columns(
    retention: Option<&ObjectRetention>,
) -> Result<(Option<&'static str>, Option<i64>, i64), ObjectError> {
    let Some(retention) = retention else {
        return Ok((None, None, 0));
    };
    let (micros, remainder) = retention
        .retain_until
        .micros_and_remainder()
        .ok_or_else(|| {
            ObjectError::InvalidFixedMetadata(
                "retention.retainUntilTime is outside the supported range".into(),
            )
        })?;
    Ok((
        Some(retention.mode.as_str()),
        Some(micros),
        i64::from(remainder),
    ))
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
        let (micros, remainder) = retention
            .retain_until
            .micros_and_remainder()
            .unwrap_or((i64::MAX, 0));
        let mut value =
            serde_json::json!({"mode": retention.mode.as_str(), "retainUntilMicros": micros});
        // Absent when zero, so sessions written before nanosecond comparison read unchanged.
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
    let retain_until = UnixNanos::from_micros_and_remainder(
        micros,
        u32::try_from(remainder).map_err(|_| invalid())?,
    )
    .ok_or_else(invalid)?;
    Ok(Some(Some(ObjectRetention { mode, retain_until })))
}

/// Reads the bucket flag inside the caller's mutation transaction.
pub(super) fn bucket_enabled(
    transaction: &Transaction<'_>,
    bucket: &str,
) -> Result<bool, ObjectError> {
    sql::optional_scalar(
        transaction,
        "SELECT object_retention_enabled FROM buckets WHERE name = ?1",
        [bucket],
    )?
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

    fn at(secs: i64, nanos: u32) -> UnixNanos {
        UnixNanos::new(secs, nanos).unwrap()
    }

    /// Not a multiple of 100, so a platform that floors a `SystemTime` to 100 ns would lose it.
    const REMAINDER: u32 = 789;

    #[test]
    fn migration_keeps_malformed_schema_errors_unavailable() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE buckets (name TEXT); PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql='invalid schema' WHERE name='buckets'; PRAGMA writable_schema=OFF; PRAGMA schema_version=123;").unwrap();
        let error = migrate(&mut connection).unwrap_err();
        assert!(matches!(error, CatalogError::Unavailable(_)));
        assert!(
            matches!(std::error::Error::source(&error).and_then(|source| source.downcast_ref::<rusqlite::Error>()), Some(rusqlite::Error::SqliteFailure(failure, _)) if failure.code == rusqlite::ErrorCode::DatabaseCorrupt)
        );
    }

    #[test]
    fn columns_split_a_time_into_microseconds_and_a_remainder() {
        let time = at(4_000_000_000, 123_456_000 + REMAINDER);
        let retention = ObjectRetention {
            mode: RetentionMode::Unlocked,
            retain_until: time,
        };
        let (mode, micros, remainder) = columns(Some(&retention)).unwrap();
        assert_eq!(mode, Some("Unlocked"));
        assert_eq!(micros, Some(4_000_000_000_123_456));
        assert_eq!(remainder, i64::from(REMAINDER));
        let whole = ObjectRetention {
            retain_until: at(4_000_000_000, 123_456_000),
            ..retention
        };
        assert_eq!(columns(Some(&whole)).unwrap().2, 0);
        assert_eq!(columns(None).unwrap(), (None, None, 0));
    }

    /// A row written with a nanosecond fraction that is not a multiple of 100 reads back exactly,
    /// and a row from 0.7.0 (no remainder) reads as whole microseconds.
    #[test]
    fn read_restores_the_exact_nanosecond_from_the_columns() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE objects (mode TEXT, until INTEGER, sub INTEGER NOT NULL);
                 INSERT INTO objects VALUES ('Locked', 4000000000123456, 789), ('Unlocked', 4000000000123456, 0), (NULL, NULL, 0);",
            )
            .unwrap();
        let rows: Vec<Option<ObjectRetention>> = connection
            .prepare("SELECT mode, until, sub FROM objects ORDER BY rowid")
            .unwrap()
            .query_map([], |row| read(row, 0, 2))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows[0].unwrap().retain_until,
            at(4_000_000_000, 123_456_789)
        );
        assert_eq!(
            rows[1].unwrap().retain_until,
            at(4_000_000_000, 123_456_000)
        );
        assert_eq!(rows[2], None);
        let retention = rows[0].unwrap();
        assert_eq!(
            columns(Some(&retention)).unwrap(),
            (Some("Locked"), Some(4_000_000_000_123_456), 789)
        );
    }

    /// Schema 47 adds the remainder column to rows written by 0.7.0 and earlier: they read as
    /// whole microseconds, and the column refuses values that would corrupt the time.
    #[test]
    fn sub_micro_column_migrates_microsecond_rows_and_checks_its_range() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE objects (
                    retention_mode TEXT,
                    retain_until_time INTEGER CHECK((retain_until_time IS NULL) = (retention_mode IS NULL)));
                 INSERT INTO objects VALUES ('Unlocked', 4000000000123456), (NULL, NULL);",
            )
            .unwrap();
        connection.execute_batch(SUB_MICRO_SCHEMA).unwrap();
        let rows: Vec<(Option<i64>, i64)> = connection
            .prepare("SELECT retain_until_time, retain_until_sub_micro_nanos FROM objects ORDER BY rowid")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows, [(Some(4_000_000_000_123_456), 0), (None, 0)]);
        connection
            .execute("UPDATE objects SET retain_until_sub_micro_nanos = 999 WHERE retention_mode IS NOT NULL", [])
            .unwrap();
        for rejected in [
            "UPDATE objects SET retain_until_sub_micro_nanos = 1000 WHERE retention_mode IS NOT NULL",
            "UPDATE objects SET retain_until_sub_micro_nanos = -1 WHERE retention_mode IS NOT NULL",
            "UPDATE objects SET retain_until_sub_micro_nanos = 1 WHERE retention_mode IS NULL",
        ] {
            assert!(connection.execute(rejected, []).is_err(), "{rejected}");
        }
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 47);
    }

    #[test]
    fn session_value_keeps_the_remainder_and_reads_older_sessions() {
        let retention = ObjectRetention {
            mode: RetentionMode::Unlocked,
            retain_until: at(4_000_000_000, 123_456_000 + REMAINDER),
        };
        let value = session_value(Some(&Some(retention)));
        assert_eq!(value["value"]["retainUntilSubMicroNanos"], REMAINDER);
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
