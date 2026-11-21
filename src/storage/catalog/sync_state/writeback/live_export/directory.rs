//! Removing the empty directory that holds an export's target name.
//!
//! Deleting `b/a/x.txt` through the API leaves the directory `b/a/` empty, and an object `b/a`
//! created later has no place in the tree while that directory stays. The export removes such a
//! directory itself rather than leave the operator to, under a proof built in two halves: the
//! catalog records nothing at or below the target ([`unreferenced`]), and the tree holds a
//! directory there under its exact spelling, below its bucket's directory and outside the names
//! the root reserves ([`RemovableDirectory::prove`]). [`remove`] takes that proof.
//!
//! Only Finder's metadata files are ever removed from the directory: a regular `.DS_Store` that
//! Finder writes into every directory it shows, and the `._.DS_Store` that holds its extended
//! attributes on a volume that cannot store them, which would otherwise keep a directory holding
//! nothing of the user's. The proof lists the directory, never below it and only a few names, to
//! find whether that is all it holds ([`empty::contents`]); any other entry, or a listing that
//! fails, keeps the directory with everything in it. The listing is also what keeps AppleDouble
//! `._*` files: macOS's `rmdir` removes a directory that holds only `._*` files of names it does
//! not hold, and those files with it, so `rmdir` alone would not refuse one.
//!
//! An entry can arrive at any time, so [`remove`] checks just before `rmdir` that the name still
//! holds the proved directory and lists that directory once more ([`unchanged`]). `rmdir` never
//! follows a symlink and refuses a directory that holds an entry that arrived after that listing,
//! which is left as a collision, with one exception no check can close: on macOS, an AppleDouble
//! `._*` file created between that listing and `rmdir` goes with the directory. No ancestor that
//! a removal empties is removed after it. On a root that folds names the directory that holds the
//! target is listed too, to confirm the spelling ([`fsutil::Lookup`]).

use super::*;
use crate::storage::catalog::sync_state::empty::{self, Contents, MetadataFile};
use std::ffi::OsString;

/// Why an export leaves the directory at its target in place, which keeps it a collision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kept {
    /// The export expects a file at the target: it replaces a tracked body, restores a protected
    /// one or follows a preserved conflict.
    Replaces,
    /// A tracked file is at or below the target.
    Tracked,
    /// A path stop names the target's object or one below it.
    PathStop,
    /// Another unfinished write-back operation targets a path below the target.
    Pending,
    /// A retained conflict belongs to the target's object or one below it.
    Conflict,
    /// The path is no object's below its bucket: a bucket's own directory, or a reserved name.
    NotAnObject,
    /// The entry is no directory under its exact spelling: a symlink, a file or an alias.
    NotDirectory,
    /// The directory lists an entry other than a regular Finder metadata file, or more names
    /// than a proof reads.
    Holds,
    /// Listing the directory failed, so what it holds is unknown. The error's kind and its
    /// system error code, which the warning reports, keep this value `Copy`.
    Unlisted {
        kind: io::ErrorKind,
        os: Option<i32>,
    },
    /// The filesystem refused the removal.
    Refused(Refusal),
}

/// Why the filesystem refused to remove the directory at an export's target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The directory holds an entry (`ENOTEMPTY`, or `EEXIST` where the system reports that).
    NotEmpty,
    /// The directory is in use, such as a mount point (`EBUSY`).
    Busy,
    /// The directory may not be removed (`EACCES`, `EPERM`).
    PermissionDenied,
    /// The entry became something other than a directory after the proof read it (`ENOTDIR`).
    NotADirectory,
}

impl Refusal {
    /// The refusal `error` reports; `None` for an error that is no refusal, which fails the
    /// attempt for a retry instead of keeping the directory.
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "io::ErrorKind is non-exhaustive, and every kind it adds is no refusal"
    )]
    fn of(error: &io::Error) -> Option<Self> {
        match error.kind() {
            io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::AlreadyExists => Some(Self::NotEmpty),
            io::ErrorKind::ResourceBusy => Some(Self::Busy),
            io::ErrorKind::PermissionDenied => Some(Self::PermissionDenied),
            io::ErrorKind::NotADirectory => Some(Self::NotADirectory),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::NotEmpty => "not_empty",
            Self::Busy => "busy",
            Self::PermissionDenied => "permission_denied",
            Self::NotADirectory => "not_a_directory",
        }
    }
}

impl Kept {
    fn as_str(self) -> &'static str {
        match self {
            Self::Replaces => "replaces",
            Self::Tracked => "tracked",
            Self::PathStop => "path_stop",
            Self::Pending => "pending",
            Self::Conflict => "conflict",
            Self::NotAnObject => "not_an_object",
            Self::NotDirectory => "not_directory",
            // The operator frees the name the same way as when the removal itself finds an
            // entry: by moving what the directory holds out of the tree.
            Self::Holds => "not_empty",
            Self::Unlisted { .. } => "unlisted",
            Self::Refused(refusal) => refusal.as_str(),
        }
    }

    /// What frees the name, as the finding [`Self::finding`] records gives it.
    fn remedy(self) -> String {
        match self {
            Self::NotDirectory => {
                crate::sync::report::remedy::Correction::UnrepresentableName.remedy()
            }
            Self::Replaces
            | Self::Tracked
            | Self::PathStop
            | Self::Pending
            | Self::Conflict
            | Self::NotAnObject
            | Self::Holds
            | Self::Unlisted { .. }
            | Self::Refused(_) => crate::sync::report::remedy::occupied_directory_remedy(),
        }
    }

    /// The finding that keeps the object out of the tree. An entry under another spelling, or
    /// one that is no directory, holds the name as any other collision does; otherwise the
    /// directory is in the way, and what is in it decides when it can go.
    pub(super) fn finding(self) -> mapping::Obstruction {
        match self {
            Self::NotDirectory => mapping::Obstruction::Collision,
            Self::Replaces
            | Self::Tracked
            | Self::PathStop
            | Self::Pending
            | Self::Conflict
            | Self::NotAnObject
            | Self::Holds
            | Self::Unlisted { .. }
            | Self::Refused(_) => mapping::Obstruction::DirectoryOccupied,
        }
    }

    /// Warns that the directory at `item`'s target stays and keeps the object out of the tree:
    /// the finding the mirror records names the object, and this also says which check kept
    /// the directory, so an operator knows what to change.
    pub(super) fn report(self, item: &Pending) {
        let error = match self {
            Self::Unlisted {
                kind,
                os: Some(code),
            } => Some(format!("{kind} (os error {code})")),
            Self::Unlisted { kind, os: None } => Some(kind.to_string()),
            Self::Replaces
            | Self::Tracked
            | Self::PathStop
            | Self::Pending
            | Self::Conflict
            | Self::NotAnObject
            | Self::NotDirectory
            | Self::Holds
            | Self::Refused(_) => None,
        };
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            "sync_directory_kept",
            serde_json::json!({
                "bucket": item.bucket.as_str(),
                "object": item.name.as_str(),
                "path": item.path,
                "reason": self.as_str(),
                "error": error,
                "remedy": crate::sync::report::command::logged(&self.remedy()),
            }),
        );
    }
}

/// The catalog's half of the proof that an export may remove the directory at its target: no
/// tracked file, path stop, other pending operation or retained conflict is at or below the
/// target. Only [`unreferenced`] makes one.
pub(super) struct Unreferenced {
    /// The export's target path, `bucket/object`.
    path: String,
}

/// The half-open range `[name/, name0)` of the names below `name`. `0` is the byte after `/`, so
/// exactly the names that start with `name/` sort into it under the binary collation: `a` never
/// takes in `ab` or `a.txt`, and no `LIKE` pattern needs escaping.
fn below(name: &str) -> (String, String) {
    (format!("{name}/"), format!("{name}0"))
}

/// Reads whether anything the catalog records refers to `item`'s target or a path below it, and
/// returns the catalog's half of the removal proof when nothing does. Only an ordinary export
/// (`purpose`) that replaces no `tracked` body may remove a directory: a tracked body, a restore
/// into an absent target and a preserved conflict each expect a file there. Letter case and
/// Unicode aliases on a folding root are not matched: names compare byte for byte.
pub(super) fn unreferenced(
    c: &Connection,
    item: &Pending,
    tracked: Option<&SyncRecord>,
    purpose: &Purpose,
) -> Result<Result<Unreferenced, Kept>, Error> {
    if tracked.is_some() || !matches!(purpose, Purpose::Ordinary) {
        return Ok(Err(Kept::Replaces));
    }
    let name = item.name.as_str();
    let (first, last) = below(name);
    let tracked: bool = c
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_objects WHERE bucket=?1 AND name=?2) OR EXISTS(SELECT 1 FROM sync_objects WHERE bucket=?1 AND name>=?3 AND name<?4)")?
        .query_row(params![item.bucket, name, first, last], |r| r.get(0))?;
    if tracked {
        return Ok(Err(Kept::Tracked));
    }
    let root = item.token.root;
    let stopped: bool = c
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_path_stops WHERE root_id=?1 AND bucket=?2 AND name=?3) OR EXISTS(SELECT 1 FROM sync_path_stops WHERE root_id=?1 AND bucket=?2 AND name>=?4 AND name<?5)")?
        .query_row(params![root, item.bucket, name, first, last], |r| r.get(0))?;
    if stopped {
        return Ok(Err(Kept::PathStop));
    }
    // The unique index on the paths of unfinished operations leaves no other one at the target.
    let (first_path, last_path) = below(&item.path);
    let pending: bool = c
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_writeback_ops WHERE root_id=?1 AND phase!=4 AND path>=?2 AND path<?3)")?
        .query_row(params![root, first_path, last_path], |r| r.get(0))?;
    if pending {
        return Ok(Err(Kept::Pending));
    }
    let retained: bool = c
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_conflicts WHERE root_id=?1 AND state!=2 AND bucket=?2 AND (name=?3 OR (name>=?4 AND name<?5)))")?
        .query_row(params![root, item.bucket, name, first, last], |r| r.get(0))?;
    if retained {
        return Ok(Err(Kept::Conflict));
    }
    Ok(Ok(Unreferenced {
        path: item.path.clone(),
    }))
}

/// Proof that an export may remove the directory at its target if the directory is empty: the
/// catalog's half ([`Unreferenced`]) completed with what the tree holds. Only
/// [`RemovableDirectory::prove`] makes one, and [`remove`] needs one.
pub(super) struct RemovableDirectory {
    /// The target path the proof answers for, `bucket/object`.
    path: String,
    /// The directory the proof read the entry in. The removal names the entry in it, and
    /// installation renames the copy into it.
    parent: File,
    /// The directory's name in `parent`.
    name: OsString,
    /// The directory itself, opened without following a symlink, which its metadata files are
    /// unlinked from.
    directory: File,
    /// The Finder metadata files the directory held, and nothing else. The removal unlinks each
    /// name only while it still holds the file the proof opened.
    metadata: Vec<MetadataFile>,
}

impl RemovableDirectory {
    /// Completes `catalog` with what the tree under `root` holds at the target. The path must
    /// decode to an object below its bucket's directory, which rules out the bucket's directory
    /// itself as well as the workspace and the marker: a removed bucket directory would let a
    /// directory created later under that name bring a deleted bucket back. The entry must open
    /// as a directory under its exact spelling without following a symlink: on a root that folds
    /// names, `lookup` lists the directory that holds it to confirm that spelling, because a
    /// Linux mount that folds case without the casefold flag, such as a Docker Desktop bind
    /// mount, cannot report it, and removing `photos/` for an object `Photos` would put a file
    /// where the parent of a tracked `photos/a.jpg` was. The directory must hold nothing but
    /// Finder metadata files ([`metadata_only`]).
    pub(super) fn prove(
        root: &File,
        naming: RootNaming,
        lookup: &fsutil::Lookup,
        catalog: Unreferenced,
    ) -> Result<Result<Self, Kept>, Error> {
        let Unreferenced { path } = catalog;
        let relative = std::path::Path::new(&path);
        if mapping::path_to_name(relative, naming).is_err() {
            return Ok(Err(Kept::NotAnObject));
        }
        let Some(name) = relative.file_name().map(std::ffi::OsStr::to_owned) else {
            return Ok(Err(Kept::NotAnObject));
        };
        let parent = parent(root, naming, lookup, &path, ParentSync::Skip)?
            .ok_or(Error::Raced(RaceReason::ParentChanged))?;
        match lookup.directory(&parent, &name, fsutil::DirectoryOpen::Existing) {
            Ok(fsutil::Child::Directory { file, .. }) => {
                Ok(metadata_only(&file, lookup)?.map(|metadata| Self {
                    path,
                    parent,
                    name,
                    directory: file,
                    metadata,
                }))
            }
            Ok(fsutil::Child::Alias | fsutil::Child::Missing(_)) => Ok(Err(Kept::NotDirectory)),
            // A directory that kept failing to open is proved again by the export the race leads
            // to.
            Ok(fsutil::Child::Transient(_)) => Err(Error::Raced(RaceReason::Unsettled)),
            Err(error) if crate::sync::tree::structural_entry_error(&error) => {
                Ok(Err(Kept::NotDirectory))
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// The Finder metadata files that `directory` holds when they are all it holds. A listing that
/// fails keeps the directory: what it holds is unknown until a later pass reads it.
fn metadata_only(
    directory: &File,
    lookup: &fsutil::Lookup,
) -> Result<Result<Vec<MetadataFile>, Kept>, Error> {
    Ok(match empty::contents(directory, lookup)? {
        Contents::Empty(metadata) => Ok(metadata),
        Contents::Holds => Err(Kept::Holds),
        Contents::Unlisted(error) => Err(Kept::Unlisted {
            kind: error.kind(),
            os: error.raw_os_error(),
        }),
    })
}

/// Unlinks the Finder metadata files of `metadata` from `directory` while each name still
/// holds the file the proof opened, without following a symlink, and returns how many it
/// unlinked. A name that holds anything else by now, such as a file Finder wrote again, keeps
/// it, and the removal then finds the directory not empty. Each unlink syncs `directory`, so it
/// is durable before the directory's removal can be.
fn remove_metadata(
    directory: &File,
    metadata: &[MetadataFile],
    install: &Install,
) -> Result<usize, Error> {
    let mut unlinked = 0;
    for file in metadata {
        #[cfg(test)]
        install.faults.hit_io(Step::SyncDirectoryMetadataUnlink)?;
        let current = match install.lookup.regular(directory, file.name.as_ref()) {
            Ok(fsutil::RegularChild::File(current)) => current,
            // A file that kept failing to open stays, and the removal finds the directory not
            // empty.
            Ok(
                fsutil::RegularChild::Missing(_)
                | fsutil::RegularChild::Respelled(_)
                | fsutil::RegularChild::Transient(_),
            ) => continue,
            Err(error) if crate::sync::tree::structural_entry_error(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        if fsutil::stat_identity(&current)? != file.identity {
            continue;
        }
        #[cfg(test)]
        install
            .faults
            .unlinking_metadata(&install.item, file.name, &current)?;
        fsutil::unlink_file_at(directory, file.name.as_ref())?;
        unlinked += 1;
    }
    #[cfg(test)]
    if unlinked > 0 {
        install.faults.metadata_removed(&install.item)?;
    }
    #[cfg(not(test))]
    let _ = install;
    Ok(unlinked)
}

/// Rechecks what the proof read just before the removal: that the name still holds the directory
/// the proof opened, and that the directory holds nothing but Finder metadata files. A name that
/// holds another directory by now fails the attempt, and the retry proves that one; an entry
/// that is no directory keeps the name. A directory gone since the proof passes, and the removal
/// finds it gone.
fn unchanged(
    parent: &File,
    name: &std::ffi::OsStr,
    directory: &File,
    lookup: &fsutil::Lookup,
) -> Result<Result<(), Kept>, Error> {
    match lookup.directory(parent, name, fsutil::DirectoryOpen::Existing) {
        Ok(fsutil::Child::Directory { file, .. }) => {
            if fsutil::stat_identity(&file)?.node() != fsutil::stat_identity(directory)?.node() {
                return Err(Error::Raced(RaceReason::DirectoryChanged));
            }
        }
        Ok(fsutil::Child::Missing(_)) => return Ok(Ok(())),
        Ok(fsutil::Child::Alias) => return Ok(Err(Kept::NotDirectory)),
        // A directory that kept failing to open is proved again by the export the race leads to.
        Ok(fsutil::Child::Transient(_)) => return Err(Error::Raced(RaceReason::Unsettled)),
        Err(error) if crate::sync::tree::structural_entry_error(&error) => {
            return Ok(Err(Kept::NotDirectory));
        }
        Err(error) => return Err(error.into()),
    }
    Ok(metadata_only(directory, lookup)?.map(drop))
}

/// Removes the directory `proof` names once its Finder metadata files are unlinked, if it is
/// empty then, and syncs the directory that held it, so the removal is durable before
/// installation takes the name; returns that directory for installation to rename into. The
/// directory is listed once more first ([`unchanged`]). A refused removal leaves the directory
/// with whatever it holds; a metadata file already unlinked stays unlinked, since the system that
/// wrote it writes it again. A directory already gone counts as removed.
pub(super) fn remove(
    proof: RemovableDirectory,
    install: &Install,
) -> Result<Result<File, Kept>, Error> {
    let RemovableDirectory {
        path,
        parent,
        name,
        directory,
        metadata,
    } = proof;
    // A proof answers for the target it was read at, not for another operation's.
    if path != install.item.path {
        return Err(Error::Invariant("a directory's proof is another target's"));
    }
    let unlinked = remove_metadata(&directory, &metadata, install)?;
    if let Err(kept) = unchanged(&parent, &name, &directory, &install.lookup)? {
        return Ok(Err(kept));
    }
    #[cfg(test)]
    let reached = install.faults.hit_io(Step::SyncEmptyDirectoryRemove);
    #[cfg(not(test))]
    let reached: io::Result<()> = Ok(());
    let removed_here =
        match reached.and_then(|()| fsutil::remove_empty_directory_at(&parent, &name)) {
            Ok(()) => {
                #[cfg(test)]
                install.faults.directory_removed(&install.item)?;
                true
            }
            // Someone removed the directory since the proof read it. Installation takes the name
            // only through a rename that never replaces an entry, so whatever took the name since,
            // a file or a new directory, fails installation and keeps its bytes. The parent's sync
            // below still makes the removal durable before the copy takes the name.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                #[cfg(test)]
                install.faults.hit(Step::SyncEmptyDirectoryRemoved)?;
                false
            }
            Err(error) => match Refusal::of(&error) {
                Some(refusal) => return Ok(Err(Kept::Refused(refusal))),
                None => return Err(error.into()),
            },
        };
    #[cfg(test)]
    failure(install.item.token.root, "directory-parent-sync")?;
    parent.sync_all()?;
    #[cfg(test)]
    install.faults.target_dir_synced(&install.item);
    if removed_here {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Info,
            "sync_directory_removed",
            serde_json::json!({
                "bucket": install.item.bucket.as_str(),
                "object": install.item.name.as_str(),
                "path": install.item.path,
                "metadata_removed": unlinked,
            }),
        );
    }
    Ok(Ok(parent))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
