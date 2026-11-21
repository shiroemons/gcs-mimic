//! How a wait ended, and the answer it gives: the reason, the holdup its remedy follows and what
//! its last pass left.

use super::{
    command::AtHand,
    listing::WaitFindings,
    outcome::WaitReason,
    remedy::unsettled_remedy,
    verdict::{Holdup, UnmappedObjects},
};
use crate::{
    storage::{change_journal::JournalFence, sync_state::observation::PauseReason},
    sync::Reconciliation,
};

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
mod tests;
