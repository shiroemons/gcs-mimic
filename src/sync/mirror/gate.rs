//! What decides whether a pass may complete the baseline, infer deletions from missing files or
//! reset the deletion history. Only the working tree's side of a pass takes part: an object the
//! API holds and the tree cannot represent has no file, so no file of it can look deleted, and
//! counting it here stopped deletion detection for the whole root over one unexportable name.

use super::{MirrorStatusSender, Reconciliation};
use crate::{storage::sync_state::root::Baseline, sync::entries::EntryProblem};
use std::path::Path;

/// Working-tree entries that a pass could not take in: an unsafe or unmappable entry, a file that
/// is also an object's parent directory, or the non-empty directory of a bucket deleted through
/// the API. A tracked file missing from its path may have moved into one of them, so while any
/// remain the baseline cannot complete and no deletion is inferred.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UnrepresentableEntries(u64);

impl UnrepresentableEntries {
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

/// The tree side of a finished pass, which is all that the baseline, deletion and history
/// decisions read. Deferred work and conflicts count on either side: an API change still owed to
/// the tree keeps the baseline open as before, and so does an interrupted pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) struct PassGate {
    baseline: Baseline,
    interrupted: bool,
    deferred: u64,
    conflicting: u64,
    entries: UnrepresentableEntries,
}

impl PassGate {
    /// Whether nothing kept the pass from observing the whole tree: it ran to the end, deferred
    /// nothing, met no conflict and left no entry it could not take in.
    pub(super) fn settled(self) -> bool {
        !self.interrupted && self.deferred == 0 && self.conflicting == 0 && self.entries.0 == 0
    }

    /// Whether the tracked files this pass found missing may become deletion candidates, and
    /// whether a deletion set it leaves without candidates may reset the deletion history.
    pub(in crate::sync) fn infers_deletions(self) -> bool {
        self.baseline == Baseline::Complete && self.settled()
    }
}

impl Reconciliation {
    /// Counts an object or bucket the API holds that the working tree cannot hold. It is
    /// reported, and recorded as a finding where it has one, but decides nothing.
    pub(in crate::sync) fn count_unmapped_object(&mut self) {
        self.unrepresentable += 1;
        self.unmapped_objects += 1;
    }

    /// Counts a working-tree entry the pass could not take in and names it. Every count of
    /// `unrepresentable` that is not an unmapped object is one, so a count added without a side
    /// stops deletions instead of letting them through.
    pub(in crate::sync) fn count_unrepresentable_entry(
        &mut self,
        path: &Path,
        problem: EntryProblem,
    ) {
        self.unrepresentable += 1;
        self.entries.record(path, problem);
    }

    /// The working-tree entries among `unrepresentable`.
    pub(crate) fn unrepresentable_entries(&self) -> UnrepresentableEntries {
        UnrepresentableEntries(self.unrepresentable.saturating_sub(self.unmapped_objects))
    }

    /// The tree side of this pass, for the decisions that must not see unmapped objects.
    pub(in crate::sync) fn gate(&self) -> PassGate {
        PassGate {
            baseline: if self.baseline_complete {
                Baseline::Complete
            } else {
                Baseline::Activating
            },
            interrupted: self.interrupted,
            deferred: self.deferred,
            conflicting: self.conflicting,
            entries: self.unrepresentable_entries(),
        }
    }
}

/// Hands the entries an ingestion could not take in to the mirror's status, replacing those of
/// the previous one. An interrupted ingestion saw only part of the tree, so it keeps the last
/// complete list.
pub(super) fn report_entries(status: &MirrorStatusSender, ingested: &mut Reconciliation) {
    if !ingested.interrupted {
        status.report_entries(std::mem::take(&mut ingested.entries));
    }
}

/// Whether a reconciliation may record the baseline as complete. It is deferred while the
/// working tree still has entries that ingestion could not take in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BaselineGate {
    Allowed,
    Deferred,
}

impl BaselineGate {
    pub(super) fn after_ingest(ingested: PassGate) -> Self {
        if ingested.settled() {
            Self::Allowed
        } else {
            Self::Deferred
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the tree side decides: any number of unmapped objects leaves a pass settled, and one
    /// entry the tree could not take in unsettles it.
    #[test]
    fn unmapped_objects_never_hold_back_the_baseline_or_deletions() {
        let mut pass = Reconciliation {
            baseline_complete: true,
            ..Default::default()
        };
        for _ in 0..1000 {
            pass.count_unmapped_object();
        }
        assert_eq!(pass.unrepresentable, 1000);
        assert_eq!(pass.unrepresentable_entries().get(), 0);
        assert!(pass.gate().infers_deletions());
        assert_eq!(
            BaselineGate::after_ingest(pass.gate()),
            BaselineGate::Allowed
        );

        pass.count_unrepresentable_entry(Path::new("bucket/link"), EntryProblem::UnsafeEntry);
        assert_eq!(pass.unrepresentable_entries().get(), 1);
        assert_eq!(pass.entries.total(), 1);
        assert!(!pass.gate().settled());
        assert!(!pass.gate().infers_deletions());
        assert_eq!(
            BaselineGate::after_ingest(pass.gate()),
            BaselineGate::Deferred
        );
    }

    /// A count added without a side is a tree entry, so it stops deletions instead of letting
    /// them through.
    #[test]
    fn an_unsided_count_stops_deletions() {
        let pass = Reconciliation {
            baseline_complete: true,
            unrepresentable: 1,
            ..Default::default()
        };
        assert!(!pass.gate().infers_deletions());
    }

    /// Deferred work, a conflict and an interruption keep their effect on either side, and an
    /// incomplete baseline infers no deletion however settled the pass is.
    #[test]
    fn deferred_conflicting_interrupted_and_incomplete_passes_infer_no_deletion() {
        let settled = Reconciliation {
            baseline_complete: true,
            ..Default::default()
        };
        assert!(settled.gate().infers_deletions());
        for pass in [
            Reconciliation {
                deferred: 1,
                ..Default::default()
            },
            Reconciliation {
                conflicting: 1,
                ..Default::default()
            },
            Reconciliation {
                interrupted: true,
                ..Default::default()
            },
        ] {
            let pass = Reconciliation {
                baseline_complete: true,
                ..pass
            };
            assert!(!pass.gate().settled());
            assert!(!pass.gate().infers_deletions());
        }
        let incomplete = Reconciliation::default();
        assert!(incomplete.gate().settled());
        assert!(!incomplete.gate().infers_deletions());
    }
}
