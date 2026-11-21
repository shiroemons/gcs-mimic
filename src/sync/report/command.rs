//! The `gcs-mimic sync` commands that advice tells an operator to run. Advice names a command
//! only by rendering an [`Advised`] value, and the command's tests parse every rendering and
//! check every piece of advice in [`catalog`](super::catalog), so advice cannot name a command,
//! an option or a value the command would refuse.

use crate::{
    storage::sync_state::{
        OpaqueId,
        deletions::{DeletionSet, WaitCause},
    },
    sync::Decision,
};
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
    /// The decision on the deletion set at this revision: an approval deletes its objects, and a
    /// rejection writes its files back.
    Decide(Decision, &'a str),
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
            Self::Decide(decision, revision) => {
                write!(f, "{} --deletion-set {revision}", decision.as_str())?;
            }
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

/// `advice` as a log event gives it: without [`ENDPOINT`], which nothing fills in a log. A
/// command run beside the server finds it the way the commands the command's own advice names
/// do, and asks for --endpoint when it cannot; an answer keeps the placeholder, which the
/// command replaces with the options that reached the server.
pub(crate) fn logged(advice: &str) -> String {
    with_connection(advice, "")
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
    /// The deletion set that waits for a decision.
    pub(crate) deletion: Option<AwaitingSet<'a>>,
    /// The files of a rejected deletion set that the server has still to write back, which hold
    /// back every wait until they are back.
    pub(crate) rejected_writeback: u64,
}

/// The deletion set that waits for a decision, as advice names it: its revision, how many of
/// the files synchronization tracked are missing, which tells a deliberate deletion of a few
/// files from a directory that went as a whole, and why it waits, when the inventory of this
/// process that took it knew. Only [`Self::of`] a set that waits makes one, so advice never
/// names a decision on a set that already took one or never needed one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AwaitingSet<'a> {
    pub(crate) revision: OpaqueId,
    /// The tracked files that are missing, whose objects the set would delete.
    pub(crate) missing: u64,
    /// The files synchronization tracked when the set was taken.
    pub(crate) tracked: u64,
    pub(crate) cause: Option<&'a WaitCause>,
}

impl<'a> AwaitingSet<'a> {
    /// `set` as advice names it, while it waits for a decision.
    pub(crate) fn of(set: &'a DeletionSet) -> Option<Self> {
        set.awaits_decision().then_some(Self {
            revision: set.revision,
            missing: set.candidate_count,
            tracked: set.baseline_count,
            cause: set.seen.cause.as_ref(),
        })
    }
}
