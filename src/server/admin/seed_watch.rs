//! The report of the passes that replay the startup-selected seed directory while the server
//! runs, and a wait for the next pass. Neither route changes the catalog or reads a
//! request-supplied path.

use crate::json::JsonErrorResponse;
use crate::{
    api_error::ApiError,
    seed::{DEFAULT_WAIT, MAX_WAIT},
    server::{RESOURCE_BODY_LIMIT, RequestBody, RequestCtx, json_response},
    storage::admin::seed_watch::WaitError,
};
use http_body_util::Full;
use hyper::{Method, Request, Response, body::Bytes};
use std::time::Duration;

/// `GET /_internal/seed`: whether passes run, how often, and what the last one found.
pub(super) fn status(
    request: &Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Response<Full<Bytes>>, ApiError> {
    if request.method() != Method::GET {
        return Err(ApiError::method_not_allowed(
            "Use GET /_internal/seed",
            Some("GET"),
        ));
    }
    if request.uri().query().is_some() {
        return Err(ApiError::invalid_argument(
            "The seed status does not accept query parameters",
        ));
    }
    Ok(json_response(&super::encode(
        &ctx.state.catalog.seed_watch().status(),
    )?))
}

/// `POST /_internal/seed/wait`: starts a pass now and answers with the report of the first pass
/// that started after the request and left no removed file or directory waiting for its second
/// proof, so that every change made to the tree before the request is applied or reported.
pub(super) async fn respond_wait(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Response<Full<Bytes>> {
    super::retry_soon(
        wait(request, ctx)
            .await
            .unwrap_or_else(ApiError::into_response),
    )
}

async fn wait(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Response<Full<Bytes>>, ApiError> {
    if request.method() != Method::POST {
        return Err(ApiError::method_not_allowed(
            "Use POST /_internal/seed/wait with an empty body",
            Some("POST"),
        ));
    }
    let timeout = wait_timeout(request.uri().query())?;
    let bytes = crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    if !bytes.is_empty() {
        return Err(ApiError::invalid_argument(
            "The seed wait accepts only an empty body",
        ));
    }
    let until = tokio::time::Instant::now() + timeout;
    match ctx.state.catalog.seed_watch().wait(until).await {
        Ok(report) => Ok(json_response(&super::encode(&report)?)),
        Err(WaitError::NotConfigured) => Err(ApiError::invalid(
            "No pass replays seed_dir while the server runs; configure seed_dir and a positive seed_watch_interval_ms at startup",
        )),
        Err(WaitError::TimedOut) => Err(ApiError::service_unavailable(format!(
            "No pass over seed_dir finished within {} ms without a removed file or directory waiting for its second proof; retry, or wait longer with timeoutMs",
            timeout.as_millis()
        ))),
        Err(WaitError::Closed) => Err(ApiError::service_unavailable(
            "The server is closing, and no further pass over seed_dir runs",
        )),
    }
}

/// The wait the query names in `timeoutMs`, its only parameter.
fn wait_timeout(query: Option<&str>) -> Result<Duration, ApiError> {
    let mut query = crate::percent::parse_query(query.unwrap_or_default())?;
    let timeout = match query.remove("timeoutMs") {
        None => DEFAULT_WAIT,
        Some(value) => value
            .parse::<u64>()
            .ok()
            .map(Duration::from_millis)
            .filter(|timeout| (Duration::from_millis(1)..=MAX_WAIT).contains(timeout))
            .ok_or_else(|| {
                ApiError::invalid_argument(format!(
                    "timeoutMs must be an integer in 1..={}",
                    MAX_WAIT.as_millis()
                ))
            })?,
    };
    if let Some(name) = query.keys().next() {
        return Err(ApiError::invalid_argument(format!(
            "The seed wait accepts only the timeoutMs query parameter, not {name}"
        )));
    }
    Ok(timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_takes_only_a_bounded_timeout() {
        let millis = |query| wait_timeout(query).map(|timeout| timeout.as_millis());
        assert_eq!(millis(None).unwrap(), 60_000);
        assert_eq!(millis(Some("timeoutMs=1")).unwrap(), 1);
        assert_eq!(millis(Some("timeoutMs=600000")).unwrap(), 600_000);
        for query in [
            "timeoutMs=0",
            "timeoutMs=600001",
            "timeoutMs=-1",
            "timeoutMs=soon",
            "timeoutMs=1&timeoutMs=2",
            "timeout=1",
        ] {
            assert!(millis(Some(query)).is_err(), "{query}");
        }
    }
}
