//! What an export's target name holds, read once through the observation that ingestion, the
//! deletion inventory and the export planner share.

use super::{Error, RaceReason};
use crate::{
    fsutil,
    sync::{
        correspond::{Above, PathEntry, observe_path},
        naming::RootNaming,
    },
};
use std::{fs::File, io, path::Path};

/// What holds an export's target name.
#[derive(Debug)]
pub(super) enum TargetState {
    /// A directory above the target is missing, so the target is too.
    ParentMissing,
    /// The directories above the target are there, and nothing holds its name.
    Absent,
    /// The regular file under the target's exact spelling.
    File(File),
    /// A directory holds the name, which failed to open as a file with this error.
    Directory(io::Error),
    /// A symlink or special file holds the name, which failed to open as a file with this error.
    Unsafe(io::Error),
    /// The path no longer leads where the step read it, for this reason.
    Changed(RaceReason),
}

/// Reads what holds `path`'s name below `root`, without following a link.
pub(super) fn observe_target(
    root: &File,
    naming: RootNaming,
    lookup: &fsutil::Lookup,
    path: &str,
) -> Result<TargetState, Error> {
    super::parent_path(path)?;
    Ok(match observe_path(root, naming, lookup, Path::new(path)) {
        PathEntry::Absent {
            above: Above::Missing,
            ..
        } => TargetState::ParentMissing,
        PathEntry::Absent {
            above: Above::Present,
            ..
        } => TargetState::Absent,
        PathEntry::File { file, .. } => TargetState::File(file),
        PathEntry::Directory(error) => TargetState::Directory(error),
        PathEntry::Special(error) => TargetState::Unsafe(error),
        PathEntry::Unplaced(_) => TargetState::Changed(RaceReason::ParentChanged),
        PathEntry::Respelled(_) => TargetState::Changed(RaceReason::Respelled),
        // Read again by the export the race leads to.
        PathEntry::Transient => TargetState::Changed(RaceReason::Unsettled),
        PathEntry::NameRejected(error) | PathEntry::Unreadable(error) => return Err(error.into()),
    })
}

impl TargetState {
    /// The regular file at the target, if any. Any other entry at the name fails the step: one
    /// that is no regular file with the error it failed to open with, a changed path as a race.
    pub(super) fn into_file(self) -> Result<Option<File>, Error> {
        match self {
            Self::ParentMissing | Self::Absent => Ok(None),
            Self::File(file) => Ok(Some(file)),
            Self::Directory(error) | Self::Unsafe(error) => Err(error.into()),
            Self::Changed(reason) => Err(Error::Raced(reason)),
        }
    }
}
