use super::*;
use crate::storage::sync_state::{observation::ScanStop, writeback::Stop};

/// The mirror's own state decides `state` until it runs: a mirror that has not finished its
/// first pass reports `Starting` whatever reason an earlier process recorded, and only a paused
/// one names its recorded reason. A running one reports the first condition that holds.
#[test]
fn status_state_follows_the_mirror_state() {
    use MirrorState::{Paused, Retrying, Running, Starting, Stopped};
    let observation = |paused_reason| Summary {
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
        paused_reason,
        scan_stop: None,
        writeback: None,
    };
    let state = |mirror, observation: &Summary| ControlState::of(mirror, observation, None);
    let clear = observation(None);
    let earlier = observation(Some(PauseReason::IoFailure));
    for (mirror, expected) in [
        (Starting, ControlState::Starting),
        (Retrying, ControlState::Retrying),
        (Stopped, ControlState::Stopped),
    ] {
        assert_eq!(state(mirror, &clear), expected, "{mirror}");
        assert_eq!(state(mirror, &earlier), expected, "{mirror}");
    }
    assert_eq!(state(Running, &clear), ControlState::Ready);
    assert_eq!(state(Paused, &clear), ControlState::Paused);
    for (reason, expected) in [
        (PauseReason::IoFailure, ControlState::IoFailure),
        (
            PauseReason::ConflictCapacity,
            ControlState::ConflictCapacity,
        ),
        (
            PauseReason::RecoveryRequired,
            ControlState::RecoveryRequired,
        ),
        (PauseReason::ObserverFailure, ControlState::Paused),
        (PauseReason::SourceChanging, ControlState::Paused),
        (PauseReason::SourceUnrepresentable, ControlState::Paused),
    ] {
        assert_eq!(state(Paused, &observation(Some(reason))), expected);
        assert_eq!(state(Running, &observation(Some(reason))), expected);
    }
    // A scan stop does not pause, so it shows only while the mirror runs.
    for (stop, expected) in [
        (ScanStop::SourceChanging, ControlState::SourceChanging),
        (
            ScanStop::SourceUnrepresentable,
            ControlState::SourceUnrepresentable,
        ),
    ] {
        let stopped = Summary {
            scan_stop: Some(stop),
            ..observation(None)
        };
        assert_eq!(state(Running, &stopped), expected);
        assert_eq!(state(Paused, &stopped), ControlState::Paused);
    }
    // The status keeps naming a scan stop as it always has.
    assert_eq!(
        serde_json::to_value(ScanStop::SourceUnrepresentable).unwrap(),
        "SourceUnrepresentable"
    );
    assert_eq!(
        PauseReason::from(ScanStop::SourceChanging),
        PauseReason::SourceChanging
    );
    let writeback = |state| {
        Some(crate::storage::sync_state::observation::WritebackSummary {
            state,
            reserved_bytes: 0,
            retained_bytes: 0,
            entries: 1,
            protected_entries: 1,
            max_bytes: 10,
            max_entries: 10,
            over_limit: false,
        })
    };
    let protected = Summary {
        writeback: writeback(Stop::ProtectedBody),
        scan_stop: Some(ScanStop::SourceChanging),
        ..observation(None)
    };
    assert_eq!(state(Running, &protected), ControlState::ProtectedBody);
    // A ready write-back holds nothing back, so the condition after it shows.
    let ready = Summary {
        writeback: writeback(Stop::Ready),
        scan_stop: Some(ScanStop::SourceChanging),
        ..observation(None)
    };
    assert_eq!(state(Running, &ready), ControlState::SourceChanging);
    let observing = Summary {
        baseline_complete: false,
        ..observation(None)
    };
    assert_eq!(state(Running, &observing), ControlState::Observing);
    // Only a finding that stops a wait holds the state at FindingsPending. Names the tree cannot
    // hold, and names it ignores, leave it Ready, as they leave a wait ready.
    let unmapped = Summary {
        mapping_findings: 2,
        unmapped_findings: 1,
        ..observation(None)
    };
    assert_eq!(state(Running, &unmapped), ControlState::Ready);
    let blocking = Summary {
        mapping_findings: 3,
        blocking_findings: 1,
        unmapped_findings: 1,
        ..observation(None)
    };
    assert_eq!(state(Running, &blocking), ControlState::FindingsPending);
}

/// `state` takes its values from one closed set, each serialized as its own name.
#[test]
fn status_states_serialize_as_their_names() {
    let names: std::collections::BTreeSet<_> = ControlState::ALL
        .iter()
        .map(|state| state.as_str())
        .collect();
    assert_eq!(names.len(), ControlState::ALL.len());
    for state in ControlState::ALL {
        assert_eq!(serde_json::to_value(state).unwrap(), state.as_str());
    }
    assert!(!names.contains("Unhealthy"));
}
