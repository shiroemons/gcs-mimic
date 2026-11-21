//! What the working tree holds at a tracked file's path. Only an empty path is a deletion: a path
//! the tree cannot show is no evidence either way, so it keeps its own object and nothing else.

use crate::{
    fsutil::{self, ListedSpelling},
    storage::sync_state::{deletions::Candidate, root::RootLease},
    sync::{
        SyncError,
        correspond::{Above, PathEntry, name_rejected, observe_path},
        report::remedy::Correction,
        tree,
    },
};
use std::{fmt, fs::File, io, path::Path};

/// What was gone above an absent tracked file. Every tracked file of a bucket gone at once needs
/// approval whatever the counts: a mount missing at startup or a directory moved away looks
/// exactly like the user deleting the files, and putting it back brings every file back.
/// Deleting files inside a bucket directory needs approval only by the counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Removal {
    /// The file, or a directory inside the bucket's own, is gone.
    Files,
    /// The bucket's own directory.
    BucketDirectory,
}

/// What a path holds now. Whether it holds anything is one of three answers, never a `bool`: a
/// path that cannot be observed is neither present nor absent.
#[derive(Debug)]
pub(in crate::sync) enum Presence {
    /// Nothing: the file is gone, or a directory above it is, as the removal tells, with the
    /// lookup's proof that the name holds nothing.
    Absent {
        removal: Removal,
        vacant: fsutil::Vacant,
    },
    /// The directory lists the file under another spelling, at which another object is tracked:
    /// the file was renamed to that object's name, and this path holds nothing of its own any
    /// more.
    Respelled,
    /// The regular file is there.
    Present,
    /// The path cannot be observed, or holds another kind of entry.
    Unobservable(Unobservable),
}

/// What a deletion candidate's own path holds now, with the candidate. Only [`observe`] makes
/// a missing one, so its candidate is the one whose path was looked at.
#[derive(Debug)]
pub(in crate::sync) enum Observed {
    /// Nothing of the file is at its path.
    Absent(Missing),
    /// The regular file is there.
    Present(Candidate),
    /// The path cannot be observed, or holds another kind of entry.
    Unobservable(Candidate, Unobservable),
}

/// A deletion candidate whose own path holds nothing of its file. Only [`observe`] makes one,
/// and recording a candidate and deleting its object each take one, so neither runs for a path
/// nobody looked at or for another candidate than the one seen.
#[derive(Debug)]
pub(crate) enum Missing {
    /// The path was seen empty, with what was gone above it and the lookup's proof that the
    /// name holds nothing, held rather than read: no failed read of the path can make one.
    Absent {
        candidate: Candidate,
        removal: Removal,
        _vacant: fsutil::Vacant,
    },
    /// The file was renamed to the spelling of another tracked object's name.
    Respelled { candidate: Candidate },
}

impl Missing {
    pub(crate) fn candidate(&self) -> &Candidate {
        match self {
            Self::Absent { candidate, .. } | Self::Respelled { candidate, .. } => candidate,
        }
    }

    /// What was gone above the candidate's path: a renamed file left the directories above it.
    pub(crate) fn removal(&self) -> Removal {
        match self {
            Self::Absent { removal, .. } => *removal,
            Self::Respelled { .. } => Removal::Files,
        }
    }
}

/// Why a tracked path tells nothing about its file. Its object is neither deleted nor kept from
/// the deletion of others.
#[derive(Debug)]
pub(in crate::sync) enum Unobservable {
    /// The path lies deeper than `max_sync_depth` admits, as after lowering it.
    BeyondLimit,
    /// The path holds `.`, `..` or a name this root's file system cannot hold.
    InvalidPath,
    /// The file system refused the name itself (`ENAMETOOLONG`, `EILSEQ` or `EINVAL`).
    NameRejected,
    /// A directory above the path is a file, a symlink or another entry.
    ParentBlocked,
    /// The directory holds the file under another spelling of its name.
    Alias,
    /// A directory or special file took the regular file's place. The log names it as the path
    /// stop and the findings view do, `entry-replaced` beside the other reasons' spelling.
    EntryReplaced,
    /// The path no longer maps back to its tracked object, as after a naming change.
    NameMismatch,
    /// Reading the path failed.
    Io(io::Error),
    /// The file, or a directory above it, kept failing to open while its name still held an
    /// entry ([`fsutil::Opened::Transient`]). It is logged as a failed read is.
    Unsettled,
}

impl Unobservable {
    pub(in crate::sync) fn as_str(&self) -> &'static str {
        match self {
            Self::BeyondLimit => "beyond-limit",
            Self::InvalidPath => "invalid-path",
            Self::NameRejected => "name-rejected",
            Self::ParentBlocked => "parent-blocked",
            Self::Alias => "alias",
            Self::EntryReplaced => "entry-replaced",
            Self::NameMismatch => "name-mismatch",
            Self::Io(_) | Self::Unsettled => "io-failure",
        }
    }

    /// Whether this also stops the path itself, in exports and inventories alike, until it is
    /// empty or holds a regular file again: only a directory or special file in the file's place,
    /// which removing or replacing it would destroy. The other reasons leave a path that holds
    /// nothing the mirror could destroy, so they only keep the object out of the deletion set.
    pub(in crate::sync) fn stops_path(&self) -> bool {
        match self {
            Self::EntryReplaced => true,
            Self::BeyondLimit
            | Self::InvalidPath
            | Self::NameRejected
            | Self::ParentBlocked
            | Self::Alias
            | Self::NameMismatch
            | Self::Io(_)
            | Self::Unsettled => false,
        }
    }

    /// What the user can do so that the path tells again whether its file was deleted.
    pub(in crate::sync) fn remedy(&self) -> String {
        match self {
            Self::BeyondLimit => {
                "Raise max_sync_depth and restart the server, or delete the object through the API"
                    .to_owned()
            }
            Self::InvalidPath | Self::NameRejected | Self::NameMismatch => {
                "This file system cannot hold the tracked path any longer; delete or rename the object through the API".to_owned()
            }
            Self::ParentBlocked => Correction::BlockedParent.remedy(),
            Self::Alias => Correction::AliasedName.remedy(),
            Self::EntryReplaced => Correction::ReplacedEntry.remedy(),
            Self::Io(_) | Self::Unsettled => Correction::UnreadablePath.remedy(),
        }
    }
}

impl fmt::Display for Unobservable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{}: {error}", self.as_str()),
            Self::Unsettled => write!(f, "{}: {}", self.as_str(), fsutil::Unsettled),
            Self::BeyondLimit
            | Self::InvalidPath
            | Self::NameRejected
            | Self::ParentBlocked
            | Self::Alias
            | Self::EntryReplaced
            | Self::NameMismatch => f.write_str(self.as_str()),
        }
    }
}

/// Looks at the path of `candidate` through the lookups of the pass that asks, and returns what
/// it holds with the candidate.
pub(in crate::sync) async fn observe(
    root: &RootLease,
    lookup: &fsutil::Lookup,
    candidate: Candidate,
) -> Result<Observed, SyncError> {
    Ok(
        match presence(root, lookup, candidate.path.clone()).await? {
            Presence::Absent { removal, vacant } => Observed::Absent(Missing::Absent {
                candidate,
                removal,
                _vacant: vacant,
            }),
            Presence::Respelled => Observed::Absent(Missing::Respelled { candidate }),
            Presence::Present => Observed::Present(candidate),
            Presence::Unobservable(reason) => Observed::Unobservable(candidate, reason),
        },
    )
}

/// Looks at `path` below the root without following any link, through the lookups of the pass
/// that asks. Only a failure of the root or the catalog is an error; whatever the path itself
/// holds is a [`Presence`].
pub(in crate::sync) async fn presence(
    root: &RootLease,
    lookup: &fsutil::Lookup,
    path: String,
) -> Result<Presence, SyncError> {
    let limits = root.tree_limits();
    let naming = root.naming()?;
    let lookup = lookup.clone();
    let looked = root
        .file_job(move |root| {
            let path = Path::new(&path);
            if !limits.admits_file(path) {
                return Ok(Looked::At(Presence::Unobservable(
                    Unobservable::BeyondLimit,
                )));
            }
            if path.is_absolute()
                || path
                    .iter()
                    .any(|part| part == "." || part == ".." || !naming.admits(part))
            {
                return Ok(Looked::At(Presence::Unobservable(
                    Unobservable::InvalidPath,
                )));
            }
            if path.file_name().is_none() {
                return Ok(Looked::At(Presence::Unobservable(
                    Unobservable::InvalidPath,
                )));
            }
            Ok(match observe_path(root, naming, &lookup, path) {
                PathEntry::File { .. } => Looked::At(Presence::Present),
                PathEntry::Absent {
                    above: Above::Present,
                    vacant,
                } => Looked::At(Presence::Absent {
                    removal: Removal::Files,
                    vacant,
                }),
                PathEntry::Absent {
                    above: Above::Missing,
                    vacant,
                } => Looked::At(missing_above(root, &lookup, path, vacant)),
                PathEntry::Unplaced(_) => {
                    Looked::At(Presence::Unobservable(Unobservable::ParentBlocked))
                }
                PathEntry::Respelled(ListedSpelling::Listed(spelling)) => {
                    match path.with_file_name(spelling).into_os_string().into_string() {
                        Ok(spelled) => Looked::Spelled(spelled),
                        Err(_) => Looked::At(Presence::Unobservable(Unobservable::Alias)),
                    }
                }
                PathEntry::Respelled(ListedSpelling::Unnamed) => {
                    Looked::At(Presence::Unobservable(Unobservable::Alias))
                }
                PathEntry::Directory(_) | PathEntry::Special(_) => {
                    Looked::At(Presence::Unobservable(Unobservable::EntryReplaced))
                }
                PathEntry::NameRejected(_) => {
                    Looked::At(Presence::Unobservable(Unobservable::NameRejected))
                }
                // A name that kept failing to open while it held an entry is no deletion: the
                // next inventory reads it again.
                PathEntry::Unreadable(error) => {
                    Looked::At(Presence::Unobservable(Unobservable::Io(error)))
                }
                PathEntry::Transient => Looked::At(Presence::Unobservable(Unobservable::Unsettled)),
            })
        })
        .await?;
    let spelled = match looked {
        Looked::At(presence) => return Ok(presence),
        Looked::Spelled(spelled) => spelled,
    };
    // Another object is tracked at the spelling the file is listed under: the file was renamed
    // to that object's name, which ingestion took in as that object, and this path holds
    // nothing of its own any more. The rename's deletion side goes through the deletion guard
    // as any rename's does. Otherwise the file may stand for this object under an alias.
    Ok(if root.tracked_live_at(spelled).await? {
        Presence::Respelled
    } else {
        Presence::Unobservable(Unobservable::Alias)
    })
}

/// What a lookup of a path found, before the catalog tells a renamed file from an alias.
enum Looked {
    At(Presence),
    /// A regular file is listed at this path below the root, under another spelling.
    Spelled(String),
}

/// The absence of `path`, a directory above which is missing as `vacant` proves: whether its
/// bucket's whole directory is gone, or only directories inside it.
fn missing_above(
    root: &File,
    lookup: &fsutil::Lookup,
    path: &Path,
    vacant: fsutil::Vacant,
) -> Presence {
    let Some(bucket) = path.iter().next() else {
        return Presence::Unobservable(Unobservable::InvalidPath);
    };
    match lookup.directory(root, bucket, fsutil::DirectoryOpen::Existing) {
        Ok(fsutil::Child::Missing(vacant)) => Presence::Absent {
            removal: Removal::BucketDirectory,
            vacant,
        },
        Ok(fsutil::Child::Directory { .. }) => Presence::Absent {
            removal: Removal::Files,
            vacant,
        },
        Ok(fsutil::Child::Alias) => Presence::Unobservable(Unobservable::ParentBlocked),
        Ok(fsutil::Child::Transient) => Presence::Unobservable(Unobservable::Unsettled),
        Err(error) => lookup_failure(error),
    }
}

/// Classifies a failed lookup of the path: the file system refused its name, another kind of
/// entry holds its place, or reading it failed.
fn lookup_failure(error: io::Error) -> Presence {
    Presence::Unobservable(if name_rejected(&error) {
        Unobservable::NameRejected
    } else if tree::structural_entry_error(&error) {
        Unobservable::EntryReplaced
    } else {
        Unobservable::Io(error)
    })
}
