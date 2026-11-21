use super::*;
use crate::sync::report::{command::Advised, remedy::pause_remedy};

/// Only the reasons a wait meets while work is still under way are worth waiting again
/// for. A wait that ended on any holdup but `Ready` has the remedy of that holdup, which
/// names what to run with the endpoint to fill in; a success, a shutdown and a rebuilt
/// journal need none. One that ran out of time and one the server answered after its minute
/// read alike.
#[test]
fn only_work_in_progress_is_waited_for_again_and_the_rest_name_a_remedy() {
    use crate::storage::sync_state::writeback::Halt;
    use WaitReason::{JournalChanged, Ready, SourceChanging, Timeout};
    for reason in WaitReason::ALL {
        assert_eq!(
            reason.is_in_progress(),
            matches!(reason, Timeout | SourceChanging | JournalChanged),
            "{reason}"
        );
    }
    for holdup in [
        Holdup::Paused(PauseReason::IoFailure),
        Holdup::Writeback(Halt::ProtectedBody),
        Holdup::SourceChanging,
        Holdup::SourceUnrepresentable,
        Holdup::SourceUnreadable,
        Holdup::Observing,
        Holdup::DeletionApproval,
        Holdup::FindingsPending,
        Holdup::UnmappedObjects { count: 2 },
    ] {
        let remedy = holdup.remedy(AtHand::default());
        assert!(
            remedy
                .as_ref()
                .is_some_and(|remedy| remedy.contains("--endpoint URL")),
            "{holdup:?}"
        );
        let ended = WaitResult::held(holdup, None, None);
        assert_eq!(ended.reason, WaitReason::from(holdup), "{holdup:?}");
        assert_eq!(ended.remedy(AtHand::default()), remedy, "{holdup:?}");
    }
    assert_eq!(Holdup::Ready.remedy(AtHand::default()), None);
    let ready = WaitResult::held(Holdup::Ready, None, None);
    assert_eq!((ready.reason, ready.completed), (Ready, true));
    assert_eq!(ready.remedy(AtHand::default()), None);
    for why in [Unfinished::Stopped, Unfinished::JournalChanged] {
        let ended = WaitResult::unfinished(why, None, None);
        assert_eq!(ended.remedy(AtHand::default()), None, "{why:?}");
    }
    let timed_out = WaitResult::timeout().remedy(AtHand::default());
    assert_eq!(
        timed_out,
        WaitResult::unfinished(Unfinished::SourceChanging, None, None).remedy(AtHand::default())
    );
    assert!(
        timed_out.is_some_and(|remedy| remedy.ends_with(": gcs-mimic sync wait --endpoint URL"))
    );
}

/// Files of a rejected set left to write back open the advice of a wait that ran out of
/// time or paused, counted, with the log that says why their write-back stopped. Since the
/// server writes them itself, a wait with nothing else changing does not ask the programs
/// writing in the directory to finish; what a pass saw changing keeps that request. Each
/// says how to leave out a file whose write-back keeps failing, and the pause keeps its own
/// remedy after the count.
#[test]
fn files_left_to_write_back_open_the_advice_of_a_wait() {
    let left = |rejected| AtHand {
        rejected_writeback: rejected,
        ..AtHand::default()
    };
    let writers = "Let the programs writing in the directory or through the API finish";
    let leave_out = "delete its object through the API, which the write-back then skips";
    for (rejected, opening) in [
        (
            1,
            "1 file of the rejected deletion set remains to write back, and the sync_deletions_rejected log names why if its write-back stopped. ",
        ),
        (
            3,
            "3 files of the rejected deletion set remain to write back, and the sync_deletions_rejected log names why if their write-back stopped. ",
        ),
    ] {
        let timed_out = WaitResult::timeout().remedy(left(rejected)).unwrap();
        assert!(timed_out.starts_with(opening), "{timed_out}");
        assert!(
            timed_out.contains("The server writes them back itself; wait again"),
            "{timed_out}"
        );
        assert!(!timed_out.contains(writers), "{timed_out}");
        assert!(timed_out.ends_with(leave_out), "{timed_out}");
        let observing = WaitResult::held(Holdup::Observing, None, None)
            .remedy(left(rejected))
            .unwrap();
        assert!(observing.starts_with(opening), "{observing}");
        assert!(observing.contains(writers), "{observing}");
        assert!(observing.ends_with(leave_out), "{observing}");
        let pause = pause_remedy(PauseReason::IoFailure, AtHand::default());
        let paused = WaitResult::held(Holdup::Paused(PauseReason::IoFailure), None, None)
            .remedy(left(rejected))
            .unwrap();
        assert_eq!(
            paused,
            format!(
                "{opening}{pause}; if the write-back of a file keeps failing and you no longer need the file, {leave_out}, before you resume"
            )
        );
    }
    let timed_out = WaitResult::timeout().remedy(left(0)).unwrap();
    assert!(
        timed_out.starts_with(&format!(
            "The directory was still changing when the wait ended. {writers}"
        )),
        "{timed_out}"
    );
    assert!(!timed_out.contains("rejected"), "{timed_out}");
}

/// A wait that refuses objects whose names the tree cannot hold ends on them only when it
/// is otherwise ready and some remain, as FindingsPending with a remedy of their own that
/// lists them and names nothing else; any other end stays as the mirror decided it.
#[test]
fn a_wait_that_refuses_unmapped_objects_ends_on_them_alone() {
    let refused = with_unmapped_found(WaitResult::held(Holdup::Ready, None, None), 2)
        .with_unmapped(UnmappedObjects::Refuse);
    assert_eq!(refused.reason, WaitReason::FindingsPending);
    assert!(!refused.completed);
    assert_eq!(refused.holdup(), Some(Holdup::UnmappedObjects { count: 2 }));
    let at_hand = AtHand {
        findings_revision: Some(crate::sync::report::catalog::SAMPLE_FINDINGS_REVISION),
        ..AtHand::default()
    };
    let remedy = refused.remedy(at_hand).unwrap();
    assert!(
        remedy.starts_with(
            "2 objects in the API cannot be written to the sync directory because of their names"
        ),
        "{remedy}"
    );
    assert!(
        remedy.ends_with(&format!(
            "the findings view lists them: {}",
            Advised::Findings(at_hand.findings_revision)
        )),
        "{remedy}"
    );
    assert!(!remedy.contains("conflicts"), "{remedy}");
    for (holdup, unmapped, policy) in [
        (Holdup::Ready, 2, UnmappedObjects::Report),
        (Holdup::Ready, 0, UnmappedObjects::Refuse),
        (Holdup::FindingsPending, 2, UnmappedObjects::Refuse),
    ] {
        let kept = with_unmapped_found(WaitResult::held(holdup, None, None), unmapped)
            .with_unmapped(policy);
        assert_eq!(
            (kept.reason, kept.holdup()),
            (WaitReason::from(holdup), Some(holdup)),
            "{policy:?}"
        );
    }
}

/// A wait's reason and the holdup its remedy follows come from one ending, so the two
/// cannot disagree. Only a pass's holdup makes a wait ready, so every ready wait meets the
/// request's policy for unmapped objects. A wait cut short keeps the reason it was cut short
/// for, and with it the exit status that reason maps to, whatever its last pass found: a
/// ready pass whose deadline then passed still times out, rather than ending on the
/// unmapped objects that pass counted.
#[test]
fn a_waits_reason_and_its_holdup_come_from_one_ending() {
    for why in [
        Unfinished::Timeout,
        Unfinished::Stopped,
        Unfinished::JournalChanged,
        Unfinished::SourceChanging,
    ] {
        let ended = with_unmapped_found(WaitResult::unfinished(why, None, None), 2)
            .with_unmapped(UnmappedObjects::Refuse);
        assert_eq!(
            (ended.reason, ended.holdup(), ended.completed),
            (WaitReason::from(why), None, false),
            "{why:?}"
        );
        let cut = with_unmapped_found(WaitResult::held(Holdup::Ready, None, None), 2)
            .cut_short(why)
            .with_unmapped(UnmappedObjects::Refuse);
        assert_eq!(
            (cut.reason, cut.holdup(), cut.completed),
            (WaitReason::from(why), Some(Holdup::Ready), false),
            "{why:?}"
        );
        assert_eq!(cut.timed_out, why == Unfinished::Timeout, "{why:?}");
        assert_eq!(cut.findings.map(|found| found.unmapped), Some(2));
    }
}

fn with_unmapped_found(result: WaitResult, unmapped: u64) -> WaitResult {
    result.with_findings(Some(WaitFindings {
        unmapped,
        observation_revision: 1,
        ..WaitFindings::default()
    }))
}

/// A wait that ran out of time names what its last pass was still waiting for: how many
/// files were still changing and the first of them, the files written after the pass took
/// the tree in, or an API that kept changing; and the pause a paused wait met is the one the
/// mirror answered with.
#[test]
fn a_wait_that_runs_out_of_time_names_what_remained() {
    let mut pass = Reconciliation::default();
    for name in ["bucket/a", "bucket/b", "bucket/c", "bucket/d"] {
        pass.count_deferred(std::path::Path::new(name));
    }
    let remedy = WaitResult::held(Holdup::SourceChanging, None, Some(pass))
        .cut_short(Unfinished::Timeout)
        .remedy(AtHand::default())
        .unwrap();
    assert!(
            remedy.starts_with(
                "4 files were still changing or racing an API write when the wait ended, the first bucket/a, bucket/b, bucket/c. "
            ),
            "{remedy}"
        );
    let written = Reconciliation {
        external_only: 1,
        ..Reconciliation::default()
    };
    let remedy = WaitResult::unfinished(Unfinished::Timeout, None, Some(written))
        .remedy(AtHand::default())
        .unwrap();
    assert!(
        remedy.starts_with("1 file was written after the wait's pass took the tree in. "),
        "{remedy}"
    );
    let remedy = WaitResult::held(Holdup::Observing, None, None)
        .remedy(AtHand::default())
        .unwrap();
    assert!(
        remedy.starts_with("The API changed during each pass"),
        "{remedy}"
    );
    let paused = WaitResult::held(Holdup::Paused(PauseReason::IoFailure), None, None);
    assert_eq!(paused.paused_reason(), Some(PauseReason::IoFailure));
    assert_eq!(
        paused.remedy(AtHand::default()),
        Some(pause_remedy(PauseReason::IoFailure, AtHand::default()))
    );
}
