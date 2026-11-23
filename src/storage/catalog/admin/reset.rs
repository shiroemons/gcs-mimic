//! Instance-local reset owns SQL execution and the gate that unpublished byte leases hold.

use super::super::{BlobBackend, objects};
use super::{Admin, AdminError};
use crate::storage::catalog::sql;
use crate::{bucket::CatalogError, storage::failure::unavailable};
use rusqlite::Connection;
use std::path::Path;

#[cfg(test)]
const QUIESCENCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

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
        let admission = self
            .catalog
            .wait_admin(self.catalog.0.admin_reads.clone(), deadline)
            .await?;
        let exclusive = self.exclusive_past_seed_commits(deadline).await?;
        let inner = self.catalog.0.clone();
        self.catalog
            .0
            .executor
            .run(move || {
                // Cancellation cannot release admission while the owned transaction/cleanup runs.
                let (_admission, _exclusive) = (admission, exclusive);
                let result = inner.with_connection(|connection| reset(connection, &inner.backend));
                inner.maintenance_wake.notify_one();
                match result {
                    Ok(result) => result.map_err(AdminError::from),
                    Err(error) => Err(error.into()),
                }
            })
            .await
            .map_err(AdminError::backend)?
    }

    /// Exclusive ownership with no seed commit under way. A seed batch lets go of its lease for its
    /// commit while its bodies already sit in the RAM store, so clearing the store then would leave
    /// rows without bytes. Such a commit needs exclusive ownership to finish, so this gives it up
    /// before waiting, and holding nothing while it waits it can never wait for itself. A batch
    /// that has not reached its commit still holds its lease and so is drained as usual.
    async fn exclusive_past_seed_commits(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<crate::storage::catalog::ExclusiveGuard, AdminError> {
        let seed_commit = &self.catalog.0.seed_commit;
        loop {
            let exclusive = self.catalog.wait_exclusive(deadline).await?;
            if seed_commit.available_permits() > 0 {
                return Ok(exclusive);
            }
            drop(exclusive);
            drop(
                self.catalog
                    .wait_admin(seed_commit.clone(), deadline)
                    .await?,
            );
        }
    }
}

fn reset(connection: &mut Connection, backend: &BlobBackend) -> Result<ResetOutcome, CatalogError> {
    let transaction = sql::immediate(connection)?;
    let cleared = clear_in_transaction(&transaction, Reclamation::for_backend(backend))?;
    if let Err(error) = objects::commit_publication(transaction) {
        // A commit can succeed before reporting an I/O error. The durable epoch resolves it.
        let committed: i64 = sql::scalar(connection, EPOCH, [])?;
        if committed != cleared.epoch {
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
        buckets: cleared.buckets,
        objects: cleared.objects,
        cleanup_pending,
    })
}

#[derive(Clone, Copy)]
pub(super) enum Reclamation {
    Journal,
    ClearMemory,
}

impl Reclamation {
    /// RAM drops its whole registry after commit; disk journals every blob for crash-safe
    /// reclamation.
    pub(super) fn for_backend(backend: &BlobBackend) -> Self {
        match backend {
            BlobBackend::Memory(_) => Self::ClearMemory,
            BlobBackend::Disk(_) => Self::Journal,
        }
    }
}

/// The count of resets and replacing reseeds committed, which each raises by one in its
/// transaction.
const EPOCH: &str = "SELECT epoch FROM reset_epoch WHERE singleton = 1";

/// Checks that the catalog holds the one reset epoch row that reset and reseed read. Schema 58
/// creates the table and moves the epoch into it.
pub(in crate::storage::catalog) fn validate_reset_epoch(
    connection: &Connection,
) -> Result<(), CatalogError> {
    sql::validate(
        connection,
        "SELECT count(*) = 1 AND min(singleton) = 1 FROM reset_epoch",
        "invalid reset epoch state",
    )
}

/// What a reset or replacing reseed removed, and the reset epoch it committed.
pub(super) struct Cleared {
    pub(super) buckets: i64,
    pub(super) objects: i64,
    pub(super) epoch: i64,
}

pub(super) fn clear_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    mode: Reclamation,
) -> Result<Cleared, CatalogError> {
    let importing: bool = sql::scalar(
        transaction,
        "SELECT EXISTS(SELECT 1 FROM import_journal WHERE active = 1)",
        [],
    )?;
    if importing {
        return Err(CatalogError::PreconditionFailed);
    }
    let epoch: i64 = sql::scalar(transaction, EPOCH, [])?;
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
        "UPDATE reset_epoch SET epoch = ?1 WHERE singleton = 1",
        [next],
    )?;
    Ok(Cleared {
        buckets,
        objects,
        epoch: next,
    })
}

// High-water allocators, the reset epoch, catalog identity, the blob deletion journal and the
// seed bucket marks survive reset. The seed records go: none may claim an object the reset
// removed.
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
    "DELETE FROM import_observations",
    "DELETE FROM import_journal",
    "DELETE FROM hmac_keys",
    "DELETE FROM seed_objects",
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
    "notification_outbox_state",
    "pending_blob_deletions",
    "reset_epoch",
    // The marks keep a reset instance empty while the server runs: a pass creates a bucket only
    // for a directory seeding never took.
    "seed_buckets",
    "startup_seed",
];
#[cfg(test)]
const CASCADED: &[&str] = &[
    "acl_bucket_entries",
    "acl_object_entries",
    "bucket_notifications",
    "bucket_notification_ids",
    "bucket_file_entries",
];

/// Removes each journaled ID's blob, staging and appendable files, then forgets it. Only a job
/// that holds exclusive ownership past the commit that journaled them may call this: it does
/// not check references, because nothing can have reused the IDs since.
pub(super) fn reclaim(connection: &mut Connection, data_dir: &Path) -> Result<(), CatalogError> {
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
