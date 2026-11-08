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
    assert_eq!(
        claim_expiry(&activity, Expiry::Request),
        ExpiryClaim::Granted
    );

    let started = tokio::time::timeout(BLOCKED, begin_catalog_wait(&activity)).await;

    assert!(started.is_err(), "the catalog wait began after the expiry");
    assert_eq!(
        activity.borrow().stage,
        RequestStage::Expired(Expiry::Request)
    );
}

#[tokio::test]
async fn a_started_catalog_wait_refuses_the_expiry() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    let _wait = begin_catalog_wait(&activity).await;

    assert_eq!(
        claim_expiry(&activity, Expiry::Request),
        ExpiryClaim::Refused
    );
    assert_eq!(activity.borrow().stage, RequestStage::AwaitingCatalog);
}

#[tokio::test]
async fn a_new_request_deadline_keeps_an_earlier_expiry() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    assert_eq!(
        claim_expiry(&activity, Expiry::ObjectRequest),
        ExpiryClaim::Granted
    );

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

    assert_eq!(
        claim_expiry(&activity, Expiry::Request),
        ExpiryClaim::Granted
    );
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
        RequestStage::Expired(Expiry::BodyInactivity),
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
    assert_eq!(
        claim_expiry(&activity, Expiry::BodyInactivity),
        ExpiryClaim::ResponseStarted
    );
    assert_eq!(activity.borrow().stage, RequestStage::Responded);
}

#[test]
fn a_finished_request_refuses_an_expiry() {
    let (activity, _progress) = watch::channel(ObjectActivity {
        stage: RequestStage::Finished,
        ..ObjectActivity::default()
    });
    assert_eq!(
        claim_expiry(&activity, Expiry::Request),
        ExpiryClaim::Refused
    );
}

/// A batch handles its parts as object requests, and a later part must still see the expiry.
#[tokio::test]
async fn finishing_an_object_request_keeps_a_claimed_expiry() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    assert_eq!(
        claim_expiry(&activity, Expiry::ObjectRequest),
        ExpiryClaim::Granted
    );
    activity.send_modify(ObjectActivity::finish_object);

    let started = tokio::time::timeout(BLOCKED, begin_catalog_wait(&activity)).await;
    assert!(started.is_err(), "the finish cleared the expiry");
    assert_eq!(
        activity.borrow().stage,
        RequestStage::Expired(Expiry::ObjectRequest)
    );
}

/// Every JSON document may reach 16 MiB, so no route's body arrives at once: whichever route
/// reads it, a pending body waits on the client.
#[test]
fn a_pending_request_body_moves_any_request_to_the_inactivity_limit() {
    let limits = ConnectionLimits {
        header_read: TimeLimit::from_millis(1),
        keep_alive: TimeLimit::from_millis(2),
        request: TimeLimit::from_millis(3),
        body_inactivity: TimeLimit::from_millis(4),
    };
    let limit = |activity| limits.during_request(&activity);
    assert_eq!(limit(ObjectActivity::default()), limits.request);
    let pending_body = ObjectActivity {
        body_read: BodyRead::Pending,
        ..Default::default()
    };
    assert_eq!(limit(pending_body), limits.body_inactivity);
    assert_eq!(pending_body.step().expiry(), Expiry::BodyInactivity);
    // Deadlines pause during a catalog wait, and its own budget answers for it.
    let catalog_wait = ObjectActivity {
        stage: RequestStage::AwaitingCatalog,
        body_read: BodyRead::Pending,
        ..Default::default()
    };
    assert_eq!(limit(catalog_wait), limits.request);
    let response = ObjectActivity {
        stage: RequestStage::Responded,
        ..Default::default()
    };
    assert_eq!(limit(response), limits.body_inactivity);
    assert_eq!(ObjectActivity::default().step().expiry(), Expiry::Request);
}

#[tokio::test]
async fn the_handler_learns_of_a_claimed_expiry_from_the_shared_channel() {
    let (activity, _progress) = watch::channel(ObjectActivity::default());
    assert!(
        tokio::time::timeout(BLOCKED, expiry_claimed(&activity))
            .await
            .is_err()
    );
    assert_eq!(
        claim_expiry(&activity, Expiry::BodyInactivity),
        ExpiryClaim::Granted
    );
    let claimed = tokio::time::timeout(BLOCKED, expiry_claimed(&activity))
        .await
        .unwrap();
    assert_eq!(claimed, Expiry::BodyInactivity);
}

/// The reply follows the limit that expired, not only the route: a silent client and a slow
/// server end the same upload with different documented errors.
#[tokio::test]
async fn the_timeout_reply_names_the_limit_that_expired() {
    use http_body_util::BodyExt;
    /// The status, the error code or reason, and the setting the message names.
    type Expected = (u16, &'static str, &'static str);
    const JSON_SILENT: Expected = (
        408,
        r#""uploadBrokenConnection""#,
        "body_inactivity_timeout_ms",
    );
    const JSON_UPLOAD: Expected = (
        408,
        r#""uploadBrokenConnection""#,
        "object_request_timeout_ms",
    );
    const JSON_SERVER: Expected = (503, r#""backendError""#, "request_timeout_ms");
    const XML_SILENT: Expected = (
        400,
        "<Code>RequestTimeout</Code>",
        "body_inactivity_timeout_ms",
    );
    const XML_UPLOAD: Expected = (
        408,
        "<Code>uploadBrokenConnection</Code>",
        "object_request_timeout_ms",
    );
    const XML_SERVER: Expected = (503, "<Code>ServiceUnavailable</Code>", "request_timeout_ms");
    let xml = ReplyApi::Xml(xml::ErrorBody::Include);
    let cases = [
        (
            ReplyApi::Json,
            Payload::Other,
            Expiry::BodyInactivity,
            JSON_SILENT,
        ),
        (
            ReplyApi::Json,
            Payload::Upload,
            Expiry::BodyInactivity,
            JSON_SILENT,
        ),
        (
            ReplyApi::Json,
            Payload::Upload,
            Expiry::ObjectRequest,
            JSON_UPLOAD,
        ),
        (
            ReplyApi::Json,
            Payload::Other,
            Expiry::ObjectRequest,
            JSON_SERVER,
        ),
        (
            ReplyApi::Json,
            Payload::Upload,
            Expiry::Request,
            JSON_SERVER,
        ),
        (xml, Payload::Other, Expiry::BodyInactivity, XML_SILENT),
        (xml, Payload::Upload, Expiry::BodyInactivity, XML_SILENT),
        (xml, Payload::Upload, Expiry::ObjectRequest, XML_UPLOAD),
        (xml, Payload::Other, Expiry::ObjectRequest, XML_SERVER),
        (xml, Payload::Upload, Expiry::Request, XML_SERVER),
    ];
    for (api, payload, expiry, (status, code, setting)) in cases {
        let response = DeadlineReply { api, payload }.response(expiry);
        let case = format!("{api:?} {payload:?} {expiry:?}");
        assert_eq!(response.status().as_u16(), status, "{case}");
        assert_eq!(response.headers()["connection"], "close", "{case}");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains(code), "{case}: {body}");
        assert!(body.contains(setting), "{case}: {body}");
    }
}
