//! Failures of a synchronization pass or control, typed for the decisions made on them.

use crate::{
    bucket::CatalogError,
    storage::{
        admin::AdminError,
        failure::{Link, TypedSource, chain},
        sync_state::{
            conflicts,
            observation::PauseReason,
            root::RootError,
            writeback::{self, Stop},
        },
    },
    sync::naming::ProbeError,
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
    /// The root's naming rules could not be established, so no name may be mapped.
    RootNaming(ProbeError),
    /// An explicit rescan of a running mirror failed in a way that pauses synchronization, which
    /// ends the run. The rescan waits inside for the mirror to pause before it hears the answer.
    RescanStopped(Box<StoppedRescan>),
    /// Unfinished write-back blocks this pass until an operator resolves it.
    WritebackUnfinished(Pass),
    /// The mirror job panicked; `catch_job` has already reported the panic.
    Panicked(JobPanicked),
    /// A value that an earlier step checked broke the next step's assumption.
    Invariant(&'static str),
}

/// An explicit rescan whose failure pauses synchronization: the reason it pauses for, its own
/// failure, and the caller waiting for the answer.
#[derive(Debug)]
pub(crate) struct StoppedRescan {
    pub(crate) reason: PauseReason,
    pub(crate) error: SyncError,
    pub(crate) reply: crate::sync::control::Reply<crate::sync::Reconciliation>,
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
    Root(&'a RootError),
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
    /// What one link of the typed chain decides. Backend and I/O causes keep their sources as
    /// typed links, so a synchronization failure carried inside a backend cause still reaches
    /// these decisions. Every link is listed, so a new one must choose whether it decides.
    fn of(link: Link<'a>) -> Self {
        match link {
            Link::Admin(error) => Self::Admin(error),
            Link::Root(error) => Self::Root(error),
            Link::Writeback(error) => Self::Writeback(error),
            Link::Conflict(error) => Self::Conflict(error),
            Link::Stop(stop) => Self::Stop(*stop),
            Link::Catalog(error) => Self::Catalog(error),
            Link::Sqlite(error) => Self::Sqlite(error),
            Link::Io(error) => Self::Io(error),
            Link::Failure(_)
            | Link::Exhausted(_)
            | Link::Context(_)
            | Link::Object(_)
            | Link::Connection(_)
            | Link::Executor(_)
            | Link::Rollback(_)
            | Link::Resume(_)
            | Link::Leaf(_) => Self::Other,
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
            Self::Root(_)
            | Self::Writeback(_)
            | Self::Conflict(_)
            | Self::Stop(_)
            | Self::Other => false,
        }
    }

    /// Whether this failure is the file system's rather than one path's: a full disk, an
    /// exhausted quota or a read-only file system fails every later write there as well.
    fn concerns_the_volume(self) -> bool {
        matches!(self, Self::Admin(AdminError::DiskFull(_)))
            || matches!(self, Self::Admin(AdminError::Io(cause)) if volume_kind(cause.kind()))
            || matches!(self, Self::Io(error) if volume_kind(error.kind()))
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
            // Every other variant is listed so that a new one must choose a reason. A wrapper
            // decides nothing itself; the failure it carries is the next cause in the chain.
            Self::Writeback(
                writeback::Error::Admin(_)
                | writeback::Error::Root(_)
                | writeback::Error::Conflict(_)
                | writeback::Error::Displaced(_),
            )
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
            | Self::Root(_)
            | Self::Catalog(_)
            | Self::Sqlite(_)
            | Self::Io(_)
            | Self::Other => None,
        }
    }
}

impl SyncError {
    /// The failures below this one, outermost first, ending with failures no decision reads.
    pub(crate) fn causes(&self) -> impl Iterator<Item = Cause<'_>> {
        self.typed_source()
            .into_iter()
            .flat_map(chain)
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
        if let Self::RescanStopped(stopped) = self {
            return stopped.reason;
        }
        if let Self::RootNaming(error) = self {
            return error.pause_reason();
        }
        self.causes()
            .find_map(Cause::pause_reason)
            .unwrap_or(PauseReason::ObserverFailure)
    }

    /// Whether the failure concerns only the path one export was writing: an I/O failure there
    /// or a file that changed under the export. An export that observes a finding again keeps
    /// that finding for a later pass on such a failure; a full disk, an exhausted quota, a
    /// read-only file system, write-back stops, conflict capacity and catalog failures concern
    /// every export and still fail the pass.
    pub(super) fn concerns_one_object(&self) -> bool {
        if self.causes().any(Cause::concerns_the_volume) {
            return false;
        }
        match self.causes().find_map(Cause::pause_reason) {
            Some(PauseReason::IoFailure) => true,
            Some(
                PauseReason::ConflictCapacity
                | PauseReason::RecoveryRequired
                | PauseReason::ObserverFailure
                | PauseReason::SourceChanging
                | PauseReason::SourceUnrepresentable,
            ) => false,
            None => self.causes().any(|cause| {
                matches!(
                    cause,
                    Cause::Writeback(
                        writeback::Error::Conflict(_) | writeback::Error::Displaced(_)
                    ) | Cause::Conflict(conflicts::Error::TargetChanged)
                )
            }),
        }
    }

    /// Whether a deletion pass met a concurrent change and must start over, not fail.
    pub(super) fn is_stale_deletion(&self) -> bool {
        matches!(self, Self::Root(error) if superseded(error))
    }
}

/// Whether a catalog write about one name failed because an API or bucket change made it moot
/// after the caller read the state it describes: the object, its generation or its bucket
/// changed. The journal brings that change to the next replay or pass, which records what then
/// holds; failing would pause the whole mirror over a race it resolves by itself.
pub(super) fn superseded(error: &RootError) -> bool {
    matches!(
        error,
        RootError::Admin(AdminError::Precondition | AdminError::NotFound)
    )
}

/// The result of a catalog write about one name, with a write that a concurrent change made
/// moot, as [`superseded`] decides, as done: whether it was recorded or is no longer needed.
pub(super) fn unless_superseded(result: Result<(), RootError>) -> Result<Recorded, RootError> {
    match result {
        Ok(()) => Ok(Recorded::Yes),
        Err(error) if superseded(&error) => Ok(Recorded::Superseded),
        Err(error) => Err(error),
    }
}

/// Whether a catalog write about one name was recorded or a concurrent change made it moot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Recorded {
    Yes,
    Superseded,
}

/// Retry a write-back step that failed under descriptor or memory pressure.
pub(super) fn resource_pressure(error: &AdminError) -> bool {
    chain(Link::Admin(error))
        .map(Cause::of)
        .any(|cause| matches!(cause, Cause::Io(error) if transient_io(error)))
}

/// The I/O failures of a whole file system, which no other path written there escapes.
fn volume_kind(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::StorageFull
            | io::ErrorKind::QuotaExceeded
            | io::ErrorKind::ReadOnlyFilesystem
    )
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
            Self::RootNaming(error) => fmt::Display::fmt(error, f),
            Self::RescanStopped(stopped) => {
                write!(
                    f,
                    "Explicit rescan stopped synchronization: {:?}",
                    stopped.reason
                )
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
        self.typed_source().map(Link::as_error)
    }
}

impl TypedSource for SyncError {
    fn typed_source(&self) -> Option<Link<'_>> {
        match self {
            Self::Admin(error) => Some(Link::Admin(error)),
            Self::Root(error) => Some(Link::Root(error)),
            Self::Writeback(error) => Some(Link::Writeback(error)),
            Self::Conflict(error) => Some(Link::Conflict(error)),
            Self::Io(error) => Some(Link::Io(error)),
            Self::Recovery(stop) => Some(Link::Stop(stop)),
            // The probe's own text is this error's; its I/O failure is the next link.
            Self::RootNaming(error) => error.typed_source(),
            Self::Panicked(error) => Some(Link::Leaf(error)),
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

impl From<ProbeError> for SyncError {
    fn from(error: ProbeError) -> Self {
        Self::RootNaming(error)
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
            | SyncError::RootNaming(_)
            | SyncError::RescanStopped(_)
            | SyncError::WritebackUnfinished(_)
            | SyncError::Panicked(_)
            | SyncError::Invariant(_) => Self::other(error),
        }
    }
}
