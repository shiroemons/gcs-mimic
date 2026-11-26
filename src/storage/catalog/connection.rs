use super::*;
use crate::storage::failure::{Link, TypedSource};

/// Why a read needs the writer's queue or its connection-local state.
#[derive(Debug)]
pub(super) enum SerializedRead {
    AdvisoryUpload,
    AdvisoryXmlUpload,
    // Production accesses these TEMP tables inside mutations. Only tests read them separately.
    #[cfg(test)]
    ResumableSessionCounts,
    #[cfg(test)]
    NotificationRuntime,
    #[cfg(test)]
    LifecycleQuarantine,
}

/// Why an operation could not run on the shared catalog connection.
#[derive(Debug)]
pub(crate) enum ConnectionUnavailable {
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
        self.typed_source().map(Link::as_error)
    }
}

impl TypedSource for ConnectionUnavailable {
    fn typed_source(&self) -> Option<Link<'_>> {
        match self {
            Self::Unrecovered(error) => Some(Link::Rollback(error)),
            Self::Closed => Some(Link::Failure(&BackendFailure::CatalogClosed)),
            Self::Panicked => None,
        }
    }
}

impl crate::storage::failure::StorageCause for ConnectionUnavailable {
    fn into_cause(self) -> crate::storage::failure::BackendCause {
        crate::storage::failure::BackendCause::from_error(self)
    }
}

/// A closed catalog reports the same failure as every other closed-catalog path.
impl From<ConnectionUnavailable> for CatalogError {
    fn from(error: ConnectionUnavailable) -> Self {
        match error {
            ConnectionUnavailable::Closed => unavailable(BackendFailure::CatalogClosed),
            error @ (ConnectionUnavailable::Panicked | ConnectionUnavailable::Unrecovered(_)) => {
                unavailable(error)
            }
        }
    }
}

/// A closed catalog reports the same failure as every other closed-catalog path.
impl From<ConnectionUnavailable> for crate::object::ObjectError {
    fn from(error: ConnectionUnavailable) -> Self {
        use crate::storage::failure::object_unavailable;
        match error {
            ConnectionUnavailable::Closed => object_unavailable(BackendFailure::CatalogClosed),
            error @ (ConnectionUnavailable::Panicked | ConnectionUnavailable::Unrecovered(_)) => {
                object_unavailable(error)
            }
        }
    }
}

/// Why a transaction abandoned by a panic could not be confirmed as rolled back.
#[derive(Debug)]
pub(crate) enum RollbackError {
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
        self.typed_source().map(Link::as_error)
    }
}

impl TypedSource for RollbackError {
    fn typed_source(&self) -> Option<Link<'_>> {
        match self {
            Self::Rollback(error) => Some(Link::Sqlite(error)),
            Self::StillOpen => None,
            #[cfg(test)]
            Self::Injected => None,
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
        let mut resources = match crate::sync::lock_checked(&self.writer.resources) {
            Ok(resources) => resources,
            Err(poisoned) => {
                let resources = poisoned.into_inner();
                if let Some(resources) = resources.as_ref() {
                    self.writer
                        .end_abandoned_transaction(&resources.connection)
                        .map_err(ConnectionUnavailable::Unrecovered)?;
                }
                self.writer.resources.clear_poison();
                crate::sync::report_recovery();
                resources
            }
        };
        let Some(resources) = resources.as_mut() else {
            return Err(ConnectionUnavailable::Closed);
        };
        let connection = &mut resources.connection;
        // A memory catalog's full database is its page budget; the operation's own error types
        // cannot tell it from a full disk.
        let _pages = match self.storage.backend.kind() {
            crate::config::Backend::Memory => {
                Some(crate::storage::failure::MemoryCatalogJob::enter())
            }
            crate::config::Backend::Disk => None,
        };
        let changes_before = connection.total_changes();
        let rule_writes = lifecycle::RuleWriteScope::enter();
        // The connection is checked after an unwind before anyone reuses it, so
        // observing it mid-operation is harmless.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let result = operation(connection);
            // A rollback or an unrelated write wakes the notification worker too; it only
            // rereads the outbox.
            if connection.total_changes() != changes_before {
                self.notifications.wake.notify_waiters();
            }
            result
        }));
        let rules_written = rule_writes.written();
        // Before the caller can learn the outcome, so a read after this write misses the cache. A
        // rollback also counts as a change, which only costs some cached answers.
        if connection.total_changes() != changes_before {
            self.reads.invalidate();
            if rules_written {
                self.maintenance.lifecycle_rules_changed();
            } else {
                self.maintenance.lifecycle.on_write(&self.maintenance.wake);
            }
        }
        match outcome {
            Ok(result) => Ok(result),
            Err(payload) => {
                if self.writer.end_abandoned_transaction(connection).is_err() {
                    // Unwinding with the guard held poisons the mutex on purpose.
                    std::panic::resume_unwind(payload);
                }
                Err(ConnectionUnavailable::Panicked)
            }
        }
    }
}

impl super::state::Writer {
    /// Transaction guards roll back on drop, including during a panic.
    /// A raw `BEGIN` or a failed drop-time rollback can leave one open.
    #[cfg_attr(
        not(test),
        expect(
            clippy::unused_self,
            reason = "only the test build injects a rollback failure through the connection's state"
        )
    )]
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
    pub(super) async fn run_serialized_read<
        T: Send + 'static,
        E: From<CatalogError> + Send + 'static,
    >(
        &self,
        reason: SerializedRead,
        operation: impl FnOnce(&Connection) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        self.run(move |connection| {
            #[cfg(not(test))]
            let _ = reason;
            #[cfg(test)]
            let before = connection.total_changes();
            let result = operation(connection);
            #[cfg(test)]
            assert_eq!(
                before,
                connection.total_changes(),
                "{reason:?} changed rows"
            );
            result
        })
        .await
    }

    /// Queues one operation on the shared connection. A caller past the queue waits its turn
    /// instead of failing: jobs run one at a time, so the wait changes latency, not the load on
    /// SQLite, and a local client cannot act on a rejection except by retrying.
    ///
    /// Nothing here times out. Request handlers bound the wait with their own deadlines, and
    /// dropping this future while it waits gives up the place in line without consuming a slot,
    /// so a disconnected client costs nothing. Close wakes every waiter with `CatalogClosed`.
    ///
    /// The execution permit alone keeps this job apart from an exclusive job, which waits in the
    /// same line and owns the permit for as long as it runs. A lease held while queueing here
    /// would let a steady stream of callers keep that job out.
    pub(super) async fn run<T: Send + 'static, E: From<CatalogError> + Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        let admission = self
            .admit()
            .await
            .map_err(|_| unavailable(BackendFailure::CatalogClosed))?;
        let execution = self
            .0
            .writer
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        let inner = self.0.clone();
        self.0
            .writer
            .executor
            .run(move || {
                let (_admission, _execution) = (admission, execution);
                let result = inner
                    .with_connection(operation)
                    .unwrap_or_else(|error| Err(CatalogError::from(error).into()));
                // Release ownership before the reply publishes completion to an immediate reopen.
                drop(inner);
                result
            })
            .await
            .map_err(unavailable)?
    }

    /// Waits for one of the slots that bound all catalog work. Close fails the wait at once.
    pub(super) async fn admit(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::AcquireError> {
        #[cfg(test)]
        let _waiting = WaitingForAdmission::enter(&self.0.admission.waiters);
        self.0.admission.slots.clone().acquire_owned().await
    }

    /// Takes every admission slot, so later `run` calls wait until the returned permit drops.
    #[cfg(test)]
    pub(crate) fn hold_admission(&self) -> tokio::sync::OwnedSemaphorePermit {
        self.0
            .admission
            .slots
            .clone()
            .try_acquire_many_owned(u32::try_from(MAX_IN_FLIGHT).unwrap())
            .unwrap()
    }

    /// The number of `run` calls currently waiting for an admission slot.
    #[cfg(test)]
    pub(crate) fn admission_waiters(&self) -> usize {
        self.0
            .admission
            .waiters
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// The admission slots no job or waiter holds.
    #[cfg(test)]
    pub(crate) fn free_admission_slots(&self) -> usize {
        self.0.admission.slots.available_permits()
    }
}

/// Counts a caller for as long as it waits for admission. The count falls when the wait ends for
/// any reason, including the caller being dropped, which is what cancellation tests observe.
#[cfg(test)]
struct WaitingForAdmission<'a>(&'a std::sync::atomic::AtomicUsize);

#[cfg(test)]
impl<'a> WaitingForAdmission<'a> {
    fn enter(waiters: &'a std::sync::atomic::AtomicUsize) -> Self {
        waiters.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(waiters)
    }
}

#[cfg(test)]
impl Drop for WaitingForAdmission<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests;
