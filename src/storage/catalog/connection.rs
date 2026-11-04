use super::*;

/// Why an operation could not run on the shared catalog connection.
#[derive(Debug)]
pub(super) enum ConnectionUnavailable {
    Closed,
    /// The operation panicked; its changes were rolled back.
    Panicked,
    /// An earlier panic left a transaction that still cannot be rolled back.
    Unrecovered(RollbackError),
}

impl std::fmt::Display for ConnectionUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str(BackendFailure::CatalogClosed.message()),
            Self::Panicked => {
                f.write_str("catalog operation panicked; its changes were rolled back")
            }
            Self::Unrecovered(error) => write!(f, "catalog connection is unusable: {error}"),
        }
    }
}

impl std::error::Error for ConnectionUnavailable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unrecovered(error) => Some(error),
            Self::Closed => Some(&BackendFailure::CatalogClosed),
            Self::Panicked => None,
        }
    }
}

impl crate::storage::failure::StorageCause for ConnectionUnavailable {
    fn into_cause(self) -> crate::storage::failure::BackendCause {
        crate::storage::failure::BackendCause::from_error(self)
    }
}

/// Why a transaction abandoned by a panic could not be confirmed as rolled back.
#[derive(Debug)]
pub(super) enum RollbackError {
    Rollback(rusqlite::Error),
    StillOpen,
    #[cfg(test)]
    Injected,
}

impl std::fmt::Display for RollbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rollback(error) => write!(f, "transaction could not be rolled back: {error}"),
            Self::StillOpen => f.write_str("connection is still inside a transaction"),
            #[cfg(test)]
            Self::Injected => f.write_str("injected rollback failure"),
        }
    }
}

impl std::error::Error for RollbackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rollback(error) => Some(error),
            _ => None,
        }
    }
}

impl Inner {
    /// Runs one operation on the shared connection. A panicking operation fails
    /// only its own request: its transaction is rolled back and the connection
    /// stays usable. Only when the rollback cannot be confirmed does the mutex
    /// stay poisoned, so later calls fail closed and `is_healthy` turns false.
    pub(super) fn with_connection<T, E>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, E>,
    ) -> Result<Result<T, E>, ConnectionUnavailable> {
        let mut resources = match self.resources.lock() {
            Ok(resources) => resources,
            Err(poisoned) => {
                let resources = poisoned.into_inner();
                if let Some(resources) = resources.as_ref() {
                    self.end_abandoned_transaction(&resources.connection)
                        .map_err(ConnectionUnavailable::Unrecovered)?;
                }
                self.resources.clear_poison();
                resources
            }
        };
        let Some(resources) = resources.as_mut() else {
            return Err(ConnectionUnavailable::Closed);
        };
        let connection = &mut resources.connection;
        // The connection is checked after an unwind before anyone reuses it, so
        // observing it mid-operation is harmless.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            change_journal::notify_on_write(connection, &self.change_wake, |connection| {
                change_journal::notify_on_write(connection, &self.notification_wake, operation)
            })
        }));
        match outcome {
            Ok(result) => Ok(result),
            Err(payload) => {
                if self.end_abandoned_transaction(connection).is_err() {
                    // Unwinding with the guard held poisons the mutex on purpose.
                    std::panic::resume_unwind(payload);
                }
                Err(ConnectionUnavailable::Panicked)
            }
        }
    }

    /// rusqlite rolls a `Transaction` back when it drops, including during a
    /// panic, but a raw `BEGIN` or a failed drop-time rollback can leave one open.
    fn end_abandoned_transaction(&self, connection: &Connection) -> Result<(), RollbackError> {
        #[cfg(test)]
        if self
            .fail_rollback
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RollbackError::Injected);
        }
        if !connection.is_autocommit() {
            connection
                .execute_batch("ROLLBACK")
                .map_err(RollbackError::Rollback)?;
        }
        if connection.is_autocommit() {
            Ok(())
        } else {
            Err(RollbackError::StillOpen)
        }
    }
}

impl Catalog {
    pub(super) fn activity(&self) -> Result<Activity, CatalogError> {
        self.0
            .activity
            .clone()
            .try_read_owned()
            .map_err(unavailable)
    }

    pub(super) async fn run<T: Send + 'static, E: From<CatalogError> + Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        let admission = self
            .0
            .admission
            .clone()
            .try_acquire_owned()
            // A closed catalog is not full: reporting a limit would invite retries at shutdown.
            .map_err(|error| admission_error(&error))?;
        self.run_admitted(admission, operation).await
    }

    pub(super) async fn run_waiting<T: Send + 'static, E: From<CatalogError> + Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        let admission = self
            .0
            .admission
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| unavailable(BackendFailure::CatalogClosed))?;
        self.run_admitted(admission, operation).await
    }

    pub(super) async fn run_admitted<T: Send + 'static, E: From<CatalogError> + Send + 'static>(
        &self,
        admission: tokio::sync::OwnedSemaphorePermit,
        operation: impl FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        let activity = self.activity()?;
        let execution = self
            .0
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        let inner = self.0.clone();
        self.0
            .executor
            .run(move || {
                let (_admission, _execution, _activity) = (admission, execution, activity);
                let result = match inner.with_connection(operation) {
                    Ok(result) => result,
                    Err(ConnectionUnavailable::Closed) => {
                        Err(unavailable(BackendFailure::CatalogClosed).into())
                    }
                    Err(error) => Err(unavailable(error).into()),
                };
                // Release ownership before the reply publishes completion to an immediate reopen.
                drop(inner);
                result
            })
            .await
            .map_err(unavailable)?
    }
}
