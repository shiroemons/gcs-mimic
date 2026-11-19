//! Bounded live-server controls; the catalog keeps no strong root ownership cycle.

use super::{Reconciliation, SyncError};
use crate::{
    auth::Identity,
    storage::{
        change_journal::JournalFence,
        sync_state::{
            OpaqueId,
            deletions::DeletionSet,
            observation::PauseReason,
            root::{RootError, RootLease},
        },
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
    /// resume keeps, or the one the failure itself recorded.
    RescanFailed {
        reason: PauseReason,
        error: SyncError,
    },
    /// Synchronization is paused; only an explicit rescan may resume it.
    Paused,
    /// The mirror stopped before running the control.
    Shutdown,
}

/// What resumes synchronization after a pause for `reason`. The `sync_paused` event, the root-stop
/// finding and a failed rescan give the same advice, naming the commands to run.
pub(crate) const fn pause_remedy(reason: PauseReason) -> &'static str {
    match reason {
        PauseReason::ConflictCapacity => {
            "Resolve or discard, within the quota, the retained conflicts that gcs-mimic sync conflicts lists, with gcs-mimic sync resolve, then run gcs-mimic sync rescan --endpoint URL"
        }
        PauseReason::IoFailure => {
            "Restore access to the directory or free disk space without deleting pending data, then run gcs-mimic sync rescan --endpoint URL"
        }
        PauseReason::RecoveryRequired => {
            "Preserve the protected workspace entries that gcs-mimic sync findings lists and settle each with gcs-mimic sync resolve, as an exact confirmed recovery discard or a registered conflict, then run gcs-mimic sync rescan --endpoint URL"
        }
        PauseReason::SourceChanging => {
            "Let the programs writing in the directory finish, restore access to it, then run gcs-mimic sync rescan --endpoint URL"
        }
        PauseReason::SourceUnrepresentable => {
            "Preserve the entries that gcs-mimic sync findings lists as unrepresentable and rename them or move them out of the directory, then run gcs-mimic sync rescan --endpoint URL"
        }
        PauseReason::ObserverFailure => {
            "Preserve pending data and correct the condition that the failure, also logged as sync_paused, names, then run gcs-mimic sync rescan --endpoint URL; a changed server setting, such as max_sync_entries, needs a restart instead"
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
            Self::RescanFailed { reason, .. } => write!(
                f,
                "the rescan failed and synchronization stays paused ({})",
                reason.as_str()
            ),
            Self::Paused => f.write_str(
                "Synchronization is paused; correct the condition and run gcs-mimic sync rescan first",
            ),
            Self::Shutdown => f.write_str("synchronization stopped before the control ran"),
        }
    }
}

impl Error for ControlError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Failed(error) | Self::RescanFailed { error, .. } => Some(error),
            Self::Paused | Self::Shutdown => None,
        }
    }
}

/// Answers a queued rescan, approval or wait.
pub(crate) type Reply<T> = oneshot::Sender<Result<T, ControlError>>;

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
    Approve {
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

#[derive(serde::Serialize)]
pub(crate) struct WaitResult {
    pub completed: bool,
    pub timed_out: bool,
    pub journal_fence: Option<WaitFence>,
    pub reconciliation: Option<Reconciliation>,
    pub reason: WaitReason,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) enum WaitReason {
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
#[derive(serde::Serialize)]
pub(crate) struct WaitFence {
    pub sequence: u64,
    pub epoch: i64,
}
impl From<JournalFence> for WaitFence {
    fn from(fence: JournalFence) -> Self {
        Self {
            sequence: fence.sequence.get(),
            epoch: fence.revision,
        }
    }
}
impl WaitResult {
    pub(crate) fn result(
        reason: WaitReason,
        journal_fence: Option<WaitFence>,
        reconciliation: Option<Reconciliation>,
    ) -> Self {
        Self {
            completed: reason == WaitReason::Ready,
            timed_out: reason == WaitReason::Timeout,
            journal_fence,
            reconciliation,
            reason,
        }
    }
    pub(crate) fn timeout() -> Self {
        Self::result(WaitReason::Timeout, None, None)
    }
}

#[derive(Clone)]
pub(crate) struct Registration {
    pub session: String,
    pub root: Weak<RootLease>,
    pub sender: mpsc::Sender<Command>,
    /// What the mirror behind `sender` is doing, which the status control reports.
    pub status: super::MirrorStatus,
}
