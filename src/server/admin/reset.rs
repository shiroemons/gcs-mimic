//! Explicit instance reset never follows or modifies an external source tree.

use crate::json::JsonErrorResponse;
use crate::{
    json::ApiError,
    server::{DeadlineWatch, RESOURCE_BODY_LIMIT, RequestBody, RequestCtx, json_response},
    storage::admin::AdminError,
};
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Bytes};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    confirm: String,
    #[serde(default)]
    state: State,
}

/// The state a reset leaves the instance in.
#[derive(Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum State {
    /// No buckets, objects or settings.
    #[default]
    Empty,
    /// What startup created: the buckets document's buckets, then the seed tree, published in
    /// the transaction that clears the rest, so a test can return to its fixtures without
    /// restarting and no reader sees them missing.
    Startup,
}

pub(super) enum Form {
    Confirmed,
    SourceAlias,
}

pub(super) async fn respond(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
    authorized: super::Authorized,
    form: Form,
) -> Response<Full<Bytes>> {
    super::retry_soon(
        reset(request, ctx, authorized, form)
            .await
            .unwrap_or_else(ApiError::into_response),
    )
}

async fn reset(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
    authorized: super::Authorized,
    form: Form,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let admin = authorized.into_admin();
    if request.method() != Method::POST {
        return Err(ApiError::method_not_allowed(
            "Use POST /_internal/reset with an explicit confirmation",
            Some("POST"),
        ));
    }
    if request.uri().query().is_some() {
        return Err(ApiError::invalid_argument(
            "Reset does not accept query parameters",
        ));
    }
    let bytes = crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    let confirmation: Confirmation = if matches!(form, Form::SourceAlias) {
        if !bytes.is_empty() {
            return Err(ApiError::invalid_argument(
                "Source delete_all accepts only an empty body",
            ));
        }
        Confirmation {
            confirm: "gcs-mimic".into(),
            state: State::Empty,
        }
    } else {
        serde_json::from_slice(&bytes).map_err(|_| {
            ApiError::invalid_argument(
                "Reset deletes this local instance's state; send {\"confirm\":\"gcs-mimic\"}, optionally with \"state\":\"empty\" or \"startup\"",
            )
        })?
    };
    if confirmation.confirm != "gcs-mimic" {
        return Err(ApiError::invalid_argument(
            "Reset requires confirm set to gcs-mimic",
        ));
    }
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    match confirmation.state {
        State::Empty => clear(ctx, &admin, deadline).await,
        State::Startup => restore(ctx, admin, deadline).await,
    }
}

async fn clear(
    ctx: RequestCtx<'_>,
    admin: &crate::storage::admin::Admin,
    deadline: tokio::time::Instant,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let ctx = ctx.object(None, deadline);
    // Expiry before exclusive ownership can still confirm that reset made no changes.
    let wait_deadline = deadline.min(ctx.config.catalog_wait_timeout().deadline());
    let result = ctx
        .await_session_writer(admin.reset_until(wait_deadline))
        .await
        .map_err(|error| failure(&error))?;
    Ok(json_response(&super::encode(&result)?))
}

/// Publishes the startup state in the reset's transaction. The fixture tree may take as long as
/// its size needs, as a reseed's does; only waits for other work end early.
async fn restore(
    ctx: RequestCtx<'_>,
    admin: crate::storage::admin::Admin,
    deadline: tokio::time::Instant,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let (config, clock) = (ctx.config, ctx.state.clock.clone());
    let initialization = crate::bucket_init::restoration(&ctx.state.catalog, config, clock.clone())
        .await
        .map_err(|diagnostics| {
            // Startup applied the same bytes, so only a changed project policy or clock can
            // refuse them; another attempt fails alike until that changes.
            ApiError::invalid(format!(
                "Reset made no changes because the startup buckets document no longer applies: {diagnostics}"
            ))
        })?;
    let ctx = ctx.object(None, deadline);
    let outcome = ctx
        .await_requested_work(async {
            crate::seed::restore(&admin, config, &clock, deadline, initialization)
                .await
                .map_err(|error| {
                    super::reseed::tree_failure(&error, super::reseed::Operation::Reset)
                })
        })
        .await?;
    Ok(json_response(&super::encode(&outcome)?))
}

pub(super) fn failure(error: &AdminError) -> ApiError {
    match error {
        AdminError::Busy { .. } => ApiError::service_unavailable(
            "Reset made no changes because catalog work or an unpublished body is active; retry after it completes",
        ),
        AdminError::Precondition => ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "Reset made no changes because an import is incomplete; complete or repair the import first",
        ),
        // Reset can only say that it changed nothing or that it cannot confirm what it changed.
        AdminError::DiskFull(_)
        | AdminError::Closed
        | AdminError::Hold
        | AdminError::SeedProjectMismatch
        | AdminError::SeedMetadataBudget { .. }
        | AdminError::SourceChanged { .. }
        | AdminError::Corrupt(_)
        | AdminError::Io(_)
        | AdminError::NotFound
        | AdminError::Invalid(_)
        | AdminError::Checksum
        | AdminError::TooLarge
        | AdminError::Forbidden
        | AdminError::BodyAborted(_)
        | AdminError::Backend(_) => ApiError::service_unavailable(
            "Reset could not confirm completion; inspect local storage and instance state before retrying",
        ),
    }
}
