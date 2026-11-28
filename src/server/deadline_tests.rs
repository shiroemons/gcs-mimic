//! The limit a connection enforces on a request, and the reply it sends once that limit expires.
//! The claims behind them are tested with `service::activity`.

use super::*;
use crate::service::activity::BodyRead;

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
    let pending_body = ObjectActivity::in_stage(RequestStage::Running, BodyRead::Pending);
    assert_eq!(limit(pending_body), limits.body_inactivity);
    assert_eq!(pending_body.step().expiry(), Expiry::BodyInactivity);
    // Deadlines pause during a catalog wait, and its own budget answers for it.
    let catalog_wait = ObjectActivity::in_stage(RequestStage::AwaitingCatalog, BodyRead::Pending);
    assert_eq!(limit(catalog_wait), limits.request);
    let response = ObjectActivity::in_stage(RequestStage::Responded, BodyRead::NotPending);
    assert_eq!(limit(response), limits.body_inactivity);
    assert_eq!(ObjectActivity::default().step().expiry(), Expiry::Request);
}

/// The reply follows the limit that expired, not only the route: a silent client and a slow
/// server end the same upload with different documented errors.
#[tokio::test]
async fn the_timeout_reply_names_the_limit_that_expired() {
    use http_body_util::BodyExt;
    /// The status, the error code or reason, and the setting the message tells the client to
    /// raise. "raise" keeps `request_timeout_ms` from matching inside `object_request_timeout_ms`.
    type Expected = (u16, &'static str, &'static str);
    const JSON_SILENT: Expected = (
        408,
        r#""uploadBrokenConnection""#,
        "raise body_inactivity_timeout_ms",
    );
    const JSON_UPLOAD: Expected = (
        408,
        r#""uploadBrokenConnection""#,
        "raise object_request_timeout_ms",
    );
    const JSON_TRANSFER: Expected = (503, r#""backendError""#, "raise object_request_timeout_ms");
    const JSON_SERVER: Expected = (503, r#""backendError""#, "raise request_timeout_ms");
    const XML_SILENT: Expected = (
        400,
        "<Code>RequestTimeout</Code>",
        "raise body_inactivity_timeout_ms",
    );
    const XML_UPLOAD: Expected = (
        408,
        "<Code>uploadBrokenConnection</Code>",
        "raise object_request_timeout_ms",
    );
    const XML_TRANSFER: Expected = (
        503,
        "<Code>ServiceUnavailable</Code>",
        "raise object_request_timeout_ms",
    );
    const XML_SERVER: Expected = (
        503,
        "<Code>ServiceUnavailable</Code>",
        "raise request_timeout_ms",
    );
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
            JSON_TRANSFER,
        ),
        (
            ReplyApi::Json,
            Payload::Upload,
            Expiry::Request,
            JSON_SERVER,
        ),
        (ReplyApi::Json, Payload::Other, Expiry::Request, JSON_SERVER),
        (xml, Payload::Other, Expiry::BodyInactivity, XML_SILENT),
        (xml, Payload::Upload, Expiry::BodyInactivity, XML_SILENT),
        (xml, Payload::Upload, Expiry::ObjectRequest, XML_UPLOAD),
        (xml, Payload::Other, Expiry::ObjectRequest, XML_TRANSFER),
        (xml, Payload::Upload, Expiry::Request, XML_SERVER),
        (xml, Payload::Other, Expiry::Request, XML_SERVER),
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
