//! Dispatch from a request to the API family that serves it.
//!
//! The order of the checks in `respond_routed` is part of the wire contract: earlier routes
//! claim overlapping paths, so the routes are tried in a fixed sequence.

use super::{
    DeadlineWatch, RequestCtx, ResponseBody, State, acl, anonymous_error, boxed, buckets,
    notifications, objects, probe_or_unknown, project_resources, resumable, rewrite, unsupported,
    xml_read,
};
use crate::{
    auth::Caller,
    config::{CompatProfile, CorsFallback},
    json::{self, JsonError, ObjectRequest},
    xml,
};
use http_body_util::{BodyExt, Full};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
    header::HeaderValue,
};

/// Wraps CORS headers around the routed response. CORS never authorizes a request.
pub(super) async fn respond(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
) -> Response<ResponseBody> {
    let selected = match xml::select_object(request.uri(), request.headers(), ctx.config) {
        Ok(selected) => selected,
        Err(error) => return boxed(error.response(request.method() == Method::HEAD)),
    };
    if selected.is_none() && json::cors::is_route(request.uri()) {
        return respond_json_cors(request, ctx).await;
    }
    // CORS must also wrap invalid object names and signed queries without authorizing them.
    if selected.as_ref().is_some_and(|object| {
        !object.name().is_empty()
            && json::validate_bucket_name_for_profile(&object.bucket, ctx.config.compat_profile)
                .is_ok()
    }) {
        return respond_xml_cors(request, ctx, selected).await;
    }
    respond_routed(request, ctx, selected).await
}

async fn respond_json_cors(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
) -> Response<ResponseBody> {
    let preflight = json::cors::is_preflight(request.method(), request.headers());
    let mut headers = match json::cors::response_headers(request.headers(), preflight) {
        Ok(headers) => headers,
        Err(error) => return boxed(error.into_response()),
    };
    let mut response = if preflight {
        let status = json::cors::preflight_status(request.headers());
        if status == StatusCode::NOT_FOUND {
            headers.insert(
                "access-control-allow-credentials",
                HeaderValue::from_static("true"),
            );
            boxed(
                JsonError::new(
                    StatusCode::NOT_FOUND,
                    "notFound",
                    "Missing CORS request method",
                )
                .into_response(),
            )
        } else {
            boxed(Response::new(Full::new(Bytes::new())))
        }
    } else {
        respond_routed(request, ctx, None).await
    };
    response.headers_mut().extend(headers);
    response
}

async fn respond_xml_cors(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    selected: Option<xml::ObjectPath>,
) -> Response<ResponseBody> {
    let Some(object) = selected.as_ref() else {
        return respond_routed(request, ctx, selected).await;
    };
    let RequestCtx { state, config, .. } = ctx;
    let has_origin = request.headers().contains_key("origin");
    let preflight = request.method() == Method::OPTIONS && has_origin;
    let cors = if has_origin {
        match state.catalog.get_bucket(&object.bucket).await {
            Ok(Some(bucket)) => {
                if bucket.cors.is_empty() && config.cors_fallback == CorsFallback::None {
                    xml::warn_unconfigured_cors(&object.bucket, request.headers());
                }
                xml::cors(
                    request.headers(),
                    request.method(),
                    &bucket.cors,
                    preflight,
                    config.cors_fallback,
                )
            }
            // A missing bucket has no stored rules, so the fallback lets browsers read the 404.
            Ok(None) => xml::cors(
                request.headers(),
                request.method(),
                &[],
                preflight,
                config.cors_fallback,
            ),
            Err(error) => {
                return boxed(
                    xml::Error::from(JsonError::from(error))
                        .response(request.method() == Method::HEAD),
                );
            }
        }
    } else {
        hyper::HeaderMap::new()
    };
    let mut response = if preflight {
        boxed(Response::new(Full::new(Bytes::new())))
    } else {
        respond_routed(request, ctx, selected).await
    };
    response.headers_mut().extend(cors);
    if !response.headers().contains_key("vary") {
        response.headers_mut().insert(
            "vary",
            HeaderValue::from_static(if preflight {
                "Origin, Access-Control-Request-Method, Access-Control-Request-Headers"
            } else {
                "Origin"
            }),
        );
    }
    response
}

/// Preflight and CORS metadata do not authorize access; actual API dispatch authenticates
/// before operation validation, body reads and authorized catalog operations.
async fn respond_routed(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    selected: Option<xml::ObjectPath>,
) -> Response<ResponseBody> {
    let (method, uri, profile) = (request.method(), request.uri(), ctx.config.compat_profile);
    if selected.is_none()
        && let Some(parsed) = json::acl::route_acl(method, uri, profile)
    {
        return respond_acl(request, ctx.state, parsed).await;
    }
    if selected.is_none()
        && let Some(route) = unsupported::route(method, uri)
    {
        return respond_unsupported(&request, ctx.state, route);
    }
    // POST Object targets the bucket itself and authenticates from form fields in the body.
    if let Some(object) = selected
        .as_ref()
        .filter(|object| method == Method::POST && object.name().is_empty())
    {
        let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
        let result = xml::post::respond(request, object, ctx, deadline).await;
        ctx.finish_object_request();
        return result.unwrap_or_else(|error| boxed(error.response(false)));
    }
    if selected.is_none()
        && let Some(parsed) = json::notifications::route(method, uri, profile)
    {
        return respond_notifications(request, ctx.state, parsed).await;
    }
    if let Some(parsed) = xml::route(method, ctx.config, selected) {
        return respond_xml_object(request, ctx, parsed).await;
    }
    if let Some(parsed) = json::resumable::session_route(method, uri, profile) {
        let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
        let result = match parsed {
            Ok((bucket, id)) => {
                resumable::session_response(request, &bucket, &id, ctx.object(None, deadline)).await
            }
            Err(error) => Err(error),
        };
        ctx.finish_object_request();
        return result.unwrap_or_else(|error| boxed(error.into_response()));
    }
    if let Some(parsed) = json::rewrite::route(method, uri, profile) {
        return respond_rewrite(request, ctx, parsed).await;
    }
    if let Some(parsed) = json::route_object_with_profile(method, uri, profile) {
        return respond_json_object(request, ctx, parsed).await;
    }
    if let Some(parsed) = json::route_project(method, uri) {
        return respond_project(request, ctx.state, parsed).await;
    }
    if let Some(parsed) = json::route_with_profile(method, uri, profile, &ctx.config.seed_project) {
        return respond_bucket(request, ctx.state, profile, parsed).await;
    }
    boxed(probe_or_unknown(&request, ctx.state))
}

fn authenticate<'a>(
    request: &Request<Incoming>,
    state: &'a State,
) -> Result<Option<Caller<'a>>, JsonError> {
    state
        .auth
        .authenticate(request.headers(), request.uri().query())
        .map_err(JsonError::from)
}

async fn respond_acl(
    request: Request<Incoming>,
    state: &State,
    parsed: Result<json::acl::AclRequest, JsonError>,
) -> Response<ResponseBody> {
    let result =
        match authenticate(&request, state).and_then(|caller| parsed.map(|op| (op, caller))) {
            Ok((operation, caller)) => acl::respond(request, operation, caller, state)
                .await
                .map_err(|error| anonymous_error(error, caller)),
            Err(error) => Err(error),
        };
    boxed(result.unwrap_or_else(JsonError::into_response))
}

fn respond_unsupported(
    request: &Request<Incoming>,
    state: &State,
    route: &str,
) -> Response<ResponseBody> {
    boxed(
        authenticate(request, state)
            .and_then(|_| {
                Err(JsonError::new(
                    StatusCode::BAD_REQUEST,
                    "badRequest",
                    format!("gcs-mimic does not support Google JSON API route {route}; remove this API call and use a supported operation (for example, GET /storage/v1/b/BUCKET/o)."),
                ))
            })
            .unwrap_or_else(JsonError::into_response),
    )
}

async fn respond_notifications(
    request: Request<Incoming>,
    state: &State,
    parsed: Result<(json::notifications::NotificationRequest, json::Fields), JsonError>,
) -> Response<ResponseBody> {
    let result =
        match authenticate(&request, state).and_then(|caller| parsed.map(|op| (op, caller))) {
            Ok(((operation, fields), caller)) => {
                notifications::respond(request, operation, fields, caller, state)
                    .await
                    .map_err(|error| anonymous_error(error, caller))
            }
            Err(error) => Err(error),
        };
    boxed(result.unwrap_or_else(JsonError::into_response))
}

async fn respond_xml_object(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<xml::ReadRequest, xml::Error>,
) -> Response<ResponseBody> {
    let head = request.method() == Method::HEAD;
    let put = request.method() == Method::PUT;
    let delete = request.method() == Method::DELETE;
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    let result = match xml::authenticate(
        &ctx.state.auth,
        request.method(),
        request.uri(),
        request.headers(),
        ctx.state.clock.as_ref(),
    ) {
        Ok(caller) => {
            let object_ctx = ctx.object(caller, deadline);
            match parsed {
                Ok(operation) if put => xml::put::respond(request, operation, object_ctx).await,
                Ok(operation) => xml_read::respond(request, operation, object_ctx).await,
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };
    // A successful GET keeps its deadline while the response body streams.
    if head || put || delete || result.is_err() {
        ctx.finish_object_request();
    }
    result.unwrap_or_else(|error| boxed(error.response(head)))
}

async fn respond_rewrite(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<json::rewrite::RewriteRequest, JsonError>,
) -> Response<ResponseBody> {
    let parsed = authenticate(&request, ctx.state).and_then(|caller| parsed.map(|op| (op, caller)));
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    let result = match parsed {
        Ok((operation, caller)) => {
            rewrite::respond(request, operation, ctx.object(caller, deadline))
                .await
                .map_err(|error| anonymous_error(error, caller))
        }
        Err(error) => Err(error),
    };
    ctx.finish_object_request();
    result.unwrap_or_else(|error| boxed(error.into_response()))
}

async fn respond_json_object(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<ObjectRequest, JsonError>,
) -> Response<ResponseBody> {
    let parsed = authenticate(&request, ctx.state).and_then(|caller| parsed.map(|op| (op, caller)));
    let media = matches!(&parsed, Ok((ObjectRequest::Get { params, .. }, _)) if params.media);
    let transfer = media
        || matches!(
            &parsed,
            Ok((
                ObjectRequest::Insert { .. }
                    | ObjectRequest::Copy(_)
                    | ObjectRequest::Compose { .. }
                    | ObjectRequest::Multipart { .. }
                    | ObjectRequest::Resumable { .. },
                _
            ))
        );
    let deadline = ctx.start_object_deadline(if transfer {
        DeadlineWatch::Enforced
    } else {
        DeadlineWatch::CatalogOnly
    });
    let result = match parsed {
        Ok((operation, caller)) => {
            objects::respond(request, operation, ctx.object(caller, deadline))
                .await
                .map_err(|error| anonymous_error(error, caller))
        }
        Err(error) => Err(error),
    };
    // A successful media download keeps its deadline while the response body streams.
    if !media || result.is_err() {
        ctx.finish_object_request();
    }
    result.unwrap_or_else(|error| boxed(error.into_response()))
}

async fn respond_project(
    request: Request<Incoming>,
    state: &State,
    parsed: Result<(json::ProjectRequest, json::Fields), JsonError>,
) -> Response<ResponseBody> {
    let result =
        match authenticate(&request, state).and_then(|caller| parsed.map(|op| (op, caller))) {
            Ok(((operation, fields), caller)) => {
                project_resources::respond(request, operation, fields, caller, state)
                    .await
                    .map_err(|error| anonymous_error(error, caller))
            }
            Err(error) => Err(error),
        };
    boxed(result.unwrap_or_else(JsonError::into_response))
}

async fn respond_bucket(
    request: Request<Incoming>,
    state: &State,
    profile: CompatProfile,
    parsed: Result<json::BucketRequest, JsonError>,
) -> Response<ResponseBody> {
    let result =
        match authenticate(&request, state).and_then(|caller| parsed.map(|op| (op, caller))) {
            Ok((operation, caller)) => buckets::respond(request, operation, caller, state, profile)
                .await
                .map_err(|error| anonymous_error(error, caller)),
            Err(error) => Err(error),
        };
    let response = result
        .map_err(|error| legacy_bucket_error(error, profile))
        .unwrap_or_else(JsonError::into_response);
    if profile == CompatProfile::LegacyEmulator && response.status() != StatusCode::NOT_MODIFIED {
        return boxed(legacy_bucket_body(response).await);
    }
    boxed(response)
}

/// fake-gcs-server reports these bucket errors with its own reasons and messages.
fn legacy_bucket_error(mut error: JsonError, profile: CompatProfile) -> JsonError {
    if profile == CompatProfile::LegacyEmulator
        && error.status == StatusCode::BAD_REQUEST
        && matches!(error.reason, "invalidArgument" | "parseError")
    {
        error.reason = "invalid";
    }
    if profile == CompatProfile::LegacyEmulator
        && error.status == StatusCode::NOT_FOUND
        && error.reason == "notFound"
    {
        error.message = "Not Found".into();
    }
    error
}

/// fake-gcs-server ends bucket bodies with a newline and labels them plain JSON.
async fn legacy_bucket_body(response: Response<Full<Bytes>>) -> Response<Full<Bytes>> {
    let (mut parts, body) = response.into_parts();
    let Ok(collected) = body.collect().await;
    let mut bytes = collected.to_bytes().to_vec();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    parts
        .headers
        .insert("content-type", HeaderValue::from_static("application/json"));
    Response::from_parts(parts, Full::new(Bytes::from(bytes)))
}
