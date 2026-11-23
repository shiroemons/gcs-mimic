//! The change journal's tables, which schema 15 created and older catalogs still upgrade
//! through. Nothing records or reads changes since directory synchronization, the journal's only
//! consumer, was removed: schema 58 drops both tables and moves the reset epoch that
//! `change_journal_state` kept into `reset_epoch`.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use crate::bucket::CatalogError;
use crate::storage::catalog::sql;
use rusqlite::Connection;

/// Schema 15, released text: test support rebuilds schema 57 from the same file.
pub(super) const SCHEMA: &str = include_str!("change_journal/schema.sql");

/// Checks the state the journal kept in a catalog from before schema 58: one state row, no record
/// at or below its floor, and at most the 65,536 records it retained.
pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    sql::validate(
        connection,
        "SELECT count(*) = 1 AND min(singleton) = 1 AND min(sequence) >= (SELECT coalesce(max(sequence), 0) FROM change_journal) AND (SELECT count(*) FROM change_journal) = min(sequence - floor) AND min(sequence - floor) <= 65536 AND NOT EXISTS(SELECT 1 FROM change_journal WHERE sequence <= (SELECT floor FROM change_journal_state)) FROM change_journal_state",
        "invalid change journal state",
    )
}

#[cfg(test)]
mod tests;
