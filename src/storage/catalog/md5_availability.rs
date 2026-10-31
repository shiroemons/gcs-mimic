//! Persist explicit MD5 absence separately from composite component counts.

use crate::object::Md5Availability;
use rusqlite::{Row, types::Type};

pub(super) const SCHEMA: &str = "ALTER TABLE objects ADD COLUMN md5_available INTEGER NOT NULL DEFAULT 1 CHECK(md5_available IN (0, 1) AND (md5_available = 1 OR component_count IS NULL)); PRAGMA user_version = 36;";

pub(super) fn read(row: &Row<'_>, index: usize) -> rusqlite::Result<Md5Availability> {
    match row.get::<_, i64>(index)? {
        0 => Ok(Md5Availability::Unavailable),
        1 => Ok(Md5Availability::Available),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Integer,
            "invalid MD5 availability".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn migration_preserves_legacy_values_and_retries_after_rollback() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE objects (name TEXT, component_count INTEGER, md5 BLOB); INSERT INTO objects VALUES ('normal',NULL,zeroblob(16)),('composite',3,zeroblob(16)); PRAGMA user_version=35;").unwrap();
        {
            let transaction = connection.transaction().unwrap();
            transaction.execute_batch(SCHEMA).unwrap();
            transaction.rollback().unwrap();
        }
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 35);
        let transaction = connection.transaction().unwrap();
        transaction.execute_batch(SCHEMA).unwrap();
        transaction.commit().unwrap();
        let records: Vec<(String, Option<i64>, Vec<u8>, i64)> = connection
            .prepare("SELECT name,component_count,md5,md5_available FROM objects ORDER BY name")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            records,
            vec![
                ("composite".into(), Some(3), vec![0; 16], 1),
                ("normal".into(), None, vec![0; 16], 1)
            ]
        );
        assert!(
            connection
                .execute("UPDATE objects SET md5_available=2", [])
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE objects SET md5_available=0 WHERE name='composite'",
                    []
                )
                .is_err()
        );
        connection
            .execute("UPDATE objects SET md5_available=0 WHERE name='normal'", [])
            .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT md5_available FROM objects WHERE name='normal'",
                    [],
                    |row| read(row, 0)
                )
                .unwrap(),
            Md5Availability::Unavailable
        );
    }
}
