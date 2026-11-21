//! Why `sync wait` ended.

use super::verdict::Holdup;
use std::{error::Error, fmt};

/// Why `sync wait` ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    Ready,
    Timeout,
    Stopped,
    Paused,
    JournalChanged,
    SourceChanging,
    SourceUnrepresentable,
    DeletionApproval,
    FindingsPending,
}

/// How a wait ended, as the one place that decides what a caller does next: the command's exit
/// status and whether it asks the server again follow it. The HTTP status of the answer follows
/// the reason instead, since a wait that ran out of time answers differently from one that saw
/// the tree still changing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing holds synchronization back, even while objects a wait only reports remain.
    Ready,
    /// Something holds it back.
    Unsettled(Unsettled),
}

/// How a wait that did not end ready ended. It has no ready variant, so that an error built
/// from it cannot describe a success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsettled {
    /// Work was still in progress, so that another wait may succeed without anyone repairing
    /// anything: the deadline passed, the tree was still changing, or the journal was rebuilt
    /// under the wait.
    InProgress,
    /// Waiting does not change what holds synchronization back, so someone must act.
    ActionNeeded,
    /// The server stopped, or no answer says how the wait ended.
    Failed,
}

impl Unsettled {
    /// The exit status of `sync wait` that did not end ready.
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Failed => 1,
            Self::ActionNeeded => 2,
            Self::InProgress => 3,
        }
    }
}

impl Outcome {
    /// The exit status of `sync wait`, so that a script tells how it ended without reading the
    /// answer.
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Ready => 0,
            Self::Unsettled(unsettled) => unsettled.exit_code(),
        }
    }

    /// Whether a wait that ended so is asked again while time remains.
    pub const fn asks_again(self) -> bool {
        match self {
            Self::Unsettled(Unsettled::InProgress) => true,
            Self::Ready | Self::Unsettled(Unsettled::ActionNeeded | Unsettled::Failed) => false,
        }
    }
}

/// How a wait answers the first condition that holds after its pass. Every write-back stop is
/// work an operator must finish, as the findings that stop a wait are, and the objects a wait
/// refuses are findings too. A baseline still open after the wait's own pass answers as a tree
/// still changing, which [`WaitReason::outcome`] reads as work in progress.
impl From<Holdup> for WaitReason {
    fn from(holdup: Holdup) -> Self {
        match holdup {
            Holdup::Paused(_) => Self::Paused,
            Holdup::SourceChanging | Holdup::Observing => Self::SourceChanging,
            Holdup::SourceUnrepresentable | Holdup::SourceUnreadable => Self::SourceUnrepresentable,
            Holdup::DeletionApproval => Self::DeletionApproval,
            Holdup::Writeback(_) | Holdup::FindingsPending | Holdup::UnmappedObjects { .. } => {
                Self::FindingsPending
            }
            Holdup::Ready => Self::Ready,
        }
    }
}

impl WaitReason {
    pub(crate) const ALL: [Self; 9] = [
        Self::Ready,
        Self::Timeout,
        Self::Stopped,
        Self::Paused,
        Self::JournalChanged,
        Self::SourceChanging,
        Self::SourceUnrepresentable,
        Self::DeletionApproval,
        Self::FindingsPending,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Timeout => "Timeout",
            Self::Stopped => "Stopped",
            Self::Paused => "Paused",
            Self::JournalChanged => "JournalChanged",
            Self::SourceChanging => "SourceChanging",
            Self::SourceUnrepresentable => "SourceUnrepresentable",
            Self::DeletionApproval => "DeletionApproval",
            Self::FindingsPending => "FindingsPending",
        }
    }

    /// How this way of ending reads to a caller. A wait asks again for a reason that waiting
    /// alone clears; the other reasons stay until someone acts, or the server has stopped.
    pub const fn outcome(self) -> Outcome {
        match self {
            Self::Ready => Outcome::Ready,
            Self::Timeout | Self::SourceChanging | Self::JournalChanged => {
                Outcome::Unsettled(Unsettled::InProgress)
            }
            Self::Paused
            | Self::SourceUnrepresentable
            | Self::DeletionApproval
            | Self::FindingsPending => Outcome::Unsettled(Unsettled::ActionNeeded),
            Self::Stopped => Outcome::Unsettled(Unsettled::Failed),
        }
    }
}

crate::serde_util::serialize_as_str!(WaitReason);

impl fmt::Display for WaitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `reason` that names no way a wait ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownWaitReason(String);

impl fmt::Display for UnknownWaitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown wait reason {:?}", self.0)
    }
}

impl Error for UnknownWaitReason {}

impl std::str::FromStr for WaitReason {
    type Err = UnknownWaitReason;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == value)
            .ok_or_else(|| UnknownWaitReason(value.to_owned()))
    }
}

#[cfg(test)]
mod tests;
