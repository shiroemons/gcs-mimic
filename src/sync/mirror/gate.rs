//! The one answer to what holds back the baseline and the deletion inventory after a pass:
//! whether a reconciliation may complete the baseline, and whether missing files may become
//! deletion candidates or reset the deletion history. Only the working tree's side of a pass
//! takes part: an object the API holds and the tree cannot represent has no file, so no file of
//! it can look deleted, and counting it here stopped deletion detection for the whole root over
//! one unexportable name.

use super::{EntryReportSender, Reconciliation};
use crate::{
    storage::sync_state::root::Baseline,
    sync::{
        entries::{EntryProblem, Skip},
        outstanding::TreeSide,
    },
};
use std::{
    io,
    path::{Path, PathBuf},
};

/// What waiting does to a failure to read a working-tree entry, as far as its kind tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) enum ReadFailure {
    /// A condition that passes on its own: the entry went away or was replaced while the pass
    /// read it, or the read was interrupted.
    Passing,
    /// A condition only an operator changes, such as a missing permission or a full disk.
    Lasting,
    /// A kind this classification does not know. Nothing shows that waiting clears it.
    Unknown,
}

impl From<io::ErrorKind> for ReadFailure {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "io::ErrorKind is non-exhaustive, and a kind this match does not name is unknown"
    )]
    fn from(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::NotFound
            | io::ErrorKind::ResourceBusy
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::IsADirectory
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut => Self::Passing,
            io::ErrorKind::PermissionDenied
            | io::ErrorKind::StorageFull
            | io::ErrorKind::QuotaExceeded
            | io::ErrorKind::ReadOnlyFilesystem => Self::Lasting,
            _ => Self::Unknown,
        }
    }
}

/// The first files a pass left for a later one, which a wait that ends while they remain names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeferredFiles(Vec<PathBuf>);

impl DeferredFiles {
    /// Files kept by path; a wait names only a few, and the count says how many there were.
    const LIMIT: usize = 3;

    fn record(&mut self, path: &Path) {
        if self.0.len() < Self::LIMIT {
            self.0.push(path.to_owned());
        }
    }

    pub(crate) fn first(&self) -> &[PathBuf] {
        &self.0
    }
}

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
    unreadable: u64,
    unimported: u64,
    held_for_approval: u64,
}

/// What holds back the baseline and the deletion inventory after a pass, in the order
/// [`PassGate::hold`] looks for it: what waiting cannot change before what it can, so that a
/// wait never spends its time on a pass that an entry only an operator can fix holds back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) enum Hold {
    /// The pass stopped before it saw the whole tree.
    Interrupted,
    /// Tree entries no object can take, where a missing file may have gone.
    TreeEntries,
    /// Entries the pass could not read for a reason waiting does not clear.
    Unreadable,
    /// Files that differ from their objects and wait for a conflict decision.
    Conflicting,
    /// Files the pass left for a later one: still changing, or racing an API write.
    Deferred,
    /// Files no object accounts for yet, such as one written after the pass took the tree in;
    /// the next pass takes them in.
    Unimported,
    /// Nothing in the pass holds anything back, but the baseline is not complete, so a missing
    /// file is not yet a deletion. After a reconciliation that may complete it, only an API
    /// change during that reconciliation leaves it open.
    BaselineOpen,
    /// Exports wait for a deletion approval, because they would recreate a bucket directory the
    /// inventory takes to approval, and nothing else holds anything back. Like `Clear`, it holds
    /// back neither the baseline nor the inventory, which offers the set to approve; a wait
    /// reads it as that approval rather than as a change still settling.
    AwaitingApproval,
    /// Nothing holds anything back.
    Clear,
}

impl PassGate {
    /// What holds back the baseline and the deletion inventory after this pass. Every decision
    /// reads this answer instead of testing the counts again.
    pub(in crate::sync) fn hold(self) -> Hold {
        match (
            self.interrupted,
            self.entries.0,
            self.unreadable,
            self.conflicting,
            self.deferred,
            self.unimported,
            self.baseline,
        ) {
            (true, ..) => Hold::Interrupted,
            (false, 1.., ..) => Hold::TreeEntries,
            (false, 0, 1.., ..) => Hold::Unreadable,
            (false, 0, 0, 1.., ..) => Hold::Conflicting,
            (false, 0, 0, 0, 1.., ..) => Hold::Deferred,
            (false, 0, 0, 0, 0, 1.., _) => Hold::Unimported,
            (false, 0, 0, 0, 0, 0, Baseline::Activating) => Hold::BaselineOpen,
            (false, 0, 0, 0, 0, 0, Baseline::Complete) if self.held_for_approval > 0 => {
                Hold::AwaitingApproval
            }
            (false, 0, 0, 0, 0, 0, Baseline::Complete) => Hold::Clear,
        }
    }
}

impl Hold {
    /// Whether a reconciliation after this pass may record the baseline as complete.
    pub(in crate::sync) fn completes_baseline(self) -> bool {
        match self {
            Self::BaselineOpen | Self::AwaitingApproval | Self::Clear => true,
            Self::Interrupted
            | Self::TreeEntries
            | Self::Unreadable
            | Self::Conflicting
            | Self::Deferred
            | Self::Unimported => false,
        }
    }

    /// Whether the tracked files this pass found missing may become deletion candidates, and
    /// whether a deletion set it leaves without candidates may reset the deletion history.
    pub(in crate::sync) fn infers_deletions(self) -> bool {
        match self {
            Self::AwaitingApproval | Self::Clear => true,
            Self::Interrupted
            | Self::TreeEntries
            | Self::Unreadable
            | Self::Conflicting
            | Self::Deferred
            | Self::Unimported
            | Self::BaselineOpen => false,
        }
    }

    /// What the working tree leaves undone after this pass, which the status records at the end
    /// of a scan and a wait reads after its own pass; `None` for a pass that saw only part of
    /// the tree. Exports held for approval leave the tree owing nothing else: the guarded set
    /// the inventory offered, which the judgment reads as a deletion approval, is what they
    /// wait for.
    pub(in crate::sync) fn tree_side(self) -> Option<TreeSide> {
        match self {
            Self::Interrupted => None,
            Self::TreeEntries => Some(TreeSide::Unrepresentable),
            Self::Unreadable => Some(TreeSide::Unreadable),
            Self::Conflicting => Some(TreeSide::Conflicting),
            Self::Deferred | Self::Unimported => Some(TreeSide::Changing),
            Self::BaselineOpen => Some(TreeSide::Observing),
            Self::AwaitingApproval | Self::Clear => Some(TreeSide::Settled),
        }
    }
}

impl Reconciliation {
    /// Counts an export held back because it would recreate a bucket directory that the
    /// inventory takes to approval. It is deferred, so a rescan result shows the object still
    /// owed to the tree, but it holds back neither the baseline nor the inventory: that
    /// inventory is what offers the set whose approval, like the directory's return, releases
    /// the export, and a wait reports that approval ([`Hold::AwaitingApproval`]).
    pub(in crate::sync) fn hold_for_approval(&mut self) {
        self.deferred += 1;
        self.held_for_approval += 1;
    }

    /// Carries into this pass the exports that a retry of skipped exports held for approval.
    /// They stay queued for the retry after the approval, and carried here a wait reads them as
    /// waiting for that approval instead of as a change still settling. A retry reports no
    /// other counts, like the baseline reconciliation of an ingestion pass.
    pub(in crate::sync) fn carry_held(&mut self, retried: &Self) {
        self.deferred += retried.held_for_approval;
        self.held_for_approval += retried.held_for_approval;
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

    /// Counts a working-tree entry the pass could not read. One that waiting may clear is left
    /// for a later pass; any other is named like an entry no object can take, since only an
    /// operator changes it.
    pub(in crate::sync) fn count_read_failure(&mut self, path: &Path, kind: io::ErrorKind) {
        match ReadFailure::from(kind) {
            ReadFailure::Passing => self.count_deferred(path),
            ReadFailure::Lasting | ReadFailure::Unknown => {
                self.unreadable += 1;
                self.entries.record(path, EntryProblem::Unreadable);
            }
        }
    }

    /// Counts a file the pass left for a later one and keeps the first few names.
    pub(in crate::sync) fn count_deferred(&mut self, path: &Path) {
        self.deferred += 1;
        self.deferred_files.record(path);
    }

    /// Adds the files another part of this pass left for a later one, with their names and the
    /// exports among them that wait for a deletion approval.
    pub(in crate::sync) fn absorb_deferred(&mut self, other: &Self) {
        self.deferred += other.deferred;
        self.held_for_approval += other.held_for_approval;
        for path in other.deferred_files.first() {
            self.deferred_files.record(path);
        }
    }

    /// Adds what holds back a reconciliation that ran inside this pass, so that the pass's own
    /// [`Hold`] names it instead of leaving the baseline open without a reason.
    pub(in crate::sync) fn absorb_hold(&mut self, reconciled: &Self) {
        self.absorb_deferred(reconciled);
        self.conflicting += reconciled.conflicting;
        self.external_only += reconciled.external_only;
        self.unreadable += reconciled.unreadable;
        self.unrepresentable += reconciled.unrepresentable_entries().get();
        self.entries.extend(&reconciled.entries);
        self.interrupted |= reconciled.interrupted;
    }

    /// The working-tree entries among `unrepresentable`: what a wait reports as an
    /// unrepresentable source, unlike the unmapped objects.
    pub(crate) fn unrepresentable_entries(&self) -> UnrepresentableEntries {
        UnrepresentableEntries(self.unrepresentable.saturating_sub(self.unmapped_objects))
    }

    /// The files among `deferred` that a later pass takes in once they settle, leaving out the
    /// exports that wait for a deletion approval.
    pub(in crate::sync) fn still_changing(&self) -> u64 {
        self.deferred.saturating_sub(self.held_for_approval)
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
            deferred: self.still_changing(),
            conflicting: self.conflicting,
            entries: self.unrepresentable_entries(),
            unreadable: self.unreadable,
            unimported: self.external_only,
            held_for_approval: self.held_for_approval,
        }
    }
}

/// Hands the entries an ingestion could not take in to the mirror's status, replacing those of
/// the previous one. An interrupted ingestion saw only part of the tree, so it keeps the last
/// complete list. The pass keeps its own copy, which a wait names.
pub(super) fn report_entries(entries: &EntryReportSender, ingested: &Reconciliation) {
    if !ingested.interrupted {
        entries.report(ingested.entries.clone());
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
                Hold::Conflicting,
            ),
            (
                Reconciliation {
                    deferred: 1,
                    ..Default::default()
                },
                Hold::Deferred,
            ),
            (
                Reconciliation {
                    unreadable: 1,
                    deferred: 1,
                    ..Default::default()
                },
                Hold::Unreadable,
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

    /// Exports held for approval, whether this pass held them or a retry of its skipped exports
    /// did, read as awaiting that approval, which holds back neither the baseline nor the
    /// inventory that offers the set. Any other deferral still comes first.
    #[test]
    fn exports_held_for_approval_await_it_and_hold_back_nothing_else() {
        let mut pass = Reconciliation {
            baseline_complete: true,
            ..Default::default()
        };
        pass.hold_for_approval();
        let mut retried = Reconciliation::default();
        retried.hold_for_approval();
        let mut carried = Reconciliation {
            baseline_complete: true,
            ..Default::default()
        };
        carried.carry_held(&retried);
        for pass in [&pass, &carried] {
            assert_eq!(pass.deferred, 1);
            let hold = pass.gate().hold();
            assert_eq!(hold, Hold::AwaitingApproval);
            assert!(hold.completes_baseline());
            assert!(hold.infers_deletions());
            assert_eq!(hold.tree_side(), Some(TreeSide::Settled));
        }

        pass.deferred += 1;
        assert_eq!(pass.gate().hold(), Hold::Deferred);
        let mut unsettled = carried;
        unsettled.external_only = 1;
        assert_eq!(unsettled.gate().hold(), Hold::Unimported);
    }

    /// What waiting cannot change comes before what it can: with every cause present, the hold
    /// names the one only an operator clears, and removing each in turn names the next.
    #[test]
    fn a_hold_names_what_waiting_cannot_change_first() {
        let mut pass = Reconciliation {
            unrepresentable: 1,
            unreadable: 1,
            conflicting: 1,
            deferred: 1,
            external_only: 1,
            ..Default::default()
        };
        let mut order = vec![pass.gate().hold()];
        for clear in [
            |pass: &mut Reconciliation| pass.unrepresentable = 0,
            |pass: &mut Reconciliation| pass.unreadable = 0,
            |pass: &mut Reconciliation| pass.conflicting = 0,
            |pass: &mut Reconciliation| pass.deferred = 0,
            |pass: &mut Reconciliation| pass.external_only = 0,
            |pass: &mut Reconciliation| pass.baseline_complete = true,
        ] {
            clear(&mut pass);
            order.push(pass.gate().hold());
        }
        assert_eq!(
            order,
            [
                Hold::TreeEntries,
                Hold::Unreadable,
                Hold::Conflicting,
                Hold::Deferred,
                Hold::Unimported,
                Hold::BaselineOpen,
                Hold::Clear,
            ]
        );
        let sides: Vec<Option<TreeSide>> = order.into_iter().map(Hold::tree_side).collect();
        assert_eq!(
            sides,
            [
                Some(TreeSide::Unrepresentable),
                Some(TreeSide::Unreadable),
                Some(TreeSide::Conflicting),
                Some(TreeSide::Changing),
                Some(TreeSide::Changing),
                Some(TreeSide::Observing),
                Some(TreeSide::Settled),
            ]
        );
        assert_eq!(Hold::Interrupted.tree_side(), None);
    }

    /// A read that a later pass may repeat is deferred and named among the deferred files; one
    /// only an operator repairs, or of a kind nothing shows waiting clears, is unreadable and
    /// named among the entries, so a wait stops on it instead of spending its timeout.
    #[test]
    fn a_read_failure_waiting_does_not_clear_holds_as_unreadable() {
        let mut pass = Reconciliation {
            baseline_complete: true,
            ..Default::default()
        };
        pass.count_read_failure(Path::new("bucket/vanished"), io::ErrorKind::NotFound);
        assert_eq!((pass.deferred, pass.unreadable), (1, 0));
        assert_eq!(
            pass.deferred_files.first(),
            [PathBuf::from("bucket/vanished")]
        );
        assert_eq!(pass.gate().hold(), Hold::Deferred);
        for (kind, unreadable) in [
            (io::ErrorKind::PermissionDenied, 1),
            (io::ErrorKind::StorageFull, 2),
            (io::ErrorKind::InvalidData, 3),
        ] {
            pass.count_read_failure(Path::new("bucket/locked"), kind);
            assert_eq!((pass.deferred, pass.unreadable), (1, unreadable), "{kind}");
        }
        assert_eq!(pass.gate().hold(), Hold::Unreadable);
        assert_eq!(pass.entries.total(), 3);
        assert_eq!(pass.unrepresentable_entries().get(), 0);
        for name in ["bucket/b", "bucket/c", "bucket/d"] {
            pass.count_deferred(Path::new(name));
        }
        assert_eq!(pass.deferred, 4);
        assert_eq!(
            pass.deferred_files.first(),
            ["bucket/vanished", "bucket/b", "bucket/c"].map(PathBuf::from)
        );
    }

    /// A reconciliation inside a pass passes on what held it back, so the pass names it rather
    /// than an open baseline without a reason.
    #[test]
    fn a_pass_takes_on_what_held_back_its_reconciliation() {
        let mut pass = Reconciliation::default();
        let mut reconciled = Reconciliation::default();
        reconciled.count_unmapped_object();
        reconciled.count_read_failure(Path::new("bucket/locked"), io::ErrorKind::PermissionDenied);
        pass.absorb_hold(&reconciled);
        assert_eq!(pass.gate().hold(), Hold::Unreadable);
        assert_eq!(pass.unrepresentable_entries().get(), 0);
        assert_eq!(pass.entries.total(), 1);
        let written = Reconciliation {
            external_only: 1,
            ..Default::default()
        };
        let mut pass = Reconciliation::default();
        pass.absorb_hold(&written);
        assert_eq!(pass.gate().hold(), Hold::Unimported);
    }
}
