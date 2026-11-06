//! Decides whether a new session may open and keeps the window of finished sessions.

use super::writers::{ClaimError, KeyOrigin, WriterGuard, WriterKey};
use super::{Catalog, CatalogError, unavailable};
use crate::storage::{blob_id::BlobId, catalog::sql, failure::BackendFailure};
use rusqlite::{Connection, Transaction};

/// How many COMPLETE and CANCELLED records stay after trimming. They exist so status queries and
/// repeated final requests get the original outcome, which clients ask for soon after the
/// transition. A seven-day window would not do: an SDK that opens one session per upload would
/// leave millions of records, and every maintenance pass would slow down with them.
const MAX_TERMINAL_SESSIONS: i64 = 4096;
/// Keeps one creation's transaction short; a larger excess carries over to the next creation.
const MAX_TRIMMED_PER_CREATION: usize = 256;
/// Candidates read beyond the excess, so a few held sessions do not cut the trim short.
const TRIM_SLACK: usize = 16;

/// Keeps exact session counts in the connection's temporary schema, because admission reads them
/// on every creation and a count over the table would scan every session. Triggers cover every
/// writer on this connection and roll back with its transaction. Temporary objects end with the
/// connection, so each open counts again and the catalog on disk stays unchanged.
pub(in crate::storage::catalog) fn open(connection: &Connection) -> Result<(), CatalogError> {
    connection
        .execute_batch(
            "CREATE TEMP TABLE resumable_session_counts (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                active INTEGER NOT NULL,
                terminal INTEGER NOT NULL
            ) WITHOUT ROWID;
            INSERT INTO temp.resumable_session_counts SELECT 1, coalesce(sum(state = 'ACTIVE'), 0), coalesce(sum(state != 'ACTIVE'), 0) FROM main.resumable_sessions;
            CREATE TEMP TRIGGER resumable_session_counted AFTER INSERT ON main.resumable_sessions BEGIN
                UPDATE resumable_session_counts SET active = active + (NEW.state = 'ACTIVE'), terminal = terminal + (NEW.state != 'ACTIVE');
            END;
            CREATE TEMP TRIGGER resumable_session_recounted AFTER UPDATE OF state ON main.resumable_sessions BEGIN
                UPDATE resumable_session_counts SET active = active + (NEW.state = 'ACTIVE') - (OLD.state = 'ACTIVE'), terminal = terminal + (NEW.state != 'ACTIVE') - (OLD.state != 'ACTIVE');
            END;
            CREATE TEMP TRIGGER resumable_session_uncounted AFTER DELETE ON main.resumable_sessions BEGIN
                UPDATE resumable_session_counts SET active = active - (OLD.state = 'ACTIVE'), terminal = terminal - (OLD.state != 'ACTIVE');
            END;",
        )
        .map_err(unavailable)
}

/// Checks the configured limit on active sessions and trims the oldest finished records in the
/// creating transaction. Only the limit can refuse: records that cannot go yet carry their excess
/// to a later creation, because a client should not fail over bookkeeping it cannot influence.
/// The returned leases keep the trimmed sessions out of reach until the transaction ends.
pub(super) fn admit_session(
    transaction: &Transaction<'_>,
    catalog: &Catalog,
) -> Result<Vec<WriterGuard>, CatalogError> {
    let (active, terminal): (i64, i64) = sql::row(
        transaction,
        "SELECT active, terminal FROM temp.resumable_session_counts",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if catalog
        .session_limits()
        .active_resumable
        .is_some_and(|limit| active >= i64::from(limit.get()))
    {
        return Err(unavailable(BackendFailure::RetainedSessionLimit));
    }
    // A count under the window is negative and fails the conversion: nothing to trim.
    let excess = usize::try_from(terminal - MAX_TERMINAL_SESSIONS).unwrap_or_default();
    trim_finished(transaction, catalog, excess.min(MAX_TRIMMED_PER_CREATION))
}

fn trim_finished(
    transaction: &Transaction<'_>,
    catalog: &Catalog,
    wanted: usize,
) -> Result<Vec<WriterGuard>, CatalogError> {
    let mut trimmed = Vec::with_capacity(wanted);
    if wanted == 0 {
        return Ok(trimmed);
    }
    // A completed session owns no staging bytes and never its published blob, which object
    // reclamation tracks. A cancelled one is reclaimable only after its cleanup journal entry
    // is gone; until then maintenance still needs the row.
    let candidates: Vec<(String, BlobId)> = transaction
        .prepare_cached("SELECT id, blob_id FROM resumable_sessions WHERE state = 'COMPLETE' OR (state = 'CANCELLED' AND NOT EXISTS(SELECT 1 FROM pending_blob_deletions WHERE blob_id = resumable_sessions.blob_id)) ORDER BY deadline, id LIMIT ?1")?
        .query_map([wanted + TRIM_SLACK], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, blob_id) in candidates {
        if trimmed.len() == wanted {
            break;
        }
        let guard = match catalog.claim_writer(WriterKey::Resumable(id.clone()), KeyOrigin::Catalog)
        {
            Ok(guard) => guard,
            // A request holding the session (such as a finalizer re-reading its outcome) keeps
            // it, and a catalog blocking new work only postpones the trim.
            Err(ClaimError::Busy | ClaimError::Catalog(_)) => continue,
            Err(error @ ClaimError::Poisoned) => return Err(error.into()),
        };
        // Finishing a session normally drops its RAM draft; this covers a failed drop.
        catalog.0.backend.finish_draft(&blob_id)?;
        sql::exec(
            transaction,
            "DELETE FROM resumable_sessions WHERE id = ?1",
            [&id],
        )?;
        trimmed.push(guard);
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests;
