//! Retrying the steps of an export that fail transiently, with backoff.

use crate::sync::backoff::{Backoff, RETRY_DELAY_MAX, RetryBudget};
#[cfg(test)]
use crate::sync::mirror::export_checkpoint;
use crate::{
    diagnostics::{self, LogLevel},
    fsutil::Lookup,
    storage::{
        admin::AdminError,
        failure::Link,
        sync_state::{
            root::{RootError, RootLease},
            writeback::{self, InstallOutcome, LiveExport, Writeback},
        },
    },
    sync::{
        SyncError,
        error::Cause,
        failure::{FailureClass, transient_below},
        report::remedy::export_retry_remedy,
        tree::{ParentError, ParentOpen, parent},
    },
};
use std::{fs::File, future::Future, io, path::PathBuf, time::Duration};

/// A failed export step waits 1 s, then 2 s; a third failure pauses synchronization.
pub(super) const EXPORT_BACKOFF: Backoff = Backoff::new(Duration::from_secs(1), RETRY_DELAY_MAX);

/// Whether a failed export step is tried again within [`RetryBudget::EXPORT`]: a failure that
/// passes, or one of the whole file system, which an operator may clear before the budget runs
/// out ([`FailureClass::retries`]). A step that still fails then fails the path as before.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "a variant not named here retries as its class says, and the classification names every variant"
)]
pub(super) fn retryable(error: &writeback::Error) -> bool {
    let class = match error {
        // A conflict or a displaced target belongs to recovery, so neither is retried
        // automatically, whatever failure it carries.
        writeback::Error::Conflict(_) | writeback::Error::Displaced(_) => return false,
        writeback::Error::Admin(error) | writeback::Error::Root(RootError::Admin(error)) => {
            match error {
                // A missing permission has no class, since only an operator restores it. An
                // export step retries it all the same: the operator repairs a file or parent
                // permission in place, and the step then goes on without recording a mapping
                // finding or advancing the journal cursor.
                AdminError::Io(cause) if cause.kind() == io::ErrorKind::PermissionDenied => {
                    return true;
                }
                // Descriptor or memory pressure shows only in the I/O failure below.
                AdminError::Io(_) if transient_below(error) => Some(FailureClass::Transient),
                error => FailureClass::of_admin(error),
            }
        }
        // An entry that kept failing to open is told by the link below the error that met it.
        error => Cause::chain(Link::Writeback(error)).find_map(Cause::class),
    };
    class.is_some_and(FailureClass::retries)
}

pub(super) async fn wait_retry(
    error: &writeback::Error,
    retry: &mut Backoff,
    root: &RootLease,
) -> bool {
    if !RetryBudget::EXPORT.allows_retry(retry.retries()) {
        return false;
    }
    #[cfg(test)]
    export_checkpoint("retry").await;
    let delay = retry.next_delay();
    diagnostics::emit(
        LogLevel::Error,
        "sync_export_retry",
        serde_json::json!({
            "reason":crate::error::Report(error).to_string(),"retry_seconds":delay.as_secs(),
            "remedy":crate::sync::report::command::logged(&export_retry_remedy())
        }),
    );
    if let Some(mut stopping) = root.stop_signal() {
        if *stopping.borrow() {
            return false;
        }
        tokio::select! {_=stopping.changed()=>false,()=tokio::time::sleep(delay)=>true}
    } else {
        tokio::time::sleep(delay).await;
        true
    }
}

/// Runs `job` as a file job, again after a failure [`retryable`] allows, each attempt with
/// lookups of its own, so that the waits of one attempt do not use up those of the next.
pub(super) async fn file_job_retry<T: Send + 'static>(
    root: &RootLease,
    job: impl Fn(&File, &Lookup) -> Result<T, AdminError> + Clone + Send + 'static,
) -> Result<T, SyncError> {
    let lookup = root.lookup()?;
    with_retry(root, || {
        let job = job.clone();
        let lookup = lookup.for_job();
        async move {
            root.file_job(move |root| job(root, &lookup))
                .await
                .map_err(writeback::Error::Root)
        }
    })
    .await
}

/// What [`create_directory`] left at its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) enum DirectoryCreation {
    /// The directory is there, whether this made it or it already was.
    Present,
    /// A finding keeps the path from holding the directory.
    Finding,
    /// An entry at the path, or above it, kept failing to open: a later pass tries again.
    Deferred,
}

pub(in crate::sync) async fn create_directory(
    root: &RootLease,
    directory: PathBuf,
) -> Result<DirectoryCreation, SyncError> {
    let naming = root.naming()?;
    file_job_retry(root, move |root, lookup| {
        match parent(root, naming, lookup, &directory, ParentOpen::Create) {
            Ok(_) => Ok(DirectoryCreation::Present),
            Err(ParentError::Finding(_) | ParentError::Missing(_)) => {
                Ok(DirectoryCreation::Finding)
            }
            Err(ParentError::Unopenable) => Ok(DirectoryCreation::Deferred),
            Err(ParentError::Io(error)) => Err(error.into()),
        }
    })
    .await
}

pub(super) async fn advance_with_retry(
    writeback: &Writeback,
    operation: LiveExport,
) -> Result<InstallOutcome, SyncError> {
    with_retry(writeback.root_lease(), || {
        writeback.advance_live_export(operation.clone())
    })
    .await
}

pub(super) async fn with_retry<T, F: Future<Output = Result<T, writeback::Error>>>(
    root: &RootLease,
    mut operation: impl FnMut() -> F,
) -> Result<T, SyncError> {
    let mut retry = EXPORT_BACKOFF;
    loop {
        // Keep nested export futures off the mirror worker's bounded thread stack.
        match Box::pin(operation()).await {
            Ok(result) => return Ok(result),
            Err(error) if retryable(&error) => {
                if !wait_retry(&error, &mut retry, root).await {
                    return Err(error.into());
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
}
