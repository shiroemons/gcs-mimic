//! Makes an ingested file's directory entry durable before the catalog tracks it.
//!
//! Deletion detection reads a tracked path that holds nothing as the user's deletion, so a
//! tracking row must never outlive its file's directory entry. A file the user created without
//! syncing it can lose its entry, and the entries of new directories above it, in a power loss,
//! and the catalog's commit does not order them. Ingestion therefore syncs the file and each
//! directory from its parent up to the root before it records the file, as write-back does
//! before it records an exported file.

use crate::fsutil::{self, FileIdentity};
use std::{
    collections::HashSet,
    fs::File,
    io,
    path::{Path, PathBuf},
};

/// The directories, as paths below the root, that a sync made durable after the current scan
/// page was listed. Each sync covers every entry the listing observed in that directory, so later
/// files of the same page skip it. A later page may observe entries made after those syncs, so
/// each page starts with an empty set.
#[derive(Debug, Default)]
pub(super) struct SyncedDirectories(HashSet<PathBuf>);

/// Why ingestion could not make a file durable. It records nothing for the file this pass, and
/// the next scan tries again.
#[derive(Debug)]
pub(super) enum NotDurable {
    /// The path no longer names the file the scan observed.
    Changed,
    /// Opening or syncing the file or a directory above it failed.
    Io(io::Error),
}

impl From<io::Error> for NotDurable {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<fsutil::FsError> for NotDurable {
    fn from(error: fsutil::FsError) -> Self {
        Self::Io(error.into())
    }
}

impl std::fmt::Display for NotDurable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Changed => formatter.write_str("the file changed before it was made durable"),
            Self::Io(_) => formatter.write_str("the file or a directory above it was not synced"),
        }
    }
}

impl std::error::Error for NotDurable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Changed => None,
            Self::Io(error) => Some(error),
        }
    }
}

/// Syncs the file at `path` below `root`, which must still have the identity `expected`, and
/// then each directory from its parent up to the root that `synced` does not hold yet.
pub(super) fn make_durable(
    root: &File,
    path: &Path,
    expected: FileIdentity,
    synced: &mut SyncedDirectories,
) -> Result<(), NotDurable> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(NotDurable::Changed);
    };
    let mut directories = vec![(PathBuf::new(), root.try_clone()?)];
    for component in parent {
        let Some((above, directory)) = directories.last() else {
            return Err(NotDurable::Changed);
        };
        let fsutil::Child::Directory { file, .. } =
            fsutil::child_directory(directory, component, fsutil::DirectoryOpen::Existing)?
        else {
            return Err(NotDurable::Changed);
        };
        let below = above.join(component);
        directories.push((below, file));
    }
    let Some((_, directory)) = directories.last() else {
        return Err(NotDurable::Changed);
    };
    let fsutil::RegularChild::File(file) = fsutil::child_regular(directory, name)? else {
        return Err(NotDurable::Changed);
    };
    if fsutil::stat_identity(&file)? != expected {
        return Err(NotDurable::Changed);
    }
    sync(root, &file)?;
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    crate::storage::fault::file_synced_at(
        &crate::storage::fault::descriptor_path(&file)?,
        &file.metadata()?,
    );
    for (below, directory) in directories.iter().rev() {
        if synced.0.contains(below) {
            continue;
        }
        sync(root, directory)?;
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        crate::storage::fault::dir_synced(&crate::storage::fault::descriptor_path(directory)?);
        synced.0.insert(below.clone());
    }
    Ok(())
}

/// Orders what `file` holds, or the entries of a directory, before the catalog commit that
/// records the file, as `fsutil::sync_ordered` does for the catalog's own blobs.
fn sync(root: &File, file: &File) -> io::Result<()> {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    crate::storage::fault::hit_io(
        &crate::storage::fault::descriptor_path(root)?,
        crate::storage::fault::Step::SyncSourceSync,
    )?;
    #[cfg(not(all(test, any(target_os = "linux", target_os = "macos"))))]
    let _ = root;
    #[cfg(unix)]
    {
        fsutil::sync_ordered(file)
    }
    #[cfg(not(unix))]
    {
        file.sync_all()
    }
}
