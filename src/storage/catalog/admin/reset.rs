//! Instance-local reset owns the same gate as SQL jobs and unpublished byte leases.

use super::super::{ConnectionUnavailable, objects};
use super::{Admin, AdminError};
use crate::{bucket::CatalogError, storage::failure::unavailable};
use rusqlite::{Connection, TransactionBehavior};
use std::{path::Path, time::Duration};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResetOutcome {
    buckets: i64,
    objects: i64,
    cleanup_pending: bool,
}

impl Admin {
    pub(crate) async fn reset(&self) -> Result<ResetOutcome, AdminError> {
        let exclusive = self
            .catalog
            .0
            .activity
            .clone()
            .try_write_owned()
            .map_err(|_| AdminError::Busy {
                retry_after: Duration::from_secs(1),
            })?;
        if !self.catalog.is_healthy() {
            return Err(AdminError::Closed);
        }
        let execution = self
            .catalog
            .0
            .execution
            .clone()
            .try_acquire_owned()
            .map_err(|_| AdminError::Busy {
                retry_after: Duration::from_secs(1),
            })?;
        let inner = self.catalog.0.clone();
        self.catalog
            .0
            .executor
            .run(move || {
                // Cancellation cannot release admission while the owned transaction/cleanup runs.
                let (_exclusive, _execution) = (exclusive, execution);
                let result = inner.with_connection(|connection| {
                    reset(connection, &inner.data_dir, inner.memory.as_deref())
                });
                inner.maintenance_wake.notify_one();
                match result {
                    Ok(result) => result.map_err(AdminError::from),
                    Err(ConnectionUnavailable::Closed) => Err(AdminError::Closed),
                    Err(error) => Err(AdminError::backend(error)),
                }
            })
            .await
            .map_err(AdminError::backend)?
    }
}

fn reset(
    connection: &mut Connection,
    data_dir: &Path,
    memory: Option<&crate::storage::memory::Store>,
) -> Result<ResetOutcome, CatalogError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let mode = if memory.is_some() {
        Reclamation::ClearMemory
    } else {
        Reclamation::Journal
    };
    let (buckets, objects, next) = clear_in_transaction(&transaction, mode)?;
    if let Err(error) = objects::commit_publication(transaction) {
        // A commit can succeed before reporting an I/O error. The durable epoch resolves it.
        let committed: i64 = connection
            .prepare_cached("SELECT epoch FROM change_journal_state WHERE singleton = 1")
            .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
            .map_err(unavailable)?;
        if committed != next {
            return Err(unavailable(error));
        }
    }
    let cleanup = match memory {
        Some(memory) => memory.clear().map_err(unavailable),
        None => reclaim(connection, data_dir),
    };
    let cleanup_pending = match cleanup {
        Ok(()) => false,
        Err(error) => {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "reset_cleanup_deferred",
                serde_json::json!({"message":error.to_string()}),
            );
            true
        }
    };
    Ok(ResetOutcome {
        buckets,
        objects,
        cleanup_pending,
    })
}

#[derive(Clone, Copy)]
pub(super) enum Reclamation {
    Journal,
    ClearMemory,
}

pub(super) fn clear_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    mode: Reclamation,
) -> Result<(i64, i64, i64), CatalogError> {
    let importing: bool = transaction
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM import_journal WHERE active = 1)")
        .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
        .map_err(unavailable)?;
    if importing {
        return Err(CatalogError::PreconditionFailed);
    }
    let epoch: i64 = transaction
        .prepare_cached("SELECT epoch FROM change_journal_state WHERE singleton = 1")
        .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
        .map_err(unavailable)?;
    let next = epoch
        .checked_add(1)
        .ok_or_else(|| unavailable("reset epoch exhausted"))?;
    let (buckets, objects) = transaction
        .prepare_cached("SELECT (SELECT count(*) FROM buckets), (SELECT count(*) FROM objects)")
        .and_then(|mut statement| statement.query_row([], |row| Ok((row.get(0)?, row.get(1)?))))
        .map_err(unavailable)?;
    if matches!(mode, Reclamation::Journal) {
        transaction.prepare_cached("INSERT OR IGNORE INTO pending_blob_deletions SELECT blob_id FROM objects UNION SELECT blob_id FROM resumable_sessions UNION SELECT blob_id FROM derived_inputs UNION SELECT id FROM derived_transfers UNION SELECT blob_id FROM derived_deletions UNION SELECT blob_id FROM appendable_objects UNION SELECT blob_id FROM xml_multipart_parts UNION SELECT blob_id FROM xml_multipart_deletions")
            .and_then(|mut statement| statement.execute([])).map_err(unavailable)?;
    } else {
        // RAM can drop its whole registry after commit; growing a disk recovery journal would block a full catalog's reset.
        transaction
            .prepare_cached("DELETE FROM pending_blob_deletions")
            .and_then(|mut statement| statement.execute([]))
            .map_err(unavailable)?;
    }
    for sql in CLEAR {
        transaction
            .prepare_cached(sql)
            .and_then(|mut statement| statement.execute([]))
            .map_err(unavailable)?;
    }
    transaction.prepare_cached("UPDATE change_journal_state SET epoch = ?1, floor = sequence, enabled = 0 WHERE singleton = 1")
        .and_then(|mut statement| statement.execute([next])).map_err(unavailable)?;
    Ok((buckets, objects, next))
}

// High-water allocators, catalog identity and the blob deletion journal survive reset.
const CLEAR: &[&str] = &[
    "DELETE FROM notification_outbox",
    "DELETE FROM xml_multipart_parts",
    "DELETE FROM xml_multipart_uploads",
    "DELETE FROM xml_multipart_deletions",
    "DELETE FROM rewrite_sessions",
    "DELETE FROM resumable_sessions",
    "DELETE FROM derived_inputs",
    "DELETE FROM derived_deletions",
    "DELETE FROM derived_transfers",
    "DELETE FROM appendable_objects",
    "DELETE FROM upload_replays",
    "DELETE FROM sync_conflict_operations",
    "DELETE FROM sync_recovery_workspaces",
    "DELETE FROM sync_conflicts",
    "DELETE FROM sync_conflict_roots",
    "DELETE FROM sync_writeback_ops",
    "DELETE FROM sync_writeback_roots",
    "DELETE FROM sync_deletion_members",
    "DELETE FROM sync_deletion_sets",
    "DELETE FROM sync_finding_owners",
    "DELETE FROM sync_findings",
    "DELETE FROM sync_buckets",
    "DELETE FROM sync_objects",
    "DELETE FROM sync_path_stops",
    "DELETE FROM sync_observations",
    "DELETE FROM sync_roots",
    "DELETE FROM import_observations",
    "DELETE FROM import_journal",
    "DELETE FROM change_journal",
    "DELETE FROM hmac_keys",
    "DELETE FROM objects",
    "DELETE FROM bucket_policies",
    "DELETE FROM bucket_incarnations",
    "DELETE FROM buckets",
    "UPDATE startup_seed SET complete = 0",
];

fn reclaim(connection: &Connection, data_dir: &Path) -> Result<(), CatalogError> {
    loop {
        let ids = connection
            .prepare_cached("SELECT blob_id FROM pending_blob_deletions ORDER BY blob_id LIMIT 128")
            .map_err(unavailable)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(unavailable)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(unavailable)?;
        if ids.is_empty() {
            return Ok(());
        }
        for id in ids {
            let blob = crate::storage::blob_id::BlobId::parse(&id)
                .ok_or_else(|| unavailable("invalid reset deletion journal ID"))?;
            for path in [
                blob.blob_path(data_dir),
                blob.staging_path(data_dir),
                data_dir.join("appendable").join(&id),
            ] {
                #[cfg(test)]
                super::super::fault::hit(data_dir, super::super::fault::Step::ResetReclaim)
                    .map_err(unavailable)?;
                objects::unlink(&path).map_err(unavailable)?;
                #[cfg(unix)]
                if let Some(parent) = path.parent()
                    && parent.exists()
                {
                    #[cfg(test)]
                    super::super::fault::hit(data_dir, super::super::fault::Step::ResetReclaimSync)
                        .map_err(unavailable)?;
                    objects::sync_dir(parent).map_err(unavailable)?;
                }
            }
            connection
                .prepare_cached("DELETE FROM pending_blob_deletions WHERE blob_id = ?1")
                .and_then(|mut statement| statement.execute([id]))
                .map_err(unavailable)?;
        }
    }
}

#[cfg(test)]
mod tests;
