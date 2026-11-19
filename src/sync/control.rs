//! Bounded live-server controls; the catalog keeps no strong root ownership cycle.

use super::{
    Reconciliation, SyncError,
    advice::{Advised, AtHand, UnmappedCount},
    entries::{EntryProblem, path_text},
    outstanding::{Holdup, UnmappedObjects},
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
        "A directory holds this object's file name. Move everything in it out of the tree, including hidden files such as ._* or editor swap files, then run {}; the directory is then replaced by the file, along with a .DS_Store if that is all it holds",
        Advised::Rescan
    )
}

/// What a control that a pause refuses answers, and how the refusal reads in a log.
pub(crate) fn paused_control_remedy() -> String {
    format!(
        "Synchronization is paused, so it cannot act on this control yet. Find the reason: {}; correct it, then resume synchronization: {}",
        Advised::Status,
        Advised::Rescan
    )
}

/// A condition outside a pause that the next pass clears once an operator corrects it. A wait
/// runs its own pass over the tree, so the advice for each ends by naming the wait; a rescan
/// resumes only a pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Correction {
    /// Entries that no object can take or that a pass could not read, which a wait ends on and
    /// a pause names too.
    UnrepresentableEntries,
    /// A file a pass could not read, which the `sync_source_deferred` event names.
    UnreadableFile,
    /// An entry a pass could not take in or could not read, which its own event names when a
    /// scan first finds it.
    Entry(EntryProblem),
    /// An object whose name the tree cannot hold, which its finding and the
    /// `sync_directory_kept` warning name. The wait's own pass exports a renamed object and
    /// takes in a removed entry, as a rescan would.
    UnrepresentableName,
    /// An export that met an edited file, which a finding names.
    ExportConflict,
    /// A file that kept changing, which a path stop names.
    ChangingPath,
    /// A path whose input or output failed, which a path stop names.
    PathIoFailure,
    /// A tracked file that a directory or special file replaced, which a path stop and a
    /// deletion check name.
    ReplacedEntry,
    /// An entry above a tracked path that is not a directory, which a deletion check names.
    BlockedParent,
    /// A differently spelled entry in place of a tracked name, which a deletion check names.
    AliasedName,
    /// A path whose permissions keep a deletion check from reading it.
    UnreadablePath,
}

impl Correction {
    /// What to correct, which names no command.
    const fn as_str(self) -> &'static str {
        match self {
            Self::UnrepresentableEntries => {
                "Rename, replace or move out of the directory the entries no object can take: symbolic links and special files, each logged as sync_unsafe_entry, names the API cannot hold, a file where an object needs a directory, and files left in the directory of a bucket deleted through the API. Restore read access to entries that could not be read"
            }
            Self::UnreadableFile => {
                "Restore access to this file and its parents (for example, chmod u+r FILE and chmod u+rx DIRECTORY), and free disk space if needed"
            }
            Self::Entry(problem) => problem.correction(),
            Self::UnrepresentableName => {
                "Preserve the source bytes, then use a representable API name or remove the conflicting directory entry"
            }
            Self::ExportConflict => {
                "Preserve the edited file, then inspect the retained conflicts or restore the previously exported bytes"
            }
            Self::ChangingPath => "Finish or stop the external writer",
            Self::PathIoFailure => {
                "Restore file permissions or free disk space, and preserve pending bytes"
            }
            Self::ReplacedEntry => {
                "A directory or special file replaced this tracked file. Move it away and put a regular file back, or delete the object through the API"
            }
            Self::BlockedParent => "Put a directory back in place of the entry above this path",
            Self::AliasedName => {
                "Rename the differently spelled entry back to the tracked name or move it away"
            }
            Self::UnreadablePath => {
                "Restore read and search permissions on this path and its parents"
            }
        }
    }

    /// The correction, then the wait whose pass takes it in.
    pub(crate) fn remedy(self) -> String {
        format!("{}; then wait again: {}", self.as_str(), Advised::Wait)
    }
}

/// What resumes synchronization after a pause for `reason`. The `sync_paused` event, the root-stop
/// finding and a failed rescan give the same advice, naming the commands to run with the
/// revisions `at_hand`; a log has none, so its advice lists the findings at the revision the
/// status reports.
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
            "Protected workspace entries wait for recovery. List them: {}; preserve each one and settle it with the resolve subcommand, as a confirmed recovery discard or a registered conflict; then resume synchronization: {rescan}",
            Advised::Findings(at_hand.findings_revision)
        ),
        PauseReason::SourceChanging => format!(
            "Let the programs writing in the directory finish and restore access to it; then resume synchronization: {rescan}"
        ),
        PauseReason::SourceUnrepresentable => format!(
            "{}. Then resume synchronization: {rescan}",
            Correction::UnrepresentableEntries.as_str()
        ),
        PauseReason::ObserverFailure => format!(
            "Preserve pending data and correct the condition that the failure, also logged as sync_paused, names. A changed server setting, such as max_sync_entries, needs a restart instead; anything else, a rescan: {rescan}"
        ),
    }
}

/// What resolves findings, path stops, retained conflicts and unfinished write-back, which stop
/// synchronization of their paths only.
pub(crate) fn findings_remedy(at_hand: AtHand<'_>) -> String {
    format!(
        "Findings or retained conflicts stop synchronization of some paths, and other paths continue. Follow the remedy of each; the findings view lists them: {}; the conflicts listing names the retained conflicts: {}",
        Advised::Findings(at_hand.findings_revision),
        Advised::Conflicts
    )
}

/// What the `sync_external_conflict` event asks for after a newer external edit arrived while
/// the earlier one was being preserved: a rescan, whose pass runs the skipped export again.
pub(crate) fn newer_edit_remedy() -> String {
    format!(
        "A newer external edit arrived after preservation. Stop the writer and inspect retained conflicts before restoring or discarding a revision; then rescan, which exports the object again: {}",
        Advised::Rescan
    )
}

/// What the `sync_export_retry` event asks for if the failure lasts and pauses synchronization.
pub(crate) fn export_retry_remedy() -> String {
    format!(
        "A transient failure is retried twice, and a lasting one pauses synchronization. Free disk space or restore permissions and inspect retained work; then resume synchronization: {}",
        Advised::Rescan
    )
}

/// What replaces a finding carried over from a release that recorded name collisions and
/// export conflicts alike: an observation of the object, which a restart or a rescan makes.
pub(crate) fn unclassified_collision_remedy() -> String {
    format!(
        "Carried over from a release that recorded name collisions and export conflicts alike. Have the object observed again and classified, by restarting the server or by a rescan: {}; then follow the remedy of the finding that replaces this one",
        Advised::Rescan
    )
}

/// What lets external deletions go on: an approval of the deletion set at the revision at
/// hand, or files put back.
pub(crate) fn deletion_approval_remedy(at_hand: AtHand<'_>) -> String {
    match at_hand.deletion_revision {
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
    }
}

/// What lets a wait that refuses objects whose names the tree cannot hold succeed while `count`
/// of them remain.
pub(crate) fn unmapped_objects_remedy(count: u64, at_hand: AtHand<'_>) -> String {
    let objects = UnmappedCount::from(count);
    let them = objects.them();
    format!(
        "{objects}, and this wait requires that none remain. Rename or delete {them} through the API, or wait without --require-no-findings, which reports {them} instead; the findings view lists {them}: {}",
        Advised::Findings(at_hand.findings_revision)
    )
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
                let names: Vec<_> = names.iter().map(|path| path_text(path)).collect();
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
    /// How the wait came to `reason`. Only the constructors set the two, so that they agree.
    #[serde(skip)]
    ending: Ending,
}

/// How a wait ended: on the first condition that held after its last pass, which decides its
/// reason, or before a pass could decide it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    Held(Holdup),
    /// `last` is what the wait's last finished pass was still waiting for, if one finished.
    Unfinished {
        why: Unfinished,
        last: Option<Holdup>,
    },
}

/// Why a wait ended before a pass could decide it by what holds synchronization back: its
/// deadline passed, the server began to stop, the journal was rebuilt under it, or the tree was
/// still settling when the server answered for it. Every other reason comes from a holdup,
/// through [`WaitResult::held`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unfinished {
    Timeout,
    Stopped,
    JournalChanged,
    SourceChanging,
}

impl From<Unfinished> for WaitReason {
    fn from(why: Unfinished) -> Self {
        match why {
            Unfinished::Timeout => Self::Timeout,
            Unfinished::Stopped => Self::Stopped,
            Unfinished::JournalChanged => Self::JournalChanged,
            Unfinished::SourceChanging => Self::SourceChanging,
        }
    }
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
/// work an operator must finish, as the findings that stop a wait are, and the objects a wait
/// refuses are findings too. A baseline still open after the wait's own pass is work in
/// progress, which the wait asks about again until its deadline, like a tree still changing.
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
    fn new(
        ending: Ending,
        journal_fence: Option<WaitFence>,
        reconciliation: Option<Reconciliation>,
    ) -> Self {
        let reason = match ending {
            Ending::Held(holdup) => WaitReason::from(holdup),
            Ending::Unfinished { why, .. } => WaitReason::from(why),
        };
        Self {
            completed: reason == WaitReason::Ready,
            timed_out: reason == WaitReason::Timeout,
            journal_fence,
            reconciliation,
            reason,
            findings: None,
            ending,
        }
    }

    /// A wait its last pass decided: by `holdup`, the first condition that held after it.
    pub(crate) fn held(
        holdup: Holdup,
        journal_fence: Option<WaitFence>,
        reconciliation: Option<Reconciliation>,
    ) -> Self {
        Self::new(Ending::Held(holdup), journal_fence, reconciliation)
    }

    /// A wait that ended for `why` before a pass could decide it.
    pub(crate) fn unfinished(
        why: Unfinished,
        journal_fence: Option<WaitFence>,
        reconciliation: Option<Reconciliation>,
    ) -> Self {
        Self::new(
            Ending::Unfinished { why, last: None },
            journal_fence,
            reconciliation,
        )
    }

    pub(crate) fn timeout() -> Self {
        Self::unfinished(Unfinished::Timeout, None, None)
    }

    /// This result with the findings the wait read.
    pub(crate) fn with_findings(self, findings: Option<WaitFindings>) -> Self {
        Self { findings, ..self }
    }

    /// The same wait ending for `why` before another pass could decide it: its deadline passed,
    /// or the server began to stop. What its last pass ended on stays, so that the remedy names
    /// what remained, but no longer decides how the wait ended.
    pub(crate) fn cut_short(self, why: Unfinished) -> Self {
        let findings = self.findings;
        let last = self.holdup();
        Self::new(
            Ending::Unfinished { why, last },
            self.journal_fence,
            self.reconciliation,
        )
        .with_findings(findings)
    }

    /// What the wait's last finished pass ended on: the holdup that decided the wait, or, for a
    /// wait cut short, what that pass was still waiting for.
    pub(crate) const fn holdup(&self) -> Option<Holdup> {
        match self.ending {
            Ending::Held(holdup) => Some(holdup),
            Ending::Unfinished { last, .. } => last,
        }
    }

    /// The reason the pause this wait ended on recorded, as the mirror knew it when it answered.
    pub(crate) const fn paused_reason(&self) -> Option<PauseReason> {
        match self.ending {
            Ending::Held(Holdup::Paused(reason)) => Some(reason),
            Ending::Held(
                Holdup::Writeback(_)
                | Holdup::SourceChanging
                | Holdup::SourceUnrepresentable
                | Holdup::SourceUnreadable
                | Holdup::Observing
                | Holdup::DeletionApproval
                | Holdup::FindingsPending
                | Holdup::UnmappedObjects { .. }
                | Holdup::Ready,
            )
            | Ending::Unfinished { .. } => None,
        }
    }

    /// What lets a later wait succeed, with the revisions `at_hand`: the remedy of the holdup
    /// the wait ended on. A wait that ended while work was under way names what its last pass
    /// was still waiting for; a success, a shutdown and a rebuilt journal need nothing.
    pub(crate) fn remedy(&self, at_hand: AtHand<'_>) -> Option<String> {
        let pass = self.reconciliation.as_ref();
        match self.ending {
            Ending::Held(holdup) if self.reason.is_in_progress() => {
                Some(unsettled_remedy(Some(holdup), pass))
            }
            Ending::Held(holdup) => holdup.remedy(at_hand),
            Ending::Unfinished {
                why: Unfinished::Timeout | Unfinished::SourceChanging,
                last,
            } => Some(unsettled_remedy(last, pass)),
            Ending::Unfinished {
                why: Unfinished::Stopped | Unfinished::JournalChanged,
                ..
            } => None,
        }
    }

    /// This wait under `unmapped`, its request's policy for objects whose names the tree cannot
    /// hold. Only a wait its pass decided ready changes: one that refuses them ends as the
    /// holdup of those that remain, with that holdup's reason and remedy. A wait cut short stays
    /// as it ended, whatever its last pass found.
    pub(crate) fn with_unmapped(self, unmapped: UnmappedObjects) -> Self {
        let Ending::Held(holdup) = self.ending else {
            return self;
        };
        let findings = self.findings;
        let held = holdup.with_unmapped(unmapped, findings.map_or(0, |found| found.unmapped));
        if held == holdup {
            return self;
        }
        Self::held(held, self.journal_fence, self.reconciliation).with_findings(findings)
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
    /// for. A wait that ended on any holdup but `Ready` has the remedy of that holdup, which
    /// names what to run with the endpoint to fill in; a success, a shutdown and a rebuilt
    /// journal need none. One that ran out of time and one the server answered after its minute
    /// read alike.
    #[test]
    fn only_work_in_progress_is_waited_for_again_and_the_rest_name_a_remedy() {
        use crate::storage::sync_state::writeback::Halt;
        use WaitReason::{JournalChanged, Ready, SourceChanging, Timeout};
        for reason in WaitReason::ALL {
            assert_eq!(
                reason.is_in_progress(),
                matches!(reason, Timeout | SourceChanging | JournalChanged),
                "{reason}"
            );
        }
        for holdup in [
            Holdup::Paused(PauseReason::IoFailure),
            Holdup::Writeback(Halt::ProtectedBody),
            Holdup::SourceChanging,
            Holdup::SourceUnrepresentable,
            Holdup::SourceUnreadable,
            Holdup::Observing,
            Holdup::DeletionApproval,
            Holdup::FindingsPending,
            Holdup::UnmappedObjects { count: 2 },
        ] {
            let remedy = holdup.remedy(AtHand::default());
            assert!(
                remedy
                    .as_ref()
                    .is_some_and(|remedy| remedy.contains("--endpoint URL")),
                "{holdup:?}"
            );
            let ended = WaitResult::held(holdup, None, None);
            assert_eq!(ended.reason, WaitReason::from(holdup), "{holdup:?}");
            assert_eq!(ended.remedy(AtHand::default()), remedy, "{holdup:?}");
        }
        assert_eq!(Holdup::Ready.remedy(AtHand::default()), None);
        let ready = WaitResult::held(Holdup::Ready, None, None);
        assert_eq!((ready.reason, ready.completed), (Ready, true));
        assert_eq!(ready.remedy(AtHand::default()), None);
        for why in [Unfinished::Stopped, Unfinished::JournalChanged] {
            let ended = WaitResult::unfinished(why, None, None);
            assert_eq!(ended.remedy(AtHand::default()), None, "{why:?}");
        }
        let timed_out = WaitResult::timeout().remedy(AtHand::default());
        assert_eq!(
            timed_out,
            WaitResult::unfinished(Unfinished::SourceChanging, None, None)
                .remedy(AtHand::default())
        );
        assert!(
            timed_out
                .is_some_and(|remedy| remedy.ends_with(": gcs-mimic sync wait --endpoint URL"))
        );
    }

    /// A wait that refuses objects whose names the tree cannot hold ends on them only when it
    /// is otherwise ready and some remain, as FindingsPending with a remedy of their own that
    /// lists them and names nothing else; any other end stays as the mirror decided it.
    #[test]
    fn a_wait_that_refuses_unmapped_objects_ends_on_them_alone() {
        let refused = with_unmapped_found(WaitResult::held(Holdup::Ready, None, None), 2)
            .with_unmapped(UnmappedObjects::Refuse);
        assert_eq!(refused.reason, WaitReason::FindingsPending);
        assert!(!refused.completed);
        assert_eq!(refused.holdup(), Some(Holdup::UnmappedObjects { count: 2 }));
        let at_hand = AtHand {
            findings_revision: Some(super::super::advice::SAMPLE_FINDINGS_REVISION),
            deletion_revision: None,
        };
        let remedy = refused.remedy(at_hand).unwrap();
        assert!(
            remedy.starts_with("2 objects in the API cannot be written to the sync directory because of their names"),
            "{remedy}"
        );
        assert!(
            remedy.ends_with(&format!(
                "the findings view lists them: {}",
                Advised::Findings(at_hand.findings_revision)
            )),
            "{remedy}"
        );
        assert!(!remedy.contains("conflicts"), "{remedy}");
        for (holdup, unmapped, policy) in [
            (Holdup::Ready, 2, UnmappedObjects::Report),
            (Holdup::Ready, 0, UnmappedObjects::Refuse),
            (Holdup::FindingsPending, 2, UnmappedObjects::Refuse),
        ] {
            let kept = with_unmapped_found(WaitResult::held(holdup, None, None), unmapped)
                .with_unmapped(policy);
            assert_eq!(
                (kept.reason, kept.holdup()),
                (WaitReason::from(holdup), Some(holdup)),
                "{policy:?}"
            );
        }
    }

    /// A wait's reason and the holdup its remedy follows come from one ending, so the two
    /// cannot disagree. Only a pass's holdup makes a wait ready, so every ready wait meets the
    /// request's policy for unmapped objects. A wait cut short keeps the reason it was cut short
    /// for, and with it the exit status that reason maps to, whatever its last pass found: a
    /// ready pass whose deadline then passed still times out, rather than ending on the
    /// unmapped objects that pass counted.
    #[test]
    fn a_waits_reason_and_its_holdup_come_from_one_ending() {
        for why in [
            Unfinished::Timeout,
            Unfinished::Stopped,
            Unfinished::JournalChanged,
            Unfinished::SourceChanging,
        ] {
            let ended = with_unmapped_found(WaitResult::unfinished(why, None, None), 2)
                .with_unmapped(UnmappedObjects::Refuse);
            assert_eq!(
                (ended.reason, ended.holdup(), ended.completed),
                (WaitReason::from(why), None, false),
                "{why:?}"
            );
            let cut = with_unmapped_found(WaitResult::held(Holdup::Ready, None, None), 2)
                .cut_short(why)
                .with_unmapped(UnmappedObjects::Refuse);
            assert_eq!(
                (cut.reason, cut.holdup(), cut.completed),
                (WaitReason::from(why), Some(Holdup::Ready), false),
                "{why:?}"
            );
            assert_eq!(cut.timed_out, why == Unfinished::Timeout, "{why:?}");
            assert_eq!(cut.findings.map(|found| found.unmapped), Some(2));
        }
    }

    fn with_unmapped_found(result: WaitResult, unmapped: u64) -> WaitResult {
        result.with_findings(Some(WaitFindings {
            unmapped,
            observation_revision: 1,
        }))
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
        let remedy = WaitResult::held(Holdup::SourceChanging, None, Some(pass))
            .cut_short(Unfinished::Timeout)
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
        let remedy = WaitResult::unfinished(Unfinished::Timeout, None, Some(written))
            .remedy(AtHand::default())
            .unwrap();
        assert!(
            remedy.starts_with("1 file was written after the wait's pass took the tree in. "),
            "{remedy}"
        );
        let remedy = WaitResult::held(Holdup::Observing, None, None)
            .remedy(AtHand::default())
            .unwrap();
        assert!(
            remedy.starts_with("The API changed during each pass"),
            "{remedy}"
        );
        let paused = WaitResult::held(Holdup::Paused(PauseReason::IoFailure), None, None);
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
