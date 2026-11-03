use super::*;

pub(in crate::storage::catalog) fn blob_referenced(
    connection: &Connection,
    id: &BlobId,
) -> rusqlite::Result<bool> {
    sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
        [id],
    )
}

pub(super) const JOURNAL_CONFLICT: &str = "SELECT pending.blob_id FROM pending_blob_deletions AS pending JOIN objects ON objects.blob_id = pending.blob_id ORDER BY pending.blob_id LIMIT 1";

pub(super) fn live_journal_error(id: BlobId) -> ObjectError {
    object_unavailable(format!(
        "deletion journal blob {id}: deletion journal references a live blob; its file is kept"
    ))
}

pub(in crate::storage::catalog) fn reclaim(
    connection: &Connection,
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
    sql::exec(
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
    pub(in crate::storage::catalog) fn any(self) -> bool {
        self.0
    }
}

impl From<Option<BlobId>> for Journaled {
    fn from(value: Option<BlobId>) -> Self {
        Self(value.is_some())
    }
}

impl std::ops::BitOr for Journaled {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
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
                "upload_cleanup_deferred",
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
        fault::hit(&self.0.data_dir, Step::DiscardProbe).map_err(object_unavailable)?;
        // Probe the committed reference before cleanup: SQL commit errors can be ambiguous.
        let journaled = self
            .run_upload(move |connection| {
                if blob_referenced(connection, &id)? {
                    return Ok(Journaled::NOTHING);
                }
                sql::exec(
                    connection,
                    "INSERT OR IGNORE INTO pending_blob_deletions VALUES (?1)",
                    [id],
                )?;
                Ok(Journaled::SOME)
            })
            .await?;
        if journaled.any() {
            self.reclaim_after_commit().await;
        }
        Ok(())
    }
}

impl Catalog {
    pub(in crate::storage::catalog) async fn reclaim_if(&self, journaled: Journaled) {
        if journaled.any() {
            self.reclaim_after_commit().await;
        }
    }

    pub(in crate::storage::catalog) async fn reclaim_after_commit(&self) {
        if let Err(error) = self.reclaim_pending().await {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "blob_reclaim_deferred",
                serde_json::json!({"message":format!("deferred blob reclamation: {error}")}),
            );
        }
    }

    pub(in crate::storage::catalog) async fn reclaim_pending(&self) -> Result<(), ObjectError> {
        let activity = self.activity()?;
        let permit = self
            .0
            .reclamation
            .clone()
            .acquire_owned()
            .await
            .map_err(object_unavailable)?;
        let catalog = self.clone();
        // Detached callers retain the cleanup slot until filesystem work and SQL settle.
        let task = crate::task::spawn_supervised("blob-reclamation", async move {
            let (_permit, _activity) = (permit, activity);
            let (ids, conflict) = catalog
                .run_upload(|connection| {
                    let ids = connection
                        .prepare_cached(
                            // Cancelled sessions need their marker until staging cleanup also succeeds.
                            "SELECT blob_id FROM pending_blob_deletions AS pending WHERE NOT EXISTS (SELECT 1 FROM resumable_sessions AS session WHERE session.blob_id = pending.blob_id AND session.state != 'COMPLETE') AND NOT EXISTS (SELECT 1 FROM objects WHERE objects.blob_id = pending.blob_id) ORDER BY blob_id LIMIT 128",
                        )?
                        .query_map([], |row| row.get::<_, BlobId>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let conflict: Option<BlobId> = sql::optional_scalar(connection, JOURNAL_CONFLICT, [])?;
                    Ok((ids, conflict))
                })
                .await?;
            if ids.is_empty() {
                return conflict.map_or(Ok(()), |id| Err(live_journal_error(id)));
            }
            let backend = catalog.0.backend.clone();
            let (removed, failure) =
                crate::task::run_blocking("blob-reclamation", move || backend.reclaim_batch(ids))
                    .await?
                    .map_err(object_unavailable)?;
            catalog
                .run_upload(move |connection| {
                    let transaction = connection.transaction()?;
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
                || conflict.map_or(Ok(()), |id| Err(live_journal_error(id))),
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
            .run_upload(|connection| {
                let id = lookup(connection, "bucket", "live")?.unwrap().blob;
                sql::exec(
                    connection,
                    "INSERT INTO pending_blob_deletions VALUES (?1)",
                    [id],
                )?;
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
