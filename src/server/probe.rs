//! The liveness and readiness probes. They answer from the process's own state, so a busy catalog
//! cannot delay them.

use crate::service::{
    body::{RequestBody, has_body},
    state::State,
};
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Bytes};
use std::time::UNIX_EPOCH;

/// What a probe path asks: whether the process answers, or whether the API is ready.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Probe {
    Liveness,
    /// `/_internal/ready` and its compatible alias `/_internal/healthcheck`.
    Readiness,
}

impl Probe {
    fn parse(path: &str) -> Option<Self> {
        match path {
            "/_internal/health" => Some(Self::Liveness),
            "/_internal/ready" | "/_internal/healthcheck" => Some(Self::Readiness),
            _ => None,
        }
    }
}

pub(super) fn probe(
    request: &Request<RequestBody>,
    state: &State,
) -> Option<Response<Full<Bytes>>> {
    let probe = Probe::parse(request.uri().path())?;
    let (status, body) = if request.method() != Method::GET && request.method() != Method::HEAD {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            "{\"error\":\"method not allowed\"}".to_owned(),
        )
    } else if has_body(request.headers()) {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "{\"error\":\"probe bodies are not supported\"}".to_owned(),
        )
    } else if probe == Probe::Readiness && !state.is_ready() {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "{\"status\":\"not ready\"}".to_owned(),
        )
    } else {
        let now = state
            .clock
            .now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        (
            StatusCode::OK,
            format!("{{\"status\":\"ok\",\"unix_time\":{now}}}"),
        )
    };
    let content_length = hyper::header::HeaderValue::from(body.len());
    let mut response = Response::new(Full::new(if request.method() == Method::HEAD {
        Bytes::new()
    } else {
        Bytes::from(body)
    }));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/json"),
    );
    headers.insert("content-length", content_length);
    if status == StatusCode::METHOD_NOT_ALLOWED {
        headers.insert(
            "allow",
            hyper::header::HeaderValue::from_static("GET, HEAD"),
        );
    }
    Some(response)
}
