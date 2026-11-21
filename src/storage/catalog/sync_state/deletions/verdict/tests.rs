use super::*;
use crate::storage::sync_state::deletions::{BucketGone, Guard, WaitCause};

fn bucket(name: &str) -> BucketName {
    BucketName::new(name).unwrap()
}

/// An enabled guard over `baseline` tracked files of which `candidates` are gone, with no history,
/// no bucket gone whole and no held file.
fn counts(baseline: u64, candidates: u64) -> SetFacts {
    SetFacts {
        guard: Guard::Enabled,
        baseline,
        candidates,
        automatic: 0,
        gone: None,
        held: HeldFiles::Absent,
    }
}

fn waits_for(cause: WaitCause) -> SetVerdict {
    SetVerdict::Wait(cause)
}

#[test]
fn a_set_that_nothing_holds_back_runs() {
    assert_eq!(verdict(counts(0, 0)), SetVerdict::Run);
    assert_eq!(verdict(counts(1000, 1)), SetVerdict::Run);
}

#[test]
fn a_disabled_guard_runs_whatever_else_the_inventory_saw() {
    let everything = SetFacts {
        guard: Guard::Disabled,
        baseline: 100,
        candidates: 100,
        automatic: 500,
        gone: Some((bucket("gone-bucket"), BucketGone::Missing)),
        held: HeldFiles::Present,
    };
    assert_eq!(verdict(everything.clone()), SetVerdict::Run);
    let emptied = SetFacts {
        gone: Some((bucket("gone-bucket"), BucketGone::Emptied)),
        ..everything
    };
    assert_eq!(verdict(emptied), SetVerdict::Run);
}

#[test]
fn fewer_than_a_hundred_candidates_run_even_when_all_of_the_baseline_is_gone() {
    assert_eq!(verdict(counts(99, 99)), SetVerdict::Run);
}

#[test]
fn exactly_half_of_a_large_baseline_runs() {
    assert_eq!(verdict(counts(200, 100)), SetVerdict::Run);
}

#[test]
fn more_than_half_of_a_large_baseline_waits_for_approval() {
    assert_eq!(verdict(counts(199, 100)), waits_for(WaitCause::Threshold));
    assert_eq!(verdict(counts(100, 100)), waits_for(WaitCause::Threshold));
    assert_eq!(verdict(counts(1000, 600)), waits_for(WaitCause::Threshold));
}

#[test]
fn a_small_share_of_a_large_baseline_runs() {
    assert_eq!(verdict(counts(1000, 499)), SetVerdict::Run);
}

#[test]
fn earlier_automatic_deletions_count_on_both_sides_of_the_threshold() {
    let with_history = |baseline, candidates, automatic| SetFacts {
        automatic,
        ..counts(baseline, candidates)
    };
    // 50 + 50 reaches the floor of 100 and is exactly half of 50 + 150.
    assert_eq!(verdict(with_history(150, 50, 50)), SetVerdict::Run);
    // One file fewer in the baseline makes it more than half.
    assert_eq!(
        verdict(with_history(149, 50, 50)),
        waits_for(WaitCause::Threshold)
    );
    // Below the floor, history or not.
    assert_eq!(verdict(with_history(10, 10, 89)), SetVerdict::Run);
    // History alone can reach the floor when the files are few.
    assert_eq!(
        verdict(with_history(0, 0, 100)),
        waits_for(WaitCause::Threshold)
    );
}

#[test]
fn a_bucket_gone_whole_waits_whatever_the_counts() {
    for gone in [BucketGone::Missing, BucketGone::Emptied] {
        let facts = SetFacts {
            gone: Some((bucket("photos"), gone)),
            ..counts(1000, 1)
        };
        assert_eq!(
            verdict(facts),
            waits_for(WaitCause::Bucket {
                bucket: bucket("photos"),
                gone
            }),
            "{gone:?}"
        );
    }
}

#[test]
fn a_bucket_emptied_while_the_mirror_was_stopped_waits_even_when_it_held_one_file() {
    let facts = SetFacts {
        gone: Some((bucket("photos"), BucketGone::Emptied)),
        ..counts(1, 1)
    };
    assert_eq!(
        verdict(facts),
        waits_for(WaitCause::Bucket {
            bucket: bucket("photos"),
            gone: BucketGone::Emptied
        })
    );
}

#[test]
fn files_of_an_earlier_waiting_set_keep_a_small_set_waiting() {
    let facts = SetFacts {
        held: HeldFiles::Present,
        ..counts(1000, 1)
    };
    assert_eq!(verdict(facts), waits_for(WaitCause::Held));
}

#[test]
fn the_cause_named_is_the_first_that_applies() {
    let all = SetFacts {
        held: HeldFiles::Present,
        gone: Some((bucket("photos"), BucketGone::Missing)),
        ..counts(100, 100)
    };
    assert_eq!(
        verdict(all.clone()),
        waits_for(WaitCause::Bucket {
            bucket: bucket("photos"),
            gone: BucketGone::Missing
        })
    );
    let counted_and_held = SetFacts { gone: None, ..all };
    assert_eq!(verdict(counted_and_held), waits_for(WaitCause::Threshold));
}
