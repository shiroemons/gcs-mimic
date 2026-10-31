//! Explicit instance reset never follows or modifies an external source/sync tree.

use crate::{
    json::{self, JsonError},
    server::{BUCKET_BODY_LIMIT, DeadlineWatch, RequestBody, RequestCtx, json_response},
    storage::admin::AdminError,
};
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Bytes};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    confirm: String,
}

pub(super) async fn respond(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Response<Full<Bytes>> {
    let mut response = reset(request, ctx)
        .await
        .unwrap_or_else(JsonError::into_response);
    if response.status() == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert("retry-after", hyper::header::HeaderValue::from_static("1"));
    }
    response
}

async fn reset(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let admin = super::authority(&request, ctx)?;
    if request.method() != Method::POST {
        let mut error = JsonError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "methodNotAllowed",
            "Use POST /_internal/reset with an explicit confirmation",
        );
        error.allow = Some("POST");
        return Err(error);
    }
    if request.uri().query().is_some() {
        return Err(JsonError::invalid_argument(
            "Reset does not accept query parameters",
        ));
    }
    if ctx.config.sync_dir.is_some() {
        return Err(JsonError::new(
            StatusCode::CONFLICT,
            "conflict",
            "Stop directory synchronization and restart without sync_dir before resetting; external files are never reset",
        ));
    }
    let source_alias = request.uri().path() == "/_internal/delete_all";
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    let confirmation: Confirmation = if source_alias {
        if !bytes.is_empty() {
            return Err(JsonError::invalid_argument(
                "Source delete_all accepts only an empty body",
            ));
        }
        Confirmation {
            confirm: "gcs-mimic".into(),
        }
    } else {
        serde_json::from_slice(&bytes).map_err(|_| {
        JsonError::invalid_argument(
            "Reset deletes this local instance's state; send exactly {\"confirm\":\"gcs-mimic\"}",
        )
    })?
    };
    if confirmation.confirm != "gcs-mimic" {
        return Err(JsonError::invalid_argument(
            "Reset requires confirm set to gcs-mimic",
        ));
    }
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    let result = ctx.object(None, deadline).await_catalog(admin.reset()).await.map_err(|error| match error {
        AdminError::Busy { .. } => JsonError::new(StatusCode::SERVICE_UNAVAILABLE, "backendError", "Reset made no changes because catalog work or an unpublished body is active; retry after it completes"),
        AdminError::Precondition => JsonError::new(StatusCode::CONFLICT, "conflict", "Reset made no changes because an import is incomplete; complete or repair the import first"),
        _ => JsonError::new(StatusCode::SERVICE_UNAVAILABLE, "backendError", "Reset could not confirm completion; inspect local storage and instance state before retrying"),
    })?;
    Ok(json_response(&serde_json::to_value(result).map_err(
        |_| JsonError::invalid_argument("Cannot encode reset result"),
    )?))
}
