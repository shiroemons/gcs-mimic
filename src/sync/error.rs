//! Failures of a synchronization pass or control, typed for the decisions made on them.

use crate::{
    bucket::CatalogError,
    storage::{
        admin::AdminError,
        sync_state::{
            conflicts,
            observation::PauseReason,
            root::RootError,
            writeback::{self, Stop},
        },
    },
    task::JobPanicked,
};
use std::{error::Error, fmt, io};

#[cfg(test)]
mod tests;

/// A failed synchronization pass or control. Each variant keeps the typed failure that decides
/// whether the mirror retries, pauses or answers a control.
#[derive(Debug)]
pub(crate) enum SyncError {
    Admin(AdminError),
    Root(RootError),
    Writeback(writeback::Error),
    Conflict(conflicts::Error),
    /// Native filesystem, watch and descriptor failures.
    Io(io::Error),
    /// Write-back recovery left this stop in place, so no pass may start.
    Recovery(Stop),
    /// An explicit rescan failed and paused synchronization for this reason.
    RescanStopped(PauseReason),
    /// Unfinished write-back blocks this pass until an operator resolves it.
    WritebackUnfinished(Pass),
    /// The mirror job panicked; `catch_job` has already reported the panic.
    Panicked(JobPanicked),
    /// A value that an earlier step checked broke the next step's assumption.
    Invariant(&'static str),
}

/// The pass that unfinished write-back blocks. Each names its own remedy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    Reconcile,
    Ingest,
}

/// One failure in a `SyncError`'s source chain, as the retry, pause and control decisions see it.
#[derive(Clone, Copy)]
pub(crate) enum Cause<'a> {
    Admin(&'a AdminError),
    Writeback(&'a writeback::Error),
    Conflict(&'a conflicts::Error),
    Stop(Stop),
    Catalog(&'a CatalogError),
    Sqlite(&'a rusqlite::Error),
    Io(&'a io::Error),
    /// A failure no decision depends on, such as a serde or join error.
    Other,
}

impl<'a> Cause<'a> {
    /// The one place that turns a chain back into types. The typed layers reach each other only
    /// through `source()`, and `BackendCause`/`IoCause` keep their sources as `dyn Error`.
    /// Write-back wraps typed sync failures into `AdminError::Backend` on purpose (a displaced
    /// live copy, a conflict transfer) so that its retry leaves them alone, and the decisions
    /// below must still see what that wrapper holds. These downcasts can go once storage stops
    /// erasing synchronization failures into backend causes.
    fn of(error: &'a (dyn Error + 'static)) -> Self {
        if let Some(error) = error.downcast_ref::<AdminError>() {
            Self::Admin(error)
        } else if let Some(error) = error.downcast_ref::<writeback::Error>() {
            Self::Writeback(error)
        } else if let Some(error) = error.downcast_ref::<conflicts::Error>() {
            Self::Conflict(error)
        } else if let Some(stop) = error.downcast_ref::<Stop>() {
            Self::Stop(*stop)
        } else if let Some(error) = error.downcast_ref::<CatalogError>() {
            Self::Catalog(error)
        } else if let Some(error) = error.downcast_ref::<rusqlite::Error>() {
            Self::Sqlite(error)
        } else if let Some(error) = error.downcast_ref::<io::Error>() {
            Self::Io(error)
        } else {
            Self::Other
        }
    }

    fn is_transient(self) -> bool {
        match self {
            Self::Admin(error) => match error {
                AdminError::Busy { .. } => true,
                // The kind alone; a raw OS error below it is its own `Io` cause.
                AdminError::Io(cause) => transient_kind(cause.kind()),
                AdminError::DiskFull(_)
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
            },
            Self::Catalog(error) => matches!(error, CatalogError::Unavailable(_)),
            Self::Sqlite(error) => matches!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ),
            Self::Io(error) => transient_io(error),
            Self::Writeback(_) | Self::Conflict(_) | Self::Stop(_) | Self::Other => false,
        }
    }

    fn pause_reason(self) -> Option<PauseReason> {
        match self {
            Self::Stop(stop) => Some(match stop {
                Stop::ConflictCapacity => PauseReason::ConflictCapacity,
                Stop::IoFailure => PauseReason::IoFailure,
                Stop::RecoveryRequired | Stop::ProtectedBody => PauseReason::RecoveryRequired,
                Stop::Ready => PauseReason::ObserverFailure,
            }),
            Self::Writeback(writeback::Error::Capacity)
            | Self::Conflict(conflicts::Error::Capacity) => Some(PauseReason::ConflictCapacity),
            Self::Writeback(
                writeback::Error::RecoveryRequired
                | writeback::Error::Protected
                | writeback::Error::WorkspaceChanged,
            ) => Some(PauseReason::RecoveryRequired),
            Self::Admin(AdminError::DiskFull(_) | AdminError::Io(_)) => {
                Some(PauseReason::IoFailure)
            }
            // Every other variant is listed so that a new one must choose a reason.
            Self::Writeback(writeback::Error::Admin(_) | writeback::Error::Root(_))
            | Self::Conflict(
                conflicts::Error::Admin(_)
                | conflicts::Error::Root(_)
                | conflicts::Error::RevisionMismatch
                | conflicts::Error::TargetChanged
                | conflicts::Error::OperationIdReused
                | conflicts::Error::InvalidResolution,
            )
            | Self::Admin(
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
                | AdminError::Backend(_),
            )
            | Self::Catalog(_)
            | Self::Sqlite(_)
            | Self::Io(_)
            | Self::Other => None,
        }
    }
}

impl SyncError {
    /// The failures below this one, outermost first, ending with whatever storage keeps as an
    /// erased source.
    pub(crate) fn causes(&self) -> impl Iterator<Item = Cause<'_>> {
        Error::source(self)
            .into_iter()
            .flat_map(crate::error::causes)
            .map(Cause::of)
    }

    /// Whether the failure may clear on its own: catalog contention, an admission timeout or an
    /// interrupted or timed-out I/O call. Everything else, such as an invalid name, a permission
    /// error or a corrupt catalog, needs an operator and pauses the mirror.
    pub(super) fn is_transient(&self) -> bool {
        self.causes().any(Cause::is_transient)
    }

    /// Whether rebuilding the native watch may succeed later: a transient failure, or read and
    /// search permission an operator may restore.
    pub(super) fn is_watch_retryable(&self) -> bool {
        self.is_transient()
            || self.causes().any(|cause| {
                matches!(cause, Cause::Io(error) if error.kind() == io::ErrorKind::PermissionDenied)
            })
    }

    /// Why the mirror pauses after this failure: the first cause in the chain that names a
    /// reason, or an observer failure when none does.
    pub(super) fn pause_reason(&self) -> PauseReason {
        if let Self::RescanStopped(reason) = self {
            return *reason;
        }
        self.causes()
            .find_map(Cause::pause_reason)
            .unwrap_or(PauseReason::ObserverFailure)
    }

    /// Whether a deletion pass met a concurrent change and must start over, not fail.
    pub(super) fn is_stale_deletion(&self) -> bool {
        matches!(
            self,
            Self::Root(RootError::Admin(
                AdminError::Precondition | AdminError::NotFound
            ))
        )
    }
}

/// Retry a write-back step that failed under descriptor or memory pressure.
pub(super) fn resource_pressure(error: &AdminError) -> bool {
    crate::error::causes(error)
        .map(Cause::of)
        .any(|cause| matches!(cause, Cause::Io(error) if transient_io(error)))
}

fn transient_kind(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::Interrupted
            | io::ErrorKind::ResourceBusy
    )
}

pub(super) fn transient_io(error: &io::Error) -> bool {
    transient_kind(error.kind())
        || matches!(
            error.raw_os_error(),
            Some(libc::EMFILE | libc::ENFILE | libc::EAGAIN | libc::ENOMEM)
        )
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A wrapped failure prints as itself: `Report` skips a cause whose text repeats the one
        // before it, so diagnostics read exactly as they did with `io::Error::other` wrappers.
        match self {
            Self::Admin(error) => fmt::Display::fmt(error, f),
            Self::Root(error) => fmt::Display::fmt(error, f),
            Self::Writeback(error) => fmt::Display::fmt(error, f),
            Self::Conflict(error) => fmt::Display::fmt(error, f),
            Self::Io(error) => fmt::Display::fmt(error, f),
            Self::Recovery(stop) => fmt::Display::fmt(stop, f),
            Self::RescanStopped(reason) => {
                write!(f, "Explicit rescan stopped synchronization: {reason:?}")
            }
            Self::WritebackUnfinished(Pass::Reconcile) => f.write_str(
                "Unfinished write-back requires recovery; inspect retained conflicts and workspace findings, resolve the exact retained revision and rescan",
            ),
            Self::WritebackUnfinished(Pass::Ingest) => f.write_str(
                "Unfinished write-back blocks ingestion; inspect retained conflicts and recovery findings, preserve the workspace and resolve the exact retained revision before rescanning",
            ),
            Self::Panicked(error) => fmt::Display::fmt(error, f),
            Self::Invariant(message) => f.write_str(message),
        }
    }
}

impl Error for SyncError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Admin(error) => Some(error),
            Self::Root(error) => Some(error),
            Self::Writeback(error) => Some(error),
            Self::Conflict(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Recovery(stop) => Some(stop),
            Self::Panicked(error) => Some(error),
            Self::RescanStopped(_) | Self::WritebackUnfinished(_) | Self::Invariant(_) => None,
        }
    }
}

impl From<AdminError> for SyncError {
    fn from(error: AdminError) -> Self {
        Self::Admin(error)
    }
}

impl From<RootError> for SyncError {
    fn from(error: RootError) -> Self {
        Self::Root(error)
    }
}

impl From<writeback::Error> for SyncError {
    fn from(error: writeback::Error) -> Self {
        Self::Writeback(error)
    }
}

impl From<conflicts::Error> for SyncError {
    fn from(error: conflicts::Error) -> Self {
        Self::Conflict(error)
    }
}

impl From<io::Error> for SyncError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<JobPanicked> for SyncError {
    fn from(error: JobPanicked) -> Self {
        Self::Panicked(error)
    }
}

/// Public `sync` functions keep their `io::Result` signatures; this is their one conversion.
impl From<SyncError> for io::Error {
    fn from(error: SyncError) -> Self {
        match error {
            // A native failure keeps its kind and raw OS error, as it did before.
            SyncError::Io(error) => error,
            SyncError::Admin(_)
            | SyncError::Root(_)
            | SyncError::Writeback(_)
            | SyncError::Conflict(_)
            | SyncError::Recovery(_)
            | SyncError::RescanStopped(_)
            | SyncError::WritebackUnfinished(_)
            | SyncError::Panicked(_)
            | SyncError::Invariant(_) => Self::other(error),
        }
    }
}
