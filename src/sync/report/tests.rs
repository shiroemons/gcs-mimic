//! What a running synchronization reports for each condition that can hold it back: the status
//! `state` and the reason a wait ends with, side by side. The HTTP status of a wait and the exit
//! code of `sync wait` are pinned beside the code that decides them, in the admin control tests
//! and the command's own tests, each keyed by the same reasons.

use super::{
    ending::{Unfinished, WaitResult},
    listing::WaitFindings,
    outcome::WaitReason,
    state::ControlState,
    verdict::{Outstanding, TreeSide, UnmappedObjects},
};
use crate::{
    storage::sync_state::{
        OpaqueId,
        deletions::{DeletionAuthorization, DeletionFence, DeletionSet},
        observation::{PauseReason, ScanStop, Summary, WritebackSummary},
        writeback::Stop,
    },
    sync::MirrorState,
};

fn clear() -> Summary {
    Summary {
        revision: 1,
        baseline_complete: true,
        tracked_objects: 0,
        mapping_findings: 0,
        blocking_findings: 0,
        unmapped_findings: 0,
        pending_writeback: 0,
        rejected_writeback: 0,
        path_stops: 0,
        unresolved_conflicts: 0,
        paused_reason: None,
        scan_stop: None,
        writeback: None,
    }
}

fn writeback_stopped(stop: Stop) -> Option<WritebackSummary> {
    Some(WritebackSummary {
        state: stop,
        reserved_bytes: 0,
        retained_bytes: 0,
        entries: 1,
        protected_entries: 1,
        max_bytes: 10,
        max_entries: 10,
        over_limit: false,
    })
}

fn withheld_set() -> DeletionSet {
    DeletionSet {
        revision: OpaqueId::generate().unwrap(),
        scan_revision: OpaqueId::generate().unwrap(),
        baseline_count: 10,
        candidate_count: 3,
        guard_enabled: true,
        guarded: true,
        completed_count: 0,
        journal_fence: DeletionFence {
            sequence: 1,
            epoch: 1,
        },
        authorization: DeletionAuthorization::Withheld,
        seen: Default::default(),
    }
}

/// One condition a running mirror can be in, with what the status and a wait say of it.
struct Row {
    name: &'static str,
    observation: Summary,
    tree: TreeSide,
    set: Option<DeletionSet>,
    state: ControlState,
    reason: WaitReason,
}

fn row(
    name: &'static str,
    observation: Summary,
    tree: TreeSide,
    state: ControlState,
    reason: WaitReason,
) -> Row {
    Row {
        name,
        observation,
        tree,
        set: None,
        state,
        reason,
    }
}

/// The rows of a running mirror, in the order the conditions are checked.
fn running_rows() -> Vec<Row> {
    let paused = |reason| Summary {
        paused_reason: Some(reason),
        ..clear()
    };
    let halted = |stop| Summary {
        writeback: writeback_stopped(stop),
        ..clear()
    };
    let scanned = |tree: TreeSide| Summary {
        scan_stop: tree.scan_stop(),
        baseline_complete: tree != TreeSide::Observing,
        unresolved_conflicts: u64::from(tree == TreeSide::Conflicting),
        ..clear()
    };
    let findings =
        |blocking_findings, path_stops, pending_writeback, unresolved_conflicts| Summary {
            blocking_findings,
            path_stops,
            pending_writeback,
            unresolved_conflicts,
            ..clear()
        };
    let mut rows = vec![
        row(
            "pause for an io failure",
            paused(PauseReason::IoFailure),
            TreeSide::Settled,
            ControlState::IoFailure,
            WaitReason::Paused,
        ),
        row(
            "pause for conflict capacity",
            paused(PauseReason::ConflictCapacity),
            TreeSide::Settled,
            ControlState::ConflictCapacity,
            WaitReason::Paused,
        ),
        row(
            "pause for recovery",
            paused(PauseReason::RecoveryRequired),
            TreeSide::Settled,
            ControlState::RecoveryRequired,
            WaitReason::Paused,
        ),
        row(
            "pause for an observer failure",
            paused(PauseReason::ObserverFailure),
            TreeSide::Settled,
            ControlState::Paused,
            WaitReason::Paused,
        ),
        row(
            "pause for a changing source",
            paused(PauseReason::SourceChanging),
            TreeSide::Settled,
            ControlState::Paused,
            WaitReason::Paused,
        ),
        row(
            "pause for an unrepresentable source",
            paused(PauseReason::SourceUnrepresentable),
            TreeSide::Settled,
            ControlState::Paused,
            WaitReason::Paused,
        ),
        row(
            "write-back stopped for recovery",
            halted(Stop::RecoveryRequired),
            TreeSide::Settled,
            ControlState::RecoveryRequired,
            WaitReason::FindingsPending,
        ),
        row(
            "write-back stopped on a protected body",
            halted(Stop::ProtectedBody),
            TreeSide::Settled,
            ControlState::ProtectedBody,
            WaitReason::FindingsPending,
        ),
        row(
            "write-back stopped for conflict capacity",
            halted(Stop::ConflictCapacity),
            TreeSide::Settled,
            ControlState::ConflictCapacity,
            WaitReason::FindingsPending,
        ),
        row(
            "write-back stopped for an io failure",
            halted(Stop::IoFailure),
            TreeSide::Settled,
            ControlState::IoFailure,
            WaitReason::FindingsPending,
        ),
        row(
            "files of a rejected set left to write back",
            Summary {
                rejected_writeback: 1,
                ..clear()
            },
            TreeSide::Settled,
            ControlState::SourceChanging,
            WaitReason::SourceChanging,
        ),
        row(
            "tree still changing",
            scanned(TreeSide::Changing),
            TreeSide::Changing,
            ControlState::SourceChanging,
            WaitReason::SourceChanging,
        ),
        row(
            "tree with unrepresentable entries",
            scanned(TreeSide::Unrepresentable),
            TreeSide::Unrepresentable,
            ControlState::SourceUnrepresentable,
            WaitReason::SourceUnrepresentable,
        ),
        row(
            "tree with unreadable entries",
            scanned(TreeSide::Unreadable),
            TreeSide::Unreadable,
            ControlState::SourceUnrepresentable,
            WaitReason::SourceUnrepresentable,
        ),
        row(
            "baseline still open",
            scanned(TreeSide::Observing),
            TreeSide::Observing,
            ControlState::Observing,
            WaitReason::SourceChanging,
        ),
        row(
            "tree with conflicts",
            scanned(TreeSide::Conflicting),
            TreeSide::Conflicting,
            ControlState::FindingsPending,
            WaitReason::FindingsPending,
        ),
        row(
            "blocking findings",
            findings(1, 0, 0, 0),
            TreeSide::Settled,
            ControlState::FindingsPending,
            WaitReason::FindingsPending,
        ),
        row(
            "path stops",
            findings(0, 1, 0, 0),
            TreeSide::Settled,
            ControlState::FindingsPending,
            WaitReason::FindingsPending,
        ),
        row(
            "write-back still pending",
            findings(0, 0, 1, 0),
            TreeSide::Settled,
            ControlState::FindingsPending,
            WaitReason::FindingsPending,
        ),
        row(
            "unresolved conflicts",
            findings(0, 0, 0, 1),
            TreeSide::Settled,
            ControlState::FindingsPending,
            WaitReason::FindingsPending,
        ),
        row(
            "nothing holds",
            clear(),
            TreeSide::Settled,
            ControlState::Ready,
            WaitReason::Ready,
        ),
    ];
    for (name, tree) in [
        (
            "deletions awaiting approval on a settled tree",
            TreeSide::Settled,
        ),
        (
            "deletions awaiting approval beside conflicts",
            TreeSide::Conflicting,
        ),
    ] {
        rows.push(Row {
            set: Some(withheld_set()),
            ..row(
                name,
                scanned(tree),
                tree,
                ControlState::DeletionApproval,
                WaitReason::DeletionApproval,
            )
        });
    }
    rows
}

/// What the status shows of a running mirror and how a wait ends, for every condition that can
/// hold it back. The status reads the tree side its last scan recorded and a wait the side its
/// own pass found; both read the same catalog summary.
#[test]
fn each_condition_of_a_running_mirror_names_a_state_and_a_wait_reason() {
    for Row {
        name,
        observation,
        tree,
        set,
        state,
        reason,
    } in running_rows()
    {
        assert_eq!(
            ControlState::of(MirrorState::Running, &observation, set.as_ref()),
            state,
            "{name}: state"
        );
        let held = Outstanding::new(&observation, set.as_ref(), tree).first();
        let result = WaitResult::held(held, None, None);
        assert_eq!(result.reason, reason, "{name}: reason");
        assert_eq!(result.completed, reason == WaitReason::Ready, "{name}");
        assert!(!result.timed_out, "{name}");
    }
}

/// While the mirror is not running, its own state decides what the status shows, whatever the
/// catalog recorded, and a pause names its recorded reason.
#[test]
fn a_mirror_that_is_not_running_decides_the_state_before_the_catalog_does() {
    let recorded = Summary {
        paused_reason: Some(PauseReason::IoFailure),
        scan_stop: Some(ScanStop::SourceChanging),
        ..clear()
    };
    for (mirror, state) in [
        (MirrorState::Starting, ControlState::Starting),
        (MirrorState::Retrying, ControlState::Retrying),
        (MirrorState::Stopped, ControlState::Stopped),
    ] {
        assert_eq!(ControlState::of(mirror, &clear(), None), state, "{mirror}");
        assert_eq!(ControlState::of(mirror, &recorded, None), state, "{mirror}");
    }
    assert_eq!(
        ControlState::of(MirrorState::Paused, &clear(), None),
        ControlState::Paused
    );
    assert_eq!(
        ControlState::of(MirrorState::Paused, &recorded, None),
        ControlState::IoFailure
    );
}

/// Objects whose names the tree cannot hold change only a wait that is otherwise ready, and
/// only when it refuses them; the status never counts them as holding anything.
#[test]
fn unmapped_objects_end_a_wait_only_when_it_refuses_them() {
    let unmapped = Summary {
        mapping_findings: 2,
        unmapped_findings: 2,
        ..clear()
    };
    assert_eq!(
        ControlState::of(MirrorState::Running, &unmapped, None),
        ControlState::Ready
    );
    let found = |result: WaitResult| {
        result.with_findings(Some(WaitFindings {
            unmapped: 2,
            observation_revision: 1,
            ..WaitFindings::default()
        }))
    };
    let held = Outstanding::new(&unmapped, None, TreeSide::Settled).first();
    for (policy, reason, completed) in [
        (UnmappedObjects::Report, WaitReason::Ready, true),
        (UnmappedObjects::Refuse, WaitReason::FindingsPending, false),
    ] {
        let result = found(WaitResult::held(held, None, None)).with_unmapped(policy);
        assert_eq!(
            (result.reason, result.completed),
            (reason, completed),
            "{policy:?}"
        );
    }
}

/// The endings a wait reaches without a pass deciding them, with the flags the answer carries:
/// a wait whose time ran out is the only one that says so.
#[test]
fn a_wait_cut_short_names_why_and_flags_only_a_timeout() {
    for (why, reason, timed_out) in [
        (Unfinished::Timeout, WaitReason::Timeout, true),
        (
            Unfinished::SourceChanging,
            WaitReason::SourceChanging,
            false,
        ),
        (
            Unfinished::JournalChanged,
            WaitReason::JournalChanged,
            false,
        ),
        (Unfinished::Stopped, WaitReason::Stopped, false),
    ] {
        let result = WaitResult::unfinished(why, None, None);
        assert_eq!(result.reason, reason, "{why:?}");
        assert_eq!(result.timed_out, timed_out, "{why:?}");
        assert!(!result.completed, "{why:?}");
    }
    let timeout = WaitResult::timeout();
    assert_eq!(
        (timeout.reason, timeout.timed_out),
        (WaitReason::Timeout, true)
    );
}
