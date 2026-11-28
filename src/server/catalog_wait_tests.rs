//! A catalog wait that times out names the limit that ended it, because each limit is raised by
//! a different setting.

use super::*;
use crate::{cause::BackendFailure, service::activity::await_upload_catalog};

const BUDGET_MS: u64 = 200;

/// Waits on an operation that never finishes under a catalog `budget` and a request deadline
/// that starts with the wait, as the object request deadline does at dispatch.
async fn expire(budget: TimeLimit, request: TimeLimit) -> BackendFailure {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    let result: Result<(), ApiError> = await_upload_catalog(
        std::future::pending(),
        budget,
        request.deadline(),
        &activity,
    )
    .await;
    let error = result.expect_err("the operation never finishes");
    assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
    error
        .backend_failure()
        .expect("expected a classified catalog wait timeout")
}

#[tokio::test(start_paused = true)]
async fn a_wait_its_budget_ends_advises_the_catalog_budget() {
    assert_eq!(
        expire(
            TimeLimit::from_millis(BUDGET_MS),
            TimeLimit::from_millis(BUDGET_MS * 2)
        )
        .await,
        BackendFailure::UploadWaitTimeout
    );
}

#[tokio::test(start_paused = true)]
async fn a_wait_the_request_deadline_ends_advises_the_request_deadline() {
    assert_eq!(
        expire(
            TimeLimit::from_millis(BUDGET_MS),
            TimeLimit::from_millis(BUDGET_MS / 2)
        )
        .await,
        BackendFailure::FinalizationTimeout
    );
    assert_eq!(
        expire(TimeLimit::DISABLED, TimeLimit::from_millis(BUDGET_MS)).await,
        BackendFailure::FinalizationTimeout
    );
}
