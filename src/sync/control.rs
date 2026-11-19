//! Bounded live-server controls; the catalog keeps no strong root ownership cycle.

use super::{
    Reconciliation, SyncError,
    advice::{Advised, AtHand},
    outstanding::Holdup,
};
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

// Advice says what to do and then names the command, at the end of a sentence or before a
// semicolon, so that the command can be copied whole: nothing but `;` or the end of the text
// follows one, which the command's tests check for every piece of advice.

/// What to do after a rescan that was to resume a pause failed transiently: the
/// `sync_resume_failed` event and the rescan's answer both say it.
pub(crate) fn resume_retry_remedy() -> String {
    format!(
        "Synchronization stays paused for the reason it recorded, and the failure may pass; run the rescan again: {}",
        Advised::Rescan
    )
}

/// What frees the file name of an object that a directory with entries holds, as its finding and
/// the `sync_directory_kept` warning give it.
pub(crate) fn occupied_directory_remedy() -> String {
    format!(
        "A directory holds this object's file name. Move everything in it out of the tree, including hidden files such as .DS_Store, then run {}; the empty directory is then replaced by the file",
        Advised::Rescan
    )
}

/// What makes an object whose name the tree cannot hold reach it, as its finding and the
/// `sync_directory_kept` warning give it.
pub(crate) const UNREPRESENTABLE_REMEDY: &str = "Use a representable API name or remove the conflicting directory entry, then rescan; preserve source bytes";

/// What a control that a pause refuses answers, and how the refusal reads in a log.
pub(crate) fn paused_control_remedy() -> String {
    format!(
        "Synchronization is paused, so it cannot act on this control yet. Find the reason: {}; correct it, then resume synchronization: {}",
        Advised::Status,
        Advised::Rescan
    )
}

/// Where the findings that the advice `at_hand` refers to are listed: the findings view at the
/// revision at hand, or without one, at the revision the status reports when it runs.
fn findings_listing(at_hand: AtHand<'_>) -> String {
    format!(
        "the findings view lists them: {}",
        Advised::Findings(at_hand.findings_revision)
    )
}

/// What resumes synchronization after a pause for `reason`. The `sync_paused` event, the root-stop
/// finding and a failed rescan give the same advice, naming the commands to run with the
/// revisions `at_hand`; a log has none, so its advice names the status, which reports them.
pub(crate) fn pause_remedy(reason: PauseReason, at_hand: AtHand<'_>) -> String {
    let rescan = Advised::Rescan;
    match reason {
        PauseReason::ConflictCapacity => format!(
            "More conflicts are retained than the quota allows. Resolve or discard them with the resolve subcommand, as the listing describes each: {}; then resume synchronization: {rescan}",
            Advised::Conflicts
        ),
        PauseReason::IoFailure => format!(
            "Restore access to the directory or free disk space without deleting pending data; then resume synchronization: {rescan}"
        ),
        PauseReason::RecoveryRequired => format!(
            "Protected workspace entries wait for recovery. Preserve them, and settle each with the resolve subcommand as an exact confirmed recovery discard or a registered conflict; {}; then resume synchronization: {rescan}",
            findings_listing(at_hand)
        ),
        PauseReason::SourceChanging => format!(
            "Let the programs writing in the directory finish and restore access to it; then resume synchronization: {rescan}"
        ),
        PauseReason::SourceUnrepresentable => format!(
            "Rename, replace or move out of the directory the entries no object can take: symbolic links and special files, each logged as sync_unsafe_entry, names the API cannot hold, a file where an object needs a directory, and files left in the directory of a bucket deleted through the API. Restore read access to entries that could not be read. Then resume synchronization: {rescan}"
        ),
        PauseReason::ObserverFailure => format!(
            "Preserve pending data and correct the condition that the failure, also logged as sync_paused, names. A changed server setting, such as max_sync_entries, needs a restart instead; anything else, a rescan: {rescan}"
        ),
    }
}

/// What a wait that ended while work was still under way leaves, and what to run: the same for
/// a wait whose deadline passed and for one the server answered after its minute, so that the
/// two read alike. `holdup` is what the wait's last pass ended on, and `pass` that pass.
pub(crate) fn unsettled_remedy(holdup: Option<Holdup>, pass: Option<&Reconciliation>) -> String {
    let files = |count: u64| {
        if count == 1 {
            "1 file was"
        } else {
            "files were"
        }
    };
    let still = match (holdup, pass) {
        (_, Some(pass)) if pass.still_changing() > 0 => {
            let changing = pass.still_changing();
            let names = pass.deferred_files.first();
            let first = if names.is_empty() {
                String::new()
            } else {
                let names: Vec<String> = names
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect();
                format!(", the first {}", names.join(", "))
            };
            let count = if changing == 1 {
                String::new()
            } else {
                format!("{changing} ")
            };
            format!(
                "{count}{} still changing or racing an API write when the wait ended{first}",
                files(changing)
            )
        }
        (_, Some(pass)) if pass.external_only > 0 => {
            let count = if pass.external_only == 1 {
                String::new()
            } else {
                format!("{} ", pass.external_only)
            };
            format!(
                "{count}{} written after the wait's pass took the tree in",
                files(pass.external_only)
            )
        }
        (Some(Holdup::Observing), _) => {
            "The API changed during each pass, so synchronization has not completed its first full comparison yet".to_owned()
        }
        _ => "The directory was still changing when the wait ended".to_owned(),
    };
    format!(
        "{still}. Let the programs writing in the directory or through the API finish and wait again, with a longer --timeout if they need more time: {}",
        Advised::Wait
    )
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
    /// What the wait's last pass ended on: the pause its remedy follows, or, for a wait that
    /// ended while work was under way, what it was still waiting for.
    #[serde(skip)]
    pub holdup: Option<Holdup>,
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
            Holdup::SourceUnrepresentable | Holdup::SourceUnreadable => Self::SourceUnrepresentable,
            Holdup::DeletionApproval => Self::DeletionApproval,
            Holdup::Writeback(_) | Holdup::FindingsPending => Self::FindingsPending,
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
    /// to run with the revisions `at_hand`; `paused` is the reason a paused root recorded. A
    /// reason that needs nothing but a restart has none. The pause reasons reuse the advice of
    /// the pause itself, and work still under way the advice of [`unsettled_remedy`], which a
    /// wait's answer gives with what its last pass found.
    pub(crate) fn remedy(self, paused: Option<PauseReason>, at_hand: AtHand<'_>) -> Option<String> {
        match self {
            Self::Ready | Self::Stopped | Self::JournalChanged => None,
            Self::Paused => Some(pause_remedy(
                paused.unwrap_or(PauseReason::ObserverFailure),
                at_hand,
            )),
            Self::Timeout | Self::SourceChanging => Some(unsettled_remedy(None, None)),
            Self::SourceUnrepresentable => {
                Some(pause_remedy(PauseReason::SourceUnrepresentable, at_hand))
            }
            Self::DeletionApproval => Some(match at_hand.deletion_revision {
                Some(revision) => format!(
                    "Files that synchronization tracked are missing, and deleting their objects waits for approval. Inspect the deletion set: {}; approve it: {}; or restore the files and wait again: {}",
                    Advised::Deletions(Some(revision)),
                    Advised::Approve(revision),
                    Advised::Wait
                ),
                None => format!(
                    "Files that synchronization tracked are missing, and deleting their objects waits for approval. Inspect the deletion set, whose revision the approve subcommand takes as --deletion-set: {}; or restore the files and wait again: {}",
                    Advised::Deletions(None),
                    Advised::Wait
                ),
            }),
            Self::FindingsPending => Some(format!(
                "Findings or retained conflicts stop synchronization of some paths, and other paths continue. Follow the remedy of each; {}; the conflicts listing names the retained conflicts: {}",
                findings_listing(at_hand),
                Advised::Conflicts
            )),
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
            holdup: None,
        }
    }
    pub(crate) fn timeout() -> Self {
        Self::result(WaitReason::Timeout, None, None)
    }

    /// This result with the findings the wait read.
    pub(crate) fn with_findings(self, findings: Option<WaitFindings>) -> Self {
        Self { findings, ..self }
    }

    /// This result with what the wait's last pass ended on.
    pub(crate) fn with_holdup(self, holdup: Holdup) -> Self {
        Self {
            holdup: Some(holdup),
            ..self
        }
    }

    /// The reason the pause this wait met recorded, as the mirror knew it when it answered.
    pub(crate) const fn paused_reason(&self) -> Option<PauseReason> {
        match self.holdup {
            Some(Holdup::Paused(reason)) => Some(reason),
            Some(
                Holdup::Writeback(_)
                | Holdup::SourceChanging
                | Holdup::SourceUnrepresentable
                | Holdup::SourceUnreadable
                | Holdup::Observing
                | Holdup::DeletionApproval
                | Holdup::FindingsPending
                | Holdup::Ready,
            )
            | None => None,
        }
    }

    /// What lets a later wait succeed, with the revisions `at_hand`. A wait that ended while
    /// work was under way names what its last pass was still waiting for.
    pub(crate) fn remedy(&self, at_hand: AtHand<'_>) -> Option<String> {
        match self.reason {
            WaitReason::Timeout | WaitReason::SourceChanging => {
                Some(unsettled_remedy(self.holdup, self.reconciliation.as_ref()))
            }
            WaitReason::Ready
            | WaitReason::Stopped
            | WaitReason::Paused
            | WaitReason::JournalChanged
            | WaitReason::SourceUnrepresentable
            | WaitReason::DeletionApproval
            | WaitReason::FindingsPending => self.reason.remedy(self.paused_reason(), at_hand),
        }
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
    /// for; each reason but a success, a shutdown and a rebuilt journal has a remedy that names
    /// what to run, with the endpoint to fill in. One that ran out of time and one the server
    /// answered after its minute read alike.
    #[test]
    fn only_work_in_progress_is_waited_for_again_and_the_rest_name_a_remedy() {
        use WaitReason::{JournalChanged, Ready, SourceChanging, Stopped, Timeout};
        for reason in WaitReason::ALL {
            assert_eq!(
                reason.is_in_progress(),
                matches!(reason, Timeout | SourceChanging | JournalChanged),
                "{reason}"
            );
            let remedy = reason.remedy(None, AtHand::default());
            if matches!(reason, Ready | Stopped | JournalChanged) {
                assert!(remedy.is_none(), "{reason}");
            } else {
                assert!(
                    remedy.is_some_and(|remedy| remedy.contains("--endpoint URL")),
                    "{reason}"
                );
            }
        }
        assert_eq!(
            WaitReason::Paused.remedy(Some(PauseReason::IoFailure), AtHand::default()),
            Some(pause_remedy(PauseReason::IoFailure, AtHand::default()))
        );
        assert_eq!(
            WaitReason::Paused.remedy(None, AtHand::default()),
            Some(pause_remedy(
                PauseReason::ObserverFailure,
                AtHand::default()
            ))
        );
        assert_eq!(
            Timeout.remedy(None, AtHand::default()),
            SourceChanging.remedy(None, AtHand::default())
        );
        assert!(
            SourceChanging
                .remedy(None, AtHand::default())
                .is_some_and(|remedy| remedy.ends_with(": gcs-mimic sync wait --endpoint URL"))
        );
    }

    /// A wait that ran out of time names what its last pass was still waiting for: how many
    /// files were still changing and the first of them, the files written after the pass took
    /// the tree in, or an API that kept changing; and the pause a paused wait met is the one the
    /// mirror answered with.
    #[test]
    fn a_wait_that_runs_out_of_time_names_what_remained() {
        let mut pass = Reconciliation::default();
        for name in ["bucket/a", "bucket/b", "bucket/c", "bucket/d"] {
            pass.count_deferred(std::path::Path::new(name));
        }
        let remedy = WaitResult::result(WaitReason::SourceChanging, None, Some(pass))
            .with_holdup(Holdup::SourceChanging)
            .with_reason(WaitReason::Timeout)
            .remedy(AtHand::default())
            .unwrap();
        assert!(
            remedy.starts_with(
                "4 files were still changing or racing an API write when the wait ended, the first bucket/a, bucket/b, bucket/c. "
            ),
            "{remedy}"
        );
        let written = Reconciliation {
            external_only: 1,
            ..Reconciliation::default()
        };
        let remedy = WaitResult::result(WaitReason::Timeout, None, Some(written))
            .remedy(AtHand::default())
            .unwrap();
        assert!(
            remedy.starts_with("1 file was written after the wait's pass took the tree in. "),
            "{remedy}"
        );
        let remedy = WaitResult::result(WaitReason::SourceChanging, None, None)
            .with_holdup(Holdup::Observing)
            .remedy(AtHand::default())
            .unwrap();
        assert!(
            remedy.starts_with("The API changed during each pass"),
            "{remedy}"
        );
        let paused = WaitResult::result(WaitReason::Paused, None, None)
            .with_holdup(Holdup::Paused(PauseReason::IoFailure));
        assert_eq!(paused.paused_reason(), Some(PauseReason::IoFailure));
        assert_eq!(
            paused.remedy(AtHand::default()),
            Some(pause_remedy(PauseReason::IoFailure, AtHand::default()))
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
