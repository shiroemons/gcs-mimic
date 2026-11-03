//! Cached, static SQL shared by catalog operations.

use rusqlite::{
    Connection, OptionalExtension, Params, Row, Transaction, TransactionBehavior, types::FromSql,
};

pub(super) fn conversion_error(
    index: usize,
    kind: rusqlite::types::Type,
    error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, kind, error.into())
}

pub(super) fn row<T>(
    connection: &Connection,
    sql: &'static str,
    params: impl Params,
    read: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    connection.prepare_cached(sql)?.query_row(params, read)
}

pub(super) fn scalar<T: FromSql>(
    connection: &Connection,
    sql: &'static str,
    params: impl Params,
) -> rusqlite::Result<T> {
    row(connection, sql, params, |row| row.get(0))
}

pub(super) fn optional_scalar<T: FromSql>(
    connection: &Connection,
    sql: &'static str,
    params: impl Params,
) -> rusqlite::Result<Option<T>> {
    scalar(connection, sql, params).optional()
}

pub(super) fn exec(
    connection: &Connection,
    sql: &'static str,
    params: impl Params,
) -> rusqlite::Result<usize> {
    connection.prepare_cached(sql)?.execute(params)
}

pub(super) fn immediate(connection: &mut Connection) -> rusqlite::Result<Transaction<'_>> {
    connection.transaction_with_behavior(TransactionBehavior::Immediate)
}

pub(super) fn validate(
    connection: &Connection,
    sql: &'static str,
    message: &'static str,
) -> Result<(), crate::bucket::CatalogError> {
    if scalar(connection, sql, [])? {
        Ok(())
    } else {
        Err(super::unavailable(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_sql_values_preserve_bounds_without_wrapping() {
        let connection = Connection::open_in_memory().unwrap();
        for value in [0, u64::try_from(i64::MAX).unwrap()] {
            assert_eq!(
                scalar::<u64>(&connection, "SELECT ?1", [value]).unwrap(),
                value
            );
        }
        assert!(matches!(
            scalar::<u64>(&connection, "SELECT -1", []),
            Err(rusqlite::Error::IntegralValueOutOfRange(0, -1))
        ));
        assert!(matches!(
            scalar::<usize>(&connection, "SELECT -1", []),
            Err(rusqlite::Error::IntegralValueOutOfRange(0, -1))
        ));
        assert!(scalar::<u64>(&connection, "SELECT ?1", [u64::MAX]).is_err());
        if usize::BITS == 64 {
            assert!(scalar::<usize>(&connection, "SELECT ?1", [usize::MAX]).is_err());
        }
    }
    #[test]
    fn cached_statements_preserve_missing_rows_nulls_and_rollback() {
        let mut connection = Connection::open_in_memory().unwrap();
        exec(
            &connection,
            "CREATE TABLE values_for_test (value INTEGER)",
            [],
        )
        .unwrap();
        assert_eq!(
            optional_scalar::<i64>(&connection, "SELECT value FROM values_for_test", []).unwrap(),
            None
        );
        assert!(matches!(
            scalar::<i64>(&connection, "SELECT value FROM values_for_test", []),
            Err(rusqlite::Error::QueryReturnedNoRows)
        ));
        assert_eq!(
            exec(
                &connection,
                "INSERT INTO values_for_test VALUES (?1)",
                [None::<i64>]
            )
            .unwrap(),
            1
        );
        assert_eq!(
            optional_scalar::<Option<i64>>(&connection, "SELECT value FROM values_for_test", [])
                .unwrap(),
            Some(None)
        );
        {
            let transaction = immediate(&mut connection).unwrap();
            exec(&transaction, "UPDATE values_for_test SET value = ?1", [42]).unwrap();
            assert_eq!(
                scalar::<i64>(&transaction, "SELECT value FROM values_for_test", []).unwrap(),
                42
            );
        }
        assert_eq!(
            scalar::<Option<i64>>(&connection, "SELECT value FROM values_for_test", []).unwrap(),
            None
        );
        assert!(
            optional_scalar::<i64>(&connection, "SELECT value FROM missing_table", []).is_err()
        );
        validate(&connection, "SELECT 1", "invalid state").unwrap();
        assert!(
            matches!(validate(&connection, "SELECT 0", "invalid state"), Err(crate::bucket::CatalogError::Unavailable(cause)) if cause == "invalid state")
        );
    }
}
