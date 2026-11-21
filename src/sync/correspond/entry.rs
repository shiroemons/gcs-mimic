//! What the working tree holds at a path below the root, observed in one place for every pass
//! that asks: the export reading its target, the deletion inventory looking for a tracked file,
//! and ingestion opening the file it stages. Each walks the parents without following a link,
//! opens the entry under its exact spelling, and, where the entry was reached under another one,
//! names the spelling the directory lists it under.

use crate::{
    fsutil::{self, ListedSpelling, Lookup, RegularChild},
    sync::{
        mapping::{LexicalReason, Unplaced},
        naming::RootNaming,
        tree::{self, ParentError, ParentOpen},
    },
};
use std::{fs::File, io, path::Path};

/// What a path below the root holds.
#[derive(Debug)]
pub(crate) enum PathEntry {
    /// Nothing, as far down as [`Above`] tells.
    Absent { above: Above },
    /// The path's own form, or what holds a directory above it, keeps it from holding a file,
    /// for the reason given.
    Unplaced(Unplaced),
    /// The regular file under the path's exact spelling, and the directory that holds it.
    File { parent: File, file: File },
    /// A regular file that the name reached under another spelling of it.
    Respelled(ListedSpelling),
    /// A directory holds the name, which failed to open as a regular file with this error.
    Directory(io::Error),
    /// A symlink or special file holds the name, which failed to open as a regular file with
    /// this error.
    Special(io::Error),
    /// The file system refused the name of the entry or of a directory above it.
    NameRejected(io::Error),
    /// Reading a directory above the entry, or the entry itself, failed.
    Unreadable(io::Error),
    /// The entry, or a directory above it, kept failing to open while its name still held an
    /// entry ([`fsutil::Opened::Transient`]): what the path holds is unknown until a later read.
    Transient,
}

/// How far down the path of an absent entry is there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Above {
    /// Every directory above the entry is there; only the entry is not.
    Present,
    /// A directory above the entry is missing.
    Missing,
}

/// Looks at `path` below `root` without following any link, through the lookups of the pass
/// that asks.
pub(crate) fn observe_path(
    root: &File,
    naming: RootNaming,
    lookup: &Lookup,
    path: &Path,
) -> PathEntry {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return PathEntry::Unplaced(LexicalReason::InvalidName.into());
    };
    let directory = match tree::parent(root, naming, lookup, parent, ParentOpen::Existing) {
        Ok(directory) => directory,
        Err(ParentError::Missing) => {
            return PathEntry::Absent {
                above: Above::Missing,
            };
        }
        Err(ParentError::Finding(reason)) => return PathEntry::Unplaced(reason),
        Err(ParentError::Transient) => return PathEntry::Transient,
        Err(ParentError::Io(error)) => return unreadable(error),
    };
    match lookup.regular(&directory, name) {
        Ok(RegularChild::File(file)) => PathEntry::File {
            parent: directory,
            file,
        },
        Ok(RegularChild::Respelled(Ok(spelling))) => PathEntry::Respelled(spelling),
        Ok(RegularChild::Missing(_)) => PathEntry::Absent {
            above: Above::Present,
        },
        Ok(RegularChild::Transient) => PathEntry::Transient,
        Err(error) if tree::structural_entry_error(&error) => {
            if fsutil::directory_at(&directory, name, fsutil::DirectoryOpen::Existing).is_ok() {
                PathEntry::Directory(error)
            } else {
                PathEntry::Special(error)
            }
        }
        Ok(RegularChild::Respelled(Err(error))) | Err(error) => unreadable(error),
    }
}

/// A failed read of the path that is not about the kind of entry it holds: the file system
/// refused a name, or reading failed.
fn unreadable(error: io::Error) -> PathEntry {
    if name_rejected(&error) {
        PathEntry::NameRejected(error)
    } else {
        PathEntry::Unreadable(error)
    }
}

/// Whether the file system refused a name itself (`ENAMETOOLONG`, `EILSEQ` or `EINVAL`).
pub(crate) fn name_rejected(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::InvalidFilename
        || matches!(error.raw_os_error(), Some(libc::EILSEQ | libc::EINVAL))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
