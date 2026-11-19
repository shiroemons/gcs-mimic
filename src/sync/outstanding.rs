//! What still holds synchronization back. A wait's answer and the status `state` of a running
//! mirror both come from [`Outstanding::first`], and the tree side of both from the hold of a
//! pass, so the two judge one condition alike. They read it at different times: the status
//! reads what the last scan recorded, and a wait what its own pass found, which also counts the
//! exports it replayed and the reconciliation it ran, so they can differ for the length of a
//! pass.

use super::{
    advice::AtHand,
    control::{
        deletion_approval_remedy, findings_remedy, pause_remedy, unmapped_objects_remedy,
        unrepresentable_remedy, unsettled_remedy,
    },
};
use crate::storage::sync_state::{
    deletions::{DeletionAuthorization, DeletionSet},
    observation::{PauseReason, ScanStop, Summary},
    writeback::Stop,
};

/// What the working tree's side leaves undone, from the hold of a pass: the one that also
/// decides deletion detection, so that objects the tree cannot hold never stop a wait and an
/// entry that stops deletions never lets one succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TreeSide {
    /// Every entry of the tree was taken in.
    Settled,
    /// Files still changing, racing an API write or written after the pass took the tree in, or
    /// exports skipped for a busy file that wait for their retry. A later pass takes them in.
    Changing,
    /// Files that differ from their objects and wait for a conflict decision.
    Conflicting,
    /// Entries that no object can take, where a missing file may have gone.
    Unrepresentable,
    /// Entries a pass could not read for a reason waiting does not clear, such as a missing
    /// permission.
    Unreadable,
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

    /// What a scan records for this side, which [`TreeSide::recorded`] reads back. A conflict
    /// is counted on its own and the baseline recorded on its own, and an unreadable entry
    /// records as an unrepresentable one, which the status names alike.
    pub(crate) const fn scan_stop(self) -> Option<ScanStop> {
        match self {
            Self::Changing => Some(ScanStop::SourceChanging),
            Self::Unrepresentable | Self::Unreadable => Some(ScanStop::SourceUnrepresentable),
            Self::Settled | Self::Conflicting | Self::Observing => None,
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
    /// Entries a pass could not read for a reason waiting does not clear.
    SourceUnreadable,
    /// Nothing that needs an operator holds, but the baseline is not complete, so no deletion is
    /// inferred yet.
    Observing,
    DeletionApproval,
    FindingsPending,
    /// Objects whose names the tree cannot hold remain, and the wait was asked to refuse them.
    UnmappedObjects {
        count: u64,
    },
    Ready,
}

/// What a wait does about objects whose names the tree cannot hold. They stay reachable through
/// the API, so they hold nothing else back; only the answer of a wait that is otherwise ready
/// differs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnmappedObjects {
    /// Succeed, and count them in the answer.
    #[default]
    Report,
    /// Fail while any remain, for a caller that needs every object in the tree.
    Refuse,
}

impl UnmappedObjects {
    /// The policy of a wait request whose `require_no_findings` is `required`.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "forwards the wire boolean require_no_findings, which the request body and the command's flag carry"
    )]
    pub const fn from_require_no_findings(required: bool) -> Self {
        if required { Self::Refuse } else { Self::Report }
    }
}

impl Holdup {
    /// The holdup of a wait that treats objects whose names the tree cannot hold by `unmapped`,
    /// of which `objects` remain, once the mirror judged it `self` without them. They never
    /// hold back a wait the mirror keeps waiting for, so only a ready wait changes: one that
    /// refuses them is held back by those that remain.
    pub(crate) const fn with_unmapped(self, unmapped: UnmappedObjects, objects: u64) -> Self {
        match (self, unmapped) {
            (Self::Ready, UnmappedObjects::Refuse) if objects > 0 => {
                Self::UnmappedObjects { count: objects }
            }
            (holdup, UnmappedObjects::Report | UnmappedObjects::Refuse) => holdup,
        }
    }

    /// What lets synchronization go past this holdup, naming the commands to run with the
    /// revisions `at_hand`; nothing for `Ready`. Work still under way gets the advice of
    /// [`unsettled_remedy`] without a pass to name what remained, which a wait's answer adds.
    pub(crate) fn remedy(self, at_hand: AtHand<'_>) -> Option<String> {
        Some(match self {
            Self::Paused(reason) => pause_remedy(reason, at_hand),
            Self::Writeback(_) | Self::FindingsPending => findings_remedy(at_hand),
            Self::SourceChanging | Self::Observing => unsettled_remedy(Some(self), None),
            Self::SourceUnrepresentable | Self::SourceUnreadable => unrepresentable_remedy(),
            Self::DeletionApproval => deletion_approval_remedy(at_hand),
            Self::UnmappedObjects { count } => unmapped_objects_remedy(count, at_hand),
            Self::Ready => return None,
        })
    }
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
            TreeSide::Unreadable => Holdup::SourceUnreadable,
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
