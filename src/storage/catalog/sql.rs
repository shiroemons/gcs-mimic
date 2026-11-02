//! Cached, static SQL shared by catalog operations.

use rusqlite::{
    Connection, OptionalExtension, Params, Transaction, TransactionBehavior, types::FromSql,
};

pub(super) fn scalar<T: FromSql>(
    connection: &Connection,
    sql: &'static str,
    params: impl Params,
) -> rusqlite::Result<T> {
    connection
        .prepare_cached(sql)?
        .query_row(params, |row| row.get(0))
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
