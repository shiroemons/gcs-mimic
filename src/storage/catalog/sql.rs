//! Cached, static SQL shared by catalog operations.

use rusqlite::{
    Connection, OptionalExtension, Params, Row, Transaction, TransactionBehavior, config::DbConfig,
    types::FromSql,
};

/// Makes a prepared statement keep its plan when a bound value changes. SQLite otherwise recompiles
/// any statement whose plan it thinks a bound value could affect, as it did the object ACL join on
/// every read. No plan here needs a bound value: no LIKE or GLOB takes a parameter, no partial
/// index predicate is compared with one, and nothing runs ANALYZE.
pub(super) fn keep_plans_across_bindings(connection: &Connection) -> rusqlite::Result<()> {
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_QPSG, true)
        .map(drop)
}

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
        $crate::storage::catalog::sql::text_enum!($name, $invalid, Self::parse);
    };
    ($name:ty, $invalid:literal, $parse:expr) => {
        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::Borrowed(self.as_str().into()))
            }
        }
        impl rusqlite::types::FromSql for $name {
            fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
                ($parse)(value.as_str()?).ok_or_else(|| rusqlite::types::FromSqlError::Other($invalid.into()))
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

/// Declares the positions of a `SELECT` list written as a literal macro elsewhere, for a row read
/// on a hot path where looking columns up by name would cost a scan of the column names per
/// read. A unit test compares the declaration with the statement SQLite prepares.
macro_rules! column_positions {
    ($visibility:vis enum $name:ident { $($variant:ident => $column:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(usize)]
        $visibility enum $name { $($variant),+ }

        impl $name {
            /// The column's position in the `SELECT` list.
            $visibility const fn index(self) -> usize {
                self as usize
            }

            /// Every column in list order, with the name SQLite reports for it.
            #[cfg(test)]
            $visibility const COLUMNS: &'static [(Self, &'static str)] =
                &[$((Self::$variant, $column)),+];
        }
    };
}

pub(super) use column_positions;

/// A BLOB column that holds exactly `N` bytes, such as a digest; another length is `invalid`.
pub(super) fn fixed_blob<const N: usize>(
    row: &Row<'_>,
    index: usize,
    invalid: &'static str,
) -> rusqlite::Result<[u8; N]> {
    sized_blob(row.get(index)?, index, invalid)
}

/// A nullable [`fixed_blob`] column.
pub(super) fn optional_fixed_blob<const N: usize>(
    row: &Row<'_>,
    index: usize,
    invalid: &'static str,
) -> rusqlite::Result<Option<[u8; N]>> {
    row.get::<_, Option<Vec<u8>>>(index)?
        .map(|bytes| sized_blob(bytes, index, invalid))
        .transpose()
}

fn sized_blob<const N: usize>(
    bytes: Vec<u8>,
    index: usize,
    invalid: &'static str,
) -> rusqlite::Result<[u8; N]> {
    bytes
        .try_into()
        .map_err(|_| conversion_error(index, rusqlite::types::Type::Blob, invalid))
}

text_enum!(
    crate::bucket::StorageClass,
    "invalid stored storage class",
    |value: &str| value.parse().ok()
);
text_enum!(
    crate::bucket::LocationKind,
    "invalid stored location type",
    crate::bucket::LocationKind::from_stored
);
text_enum!(
    crate::bucket::Location,
    "invalid stored bucket location",
    crate::bucket::Location::from_canonical
);
integer_enum!(crate::bucket::PublicAccessPrevention, { Inherited => 0, Enforced => 1 });
// The effective access a bucket query computes from its own setting and its parent project's.
integer_enum!(crate::auth::PublicAccess, { Allowed => 0, Prevented => 1 });

// Only sync_state columns decode into these types: those rows are written from them. Catalog
// tables (`objects`, `buckets`, ...) may hold names the fake-gcs-server profile or an older
// transport accepted, so their rows decode as `String`.
macro_rules! name_column {
    ($name:ty) => {
        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::Borrowed(
                    rusqlite::types::ValueRef::Text(self.as_str().as_bytes()),
                ))
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                <$name>::try_from(value.as_str()?)
                    .map_err(|error| rusqlite::types::FromSqlError::Other(Box::new(error)))
            }
        }
    };
}

name_column!(crate::bucket::name::BucketName);
name_column!(crate::object::name::ObjectName);

// Binding only: stored project columns may predate the identifier rules, so rows decode as
// `String`.
impl rusqlite::ToSql for crate::auth::ProjectId {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Borrowed(
            rusqlite::types::ValueRef::Text(self.as_str().as_bytes()),
        ))
    }
}

impl rusqlite::ToSql for crate::object::generation::Generation {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Owned(self.get().into()))
    }
}

impl rusqlite::types::FromSql for crate::object::generation::Generation {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let value = i64::column_result(value)?;
        Self::try_from(value).map_err(|_| rusqlite::types::FromSqlError::OutOfRange(value))
    }
}

pub(super) fn conversion_error(
    index: usize,
    kind: rusqlite::types::Type,
    error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, kind, error.into())
}

/// A value stored as JSON text. Decoding failures surface as `FromSqlConversionFailure` for the
/// column (classified as corrupt, with the serde error as the source); encoding failures as
/// `ToSqlConversionFailure` (unavailable), as the hand-written `map_err(unavailable)` did.
pub(super) struct Json<T>(pub(super) T);

impl<T: serde::de::DeserializeOwned> FromSql for Json<T> {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        serde_json::from_str(value.as_str()?)
            .map(Self)
            .map_err(|error| rusqlite::types::FromSqlError::Other(Box::new(error)))
    }
}

impl<T: serde::Serialize> rusqlite::ToSql for Json<T> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        serde_json::to_string(&self.0)
            .map(rusqlite::types::ToSqlOutput::from)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
    }
}

#[cfg(test)]
mod json_tests {
    use super::{Json, row, scalar};
    use crate::bucket::CatalogError;
    use rusqlite::Connection;
    use std::collections::BTreeMap;

    #[test]
    fn json_columns_round_trip_and_report_corruption_with_the_serde_source() {
        let connection = Connection::open_in_memory().unwrap();
        let labels = BTreeMap::from([("b".to_owned(), "2".to_owned()), ("a".into(), "1".into())]);
        assert_eq!(
            scalar::<String>(&connection, "SELECT ?1", [Json(&labels)]).unwrap(),
            r#"{"a":"1","b":"2"}"#
        );
        assert_eq!(
            scalar::<Json<BTreeMap<String, String>>>(&connection, "SELECT ?1", [Json(&labels)])
                .unwrap()
                .0,
            labels
        );
        assert!(
            scalar::<Option<Json<Vec<u8>>>>(&connection, "SELECT NULL", [])
                .unwrap()
                .is_none()
        );

        let error = scalar::<Json<Vec<u8>>>(&connection, "SELECT 'broken'", [])
            .err()
            .unwrap();
        assert!(matches!(
            error,
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, _)
        ));
        let error = super::super::unavailable(error);
        assert!(matches!(error, CatalogError::Corrupt(_)), "{error:?}");
        assert!(crate::error::causes(&error).any(|cause| cause.is::<serde_json::Error>()));

        // JSON objects need string keys, so this map cannot be encoded.
        let unencodable = BTreeMap::from([(vec![1_u8], 1_u8)]);
        let error = scalar::<String>(&connection, "SELECT ?1", [Json(&unencodable)]).unwrap_err();
        assert!(matches!(error, rusqlite::Error::ToSqlConversionFailure(_)));
        let error = super::super::unavailable(error);
        assert!(matches!(error, CatalogError::Unavailable(_)), "{error:?}");
        assert!(crate::error::causes(&error).any(|cause| cause.is::<serde_json::Error>()));
    }

    /// A JSON column reports its own index: malformed text as a conversion failure and NULL or a
    /// non-text value as the wrong column type, both catalog corruption.
    #[test]
    fn json_columns_report_malformed_values_at_their_index_as_corruption() {
        let connection = Connection::open_in_memory().unwrap();
        let stored = BTreeMap::from([("key".to_owned(), "value".to_owned())]);
        let text = scalar::<String>(&connection, "SELECT ?1", [Json(&stored)]).unwrap();
        assert_eq!(text, r#"{"key":"value"}"#);
        let read = row(&connection, "SELECT 0, ?1", [&text], |row| {
            row.get::<_, Json<BTreeMap<String, String>>>(1)
        })
        .unwrap();
        assert_eq!(read.0, stored);
        let malformed = row(&connection, "SELECT 0, '{'", [], |row| {
            row.get::<_, Json<BTreeMap<String, String>>>(1)
        })
        .err()
        .unwrap();
        assert!(matches!(
            malformed,
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, ref error)
                if error.is::<serde_json::Error>()
        ));
        for sql in ["SELECT 0, NULL", "SELECT 0, 7"] {
            let error = row(&connection, sql, [], |row| {
                row.get::<_, Json<BTreeMap<String, String>>>(1)
            })
            .err()
            .unwrap();
            assert!(
                matches!(error, rusqlite::Error::InvalidColumnType(1, ..)),
                "{sql}: {error:?}"
            );
            assert!(matches!(
                super::super::unavailable(error),
                CatalogError::Corrupt(_)
            ));
        }
        assert!(matches!(
            super::super::unavailable(malformed),
            CatalogError::Corrupt(_)
        ));
        let absent = row(&connection, "SELECT NULL", [], |row| {
            row.get::<_, Option<Json<BTreeMap<String, String>>>>(0)
        })
        .unwrap();
        assert!(absent.is_none());
    }
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

/// One consistent view across a read's statements. `Connection::transaction` parses `BEGIN` and
/// `ROLLBACK` again on every call, which is a real share of a point read; these stay prepared.
pub(super) struct ReadSnapshot<'a>(&'a Connection);

pub(super) fn read_snapshot(connection: &Connection) -> rusqlite::Result<ReadSnapshot<'_>> {
    exec(connection, "BEGIN DEFERRED", [])?;
    Ok(ReadSnapshot(connection))
}

impl std::ops::Deref for ReadSnapshot<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.0
    }
}

impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        // A failure leaves the transaction open: the read pool retires such a connection, and the
        // writer's panic recovery and `BEGIN` errors surface it on the serialized path.
        let _ = exec(self.0, "ROLLBACK", []);
    }
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
                row.get::<_, crate::bucket::StorageClass>(0)
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
        use crate::bucket::{Location, LocationKind, StorageClass};
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
            (StorageClass::Standard, "STANDARD"),
            (StorageClass::Nearline, "NEARLINE"),
            (StorageClass::Coldline, "COLDLINE"),
            (StorageClass::Archive, "ARCHIVE"),
            (StorageClass::MultiRegional, "MULTI_REGIONAL"),
            (
                StorageClass::DurableReducedAvailability,
                "DURABLE_REDUCED_AVAILABILITY",
            ),
        ]);
        text_values(&[
            (LocationKind::Region, "region"),
            (LocationKind::DualRegion, "dual-region"),
            (LocationKind::MultiRegion, "multi-region"),
        ]);
        text_values(
            &["US", "ASIA-NORTHEAST1", "NAM4"]
                .map(|name| (name.parse::<Location>().unwrap(), name)),
        );
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
        {
            use crate::storage::catalog::resumable::{StoredState, TerminalReason};
            text_values(&[
                (StoredState::Active, "ACTIVE"),
                (StoredState::Complete, "COMPLETE"),
                (StoredState::Cancelled, "CANCELLED"),
            ]);
            text_values(&[
                (TerminalReason::Aborted, "aborted"),
                (TerminalReason::ContentRange, "content-range"),
            ]);
        }
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
                (crate::bucket::PublicAccessPrevention::Inherited, 0),
                (crate::bucket::PublicAccessPrevention::Enforced, 1),
            ],
            2,
        );
        integer_values(
            &[
                (crate::auth::PublicAccess::Allowed, 0),
                (crate::auth::PublicAccess::Prevented, 1),
            ],
            2,
        );
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
        integer_values(
            &[
                (
                    crate::storage::catalog::sync_state::conflicts::Action::Restore,
                    0,
                ),
                (
                    crate::storage::catalog::sync_state::conflicts::Action::Discard,
                    1,
                ),
            ],
            2,
        );
    }

    /// sync_state rows are written only from these types, so a value outside their rules is a
    /// corrupt catalog.
    #[test]
    fn sync_state_name_and_generation_columns_reject_invalid_values_as_corruption() {
        use crate::bucket::name::BucketName;
        use crate::object::{generation::Generation, name::ObjectName};
        let connection = Connection::open_in_memory().unwrap();
        for value in [1, i64::MAX] {
            let generation = Generation::try_from(value).unwrap();
            assert_eq!(
                scalar::<i64>(&connection, "SELECT ?1", [generation]).unwrap(),
                value
            );
            assert_eq!(
                scalar::<Generation>(&connection, "SELECT ?1", [value]).unwrap(),
                generation
            );
        }
        for invalid in [0, -1, i64::MIN] {
            let error = scalar::<Generation>(&connection, "SELECT ?1", [invalid]).unwrap_err();
            assert!(
                matches!(error, rusqlite::Error::IntegralValueOutOfRange(0, value) if value == invalid)
            );
            assert!(matches!(
                super::super::unavailable(error),
                crate::bucket::CatalogError::Corrupt(_)
            ));
        }
        let bucket = BucketName::try_from("test-bucket").unwrap();
        let name = ObjectName::try_from("日本語/a b").unwrap();
        assert_eq!(
            scalar::<BucketName>(&connection, "SELECT ?1", [&bucket]).unwrap(),
            bucket
        );
        assert_eq!(
            scalar::<ObjectName>(&connection, "SELECT ?1", [&name]).unwrap(),
            name
        );
        let errors = [
            scalar::<BucketName>(&connection, "SELECT 'UPPER'", []).unwrap_err(),
            scalar::<ObjectName>(&connection, "SELECT '..'", []).unwrap_err(),
        ];
        for error in errors {
            assert!(matches!(
                error,
                rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, _)
            ));
            assert!(matches!(
                super::super::unavailable(error),
                crate::bucket::CatalogError::Corrupt(_)
            ));
        }
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
