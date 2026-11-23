//! The change journal's tables, kept for the catalogs that hold them. Nothing records or reads
//! changes since directory synchronization, the journal's only consumer, was removed; reset still
//! keeps its epoch in `change_journal_state`.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use crate::bucket::CatalogError;
use crate::storage::catalog::sql;
use rusqlite::Connection;

pub(super) const SCHEMA: &str = "CREATE TABLE change_journal_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    sequence INTEGER NOT NULL CHECK(typeof(sequence) = 'integer' AND sequence >= 0),
    floor INTEGER NOT NULL CHECK(floor >= 0 AND floor <= sequence),
    epoch INTEGER NOT NULL CHECK(typeof(epoch) = 'integer' AND epoch >= 0),
    parent_pap TEXT NOT NULL
) WITHOUT ROWID;
INSERT INTO change_journal_state VALUES (1, 0, 0, 0, 0, '[]');
CREATE TABLE change_journal (
    sequence INTEGER PRIMARY KEY CHECK(sequence > 0),
    origin TEXT NOT NULL CHECK(origin IN ('api', 'seed', 'bucket-init', 'sync-ingest', 'import')),
    kind TEXT NOT NULL CHECK(kind IN ('publish', 'metadata', 'delete', 'bucket-create', 'bucket-update', 'bucket-delete', 'iam')),
    bucket TEXT NOT NULL,
    project TEXT NOT NULL,
    incarnation TEXT NOT NULL,
    object TEXT,
    generation INTEGER,
    metageneration INTEGER
);
PRAGMA user_version = 15;";

/// Checks the state the journal kept: one state row, no record at or below its floor, and at most
/// the 65,536 records it retained.
pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    sql::validate(
        connection,
        "SELECT count(*) = 1 AND min(singleton) = 1 AND min(sequence) >= (SELECT coalesce(max(sequence), 0) FROM change_journal) AND (SELECT count(*) FROM change_journal) = min(sequence - floor) AND min(sequence - floor) <= 65536 AND NOT EXISTS(SELECT 1 FROM change_journal WHERE sequence <= (SELECT floor FROM change_journal_state)) FROM change_journal_state",
        "invalid change journal state",
    )
}

#[cfg(test)]
mod tests;
