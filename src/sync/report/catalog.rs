//! Every piece of advice that names a `gcs-mimic sync` command, rendered with sample revisions,
//! so that the command's tests can parse each one and check it against what the command accepts.

use super::{
    command::{Advised, AtHand, AwaitingSet},
    remedy::{
        Correction, export_retry_remedy, newer_edit_remedy, occupied_directory_remedy,
        paused_control_remedy, resume_retry_remedy, stale_decision_remedy,
        stale_deletions_view_remedy, unclassified_collision_remedy, unsettled_remedy,
    },
    verdict::Holdup,
};
use crate::{
    storage::sync_state::{
        OpaqueId, deletions::Refusal, observation::PauseReason, writeback::Halt,
    },
    sync::{Decision, entries::EntryProblem},
};

/// The set the catalog renders where an answer would have a real one at hand.
const SAMPLE_AWAITING_SET: AwaitingSet<'static> = AwaitingSet {
    revision: OpaqueId::SAMPLE,
    missing: 150,
    tracked: 200,
    cause: None,
};

/// A findings revision and a deletion revision of the shapes the command takes, which the
/// catalog renders where an answer would have real ones at hand.
#[doc(hidden)]
pub const SAMPLE_FINDINGS_REVISION: &str = "000102030405060708090a0b0c0d0e0f1011121314151617";
#[doc(hidden)]
pub const SAMPLE_DELETION_REVISION: &str = "000102030405060708090a0b0c0d0e0f";

/// Every command advice can name, rendered with the sample revisions.
#[doc(hidden)]
pub fn commands() -> Vec<String> {
    let all = [
        Advised::Status,
        Advised::Rescan,
        Advised::Wait,
        Advised::Conflicts,
        Advised::Findings(Some(SAMPLE_FINDINGS_REVISION)),
        Advised::Findings(None),
        Advised::Deletions(Some(SAMPLE_DELETION_REVISION)),
        Advised::Deletions(None),
    ]
    .into_iter()
    .chain(Decision::ALL.map(|decision| Advised::Decide(decision, SAMPLE_DELETION_REVISION)));
    // Naming every variant makes a new one fail to compile here, beside the list it must join.
    let listed = |command: Advised<'_>| match command {
        Advised::Status
        | Advised::Rescan
        | Advised::Wait
        | Advised::Conflicts
        | Advised::Findings(_)
        | Advised::Deletions(_)
        | Advised::Decide(..) => command.to_string(),
    };
    all.map(listed).collect()
}

/// Every piece of advice the server gives that names a command, with [`ENDPOINT`] left in: the
/// remedy of every holdup with and without the revisions at hand, which a wait's answer gives,
/// and for a pause also the `sync_paused` event, the root-stop finding and a failed rescan; of
/// a wait that ran out of time; of every correction outside a pause, which findings and events
/// give; of every refusal of each decision on a deletion set that no longer waits for it, and
/// of a page of the deletions view of such a set; of
/// the export events and the finding carried over from an older release; of a failed resume,
/// which the `sync_resume_failed` event also gives; of a control a pause refuses; and of a
/// directory that holds an object's file name.
#[doc(hidden)]
pub fn catalog() -> Vec<String> {
    let at_hand = [
        AtHand::default(),
        AtHand {
            findings_revision: Some(SAMPLE_FINDINGS_REVISION),
            deletion: Some(SAMPLE_AWAITING_SET),
            rejected_writeback: 2,
        },
    ];
    let holdups = PauseReason::ALL.into_iter().map(Holdup::Paused).chain([
        Holdup::Writeback(Halt::RecoveryRequired),
        Holdup::SourceChanging,
        Holdup::SourceUnrepresentable,
        Holdup::SourceUnreadable,
        Holdup::Observing,
        Holdup::DeletionApproval,
        Holdup::FindingsPending,
        Holdup::UnmappedObjects { count: 1 },
        Holdup::UnmappedObjects { count: 2 },
        Holdup::Ready,
    ]);
    // Naming every variant makes a new one fail to compile here, beside the list it must join.
    let listed = |holdup: Holdup| match holdup {
        Holdup::Paused(_)
        | Holdup::Writeback(_)
        | Holdup::SourceChanging
        | Holdup::SourceUnrepresentable
        | Holdup::SourceUnreadable
        | Holdup::Observing
        | Holdup::DeletionApproval
        | Holdup::FindingsPending
        | Holdup::UnmappedObjects { .. }
        | Holdup::Ready => holdup,
    };
    let corrections = [
        Correction::UnrepresentableEntries,
        Correction::UnreadableFile,
        Correction::Entry(EntryProblem::UnmappableName),
        Correction::Entry(EntryProblem::UnsafeEntry),
        Correction::Entry(EntryProblem::Unreadable),
        Correction::Entry(EntryProblem::PathCollision),
        Correction::Entry(EntryProblem::DeletedBucket),
        Correction::UnrepresentableName,
        Correction::CollidingName,
        Correction::ExportConflict,
        Correction::ChangingPath,
        Correction::PathIoFailure,
        Correction::ReplacedEntry,
        Correction::BlockedParent,
        Correction::AliasedName,
        Correction::UnreadablePath,
        Correction::AliasedObjects,
        Correction::EditedAlias,
    ];
    let correction_listed = |correction: Correction| match correction {
        Correction::UnrepresentableEntries
        | Correction::UnreadableFile
        | Correction::Entry(
            EntryProblem::UnmappableName
            | EntryProblem::UnsafeEntry
            | EntryProblem::Unreadable
            | EntryProblem::PathCollision
            | EntryProblem::DeletedBucket,
        )
        | Correction::UnrepresentableName
        | Correction::CollidingName
        | Correction::ExportConflict
        | Correction::ChangingPath
        | Correction::PathIoFailure
        | Correction::ReplacedEntry
        | Correction::BlockedParent
        | Correction::AliasedName
        | Correction::UnreadablePath
        | Correction::AliasedObjects
        | Correction::EditedAlias => correction.remedy(),
    };
    holdups
        .map(listed)
        .flat_map(|holdup| {
            at_hand
                .into_iter()
                .filter_map(move |at_hand| holdup.remedy(at_hand))
        })
        .chain(corrections.map(correction_listed))
        .chain(refusal_remedies())
        .chain([
            unsettled_remedy(None, None, 0),
            unsettled_remedy(Some(Holdup::Observing), None, 2),
            newer_edit_remedy(),
            export_retry_remedy(),
            unclassified_collision_remedy(),
            resume_retry_remedy(),
            paused_control_remedy(),
            occupied_directory_remedy(),
        ])
        .collect()
}

/// The advice of every refusal of each decision on a deletion set that no longer waits for it,
/// and of a page of the deletions view of such a set, as [`catalog`] lists it.
fn refusal_remedies() -> impl Iterator<Item = String> {
    let refusals = [
        Refusal::Approved,
        Refusal::Rejected,
        Refusal::Replaced {
            revision: OpaqueId::SAMPLE,
            missing: SAMPLE_AWAITING_SET.missing,
            tracked: SAMPLE_AWAITING_SET.tracked,
        },
        Refusal::Moved,
        Refusal::Settled,
    ];
    let listed = |refusal: Refusal| match refusal {
        Refusal::Approved
        | Refusal::Rejected
        | Refusal::Replaced { .. }
        | Refusal::Moved
        | Refusal::Settled => refusal,
    };
    refusals.map(listed).into_iter().flat_map(|refusal| {
        Decision::ALL
            .map(|decision| stale_decision_remedy(decision, OpaqueId::SAMPLE, refusal))
            .into_iter()
            .chain([stale_deletions_view_remedy(OpaqueId::SAMPLE, refusal)])
    })
}
