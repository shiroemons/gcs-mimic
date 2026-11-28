//! The limits on each phase of a connection, and the reply a request gets once its deadline
//! expires. The watch of [`super::connection`] applies the limits, and the handshake reports a
//! client that never sends its request head through [`warn_connection_timeout`].

use super::{
    routing,
    slots::{IdlePlace, IdleTurn},
};
use crate::{
    config::Config,
    json::{ApiError, JsonErrorResponse},
    service::{
        activity::{AfterResponse, Expiry, ObjectActivity, RequestStep},
        body::{ResponseBody, boxed, has_body},
    },
    time_limit::TimeLimit,
    xml,
};
use hyper::{Method, Request, Response, StatusCode};
use std::time::Duration;

/// How long writing a timeout reply and closing the socket may take. These steps stay bounded
/// even with every timeout setting disabled, so a client that stopped reading cannot keep the
/// connection after the server has given up on it.
pub(super) const CLOSE_BUDGET: Duration = Duration::from_secs(5);

/// The response for a request whose deadline expires before its handler answers. The
/// connection picks it when the deadline expires, from the limit that expired: the request's
/// route alone cannot tell a silent client from a slow server. Cloud Storage documents 408 and
/// 503 as retryable; the XML API documents a silent socket as 400 `RequestTimeout`, which
/// clients do not retry, and this reply matches that.
pub(super) struct DeadlineReply {
    pub(super) api: ReplyApi,
    pub(super) payload: Payload,
}

/// Which API's error document a timeout reply carries.
#[derive(Clone, Copy, Debug)]
pub(super) enum ReplyApi {
    Json,
    /// An XML error omits its document for HEAD.
    Xml(xml::ErrorBody),
}

/// What the request sends, which decides how its whole-request limit is reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Payload {
    /// Object data with a body: `/upload/...`, or an XML PUT or POST.
    Upload,
    Other,
}

/// The JSON error for `expiry` on a request that sends `payload`, naming the setting that bounds
/// the expired limit.
fn json_timeout(expiry: Expiry, payload: Payload) -> ApiError {
    match (expiry, payload) {
        (Expiry::BodyInactivity, Payload::Upload | Payload::Other) => request_body_silent(),
        // Only a transfer's whole-request limit reports a broken upload: a server that is
        // slow to answer an upload is still a server-side timeout.
        (Expiry::ObjectRequest, Payload::Upload) => upload_timed_out(),
        (Expiry::ObjectRequest, Payload::Other) => object_request_timed_out(),
        (Expiry::Request, Payload::Upload | Payload::Other) => request_timed_out(),
    }
}

const REQUEST_TIMED_OUT: &str = "The request timed out before a response was ready; retry the request, or raise request_timeout_ms (0 disables it).";

fn request_timed_out() -> ApiError {
    ApiError::service_unavailable(REQUEST_TIMED_OUT)
}

/// The JSON API documents 408 `uploadBrokenConnection` as its only request timeout.
fn request_body_silent() -> ApiError {
    ApiError::new(
        StatusCode::REQUEST_TIMEOUT,
        "uploadBrokenConnection",
        "The request timed out: no request body bytes arrived within body_inactivity_timeout_ms; retry the request, or raise body_inactivity_timeout_ms for slow clients (0 disables it).",
    )
}

/// The XML API documents a silent socket as 400 `RequestTimeout`; this repeats its meaning.
const XML_REQUEST_BODY_SILENT: &str = "Your socket connection to the server was not read from or written to within the timeout period: no request body bytes arrived within body_inactivity_timeout_ms; retry the request, or raise body_inactivity_timeout_ms for slow clients (0 disables it).";

const UPLOAD_TIMED_OUT: &str = "The upload timed out: it did not finish within object_request_timeout_ms; retry the request, or raise object_request_timeout_ms for large uploads (0 disables it).";

fn upload_timed_out() -> ApiError {
    ApiError::new(
        StatusCode::REQUEST_TIMEOUT,
        "uploadBrokenConnection",
        UPLOAD_TIMED_OUT,
    )
}

const OBJECT_REQUEST_TIMED_OUT: &str = "The request timed out: it did not finish within object_request_timeout_ms; retry the request, or raise object_request_timeout_ms for large transfers (0 disables it).";

/// A copy, rewrite, compose or media read past its whole-request limit: a server-side timeout.
fn object_request_timed_out() -> ApiError {
    ApiError::service_unavailable(OBJECT_REQUEST_TIMED_OUT)
}

impl DeadlineReply {
    /// The reply for the API that serves `target`, which `request` now addresses.
    pub(super) fn new<B>(request: &Request<B>, target: &routing::Target) -> Self {
        let headers = request.headers();
        let api = if target.answers_xml() {
            ReplyApi::Xml(xml::ErrorBody::for_method(request.method()))
        } else {
            ReplyApi::Json
        };
        let upload_route = request.uri().path().starts_with("/upload/")
            || (matches!(api, ReplyApi::Xml(_))
                && matches!(*request.method(), Method::PUT | Method::POST));
        let payload = if has_body(headers) && upload_route {
            Payload::Upload
        } else {
            Payload::Other
        };
        Self { api, payload }
    }

    fn xml_error(&self, expiry: Expiry) -> xml::Error {
        match (expiry, self.payload) {
            // The XML API lists a silent socket under 400, apart from the JSON API's 408.
            (Expiry::BodyInactivity, _) => xml::Error::request_timeout(XML_REQUEST_BODY_SILENT),
            (Expiry::Request, _) => xml::Error::service_unavailable(REQUEST_TIMED_OUT),
            (Expiry::ObjectRequest, Payload::Upload) => {
                xml::Error::upload_broken_connection(UPLOAD_TIMED_OUT)
            }
            (Expiry::ObjectRequest, Payload::Other) => {
                xml::Error::service_unavailable(OBJECT_REQUEST_TIMED_OUT)
            }
        }
    }

    pub(super) fn response(self, expiry: Expiry) -> Response<ResponseBody> {
        let mut response = match self.api {
            ReplyApi::Json => boxed(json_timeout(expiry, self.payload).into_response()),
            ReplyApi::Xml(body) => boxed(self.xml_error(expiry).response(body)),
        };
        response.headers_mut().insert(
            "connection",
            hyper::header::HeaderValue::from_static("close"),
        );
        response
    }
}

/// What a batch part answers once [`batch_part_expired`](crate::service::activity::batch_part_expired)
/// has claimed its timeout. A part's body is already buffered, so it is never an upload.
pub(super) fn batch_part_timeout(expiry: Expiry) -> Response<ResponseBody> {
    boxed(json_timeout(expiry, Payload::Other).into_response())
}

/// How long each phase of a connection may last without progress.
#[derive(Clone, Copy)]
pub(super) struct ConnectionLimits {
    pub(super) header_read: TimeLimit,
    pub(super) keep_alive: TimeLimit,
    pub(super) request: TimeLimit,
    pub(super) body_inactivity: TimeLimit,
}

impl ConnectionLimits {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            header_read: config.header_read_timeout(),
            keep_alive: config.keep_alive_timeout(),
            request: config.request_timeout(),
            body_inactivity: config.body_inactivity_timeout(),
        }
    }

    pub(super) fn during_request(&self, activity: &ObjectActivity) -> TimeLimit {
        match activity.step() {
            RequestStep::ClientPaced => self.body_inactivity,
            RequestStep::ServerPaced => self.request,
        }
    }
}

/// What a connection is waiting for, which selects the limit on how long that may take.
pub(super) enum ConnectionPhase {
    /// A new connection's first request head.
    FirstHead,
    /// The next request, between keep-alive requests. The place in the idle line lives here, so
    /// leaving this phase gives it up, and passes on any wake it received, on every path.
    Idle { turn: IdleTurn },
    /// The rest of a next request's head, which has started arriving.
    NextHead,
    /// A request's handler, or its bodies.
    Request,
    /// The write of a reply after which hyper closes the connection, such as a timeout reply,
    /// bounded by `CLOSE_BUDGET` because the client may not read.
    ClosingReply,
}

impl ConnectionPhase {
    /// The phase a connection enters when its request ends, and when that phase expires. Only a
    /// connection that stays open turns idle: one that closes waits for its reply to be written
    /// outside the idle line, so a waiting client's wake cannot reach it and close it before the
    /// lingering close, and the rest of a rejected body cannot pass for a next request head.
    pub(super) fn after_request(
        end: AfterResponse,
        place: &IdlePlace,
        keep_alive: TimeLimit,
    ) -> (Self, tokio::time::Instant) {
        match end {
            AfterResponse::KeepAlive => (Self::Idle { turn: place.take() }, keep_alive.deadline()),
            AfterResponse::Closes => (
                Self::ClosingReply,
                tokio::time::Instant::now() + CLOSE_BUDGET,
            ),
        }
    }

    /// Resolves when an idle connection reaches the front of the idle line, and never in other
    /// phases.
    pub(super) async fn idle_turn(&mut self) {
        match self {
            Self::Idle { turn } => turn.reached().await,
            Self::FirstHead | Self::NextHead | Self::Request | Self::ClosingReply => {
                std::future::pending().await
            }
        }
    }
}

pub(super) fn warn_connection_timeout(message: &str) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        crate::diagnostics::Event::ConnectionTimeout,
        serde_json::json!({"message": message}),
    );
}

#[cfg(test)]
mod tests;
