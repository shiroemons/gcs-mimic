//! The board alone, with passes the tests begin and finish themselves. Time is paused, so a
//! deadline passes as soon as nothing else can happen.

use super::*;
use crate::test_support::hang_deadline;
use std::time::UNIX_EPOCH;

fn running() -> Arc<SeedWatch> {
    let watch = Arc::new(SeedWatch::default());
    watch
        .state
        .send_modify(|board| board.schedule = Schedule::Every(Duration::from_secs(1)));
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
        Ok(LiveOutcome {
            complete: true,
            counts,
            problems: Problems::default(),
        }),
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

/// A wait ends at its deadline, or once the catalog closes, and a wait where no pass is
/// configured ends at once and says so.
#[tokio::test(start_paused = true)]
async fn a_wait_ends_at_its_deadline_or_when_the_passes_stop() {
    let off = SeedWatch::default();
    assert_eq!(
        off.wait(hang_deadline()).await.unwrap_err(),
        WaitError::NotConfigured
    );
    assert!(!off.status().enabled);
    let watch = running();
    assert_eq!(
        watch.wait(hang_deadline()).await.unwrap_err(),
        WaitError::TimedOut
    );
    let wait = waiting(&watch);
    tokio::task::yield_now().await;
    watch.stop();
    assert_eq!(wait.await.unwrap().unwrap_err(), WaitError::Closed);
    assert_eq!(
        watch.wait(hang_deadline()).await.unwrap_err(),
        WaitError::Closed
    );
    assert!(!watch.status().enabled);
}

/// A pass that panics restarts the task, and readiness reports it down until a pass of the
/// replacement completes. The replacement passes at once, so a request that waited for the pass
/// the panic ended is served long before the interval.
#[tokio::test]
async fn a_pass_that_panics_drops_readiness_until_the_replacement_passes_at_once() {
    use std::sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    };
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let calls = Arc::new(AtomicU64::new(0));
    let (entered, mut replacement) = tokio::sync::mpsc::unbounded_channel();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let released = Arc::new(Mutex::new(Some(released)));
    catalog.start_seed_watch(Duration::from_secs(3600), move |_, _| {
        let call = calls.fetch_add(1, Ordering::SeqCst);
        let entered = entered.clone();
        let released = (call > 0)
            .then(|| released.lock().unwrap().take())
            .flatten();
        async move {
            assert!(call > 0, "injected seed pass panic");
            entered.send(()).unwrap();
            if let Some(released) = released {
                released.await.unwrap();
            }
            report(0, 0)
        }
    });
    assert!(catalog.background_tasks_running());
    let wait = {
        let catalog = catalog.clone();
        tokio::spawn(async move { catalog.seed_watch().wait(hang_deadline()).await })
    };
    tokio::time::timeout_at(hang_deadline(), replacement.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(!catalog.background_tasks_running());
    release.send(()).unwrap();
    wait.await.unwrap().unwrap();
    assert!(catalog.background_tasks_running());
    catalog.close().await.unwrap();
}

/// A pass that could not run reports why, listed nothing and is not clean; the times of every
/// report read as RFC 3339.
#[test]
fn a_pass_that_could_not_run_reports_why_and_changed_nothing() {
    let later = UNIX_EPOCH + Duration::from_millis(1500);
    let failed = PassReport::new((UNIX_EPOCH, later), Err("seed_dir is gone".to_owned()));
    assert_eq!(
        serde_json::to_value(&failed).unwrap(),
        serde_json::json!({
            "startedAt": "1970-01-01T00:00:00.000Z",
            "completedAt": "1970-01-01T00:00:01.500Z",
            "complete": false,
            "bucketsCreated": 0,
            "published": 0,
            "recorded": 0,
            "deleted": 0,
            "pendingDeletions": 0,
            "tombstones": 0,
            "bucketsKeptDeleted": 0,
            "problems": [],
            "problemsOmitted": 0,
            "error": "seed_dir is gone",
            "clean": false,
        })
    );
    assert_eq!(serde_json::to_value(report(0, 0)).unwrap()["clean"], true);
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
