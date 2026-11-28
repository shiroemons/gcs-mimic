use super::*;
use crate::{cause::BackendFailure, json::ApiError};
use hyper::StatusCode;
use std::assert_matches;
use std::time::Duration;
use tokio::{sync::oneshot, task::JoinHandle, time::advance};

const BUDGET: Duration = Duration::from_millis(200);
const REQUEST: Duration = Duration::from_secs(10);

type Waiting = (
    JoinHandle<Result<(), ApiError>>,
    watch::Sender<ChunkWait>,
    oneshot::Sender<()>,
);

async fn wait(waiting: ChunkWait) -> Waiting {
    wait_within(waiting, REQUEST).await
}

// Returns once the task is waiting, so its deadlines start before any advance.
async fn wait_within(waiting: ChunkWait, request: Duration) -> Waiting {
    let (progress, updates) = watch::channel(waiting);
    let (finish, finished) = oneshot::channel();
    // The request deadline starts as a handler starts it, on the paused clock.
    let deadline = TimeLimit::from_millis(request.as_millis() as u64).deadline();
    let task = tokio::spawn(async move {
        let (activity, _updates) = watch::channel(ObjectActivity::default());
        let operation = async {
            finished
                .await
                .map_err(|_| ApiError::from(ObjectError::Unavailable("dropped".into())))
        };
        let budget = TimeLimit::from_millis(BUDGET.as_millis() as u64);
        await_session_upload(operation, updates, budget, deadline, &activity).await
    });
    settle().await;
    (task, progress, finish)
}

async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

// Checks completion before awaiting: an idle paused runtime would otherwise
// auto-advance to whichever later deadline is pending.
async fn timed_out(task: JoinHandle<Result<(), ApiError>>) -> BackendFailure {
    settle().await;
    assert!(task.is_finished());
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
    error
        .backend_failure()
        .expect("expected a classified upload timeout")
}

#[tokio::test(start_paused = true)]
async fn work_outside_catalog_waits_uses_the_request_deadline() {
    let (task, _progress, finish) = wait(ChunkWait::Transfer).await;
    advance(BUDGET * 10).await;
    settle().await;
    assert!(!task.is_finished());
    finish.send(()).unwrap();
    task.await.unwrap().unwrap();

    let (task, _progress, _finish) = wait(ChunkWait::Transfer).await;
    advance(REQUEST - Duration::from_millis(1)).await;
    settle().await;
    assert!(!task.is_finished());
    advance(Duration::from_millis(1)).await;
    assert_matches!(timed_out(task).await, BackendFailure::FinalizationTimeout);
}

#[tokio::test(start_paused = true)]
async fn catalog_wait_times_out_after_its_budget() {
    let (task, _progress, _finish) = wait(ChunkWait::Catalog).await;
    advance(BUDGET - Duration::from_millis(1)).await;
    settle().await;
    assert!(!task.is_finished());
    advance(Duration::from_millis(1)).await;
    assert_matches!(timed_out(task).await, BackendFailure::UploadWaitTimeout);
}

/// A larger catalog budget would not have helped, so the reply advises the request deadline.
#[tokio::test(start_paused = true)]
async fn a_catalog_wait_the_request_deadline_ends_reports_finalization() {
    let (task, _progress, _finish) = wait_within(ChunkWait::Catalog, BUDGET / 2).await;
    advance(BUDGET / 2 - Duration::from_millis(1)).await;
    settle().await;
    assert!(!task.is_finished());
    advance(Duration::from_millis(1)).await;
    assert_matches!(timed_out(task).await, BackendFailure::FinalizationTimeout);
}

#[tokio::test(start_paused = true)]
async fn each_catalog_wait_starts_a_new_budget() {
    let (task, progress, _finish) = wait(ChunkWait::Catalog).await;
    advance(BUDGET - Duration::from_millis(1)).await;
    progress.send_replace(ChunkWait::Transfer);
    settle().await;
    advance(BUDGET * 5).await;
    progress.send_replace(ChunkWait::Catalog);
    settle().await;
    advance(BUDGET - Duration::from_millis(1)).await;
    settle().await;
    assert!(!task.is_finished());
    advance(Duration::from_millis(1)).await;
    assert_matches!(timed_out(task).await, BackendFailure::UploadWaitTimeout);
}
