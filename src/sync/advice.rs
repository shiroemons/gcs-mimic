//! The `gcs-mimic sync` commands that advice tells an operator to run. Advice names a command
//! only by rendering an [`Advised`] value, and the command's tests parse every rendering and
//! check every piece of advice in [`catalog`], so advice cannot name a command, an option or a
//! value the command would refuse.

use super::{
    control::{
        Correction, export_retry_remedy, newer_edit_remedy, occupied_directory_remedy,
        paused_control_remedy, resume_retry_remedy, unclassified_collision_remedy,
        unsettled_remedy,
    },
    entries::EntryProblem,
    outstanding::Holdup,
};
use crate::storage::sync_state::{observation::PauseReason, writeback::Halt};
use std::fmt;

/// The endpoint option every advised command ends with: the one placeholder advice keeps, since
/// the server cannot know which origin reaches it. The command replaces it, through
/// [`with_connection`], with the options that reached the server when it prints advice; an
/// operator reading a log puts in the server's endpoint.
pub const ENDPOINT: &str = "--endpoint URL";

/// A `gcs-mimic sync` command, complete but for [`ENDPOINT`]. A revision it names comes from
/// what the advice has at hand. A view without one reads the revision that the status reports
/// when the command runs; an approval always names the revision of the set it approves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advised<'a> {
    Status,
    Rescan,
    Wait,
    Conflicts,
    /// The findings view at this findings revision, or at the current one.
    Findings(Option<&'a str>),
    /// The deletion view of the deletion set at this revision, or of the one awaiting approval.
    Deletions(Option<&'a str>),
    /// The approval of the deletion set at this revision.
    Approve(&'a str),
}

impl fmt::Display for Advised<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("gcs-mimic sync ")?;
        let view = |f: &mut fmt::Formatter<'_>, view, revision: Option<&str>| {
            write!(f, "status --view {view}")?;
            revision.map_or(Ok(()), |revision| write!(f, " --revision {revision}"))
        };
        match self {
            Self::Status => f.write_str("status")?,
            Self::Rescan => f.write_str("rescan")?,
            Self::Wait => f.write_str("wait")?,
            Self::Conflicts => f.write_str("conflicts")?,
            Self::Findings(revision) => view(f, "findings", *revision)?,
            Self::Deletions(revision) => view(f, "deletions", *revision)?,
            Self::Approve(revision) => write!(f, "approve --deletion-set {revision}")?,
        }
        write!(f, " {ENDPOINT}")
    }
}

/// `advice` with [`ENDPOINT`] replaced by `connection`, the options that reached the server. An
/// empty `connection`, from a command that found the server without options, drops the
/// placeholder with the space before it, since the command finds the server again.
pub fn with_connection(advice: &str, connection: &str) -> String {
    if connection.is_empty() {
        advice.replace(&format!(" {ENDPOINT}"), "")
    } else {
        advice.replace(ENDPOINT, connection)
    }
}

/// Objects in the API whose names the tree cannot hold, worded alike wherever a line names
/// them: the remedy of a wait that refuses them and the warning of a command that reports them.
/// Its display is the clause that opens such a line, in the singular for one object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnmappedCount(u64);

impl From<u64> for UnmappedCount {
    fn from(count: u64) -> Self {
        Self(count)
    }
}

impl UnmappedCount {
    /// The pronoun that refers back to the objects.
    pub const fn them(self) -> &'static str {
        if self.0 == 1 { "it" } else { "them" }
    }

    /// "stay", agreeing with the objects.
    pub const fn stay(self) -> &'static str {
        if self.0 == 1 { "stays" } else { "stay" }
    }
}

impl fmt::Display for UnmappedCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (objects, names) = if self.0 == 1 {
            ("object", "its name")
        } else {
            ("objects", "their names")
        };
        write!(
            f,
            "{} {objects} in the API cannot be written to the sync directory because of {names}",
            self.0
        )
    }
}

/// The revisions an answer has at hand, which the advice it gives fills in.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AtHand<'a> {
    /// The findings revision the answer read its findings at.
    pub(crate) findings_revision: Option<&'a str>,
    /// The revision of the deletion set that waits for approval.
    pub(crate) deletion_revision: Option<&'a str>,
}

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
        Advised::Approve(SAMPLE_DELETION_REVISION),
    ];
    // Naming every variant makes a new one fail to compile here, beside the list it must join.
    let listed = |command: Advised<'_>| match command {
        Advised::Status
        | Advised::Rescan
        | Advised::Wait
        | Advised::Conflicts
        | Advised::Findings(_)
        | Advised::Deletions(_)
        | Advised::Approve(_) => command.to_string(),
    };
    all.into_iter().map(listed).collect()
}

/// Every piece of advice the server gives that names a command, with [`ENDPOINT`] left in: the
/// remedy of every holdup with and without the revisions at hand, which a wait's answer gives,
/// and for a pause also the `sync_paused` event, the root-stop finding and a failed rescan; of
/// a wait that ran out of time; of every correction outside a pause, which findings and events
/// give; of the export events and the finding carried over from an older release; of a failed
/// resume, which the `sync_resume_failed` event also gives; of a control a pause refuses; and of
/// a directory that holds an object's file name.
#[doc(hidden)]
pub fn catalog() -> Vec<String> {
    let at_hand = [
        AtHand::default(),
        AtHand {
            findings_revision: Some(SAMPLE_FINDINGS_REVISION),
            deletion_revision: Some(SAMPLE_DELETION_REVISION),
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
        Correction::ExportConflict,
        Correction::ChangingPath,
        Correction::PathIoFailure,
        Correction::ReplacedEntry,
        Correction::BlockedParent,
        Correction::AliasedName,
        Correction::UnreadablePath,
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
        | Correction::ExportConflict
        | Correction::ChangingPath
        | Correction::PathIoFailure
        | Correction::ReplacedEntry
        | Correction::BlockedParent
        | Correction::AliasedName
        | Correction::UnreadablePath => correction.remedy(),
    };
    holdups
        .map(listed)
        .flat_map(|holdup| {
            at_hand
                .into_iter()
                .filter_map(move |at_hand| holdup.remedy(at_hand))
        })
        .chain(corrections.map(correction_listed))
        .chain([
            unsettled_remedy(None, None),
            newer_edit_remedy(),
            export_retry_remedy(),
            unclassified_collision_remedy(),
            resume_retry_remedy(),
            paused_control_remedy(),
            occupied_directory_remedy(),
        ])
        .collect()
}
