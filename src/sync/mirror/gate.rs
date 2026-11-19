//! The one answer to what holds back the baseline and the deletion inventory after a pass:
//! whether a reconciliation may complete the baseline, and whether missing files may become
//! deletion candidates or reset the deletion history. Only the working tree's side of a pass
//! takes part: an object the API holds and the tree cannot represent has no file, so no file of
//! it can look deleted, and counting it here stopped deletion detection for the whole root over
//! one unexportable name.

use super::{EntryReportSender, Reconciliation};
use crate::{
    storage::sync_state::root::Baseline,
    sync::entries::{EntryProblem, Skip},
};
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

/// The tree side of a finished pass, which is all that [`PassGate::hold`] reads. Deferred work
/// and conflicts count on either side: an API change still owed to the tree keeps the baseline
/// open as before, and so does an interrupted pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) struct PassGate {
    baseline: Baseline,
    interrupted: bool,
    deferred: u64,
    conflicting: u64,
    entries: UnrepresentableEntries,
    unimported: u64,
}

/// What holds back the baseline and the deletion inventory after a pass, in the order
/// [`PassGate::hold`] looks for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) enum Hold {
    /// The pass stopped before it saw the whole tree.
    Interrupted,
    /// Files the pass left for a later one: still changing, unreadable, or racing an API write.
    Deferred,
    /// Files that differ from their objects and wait for a conflict decision.
    Conflicting,
    /// Tree entries no object can take, where a missing file may have gone.
    TreeEntries,
    /// Files no object accounts for yet, such as the new name of a renamed file.
    Unimported,
    /// Nothing in the pass holds anything back, but the baseline is not complete, so a missing
    /// file is not yet a deletion.
    BaselineOpen,
    /// Nothing holds anything back.
    Clear,
}

impl PassGate {
    /// What holds back the baseline and the deletion inventory after this pass. Every decision
    /// reads this answer instead of testing the counts again.
    pub(in crate::sync) fn hold(self) -> Hold {
        match (
            self.interrupted,
            self.deferred,
            self.conflicting,
            self.entries.0,
            self.unimported,
            self.baseline,
        ) {
            (true, ..) => Hold::Interrupted,
            (false, 1.., ..) => Hold::Deferred,
            (false, 0, 1.., ..) => Hold::Conflicting,
            (false, 0, 0, 1.., ..) => Hold::TreeEntries,
            (false, 0, 0, 0, 1.., _) => Hold::Unimported,
            (false, 0, 0, 0, 0, Baseline::Activating) => Hold::BaselineOpen,
            (false, 0, 0, 0, 0, Baseline::Complete) => Hold::Clear,
        }
    }
}

impl Hold {
    /// Whether a reconciliation after this pass may record the baseline as complete.
    pub(in crate::sync) fn completes_baseline(self) -> bool {
        match self {
            Self::BaselineOpen | Self::Clear => true,
            Self::Interrupted
            | Self::Deferred
            | Self::Conflicting
            | Self::TreeEntries
            | Self::Unimported => false,
        }
    }

    /// Whether the tracked files this pass found missing may become deletion candidates, and
    /// whether a deletion set it leaves without candidates may reset the deletion history.
    pub(in crate::sync) fn infers_deletions(self) -> bool {
        match self {
            Self::Clear => true,
            Self::Interrupted
            | Self::Deferred
            | Self::Conflicting
            | Self::TreeEntries
            | Self::Unimported
            | Self::BaselineOpen => false,
        }
    }
}

impl Reconciliation {
    /// Counts an export held back because it would recreate a bucket directory that the
    /// inventory takes to approval. It is deferred, so a report and a wait show the object
    /// still owed to the tree, but it holds back neither the baseline nor the inventory: that
    /// inventory is what offers the set whose approval, like the directory's return, releases
    /// the export.
    pub(in crate::sync) fn hold_for_approval(&mut self) {
        self.deferred += 1;
        self.held_for_approval += 1;
    }

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

    /// Counts a regular file the pass passed over, which holds nothing back. A file directly in
    /// the root is named too, so that the mirror warns about it.
    pub(in crate::sync) fn count_skipped(&mut self, path: &Path, skip: Skip) {
        self.ignored += 1;
        match skip {
            Skip::IgnoredName => {}
            Skip::RootFile => self.entries.record_root_file(path),
        }
    }

    /// The working-tree entries among `unrepresentable`: what a wait reports as an
    /// unrepresentable source, unlike the unmapped objects.
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
            deferred: self.deferred.saturating_sub(self.held_for_approval),
            conflicting: self.conflicting,
            entries: self.unrepresentable_entries(),
            unimported: self.external_only,
        }
    }
}

/// Hands the entries an ingestion could not take in to the mirror's status, replacing those of
/// the previous one. An interrupted ingestion saw only part of the tree, so it keeps the last
/// complete list.
pub(super) fn report_entries(entries: &EntryReportSender, ingested: &mut Reconciliation) {
    if !ingested.interrupted {
        entries.report(std::mem::take(&mut ingested.entries));
    }
}

/// Whether a reconciliation may record the baseline as complete, as the latest ingestion left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BaselineGate {
    Allowed,
    Deferred,
}

impl BaselineGate {
    pub(super) fn after_ingest(ingested: PassGate) -> Self {
        if ingested.hold().completes_baseline() {
            Self::Allowed
        } else {
            Self::Deferred
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the tree side decides: any number of unmapped objects leaves a pass clear, and one
    /// entry the tree could not take in holds it back.
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
        assert_eq!(pass.gate().hold(), Hold::Clear);
        assert_eq!(
            BaselineGate::after_ingest(pass.gate()),
            BaselineGate::Allowed
        );

        pass.count_unrepresentable_entry(Path::new("bucket/link"), EntryProblem::UnsafeEntry);
        assert_eq!(pass.unrepresentable_entries().get(), 1);
        assert_eq!(pass.entries.total(), 1);
        assert_eq!(pass.gate().hold(), Hold::TreeEntries);
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
        assert_eq!(pass.gate().hold(), Hold::TreeEntries);
    }

    /// Each cause holds back both decisions on its own, and an incomplete baseline holds back
    /// only the deletions.
    #[test]
    fn each_cause_holds_back_the_baseline_and_deletions_and_an_open_baseline_only_deletions() {
        for (pass, hold) in [
            (
                Reconciliation {
                    interrupted: true,
                    deferred: 1,
                    ..Default::default()
                },
                Hold::Interrupted,
            ),
            (
                Reconciliation {
                    deferred: 1,
                    conflicting: 1,
                    ..Default::default()
                },
                Hold::Deferred,
            ),
            (
                Reconciliation {
                    conflicting: 1,
                    ..Default::default()
                },
                Hold::Conflicting,
            ),
            (
                Reconciliation {
                    unrepresentable: 1,
                    external_only: 1,
                    ..Default::default()
                },
                Hold::TreeEntries,
            ),
            (
                Reconciliation {
                    external_only: 1,
                    ..Default::default()
                },
                Hold::Unimported,
            ),
        ] {
            let mut pass = pass;
            for baseline_complete in [false, true] {
                pass.baseline_complete = baseline_complete;
                assert_eq!(pass.gate().hold(), hold);
                assert!(!hold.completes_baseline(), "{hold:?}");
                assert!(!hold.infers_deletions(), "{hold:?}");
            }
        }
        let open = Reconciliation::default().gate().hold();
        assert_eq!(open, Hold::BaselineOpen);
        assert!(open.completes_baseline());
        assert!(!open.infers_deletions());
        let clear = Reconciliation {
            baseline_complete: true,
            ..Default::default()
        };
        assert!(clear.gate().hold().completes_baseline());
        assert!(clear.gate().hold().infers_deletions());
    }
}
