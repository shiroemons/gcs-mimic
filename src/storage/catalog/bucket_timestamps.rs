//! Imported bucket times keep nanoseconds without changing native microsecond clocks.

use super::{system_time_micros, unavailable};
use crate::bucket::{BucketRecord, CatalogError};
use crate::storage::catalog::sql;
use rusqlite::{Connection, Row, params, types::Type};
use std::time::{Duration, SystemTime};

pub(super) const SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN time_created_nanos INTEGER NOT NULL DEFAULT 0 CHECK(typeof(time_created_nanos)='integer' AND time_created_nanos BETWEEN 0 AND 999);
ALTER TABLE buckets ADD COLUMN updated_nanos INTEGER NOT NULL DEFAULT 0 CHECK(typeof(updated_nanos)='integer' AND updated_nanos BETWEEN 0 AND 999);
PRAGMA user_version=39;";

/// Nanoseconds past a stored microsecond, as the `*_nanos` columns hold them (their CHECK is
/// `BETWEEN 0 AND 999`).
const MAX_SUB_MICRO_NANOS: u16 = 999;

/// A bucket time split into stored whole microseconds and the nanoseconds past them. Flooring
/// to the microsecond leaves at most 999 nanoseconds, so no remainder needs a check.
fn split(time: SystemTime) -> Result<(i64, u16), CatalogError> {
    let nanos = crate::clock::signed_nanos(time);
    let micros = i64::try_from(nanos.div_euclid(1000)).map_err(unavailable)?;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "rem_euclid(1000) is 0..=999"
    )]
    let remainder = nanos.rem_euclid(1000) as u16;
    Ok((micros, remainder))
}

/// The time a bucket row stores as whole microseconds in `column` and the nanoseconds past them
/// in `nanos_column`.
pub(super) fn read(
    row: &Row<'_>,
    column: &str,
    nanos_column: &str,
) -> rusqlite::Result<SystemTime> {
    let micros: i64 = row.get(column)?;
    let index = row.as_ref().column_index(nanos_column)?;
    let remainder: u64 = row.get(index)?;
    if remainder > u64::from(MAX_SUB_MICRO_NANOS) {
        return Err(sql::conversion_error(
            index,
            Type::Integer,
            "invalid bucket nanosecond remainder",
        ));
    }
    system_time_micros(micros)?
        .checked_add(Duration::from_nanos(remainder))
        .ok_or_else(|| {
            sql::conversion_error(
                index,
                Type::Integer,
                "bucket timestamp outside SystemTime range",
            )
        })
}

pub(super) fn store_imported(
    connection: &Connection,
    record: &mut BucketRecord,
    created: SystemTime,
) -> Result<(), CatalogError> {
    let (micros, nanos) = split(created)?;
    if !crate::clock::rfc3339_micros(i128::from(micros)) {
        return Err(CatalogError::InvalidMetadata(
            "imported bucket timestamp is outside RFC3339 UTC range".into(),
        ));
    }
    sql::exec(
        connection,
        "UPDATE buckets SET time_created=?2, updated=?2, time_created_nanos=?3, updated_nanos=?3 WHERE name=?1",
        params![record.name, micros, nanos],
    )?;
    record.time_created = created;
    record.updated = created;
    Ok(())
}

#[cfg(test)]
mod tests;
