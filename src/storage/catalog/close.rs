use super::*;

impl Catalog {
    /// Stops admission on all handles and waits for blocking work before releasing ownership.
    /// Remaining handles become unavailable; repeated closes succeed without touching a new owner.
    /// The runtime used at open must stay driven through this drain: it owns the blocking
    /// descriptor closes and their supervisors. Runtime shutdown cancels queued work and
    /// drops its owned files, but cannot guarantee completion of an orderly catalog close.
    pub async fn close(&self) -> Result<(), CatalogError> {
        self.0.shutdown.token.cancel();
        self.0.admission.slots.close();
        self.0.shutdown.tasks.drain().await;
        if let Some(pool) = &self.0.reads.pool {
            pool.close().await;
        }
        let execution = self
            .0
            .writer
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        // Once every cache producer has stopped, release the files before directory ownership.
        self.0.reads.invalidate();
        self.0.reads.cache.drain().await?;
        let inner = self.0.clone();
        let closed = self
            .0
            .writer
            .executor
            .run_last(move || {
                let _execution = execution;
                // Poison must not prevent shutdown from closing the connection and its lock.
                let mut resources = crate::sync::lock_for_cleanup(&inner.writer.resources);
                if let Some(resources) = resources.take() {
                    resources.close(&inner.storage.backend)?;
                }
                Ok(())
            })
            .await;
        #[cfg(test)]
        fault::assert_no_catalog_thread_io(&self.0.storage.data_dir);
        #[cfg(test)]
        assert_eq!(
            self.readonly_answers(),
            0,
            "pooled reads returned SQLITE_READONLY"
        );
        match closed {
            Ok(result) => result,
            // An earlier close already released the connection and stopped the thread.
            Err(executor::ExecutorError::Closed) => Ok(()),
            Err(error) => Err(unavailable(error)),
        }
    }
}

impl Resources {
    pub(super) fn close(self, backend: &BlobBackend) -> Result<(), CatalogError> {
        self.connection
            .close()
            .map_err(|(_, error)| unavailable(error))?;
        #[cfg(test)]
        if let BlobBackend::Disk(path) = backend {
            fault::hit_io(path, fault::Step::CatalogClose)?;
        }
        if let Some(lock) = self.lock {
            lock.release()?;
        }
        backend.clear_memory().map_err(CatalogError::from)
    }
}
