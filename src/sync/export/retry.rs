//! Retrying the steps of an export that fail transiently, with backoff.

use crate::sync::backoff::{Backoff, RETRY_DELAY_MAX};
#[cfg(test)]
use crate::sync::mirror::export_checkpoint;
use crate::{
    diagnostics::{self, LogLevel},
    storage::{
        admin::AdminError,
        sync_state::{
            root::{RootError, RootLease},
            writeback::{self, InstallOutcome, LiveExport, Writeback},
        },
    },
    sync::{
        SyncError,
        report::remedy::export_retry_remedy,
        tree::{ParentError, ParentOpen, parent},
    },
};
use std::{fs::File, future::Future, io, path::PathBuf, time::Duration};

/// A failed export step waits 1 s, then 2 s; a third failure pauses synchronization.
pub(in crate::sync) const EXPORT_BACKOFF: Backoff =
    Backoff::new(Duration::from_secs(1), RETRY_DELAY_MAX);
pub(in crate::sync) const EXPORT_RETRIES: u32 = 2;

pub(in crate::sync) fn retryable(error: &writeback::Error) -> bool {
    let error = match error {
        writeback::Error::Admin(error) | writeback::Error::Root(RootError::Admin(error)) => error,
        writeback::Error::Root(
            RootError::InUse
            | RootError::Unsupported { .. }
            | RootError::RootChanged
            | RootError::MissingMarker
            | RootError::InvalidMarker
            | RootError::ForeignMarker
            | RootError::ReinitializeDenied
            | RootError::ReplacementOccupied { .. },
        )
        // A displaced target belongs to recovery, so neither is retried automatically.
        | writeback::Error::Conflict(_)
        | writeback::Error::Displaced(_)
        | writeback::Error::Capacity
        | writeback::Error::RecoveryRequired
        | writeback::Error::Protected
        | writeback::Error::WorkspaceChanged => return false,
    };
    match error {
        AdminError::DiskFull(_) => true,
        AdminError::Io(cause) => {
            matches!(
                cause.kind(),
                std::io::ErrorKind::StorageFull | std::io::ErrorKind::PermissionDenied
            ) || crate::sync::error::transient_io(&io::Error::from(cause.kind()))
                || crate::sync::error::resource_pressure(error)
        }
        AdminError::Busy { .. }
        | AdminError::Closed
        | AdminError::Precondition
        | AdminError::Hold
        | AdminError::SeedProjectMismatch
        | AdminError::SeedMetadataBudget { .. }
        | AdminError::SourceChanged { .. }
        | AdminError::Corrupt(_)
        | AdminError::NotFound
        | AdminError::Invalid(_)
        | AdminError::Checksum
        | AdminError::TooLarge
        | AdminError::Forbidden
        | AdminError::BodyAborted(_)
        | AdminError::Backend(_) => false,
    }
}

pub(super) async fn wait_retry(
    error: &writeback::Error,
    retry: &mut Backoff,
    root: &RootLease,
) -> bool {
    if retry.retries() >= EXPORT_RETRIES {
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

pub(super) async fn file_job_retry<T: Send + 'static>(
    root: &RootLease,
    job: impl Fn(&File) -> Result<T, AdminError> + Clone + Send + 'static,
) -> Result<T, SyncError> {
    with_retry(root, || {
        let job = job.clone();
        async move { root.file_job(job).await.map_err(writeback::Error::Root) }
    })
    .await
}

pub(in crate::sync) async fn create_directory(
    root: &RootLease,
    directory: PathBuf,
) -> Result<bool, SyncError> {
    let naming = root.naming()?;
    let lookup = root.lookup()?;
    file_job_retry(root, move |root| {
        match parent(root, naming, &lookup, &directory, ParentOpen::Create) {
            Ok(_) => Ok(true),
            Err(ParentError::Finding(_) | ParentError::Missing) => Ok(false),
            Err(ParentError::Transient(error) | ParentError::Io(error)) => Err(error.into()),
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

pub(in crate::sync) async fn with_retry<T, F: Future<Output = Result<T, writeback::Error>>>(
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

pub(super) fn concurrent_change(error: &writeback::Error) -> bool {
    match error {
        writeback::Error::Admin(AdminError::Precondition | AdminError::NotFound) => true,
        writeback::Error::Admin(AdminError::Io(cause)) => {
            cause.kind() == io::ErrorKind::AlreadyExists
        }
        writeback::Error::Admin(
            AdminError::Busy { .. }
            | AdminError::DiskFull(_)
            | AdminError::Closed
            | AdminError::Hold
            | AdminError::SeedProjectMismatch
            | AdminError::SeedMetadataBudget { .. }
            | AdminError::SourceChanged { .. }
            | AdminError::Corrupt(_)
            | AdminError::Invalid(_)
            | AdminError::Checksum
            | AdminError::TooLarge
            | AdminError::Forbidden
            | AdminError::BodyAborted(_)
            | AdminError::Backend(_),
        )
        | writeback::Error::Root(_)
        | writeback::Error::Conflict(_)
        | writeback::Error::Displaced(_)
        | writeback::Error::Capacity
        | writeback::Error::RecoveryRequired
        | writeback::Error::Protected
        | writeback::Error::WorkspaceChanged => false,
    }
}
