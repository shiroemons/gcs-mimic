//! Retrying the steps of an export that fail transiently, with backoff.

#[cfg(test)]
use crate::sync::mirror::export_checkpoint;
use crate::{
    diagnostics::{self, LogLevel},
    storage::{
        admin::AdminError,
        sync_state::{
            root::{RootError, RootLease},
            writeback::{self, LiveExport, LiveResult, Writeback},
        },
    },
    sync::{
        SyncError,
        report::remedy::export_retry_remedy,
        tree::{ParentError, ParentOpen, parent},
    },
};
use std::{fs::File, future::Future, io, path::PathBuf, time::Duration};

/// The first retry delay after a transient failure; it doubles after each consecutive one.
/// Tests shorten it so a retry does not dominate their run time.
const RETRY_DELAY_MIN: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(1)
};
/// The retry delay cap, also the run length after which a failure starts the backoff over.
pub(in crate::sync) const RETRY_DELAY_MAX: Duration = Duration::from_secs(30);

/// A retry delay that doubles after each consecutive failure, up to `max`.
#[derive(Clone, Copy, Debug)]
pub(in crate::sync) struct Backoff {
    first: Duration,
    max: Duration,
    next: Duration,
    retries: u32,
}

impl Backoff {
    pub(in crate::sync) const fn new(first: Duration, max: Duration) -> Self {
        Self {
            first,
            max,
            next: first,
            retries: 0,
        }
    }

    /// The wait before the next retry; the one after it is twice as long, up to `max`.
    pub(in crate::sync) fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = delay.saturating_mul(2).min(self.max);
        self.retries = self.retries.saturating_add(1);
        delay
    }

    /// How many retries this backoff has scheduled.
    pub(in crate::sync) fn retries(&self) -> u32 {
        self.retries
    }

    /// Starts over, after a run that had recovered.
    pub(in crate::sync) fn reset(&mut self) {
        *self = Self::new(self.first, self.max);
    }
}

/// The mirror's restart and watch-rebuild backoff.
pub(in crate::sync) const MIRROR_BACKOFF: Backoff = Backoff::new(RETRY_DELAY_MIN, RETRY_DELAY_MAX);
/// A failed export step waits 1 s, then 2 s; a third failure pauses synchronization.
pub(in crate::sync) const EXPORT_BACKOFF: Backoff =
    Backoff::new(Duration::from_secs(1), RETRY_DELAY_MAX);
pub(in crate::sync) const EXPORT_RETRIES: u32 = 2;

pub(in crate::sync) fn retryable(error: &writeback::Error) -> bool {
    let error = match error {
        writeback::Error::Admin(error) | writeback::Error::Root(RootError::Admin(error)) => error,
        writeback::Error::Root(
            RootError::InUse
            | RootError::Unsupported
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
            Err(ParentError::Io(error)) => Err(error.into()),
        }
    })
    .await
}

pub(super) async fn advance_with_retry(
    writeback: &Writeback,
    operation: LiveExport,
) -> Result<LiveResult, SyncError> {
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
