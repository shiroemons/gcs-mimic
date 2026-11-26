use super::open::{ADMIN_READS, ADMIN_UPLOADS, PUBLICATION_REQUESTS, STAGED_HANDLES};
use super::*;
use tokio::sync::OwnedSemaphorePermit;

/// Waits until every one of `semaphore`'s `permits` is back, so no holder is still working, and
/// keeps them so no new holder starts.
async fn drain(
    semaphore: &Arc<Semaphore>,
    permits: u32,
) -> Result<OwnedSemaphorePermit, CatalogError> {
    semaphore
        .clone()
        .acquire_many_owned(permits)
        .await
        .map_err(unavailable)
}

impl Catalog {
    /// Stops admission on all handles and waits for blocking work before releasing ownership.
    /// Remaining handles become unavailable; repeated closes succeed without touching a new owner.
    /// The runtime used at open must stay driven through this drain: it owns the blocking
    /// descriptor closes and their supervisors. Runtime shutdown cancels queued work and
    /// drops its owned files, but cannot guarantee completion of an orderly catalog close.
    pub async fn close(&self) -> Result<(), CatalogError> {
        crate::sync::lock(&self.0.admission.uploads).closed = true;
        self.0.admission.slots.close();
        self.0.writers.released.notify_waiters();
        self.0.admission.activity.wake_waiters();
        self.0.shutdown.admin_wake.notify_waiters();
        self.0.notifications.wake.notify_one();
        self.0.maintenance.wake.notify_one();
        self.0.admin.seed_watch.wake();
        let _notification_attempt = drain(&self.0.notifications.attempts, 1).await?;
        let _publication_requests =
            drain(&self.0.admin.publication_requests, PUBLICATION_REQUESTS).await?;
        let _staged_handles = drain(&self.0.admin.staged_handles, STAGED_HANDLES).await?;
        let _admin_uploads = drain(&self.0.admin.uploads, ADMIN_UPLOADS).await?;
        let _admin_reads = drain(&self.0.admin.reads, ADMIN_READS).await?;
        loop {
            let idle = self.0.admission.uploads_idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if crate::sync::lock(&self.0.admission.uploads).active == 0 {
                break;
            }
            idle.await;
        }
        let _reclamation = drain(&self.0.storage.reclamation, 1).await?;
        if let Some(pool) = &self.0.reads.pool {
            pool.close().await;
        }
        let execution = drain(&self.0.writer.execution, 1).await?;
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
        if let Some(lock) = self.lock {
            lock.release()?;
        }
        backend.clear_memory().map_err(CatalogError::from)
    }
}
