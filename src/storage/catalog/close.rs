use super::*;

impl Catalog {
    /// Stops admission on all handles and waits for blocking work before releasing ownership.
    /// Remaining handles become unavailable; repeated closes succeed without touching a new owner.
    pub async fn close(&self) -> Result<(), CatalogError> {
        self.0
            .uploads
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closed = true;
        self.0.admission.close();
        self.0.writer_released.notify_waiters();
        self.0.activity.wake_waiters();
        self.0.admin_shutdown.notify_waiters();
        self.0.notification_wake.notify_one();
        self.0.maintenance_wake.notify_one();
        let _notification_attempt = self
            .0
            .notification_attempts
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        let _publication_requests = self
            .0
            .publication_requests
            .clone()
            .acquire_many_owned(2)
            .await
            .map_err(unavailable)?;
        let _staged_handles = self
            .0
            .staged_handles
            .clone()
            .acquire_many_owned(256)
            .await
            .map_err(unavailable)?;
        let _admin_uploads = self
            .0
            .admin_uploads
            .clone()
            .acquire_many_owned(2)
            .await
            .map_err(unavailable)?;
        let _admin_reads = self
            .0
            .admin_reads
            .clone()
            .acquire_many_owned(32)
            .await
            .map_err(unavailable)?;
        loop {
            let idle = self.0.uploads_idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if self
                .0
                .uploads
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .active
                == 0
            {
                break;
            }
            idle.await;
        }
        let _reclamation = self
            .0
            .reclamation
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        let execution = self
            .0
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
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
                    .unwrap_or_else(|error| error.into_inner());
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
