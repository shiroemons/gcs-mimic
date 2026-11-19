//! What the working tree holds at a tracked file's path. Only an empty path is a deletion: a path
//! the tree cannot show is no evidence either way, so it keeps its own object and nothing else.

use crate::{
    fsutil,
    storage::sync_state::{
        deletions::{BucketGone, Candidate, Removal},
        root::RootLease,
    },
    sync::{
        SyncError,
        control::Correction,
        tree::{self, ParentError, ParentOpen},
    },
};
use std::{fmt, fs::File, io, path::Path};

/// What a tracked file's path holds now. Whether it holds anything is one of three answers,
/// never a `bool`: a path that cannot be observed is neither present nor absent.
#[derive(Debug)]
pub(in crate::sync) enum Presence {
    /// Nothing: the file is gone, or a directory above it is.
    Absent(Absence),
    /// The regular file is there.
    Present,
    /// The path cannot be observed, or holds another kind of entry.
    Unobservable(Unobservable),
}

/// Proof that a tracked path was seen empty, which only [`presence`] gives, with what was gone
/// above it. Recording a deletion candidate and deleting its object each take one, so that
/// neither runs for a path nobody looked at.
#[derive(Debug)]
pub(in crate::sync) struct Absence(Removal);

impl Absence {
    /// `candidate`, whose path this proof saw empty.
    pub(in crate::sync) fn of(self, candidate: Candidate) -> Missing {
        Missing {
            candidate,
            absence: self,
        }
    }
}

/// A deletion candidate together with the proof that its path was seen empty.
#[derive(Debug)]
pub(in crate::sync) struct Missing {
    candidate: Candidate,
    absence: Absence,
}

impl Missing {
    pub(in crate::sync) fn candidate(&self) -> &Candidate {
        &self.candidate
    }

    /// What was gone above the candidate's path.
    pub(in crate::sync) fn removal(&self) -> Removal {
        self.absence.0
    }

    /// The candidate, for the inventory page that records it; the proof ends here.
    pub(in crate::sync) fn into_candidate(self) -> Candidate {
        let Self {
            candidate,
            absence: Absence(_),
        } = self;
        candidate
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
    /// A directory or special file took the regular file's place.
    Replaced,
    /// The path no longer maps back to its tracked object, as after a naming change.
    NameMismatch,
    /// Reading the path failed.
    Io(io::Error),
}

impl Unobservable {
    pub(in crate::sync) fn as_str(&self) -> &'static str {
        match self {
            Self::BeyondLimit => "beyond-limit",
            Self::InvalidPath => "invalid-path",
            Self::NameRejected => "name-rejected",
            Self::ParentBlocked => "parent-blocked",
            Self::Alias => "alias",
            Self::Replaced => "replaced",
            Self::NameMismatch => "name-mismatch",
            Self::Io(_) => "io-failure",
        }
    }

    /// Whether this also stops the path itself, in exports and inventories alike, until it is
    /// empty or holds a regular file again: only a directory or special file in the file's place,
    /// which removing or replacing it would destroy. The other reasons leave a path that holds
    /// nothing the mirror could destroy, so they only keep the object out of the deletion set.
    pub(in crate::sync) fn stops_path(&self) -> bool {
        match self {
            Self::Replaced => true,
            Self::BeyondLimit
            | Self::InvalidPath
            | Self::NameRejected
            | Self::ParentBlocked
            | Self::Alias
            | Self::NameMismatch
            | Self::Io(_) => false,
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
            Self::Replaced => Correction::ReplacedEntry.remedy(),
            Self::Io(_) => Correction::UnreadablePath.remedy(),
        }
    }
}

impl fmt::Display for Unobservable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{}: {error}", self.as_str()),
            Self::BeyondLimit
            | Self::InvalidPath
            | Self::NameRejected
            | Self::ParentBlocked
            | Self::Alias
            | Self::Replaced
            | Self::NameMismatch => f.write_str(self.as_str()),
        }
    }
}

/// Looks at `path` below the root without following any link. Only a failure of the root or
/// the catalog is an error; whatever the path itself holds is a [`Presence`].
pub(in crate::sync) async fn presence(
    root: &RootLease,
    path: String,
) -> Result<Presence, SyncError> {
    let limits = root.tree_limits();
    let naming = root.naming()?;
    root.file_job(move |root| {
        let path = Path::new(&path);
        if !limits.admits_file(path) {
            return Ok(Presence::Unobservable(Unobservable::BeyondLimit));
        }
        if path.is_absolute()
            || path
                .iter()
                .any(|part| part == "." || part == ".." || !naming.admits(part))
        {
            return Ok(Presence::Unobservable(Unobservable::InvalidPath));
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Ok(Presence::Unobservable(Unobservable::InvalidPath));
        };
        let directory = match tree::parent(root, naming, parent, ParentOpen::Existing) {
            Ok(directory) => directory,
            Err(ParentError::Missing) => return Ok(missing_above(root, path)),
            Err(ParentError::Finding(_)) => {
                return Ok(Presence::Unobservable(Unobservable::ParentBlocked));
            }
            Err(ParentError::Io(error)) => return Ok(lookup_failure(error)),
        };
        Ok(match fsutil::child_regular(&directory, name) {
            Ok(fsutil::RegularChild::File(_)) => Presence::Present,
            Ok(fsutil::RegularChild::Missing) => Presence::Absent(Absence(Removal::Files)),
            Ok(fsutil::RegularChild::Alias) => Presence::Unobservable(Unobservable::Alias),
            Err(error) => lookup_failure(error),
        })
    })
    .await
    .map_err(SyncError::from)
}

/// The absence of `path`, a directory above which is missing: whether its bucket's whole
/// directory is gone, or only directories inside it.
fn missing_above(root: &File, path: &Path) -> Presence {
    let Some(bucket) = path.iter().next() else {
        return Presence::Unobservable(Unobservable::InvalidPath);
    };
    match fsutil::child_directory(root, bucket, fsutil::DirectoryOpen::Existing) {
        Ok(fsutil::Child::Missing) => {
            Presence::Absent(Absence(Removal::Bucket(BucketGone::Missing)))
        }
        Ok(fsutil::Child::Directory { .. }) => Presence::Absent(Absence(Removal::Files)),
        Ok(fsutil::Child::Alias) => Presence::Unobservable(Unobservable::ParentBlocked),
        Err(error) => lookup_failure(error),
    }
}

/// Classifies a failed lookup of the path: the file system refused its name, another kind of
/// entry holds its place, or reading it failed.
fn lookup_failure(error: io::Error) -> Presence {
    let name_refused = error.kind() == io::ErrorKind::InvalidFilename
        || matches!(error.raw_os_error(), Some(libc::EILSEQ | libc::EINVAL));
    Presence::Unobservable(if name_refused {
        Unobservable::NameRejected
    } else if tree::structural_entry_error(&error) {
        Unobservable::Replaced
    } else {
        Unobservable::Io(error)
    })
}
