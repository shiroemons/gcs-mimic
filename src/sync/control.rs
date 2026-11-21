//! Bounded live-server controls; the catalog keeps no strong root ownership cycle.

use super::{
    Reconciliation, SyncError,
    report::{
        ending::WaitResult,
        remedy::{paused_control_remedy, stale_decision_remedy},
    },
};
use crate::{
    auth::Identity,
    storage::sync_state::{
        OpaqueId,
        deletions::{DeletionSet, Refusal},
        observation::PauseReason,
        root::{RootError, RootLease},
    },
};
use std::{
    error::Error,
    fmt,
    sync::{Arc, Weak},
};
use tokio::sync::{mpsc, oneshot};

/// Why a queued control produced no result.
#[derive(Debug)]
pub(crate) enum ControlError {
    /// The rescan, approval or wait ran, or was being authorized, and failed.
    Failed(SyncError),
    /// A rescan ran and failed, and synchronization is paused for `reason`: the reason a failed
    /// resume keeps, or the one the failure itself recorded, as `change` says.
    RescanFailed {
        reason: PauseReason,
        change: PauseChange,
        error: SyncError,
    },
    /// Synchronization is paused; only an explicit rescan may resume it.
    Paused,
    /// The mirror stopped before running the control.
    Shutdown,
    /// A decision on the deletion set `revision` failed its precondition, `error`, because the
    /// set no longer waits for it, as `refusal` says.
    Stale {
        decision: Decision,
        revision: OpaqueId,
        refusal: Refusal,
        error: SyncError,
    },
}

/// What a failed rescan did to the pause, which its answer names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PauseChange {
    /// The mirror was running, and the rescan's failure paused it.
    Began,
    /// The rescan was to resume a pause and failed in a way that needs an operator, so the
    /// pause stays and records the failure's reason.
    Recorded,
    /// The rescan was to resume a pause and failed transiently, so the pause stays with the
    /// reason it recorded before, and running the rescan again may succeed.
    Kept,
}

impl PauseChange {
    /// How synchronization stands after the failed rescan.
    pub(crate) const fn paused(self) -> &'static str {
        match self {
            Self::Began => "is now paused",
            Self::Recorded | Self::Kept => "stays paused",
        }
    }
}

impl From<SyncError> for ControlError {
    fn from(error: SyncError) -> Self {
        Self::Failed(error)
    }
}

impl From<RootError> for ControlError {
    fn from(error: RootError) -> Self {
        Self::Failed(SyncError::Root(error))
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(error) => fmt::Display::fmt(error, f),
            Self::RescanFailed { reason, change, .. } => write!(
                f,
                "the rescan failed and synchronization {} ({})",
                change.paused(),
                reason.as_str()
            ),
            Self::Paused => f.write_str(&paused_control_remedy()),
            Self::Shutdown => f.write_str("synchronization stopped before the control ran"),
            Self::Stale {
                decision,
                revision,
                refusal,
                ..
            } => f.write_str(&stale_decision_remedy(*decision, *revision, *refusal)),
        }
    }
}

impl Error for ControlError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Failed(error) | Self::RescanFailed { error, .. } | Self::Stale { error, .. } => {
                Some(error)
            }
            Self::Paused | Self::Shutdown => None,
        }
    }
}

/// Answers a queued rescan, approval or wait.
pub(crate) type Reply<T> = oneshot::Sender<Result<T, ControlError>>;

/// What an operator decides about the deletion set that waits for a decision. The control
/// routes, the mirror and the CLI carry it end to end, so the two decisions share every step
/// but the ones that differ, and a route or a command can name only one of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Delete the set's objects.
    Approve,
    /// Keep the objects and write the set's files back from them.
    Reject,
}

impl Decision {
    pub const ALL: [Self; 2] = [Self::Approve, Self::Reject];

    /// The CLI command, which answers and advice name the decision by.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
        }
    }

    /// The control route that takes the decision, which ends with its command.
    pub const fn path(self) -> &'static str {
        match self {
            Self::Approve => "/_internal/sync/approve",
            Self::Reject => "/_internal/sync/reject",
        }
    }

    /// The decision as a set that took it is described.
    pub(in crate::sync) const fn past(self) -> &'static str {
        match self {
            Self::Approve => "approved",
            Self::Reject => "rejected",
        }
    }
}

// An accepted decision names itself by its command, so a reader of the answer alone can tell an
// approval, whose objects go, from a rejection, whose files come back.
crate::serde_util::serialize_as_str!(Decision);

/// A name that is neither decision's command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownDecision(String);

impl fmt::Display for UnknownDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown deletion decision {:?}", self.0)
    }
}

impl Error for UnknownDecision {}

/// A decision parses from the command that takes it, which its route ends with too.
impl std::str::FromStr for Decision {
    type Err = UnknownDecision;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|decision| decision.as_str() == value)
            .ok_or_else(|| UnknownDecision(value.to_owned()))
    }
}

pub(crate) enum Command {
    Resolve {
        request: crate::storage::sync_state::conflicts::ResolveRequest,
        reply: oneshot::Sender<
            Result<
                crate::storage::sync_state::conflicts::ResolveOutcome,
                crate::storage::sync_state::conflicts::Error,
            >,
        >,
        identity: Option<Identity>,
    },
    Rescan {
        reply: Reply<Reconciliation>,
        identity: Option<Identity>,
    },
    /// Approves or rejects the deletion set `revision`, as `decision` says.
    Decide {
        decision: Decision,
        revision: OpaqueId,
        reply: Reply<DeletionSet>,
        identity: Option<Identity>,
    },
    Wait {
        deadline: tokio::time::Instant,
        settling: Arc<std::sync::atomic::AtomicBool>,
        reply: Reply<WaitResult>,
        identity: Option<Identity>,
    },
}

#[derive(Clone)]
pub(crate) struct Registration {
    pub session: String,
    pub root: Weak<RootLease>,
    pub sender: mpsc::Sender<Command>,
    /// What the mirror behind `sender` is doing, which the status control reports.
    pub status: super::MirrorStatus,
}

#[cfg(test)]
mod tests;
