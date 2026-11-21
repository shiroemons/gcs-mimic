//! The one answer to what holds back the baseline and the deletion inventory after a pass:
//! whether a reconciliation may complete the baseline, and whether missing files may become
//! deletion candidates or reset the deletion history. Only the working tree's side of a pass
//! takes part: an object the API holds and the tree cannot represent has no file, so no file of
//! it can look deleted, and counting it here stopped deletion detection for the whole root over
//! one unexportable name.

use super::Reconciliation;
use crate::{
    fsutil,
    storage::sync_state::root::Baseline,
    sync::{
        entries::{EntryProblem, EntryReport, Skip},
        failure::Failure,
        report::verdict::TreeSide,
        tree::EntryError,
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

impl ReadFailure {
    /// A transient failure passes and a volume's lasts, as [`Failure`] classifies them for every
    /// step; the other classes say nothing about a read.
    fn of(class: Failure) -> Option<Self> {
        match class {
            Failure::Transient => Some(Self::Passing),
            Failure::Volume => Some(Self::Lasting),
            Failure::Superseded | Failure::PathLocal => None,
        }
    }
}

impl From<EntryError> for ReadFailure {
    /// The class the one classification gives the failure decides first; the kinds only a read
    /// gives a meaning to are decided after it.
    fn from(error: EntryError) -> Self {
        match error {
            EntryError::Unopenable => {
                Self::of(Failure::of_unopenable(fsutil::Unopenable)).unwrap_or(Self::Unknown)
            }
            EntryError::Io(cause) => cause
                .class()
                .and_then(Self::of)
                .unwrap_or_else(|| Self::of_read_kind(cause.kind())),
        }
    }
}

impl ReadFailure {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "io::ErrorKind is non-exhaustive, and a kind this match does not name is unknown"
    )]
    fn of_read_kind(kind: io::ErrorKind) -> Self {
        match kind {
            // The entry went away or was replaced while the pass read it.
            io::ErrorKind::NotFound
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::IsADirectory => Self::Passing,
            io::ErrorKind::PermissionDenied => Self::Lasting,
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
    #[cfg(test)]
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

/// What a pass could not map, on either side, and the entries it could not read. Only
/// [`Reconciliation::count_unmapped_object`], [`Reconciliation::count_unrepresentable_entry`] and
/// [`Reconciliation::count_read_failure`] add to it, and each names its side. The working tree's
/// side is the entry report's own counts, so the counts that hold back the baseline and
/// deletions cannot drift from the list that names the entries, and the total is only ever
/// computed: a count added to the total alone, which read as a tree entry, cannot be written.
#[derive(Debug, Default)]
pub(crate) struct Unrepresentable {
    /// Objects and buckets the API holds that the working tree cannot, which are reported only.
    unmapped_objects: u64,
    /// The working-tree entries the pass could not take in or could not read.
    entries: EntryReport,
}

impl Unrepresentable {
    fn total(&self) -> u64 {
        self.unmapped_objects
            .saturating_add(self.entries.unrepresentable())
    }

    /// Adds the working-tree entries of another part of the same pass. Its unmapped objects stay
    /// its own: they hold nothing back, and the part reports them itself.
    fn absorb_entries(&mut self, other: &Self) {
        self.entries.extend(&other.entries);
    }
}

/// The JSON keeps the total as `unrepresentable`, which earlier clients read, beside each side:
/// the API's as `unmapped_objects` and the tree's as `unrepresentable_entries`, the count that
/// holds back the baseline and deletions. The entries that could not be read are `unreadable`,
/// apart from all three.
impl serde::Serialize for Unrepresentable {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(serde::Serialize)]
        struct Counts {
            unrepresentable: u64,
            unreadable: u64,
            unmapped_objects: u64,
            unrepresentable_entries: u64,
        }
        Counts {
            unrepresentable: self.total(),
            unreadable: self.entries.unreadable(),
            unmapped_objects: self.unmapped_objects,
            unrepresentable_entries: self.entries.unrepresentable(),
        }
        .serialize(serializer)
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
        self.unrepresentable.unmapped_objects += 1;
    }

    /// Counts a working-tree entry the pass could not take in and names it, in the one report
    /// whose count holds back the baseline and deletions.
    pub(in crate::sync) fn count_unrepresentable_entry(
        &mut self,
        path: &Path,
        problem: EntryProblem,
    ) {
        self.unrepresentable.entries.record(path, problem);
    }

    /// Counts a regular file the pass passed over, which holds nothing back. A file directly in
    /// the root is named too, so that the mirror warns about it.
    pub(in crate::sync) fn count_skipped(&mut self, path: &Path, skip: Skip) {
        self.ignored += 1;
        match skip {
            Skip::IgnoredName => {}
            Skip::RootFile => self.unrepresentable.entries.record_root_file(path),
        }
    }

    /// Counts a working-tree entry the pass could not read. One that waiting may clear is left
    /// for a later pass; any other is named like an entry no object can take, since only an
    /// operator changes it, and the report counts it apart from those.
    pub(in crate::sync) fn count_read_failure(&mut self, path: &Path, error: EntryError) {
        match ReadFailure::from(error) {
            ReadFailure::Passing => self.count_deferred(path),
            ReadFailure::Lasting | ReadFailure::Unknown => {
                self.unrepresentable
                    .entries
                    .record(path, EntryProblem::Unreadable);
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

    /// Adds the exports that another part of this pass ran, such as a journal replay: what
    /// reached the tree, the conflicts they preserved and what they left for a later pass.
    pub(in crate::sync) fn absorb_exports(&mut self, other: &Self) {
        self.exported += other.exported;
        self.identical += other.identical;
        self.conflicting += other.conflicting;
        self.absorb_deferred(other);
    }

    /// Adds what holds back a reconciliation that ran inside this pass, so that the pass's own
    /// [`Hold`] names it instead of leaving the baseline open without a reason.
    pub(in crate::sync) fn absorb_hold(&mut self, reconciled: &Self) {
        self.absorb_deferred(reconciled);
        self.conflicting += reconciled.conflicting;
        self.external_only += reconciled.external_only;
        self.unrepresentable
            .absorb_entries(&reconciled.unrepresentable);
        self.interrupted |= reconciled.interrupted;
    }

    /// Every name this pass could not map, on either side: the objects and buckets the API holds
    /// that the working tree cannot, and the working-tree entries the pass could not take in.
    pub fn unrepresentable(&self) -> u64 {
        self.unrepresentable.total()
    }

    /// The objects and buckets among [`Self::unrepresentable`] that the API holds and the tree
    /// cannot. Only the mirror's tests, which run where synchronization does, read it.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn unmapped_objects(&self) -> u64 {
        self.unrepresentable.unmapped_objects
    }

    /// The entries this pass could not read for a reason waiting does not clear.
    #[cfg(test)]
    pub(crate) fn unreadable(&self) -> u64 {
        self.unrepresentable.entries.unreadable()
    }

    /// The working-tree entries among [`Self::unrepresentable`]: what a wait reports as an
    /// unrepresentable source, unlike the unmapped objects.
    pub(crate) fn unrepresentable_entries(&self) -> UnrepresentableEntries {
        UnrepresentableEntries(self.unrepresentable.entries.unrepresentable())
    }

    /// The working-tree entries this pass could not take in, by path up to a bound.
    pub(crate) fn entries(&self) -> &EntryReport {
        &self.unrepresentable.entries
    }

    /// The files among `deferred` that a later pass takes in once they settle, leaving out the
    /// exports that wait for a deletion approval.
    pub(in crate::sync) fn still_changing(&self) -> u64 {
        self.deferred.saturating_sub(self.held_for_approval)
    }

    /// The tree side of this pass, for the decisions that must not see unmapped objects.
    pub(in crate::sync) fn gate(&self) -> PassGate {
        PassGate {
            baseline: Baseline::from_complete(self.baseline_complete),
            interrupted: self.interrupted,
            deferred: self.still_changing(),
            conflicting: self.conflicting,
            entries: self.unrepresentable_entries(),
            unreadable: self.unrepresentable.entries.unreadable(),
            unimported: self.external_only,
            held_for_approval: self.held_for_approval,
        }
    }
}

/// Whether a reconciliation may record the baseline as complete, as the latest ingestion left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) enum BaselineGate {
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

    /// A read that failed for an entry going away, a busy or interrupted call or a timeout passes;
    /// a missing permission or a whole file system's failure lasts; any other kind is unknown.
    #[test]
    fn a_read_failure_passes_lasts_or_is_unknown_by_its_kind() {
        for kind in [
            io::ErrorKind::NotFound,
            io::ErrorKind::ResourceBusy,
            io::ErrorKind::NotADirectory,
            io::ErrorKind::IsADirectory,
            io::ErrorKind::Interrupted,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::TimedOut,
        ] {
            assert_eq!(
                ReadFailure::from(EntryError::Io(kind.into())),
                ReadFailure::Passing,
                "{kind:?}"
            );
        }
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::StorageFull,
            io::ErrorKind::QuotaExceeded,
            io::ErrorKind::ReadOnlyFilesystem,
        ] {
            assert_eq!(
                ReadFailure::from(EntryError::Io(kind.into())),
                ReadFailure::Lasting,
                "{kind:?}"
            );
        }
        for kind in [
            io::ErrorKind::InvalidInput,
            io::ErrorKind::Other,
            io::ErrorKind::AlreadyExists,
        ] {
            assert_eq!(
                ReadFailure::from(EntryError::Io(kind.into())),
                ReadFailure::Unknown,
                "{kind:?}"
            );
        }
    }

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
        assert_eq!(pass.unrepresentable(), 1000);
        assert_eq!(pass.unrepresentable_entries().get(), 0);
        assert_eq!(pass.gate().hold(), Hold::Clear);
        assert_eq!(
            BaselineGate::after_ingest(pass.gate()),
            BaselineGate::Allowed
        );

        pass.count_unrepresentable_entry(Path::new("bucket/link"), EntryProblem::UnsafeEntry);
        assert_eq!(pass.unrepresentable_entries().get(), 1);
        assert_eq!(pass.entries().total(), 1);
        assert_eq!(pass.gate().hold(), Hold::TreeEntries);
        assert_eq!(
            BaselineGate::after_ingest(pass.gate()),
            BaselineGate::Deferred
        );
    }

    /// The status gets a copy of a pass's entries, and the pass keeps its own count of them, so
    /// it still holds back the baseline and deletions after the report. Taking the entries out
    /// for the status would have emptied the count the gate reads.
    #[test]
    fn a_reported_pass_still_holds_back_deletions() {
        let mut pass = Reconciliation {
            baseline_complete: true,
            ..Default::default()
        };
        pass.count_unrepresentable_entry(Path::new("bucket/link"), EntryProblem::UnsafeEntry);
        let status = crate::sync::status::MirrorStatusSender::default();
        status.entries().report_pass(&pass);
        assert_eq!(status.subscribe().unrepresentable_entries().total(), 1);
        assert_eq!(pass.gate().hold(), Hold::TreeEntries);
        assert_eq!(
            BaselineGate::after_ingest(pass.gate()),
            BaselineGate::Deferred
        );
    }

    /// The JSON keeps `unrepresentable`, which earlier clients read, as the sum of both sides,
    /// and names each side. A deleted bucket's directory is one of the tree's entries, and
    /// `blocked_buckets` counts it again only as a breakdown of them. The entries that could not
    /// be read count apart from all three.
    #[test]
    fn the_json_total_is_the_sum_of_both_sides() {
        let mut pass = with_unreadable(Reconciliation::default());
        pass.count_unmapped_object();
        pass.count_unrepresentable_entry(Path::new("bucket/link"), EntryProblem::UnsafeEntry);
        pass.count_unrepresentable_entry(Path::new("deleted"), EntryProblem::DeletedBucket);
        pass.blocked_buckets += 1;
        let json = serde_json::to_value(&pass).unwrap();
        assert_eq!(json["unrepresentable"], 3);
        assert_eq!(json["unreadable"], 1);
        assert_eq!(json["unmapped_objects"], 1);
        assert_eq!(json["unrepresentable_entries"], 2);
        assert_eq!(pass.unrepresentable_entries().get(), 2);
        assert_eq!(json["blocked_buckets"], 1);
    }

    /// A pass with one working-tree entry, which holds back both decisions.
    fn with_entry(mut pass: Reconciliation) -> Reconciliation {
        pass.count_unrepresentable_entry(Path::new("bucket/link"), EntryProblem::UnsafeEntry);
        pass
    }

    /// A pass with one entry it could not read for a reason waiting does not clear.
    fn with_unreadable(mut pass: Reconciliation) -> Reconciliation {
        pass.count_read_failure(
            Path::new("bucket/locked"),
            EntryError::Io(io::ErrorKind::PermissionDenied.into()),
        );
        pass
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
                with_unreadable(Reconciliation {
                    deferred: 1,
                    ..Default::default()
                }),
                Hold::Unreadable,
            ),
            (
                with_entry(Reconciliation {
                    external_only: 1,
                    ..Default::default()
                }),
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

    /// What waiting cannot change comes before what it can: adding the causes in turn, from the
    /// one waiting clears soonest to the one only an operator clears, names each before every
    /// cause already present.
    #[test]
    fn a_hold_names_what_waiting_cannot_change_first() {
        let mut pass = Reconciliation {
            baseline_complete: true,
            ..Default::default()
        };
        let mut order = vec![pass.gate().hold()];
        for add in [
            |pass: &mut Reconciliation| pass.baseline_complete = false,
            |pass: &mut Reconciliation| pass.external_only = 1,
            |pass: &mut Reconciliation| pass.deferred = 1,
            |pass: &mut Reconciliation| pass.conflicting = 1,
            |pass: &mut Reconciliation| {
                pass.count_read_failure(
                    Path::new("bucket/locked"),
                    EntryError::Io(io::ErrorKind::PermissionDenied.into()),
                );
            },
            |pass: &mut Reconciliation| {
                pass.count_unrepresentable_entry(
                    Path::new("bucket/link"),
                    EntryProblem::UnsafeEntry,
                );
            },
        ] {
            add(&mut pass);
            order.push(pass.gate().hold());
        }
        order.reverse();
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

    /// Descriptor or memory pressure, which no I/O kind names, passes for a scan's read as it
    /// does for every other step: the entry is deferred to a later pass, not counted unreadable.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_scan_read_that_meets_descriptor_or_memory_pressure_is_deferred() {
        use crate::{
            fsutil::{self, Lookup},
            sync::tree::{TreeLimits, TreeScan},
            test_support::TestDir,
        };
        for errno in [libc::EMFILE, libc::ENFILE, libc::ENOMEM] {
            let tree = TestDir::new();
            std::fs::create_dir(tree.0.join("bucket")).unwrap();
            std::fs::write(tree.0.join("bucket/pressed"), b"body").unwrap();
            let _refused = fsutil::FailingOpens::refusing(&tree.0.join("bucket"), "pressed", errno);
            let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
            let mut scan = TreeScan::new(TreeLimits::default(), Lookup::unprobed());
            let mut pass = Reconciliation {
                baseline_complete: true,
                ..Default::default()
            };
            while !scan.is_empty() {
                let (next, page) = scan.page(&root).unwrap();
                scan = next;
                for entry in page {
                    if let Some(error) = entry.error {
                        pass.count_read_failure(&entry.path, error);
                    }
                }
            }
            assert_eq!((pass.deferred, pass.unreadable()), (1, 0), "{errno}");
            assert_eq!(
                pass.deferred_files.first(),
                [PathBuf::from("bucket/pressed")]
            );
        }
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
        pass.count_read_failure(
            Path::new("bucket/vanished"),
            EntryError::Io(io::ErrorKind::NotFound.into()),
        );
        assert_eq!((pass.deferred, pass.unreadable()), (1, 0));
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
            pass.count_read_failure(Path::new("bucket/locked"), EntryError::Io(kind.into()));
            assert_eq!(
                (pass.deferred, pass.unreadable()),
                (1, unreadable),
                "{kind}"
            );
        }
        assert_eq!(pass.gate().hold(), Hold::Unreadable);
        assert_eq!(pass.entries().total(), 3);
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
        reconciled.count_read_failure(
            Path::new("bucket/locked"),
            EntryError::Io(io::ErrorKind::PermissionDenied.into()),
        );
        pass.absorb_hold(&reconciled);
        assert_eq!(pass.gate().hold(), Hold::Unreadable);
        assert_eq!(pass.unrepresentable_entries().get(), 0);
        assert_eq!(pass.entries().total(), 1);
        let written = Reconciliation {
            external_only: 1,
            ..Default::default()
        };
        let mut pass = Reconciliation::default();
        pass.absorb_hold(&written);
        assert_eq!(pass.gate().hold(), Hold::Unimported);
    }
}
