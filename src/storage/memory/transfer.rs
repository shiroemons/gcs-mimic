use super::*;

impl Store {
    pub(in crate::storage) fn pin(&self, original: &str, id: String) -> io::Result<()> {
        let mut blobs = self.blobs.lock().map_err(|_| poisoned())?;
        let original = blobs
            .get(original)
            .ok_or_else(|| io::Error::other("missing RAM source"))?;
        let blob = self.snapshot(&original.chunks, original.size)?;
        if blobs.contains_key(&id) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        blobs.try_reserve(1).map_err(io::Error::other)?;
        blobs.insert(id, blob);
        Ok(())
    }

    fn snapshot(&self, chunks: &[Bytes], size: u64) -> io::Result<Arc<Blob>> {
        let mut reservation = Reservation {
            budget: self.budget.clone(),
            charge: 0,
        };
        let charge = |capacity: usize| {
            (capacity as u64)
                .checked_mul(std::mem::size_of::<Bytes>() as u64)
                .and_then(|bytes| bytes.checked_add(CHUNK_OVERHEAD))
                .ok_or_else(exhausted)
        };
        reservation.resize(charge(chunks.len())?)?;
        let mut copy = Vec::new();
        copy.try_reserve_exact(chunks.len())
            .map_err(io::Error::other)?;
        reservation.resize(charge(copy.capacity())?)?;
        copy.extend_from_slice(chunks);
        Ok(Arc::new(Blob {
            chunks: copy,
            size,
            _reservation: reservation,
        }))
    }

    pub(in crate::storage) fn create_draft(self: &Arc<Self>, id: String) -> io::Result<()> {
        self.create_draft_state(id, None)
    }

    pub(in crate::storage) fn create_resumable_draft(
        self: &Arc<Self>,
        id: String,
    ) -> io::Result<()> {
        self.create_draft_state(id, Some(crate::hashing::Hasher::default()))
    }

    fn create_draft_state(
        self: &Arc<Self>,
        id: String,
        hasher: Option<crate::hashing::Hasher>,
    ) -> io::Result<()> {
        let mut stage = self.stage()?;
        if hasher.is_some() {
            stage.fixed_charge += std::mem::size_of::<crate::hashing::Hasher>() as u64;
            stage.reservation.resize(stage.fixed_charge)?;
        }
        stage.hasher = hasher;
        let mut drafts = self.drafts.lock().map_err(|_| poisoned())?;
        if drafts.contains_key(&id) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        drafts.try_reserve(1).map_err(io::Error::other)?;
        drafts.insert(id, Box::new(stage));
        Ok(())
    }

    pub(in crate::storage) fn draft(self: &Arc<Self>, id: &str, offset: u64) -> io::Result<Draft> {
        let stage = self
            .drafts
            .lock()
            .map_err(|_| poisoned())?
            .remove(id)
            .ok_or_else(|| io::Error::other("missing RAM draft"))?;
        let mut draft = Draft {
            store: self.clone(),
            id: id.into(),
            acknowledged_hash: stage.hasher.clone(),
            stage: Some(stage),
            acknowledged: offset,
        };
        draft
            .stage
            .as_mut()
            .ok_or_else(poisoned)?
            .truncate(offset)?;
        Ok(draft)
    }

    pub(in crate::storage) fn remove_draft(&self, id: &str) -> io::Result<()> {
        self.drafts.lock().map_err(|_| poisoned())?.remove(id);
        Ok(())
    }

    pub(in crate::storage) fn publish_draft(&self, id: &str) -> io::Result<()> {
        let mut drafts = self.drafts.lock().map_err(|_| poisoned())?;
        let stage = drafts
            .get_mut(id)
            .ok_or_else(|| io::Error::other("missing RAM draft"))?;
        stage.flush_pending()?;
        let mut blobs = self.blobs.lock().map_err(|_| poisoned())?;
        // A publication retry reuses the immutable candidate after a rolled-back SQL transaction.
        if !blobs.contains_key(id) {
            let blob = self.snapshot(&stage.chunks, stage.size)?;
            blobs.try_reserve(1).map_err(io::Error::other)?;
            blobs.insert(id.into(), blob);
        }
        Ok(())
    }

    pub(in crate::storage) fn checkpoint_draft(
        self: &Arc<Self>,
        id: &str,
    ) -> io::Result<Checkpoint> {
        let mut drafts = self.drafts.lock().map_err(|_| poisoned())?;
        let stage = drafts
            .get_mut(id)
            .ok_or_else(|| io::Error::other("missing RAM draft"))?;
        stage.flush_pending()?;
        let blob = self.snapshot(&stage.chunks, stage.size)?;
        let mut blobs = self.blobs.lock().map_err(|_| poisoned())?;
        blobs.try_reserve(1).map_err(io::Error::other)?;
        // Existing readers retain the previous snapshot and its leases until their last clone.
        let previous = blobs.insert(id.into(), blob.clone());
        Ok(Checkpoint {
            store: self.clone(),
            id: id.into(),
            previous,
            installed: Some(blob),
            owns_draft: false,
        })
    }

    pub(in crate::storage) fn create_draft_checkpoint(
        self: &Arc<Self>,
        id: &str,
    ) -> io::Result<Checkpoint> {
        self.create_draft(id.into())?;
        match self.checkpoint_draft(id) {
            Ok(mut checkpoint) => {
                checkpoint.owns_draft = true;
                Ok(checkpoint)
            }
            Err(error) => {
                self.remove_draft(id)?;
                Err(error)
            }
        }
    }
}

pub(in crate::storage) struct Checkpoint {
    store: Arc<Store>,
    id: String,
    previous: Option<Arc<Blob>>,
    installed: Option<Arc<Blob>>,
    owns_draft: bool,
}

impl Checkpoint {
    pub(in crate::storage) fn commit(mut self) {
        self.installed = None;
        self.owns_draft = false;
    }
}

impl Drop for Checkpoint {
    fn drop(&mut self) {
        if self.owns_draft
            && let Err(error) = self.store.remove_draft(&self.id)
        {
            self.store.failed.store(true, Ordering::Release);
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Error,
                "ram_draft_cleanup_failed",
                serde_json::json!({"message": error.to_string()}),
            );
        }
        let Some(installed) = self.installed.take() else {
            return;
        };
        match self.store.blobs.lock() {
            Ok(mut blobs) => {
                if blobs
                    .get(&self.id)
                    .is_some_and(|blob| Arc::ptr_eq(blob, &installed))
                {
                    match self.previous.take() {
                        Some(previous) => {
                            blobs.insert(self.id.clone(), previous);
                        }
                        None => {
                            blobs.remove(&self.id);
                        }
                    }
                }
            }
            Err(error) => {
                self.store.failed.store(true, Ordering::Release);
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Error,
                    "ram_checkpoint_cleanup_failed",
                    serde_json::json!({"message": error.to_string()}),
                );
            }
        }
    }
}

pub(in crate::storage) struct Draft {
    store: Arc<Store>,
    id: String,
    stage: Option<Box<Stage>>,
    acknowledged: u64,
    acknowledged_hash: Option<crate::hashing::Hasher>,
}

impl Draft {
    pub(in crate::storage) fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let stage = self.stage.as_mut().ok_or_else(poisoned)?;
        stage.write(bytes)?;
        if let Some(hasher) = &mut stage.hasher {
            hasher.update(bytes);
        }
        Ok(())
    }

    pub(in crate::storage) fn acknowledge(&mut self, offset: u64) {
        self.acknowledged = offset;
        self.acknowledged_hash = self.stage.as_ref().and_then(|stage| stage.hasher.clone());
    }

    pub(in crate::storage) fn hashes(&self) -> io::Result<(crate::hashing::Hashes, u64)> {
        let stage = self.stage.as_ref().ok_or_else(poisoned)?;
        let hasher = stage
            .hasher
            .clone()
            .ok_or_else(|| io::Error::other("missing RAM upload hash checkpoint"))?;
        Ok((hasher.finish(), stage.size))
    }
}

impl Drop for Draft {
    fn drop(&mut self) {
        let Some(mut stage) = self.stage.take() else {
            return;
        };
        stage.hasher = self.acknowledged_hash.take();
        let result = stage.truncate(self.acknowledged).and_then(|()| {
            self.store
                .drafts
                .lock()
                .map_err(|_| poisoned())?
                .insert(self.id.clone(), stage);
            Ok(())
        });
        if let Err(error) = result {
            self.store.failed.store(true, Ordering::Release);
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Error,
                "ram_draft_cleanup_failed",
                serde_json::json!({"message": error.to_string()}),
            );
        }
    }
}

impl Stage {
    fn truncate(&mut self, offset: u64) -> io::Result<()> {
        if offset > self.size {
            return Err(io::Error::other(
                "RAM draft is shorter than its acknowledged offset",
            ));
        }
        let mut remaining = offset;
        let mut keep = 0;
        for chunk in &mut self.chunks {
            if remaining == 0 {
                break;
            }
            let length = remaining.min(chunk.len() as u64) as usize;
            if length < chunk.len() {
                *chunk = chunk.slice(..length);
            }
            remaining -= length as u64;
            keep += 1;
        }
        self.chunks.truncate(keep);
        if remaining == 0 {
            self.pending = None;
        } else if let Some(pending) = &mut self.pending {
            pending.bytes.truncate(remaining as usize);
        } else {
            return Err(io::Error::other("missing acknowledged RAM draft bytes"));
        }
        self.size = offset;
        Ok(())
    }
}

fn poisoned() -> io::Error {
    io::Error::other("RAM transfer store lock poisoned or state unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_draft_rollback_and_source_aliases_share_the_aggregate_budget() {
        let store = Store::new(3 * BLOCK_SIZE + 4096);
        let mut stage = store.stage().unwrap();
        stage.write(&vec![7; BLOCK_SIZE as usize]).unwrap();
        stage.publish("source".into()).unwrap();
        store.pin("source", "pin".into()).unwrap();
        store.create_draft("draft".into()).unwrap();
        let baseline = store.budget.used.load(Ordering::Acquire);
        let mut draft = store.draft("draft", 0).unwrap();
        draft.write(&[3; 1024]).unwrap();
        draft.acknowledge(1024);
        drop(draft);
        let mut draft = store.draft("draft", 1024).unwrap();
        assert!(draft.write(&vec![4; 3 * BLOCK_SIZE as usize]).is_err());
        drop(draft);
        let stage = store.drafts.lock().unwrap();
        assert_eq!(stage["draft"].size, 1024);
        drop(stage);
        store.publish_draft("draft").unwrap();
        let mut reader = store.open("draft").unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, vec![3; 1024]);
        store.remove("source").unwrap();
        let pinned = store.open("pin").unwrap();
        store.remove("pin").unwrap();
        store.remove("draft").unwrap();
        store.remove_draft("draft").unwrap();
        assert!(store.budget.used.load(Ordering::Acquire) > 0);
        drop(reader);
        drop(pinned);
        assert_eq!(store.budget.used.load(Ordering::Acquire), 0);
        assert!(baseline < BLOCK_SIZE + 1024);
    }

    #[test]
    fn ram_draft_registry_does_not_keep_the_store_alive() {
        let store = Store::new(1024);
        store.create_draft("draft".into()).unwrap();
        let weak = Arc::downgrade(&store);
        drop(store);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn ram_draft_rollback_keeps_retained_chunk_table_capacity_charged() {
        let store = Store::new(8 * 1024 * 1024);
        store.create_draft("draft".into()).unwrap();
        let mut draft = store.draft("draft", 0).unwrap();
        draft.write(&vec![1; 64 * BLOCK_SIZE as usize]).unwrap();
        draft.acknowledge(1);
        drop(draft);
        let drafts = store.drafts.lock().unwrap();
        let stage = &drafts["draft"];
        assert_eq!(stage.size, 1);
        assert_eq!(stage.chunks.len(), 1);
        let descriptors = stage.chunks.capacity() as u64 * std::mem::size_of::<Bytes>() as u64;
        assert!(descriptors > CHUNK_OVERHEAD * 2);
        assert_eq!(stage.reservation.charge, descriptors);
        drop(drafts);
        store.remove_draft("draft").unwrap();
        assert_eq!(store.budget.used.load(Ordering::Acquire), 0);
    }
}
