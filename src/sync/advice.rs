//! The `gcs-mimic sync` commands that advice tells an operator to run. Advice names a command
//! only by rendering an [`Advised`] value, and the command's tests parse every rendering and
//! check every piece of advice in [`catalog`], so advice cannot name a command, an option or a
//! value the command would refuse.

use super::control::{
    WaitReason, occupied_directory_remedy, pause_remedy, paused_control_remedy, resume_retry_remedy,
};
use crate::storage::sync_state::observation::PauseReason;
use std::fmt;

/// The endpoint option every advised command ends with: the one placeholder advice keeps, since
/// the server cannot know which origin reaches it. The command replaces it, through
/// [`with_connection`], with the options that reached the server when it prints advice; an
/// operator reading a log puts in the server's endpoint.
pub const ENDPOINT: &str = "--endpoint URL";

/// A `gcs-mimic sync` command, complete but for [`ENDPOINT`]. A revision it names comes from
/// what the advice has at hand; advice without one names [`Advised::Status`], which reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advised<'a> {
    Status,
    Rescan,
    Wait,
    Conflicts,
    /// The findings view at this findings revision.
    Findings(&'a str),
    /// The deletion view of the deletion set at this revision.
    Deletions(&'a str),
    /// The approval of the deletion set at this revision.
    Approve(&'a str),
}

impl fmt::Display for Advised<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("gcs-mimic sync ")?;
        match self {
            Self::Status => f.write_str("status")?,
            Self::Rescan => f.write_str("rescan")?,
            Self::Wait => f.write_str("wait")?,
            Self::Conflicts => f.write_str("conflicts")?,
            Self::Findings(revision) => write!(f, "status --view findings --revision {revision}")?,
            Self::Deletions(revision) => {
                write!(f, "status --view deletions --revision {revision}")?;
            }
            Self::Approve(revision) => write!(f, "approve --deletion-set {revision}")?,
        }
        write!(f, " {ENDPOINT}")
    }
}

/// `advice` with [`ENDPOINT`] replaced by `connection`, the options that reached the server.
pub fn with_connection(advice: &str, connection: &str) -> String {
    advice.replace(ENDPOINT, connection)
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
        Advised::Findings(SAMPLE_FINDINGS_REVISION),
        Advised::Deletions(SAMPLE_DELETION_REVISION),
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
/// remedy of every pause, which the `sync_paused` event, the root-stop finding and a failed
/// rescan give; of every wait reason with and without the revisions at hand; of a failed resume,
/// which the `sync_resume_failed` event also gives; of a control a pause refuses; and of a
/// directory that holds an object's file name.
#[doc(hidden)]
pub fn catalog() -> Vec<String> {
    let at_hand = [
        AtHand::default(),
        AtHand {
            findings_revision: Some(SAMPLE_FINDINGS_REVISION),
            deletion_revision: Some(SAMPLE_DELETION_REVISION),
        },
    ];
    let waits = WaitReason::ALL.into_iter().flat_map(|reason| {
        at_hand
            .into_iter()
            .filter_map(move |at_hand| reason.remedy(None, at_hand))
    });
    PauseReason::ALL
        .into_iter()
        .flat_map(|reason| {
            at_hand
                .into_iter()
                .map(move |at_hand| pause_remedy(reason, at_hand))
        })
        .chain(waits)
        .chain([
            resume_retry_remedy(),
            paused_control_remedy(),
            occupied_directory_remedy(),
        ])
        .collect()
}
