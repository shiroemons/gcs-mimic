//! Persist the three states of an object hold: never set, released and held.
//!
//! Cloud Storage reports `temporaryHold` and `eventBasedHold` as `false` once they were set
//! and omits them on an object that never had one. The original `temporary_hold` and
//! `event_based_hold` columns hold only 0 and 1, so each gains a companion flag that marks
//! "released". Adding a column keeps every older row readable and avoids rebuilding the
//! objects table; the state is `held` for 1, `released` for 0 with the flag, otherwise unset.

use rusqlite::{Row, types::Type};

/// Schema 48. Rows written earlier read as unset (0) or held (1): a stored 0 cannot show
/// whether the hold was ever set, and reporting a `false` that Cloud Storage might not have
/// reported would be invented information.
pub(super) const SCHEMA: &str = "ALTER TABLE objects ADD COLUMN temporary_hold_released INTEGER NOT NULL DEFAULT 0 CHECK(temporary_hold_released IN (0, 1) AND (temporary_hold = 0 OR temporary_hold_released = 0));
ALTER TABLE objects ADD COLUMN event_based_hold_released INTEGER NOT NULL DEFAULT 0 CHECK(event_based_hold_released IN (0, 1) AND (event_based_hold = 0 OR event_based_hold_released = 0));
PRAGMA user_version = 48;";

/// The column pair (`hold`, `hold_released`) that stores a state.
pub(super) fn columns(state: Option<bool>) -> (bool, bool) {
    (state == Some(true), state == Some(false))
}

pub(super) fn read(row: &Row<'_>, hold: usize, released: usize) -> rusqlite::Result<Option<bool>> {
    let flag = |index: usize| -> rusqlite::Result<bool> {
        match row.get::<_, i64>(index)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(rusqlite::Error::FromSqlConversionFailure(
                index,
                Type::Integer,
                "invalid hold flag".into(),
            )),
        }
    };
    Ok(match (flag(hold)?, flag(released)?) {
        (true, _) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn legacy() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE objects (
                    name TEXT,
                    event_based_hold INTEGER NOT NULL DEFAULT 0 CHECK(event_based_hold IN (0, 1)),
                    temporary_hold INTEGER NOT NULL DEFAULT 0 CHECK(temporary_hold IN (0, 1)));
                 INSERT INTO objects VALUES ('plain', 0, 0), ('temporary', 0, 1), ('event', 1, 0);
                 PRAGMA user_version = 47;",
            )
            .unwrap();
        connection
    }

    fn states(connection: &Connection) -> Vec<(String, Option<bool>, Option<bool>)> {
        connection
            .prepare("SELECT name, temporary_hold, temporary_hold_released, event_based_hold, event_based_hold_released FROM objects ORDER BY name")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, read(row, 1, 2)?, read(row, 3, 4)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Existing 0 rows become unset and 1 rows stay held; the migration is atomic.
    #[test]
    fn migration_reads_legacy_rows_as_unset_or_held_and_retries_after_rollback() {
        let mut connection = legacy();
        {
            let transaction = connection.transaction().unwrap();
            transaction.execute_batch(SCHEMA).unwrap();
            transaction.rollback().unwrap();
        }
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 47);
        let transaction = connection.transaction().unwrap();
        transaction.execute_batch(SCHEMA).unwrap();
        transaction.commit().unwrap();
        assert_eq!(
            states(&connection),
            [
                ("event".into(), None, Some(true)),
                ("plain".into(), None, None),
                ("temporary".into(), Some(true), None),
            ]
        );
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 48);
    }

    #[test]
    fn released_state_round_trips_and_conflicting_flags_are_refused() {
        let connection = legacy();
        connection.execute_batch(SCHEMA).unwrap();
        for (state, name) in [
            (None, "plain"),
            (Some(false), "event"),
            (Some(true), "temporary"),
        ] {
            let (hold, released) = columns(state);
            connection
                .execute(
                    "UPDATE objects SET temporary_hold = ?1, temporary_hold_released = ?2 WHERE name = ?3",
                    rusqlite::params![hold, released, name],
                )
                .unwrap();
        }
        let temporary: Vec<_> = states(&connection).into_iter().map(|row| row.1).collect();
        assert_eq!(temporary, [Some(false), None, Some(true)]);
        for rejected in [
            "UPDATE objects SET temporary_hold = 1, temporary_hold_released = 1",
            "UPDATE objects SET event_based_hold = 1, event_based_hold_released = 1",
            "UPDATE objects SET temporary_hold_released = 2",
        ] {
            assert!(connection.execute(rejected, []).is_err(), "{rejected}");
        }
    }
}
