use super::*;

#[must_use = "reclamation must retain its leases until file and SQL work settle"]
pub(in crate::storage::catalog) struct ReclamationPermit {
    _activity: Activity,
    _slot: OwnedSemaphorePermit,
    _task: Arc<crate::task::TaskToken>,
}

// Both probes use the same source of live references, with an indexed predicate for each shape.
macro_rules! referenced_blobs {
    ($predicate:literal) => {
        concat!(
            "SELECT DISTINCT blob_id FROM objects WHERE blob_id ",
            $predicate
        )
    };
}

pub(in crate::storage::catalog) fn referenced_batch(
    connection: &Connection,
    ids: &[BlobId],
) -> rusqlite::Result<std::collections::HashSet<BlobId>> {
    connection
        .prepare_cached(referenced_blobs!("IN (SELECT value FROM json_each(?1))"))?
        .query_map([sql::Json(ids)], |row| row.get(0))?
        .collect()
}

pub(in crate::storage::catalog) fn blob_referenced(
    connection: &Connection,
    id: &BlobId,
) -> rusqlite::Result<bool> {
    sql::scalar(
        connection,
        concat!("SELECT EXISTS(", referenced_blobs!("= ?1"), ")"),
        [id],
    )
}

/// Journals `id` for deletion. Ignoring a duplicate is right here: the blob may already be
/// journaled by an earlier retirement in the same transaction or by a retried cleanup.
pub(in crate::storage::catalog) fn journal_deletion(
    connection: &sql::WriteTx<'_>,
    id: &BlobId,
) -> rusqlite::Result<()> {
    sql::exec(connection, JOURNAL_DELETION, [id])?;
    Ok(())
}

const JOURNAL_DELETION: &str = "INSERT OR IGNORE INTO pending_blob_deletions VALUES (?1)";

/// Journals a blob nothing else can have journaled; a duplicate is a catalog inconsistency.
pub(in crate::storage::catalog) fn journal_new_deletion(
    connection: &sql::WriteTx<'_>,
    id: &BlobId,
) -> rusqlite::Result<()> {
    sql::exec(connection, JOURNAL_NEW_DELETION, [id])?;
    Ok(())
}

const JOURNAL_NEW_DELETION: &str = "INSERT INTO pending_blob_deletions VALUES (?1)";

pub(super) const JOURNAL_CONFLICT: &str = "SELECT pending.blob_id FROM pending_blob_deletions AS pending JOIN objects ON objects.blob_id = pending.blob_id ORDER BY pending.blob_id LIMIT 1";

/// Journal rows one reclamation handles between its catalog transactions.
const RECLAIM_PAGE: usize = 128;

pub(super) fn live_journal_error(id: BlobId) -> ObjectError {
    object_unavailable(format!(
        "deletion journal blob {id}: deletion journal references a live blob; its file is kept"
    ))
}

/// At startup recovery, the connection is local and no API request can use it.
pub(in crate::storage::catalog) fn reclaim(
    connection: &mut Connection,
    data_dir: &Path,
    id: &BlobId,
) -> Result<(), ObjectError> {
    let referenced: bool = blob_referenced(connection, id)?;
    if referenced {
        return Err(ObjectError::Unavailable(
            "deletion journal references a live blob".into(),
        ));
    }
    let path = id.blob_path(data_dir);
    let shard = blob_shard(&path)?;
    #[cfg(test)]
    fault::hit(data_dir, Step::ReclaimUnlink).map_err(object_unavailable)?;
    unlink(&path)?;
    #[cfg(unix)]
    if shard.exists() {
        #[cfg(test)]
        fault::hit(data_dir, Step::ReclaimSync).map_err(object_unavailable)?;
        sync_dir(shard)?;
    }
    // Windows has no directory fsync; the shard is still resolved so both platforms reject
    // the same malformed blob paths.
    #[cfg(not(unix))]
    let _ = shard;
    #[cfg(test)]
    fault::hit(data_dir, Step::ReclaimForget).map_err(object_unavailable)?;
    sql::exec_on_writer(
        connection,
        "DELETE FROM pending_blob_deletions WHERE blob_id = ?1",
        [id],
    )?;
    Ok(())
}

/// Whether a transaction left blobs for reclamation.
#[must_use = "journaled blobs require reclaim_if"]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::storage::catalog) struct Journaled(bool);

impl Journaled {
    pub(in crate::storage::catalog) const NOTHING: Self = Self(false);
    pub(in crate::storage::catalog) const SOME: Self = Self(true);
    pub(in crate::storage::catalog) fn for_rows(rows: usize) -> Self {
        Self(rows != 0)
    }

    pub(in crate::storage::catalog) fn any(self) -> bool {
        self.0
    }
}

impl From<bool> for Journaled {
    fn from(pending: bool) -> Self {
        Self(pending)
    }
}

impl From<Retired> for Journaled {
    fn from(retired: Retired) -> Self {
        match retired {
            Retired::Journaled => Self::SOME,
            Retired::Nothing | Retired::SoftDeleted => Self::NOTHING,
        }
    }
}

impl std::ops::BitOr for Journaled {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for Journaled {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

/// A publication candidate whose outcome has settled before its reference is probed.
pub(in crate::storage::catalog) struct StagedBlob {
    pub(in crate::storage::catalog) id: BlobId,
    pub(in crate::storage::catalog) stage: Option<PathBuf>,
}

impl StagedBlob {
    pub(in crate::storage::catalog) fn pin(
        backend: &super::super::BlobBackend,
        root: &Path,
        id: BlobId,
    ) -> Self {
        Self {
            id,
            stage: (!backend.is_memory()).then(|| id.staging_path(root)),
        }
    }
}

impl Catalog {
    /// Cleanup cannot change a settled publication result; recovery retries remaining bytes.
    pub(in crate::storage::catalog) async fn discard_staged(&self, blob: StagedBlob) {
        if let Err(error) = self.try_discard_staged(blob).await {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                crate::diagnostics::Event::UploadCleanupDeferred,
                serde_json::json!({"message": format!("deferred upload cleanup: {error}")}),
            );
        }
    }

    async fn try_discard_staged(&self, blob: StagedBlob) -> Result<(), ObjectError> {
        let StagedBlob { id, stage } = blob;
        if let Some(stage) = stage {
            crate::task::run_blocking("upload-discard", move || unlink(&stage)).await??;
        }
        #[cfg(test)]
        fault::hit(&self.0.storage.data_dir, Step::DiscardProbe).map_err(object_unavailable)?;
        // Probe the committed reference before cleanup: SQL commit errors can be ambiguous.
        let journaled = self
            .run_upload::<_, ObjectError>(move |connection| {
                if blob_referenced(connection, &id)? {
                    return Ok(Journaled::NOTHING);
                }
                sql::exec_on_writer(connection, JOURNAL_DELETION, [&id])?;
                Ok(Journaled::SOME)
            })
            .await?;
        self.reclaim_if(journaled).await;
        Ok(())
    }
}

impl Catalog {
    pub(in crate::storage::catalog) async fn reclaim_if(&self, journaled: Journaled) {
        if !journaled.any() {
            return;
        }
        match self.reclaim_pending().await {
            Ok(RECLAIM_PAGE) => {}
            Ok(_) => return,
            Err(error) if self.shutdown_interrupted(&error) => return,
            Err(error) => crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                crate::diagnostics::Event::BlobReclaimDeferred,
                serde_json::json!({"message":format!("deferred blob reclamation: {error}")}),
            ),
        }
        // A full page can leave work behind even if no later request journals a blob.
        self.0.maintenance.wake.notify_one();
    }

    /// Reclaims the whole journal, one bounded page at a time, for the maintenance scheduler.
    ///
    /// A request reclaims one page, enough for what it journaled, and leaves anything more behind
    /// when a sweep or a bucket deletion journaled a larger batch or an earlier reclamation
    /// failed. A failure here fails the pass, so the scheduler retries it and readiness reports
    /// a journal that cannot drain.
    pub(in crate::storage::catalog) async fn maintain_deletion_journal(
        &self,
    ) -> Result<Option<std::time::SystemTime>, CatalogError> {
        while self.reclaim_pending().await.map_err(unavailable)? == RECLAIM_PAGE {}
        Ok(None)
    }

    pub(in crate::storage::catalog) async fn reclamation_permit(
        &self,
    ) -> Result<ReclamationPermit, ObjectError> {
        // An upload reclaims under its own lease after it commits, so this must not line up behind
        // an exclusive job that is waiting for that upload to finish.
        let activity = self.await_activity(Work::UnderWay).await?;
        let task = self.0.shutdown.tasks.track_cleanup().ok_or_else(|| {
            #[cfg(test)]
            self.0
                .storage
                .refused_reclamations
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            object_unavailable(crate::cause::BackendFailure::CatalogClosed)
        })?;
        let permit = self
            .0
            .storage
            .reclamation
            .clone()
            .acquire_owned()
            .await
            .map_err(object_unavailable)?;
        Ok(ReclamationPermit {
            _activity: activity,
            _slot: permit,
            _task: task,
        })
    }

    /// Reclaims up to `RECLAIM_PAGE` journaled blobs and returns how many it selected.
    pub(in crate::storage::catalog) async fn reclaim_pending(&self) -> Result<usize, ObjectError> {
        let permit = self.reclamation_permit().await?;
        let catalog = self.clone();
        // Detached callers retain the cleanup slot until filesystem work and SQL settle.
        #[cfg(test)]
        self.count_cleanup_task();
        let task = crate::task::spawn_supervised("blob-reclamation", async move {
            let _permit = permit;
            let (ids, conflict) = catalog
                .run_upload::<_, ObjectError>(|connection| {
                    let ids = connection
                        .prepare_cached(
                            // Cancelled sessions need their marker until staging cleanup also succeeds.
                            "SELECT blob_id FROM pending_blob_deletions AS pending WHERE NOT EXISTS (SELECT 1 FROM resumable_sessions AS session WHERE session.blob_id = pending.blob_id AND session.state != 'COMPLETE') AND NOT EXISTS (SELECT 1 FROM objects WHERE objects.blob_id = pending.blob_id) ORDER BY blob_id LIMIT ?1",
                        )?
                        .query_map([RECLAIM_PAGE], |row| row.get::<_, BlobId>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let conflict: Option<BlobId> = sql::optional_scalar(connection, JOURNAL_CONFLICT, [])?;
                    Ok((ids, conflict))
                })
                .await?;
            if ids.is_empty() {
                return conflict.map_or(Ok(0), |id| Err(live_journal_error(id)));
            }
            let selected = ids.len();
            let backend = catalog.0.storage.backend.clone();
            let (removed, failure) =
                crate::task::run_blocking("blob-reclamation", move || backend.reclaim_batch(ids))
                    .await?
                    .map_err(object_unavailable)?;
            catalog
                .run_upload(move |connection| {
                    let transaction = sql::deferred_write(connection)?;
                    for id in removed {
                        sql::exec(
                            &transaction,
                            "DELETE FROM pending_blob_deletions WHERE blob_id = ?1 AND NOT EXISTS (SELECT 1 FROM objects WHERE blob_id = ?1)",
                            [&id],
                        )?;
                        sql::exec(
                            &transaction,
                            "DELETE FROM derived_deletions WHERE blob_id = ?1 AND NOT EXISTS (SELECT 1 FROM objects WHERE blob_id = ?1)",
                            [&id],
                        )?;
                    }
                    #[cfg(test)]
                    fault::hit(&fault::database_dir(&transaction), Step::ReclaimForget).map_err(object_unavailable)?;
                    transaction.commit().map_err(object_unavailable)
                })
                .await?;
            failure.map_or_else(
                || conflict.map_or(Ok(selected), |id| Err(live_journal_error(id))),
                Err,
            )
        });
        task.await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_fixtures::{body, bucket, permissive_upload};

    #[tokio::test]
    async fn discard_succeeds_while_an_unrelated_live_journal_entry_remains() {
        let data = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        catalog
            .put_object(
                permissive_upload("bucket", "live", 1000),
                body(vec![Bytes::from_static(b"live")]),
            )
            .await
            .unwrap();
        let live = catalog
            .run_upload::<_, ObjectError>(|connection| {
                let id = lookup(connection, "bucket", "live")?.unwrap().blob;
                sql::exec_on_writer(connection, JOURNAL_NEW_DELETION, [&id])?;
                Ok(id)
            })
            .await
            .unwrap();
        let id = BlobId::generate().unwrap();
        let path = id.blob_path(&data.0);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"orphan").unwrap();
        catalog
            .try_discard_staged(StagedBlob { id, stage: None })
            .await
            .unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read(live.blob_path(&data.0)).unwrap(), b"live");
        assert!(
            catalog
                .reclaim_pending()
                .await
                .unwrap_err()
                .to_string()
                .contains(&live.to_hex())
        );
        catalog.close().await.unwrap();
    }
}
