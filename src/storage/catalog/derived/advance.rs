use super::*;

impl Catalog {
    #[cfg(test)]
    pub(crate) async fn get_derived(&self, id: &BlobId) -> Result<DerivedTransfer, ObjectError> {
        let id = *id;
        let now = unix_millis(self.0.clock.now())?;
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
        let permit = self.admit_object_upload()?;
        let guard = self.derived_guard(id)?;
        let catalog = self.clone();
        let id = *id;
        // Detached ownership keeps cancellation from racing directory closure or cleanup.
        crate::task::spawn_supervised("derived-transfer", async move {
            let (_permit, _guard) = (permit, guard);
            let query = id;
            let backend = catalog.0.backend.clone();
            let now = unix_millis(catalog.0.clock.now())?;
            let (mut progress, mut hasher, sources) = catalog.run_upload(move |connection| {
                let (progress, hasher) = transfer(connection, &query, now)?;
                Ok((progress, hasher, inputs(connection, &query)?))
            }).await?;
            crate::task::run_blocking("derived-restore", move || backend.restore_draft(&query)).await??;
            let mut output = transfer_io::Output::open(&catalog, &id, progress.offset).await?;
            let target = progress.offset.saturating_add(budget).min(progress.total);
            let mut skip = progress.offset;
            let mut buffer = [0_u8; BUFFER_SIZE];
            for source in sources {
                if skip >= source.length { skip -= source.length; continue; }
                if progress.offset == target { break; }
                let mut input = transfer_io::InputReader::open(&catalog, &source.id, source.start + skip).await?;
                let mut remaining = (source.length - skip).min(target - progress.offset);
                skip = 0;
                while remaining > 0 {
                    if catalog.0.admission.is_closed() { return Err(object_unavailable("catalog is closing")); }
                    let length = usize::try_from(remaining.min(BUFFER_SIZE as u64)).map_err(object_unavailable)?;
                    let bytes = buffer.get_mut(..length).ok_or_else(|| object_unavailable("invalid derived buffer length"))?;
                    input.read_exact(bytes).await?;
                    #[cfg(test)]
                    fault::hit(&catalog.0.data_dir, Step::BodyWrite).map_err(object_unavailable)?;
                    output.write(bytes).await?;
                    hasher.update(bytes);
                    progress.offset += length as u64;
                    remaining -= length as u64;
                    if catalog.0.backend.is_memory() { tokio::task::yield_now().await; }
                }
            }
            output.finish(&catalog).await?;
            catalog.run_upload(move |connection| {
                let (md5_state, crc) = hasher.md5_state();
                sql::exec(connection, "UPDATE derived_transfers SET offset = ?2, md5_state = ?3, crc32c = ?4 WHERE id = ?1", params![id, progress.offset, md5_state.as_slice(), crc])?;
                // The queued SQL job owns the draft until its acknowledged prefix is updated.
                output.acknowledge(progress.offset);
                Ok(progress)
            }).await
        }).await?
    }
}
