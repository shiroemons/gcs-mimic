//! The two comparisons of a file with a body that write-back knows. [`own_copy`] asks whether a
//! file is the private copy this operation staged; [`same_content`] asks whether a file holds the
//! bytes of a body the operation recorded: its API copy, the tracked body it replaces or removes,
//! or a preserved conflict. Installation, cancellation and restart recovery decide through these
//! two alone, so one body cannot get two answers from them. A stat comparison only ever shows
//! that a file differs; that it holds a body takes its hashes or the staged copy's identity.

use super::{Error, Pending, SyncFingerprint};
use crate::{
    fsutil::{self, FileIdentity, IdentityChanges},
    hashing::Hashes,
};
use std::{fs::File, io};

/// The size and hashes that identify a body's bytes.
#[derive(Clone, Copy)]
pub(super) struct Digest {
    size: u64,
    hashes: Hashes,
}

impl Digest {
    pub(super) fn new(size: u64, hashes: Hashes) -> Self {
        Self { size, hashes }
    }
}

/// The body `item` records: the API copy of a publication, or the tracked body a removal removes.
impl From<&Pending> for Digest {
    fn from(item: &Pending) -> Self {
        Self::new(
            item.size,
            Hashes {
                md5: item.md5,
                crc32c: item.crc32c,
            },
        )
    }
}

impl From<&SyncFingerprint> for Digest {
    fn from(fingerprint: &SyncFingerprint) -> Self {
        Self::new(
            fingerprint.size,
            Hashes {
                md5: fingerprint.md5,
                crc32c: fingerprint.crc32c,
            },
        )
    }
}

/// Whether a file holds a body, by [`same_content`].
pub(super) enum Match {
    /// It holds the body; the identity is the one it kept while its bytes were read.
    Same(FileIdentity),
    /// It holds other bytes, or it changed while they were read.
    Differs,
}

impl Match {
    /// The identity of a file that holds the body; `None` when it differs.
    pub(super) fn same(self) -> Option<FileIdentity> {
        match self {
            Self::Same(identity) => Some(identity),
            Self::Differs => None,
        }
    }
}

/// Whether `file` holds exactly the bytes of `digest`: its size and the hashes read through this
/// handle match, and its identity did not change while they were read. With `mtime`, the
/// modification time recorded with the body, a file whose own differs is taken to differ without
/// reading it. The inode number and ctime are not compared: Docker Desktop's shared folders can
/// report a new number for an unchanged file, and repairing its permissions changes its ctime.
pub(super) fn same_content(
    file: &File,
    digest: Digest,
    mtime: Option<i128>,
) -> Result<Match, Error> {
    let before = fsutil::stat_identity(file)?;
    if before.size != digest.size || mtime.is_some_and(|mtime| mtime != before.modified_ns) {
        return Ok(Match::Differs);
    }
    let fsutil::HashOutcome::Complete(hashes) =
        fsutil::hash_exact(file, digest.size, |_| Ok::<_, io::Error>(()))?
    else {
        return Ok(Match::Differs);
    };
    Ok(
        if hashes == digest.hashes && before == fsutil::stat_identity(file)? {
            Match::Same(before)
        } else {
            Match::Differs
        },
    )
}

/// How a file compares with the identity the private copy was staged under, by [`own_copy`].
pub(super) enum CopyMatch {
    /// It is the staged copy; the identity is the one to compare it with from now on.
    Same(FileIdentity),
    /// It is not, or it changed while it was checked.
    Differs {
        changes: IdentityChanges,
        content: CopyContent,
    },
}

/// Whether the bytes of a private copy that differs from its staged identity were read.
#[derive(Clone, Copy)]
pub(super) enum CopyContent {
    /// Fields other than the inode number changed, so the bytes were not read.
    NotChecked,
    /// Only the inode number differed, but the bytes, or the identity while they were read,
    /// did not match.
    Differs,
}

impl CopyContent {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::NotChecked => "not_checked",
            Self::Differs => "differs",
        }
    }
}

/// Whether `file` is the private copy staged as `staged`, the copy of `digest`. Every field of
/// its identity must match, except that the inode number alone may differ
/// ([`fsutil::IdentityChanges::is_inode_only`]): the same handle then proves the staged bytes by
/// [`same_content`], and the identity observed while they were read must still differ in
/// nothing but the inode number.
pub(super) fn own_copy(
    file: &File,
    staged: FileIdentity,
    digest: Digest,
) -> Result<CopyMatch, Error> {
    let current = fsutil::stat_identity(file)?;
    let changes = staged.changed_fields(&current);
    if changes.is_empty() {
        return Ok(CopyMatch::Same(current));
    }
    if !changes.is_inode_only() {
        return Ok(CopyMatch::Differs {
            changes,
            content: CopyContent::NotChecked,
        });
    }
    let confirmed = same_content(file, digest, None)?
        .same()
        .filter(|confirmed| {
            let changes = staged.changed_fields(confirmed);
            changes.is_empty() || changes.is_inode_only()
        });
    Ok(match confirmed {
        Some(confirmed) => CopyMatch::Same(confirmed),
        None => CopyMatch::Differs {
            changes,
            content: CopyContent::Differs,
        },
    })
}
