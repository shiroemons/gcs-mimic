//! Upload and part times keep the microseconds Cloud Storage reports in `Initiated` and
//! `LastModified`, past the whole milliseconds that expiry compares.

use crate::bucket::CatalogError;
use crate::storage::catalog::{UnixMillis, sql, unavailable, unix_micros};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, Type, ValueRef};
use std::time::{Duration, SystemTime};

/// Adds the microseconds past `xml_multipart_uploads.created` and `xml_multipart_parts.updated`.
/// Those columns stay whole milliseconds: `deadline` is checked as `created + 604800000`, indexed,
/// and compared with millisecond clock readings by every request, the maintenance sweep and
/// recovery, so rebuilding both tables in microseconds would rewrite all of that without changing
/// when an upload expires. Rows written before this version hold 0 here.
pub(in crate::storage::catalog) const SCHEMA: &str = "ALTER TABLE xml_multipart_uploads ADD COLUMN created_sub_milli_micros INTEGER NOT NULL DEFAULT 0 CHECK(typeof(created_sub_milli_micros)='integer' AND created_sub_milli_micros BETWEEN 0 AND 999);
ALTER TABLE xml_multipart_parts ADD COLUMN updated_sub_milli_micros INTEGER NOT NULL DEFAULT 0 CHECK(typeof(updated_sub_milli_micros)='integer' AND updated_sub_milli_micros BETWEEN 0 AND 999);
PRAGMA user_version = 54;";

/// The largest value the `*_sub_milli_micros` columns' CHECK allows.
const MAX_SUB_MILLI_MICROS: u16 = 999;

/// Microseconds past a stored whole millisecond, `0..=999`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SubMilliMicros(u16);

impl FromSql for SubMilliMicros {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let micros = i64::column_result(value)?;
        u16::try_from(micros)
            .ok()
            .filter(|micros| *micros <= MAX_SUB_MILLI_MICROS)
            .map(Self)
            .ok_or(FromSqlError::OutOfRange(micros))
    }
}

impl ToSql for SubMilliMicros {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

/// An upload or part time as its row stores it: whole milliseconds since the Unix epoch, which
/// `deadline` and expiry use, and the microseconds past them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StoredTime {
    pub(super) millis: UnixMillis,
    pub(super) micros: SubMilliMicros,
}

impl StoredTime {
    pub(super) fn system_time(self) -> rusqlite::Result<SystemTime> {
        self.millis
            .system_time()?
            .checked_add(Duration::from_micros(u64::from(self.micros.0)))
            .ok_or_else(|| {
                sql::conversion_error(0, Type::Integer, "timestamp outside SystemTime range")
            })
    }
}

impl TryFrom<SystemTime> for StoredTime {
    type Error = CatalogError;

    /// Truncates `time` toward the past to whole microseconds. The milliseconds equal
    /// `UnixMillis::try_from(time)`, so deadlines and expiry see the same instant as before.
    fn try_from(time: SystemTime) -> Result<Self, CatalogError> {
        let micros = unix_micros(time)?;
        let past_millis = u16::try_from(micros.rem_euclid(1000)).map_err(unavailable)?;
        Ok(Self {
            millis: UnixMillis(micros.div_euclid(1000)),
            micros: SubMilliMicros(past_millis),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn clock_readings_split_into_milliseconds_and_the_microseconds_past_them() {
        let time = UNIX_EPOCH + Duration::new(1_794_811_073, 548_719_321);
        let stored = StoredTime::try_from(time).unwrap();
        assert_eq!(
            stored,
            StoredTime {
                millis: UnixMillis(1_794_811_073_548),
                micros: SubMilliMicros(719),
            }
        );
        assert_eq!(stored.millis, UnixMillis::try_from(time).unwrap());
        assert_eq!(
            stored.system_time().unwrap(),
            UNIX_EPOCH + Duration::new(1_794_811_073, 548_719_000)
        );
    }

    /// Before the epoch the microseconds still count forward from the earlier millisecond, which
    /// is the one `UnixMillis` floors to.
    #[test]
    fn times_before_the_epoch_floor_to_the_earlier_microsecond_and_millisecond() {
        let time = UNIX_EPOCH - Duration::from_nanos(1_500);
        let stored = StoredTime::try_from(time).unwrap();
        assert_eq!(
            stored,
            StoredTime {
                millis: UnixMillis(-1),
                micros: SubMilliMicros(998),
            }
        );
        assert_eq!(stored.millis, UnixMillis::try_from(time).unwrap());
        assert_eq!(
            stored.system_time().unwrap(),
            UNIX_EPOCH - Duration::from_micros(2)
        );
    }

    #[test]
    fn stored_microseconds_outside_one_millisecond_fail_to_decode() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let decode = |value: i64| {
            let mut statement = connection.prepare_cached("SELECT ?1").unwrap();
            statement.query_row([value], |row| row.get::<_, SubMilliMicros>(0))
        };
        assert_eq!(decode(0).unwrap(), SubMilliMicros(0));
        assert_eq!(decode(999).unwrap(), SubMilliMicros(999));
        for invalid in [-1, 1000, i64::from(u16::MAX) + 1] {
            assert!(decode(invalid).is_err(), "{invalid}");
        }
    }
}
