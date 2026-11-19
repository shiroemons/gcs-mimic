//! Removing the empty directory that holds an export's target name.
//!
//! Deleting `b/a/x.txt` through the API leaves the directory `b/a/` empty, and an object `b/a`
//! created later has no place in the tree while that directory stays. The export removes such a
//! directory itself rather than leave the operator to, under a proof built in two halves: the
//! catalog records nothing at or below the target ([`unreferenced`]), and the tree holds a
//! directory there under its exact spelling, below its bucket's directory and outside the names
//! the root reserves ([`RemovableDirectory::prove`]). [`remove`] takes that proof. Removal itself
//! is the last check, because an entry can arrive after every other one: `rmdir` removes only an
//! empty directory and never follows a symlink, so a directory that holds anything, even a file
//! that scans ignore, is left as a collision. Nothing here lists the directory or removes what
//! it holds, and no ancestor that a removal empties is removed after it; on a root that folds
//! letter case, only the directory that holds it is listed, to confirm the spelling.

use super::*;
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
    /// The filesystem refused the removal: the directory holds an entry, is in use, may not be
    /// removed, or changed before the removal.
    Refused(io::ErrorKind),
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
            Self::Refused(_) => "refused",
        }
    }

    /// The finding that keeps the object out of the tree. An entry under another spelling, or
    /// one that is no directory, holds the name as any other collision does; otherwise the
    /// directory is in the way, and what is in it decides when it can go.
    pub(super) fn finding(self) -> mapping::Reason {
        match self {
            Self::NotDirectory => mapping::Reason::Collision,
            Self::Replaces
            | Self::Tracked
            | Self::PathStop
            | Self::Pending
            | Self::Conflict
            | Self::NotAnObject
            | Self::Refused(_) => mapping::Reason::DirectoryOccupied,
        }
    }

    /// Logs why the directory at `item`'s target stays. The collision finding the mirror records
    /// names the object; this says which check kept the directory.
    pub(super) fn report(self, item: &Pending) {
        let error = match self {
            Self::Refused(kind) => Some(kind.to_string()),
            Self::Replaces
            | Self::Tracked
            | Self::PathStop
            | Self::Pending
            | Self::Conflict
            | Self::NotAnObject
            | Self::NotDirectory => None,
        };
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Info,
            "sync_directory_kept",
            serde_json::json!({
                "path": item.path,
                "reason": self.as_str(),
                "error": error,
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
}

impl RemovableDirectory {
    /// Completes `catalog` with what the tree under `root` holds at the target. The path must
    /// decode to an object below its bucket's directory, which rules out the bucket's directory
    /// itself as well as the workspace and the marker: a removed bucket directory would let a
    /// directory created later under that name bring a deleted bucket back. The entry must open
    /// as a directory under its exact spelling without following a symlink, and on a root that
    /// folds letter case the directory that holds it must also list that spelling.
    pub(super) fn prove(
        root: &File,
        naming: RootNaming,
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
        let parent = parent(root, naming, &path, ParentSync::Skip)?.ok_or_else(stale)?;
        match fsutil::child_directory(&parent, &name, fsutil::DirectoryOpen::Existing) {
            Ok(fsutil::Child::Directory { .. })
                if naming.folds_case() && !listed(&parent, &name)? =>
            {
                Ok(Err(Kept::NotDirectory))
            }
            Ok(fsutil::Child::Directory { .. }) => Ok(Ok(Self { path, parent, name })),
            Ok(fsutil::Child::Alias | fsutil::Child::Missing) => Ok(Err(Kept::NotDirectory)),
            Err(error) if crate::sync::tree::structural_entry_error(&error) => {
                Ok(Err(Kept::NotDirectory))
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// The most names [`listed`] reads from the directory that holds the target. A directory with
/// more leaves the spelling unconfirmed, which keeps the target's directory.
const SPELLING_LIST_LIMIT: usize = 4096;

/// Whether `parent` lists `name` under exactly that spelling. On a root that folds letter case,
/// opening a name can reach an entry spelled otherwise, and the filesystem need not say so: a
/// Linux mount that folds case without the casefold flag, as Docker Desktop's bind mounts do,
/// reports every spelling as exact. Removing `photos/` for an object `Photos` would then put a
/// file where the parent of a tracked `photos/a.jpg` was, and stop that file's deletion. The
/// root probe cannot find a mount that equates names only by Unicode normalization, so this
/// check never runs on one.
fn listed(parent: &File, name: &std::ffi::OsStr) -> Result<bool, Error> {
    let mut names = fsutil::DirectoryNames::default();
    loop {
        match names.next_name(parent, SPELLING_LIST_LIMIT) {
            Ok(Some(listed)) if listed == name => return Ok(true),
            Ok(Some(_)) => {}
            Ok(None) => return Ok(false),
            // More names than the limit: the spelling stays unconfirmed.
            Err(error) if error.kind() == io::ErrorKind::InvalidData => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
}

/// Whether a failed removal leaves a collision rather than an error to retry: the directory holds
/// an entry (`ENOTEMPTY`, or `EEXIST` where the system reports that), is in use (`EBUSY`), may not
/// be removed (`EACCES`, `EPERM`), or changed into something else since it was proved.
fn refused(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::DirectoryNotEmpty
            | io::ErrorKind::AlreadyExists
            | io::ErrorKind::ResourceBusy
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::NotADirectory
    )
}

/// Removes the directory `proof` names if it is empty, then syncs the directory that held it, so
/// the removal is durable before installation takes the name, and returns that directory for
/// installation to rename into. A refused removal changes nothing. A directory already gone
/// counts as removed.
pub(super) fn remove(
    proof: RemovableDirectory,
    install: &Install,
) -> Result<Result<File, Kept>, Error> {
    let RemovableDirectory { path, parent, name } = proof;
    // A proof answers for the target it was read at, not for another operation's.
    if path != install.item.path {
        return Err(stale());
    }
    #[cfg(test)]
    let reached = install.faults.hit_io(Step::SyncEmptyDirectoryRemove);
    #[cfg(not(test))]
    let reached: io::Result<()> = Ok(());
    match reached.and_then(|()| fsutil::remove_empty_directory_at(&parent, &name)) {
        Ok(()) => {
            #[cfg(test)]
            install.faults.directory_removed(&install.item)?;
        }
        // Someone removed the directory since the proof read it. Installation takes the name
        // only through a rename that never replaces an entry, so whatever took the name since,
        // a file or a new directory, fails installation and keeps its bytes. The parent's sync
        // below still makes the removal durable before the copy takes the name.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            #[cfg(test)]
            install.faults.hit(Step::SyncEmptyDirectoryRemoved)?;
        }
        Err(error) if refused(&error) => return Ok(Err(Kept::Refused(error.kind()))),
        Err(error) => return Err(error.into()),
    }
    #[cfg(test)]
    failure(install.item.token.root, "directory-parent-sync")?;
    parent.sync_all()?;
    #[cfg(test)]
    install.faults.target_dir_synced(&install.item);
    Ok(Ok(parent))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
