//! Opt-in local management; these routes never confer Google API permissions.

use super::{
    BUCKET_BODY_LIMIT, RequestBody, RequestCtx, ResponseBody, authorize_project, boxed,
    empty_response,
};
use crate::{
    auth,
    config::EndpointError,
    json::{self, JsonError},
};
use hyper::{Method, Request, StatusCode};
use serde::Deserialize;

mod fixtures;
mod reseed;
mod reset;
mod sync;

fn authority(
    request: &Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<crate::storage::admin::Admin, JsonError> {
    let admin = ctx.state.catalog.local_admin().ok_or_else(|| {
        JsonError::new(
            StatusCode::NOT_FOUND,
            "notFound",
            "Local administration is disabled at startup",
        )
    })?;
    if let Some(caller) = ctx.state.auth.authenticate_at(
        request.headers(),
        request.uri().query(),
        ctx.state.clock.as_ref(),
    )? {
        let principal = caller.principal()?;
        if ctx.config.admin_principal.as_deref() != Some(principal.id()) {
            return Err(auth::AuthFailure::Forbidden.into());
        }
    }
    Ok(admin)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Update {
    external_url: Option<String>,
    public_host: Option<String>,
}

pub(super) async fn respond(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> hyper::Response<ResponseBody> {
    if request.uri().path() == "/_internal/sync"
        || request.uri().path().starts_with("/_internal/sync/")
    {
        return boxed(sync::respond(request, ctx).await);
    }
    if matches!(
        request.uri().path(),
        "/_internal/reset" | "/_internal/delete_all"
    ) {
        return boxed(reset::respond(request, ctx).await);
    }
    if request.uri().path() == "/_internal/reseed" {
        return boxed(reseed::respond(request, ctx).await);
    }
    if request.uri().path() == "/_internal/objects" {
        return boxed(
            fixtures::insert(request, ctx)
                .await
                .unwrap_or_else(JsonError::into_response),
        );
    }
    boxed(
        update(request, ctx)
            .await
            .unwrap_or_else(JsonError::into_response),
    )
}

async fn update(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<hyper::Response<http_body_util::Full<hyper::body::Bytes>>, JsonError> {
    if !ctx.config.admin_enabled {
        return Err(JsonError::new(
            StatusCode::NOT_FOUND,
            "notFound",
            "Local management is disabled; enable admin_enabled only on an isolated local listener",
        ));
    }
    let caller = ctx.state.auth.authenticate_at(
        request.headers(),
        request.uri().query(),
        ctx.state.clock.as_ref(),
    )?;
    authorize_project(caller, auth::BUCKETS_UPDATE, &ctx.config.seed_project)?;
    if request.method() != Method::PUT {
        let mut response = JsonError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "methodNotAllowed",
            "Use PUT /_internal/config",
        )
        .into_response();
        response
            .headers_mut()
            .insert("allow", hyper::header::HeaderValue::from_static("PUT"));
        return Ok(response);
    }
    if request.uri().query().is_some() {
        return Err(JsonError::invalid_argument(
            "Local configuration does not accept query parameters",
        ));
    }
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    let update: Update = serde_json::from_slice(&bytes).map_err(|_| {
        JsonError::invalid_argument(
            "Send one JSON object with string externalUrl and publicHost fields",
        )
    })?;
    ctx.config
        .runtime_endpoints
        .update(
            ctx.config,
            &ctx.state.api_base_url,
            update.external_url.as_deref(),
            update.public_host.as_deref(),
        )
        .map_err(JsonError::from)?;
    Ok(empty_response(StatusCode::OK))
}

impl From<EndpointError> for JsonError {
    fn from(error: EndpointError) -> Self {
        let (status, reason) = if matches!(error, EndpointError::Poisoned) {
            (StatusCode::SERVICE_UNAVAILABLE, "backendError")
        } else {
            (StatusCode::BAD_REQUEST, "invalidArgument")
        };
        Self::new(status, reason, error.to_string())
    }
}
