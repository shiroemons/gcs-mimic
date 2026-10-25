//! Durable local import progress; adapters own source verification, never SQL.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{Catalog, admin::AdminError, unavailable};
use crate::bucket::CatalogError;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub const MAX_PAGE: usize = 256;
pub(super) const SCHEMA: &str = "CREATE TABLE import_journal (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    source BLOB NOT NULL CHECK(typeof(source) = 'blob' AND length(source) = 32),
    fingerprint BLOB NOT NULL CHECK(typeof(fingerprint) = 'blob' AND length(fingerprint) = 32),
    expected INTEGER NOT NULL CHECK(typeof(expected) = 'integer' AND expected >= 0),
    cursor INTEGER NOT NULL CHECK(typeof(cursor) = 'integer' AND cursor BETWEEN 0 AND expected),
    active INTEGER NOT NULL CHECK(active IN (0, 1) AND (active = 1 OR cursor = expected))
) WITHOUT ROWID;
CREATE TABLE import_observations (
    ordinal INTEGER PRIMARY KEY CHECK(ordinal > 0),
    fingerprint BLOB NOT NULL CHECK(typeof(fingerprint) = 'blob' AND length(fingerprint) = 32),
    inode BLOB CHECK(inode IS NULL OR (typeof(inode) = 'blob' AND length(inode) = 8)),
    ctime_ns BLOB CHECK(ctime_ns IS NULL OR (typeof(ctime_ns) = 'blob' AND length(ctime_ns) = 16))
);
PRAGMA user_version = 16;";

pub(super) const FAILURE_SCHEMA: &str = "ALTER TABLE import_journal ADD COLUMN failure TEXT CHECK(failure IS NULL OR length(CAST(failure AS BLOB)) BETWEEN 1 AND 1024);
PRAGMA user_version = 18;";

pub(super) fn migrate_failure(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction
        .execute_batch(FAILURE_SCHEMA)
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

#[cfg(test)]
#[path = "import_journal_tests.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportBinding {
    pub source: [u8; 32],
    pub fingerprint: [u8; 32],
    pub expected_elements: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportElement {
    pub ordinal: i64,
    pub fingerprint: [u8; 32],
    pub inode: Option<u64>,
    pub ctime_ns: Option<i128>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportProgress {
    pub binding: ImportBinding,
    pub cursor: i64,
    pub active: bool,
    pub failure: Option<String>,
}

#[derive(Clone)]
pub struct ImportJournal(Catalog);

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection.query_row(
        "SELECT (SELECT count(*) FROM import_journal) <= 1
         AND (SELECT count(*) FROM import_observations) = coalesce((SELECT cursor FROM import_journal), 0)
         AND NOT EXISTS(SELECT 1 FROM import_observations WHERE ordinal > (SELECT cursor FROM import_journal))
         AND NOT EXISTS(SELECT 1 FROM import_journal WHERE active = 0 AND (SELECT complete FROM startup_seed) != 1)",
        [], |row| row.get(0),
    ).map_err(unavailable)?;
    if !valid {
        return Err(unavailable("invalid import journal state"));
    }
    Ok(())
}

fn progress(connection: &Connection) -> Result<Option<ImportProgress>, CatalogError> {
    connection.query_row(
        "SELECT source, fingerprint, expected, cursor, active, failure FROM import_journal WHERE singleton = 1",
        [], |row| Ok(ImportProgress {
            binding: ImportBinding { source: row.get(0)?, fingerprint: row.get(1)?, expected_elements: row.get(2)? },
            cursor: row.get(3)?, active: row.get(4)?,
            failure: row.get(5)?,
        }),
    ).optional().map_err(unavailable)
}

pub(super) fn matching(
    connection: &Connection,
    binding: &ImportBinding,
) -> Result<ImportProgress, CatalogError> {
    progress(connection)?
        .filter(|state| state.active && state.failure.is_none() && &state.binding == binding)
        .ok_or(CatalogError::PreconditionFailed)
}

impl Catalog {
    /// Trusted operator interface, independent of API authentication profiles.
    pub fn import_journal(&self) -> ImportJournal {
        ImportJournal(self.clone())
    }
}

pub(super) fn verify_destination(
    connection: &Connection,
    state: &ImportProgress,
) -> Result<(), CatalogError> {
    if state.active && state.failure.is_none() {
        let count: i64 = connection
            .query_row(
                "SELECT (SELECT count(*) FROM buckets) + (SELECT count(*) FROM objects)",
                [],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if count != state.cursor {
            return Err(CatalogError::PreconditionFailed);
        }
    }
    Ok(())
}

pub(super) fn check_publication(
    connection: &Connection,
    binding: &ImportBinding,
    cursor: i64,
) -> Result<(), CatalogError> {
    let state = matching(connection, binding)?;
    verify_destination(connection, &state)?;
    if state.cursor != cursor {
        return Err(CatalogError::PreconditionFailed);
    }
    Ok(())
}

pub(super) fn record_outcomes(
    transaction: &Transaction<'_>,
    binding: &ImportBinding,
    cursor: i64,
    elements: &[ImportElement],
    succeeded: &[bool],
) -> Result<(), CatalogError> {
    let state = matching(transaction, binding)?;
    if state.cursor != cursor
        || elements.is_empty()
        || elements.len() > MAX_PAGE
        || elements.len() != succeeded.len()
    {
        return Err(CatalogError::PreconditionFailed);
    }
    let prefix = succeeded.iter().take_while(|success| **success).count();
    if prefix == succeeded.len() {
        return record_in_transaction(transaction, binding, cursor, elements);
    }
    for (offset, (element, success)) in elements.iter().zip(succeeded).enumerate() {
        let ordinal = cursor
            .checked_add(offset as i64 + 1)
            .filter(|ordinal| *ordinal <= binding.expected_elements)
            .ok_or(CatalogError::PreconditionFailed)?;
        if element.ordinal != ordinal {
            return Err(CatalogError::PreconditionFailed);
        }
        if *success {
            transaction
                .execute(
                    "INSERT INTO import_observations VALUES (?1,?2,?3,?4)",
                    params![
                        ordinal,
                        element.fingerprint,
                        element.inode.map(u64::to_be_bytes),
                        element.ctime_ns.map(i128::to_be_bytes)
                    ],
                )
                .map_err(unavailable)?;
        }
    }
    transaction
        .execute(
            "UPDATE import_journal SET cursor = ?1, failure = ?2 WHERE singleton = 1",
            params![
                cursor + prefix as i64,
                (prefix != succeeded.len()).then_some("element_rejected")
            ],
        )
        .map_err(unavailable)?;
    Ok(())
}

pub(super) fn validate_failure(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection.query_row(
        "SELECT (SELECT count(*) FROM import_journal) <= 1
         AND NOT EXISTS(SELECT 1 FROM import_journal WHERE failure IS NOT NULL AND active = 0)
         AND NOT EXISTS(SELECT 1 FROM import_observations WHERE ordinal > coalesce((SELECT expected FROM import_journal),0))
         AND (SELECT count(*) FROM import_observations WHERE ordinal <= coalesce((SELECT cursor FROM import_journal),0)) = coalesce((SELECT cursor FROM import_journal),0)
         AND NOT EXISTS(SELECT 1 FROM import_journal WHERE failure IS NULL AND (SELECT count(*) FROM import_observations) != cursor)
         AND NOT EXISTS(SELECT 1 FROM import_journal WHERE active = 0 AND (SELECT complete FROM startup_seed) != 1)", [], |row| row.get(0),
    ).map_err(unavailable)?;
    if !valid {
        return Err(unavailable("invalid import failure state"));
    }
    Ok(())
}

impl ImportJournal {
    pub async fn progress(&self) -> Result<Option<ImportProgress>, AdminError> {
        self.0
            .run_admin(|connection| {
                let state = progress(connection)?;
                if let Some(state) = &state {
                    verify_destination(connection, state)?;
                }
                Ok(state)
            })
            .await
    }

    pub async fn fail(
        &self,
        binding: ImportBinding,
        reason: impl Into<String>,
    ) -> Result<(), AdminError> {
        let reason: String = reason.into().chars().take(256).collect();
        let reason = if reason.is_empty() {
            "adapter_failure".into()
        } else {
            reason
        };
        self.0
            .run_admin(move |connection| {
                let transaction = connection.transaction().map_err(unavailable)?;
                let state = progress(&transaction)?
                    .filter(|state| state.active && state.binding == binding)
                    .ok_or(CatalogError::PreconditionFailed)?;
                if state.failure.is_none() {
                    transaction
                        .execute(
                            "UPDATE import_journal SET failure = ?1 WHERE singleton = 1",
                            [reason],
                        )
                        .map_err(unavailable)?;
                }
                transaction.commit().map_err(unavailable)
            })
            .await
    }

    pub async fn in_progress(&self) -> Result<bool, AdminError> {
        self.0
            .run_admin(|connection| Ok(progress(connection)?.is_some_and(|state| state.active)))
            .await
    }

    pub async fn begin(&self, binding: ImportBinding) -> Result<ImportProgress, AdminError> {
        if binding.expected_elements < 0 {
            return Err(AdminError::Invalid(
                "import count must be nonnegative".into(),
            ));
        }
        let expected = binding.expected_elements;
        self.0.run_admin(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            if let Some(state) = progress(&transaction)? {
                verify_destination(&transaction, &state)?;
                return if state.active && state.failure.is_none() && state.binding == binding { Ok(state) }
                    else { Err(CatalogError::PreconditionFailed) };
            }
            let occupied: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM buckets) OR (SELECT complete FROM startup_seed WHERE singleton = 1)",
                [], |row| row.get(0),
            ).map_err(unavailable)?;
            if occupied { return Err(CatalogError::PreconditionFailed); }
            transaction.execute(
                "INSERT INTO import_journal (singleton,source,fingerprint,expected,cursor,active) VALUES (1, ?1, ?2, ?3, 0, 1)",
                params![binding.source, binding.fingerprint, expected],
            ).map_err(unavailable)?;
            transaction.commit().map_err(unavailable)?;
            Ok(ImportProgress { binding, cursor: 0, active: true, failure: None })
        }).await
    }

    pub async fn page(
        &self,
        binding: ImportBinding,
        after: i64,
        limit: usize,
    ) -> Result<Vec<ImportElement>, AdminError> {
        if !(1..=MAX_PAGE).contains(&limit) || !(0..=binding.expected_elements).contains(&after) {
            return Err(AdminError::Invalid(
                "import page requires a valid cursor and limit 1..=256".into(),
            ));
        }
        self.0.run_admin(move |connection| {
            matching(connection, &binding)?;
            connection.prepare(
                "SELECT ordinal, fingerprint, inode, ctime_ns FROM import_observations WHERE ordinal > ?1 ORDER BY ordinal LIMIT ?2",
            ).map_err(unavailable)?.query_map(params![after, limit as i64], |row| {
                Ok(ImportElement {
                    ordinal: row.get(0)?, fingerprint: row.get(1)?,
                    inode: row.get::<_, Option<[u8; 8]>>(2)?.map(u64::from_be_bytes),
                    ctime_ns: row.get::<_, Option<[u8; 16]>>(3)?.map(i128::from_be_bytes),
                })
            }).map_err(unavailable)?.collect::<rusqlite::Result<Vec<_>>>().map_err(unavailable)
        }).await
    }

    /// Caller must verify unchanged body digests before replacing a drifted observation.
    pub async fn revalidate(
        &self,
        binding: ImportBinding,
        before: ImportElement,
        after: ImportElement,
    ) -> Result<(), AdminError> {
        if before.ordinal != after.ordinal || before.fingerprint != after.fingerprint {
            return Err(AdminError::Invalid(
                "revalidation may only replace inode and ctime".into(),
            ));
        }
        self.0.run_admin(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            matching(&transaction, &binding)?;
            let updated = transaction.execute(
                "UPDATE import_observations SET inode = ?1, ctime_ns = ?2 WHERE ordinal = ?3 AND fingerprint = ?4 AND inode IS ?5 AND ctime_ns IS ?6",
                params![after.inode.map(u64::to_be_bytes), after.ctime_ns.map(i128::to_be_bytes), before.ordinal, before.fingerprint, before.inode.map(u64::to_be_bytes), before.ctime_ns.map(i128::to_be_bytes)],
            ).map_err(unavailable)?;
            if updated != 1 { return Err(CatalogError::PreconditionFailed); }
            transaction.commit().map_err(unavailable)
        }).await
    }

    /// Adapter calls only after final fingerprint/count/digest validation succeeds.
    pub async fn finish(&self, binding: ImportBinding) -> Result<(), AdminError> {
        self.0
            .run_admin(move |connection| {
                let transaction = connection.transaction().map_err(unavailable)?;
                let state = matching(&transaction, &binding)?;
                verify_destination(&transaction, &state)?;
                if state.cursor != binding.expected_elements {
                    return Err(CatalogError::PreconditionFailed);
                }
                transaction
                    .execute(
                        "UPDATE import_journal SET active = 0 WHERE singleton = 1",
                        [],
                    )
                    .map_err(unavailable)?;
                transaction
                    .execute(
                        "UPDATE startup_seed SET complete = 1 WHERE singleton = 1",
                        [],
                    )
                    .map_err(unavailable)?;
                transaction.commit().map_err(unavailable)
            })
            .await
    }
}

/// Shared publication calls this inside the transaction that commits the same elements.
pub(crate) fn record_in_transaction(
    transaction: &Transaction<'_>,
    binding: &ImportBinding,
    expected_cursor: i64,
    elements: &[ImportElement],
) -> Result<(), CatalogError> {
    if elements.is_empty() || elements.len() > MAX_PAGE {
        return Err(CatalogError::InvalidMetadata(
            "import batch requires 1..=256 elements".into(),
        ));
    }
    let state = matching(transaction, binding)?;
    if state.cursor != expected_cursor {
        return Err(CatalogError::PreconditionFailed);
    }
    let mut cursor = expected_cursor;
    for element in elements {
        cursor = cursor
            .checked_add(1)
            .filter(|value| *value <= binding.expected_elements)
            .ok_or(CatalogError::PreconditionFailed)?;
        if element.ordinal != cursor {
            return Err(CatalogError::PreconditionFailed);
        }
    }
    for element in elements {
        transaction
            .execute(
                "INSERT INTO import_observations VALUES (?1, ?2, ?3, ?4)",
                params![
                    element.ordinal,
                    element.fingerprint,
                    element.inode.map(u64::to_be_bytes),
                    element.ctime_ns.map(i128::to_be_bytes)
                ],
            )
            .map_err(unavailable)?;
    }
    transaction
        .execute(
            "UPDATE import_journal SET cursor = ?1 WHERE singleton = 1",
            [cursor],
        )
        .map_err(unavailable)?;
    Ok(())
}
