//! What still holds synchronization back. A wait's answer and the status `state` of a running
//! mirror both come from [`Outstanding::first`], so the two cannot judge one condition apart.

use crate::storage::sync_state::{
    deletions::{DeletionAuthorization, DeletionSet},
    observation::{PauseReason, ScanStop, Summary},
    writeback::Stop,
};

/// What the working tree's side leaves undone. The status reads it from the stop and the baseline
/// the last scan recorded; a wait from what holds back its own pass, the gate that also decides
/// deletion detection, so that objects the tree cannot hold never stop a wait and an entry that
/// stops deletions never lets one succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TreeSide {
    /// Every entry of the tree was taken in.
    Settled,
    /// Files still changing or racing an API write, or exports skipped for a busy file that
    /// wait for their retry.
    Changing,
    /// Files that differ from their objects and wait for a conflict decision.
    Conflicting,
    /// Entries that no object can take, where a missing file may have gone, or files that no
    /// object accounts for yet.
    Unrepresentable,
    /// Nothing holds the tree back, but the baseline is not complete.
    Observing,
}

impl TreeSide {
    /// The tree side as the last scan recorded it.
    const fn recorded(observation: &Summary) -> Self {
        match observation.scan_stop {
            Some(ScanStop::SourceChanging) => Self::Changing,
            Some(ScanStop::SourceUnrepresentable) => Self::Unrepresentable,
            None if !observation.baseline_complete => Self::Observing,
            None => Self::Settled,
        }
    }
}

/// The conditions that keep the tree and the API apart, read at one observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Outstanding {
    /// The pause the catalog records.
    paused: Option<PauseReason>,
    /// The write-back state, when it is not `Ready`.
    writeback: Option<Stop>,
    tree: TreeSide,
    /// A guarded deletion set waits for an operator's approval.
    deletion_approval: bool,
    /// Findings that stop a wait, path stops, unfinished write-back or unresolved conflicts.
    findings_pending: bool,
}

/// The first condition that holds, in the one order a wait and the status share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Holdup {
    Paused(PauseReason),
    Writeback(Stop),
    SourceChanging,
    SourceUnrepresentable,
    /// Nothing that needs an operator holds, but the baseline is not complete, so no deletion is
    /// inferred yet.
    Observing,
    DeletionApproval,
    FindingsPending,
    Ready,
}

impl Outstanding {
    /// The conditions as the status reads them, with the tree side the last scan recorded.
    pub(crate) fn of_status(observation: &Summary, deletion: Option<&DeletionSet>) -> Self {
        Self::new(observation, deletion, TreeSide::recorded(observation))
    }

    /// The conditions with the `tree` side a reader found, such as a wait after its own pass,
    /// instead of the one an earlier scan recorded.
    pub(crate) fn new(
        observation: &Summary,
        deletion: Option<&DeletionSet>,
        tree: TreeSide,
    ) -> Self {
        Self {
            paused: observation.paused_reason,
            writeback: observation
                .writeback
                .as_ref()
                .map(|writeback| writeback.state)
                .filter(|state| *state != Stop::Ready),
            tree,
            // A guarded set waits only until an operator grants it. A granted set runs in the
            // pass that completes it, so it waits for nobody.
            deletion_approval: deletion.is_some_and(|set| {
                set.guarded && set.authorization == DeletionAuthorization::Withheld
            }),
            findings_pending: observation.blocking_findings
                + observation.path_stops
                + observation.pending_writeback
                + observation.unresolved_conflicts
                > 0,
        }
    }

    /// The first condition that holds. A pause and a write-back stop need an operator, so they
    /// come first; then what the tree still owes, since findings read before the tree settles may
    /// change; then what an operator must decide before the findings to inspect.
    pub(crate) const fn first(self) -> Holdup {
        if let Some(reason) = self.paused {
            return Holdup::Paused(reason);
        }
        if let Some(stop) = self.writeback {
            return Holdup::Writeback(stop);
        }
        match self.tree {
            TreeSide::Changing => Holdup::SourceChanging,
            TreeSide::Unrepresentable => Holdup::SourceUnrepresentable,
            TreeSide::Observing => Holdup::Observing,
            TreeSide::Settled | TreeSide::Conflicting if self.deletion_approval => {
                Holdup::DeletionApproval
            }
            TreeSide::Conflicting => Holdup::FindingsPending,
            TreeSide::Settled if self.findings_pending => Holdup::FindingsPending,
            TreeSide::Settled => Holdup::Ready,
        }
    }
}

#[cfg(test)]
mod tests;
