use super::*;
use crate::sync::WaitReason;

fn summary() -> Summary {
    Summary {
        revision: 1,
        baseline_complete: true,
        tracked_objects: 0,
        mapping_findings: 0,
        blocking_findings: 0,
        unmapped_findings: 0,
        pending_writeback: 0,
        path_stops: 0,
        unresolved_conflicts: 0,
        paused_reason: None,
        scan_stop: None,
        writeback: None,
    }
}

fn settled() -> Outstanding {
    Outstanding::new(&summary(), None, TreeSide::Settled)
}

/// A change that makes one condition hold, and the holdup it names.
type Condition = (fn(&mut Outstanding), Holdup);

/// Every condition, in the order [`Outstanding::first`] checks them, with what it names.
fn conditions() -> [Condition; 9] {
    [
        (
            |outstanding| outstanding.paused = Some(PauseReason::IoFailure),
            Holdup::Paused(PauseReason::IoFailure),
        ),
        (
            |outstanding| outstanding.writeback = Some(Halt::ProtectedBody),
            Holdup::Writeback(Halt::ProtectedBody),
        ),
        (
            |outstanding| outstanding.tree = TreeSide::Changing,
            Holdup::SourceChanging,
        ),
        (
            |outstanding| outstanding.tree = TreeSide::Unrepresentable,
            Holdup::SourceUnrepresentable,
        ),
        (
            |outstanding| outstanding.tree = TreeSide::Unreadable,
            Holdup::SourceUnreadable,
        ),
        (
            |outstanding| outstanding.tree = TreeSide::Observing,
            Holdup::Observing,
        ),
        (
            |outstanding| outstanding.deletion_approval = true,
            Holdup::DeletionApproval,
        ),
        (
            |outstanding| outstanding.tree = TreeSide::Conflicting,
            Holdup::FindingsPending,
        ),
        (
            |outstanding| outstanding.findings_pending = true,
            Holdup::FindingsPending,
        ),
    ]
}

/// Each condition alone names itself, and it still comes first when every later condition holds
/// too; a later tree side replaces an earlier one, so only the later conditions apply after it.
#[test]
fn the_first_condition_that_holds_names_the_holdup() {
    assert_eq!(settled().first(), Holdup::Ready);
    let conditions = conditions();
    for (index, (set, expected)) in conditions.iter().enumerate() {
        let mut alone = settled();
        set(&mut alone);
        assert_eq!(alone.first(), *expected, "{expected:?} alone");
        for (later, _) in &conditions[index + 1..] {
            let mut with_later = alone;
            later(&mut with_later);
            if with_later.tree == alone.tree {
                assert_eq!(
                    with_later.first(),
                    *expected,
                    "{expected:?} with a later one"
                );
            }
        }
    }
}

/// A wait stops for every condition the status shows; one that is work in progress makes the
/// wait ask again, as for a tree still changing.
#[test]
fn a_wait_stops_for_every_holdup_the_status_shows() {
    for (_, holdup) in conditions() {
        let reason = WaitReason::from(holdup);
        assert_ne!(reason, WaitReason::Ready, "{holdup:?}");
        assert_eq!(
            reason == WaitReason::SourceChanging,
            matches!(holdup, Holdup::SourceChanging | Holdup::Observing),
            "{holdup:?}"
        );
    }
    assert_eq!(WaitReason::from(Holdup::Ready), WaitReason::Ready);
}

/// Only a guarded set that no operator has granted waits for approval, for a wait as for the
/// status, however many of its candidates remain: a granted or unguarded set runs in the pass
/// that completes it.
#[test]
fn only_a_withheld_guarded_deletion_set_waits_for_approval() {
    use crate::storage::sync_state::{OpaqueId, deletions::DeletionFence};
    let set = |guarded, authorization| DeletionSet {
        revision: OpaqueId::generate().unwrap(),
        scan_revision: OpaqueId::generate().unwrap(),
        baseline_count: 10,
        candidate_count: 3,
        guard_enabled: true,
        guarded,
        completed_count: 1,
        journal_fence: DeletionFence {
            sequence: 1,
            epoch: 1,
        },
        authorization,
    };
    for (guarded, authorization, expected) in [
        (
            true,
            DeletionAuthorization::Withheld,
            Holdup::DeletionApproval,
        ),
        (true, DeletionAuthorization::Granted, Holdup::Ready),
        (false, DeletionAuthorization::Granted, Holdup::Ready),
    ] {
        let set = set(guarded, authorization);
        assert_eq!(
            Outstanding::of_status(&summary(), Some(&set)).first(),
            expected,
            "{set:?}"
        );
        assert_eq!(
            WaitReason::from(Outstanding::new(&summary(), Some(&set), TreeSide::Settled).first()),
            WaitReason::from(expected),
            "{set:?}"
        );
    }
}

/// What a scan records for the tree side of its pass reads back, for the status, as a side that
/// names the reason a wait from the same pass gives, so the two put the same condition first. A
/// conflict a pass counts is one the catalog retains, which the status counts.
#[test]
fn the_status_names_what_a_wait_from_the_same_pass_names() {
    for tree in [
        TreeSide::Settled,
        TreeSide::Changing,
        TreeSide::Conflicting,
        TreeSide::Unrepresentable,
        TreeSide::Unreadable,
        TreeSide::Observing,
    ] {
        let observation = Summary {
            scan_stop: tree.scan_stop(),
            baseline_complete: tree != TreeSide::Observing,
            unresolved_conflicts: u64::from(tree == TreeSide::Conflicting),
            ..summary()
        };
        let status = Outstanding::of_status(&observation, None).first();
        let wait = Outstanding::new(&observation, None, tree).first();
        assert_eq!(WaitReason::from(status), WaitReason::from(wait), "{tree:?}");
    }
}

/// The status takes the tree side from the last scan's stop and baseline, a wait from its own
/// pass; both read everything else from the same catalog summary, and objects whose names the
/// tree cannot hold stop neither.
#[test]
fn a_wait_and_the_status_read_the_catalog_alike() {
    for (observation, tree) in [
        (summary(), TreeSide::Settled),
        (
            Summary {
                scan_stop: Some(ScanStop::SourceChanging),
                ..summary()
            },
            TreeSide::Changing,
        ),
        (
            Summary {
                scan_stop: Some(ScanStop::SourceUnrepresentable),
                ..summary()
            },
            TreeSide::Unrepresentable,
        ),
        (
            Summary {
                baseline_complete: false,
                ..summary()
            },
            TreeSide::Observing,
        ),
    ] {
        assert_eq!(TreeSide::recorded(&observation), tree, "{observation:?}");
        assert_eq!(
            Outstanding::of_status(&observation, None),
            Outstanding::new(&observation, None, tree)
        );
    }
    // A wait whose own pass settled the tree is not held back by a stop an earlier scan left.
    let stopped = Summary {
        scan_stop: Some(ScanStop::SourceUnrepresentable),
        ..summary()
    };
    assert_eq!(
        Outstanding::new(&stopped, None, TreeSide::Settled).first(),
        Holdup::Ready
    );
    for (observation, expected) in [
        (
            Summary {
                unmapped_findings: 3,
                mapping_findings: 3,
                ..summary()
            },
            Holdup::Ready,
        ),
        (
            Summary {
                blocking_findings: 1,
                mapping_findings: 1,
                ..summary()
            },
            Holdup::FindingsPending,
        ),
        (
            Summary {
                path_stops: 1,
                ..summary()
            },
            Holdup::FindingsPending,
        ),
        (
            Summary {
                unresolved_conflicts: 1,
                ..summary()
            },
            Holdup::FindingsPending,
        ),
        (
            Summary {
                paused_reason: Some(PauseReason::ConflictCapacity),
                ..summary()
            },
            Holdup::Paused(PauseReason::ConflictCapacity),
        ),
    ] {
        assert_eq!(
            Outstanding::of_status(&observation, None).first(),
            expected,
            "{observation:?}"
        );
        assert_eq!(
            Outstanding::new(&observation, None, TreeSide::Settled).first(),
            expected,
            "{observation:?}"
        );
    }
}
