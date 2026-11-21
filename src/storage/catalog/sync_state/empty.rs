//! What counts as an empty directory for an export that replaces a directory at its target: one
//! that holds nothing, or nothing but Finder's metadata files, which the export removes. Startup,
//! which takes a directory without a marker in place of the synchronized one, has a looser
//! definition of its own (`root::switch`) because it removes nothing. The two stay apart so that
//! loosening the switch never lets an export remove more.

use crate::fsutil;
use std::{fs::File, io};

/// The names of the only entries a directory may hold and still count as empty, when they are
/// regular files: Finder's `.DS_Store`, which it writes into every directory it shows, and the
/// AppleDouble `._.DS_Store` that holds that file's extended attributes on a volume that cannot
/// store them. Each is a name that scans ignore, so none of them would become an object. Other
/// names scans ignore, such as any other `._*` or an editor's swap file, may hold a user's data,
/// and a directory that holds one is not empty.
const FINDER_METADATA: [&str; 2] = [".DS_Store", "._.DS_Store"];

/// The most names [`contents`] reads. Any name but a Finder metadata file makes a directory hold
/// something, so one that lists more is never empty, and the bound keeps a large directory from
/// costing a full listing.
const LIST_LIMIT: usize = 8;

/// A Finder metadata file that an empty directory holds.
pub(super) struct MetadataFile {
    /// Its name, one of [`FINDER_METADATA`].
    pub(super) name: &'static str,
    /// The file as [`contents`] opened it, so that a later step can tell it from a file written
    /// under the same name since.
    pub(super) identity: fsutil::FileIdentity,
}

/// What a directory holds, as far as counting it empty goes.
pub(super) enum Contents {
    /// Nothing but these Finder metadata files, which may be none.
    Empty(Vec<MetadataFile>),
    /// An entry other than a regular Finder metadata file, or more names than are read.
    Holds,
    /// Listing the directory failed with this error, so what it holds is unknown.
    Unlisted(io::Error),
}

/// Lists `directory`, never below it and at most [`LIST_LIMIT`] names, and opens each
/// [`FINDER_METADATA`] name without following a symlink. A symlink, a directory or a special
/// file under such a name is content like any other entry, and a listing that fails leaves what
/// the directory holds unknown.
pub(super) fn contents(directory: &File, lookup: &fsutil::Lookup) -> io::Result<Contents> {
    let mut names = fsutil::DirectoryNames::default();
    let mut metadata = Vec::new();
    loop {
        let listed = match names.next_name(directory, LIST_LIMIT) {
            Ok(Some(listed)) => listed,
            Ok(None) => return Ok(Contents::Empty(metadata)),
            // More names than are read.
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                return Ok(Contents::Holds);
            }
            Err(error) => return Ok(Contents::Unlisted(error)),
        };
        let Some(name) = FINDER_METADATA
            .into_iter()
            .find(|metadata| listed == **metadata)
        else {
            return Ok(Contents::Holds);
        };
        match lookup.regular(directory, name.as_ref()) {
            Ok(fsutil::RegularChild::File(file)) => metadata.push(MetadataFile {
                name,
                identity: fsutil::stat_identity(&file)?,
            }),
            // Gone since the listing: nothing of it is left.
            Ok(fsutil::RegularChild::Missing(_)) => {}
            // An entry that kept failing to open may hold anything.
            Ok(fsutil::RegularChild::Respelled(_) | fsutil::RegularChild::Unopenable) => {
                return Ok(Contents::Holds);
            }
            Err(error) if crate::sync::tree::structural_entry_error(&error) => {
                return Ok(Contents::Holds);
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
