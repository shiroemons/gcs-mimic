//! What the working tree holds at a path below the root, observed in one place for every pass
//! that asks: the export reading its target, write-back checking the target it installs at, the
//! deletion inventory looking for a tracked file, ingestion opening the file it stages, and the
//! conflict store capturing the file at a conflicting path. Each walks the parents without
//! following a link, opens the entry under its exact spelling, and, where the entry was reached
//! under another one, names the spelling the directory lists it under.

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
    /// Nothing, as far down as [`Above`] tells, with the lookup's proof that the first name
    /// missing on the path holds nothing. Only a lookup makes that proof, so no failed read
    /// passes for an empty path, which the deletion inventory takes for a deleted file.
    Absent {
        above: Above,
        vacant: fsutil::Vacant,
    },
    /// The path's own form, or what holds a directory above it, keeps it from holding a file,
    /// for the reason given.
    Unplaced(Unplaced),
    /// The regular file under the path's exact spelling, and the directory that holds it.
    File { parent: File, file: File },
    /// A regular file that the name reached under another spelling of it, with the spelling its
    /// directory lists it under, or none where the listing named none or failed.
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
    /// entry ([`fsutil::Opened::Transient`]), or went while the kind of entry was read: what the
    /// path holds is unknown until a later read.
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
        Err(ParentError::Missing(vacant)) => {
            return PathEntry::Absent {
                above: Above::Missing,
                vacant,
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
        // The name reached the file under another spelling whichever one the listing would have
        // named, so a failed listing leaves an alias without its spelling, not a failed read.
        Ok(RegularChild::Respelled(Err(_))) => PathEntry::Respelled(ListedSpelling::Unnamed),
        Ok(RegularChild::Missing(vacant)) => PathEntry::Absent {
            above: Above::Present,
            vacant,
        },
        Ok(RegularChild::Transient) => PathEntry::Transient,
        // `EINVAL` is both a refused name and an error of the kind of entry; the refusal decides,
        // in this one place.
        Err(error) if !name_rejected(&error) && tree::structural_entry_error(&error) => {
            match lookup.directory_unconfirmed(&directory, name) {
                Ok(fsutil::Opened::Entry(_)) => PathEntry::Directory(error),
                // The entry that failed to open as a file is gone or kept failing to open as a
                // directory: the path changed while it was read, and a later read decides.
                Ok(fsutil::Opened::Missing(_) | fsutil::Opened::Transient) => PathEntry::Transient,
                Err(_) => PathEntry::Special(error),
            }
        }
        Err(error) => unreadable(error),
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
