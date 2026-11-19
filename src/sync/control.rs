//! Bounded live-server controls; the catalog keeps no strong root ownership cycle.

use super::{Reconciliation, SyncError, outstanding::Holdup};
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

/// What to do after a rescan that was to resume a pause failed transiently: the
/// `sync_resume_failed` event and the rescan's answer both say it.
pub(crate) const RESUME_RETRY_REMEDY: &str = "Synchronization stays paused for the reason it recorded; run gcs-mimic sync rescan --endpoint URL again";

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
            "Rename, replace or move out of the directory the entries no object can take: symbolic links and special files, each logged as sync_unsafe_entry, names the API cannot hold, a file where an object needs a directory, and files left in the directory of a bucket deleted through the API; then run gcs-mimic sync rescan --endpoint URL"
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
            Self::RescanFailed { reason, change, .. } => write!(
                f,
                "the rescan failed and synchronization {} ({})",
                change.paused(),
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
    /// The findings the wait read with the observation it decided on; the control that answers
    /// the wait renders them.
    #[serde(skip)]
    pub findings: Option<WaitFindings>,
}

/// The findings a wait leaves behind without stopping, read with the observation it decided on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WaitFindings {
    /// The objects the API holds whose names the tree cannot.
    pub unmapped: u64,
    /// The observation revision these counts were read at, which binds the findings view.
    pub observation_revision: i64,
}

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

/// How a wait answers the first condition that holds after its pass. Every write-back stop is
/// work an operator must finish, as the findings that stop a wait are. A baseline still open
/// after the wait's own pass is work in progress, which the wait asks about again until its
/// deadline, like a tree still changing.
impl From<Holdup> for WaitReason {
    fn from(holdup: Holdup) -> Self {
        match holdup {
            Holdup::Paused(_) => Self::Paused,
            Holdup::SourceChanging | Holdup::Observing => Self::SourceChanging,
            Holdup::SourceUnrepresentable => Self::SourceUnrepresentable,
            Holdup::DeletionApproval => Self::DeletionApproval,
            Holdup::Writeback(_) | Holdup::FindingsPending => Self::FindingsPending,
            Holdup::Ready => Self::Ready,
        }
    }
}

impl WaitReason {
    const ALL: [Self; 9] = [
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

    /// Whether the work the wait waited for was still in progress, so that another wait may
    /// succeed without anyone repairing anything: its deadline passed, the tree was still
    /// changing, or the journal was rebuilt under it. The other reasons stay until someone acts.
    pub const fn is_in_progress(self) -> bool {
        match self {
            Self::Timeout | Self::SourceChanging | Self::JournalChanged => true,
            Self::Ready
            | Self::Stopped
            | Self::Paused
            | Self::SourceUnrepresentable
            | Self::DeletionApproval
            | Self::FindingsPending => false,
        }
    }

    /// What lets a later wait succeed after this one ended for this reason, naming the commands
    /// to run; `paused` is the reason a paused root recorded. A reason that needs nothing but
    /// time or a restart has none. The pause reasons reuse the advice of the pause itself.
    pub(crate) const fn remedy(self, paused: Option<PauseReason>) -> Option<&'static str> {
        match self {
            Self::Ready | Self::Timeout | Self::Stopped | Self::JournalChanged => None,
            Self::Paused => Some(pause_remedy(match paused {
                Some(reason) => reason,
                None => PauseReason::ObserverFailure,
            })),
            // The directory was still changing when the wait's own deadline passed; nothing is
            // paused, so waiting again once the writers finish is enough.
            Self::SourceChanging => Some(
                "Let the programs writing in the directory finish, then run gcs-mimic sync wait --endpoint URL again, with a longer --timeout if they need more time",
            ),
            Self::SourceUnrepresentable => Some(pause_remedy(PauseReason::SourceUnrepresentable)),
            Self::DeletionApproval => Some(
                "Inspect the deletion set that gcs-mimic sync status --view deletions --revision REVISION --endpoint URL lists for the pending_deletion_revision status reports, and approve it with gcs-mimic sync approve --deletion-set REVISION --endpoint URL, or restore the files and run gcs-mimic sync rescan --endpoint URL; changed candidates require fresh approval",
            ),
            Self::FindingsPending => Some(
                "List the findings with gcs-mimic sync status --view findings --revision REVISION --endpoint URL, using the findings revision this answer or status reports, and the retained conflicts with gcs-mimic sync conflicts --endpoint URL, and follow each one's remedy; other paths continue",
            ),
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
            findings: None,
        }
    }
    pub(crate) fn timeout() -> Self {
        Self::result(WaitReason::Timeout, None, None)
    }

    /// This result with the findings the wait read.
    pub(crate) fn with_findings(self, findings: Option<WaitFindings>) -> Self {
        Self { findings, ..self }
    }

    /// The same wait ending for `reason` instead, as a control that applies a stricter
    /// condition than the mirror decides it.
    pub(crate) fn with_reason(self, reason: WaitReason) -> Self {
        Self {
            completed: reason == WaitReason::Ready,
            timed_out: reason == WaitReason::Timeout,
            reason,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_reasons_parse_from_the_names_they_serialize_as() {
        for reason in WaitReason::ALL {
            assert_eq!(reason.as_str().parse::<WaitReason>(), Ok(reason));
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::Value::from(reason.as_str())
            );
        }
        assert!("ready".parse::<WaitReason>().is_err());
        assert!("".parse::<WaitReason>().is_err());
    }

    /// Only the reasons a wait meets while work is still under way are worth waiting again
    /// for; each other one has a remedy that names what to run, with the endpoint to fill in.
    #[test]
    fn only_work_in_progress_is_waited_for_again_and_the_rest_name_a_remedy() {
        use WaitReason::{JournalChanged, Ready, SourceChanging, Stopped, Timeout};
        for reason in WaitReason::ALL {
            assert_eq!(
                reason.is_in_progress(),
                matches!(reason, Timeout | SourceChanging | JournalChanged),
                "{reason}"
            );
            let remedy = reason.remedy(None);
            if matches!(reason, Ready | Timeout | Stopped | JournalChanged) {
                assert!(remedy.is_none(), "{reason}");
            } else {
                assert!(
                    remedy.is_some_and(|remedy| remedy.contains("--endpoint URL")),
                    "{reason}"
                );
            }
        }
        assert_eq!(
            WaitReason::Paused.remedy(Some(PauseReason::IoFailure)),
            Some(pause_remedy(PauseReason::IoFailure))
        );
        assert_eq!(
            WaitReason::Paused.remedy(None),
            Some(pause_remedy(PauseReason::ObserverFailure))
        );
        assert!(
            SourceChanging
                .remedy(None)
                .is_some_and(|remedy| remedy.contains("gcs-mimic sync wait --endpoint URL again"))
        );
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
