//! Replays only the startup-selected read-only fixtures, never a request-supplied path.

use crate::{
    json::{self, JsonError},
    server::{BUCKET_BODY_LIMIT, DeadlineWatch, RequestBody, RequestCtx, json_response},
    storage::admin::AdminError,
};
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Bytes};

pub(super) async fn respond(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Response<Full<Bytes>> {
    reseed(request, ctx)
        .await
        .unwrap_or_else(JsonError::into_response)
}

async fn reseed(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let admin = super::authority(&request, ctx)?;
    if !matches!(*request.method(), Method::POST | Method::PUT) {
        let mut error = JsonError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "methodNotAllowed",
            "Use POST or PUT /_internal/reseed with an empty body",
        );
        error.allow = Some("POST, PUT");
        return Err(error);
    }
    if request.uri().query().is_some() {
        return Err(JsonError::invalid_argument(
            "Reseed does not accept query parameters",
        ));
    }
    if ctx.config.seed_dir.is_none() {
        return Err(JsonError::invalid_argument(
            "Configure seed_dir at startup before reseeding",
        ));
    }
    if ctx.config.sync_dir.is_some() {
        return Err(JsonError::new(
            StatusCode::CONFLICT,
            "conflict",
            "Stop synchronization and restart without sync_dir before reseeding; external files are never reset",
        ));
    }
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    if !bytes.is_empty() {
        return Err(JsonError::invalid_argument(
            "Reseed accepts only an empty body; the source is configured at startup",
        ));
    }
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    let outcome = ctx
        .object(None, deadline)
        .await_catalog(async {
            crate::seed::reseed(&admin, ctx.config, ctx.state.clock.as_ref())
                .await
                .map_err(|error| failure(&error))
        })
        .await?;
    Ok(json_response(&serde_json::to_value(outcome).map_err(
        |_| JsonError::invalid_argument("Cannot encode reseed result"),
    )?))
}

fn failure(error: &std::io::Error) -> JsonError {
    let admin = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<AdminError>());
    match admin {
        Some(AdminError::Precondition) => JsonError::new(
            StatusCode::CONFLICT,
            "conflict",
            "Reseed made no changes because import or synchronization state is active",
        ),
        Some(AdminError::Hold) => JsonError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Reseed made no changes because an object is protected by a hold or retention",
        ),
        Some(AdminError::Invalid(_)) => {
            JsonError::invalid_argument("Invalid fixture state; reseed made no changes")
        }
        _ => JsonError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "backendError",
            "Reseed failed; inspect local fixture/storage diagnostics and retry",
        ),
    }
}
