//! Why a write-back step found the tree or the catalog other than it read or planned them.

use std::fmt;

/// How the working tree differed from what a step read moments before. That is a user's
/// concurrent change, which a new export plans around, not a failure of the root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RaceReason {
    /// A directory above the target is missing, or no directory any more.
    ParentChanged,
    /// The target's name reaches a file under another spelling of it.
    Respelled,
    /// The target, or a directory above it, kept failing to open while its name still held an
    /// entry.
    Unopenable,
    /// The file at the target is not the one the step read or expects there.
    TargetChanged,
    /// The directory at the target is not the one proved removable.
    DirectoryChanged,
}

impl fmt::Display for RaceReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ParentChanged => "a directory above the target changed",
            Self::Respelled => "the target is reached under another spelling",
            Self::Unopenable => "the target kept failing to open",
            Self::TargetChanged => "the file at the target changed",
            Self::DirectoryChanged => "the directory at the target changed",
        })
    }
}

/// How the catalog differed from an operation's plan: the API object, its bucket, the tracking
/// or the operation itself changed after the operation read them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StaleReason {
    /// The change journal moved past the fence the operation was planned at.
    JournalFence,
    /// The bucket's project or incarnation changed.
    Bucket,
    /// The API object is not the one the operation exports.
    Object,
    /// The name's tracking is not the one the operation was planned against.
    Tracked,
    /// A removal found the root's baseline incomplete.
    Baseline,
    /// The operation's row changed since it was read.
    Revision,
    /// The operation completed.
    Completed,
    /// The handle or the operation belongs to another catalog or root.
    Root,
    /// The name no longer maps to the operation's path under the root's naming.
    Path,
    /// The conflict store holds more entries than the root accounts for.
    Usage,
}

impl fmt::Display for StaleReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::JournalFence => "the change journal moved past the operation's fence",
            Self::Bucket => "the bucket changed",
            Self::Object => "the API object changed",
            Self::Tracked => "the tracked state changed",
            Self::Baseline => "the baseline is incomplete",
            Self::Revision => "the operation changed",
            Self::Completed => "the operation completed",
            Self::Root => "the operation belongs to another root",
            Self::Path => "the name maps to another path",
            Self::Usage => "the conflict store changed",
        })
    }
}
