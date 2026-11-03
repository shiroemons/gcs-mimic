//! Cached, static SQL shared by catalog operations.

use rusqlite::{
    Connection, OptionalExtension, Params, Row, Transaction, TransactionBehavior, types::FromSql,
};

macro_rules! text_enum {
    ($visibility:vis $name:ty, $invalid:literal, { $($variant:ident => $value:literal),+ $(,)? }) => {
        impl $name {
            $visibility fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
            $visibility fn parse(value: &str) -> Option<Self> {
                match value { $($value => Some(Self::$variant)),+, _ => None }
            }
        }
        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::Borrowed(self.as_str().into()))
            }
        }
        impl rusqlite::types::FromSql for $name {
            fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
                Self::parse(value.as_str()?).ok_or_else(|| rusqlite::types::FromSqlError::Other($invalid.into()))
            }
        }
    };
}

macro_rules! integer_enum {
    ($name:ty, { $($variant:ident => $value:literal),+ $(,)? }) => {
        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                let value: i64 = match self { $(Self::$variant => $value),+ };
                Ok(rusqlite::types::ToSqlOutput::Owned(value.into()))
            }
        }
        impl rusqlite::types::FromSql for $name {
            fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
                let value = <i64 as rusqlite::types::FromSql>::column_result(value)?;
                match value { $($value => Ok(Self::$variant)),+, _ => Err(rusqlite::types::FromSqlError::OutOfRange(value)) }
            }
        }
    };
}

pub(super) use {integer_enum, text_enum};

pub(super) fn conversion_error(
    index: usize,
    kind: rusqlite::types::Type,
    error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, kind, error.into())
}

pub(super) fn storage_class(row: &Row<'_>, index: usize) -> rusqlite::Result<String> {
    let value: String = row.get(index)?;
    crate::bucket::validate_storage_class(&value)
        .map_err(|error| conversion_error(index, rusqlite::types::Type::Text, error))?;
    Ok(value)
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
    fn unknown_stored_storage_class_is_catalog_corruption() {
        let connection = Connection::open_in_memory().unwrap();
        for class in ["unknown", "standard", "REGIONAL", "RAPID", ""] {
            let error = row(&connection, "SELECT ?1", [class], |row| {
                storage_class(row, 0)
            })
            .unwrap_err();
            assert!(
                matches!(
                    super::super::unavailable(error),
                    crate::bucket::CatalogError::Corrupt(_)
                ),
                "{class}"
            );
        }
    }

    fn text_values<T: rusqlite::ToSql + FromSql + std::fmt::Debug + PartialEq>(
        values: &[(T, &str)],
    ) {
        let connection = Connection::open_in_memory().unwrap();
        for (variant, text) in values {
            assert_eq!(
                scalar::<String>(&connection, "SELECT ?1", [variant]).unwrap(),
                *text
            );
            assert_eq!(
                &scalar::<T>(&connection, "SELECT ?1", [text]).unwrap(),
                variant
            );
        }
        let error = scalar::<T>(&connection, "SELECT 'bogus'", []).unwrap_err();
        assert!(matches!(
            error,
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, _)
        ));
        assert!(matches!(
            super::super::unavailable(error),
            crate::bucket::CatalogError::Corrupt(_)
        ));
    }

    fn integer_values<T: rusqlite::ToSql + FromSql + std::fmt::Debug + PartialEq>(
        values: &[(T, i64)],
        upper: i64,
    ) {
        let connection = Connection::open_in_memory().unwrap();
        for (variant, integer) in values {
            assert_eq!(
                scalar::<i64>(&connection, "SELECT ?1", [variant]).unwrap(),
                *integer
            );
            assert_eq!(
                &scalar::<T>(&connection, "SELECT ?1", [integer]).unwrap(),
                variant
            );
        }
        for invalid in [-1, upper] {
            let error = scalar::<T>(&connection, "SELECT ?1", [invalid]).unwrap_err();
            assert!(
                matches!(error, rusqlite::Error::IntegralValueOutOfRange(0, value) if value == invalid)
            );
            assert!(matches!(
                super::super::unavailable(error),
                crate::bucket::CatalogError::Corrupt(_)
            ));
        }
    }

    #[test]
    fn stored_enum_values_keep_the_catalog_format() {
        use crate::storage::catalog::{
            admin::Origin,
            change_journal::ChangeKind,
            hmac_keys::HmacKeyState,
            sync_state::{
                observation::PathStop,
                writeback::{Kind, Phase},
            },
        };
        use crate::sync::mapping::Reason;
        text_values(&[
            (Origin::Api, "api"),
            (Origin::Seed, "seed"),
            (Origin::BucketInit, "bucket-init"),
            (Origin::SyncIngest, "sync-ingest"),
            (Origin::Import, "import"),
        ]);
        text_values(&[
            (ChangeKind::Publish, "publish"),
            (ChangeKind::Metadata, "metadata"),
            (ChangeKind::Delete, "delete"),
            (ChangeKind::BucketCreate, "bucket-create"),
            (ChangeKind::BucketUpdate, "bucket-update"),
            (ChangeKind::BucketDelete, "bucket-delete"),
            (ChangeKind::Iam, "iam"),
        ]);
        text_values(&[
            (HmacKeyState::Active, "ACTIVE"),
            (HmacKeyState::Inactive, "INACTIVE"),
            (HmacKeyState::Deleted, "DELETED"),
        ]);
        text_values(&[
            (Reason::InvalidName, "invalid-name"),
            (Reason::HostRestriction, "host-restriction"),
            (Reason::Reserved, "reserved"),
            (Reason::Ignored, "ignored"),
            (Reason::Collision, "collision"),
            (Reason::UnsafeEntry, "unsafe-entry"),
        ]);
        text_values(&[
            (PathStop::ProtectedDeletion, "ProtectedDeletion"),
            (PathStop::SourceChanging, "SourceChanging"),
            (PathStop::IoFailure, "IoFailure"),
        ]);
        integer_values(&[(Kind::Publish, 0), (Kind::Remove, 1)], 2);
        integer_values(
            &[
                (Phase::Prepared, 0),
                (Phase::Armed, 1),
                (Phase::Protected, 2),
                (Phase::Durable, 3),
                (Phase::Complete, 4),
            ],
            5,
        );
    }

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
