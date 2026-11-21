//! The one classification of a synchronization failure. The retry, pause, deferral and
//! plan-again decisions read the class it gives a failure instead of matching error variants of
//! their own, which had drifted apart: one place deferred what another failed the pass on.

use crate::{
    bucket::CatalogError,
    storage::{
        admin::AdminError,
        failure::{Link, chain},
        sync_state::{
            root::RootError,
            writeback::{self, RaceReason},
        },
    },
};
use std::io;

#[cfg(test)]
mod tests;

/// What a failure says about the step that met it. A failure with no class, such as a missing
/// permission, a corrupt catalog or a write-back stop, needs an operator, and the decision that
/// reads it says how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) enum Failure {
    /// A concurrent change made the step moot: the object, its generation or its bucket changed
    /// after the step read them, or the tree no longer holds what the step read there. The
    /// journal or the next pass brings that change, so the step is planned again, not failed.
    Superseded,
    /// The whole file system's: a full disk, an exhausted quota or a read-only file system fails
    /// every later write there as well, so no one path is to blame.
    Volume,
    /// It passes on its own: catalog contention, an admission timeout, an interrupted or
    /// timed-out call, descriptor or memory pressure, or an entry that kept failing to open
    /// while its name still held it.
    Transient,
}

impl Failure {
    /// What an I/O failure's kind alone says.
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "io::ErrorKind is non-exhaustive, and a kind this match does not name has no class"
    )]
    pub(in crate::sync) fn of_kind(kind: io::ErrorKind) -> Option<Self> {
        match kind {
            io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::Interrupted
            | io::ErrorKind::ResourceBusy => Some(Self::Transient),
            io::ErrorKind::StorageFull
            | io::ErrorKind::QuotaExceeded
            | io::ErrorKind::ReadOnlyFilesystem => Some(Self::Volume),
            _ => None,
        }
    }

    /// What an I/O failure says, with exhausted descriptors or memory, which its kind does not
    /// name, as transient.
    pub(in crate::sync) fn of_io(error: &io::Error) -> Option<Self> {
        Self::of_kind(error.kind()).or_else(|| {
            matches!(
                error.raw_os_error(),
                Some(libc::EMFILE | libc::ENFILE | libc::EAGAIN | libc::ENOMEM)
            )
            .then_some(Self::Transient)
        })
    }

    /// What one admin failure says by itself; an I/O failure below it is its own link.
    pub(in crate::sync) fn of_admin(error: &AdminError) -> Option<Self> {
        match error {
            AdminError::Busy { .. } => Some(Self::Transient),
            AdminError::DiskFull(_) => Some(Self::Volume),
            AdminError::Precondition | AdminError::NotFound => Some(Self::Superseded),
            AdminError::Io(cause) => Self::of_kind(cause.kind()),
            AdminError::Closed
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
            | AdminError::Backend(_) => None,
        }
    }

    /// What a catalog failure says: an unavailable catalog comes back.
    pub(in crate::sync) fn of_catalog(error: &CatalogError) -> Option<Self> {
        matches!(error, CatalogError::Unavailable(_)).then_some(Self::Transient)
    }

    /// What a SQLite failure says: a busy or locked database frees up.
    pub(in crate::sync) fn of_sqlite(error: &rusqlite::Error) -> Option<Self> {
        matches!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
        )
        .then_some(Self::Transient)
    }

    /// What a catalog write about one name says, as the root reports it.
    pub(in crate::sync) fn of_root(error: &RootError) -> Option<Self> {
        match error {
            RootError::Admin(error) => Self::of_admin(error),
            RootError::InUse
            | RootError::Unsupported { .. }
            | RootError::RootChanged
            | RootError::MissingMarker
            | RootError::InvalidMarker
            | RootError::ForeignMarker
            | RootError::ReinitializeDenied
            | RootError::ReplacementOccupied { .. } => None,
        }
    }

    /// What a write-back step's failure says. The root's own failures and write-back stops have
    /// no class here: recovery or an operator settles them.
    pub(in crate::sync) fn of_writeback(error: &writeback::Error) -> Option<Self> {
        match error {
            // The step met an entry it was to create, which the user made meanwhile.
            writeback::Error::Admin(AdminError::Io(cause))
                if cause.kind() == io::ErrorKind::AlreadyExists =>
            {
                Some(Self::Superseded)
            }
            writeback::Error::Admin(error) => Self::of_admin(error),
            writeback::Error::Raced(RaceReason::Unsettled) => Some(Self::Transient),
            writeback::Error::Raced(
                RaceReason::ParentChanged
                | RaceReason::Respelled
                | RaceReason::TargetChanged
                | RaceReason::DirectoryChanged,
            )
            | writeback::Error::Obsolete(_)
            // A value an earlier step checked broke this step's assumption. Its caller records
            // the break and plans the step again, as after a concurrent change.
            | writeback::Error::Invariant(_) => Some(Self::Superseded),
            writeback::Error::Root(_)
            | writeback::Error::Conflict(_)
            | writeback::Error::Displaced(_)
            | writeback::Error::Capacity
            | writeback::Error::RecoveryRequired
            | writeback::Error::Protected
            | writeback::Error::WorkspaceChanged => None,
        }
    }
}

/// Whether descriptor or memory pressure, or another transient I/O failure, is in `error`'s
/// chain, where the admin failure's own kind does not show it.
pub(in crate::sync) fn transient_below(error: &AdminError) -> bool {
    chain(Link::Admin(error)).any(
        |link| matches!(link, Link::Io(error) if Failure::of_io(error) == Some(Failure::Transient)),
    )
}

/// Whether a catalog write about one name failed because an API or bucket change made it moot
/// after the caller read the state it describes: the object, its generation or its bucket
/// changed. The journal brings that change to the next replay or pass, which records what then
/// holds; failing would pause the whole mirror over a race it resolves by itself.
pub(in crate::sync) fn superseded(error: &RootError) -> bool {
    Failure::of_root(error) == Some(Failure::Superseded)
}

/// The result of a catalog write about one name, with a write that a concurrent change made
/// moot, as [`superseded`] decides, as done: whether it was recorded or is no longer needed.
pub(in crate::sync) fn unless_superseded(
    result: Result<(), RootError>,
) -> Result<Recorded, RootError> {
    match result {
        Ok(()) => Ok(Recorded::Yes),
        Err(error) if superseded(&error) => Ok(Recorded::Superseded),
        Err(error) => Err(error),
    }
}

/// Whether a catalog write about one name was recorded or a concurrent change made it moot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) enum Recorded {
    Yes,
    Superseded,
}
