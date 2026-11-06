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
    assert_eq!(claim_expiry(&activity), ExpiryClaim::Granted);

    let started = tokio::time::timeout(BLOCKED, begin_catalog_wait(&activity)).await;

    assert!(started.is_err(), "the catalog wait began after the expiry");
    assert_eq!(activity.borrow().stage, RequestStage::Expired);
}

#[tokio::test]
async fn a_started_catalog_wait_refuses_the_expiry() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    let _wait = begin_catalog_wait(&activity).await;

    assert_eq!(claim_expiry(&activity), ExpiryClaim::Refused);
    assert_eq!(activity.borrow().stage, RequestStage::AwaitingCatalog);
}

#[tokio::test]
async fn a_new_request_deadline_keeps_an_earlier_expiry() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    assert_eq!(claim_expiry(&activity), ExpiryClaim::Granted);

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
    drop(begin_catalog_wait(&activity).await);

    assert_eq!(claim_expiry(&activity), ExpiryClaim::Granted);
}

/// A handler that returns early, or is dropped, must not leave the request's deadlines paused.
#[tokio::test]
async fn a_catalog_wait_ends_on_every_path_out_of_the_waiting_code() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    let early_return = async {
        let _wait = begin_catalog_wait(&activity).await;
        Err::<(), _>("failed")?;
        Ok::<_, &str>(())
    };
    assert!(early_return.await.is_err());
    assert_eq!(activity.borrow().stage, RequestStage::Running);

    let cancelled = async {
        let _wait = begin_catalog_wait(&activity).await;
        std::future::pending::<()>().await;
    };
    assert!(tokio::time::timeout(BLOCKED, cancelled).await.is_err());
    assert_eq!(activity.borrow().stage, RequestStage::Running);
}

#[tokio::test]
async fn ending_a_wait_leaves_later_stages_alone() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    for stage in [
        RequestStage::Expired,
        RequestStage::Responded,
        RequestStage::Finished,
    ] {
        activity.send_modify(|progress| progress.stage = stage);
        record_progress(&activity);
        assert_eq!(activity.borrow().stage, stage);
    }
}

#[test]
fn a_started_response_refuses_a_timeout_reply() {
    let (activity, _progress) = watch::channel(ObjectActivity {
        stage: RequestStage::Responded,
        ..ObjectActivity::default()
    });
    assert_eq!(claim_expiry(&activity), ExpiryClaim::ResponseStarted);
    assert_eq!(activity.borrow().stage, RequestStage::Responded);
}

#[test]
fn a_finished_request_refuses_an_expiry() {
    let (activity, _progress) = watch::channel(ObjectActivity {
        stage: RequestStage::Finished,
        ..ObjectActivity::default()
    });
    assert_eq!(claim_expiry(&activity), ExpiryClaim::Refused);
}

/// A batch handles its parts as object requests, and a later part must still see the expiry.
#[tokio::test]
async fn finishing_an_object_request_keeps_a_claimed_expiry() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    assert_eq!(claim_expiry(&activity), ExpiryClaim::Granted);
    activity.send_modify(ObjectActivity::finish_object);

    let started = tokio::time::timeout(BLOCKED, begin_catalog_wait(&activity)).await;
    assert!(started.is_err(), "the finish cleared the expiry");
}

#[test]
fn only_client_paced_bodies_move_a_request_to_the_inactivity_limit() {
    let limits = ConnectionLimits {
        header_read: TimeLimit::from_millis(1),
        keep_alive: TimeLimit::from_millis(2),
        request: TimeLimit::from_millis(3),
        body_inactivity: TimeLimit::from_millis(4),
    };
    let limit = |activity| limits.during_request(&activity);
    assert_eq!(limit(ObjectActivity::default()), limits.request);
    let small_body = ObjectActivity {
        body_read: BodyRead::Pending,
        ..Default::default()
    };
    assert_eq!(limit(small_body), limits.request);
    let finished_upload = ObjectActivity {
        body_pace: BodyPace::ClientPaced,
        ..Default::default()
    };
    assert_eq!(limit(finished_upload), limits.request);
    let upload = ObjectActivity {
        body_read: BodyRead::Pending,
        body_pace: BodyPace::ClientPaced,
        ..Default::default()
    };
    assert_eq!(limit(upload), limits.body_inactivity);
    let response = ObjectActivity {
        stage: RequestStage::Responded,
        ..Default::default()
    };
    assert_eq!(limit(response), limits.body_inactivity);
}

#[tokio::test]
async fn the_handler_learns_of_a_claimed_expiry_from_the_shared_channel() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    assert!(
        tokio::time::timeout(BLOCKED, expiry_claimed(&activity))
            .await
            .is_err()
    );
    assert_eq!(claim_expiry(&activity), ExpiryClaim::Granted);
    tokio::time::timeout(BLOCKED, expiry_claimed(&activity))
        .await
        .unwrap();
}
