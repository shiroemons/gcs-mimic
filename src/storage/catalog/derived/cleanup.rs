use super::*;

pub(crate) struct DerivedRequestGuard {
    catalog: Catalog,
    id: Option<BlobId>,
}

impl DerivedRequestGuard {
    pub(crate) fn disarm(&mut self) {
        self.id = None;
    }
}

impl Drop for DerivedRequestGuard {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        let catalog = self.catalog.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            crate::task::spawn_supervised_on(&runtime, "derived-cleanup", async move {
                loop {
                    let notified = catalog.0.derived_wake.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    match catalog.derived_guard(&id) {
                        Ok(_guard) => {
                            if let Err(error) = catalog.cleanup_derived(&id).await
                                && error != ObjectError::DerivedTransferNotFound
                            {
                                deferred_cleanup(&error);
                            }
                            return;
                        }
                        Err(ObjectError::DerivedTransferBusy) => notified.await,
                        Err(error) => {
                            deferred_cleanup(&error);
                            return;
                        }
                    }
                }
            });
        } else {
            deferred_cleanup(
                &"abandoned derived transfer cleanup needs a runtime; deferred until expiry",
            );
        }
    }
}

impl Catalog {
    pub(crate) fn guard_derived_request(&self, id: &BlobId) -> DerivedRequestGuard {
        DerivedRequestGuard {
            catalog: self.clone(),
            id: Some(*id),
        }
    }

    pub(super) async fn cleanup_derived(&self, id: &BlobId) -> Result<(), ObjectError> {
        let id = *id;
        let discarded = self
            .run_upload(move |connection| discard_transfer(connection, &id))
            .await?;
        self.release_transfer(discarded).await;
        self.reclaim_after_commit().await;
        Ok(())
    }

    pub(super) async fn release_transfer(&self, discarded: DiscardedTransfer) {
        let backend = self.0.backend.clone();
        #[cfg(test)]
        let root = self.0.data_dir.clone();
        match crate::task::run_blocking("derived-release", move || {
            #[cfg(test)]
            fault::hit(&root, Step::DerivedCleanup).map_err(object_unavailable)?;
            for pin in discarded.pins {
                if let Err(error) = backend.remove_pin(&pin) {
                    deferred_cleanup(&error);
                }
            }
            backend.remove_draft(&discarded.id)?;
            #[cfg(test)]
            fault::hit(&root, Step::DerivedCleaned).map_err(object_unavailable)?;
            Ok::<_, ObjectError>(())
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => deferred_cleanup(&error),
            Err(error) => deferred_cleanup(&error),
        }
    }

    pub(crate) async fn cancel_derived(&self, id: &BlobId) -> Result<(), ObjectError> {
        let _guard = self.derived_guard(id)?;
        self.cleanup_derived(id).await
    }

    /// The existing maintenance scheduler includes these deadlines and sleeps until work is due.
    #[cfg(test)]
    pub(crate) async fn maintain_derived_transfers(
        &self,
    ) -> Result<Option<std::time::SystemTime>, crate::bucket::CatalogError> {
        let now = UnixMillis::try_from(self.0.clock.now())?;
        self.maintain_derived_transfers_at(now).await
    }

    pub(in crate::storage::catalog) async fn maintain_derived_transfers_at(
        &self,
        now: UnixMillis,
    ) -> Result<Option<std::time::SystemTime>, crate::bucket::CatalogError> {
        let mut failure = self.reclaim_pending().await.err();
        let mut after = String::new();
        loop {
            let cursor = after.clone();
            let due = self
                .run(move |connection| {
                    connection.prepare_cached("SELECT id FROM derived_transfers WHERE deadline <= ?1 AND id > ?2 ORDER BY id LIMIT 128")?
                        .query_map(params![now, cursor], |row| row.get::<_, BlobId>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map_err(object_unavailable)
                })
                .await
                .map_err(super::super::unavailable)?;
            if due.is_empty() {
                break;
            }
            for id in due {
                after = id.to_hex();
                let _guard = match self.derived_guard(&id) {
                    Ok(guard) => guard,
                    Err(ObjectError::DerivedTransferBusy) => continue,
                    Err(error) => {
                        failure = Some(error);
                        continue;
                    }
                };
                if let Err(error) = self.cleanup_derived(&id).await
                    && error != ObjectError::DerivedTransferNotFound
                {
                    failure = Some(error);
                }
            }
        }
        if let Some(error) = failure {
            return Err(super::super::unavailable(error));
        }
        self.run(move |connection| {
            // Derived transfers wake the maintenance sweep, so these run per request.
            let deadline: Option<UnixMillis> = sql::scalar(
                connection,
                "SELECT min(deadline) FROM derived_transfers WHERE deadline > ?1",
                [now],
            )?;
            let pending: bool = sql::scalar(
                connection,
                "SELECT EXISTS(SELECT 1 FROM derived_deletions)",
                [],
            )?;
            let deadline = if pending {
                Some(deadline.map_or(now, |deadline| deadline.min(now)))
            } else {
                deadline
            };
            deadline
                .map(UnixMillis::system_time)
                .transpose()
                .map_err(super::super::unavailable)
        })
        .await
    }
}

#[must_use = "a discarded transfer must release its staged data"]
pub(super) struct DiscardedTransfer {
    pub(super) id: BlobId,
    pub(super) pins: Vec<BlobId>,
}

pub(super) fn discard_transfer(
    connection: &mut Connection,
    id: &BlobId,
) -> Result<DiscardedTransfer, ObjectError> {
    let transaction = sql::immediate(connection)?;
    let exists: bool = sql::scalar(
        &transaction,
        "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1)",
        [id],
    )?;
    if !exists {
        return Err(ObjectError::DerivedTransferNotFound);
    }
    let pins = inputs(&transaction, id)?
        .into_iter()
        .map(|source| source.id)
        .collect();
    if !objects::blob_referenced(&transaction, id)? {
        sql::exec(
            &transaction,
            "INSERT OR IGNORE INTO pending_blob_deletions VALUES (?1)",
            [id],
        )?;
    }
    sql::exec(
        &transaction,
        "DELETE FROM derived_inputs WHERE transfer_id=?1",
        [id],
    )?;
    sql::exec(
        &transaction,
        "DELETE FROM derived_transfers WHERE id=?1",
        [id],
    )?;
    #[cfg(test)]
    fault::hit(
        &fault::database_dir(&transaction),
        Step::DerivedCleanupCommit,
    )
    .map_err(object_unavailable)?;
    transaction.commit()?;
    Ok(DiscardedTransfer { id: *id, pins })
}
