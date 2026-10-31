//! Imported bucket times keep nanoseconds without changing native microsecond clocks.

use super::{system_time_micros, unavailable, unix_micros};
use crate::bucket::{BucketRecord, CatalogError};
use rusqlite::{Connection, Row, params, types::Type};
use std::time::{Duration, SystemTime};

pub(super) const SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN time_created_nanos INTEGER NOT NULL DEFAULT 0 CHECK(typeof(time_created_nanos)='integer' AND time_created_nanos BETWEEN 0 AND 999);
ALTER TABLE buckets ADD COLUMN updated_nanos INTEGER NOT NULL DEFAULT 0 CHECK(typeof(updated_nanos)='integer' AND updated_nanos BETWEEN 0 AND 999);
PRAGMA user_version=39;";

pub(super) fn read(row: &Row<'_>, micros: i64, column: &str) -> rusqlite::Result<SystemTime> {
    let index = row.as_ref().column_index(column)?;
    let remainder: i64 = row.get(column)?;
    if !(0..=999).contains(&remainder) {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Integer,
            "invalid bucket nanosecond remainder".into(),
        ));
    }
    system_time_micros(micros)?
        .checked_add(Duration::from_nanos(remainder as u64))
        .ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                Type::Integer,
                "bucket timestamp outside SystemTime range".into(),
            )
        })
}

pub(super) fn store_imported(
    connection: &Connection,
    record: &mut BucketRecord,
    created: SystemTime,
) -> Result<(), CatalogError> {
    let micros = unix_micros(created)?;
    if !crate::clock::rfc3339_micros(i128::from(micros)) {
        return Err(CatalogError::InvalidMetadata(
            "imported bucket timestamp is outside RFC3339 UTC range".into(),
        ));
    }
    let base = system_time_micros(micros).map_err(unavailable)?;
    let nanos = created
        .duration_since(base)
        .map_err(unavailable)?
        .as_nanos();
    let nanos = i64::try_from(nanos).map_err(unavailable)?;
    if nanos > 999 {
        return Err(unavailable("invalid imported bucket timestamp remainder"));
    }
    connection.prepare_cached("UPDATE buckets SET time_created=?2, updated=?2, time_created_nanos=?3, updated_nanos=?3 WHERE name=?1")
        .and_then(|mut statement|statement.execute(params![record.name,micros,nanos])).map_err(unavailable)?;
    record.time_created = created;
    record.updated = created;
    Ok(())
}

#[cfg(test)]
mod tests;
