use super::super::transfer_io::InputReader;
use super::*;

impl RequestTransfer {
    pub(crate) async fn advance(self, catalog: &Catalog, budget: u64) -> Result<Self, ObjectError> {
        let (progress, hasher) = catalog
            .advance_from(Checkpoint::Request(self), budget)
            .await?;
        Ok(Self { progress, hasher })
    }
}

impl Catalog {
    #[cfg(test)]
    pub(crate) async fn get_derived(&self, id: &BlobId) -> Result<DerivedTransfer, ObjectError> {
        let id = *id;
        let now = UnixMillis::try_from(self.0.policy.clock.now())?;
        self.run(move |connection| transfer(connection, &id, now).map(|value| value.0))
            .await
    }

    /// Every durable offset includes the matching incremental hash state. File fsync precedes
    /// SQLite commit; an interrupted tail is discarded before retry, never counted twice.
    pub(crate) async fn advance_derived(
        &self,
        id: &BlobId,
        budget: u64,
    ) -> Result<DerivedTransfer, ObjectError> {
        self.advance_from(Checkpoint::Stored(*id), budget)
            .await
            .map(|(progress, _)| progress)
    }

    async fn advance_from(
        &self,
        checkpoint: Checkpoint,
        budget: u64,
    ) -> Result<(DerivedTransfer, Hasher), ObjectError> {
        let id = checkpoint.id();
        let permit = self.continue_object_upload().await?;
        let guard = self.derived_guard(&id)?;
        let catalog = self.clone();
        // Detached ownership keeps cancellation from racing directory closure or cleanup.
        crate::task::spawn_supervised("derived-transfer", async move {
            let (_permit, guard) = (permit, std::sync::Arc::new(guard));
            let query = id;
            let now = UnixMillis::try_from(catalog.0.policy.clock.now())?;
            let (mut progress, mut hasher, sources) = catalog.run_upload::<_, ObjectError>(move |connection| {
                let (progress, hasher) = checkpoint.read(connection, now)?;
                Ok((progress, hasher, inputs(connection, &query)?))
            }).await?;
            let worker = catalog.clone();
            let writer = guard.clone();
            let (progress, hasher, mut output) = crate::task::run_blocking("derived-advance", move || {
                let backend = &worker.0.storage.backend;
                backend.restore_draft(&id)?;
                let mut output = backend.open_copy_output(&id, progress.offset, writer)?;
                let target = progress.offset.saturating_add(budget).min(progress.total);
                objects::with_copy_buffer(|buffer| {
                    let mut skip = progress.offset;
                    for source in sources {
                        if skip >= source.length {
                            skip -= source.length;
                            continue;
                        }
                        if progress.offset == target {
                            break;
                        }
                        let mut input = backend.open_input(&source.id, source.start + skip)?;
                        let count = (source.length - skip).min(target - progress.offset);
                        skip = 0;
                        worker.copy_range(&mut input, count, buffer, |bytes| {
                            hasher.update(bytes);
                            output.write(bytes)
                        }, InputReader::short_derived_input)?;
                        progress.offset += count;
                    }
                    Ok::<_, ObjectError>(())
                })?;
                output.make_durable()?;
                Ok::<_, ObjectError>((progress, hasher, output))
            }).await??;
            // An ephemeral lifetime only reclaims an abandoned transfer, so progress renews it: a
            // request may copy for longer, and has a deadline of its own if configured. The time
            // is read after the copy, because a compose advances through all its bytes at once.
            let renewed = UnixMillis::try_from(catalog.0.policy.clock.now())?.saturating_add_millis(EPHEMERAL_LIFETIME_MILLIS);
            catalog.run_upload(move |connection| {
                let (md5_state, crc) = hasher.md5_state();
                sql::exec_on_writer(connection, "UPDATE derived_transfers SET offset = ?2, md5_state = ?3, crc32c = ?4, deadline = CASE ephemeral WHEN 1 THEN max(deadline, ?5) ELSE deadline END WHERE id = ?1", params![id, progress.offset, md5_state.as_slice(), crc, renewed])?;
                // The queued SQL job owns the draft until its acknowledged prefix is updated.
                output.acknowledge(progress.offset);
                Ok((progress, hasher))
            }).await
        }).await?
    }
}
