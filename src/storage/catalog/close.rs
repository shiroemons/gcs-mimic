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
    pub async fn close(&self) -> Result<(), CatalogError> {
        self.0
            .uploads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed = true;
        self.0.admission.close();
        self.0.writer_released.notify_waiters();
        self.0.activity.wake_waiters();
        self.0.admin_shutdown.notify_waiters();
        self.0.notification_wake.notify_one();
        self.0.maintenance_wake.notify_one();
        let _notification_attempt = drain(&self.0.notification_attempts, 1).await?;
        let _publication_requests =
            drain(&self.0.publication_requests, PUBLICATION_REQUESTS).await?;
        let _staged_handles = drain(&self.0.staged_handles, STAGED_HANDLES).await?;
        let _admin_uploads = drain(&self.0.admin_uploads, ADMIN_UPLOADS).await?;
        let _admin_reads = drain(&self.0.admin_reads, ADMIN_READS).await?;
        loop {
            let idle = self.0.uploads_idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if self
                .0
                .uploads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active
                == 0
            {
                break;
            }
            idle.await;
        }
        let _reclamation = drain(&self.0.reclamation, 1).await?;
        if let Some(pool) = &self.0.read_pool {
            pool.close().await;
        }
        // Remaining handles must not keep the blobs open: ownership of the directory ends here.
        self.0.read_cache.invalidate();
        let execution = drain(&self.0.execution, 1).await?;
        let inner = self.0.clone();
        let closed = self
            .0
            .executor
            .run_last(move || {
                let _execution = execution;
                // Poison must not prevent shutdown from closing the connection and its lock.
                let mut resources = inner
                    .resources
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(Resources { connection, lock }) = resources.take() {
                    connection
                        .close()
                        .map_err(|(_, error)| unavailable(error))?;
                    if let Some(lock) = lock {
                        lock.release()?;
                    }
                    inner.backend.clear_memory()?;
                }
                Ok(())
            })
            .await;
        match closed {
            Ok(result) => result,
            // An earlier close already released the connection and stopped the thread.
            Err(executor::ExecutorError::Closed) => Ok(()),
            Err(error) => Err(unavailable(error)),
        }
    }
}
