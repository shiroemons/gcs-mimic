//! The board alone, with passes the tests begin and finish themselves. Time is paused, so a
//! deadline passes as soon as nothing else can happen.

use super::*;
use crate::test_support::hang_deadline;
use std::time::UNIX_EPOCH;

fn running() -> Arc<SeedWatch> {
    let watch = Arc::new(SeedWatch::default());
    watch
        .state
        .send_modify(|board| board.interval = Some(Duration::from_secs(1)));
    watch
}

fn report(pending_deletions: usize, deleted: usize) -> PassReport {
    let counts = LiveCounts {
        pending_deletions,
        deleted,
        ..LiveCounts::default()
    };
    PassReport::new(
        (UNIX_EPOCH, UNIX_EPOCH),
        true,
        counts,
        Problems::default(),
        None,
    )
}

fn waiting(watch: &Arc<SeedWatch>) -> tokio::task::JoinHandle<Result<PassReport, WaitError>> {
    let watch = Arc::clone(watch);
    tokio::spawn(async move { watch.wait(hang_deadline()).await })
}

/// A pass that was running when the wait began may have walked the tree before the change the
/// caller waits for, and a pass that left a deletion waiting has not applied it yet. The wait
/// asks for one pass only, which a pass the interval starts may serve.
#[tokio::test(start_paused = true)]
async fn a_wait_takes_the_first_settled_pass_that_started_after_it() {
    let watch = running();
    let running_before = watch.begin();
    let wait = waiting(&watch);
    tokio::task::yield_now().await;
    assert_eq!(watch.state.borrow().wanted, running_before + 1);
    let second = waiting(&watch);
    tokio::task::yield_now().await;
    assert_eq!(watch.state.borrow().wanted, running_before + 1);
    watch.finish(running_before, report(0, 0));
    let pending = watch.begin();
    watch.finish(pending, report(1, 0));
    tokio::task::yield_now().await;
    assert!(!wait.is_finished());
    let settled = watch.begin();
    watch.finish(settled, report(0, 1));
    for wait in [wait, second] {
        assert_eq!(wait.await.unwrap().unwrap().counts.deleted, 1);
    }
}

#[tokio::test(start_paused = true)]
async fn a_wait_ends_at_its_deadline_or_when_the_passes_stop() {
    let watch = running();
    assert_eq!(
        watch.wait(hang_deadline()).await.unwrap_err(),
        WaitError::TimedOut
    );
    let wait = waiting(&watch);
    tokio::task::yield_now().await;
    watch.stop();
    assert_eq!(wait.await.unwrap().unwrap_err(), WaitError::Disabled);
    assert_eq!(
        watch.wait(hang_deadline()).await.unwrap_err(),
        WaitError::Disabled
    );
    assert!(!watch.status().enabled);
}

#[test]
fn a_pass_reports_at_most_a_hundred_problems_by_path() {
    let mut problems = Problems::default();
    for index in 0..MAX_PROBLEMS + 3 {
        problems.report(Some(&index.to_string()), &"unreadable");
    }
    let value = serde_json::to_value(&problems).unwrap();
    assert_eq!(value["problems"].as_array().unwrap().len(), MAX_PROBLEMS);
    assert_eq!(value["problemsOmitted"], 3);
}
