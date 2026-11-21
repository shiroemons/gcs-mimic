//! What an operator does about each condition that holds synchronization back, as the advice that
//! answers, findings and log events give it. Each piece of advice names the commands to run only
//! through [`Advised`], and the catalog renders every one.

use super::{
    command::{Advised, AtHand, UnmappedCount},
    verdict::Holdup,
};
use crate::{
    storage::sync_state::{
        OpaqueId,
        deletions::{BucketGone, Refusal, WaitCause},
        observation::PauseReason,
    },
    sync::{
        Decision, Reconciliation,
        entries::{EntryProblem, path_text},
    },
};

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
                "The file at tree_path (path in the log) stands for this object and another (partner) whose names differ only in letter case or Unicode normalization, and it changed while frozen. The edit stays in the file and was not uploaded, since it may be meant for either object. To upload it, delete or rename through the API the object it is not meant for, and the next pass uploads it for the other, unless the API wrote that one since: then its write stays and the edit is kept as a conflict. To drop it, put the earlier bytes back or delete the file, which ends this stop"
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

#[cfg(test)]
mod tests;
