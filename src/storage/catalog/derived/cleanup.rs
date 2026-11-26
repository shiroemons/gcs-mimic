use super::super::resumable_maintenance::SweepOutcome;
use super::*;

/// Names the expired-transfer sweep in the maintenance log.
const DERIVED_SWEEP: &str = "derived transfer";

#[must_use = "dropping an armed guard schedules transfer cleanup"]
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
        let (discarded, journaled) = self
            .run_upload(move |connection| discard_transfer(connection, &id))
            .await?;
        self.release_transfer(discarded).await;
        self.reclaim_if(journaled).await;
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

    pub(in crate::storage::catalog) async fn expire_derived(
        &self,
        id: &BlobId,
    ) -> Result<(), ObjectError> {
        let _guard = self
            .claim_sweep_writer(WriterKey::Derived(*id))
            .await
            .map_err(derived_claim_error)?;
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
        let mut sweep = SweepOutcome::default();
        let mut after = After::Start;
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
                after = After::Key(id);
                match self.expire_derived(&id).await {
                    Ok(())
                    | Err(
                        ObjectError::DerivedTransferNotFound | ObjectError::DerivedTransferBusy,
                    ) => {}
                    Err(error) => sweep.fail(DERIVED_SWEEP, super::super::unavailable(error)),
                }
            }
        }
        let next = self.next_derived_deadline(now).await;
        sweep.record(DERIVED_SWEEP, next);
        sweep.finish()
    }

    async fn next_derived_deadline(
        &self,
        now: UnixMillis,
    ) -> Result<Option<std::time::SystemTime>, crate::bucket::CatalogError> {
        self.run(move |connection| {
            // Derived transfers wake the maintenance sweep, so these run per request.
            let deadline: Option<UnixMillis> = sql::scalar(
                connection,
                "SELECT min(deadline) FROM derived_transfers WHERE deadline > ?1",
                [now],
            )?;
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
) -> Result<(DiscardedTransfer, objects::Journaled), ObjectError> {
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
    let journaled = if objects::blob_referenced(&transaction, id)? {
        objects::Journaled::NOTHING
    } else {
        objects::journal_deletion(&transaction, id)?;
        objects::Journaled::SOME
    };
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
    Ok((DiscardedTransfer { id: *id, pins }, journaled))
}
