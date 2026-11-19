//! Makes a file in the working tree durable, together with every directory entry above it,
//! before the catalog tracks it.
//!
//! Deletion detection reads a tracked path that holds nothing as the user's deletion, so a
//! tracking row must never outlive its file's directory entry. A file the user created without
//! syncing it can lose its entry, and the entries of new directories above it, in a power loss,
//! and the catalog's commit does not order them. Every path that records a tracking row, the
//! ingestion of a file and the write-back that installs or observes one alike, therefore syncs
//! the file and each directory from its parent up to the root first. [`Durable`] is the proof,
//! and [`super::SyncUpdate::new`] takes it, so a path that skips the syncs does not compile.

use super::SyncPath;
use crate::fsutil::{self, FileIdentity};
use std::{
    collections::HashSet,
    fs::File,
    io,
    path::{Path, PathBuf},
};

/// Proof that the file at a path, and every directory from its parent up to the root, were
/// synced after the file was observed there. Only [`make_durable`] gives one.
#[derive(Debug)]
pub(crate) struct Durable(SyncPath);

impl Durable {
    /// A proof for a test that records rows without a working tree to sync.
    #[cfg(test)]
    pub(crate) fn assumed(path: SyncPath) -> Self {
        Self(path)
    }

    /// The path this proof covers.
    pub(crate) fn path(&self) -> &SyncPath {
        &self.0
    }

    pub(super) fn into_path(self) -> SyncPath {
        self.0
    }
}

/// The directories, as paths below the root, that a sync made durable after the entries a
/// caller acts on were observed. Each sync covers every entry observed in that directory before
/// it, so later files of the same observation skip it. Entries observed later, such as those of
/// the next scan page, may have been made after those syncs, so each observation starts with an
/// empty set.
#[derive(Debug, Default)]
pub(crate) struct SyncedDirectories(HashSet<PathBuf>);

/// Why a file could not be made durable. The caller records nothing for it this time, and the
/// next scan tries again.
#[derive(Debug)]
pub(crate) enum NotDurable {
    /// The path no longer names the file that was observed, or another kind of entry took the
    /// place of the file or of a directory above it.
    Changed,
    /// Opening or syncing the file or a directory above it failed.
    Io(io::Error),
}

impl From<io::Error> for NotDurable {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The result of looking up an entry on the path. What took the place of an expected file or
/// directory, such as a symlink or a file where a directory was, is a change of the path rather
/// than a failure to read it, whose remedy would wrongly advise restoring permissions.
fn opened<T>(result: io::Result<T>) -> Result<T, NotDurable> {
    result.map_err(|error| {
        if crate::sync::tree::structural_entry_error(&error) {
            NotDurable::Changed
        } else {
            NotDurable::Io(error)
        }
    })
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
pub(crate) fn make_durable(
    root: &File,
    lookup: &fsutil::Lookup,
    path: &SyncPath,
    expected: FileIdentity,
    synced: &mut SyncedDirectories,
) -> Result<Durable, NotDurable> {
    let relative: &Path = path.as_ref();
    let (Some(parent), Some(name)) = (relative.parent(), relative.file_name()) else {
        return Err(NotDurable::Changed);
    };
    let mut directories = vec![(PathBuf::new(), root.try_clone()?)];
    for component in parent {
        let Some((above, directory)) = directories.last() else {
            return Err(NotDurable::Changed);
        };
        let fsutil::Child::Directory { file, .. } =
            opened(lookup.directory(directory, component, fsutil::DirectoryOpen::Existing))?
        else {
            return Err(NotDurable::Changed);
        };
        let below = above.join(component);
        directories.push((below, file));
    }
    let Some((_, directory)) = directories.last() else {
        return Err(NotDurable::Changed);
    };
    let fsutil::RegularChild::File(file) = opened(lookup.regular(directory, name))? else {
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
    Ok(Durable(path.clone()))
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
