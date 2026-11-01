//! The connection deadline and the start of a catalog wait claim a request exclusively.
//!
//! Detached catalog work outlives its handler, so a timeout reply that raced the start of
//! such work would report failure for a mutation that still commits.

use super::*;

/// How long a test lets a catalog wait run before treating it as blocked.
const BLOCKED: Duration = Duration::from_millis(50);

#[tokio::test]
async fn an_expired_request_never_starts_a_catalog_wait() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    assert!(claim_expiry(&activity));

    let started = tokio::time::timeout(BLOCKED, begin_catalog_wait(&activity)).await;

    assert!(started.is_err(), "the catalog wait began after the expiry");
    assert!(!activity.borrow().awaiting_catalog);
}

#[tokio::test]
async fn a_started_catalog_wait_refuses_the_expiry() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    begin_catalog_wait(&activity).await;

    assert!(!claim_expiry(&activity));
    assert!(!activity.borrow().expired);
}

#[tokio::test]
async fn a_new_request_deadline_keeps_an_earlier_expiry() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    assert!(claim_expiry(&activity));

    let deadline = Some(tokio::time::Instant::now() + Duration::from_secs(60));
    let started =
        tokio::time::timeout(BLOCKED, begin_catalog_wait_until(&activity, deadline)).await;

    assert!(
        started.is_err(),
        "replacing the deadline cleared the expiry"
    );
}

#[tokio::test]
async fn the_expiry_resumes_after_the_catalog_wait_ends() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    begin_catalog_wait(&activity).await;
    activity.send_modify(|progress| progress.awaiting_catalog = false);

    assert!(claim_expiry(&activity));
}
