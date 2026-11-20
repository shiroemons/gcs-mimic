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
            deletions::{BucketGone, DeletionSet, Refusal, WaitCause},
            findings::alias::FrozenFile,
            observation::{ListedStop, PauseReason},
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
    /// An object whose path, or a directory of it, another object or a file in the tree needs
    /// for another kind of entry, which its collision finding names. A file directly in the
    /// root named like the bucket is one: moving it away is all it takes, and nothing is lost.
    CollidingName,
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
    /// An object whose name the root's file system takes for another object's, which the
    /// alias findings of both name. Only an API change ends it: deleting or renaming the file on
    /// the host deletes neither object, and a file under a new name is ingested as any new file
    /// is.
    AliasedObjects,
    /// The file of an aliased pair that changed while frozen, which a path stop names.
    EditedAlias,
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
            Self::CollidingName => {
                "If a regular file directly in the sync directory is named like this bucket, it keeps the bucket's directory and every object in it out of the tree: move that file out of the sync directory, and the bucket is exported while the file stays as it is. Otherwise this object's name needs a directory where another object or a file in the bucket's directory is, or the other way round: rename or delete one of the objects through the API, or move the file in the way"
            }
            Self::ExportConflict => {
                "Preserve the edited file, then inspect the retained conflicts or restore the previously exported bytes"
            }
            Self::ChangingPath => "Finish or stop the external writer",
            Self::PathIoFailure => {
                "Restore file permissions or free disk space, and preserve pending bytes"
            }
            Self::ReplacedEntry => {
                "A directory or special file replaced this tracked file. To keep the object, move the entry away and put a regular file back. Moving the entry away without putting a file back deletes the object, as deleting the file does"
            }
            Self::BlockedParent => "Put a directory back in place of the entry above this path",
            Self::AliasedName => {
                "Rename the differently spelled entry back to the tracked name or move it away"
            }
            Self::UnreadablePath => {
                "Restore read and search permissions on this path and its parents"
            }
            Self::AliasedObjects => {
                "This object's name and another object's (partner) differ only in letter case or Unicode normalization, such as NFC and NFD, which this file system takes for one name, so the directory holds one file for both at tree_path (path in the log), with the bytes of the object spelled like it. While both objects exist that file stays as it is: API changes do not overwrite it, edits to it are not uploaded, and renaming or deleting it on the host deletes neither object and does not end this. Delete or rename one of the objects through the API, for example with gcloud storage mv, and the other gets its own file"
            }
            Self::EditedAlias => {
                "The file at tree_path (path in the log) stands for this object and another (partner) whose names differ only in letter case or Unicode normalization, and it changed while frozen. The edit stays in the file and was not uploaded, since it may be meant for either object. To upload it, delete or rename through the API the object it is not meant for, and the next pass uploads it for the other. To drop it, put the earlier bytes back or delete the file, which ends this stop"
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
    let remedy = paused_for(reason, at_hand);
    match rejected_left(at_hand.rejected_writeback) {
        Some(left) => format!("{left}. {remedy}; {LEAVE_OUT}, before you resume"),
        None => remedy,
    }
}

/// What resumes synchronization after a pause for `reason`, whatever is left to write back.
fn paused_for(reason: PauseReason, at_hand: AtHand<'_>) -> String {
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
/// export conflicts alike: an observation of the object, which every ingestion makes, so a wait
/// or a rescan does.
pub(crate) fn unclassified_collision_remedy() -> String {
    format!(
        "Carried over from a release that recorded name collisions and export conflicts alike. Have the object observed again and classified by a wait or a rescan: {}; then follow the remedy of the finding that replaces this one",
        Advised::Rescan
    )
}

/// The step before any decision on a deletion set when a mount failed, as every piece of advice
/// that names a decision puts it: a failed mount looks like a directory emptied at once.
pub(crate) const MOUNT_FIRST: &str = "if a mount failed, fix the mount and restart the server, which brings the files back, before anything else: a rejection before the mount returns writes them under the mount point, where the mount then hides them";

/// How many of the files synchronization tracked a deletion set finds missing, as the clause
/// that opens the advice on it.
fn missing_of(missing: u64, tracked: u64) -> String {
    let files = if tracked == 1 { "file" } else { "files" };
    let are = if missing == 1 { "is" } else { "are" };
    format!("{missing} of the {tracked} {files} that synchronization tracked {are} missing")
}

/// What lets external deletions go on once the files the set at hand counts went missing:
/// inspecting the set, then the ways out that keep the objects before the one that deletes
/// them, each with the case it is for. A rejection writes the files back from the objects,
/// files restored by hand let the next pass find nothing missing, and only an approval deletes.
/// A mount that failed looks like a directory emptied at once; it comes before the rejection,
/// since a rejection before the mount returns writes the files under the mount point, where the
/// mount then hides them, and the mount alone brings them back.
pub(crate) fn deletion_approval_remedy(at_hand: AtHand<'_>) -> String {
    const REJECT: &str =
        "if the files went by mistake, reject the set to write them back from the objects";
    const RESTORE: &str = "or restore the files yourself and wait again";
    const APPROVE: &str =
        "if the files were deleted on purpose, approve the set to delete their objects";
    let Some(set) = at_hand.deletion else {
        return format!(
            "Files that synchronization tracked are missing, and deleting their objects waits for a decision. Inspect the deletion set, whose revision the reject and approve subcommands take as --deletion-set: {}; {MOUNT_FIRST}; {REJECT}; {RESTORE}: {}; {APPROVE}",
            Advised::Deletions(None),
            Advised::Wait
        );
    };
    let revision = set.revision.to_hex();
    let cause = set.cause.map_or_else(String::new, wait_cause);
    format!(
        "{cause}{}, and deleting their objects waits for a decision. Inspect the deletion set: {}; {MOUNT_FIRST}; {REJECT}: {}; {RESTORE}: {}; {APPROVE}: {}",
        missing_of(set.missing, set.tracked),
        Advised::Deletions(Some(&revision)),
        Advised::Decide(Decision::Reject, &revision),
        Advised::Wait,
        Advised::Decide(Decision::Approve, &revision)
    )
}

/// The sentence that opens the advice on a set that waits for `cause`, saying what the inventory
/// saw and not why it happened: a mount that did not come up looks exactly like a directory
/// deleted on purpose. A set that holds files of an earlier undecided set says that files
/// deleted meanwhile joined it, since a decision on it covers them too.
fn wait_cause(cause: &WaitCause) -> String {
    match cause {
        WaitCause::Bucket {
            bucket,
            gone: BucketGone::Missing,
        } => format!(
            "The directory of bucket {bucket} is missing, as it is when a mount did not come up, so its tracked files wait for a decision however few they are. "
        ),
        WaitCause::Bucket {
            bucket,
            gone: BucketGone::Emptied,
        } => format!(
            "The directory of bucket {bucket} has shown none of its tracked files since the server started, as it does when a mount came up empty, so they wait for a decision however few they are. "
        ),
        WaitCause::Threshold => "At least 100 tracked files went missing, more than half of those synchronization tracked, counting deletions applied without a decision since the last approval. ".to_owned(),
        WaitCause::Held => "The set holds files of an earlier deletion set that nobody approved or rejected, so it waits too; files deleted while a set waits join it, and a decision covers them as well. ".to_owned(),
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
/// `rejected` files of a rejected deletion set left to write back come first: the server writes
/// those itself, so nobody else has anything to finish for them, and their log says why a
/// write-back stopped.
pub(crate) fn unsettled_remedy(
    holdup: Option<Holdup>,
    pass: Option<&Reconciliation>,
    rejected: u64,
) -> String {
    let wait = Advised::Wait;
    let writers = |still: &str| {
        format!(
            "{still}. Let the programs writing in the directory or through the API finish and wait again, with a longer --timeout if they need more time: {wait}"
        )
    };
    match (rejected_left(rejected), still_changing(holdup, pass)) {
        (None, still) => writers(
            still
                .as_deref()
                .unwrap_or("The directory was still changing when the wait ended"),
        ),
        (Some(left), Some(still)) => format!("{left}. {}; {LEAVE_OUT}", writers(&still)),
        (Some(left), None) => format!(
            "{left}. The server writes them back itself; wait again, with a longer --timeout if it needs more time: {wait}; {LEAVE_OUT}"
        ),
    }
}

/// How a member of a rejected set that keeps failing to write back can be left out: the
/// write-back skips a member whose object went, as it skips one whose object changed.
const LEAVE_OUT: &str = "if the write-back of a file keeps failing and you no longer need the file, delete its object through the API, which the write-back then skips";

/// The sentence that opens advice while `rejected` files of a rejected deletion set are left to
/// write back, which holds back every wait; `None` when none are.
fn rejected_left(rejected: u64) -> Option<String> {
    let (files, remain, their) = match rejected {
        0 => return None,
        1 => ("1 file".to_owned(), "remains", "its"),
        count => (format!("{count} files"), "remain", "their"),
    };
    Some(format!(
        "{files} of the rejected deletion set {remain} to write back, and the sync_deletions_rejected log names why if {their} write-back stopped"
    ))
}

/// What the wait's last pass still saw changing in the tree, if it saw anything.
fn still_changing(holdup: Option<Holdup>, pass: Option<&Reconciliation>) -> Option<String> {
    let files = |count: u64| {
        if count == 1 {
            "1 file was"
        } else {
            "files were"
        }
    };
    Some(match (holdup, pass) {
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
        _ => return None,
    })
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
    const fn past(self) -> &'static str {
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

/// The refusal of `decision` on the deletion set `revision`, which `refusal` says what became
/// of, and what to do instead: a decided set names the status or the wait that follows its
/// decision, and one an API write raced the same decision again. While another set waits, the
/// refusal says only that the revision is not that set, since the root no longer knows why,
/// and counts what the other set holds. It never hands over an approval of a set the operator
/// did not inspect: a revision mistyped, kept from before a restart or read from another
/// server's log would otherwise turn into a ready command that deletes whatever waits now. A
/// rejection deletes nothing, so a refused one names the rejection of the set that waits.
pub(crate) fn stale_decision_remedy(
    decision: Decision,
    revision: OpaqueId,
    refusal: Refusal,
) -> String {
    let past = decision.past();
    let revision = revision.to_hex();
    match refusal {
        Refusal::Replaced {
            revision: current,
            missing,
            tracked,
        } => {
            let current = current.to_hex();
            let inspect = Advised::Deletions(Some(&current));
            let not_it = format!(
                "Deletion set {revision} was not {past}: it is not the deletion set that waits for a decision now (it was replaced, decided before, taken before a restart, or not issued by this server). Deletion set {current} waits instead, and {}",
                missing_of(missing, tracked)
            );
            match decision {
                Decision::Approve => format!(
                    "{not_it}; approving it would delete their objects. Inspect it: {inspect}; {MOUNT_FIRST}; approve that revision only after checking that it holds the files you meant to delete"
                ),
                Decision::Reject => format!(
                    "{not_it}. Inspect it: {inspect}; {MOUNT_FIRST}; to write its files back from the objects: {}",
                    Advised::Decide(Decision::Reject, &current)
                ),
            }
        }
        Refusal::Approved => format!(
            "Deletion set {revision} was already approved, and its objects are deleted or being deleted; a deletion set is decided once, so it was not {past} now. The status names any deletion set that waits for a decision: {}",
            Advised::Status
        ),
        Refusal::Rejected => format!(
            "Deletion set {revision} was already rejected, and a deletion set is decided once, so it was not {past} now. Its files are written back from their objects; wait for that to finish: {}; if you still want them gone, delete the files again after the write-back ends",
            Advised::Wait
        ),
        Refusal::Moved => format!(
            "Deletion set {revision} was not {past}: an API write arrived while the decision compared the set with the directory. The set still waits for a decision; send it again: {}",
            Advised::Decide(decision, &revision)
        ),
        Refusal::Settled => format!(
            "Deletion set {revision} was not {past}: no deletion set waits for a decision now. The missing files may have come back, a later pass may have found nothing that needs a decision, or the inventory that replaces the set has not finished. Wait for synchronization, which names any deletion set that waits then: {}",
            Advised::Wait
        ),
    }
}

/// The refusal of a page of the deletions view for the set `revision`, which is no longer the set
/// the status reads, and what `refusal` says became of it. A listing decides nothing, so it names
/// the view of the set that waits instead, if one does, and never a decision.
pub(crate) fn stale_deletions_view_remedy(revision: OpaqueId, refusal: Refusal) -> String {
    let revision = revision.to_hex();
    match refusal {
        Refusal::Approved => format!(
            "Deletion set {revision} was approved, and its objects are deleted or being deleted, so its pages are no longer listed. The status names any deletion set that waits for a decision: {}",
            Advised::Status
        ),
        Refusal::Rejected => format!(
            "Deletion set {revision} was rejected, and its files are written back from their objects, so its pages are no longer listed; wait for the write-back to finish: {}",
            Advised::Wait
        ),
        Refusal::Replaced {
            revision: current,
            missing,
            tracked,
        } => {
            let current = current.to_hex();
            format!(
                "Deletion set {revision} is not the deletion set that waits for a decision now (it was replaced, decided before, taken before a restart, or not issued by this server). Deletion set {current} waits instead, and {}; list it: {}",
                missing_of(missing, tracked),
                Advised::Deletions(Some(&current))
            )
        }
        Refusal::Moved => format!(
            "Deletion set {revision} still waits for a decision, but this page cannot continue from the cursor it was given; list the set again from its first page: {}",
            Advised::Deletions(Some(&revision))
        ),
        Refusal::Settled => format!(
            "Deletion set {revision} is not listed: no deletion set waits for a decision now. The missing files may have come back, a later pass may have found nothing that needs a decision, or the inventory that replaces the set has not finished. Wait for synchronization, which names any deletion set that waits then: {}",
            Advised::Wait
        ),
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WaitFindings {
    /// The objects the API holds whose names the tree cannot.
    pub unmapped: u64,
    /// The observation revision these counts were read at, which binds the findings view.
    pub observation_revision: i64,
    /// For a wait that ended on findings, the first path stops, which its answer names.
    pub stops: Listed<ListedStop>,
    /// For a wait that succeeded, the files that stand for several objects and stay frozen,
    /// which its answer names, since edits to them are not uploaded.
    pub frozen: Listed<FrozenFile>,
}

/// The first items of a listing a wait's answer names, and how many there are in all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed<T> {
    pub first: Vec<T>,
    pub total: u64,
}

impl<T> Default for Listed<T> {
    fn default() -> Self {
        Self {
            first: Vec::new(),
            total: 0,
        }
    }
}

impl<T> Listed<T> {
    /// The items a wait's answer names, at most this many; the rest are only counted.
    pub(crate) const NAMED: usize = 5;

    /// Names the first [`Self::NAMED`] of `items`, of `total` in all.
    pub(crate) fn of(mut items: Vec<T>, total: u64) -> Self {
        items.truncate(Self::NAMED);
        Self {
            first: items,
            total,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.total == 0
    }
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
        let last = self.holdup();
        let findings = self.findings;
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
            Ending::Held(holdup) if self.reason.is_in_progress() => Some(unsettled_remedy(
                Some(holdup),
                pass,
                at_hand.rejected_writeback,
            )),
            Ending::Held(holdup) => holdup.remedy(at_hand),
            Ending::Unfinished {
                why: Unfinished::Timeout | Unfinished::SourceChanging,
                last,
            } => Some(unsettled_remedy(last, pass, at_hand.rejected_writeback)),
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
        let found = self.findings.as_ref().map_or(0, |found| found.unmapped);
        let held = holdup.with_unmapped(unmapped, found);
        if held == holdup {
            return self;
        }
        Self::held(held, self.journal_fence, self.reconciliation).with_findings(self.findings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::advice::AwaitingSet;

    /// The remedy of a set waiting for a decision counts the missing files against the tracked
    /// ones, and names the inspection first, then a failed mount to fix before anything else,
    /// then the ways out that keep the objects, rejecting and restoring by hand, before the
    /// approval that deletes them; it says which files call for which decision. Without a set at
    /// hand it names the same steps without a revision.
    #[test]
    fn a_set_awaiting_a_decision_names_the_ways_that_keep_the_objects_first() {
        let set = AwaitingSet {
            revision: OpaqueId::SAMPLE,
            missing: 150,
            tracked: 200,
            cause: None,
        };
        let revision = set.revision.to_hex();
        let remedy = deletion_approval_remedy(AtHand {
            deletion: Some(set),
            ..AtHand::default()
        });
        assert!(
            remedy.starts_with("150 of the 200 files that synchronization tracked are missing"),
            "{remedy}"
        );
        let order = |remedy: &str, steps: &[String]| {
            let at: Vec<_> = steps
                .iter()
                .map(|step| {
                    remedy
                        .find(step.as_str())
                        .unwrap_or_else(|| panic!("{step}: {remedy}"))
                })
                .collect();
            assert!(at.is_sorted(), "{remedy}");
        };
        let steps = [
            Advised::Deletions(Some(&revision)).to_string(),
            "if a mount failed, fix the mount and restart the server".to_owned(),
            "if the files went by mistake, reject".to_owned(),
            Advised::Decide(Decision::Reject, &revision).to_string(),
            format!(
                "restore the files yourself and wait again: {}",
                Advised::Wait
            ),
            "if the files were deleted on purpose, approve".to_owned(),
            Advised::Decide(Decision::Approve, &revision).to_string(),
        ];
        order(&remedy, &steps);
        let one = deletion_approval_remedy(AtHand {
            deletion: Some(AwaitingSet {
                missing: 1,
                tracked: 1,
                ..set
            }),
            ..AtHand::default()
        });
        assert!(
            one.starts_with("1 of the 1 file that synchronization tracked is missing"),
            "{one}"
        );
        let without = deletion_approval_remedy(AtHand::default());
        order(
            &without,
            &[
                Advised::Deletions(None).to_string(),
                "fix the mount".to_owned(),
                "reject the set".to_owned(),
                format!("wait again: {}", Advised::Wait),
                "approve the set".to_owned(),
            ],
        );
    }

    /// The advice on a set that waits opens with the one sentence that says why it waits, when
    /// the inventory knew: a bucket directory missing or showing none of its files, named, the
    /// threshold, or files of an earlier undecided set, which also says that files deleted
    /// meanwhile joined it. It states what was seen, never that the files were deleted, and the
    /// counts and commands follow unchanged.
    #[test]
    fn a_set_awaiting_a_decision_says_first_why_it_waits() {
        let bucket = crate::storage::admin::BucketName::new("photos").unwrap();
        let without = deletion_approval_remedy(AtHand {
            deletion: Some(AwaitingSet {
                revision: OpaqueId::SAMPLE,
                missing: 2,
                tracked: 2,
                cause: None,
            }),
            ..AtHand::default()
        });
        for (cause, opening) in [
            (
                WaitCause::Bucket {
                    bucket: bucket.clone(),
                    gone: BucketGone::Missing,
                },
                "The directory of bucket photos is missing, as it is when a mount did not come up,",
            ),
            (
                WaitCause::Bucket {
                    bucket,
                    gone: BucketGone::Emptied,
                },
                "The directory of bucket photos has shown none of its tracked files since the server started, as it does when a mount came up empty,",
            ),
            (
                WaitCause::Threshold,
                "At least 100 tracked files went missing",
            ),
            (
                WaitCause::Held,
                "The set holds files of an earlier deletion set that nobody approved or rejected, so it waits too; files deleted while a set waits join it",
            ),
        ] {
            let remedy = deletion_approval_remedy(AtHand {
                deletion: Some(AwaitingSet {
                    revision: OpaqueId::SAMPLE,
                    missing: 2,
                    tracked: 2,
                    cause: Some(&cause),
                }),
                ..AtHand::default()
            });
            assert!(remedy.starts_with(opening), "{remedy}");
            assert_eq!(
                remedy.matches(". ").count(),
                without.matches(". ").count() + 1
            );
            assert!(remedy.ends_with(&without), "{remedy}");
            assert!(!remedy.contains("deleted while the server"), "{remedy}");
        }
    }

    /// A refused decision names what became of its set and the command that follows: the
    /// status or the wait for a set decided already, the same decision again for one an API
    /// write raced, and the status when nothing waits. While another set waits, it counts that
    /// set's missing files and names its view and the failed mount to fix first, without saying
    /// why the refused revision is not that set; a refused rejection names the rejection of
    /// that set, and a refused approval names no approval at all, so a revision that was never
    /// the inspected one cannot become a ready command that deletes. A refusal never names the
    /// other decision. The sample identifier advice renders is the revision the command samples.
    #[test]
    fn a_refused_decision_names_what_became_of_its_set() {
        assert_eq!(
            OpaqueId::SAMPLE.to_hex(),
            super::super::advice::SAMPLE_DELETION_REVISION
        );
        let revision: OpaqueId = "0123456789abcdef0123456789abcdef".parse().unwrap();
        let current: OpaqueId = "fedcba9876543210fedcba9876543210".parse().unwrap();
        let replaced = Refusal::Replaced {
            revision: current,
            missing: 650,
            tracked: 800,
        };
        for decision in Decision::ALL {
            let refused = stale_decision_remedy(decision, revision, replaced);
            for part in [
                "it is not the deletion set that waits for a decision now (it was replaced, decided before, taken before a restart, or not issued by this server)".to_owned(),
                format!("Deletion set {current} waits instead, and 650 of the 800 files that synchronization tracked are missing"),
                format!("gcs-mimic sync status --view deletions --revision {current} --endpoint URL;"),
                "if a mount failed, fix the mount and restart the server".to_owned(),
            ] {
                assert!(refused.contains(&part), "{decision:?}: {refused}");
            }
            assert!(!refused.contains("replaced it"), "{decision:?}: {refused}");
        }
        let approval = stale_decision_remedy(Decision::Approve, revision, replaced);
        assert!(!approval.contains("gcs-mimic sync approve"), "{approval}");
        assert!(
            approval.ends_with(
                "approve that revision only after checking that it holds the files you meant to delete"
            ),
            "{approval}"
        );
        let rejection = stale_decision_remedy(Decision::Reject, revision, replaced);
        assert!(
            rejection.ends_with(&format!(
                "to write its files back from the objects: gcs-mimic sync reject --deletion-set {current} --endpoint URL"
            )),
            "{rejection}"
        );
        for decision in Decision::ALL {
            let (name, past) = (decision.as_str(), decision.past());
            let same = |set: OpaqueId| format!("gcs-mimic sync {name} --deletion-set {set}");
            for (refusal, parts) in [
                (
                    Refusal::Approved,
                    vec![
                        "was already approved".to_owned(),
                        "its objects are deleted or being deleted".to_owned(),
                        format!("it was not {past} now"),
                        "gcs-mimic sync status --endpoint URL".to_owned(),
                    ],
                ),
                (
                    Refusal::Rejected,
                    vec![
                        "was already rejected".to_owned(),
                        format!("it was not {past} now"),
                        "gcs-mimic sync wait --endpoint URL;".to_owned(),
                        "if you still want them gone, delete the files again after the write-back ends".to_owned(),
                    ],
                ),
                (
                    replaced,
                    vec![format!("Deletion set {current} waits instead")],
                ),
                (Refusal::Moved, vec![same(revision)]),
                (
                    Refusal::Settled,
                    vec![
                        "no deletion set waits for a decision now".to_owned(),
                        "or the inventory that replaces the set has not finished".to_owned(),
                        "gcs-mimic sync wait --endpoint URL".to_owned(),
                    ],
                ),
            ] {
                let remedy = stale_decision_remedy(decision, revision, refusal);
                assert!(
                    remedy.starts_with(&format!("Deletion set {revision} ")),
                    "{remedy}"
                );
                for part in parts {
                    assert!(remedy.contains(&part), "{decision:?} {refusal:?}: {remedy}");
                }
                for other in Decision::ALL.into_iter().filter(|other| *other != decision) {
                    let command = format!("gcs-mimic sync {}", other.as_str());
                    assert!(
                        !remedy.contains(&command),
                        "{decision:?} {refusal:?}: {remedy}"
                    );
                }
            }
        }
    }

    /// A decision parses from its command, which the control route ends with and the accepted
    /// answer names it by, and from nothing else.
    #[test]
    fn decisions_parse_from_the_command_their_route_ends_with() {
        for decision in Decision::ALL {
            assert_eq!(decision.as_str().parse::<Decision>(), Ok(decision));
            assert_eq!(
                decision.path(),
                format!("/_internal/sync/{}", decision.as_str())
            );
            assert_eq!(
                serde_json::to_value(decision).unwrap(),
                serde_json::Value::from(decision.as_str())
            );
        }
        for name in ["", "Approve", "approved", "/_internal/sync/approve"] {
            assert!(name.parse::<Decision>().is_err(), "{name}");
        }
    }

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

    /// Files of a rejected set left to write back open the advice of a wait that ran out of
    /// time or paused, counted, with the log that says why their write-back stopped. Since the
    /// server writes them itself, a wait with nothing else changing does not ask the programs
    /// writing in the directory to finish; what a pass saw changing keeps that request. Each
    /// says how to leave out a file whose write-back keeps failing, and the pause keeps its own
    /// remedy after the count.
    #[test]
    fn files_left_to_write_back_open_the_advice_of_a_wait() {
        let left = |rejected| AtHand {
            rejected_writeback: rejected,
            ..AtHand::default()
        };
        let writers = "Let the programs writing in the directory or through the API finish";
        let leave_out = "delete its object through the API, which the write-back then skips";
        for (rejected, opening) in [
            (
                1,
                "1 file of the rejected deletion set remains to write back, and the sync_deletions_rejected log names why if its write-back stopped. ",
            ),
            (
                3,
                "3 files of the rejected deletion set remain to write back, and the sync_deletions_rejected log names why if their write-back stopped. ",
            ),
        ] {
            let timed_out = WaitResult::timeout().remedy(left(rejected)).unwrap();
            assert!(timed_out.starts_with(opening), "{timed_out}");
            assert!(
                timed_out.contains("The server writes them back itself; wait again"),
                "{timed_out}"
            );
            assert!(!timed_out.contains(writers), "{timed_out}");
            assert!(timed_out.ends_with(leave_out), "{timed_out}");
            let observing = WaitResult::held(Holdup::Observing, None, None)
                .remedy(left(rejected))
                .unwrap();
            assert!(observing.starts_with(opening), "{observing}");
            assert!(observing.contains(writers), "{observing}");
            assert!(observing.ends_with(leave_out), "{observing}");
            let pause = pause_remedy(PauseReason::IoFailure, AtHand::default());
            let paused = WaitResult::held(Holdup::Paused(PauseReason::IoFailure), None, None)
                .remedy(left(rejected))
                .unwrap();
            assert_eq!(
                paused,
                format!(
                    "{opening}{pause}; if the write-back of a file keeps failing and you no longer need the file, {leave_out}, before you resume"
                )
            );
        }
        let timed_out = WaitResult::timeout().remedy(left(0)).unwrap();
        assert!(
            timed_out.starts_with(&format!(
                "The directory was still changing when the wait ended. {writers}"
            )),
            "{timed_out}"
        );
        assert!(!timed_out.contains("rejected"), "{timed_out}");
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
            ..AtHand::default()
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
            ..WaitFindings::default()
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
