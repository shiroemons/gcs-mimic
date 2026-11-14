//! Failures of a synchronization pass or control, typed for the decisions made on them.

use crate::{
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

impl SyncError {
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
