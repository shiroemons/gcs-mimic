//! Instance-local reset owns the same gate as SQL jobs and unpublished byte leases.

use super::super::{BlobBackend, ConnectionUnavailable, objects};
use super::{Admin, AdminError};
use crate::storage::catalog::sql;
use crate::{bucket::CatalogError, storage::failure::unavailable};
use rusqlite::Connection;
use std::{path::Path, time::Duration};

const QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResetOutcome {
    buckets: i64,
    objects: i64,
    cleanup_pending: bool,
}

impl Admin {
    #[cfg(test)]
    pub(crate) async fn reset(&self) -> Result<ResetOutcome, AdminError> {
        self.reset_until(tokio::time::Instant::now() + QUIESCENCE_TIMEOUT)
            .await
    }

    pub(crate) async fn reset_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<ResetOutcome, AdminError> {
        let deadline = deadline.min(tokio::time::Instant::now() + QUIESCENCE_TIMEOUT);
        let admission = self
            .catalog
            .wait_admin(self.catalog.0.admin_reads.clone(), deadline)
            .await?;
        let (exclusive, execution) = loop {
            let execution = self
                .catalog
                .wait_admin(self.catalog.0.execution.clone(), deadline)
                .await?;
            // Queuing a writer would reject the nested SQL read leases needed by draining uploads.
            if let Ok(exclusive) = self.catalog.0.activity.clone().try_write_owned() {
                break (exclusive, execution);
            }
            drop(execution);
            tokio::time::sleep_until(
                deadline.min(tokio::time::Instant::now() + Duration::from_millis(10)),
            )
            .await;
        };
        let inner = self.catalog.0.clone();
        self.catalog
            .0
            .executor
            .run(move || {
                // Cancellation cannot release admission while the owned transaction/cleanup runs.
                let (_admission, _exclusive, _execution) = (admission, exclusive, execution);
                let result = inner.with_connection(|connection| reset(connection, &inner.backend));
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

fn reset(connection: &mut Connection, backend: &BlobBackend) -> Result<ResetOutcome, CatalogError> {
    let transaction = sql::immediate(connection)?;
    let mode = if backend.is_memory() {
        Reclamation::ClearMemory
    } else {
        Reclamation::Journal
    };
    let (buckets, objects, next) = clear_in_transaction(&transaction, mode)?;
    if let Err(error) = objects::commit_publication(transaction) {
        // A commit can succeed before reporting an I/O error. The durable epoch resolves it.
        let committed: i64 = sql::scalar(
            connection,
            "SELECT epoch FROM change_journal_state WHERE singleton = 1",
            [],
        )?;
        if committed != next {
            return Err(unavailable(error));
        }
    }
    let cleanup = match backend {
        BlobBackend::Memory(_) => backend.clear_memory().map_err(unavailable),
        BlobBackend::Disk(data_dir) => reclaim(connection, data_dir),
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
    let importing: bool = sql::scalar(
        transaction,
        "SELECT EXISTS(SELECT 1 FROM import_journal WHERE active = 1)",
        [],
    )?;
    if importing {
        return Err(CatalogError::PreconditionFailed);
    }
    let epoch: i64 = sql::scalar(
        transaction,
        "SELECT epoch FROM change_journal_state WHERE singleton = 1",
        [],
    )?;
    let next = epoch
        .checked_add(1)
        .ok_or_else(|| unavailable("reset epoch exhausted"))?;
    let (buckets, objects) = sql::row(
        transaction,
        "SELECT (SELECT count(*) FROM buckets), (SELECT count(*) FROM objects)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if matches!(mode, Reclamation::Journal) {
        sql::exec(
            transaction,
            "INSERT OR IGNORE INTO pending_blob_deletions SELECT blob_id FROM objects UNION SELECT blob_id FROM resumable_sessions UNION SELECT blob_id FROM derived_inputs UNION SELECT id FROM derived_transfers UNION SELECT blob_id FROM derived_deletions UNION SELECT blob_id FROM appendable_objects UNION SELECT blob_id FROM xml_multipart_parts UNION SELECT blob_id FROM xml_multipart_deletions",
            [],
        )?;
    } else {
        // RAM can drop its whole registry after commit; growing a disk recovery journal would block a full catalog's reset.
        sql::exec(transaction, "DELETE FROM pending_blob_deletions", [])?;
    }
    for query in CLEAR {
        sql::exec(transaction, query, [])?;
    }
    sql::exec(
        transaction,
        "UPDATE change_journal_state SET epoch = ?1, floor = sequence, enabled = 0 WHERE singleton = 1",
        [next],
    )?;
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
    "DELETE FROM soft_deleted_buckets",
    "DELETE FROM bucket_policies",
    "DELETE FROM bucket_incarnations",
    "DELETE FROM buckets",
    "UPDATE startup_seed SET complete = 0",
];

#[cfg(test)]
const RETAINED: &[&str] = &[
    "object_generation",
    "bucket_generation",
    "policy_revision",
    "catalog_identity",
    "change_journal_state",
    "notification_outbox_state",
    "pending_blob_deletions",
    "startup_seed",
];
#[cfg(test)]
const CASCADED: &[&str] = &[
    "sync_deletion_history",
    "acl_bucket_entries",
    "acl_object_entries",
    "bucket_notifications",
    "bucket_notification_ids",
];

fn reclaim(connection: &mut Connection, data_dir: &Path) -> Result<(), CatalogError> {
    loop {
        let ids = connection
            .prepare_cached(
                "SELECT blob_id FROM pending_blob_deletions ORDER BY blob_id LIMIT 128",
            )?
            .query_map([], |row| row.get::<_, crate::storage::BlobId>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if ids.is_empty() {
            return Ok(());
        }
        #[cfg(unix)]
        let mut parents = std::collections::BTreeSet::new();
        for id in &ids {
            for path in [
                id.blob_path(data_dir),
                id.staging_path(data_dir),
                data_dir.join("appendable").join(id.to_hex()),
            ] {
                #[cfg(test)]
                super::super::fault::hit(data_dir, super::super::fault::Step::ResetReclaim)
                    .map_err(unavailable)?;
                objects::unlink(&path)?;
                #[cfg(unix)]
                if let Some(parent) = path.parent()
                    && parent.exists()
                {
                    parents.insert(parent.to_owned());
                }
            }
        }
        #[cfg(unix)]
        for parent in parents {
            #[cfg(test)]
            super::super::fault::hit(data_dir, super::super::fault::Step::ResetReclaimSync)
                .map_err(unavailable)?;
            objects::sync_dir(&parent)?;
        }
        // Journal removal follows every directory sync, so a partial page remains retryable.
        let transaction = sql::immediate(connection)?;
        for id in ids {
            sql::exec(
                &transaction,
                "DELETE FROM pending_blob_deletions WHERE blob_id = ?1",
                [id],
            )?;
        }
        transaction.commit()?;
    }
}

#[cfg(test)]
mod tests;
