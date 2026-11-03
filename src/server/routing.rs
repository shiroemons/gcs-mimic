//! Dispatch from a request to the API family that serves it.
//!
//! The order of the checks in `respond_routed` is part of the wire contract: earlier routes
//! claim overlapping paths, so the routes are tried in a fixed sequence.

use super::RequestBody as Incoming;
use super::{
    DeadlineWatch, RequestCtx, ResponseBody, State, acl, anonymous_error, boxed, buckets,
    empty_response, notifications, objects, probe_or_unknown, project_resources, resumable,
    rewrite, unsupported, xml_read,
};
use crate::{
    auth::Caller,
    bucket::{BucketAccess, CatalogError},
    config::{CompatProfile, CorsFallback},
    json::{self, JsonError, ObjectRequest, cors::CorsRequest},
    xml,
};
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Method, Request, Response, StatusCode, body::Bytes, header::HeaderValue};

/// Which API family claimed a request, known only once the router has chosen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Undecided,
    Json,
    Xml,
}

/// The JSON API fields apply only to requests the JSON router claimed: a virtual-host or
/// path-style XML request can share the `/storage/v1/` path prefix with an object name.
pub(super) async fn respond(
    mut request: Request<Incoming>,
    ctx: RequestCtx<'_>,
) -> Response<ResponseBody> {
    if let Some((uri, kind)) = resumable_protocol_path(&request, ctx) {
        *request.uri_mut() = uri;
        request.extensions_mut().insert(kind);
    }
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let call = json::EnvelopeCall::of_request(&request);
    let mut family = Family::Undecided;
    let mut response = route(request, ctx, &mut family).await;
    match family {
        Family::Json => {
            json::apply_envelope(&method, &path, call, ctx.state.clock.now(), &mut response);
        }
        Family::Xml => xml::envelope::apply(&method, ctx.state.clock.now(), &mut response),
        Family::Undecided => {}
    }
    response
}

/// What a request on the Discovery resumable protocol path asks for, kept as a request
/// extension once the path is rewritten.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProtocolUpload {
    /// `uploadType=resumable` initiation, served as on `/upload/storage/v1`.
    Resumable,
    /// Any other upload type, or a session request, which only `/upload/storage/v1` serves.
    Other,
}

/// `storage.objects.insert` defines its resumable protocol at
/// `POST /resumable/upload/storage/v1/b/{bucket}/o`, and gcloud storage (apitools) initiates
/// resumable uploads there. Path-style, that path would otherwise name the XML object
/// `upload/storage/v1/b/{bucket}/o` in a bucket called `resumable`, so only a path-style POST of
/// exactly that shape with an `uploadType` query parameter, which XML requests never send, is
/// taken; it is rewritten to `/upload/storage/v1/b/{bucket}/o` with the same query.
fn resumable_protocol_path(
    request: &Request<Incoming>,
    ctx: RequestCtx<'_>,
) -> Option<(hyper::Uri, ProtocolUpload)> {
    const PREFIX: &str = "/resumable/upload/storage/v1/b/";
    let uri = request.uri();
    if request.method() != Method::POST {
        return None;
    }
    let bucket = uri.path().strip_prefix(PREFIX)?.strip_suffix("/o")?;
    if bucket.is_empty() || bucket.contains('/') {
        return None;
    }
    // A virtual-hosted request names an object; only the path-style reading is the JSON path.
    let selected =
        xml::select_object_with_endpoints(uri, request.headers(), ctx.config, ctx.endpoints)
            .ok()
            .flatten();
    if selected.is_some_and(|object| {
        object.bucket != "resumable" || object.name() != &uri.path()["/resumable/".len()..]
    }) {
        return None;
    }
    let query = json::parse_query(uri.query().unwrap_or_default()).ok()?;
    let kind = if query.get("uploadType")? == "resumable" && !query.contains_key("upload_id") {
        ProtocolUpload::Resumable
    } else {
        ProtocolUpload::Other
    };
    let mut parts = uri.clone().into_parts();
    let path_and_query = match uri.query() {
        Some(query) => format!("/upload/storage/v1/b/{bucket}/o?{query}"),
        None => format!("/upload/storage/v1/b/{bucket}/o"),
    };
    parts.path_and_query = Some(path_and_query.parse().ok()?);
    Some((hyper::Uri::from_parts(parts).ok()?, kind))
}

fn is_unsupported_protocol_upload(request: &Request<Incoming>) -> bool {
    request.extensions().get::<ProtocolUpload>() == Some(&ProtocolUpload::Other)
}

/// Wraps CORS headers around the routed response. CORS never authorizes a request.
async fn route(
    mut request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    family: &mut Family,
) -> Response<ResponseBody> {
    let selected = match xml::select_object_with_endpoints(
        request.uri(),
        request.headers(),
        ctx.config,
        ctx.endpoints,
    ) {
        Ok(selected) => selected,
        Err(error) => {
            *family = Family::Xml;
            return boxed(error.response(xml::ErrorBody::for_method(request.method())));
        }
    };
    if selected.is_none()
        && (matches!(
            request.uri().path(),
            "/_internal/config"
                | "/_internal/objects"
                | "/_internal/reset"
                | "/_internal/reseed"
                | "/_internal/delete_all"
        ) || request.uri().path() == "/_internal/sync"
            || request.uri().path().starts_with("/_internal/sync/"))
    {
        return super::admin::respond(request, ctx).await;
    }
    if selected.is_none() && request.uri().path() == "/batch/storage/v1" {
        return super::batch::respond(request, ctx).await;
    }
    let xml_bucket = xml::buckets::matches(request.method(), request.uri(), selected.as_ref());
    *family = if selected.is_none() && !xml_bucket {
        Family::Json
    } else {
        Family::Xml
    };
    // Only resumable initiation is served on the Discovery protocol path. Other upload types
    // answer before a method override or upload Content-Encoding is read, as JSON with CORS.
    if is_unsupported_protocol_upload(&request) {
        *family = Family::Json;
        return respond_json_cors(request, ctx).await;
    }
    if (selected.is_some() || xml_bucket)
        && request.headers().contains_key("x-http-method-override")
    {
        return boxed(
            xml::Error::from(JsonError::invalid_argument(
                "Method override is supported only by the JSON API",
            ))
            .response(xml::ErrorBody::for_method(request.method())),
        );
    }
    if let Err(error) = super::ingress::override_method(&mut request) {
        return boxed(error.into_response());
    }
    let request = match if selected.is_none() && !xml_bucket {
        super::ingress::decode_metadata(request, ctx).await
    } else {
        Ok(request)
    } {
        Ok(request) => request,
        Err(error) => return boxed(error.into_response()),
    };
    if selected.is_none() && json::cors::is_route(request.uri()) {
        return respond_json_cors(request, ctx).await;
    }
    // CORS must also wrap invalid object names and signed queries without authorizing them.
    if selected.as_ref().is_some_and(|object| {
        json::validate_bucket_name_for_profile(&object.bucket, ctx.config.compat_profile).is_ok()
    }) {
        return respond_xml_cors(request, ctx, selected).await;
    }
    respond_routed(request, ctx, selected).await
}

async fn respond_json_cors(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
) -> Response<ResponseBody> {
    let kind = json::cors::request_kind(request.method(), request.headers());
    let mut headers = match json::cors::response_headers(request.headers(), kind) {
        Ok(headers) => headers,
        Err(error) => return boxed(error.into_response()),
    };
    let mut response = if kind == CorsRequest::Preflight {
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
            boxed(empty_response(StatusCode::OK))
        }
    } else {
        respond_routed(request, ctx, None).await
    };
    apply_cors(response.headers_mut(), headers, None);
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
    let kind = if request.method() == Method::OPTIONS && has_origin {
        CorsRequest::Preflight
    } else {
        CorsRequest::Actual
    };
    // The rules and the existence of the bucket matter only to a request with an Origin.
    // Every other request needs just whether `Vary: Origin` applies: the configured fallback
    // always adds it, so only a fallback-less bucket needs a (light) lookup.
    //
    // A failed lookup counts as "no rules" instead of answering 503 here: this wrapper runs
    // before authentication and signature checks, so an unauthenticated request must not
    // learn of catalog trouble ahead of the 401/403 it would otherwise get. The routed
    // handler reports a real catalog failure after authorizing. Without rules no CORS access
    // is granted, so the failure fails closed. A missing bucket has no stored rules either;
    // the fallback lets browsers read the 404.
    let mut lookup_failed = false;
    let (rules, found, has_rules) = if has_origin {
        match state
            .catalog
            .get_bucket(&object.bucket, &BucketAccess::Permissive)
            .await
        {
            Ok(Some(bucket)) => {
                let has_rules = !bucket.cors.is_empty();
                (bucket.cors, true, has_rules)
            }
            Ok(None) => (Vec::new(), false, false),
            Err(error) => {
                report_cors_lookup_failed(&object.bucket, &error);
                lookup_failed = true;
                (Vec::new(), false, false)
            }
        }
    } else if config.cors_fallback != CorsFallback::None {
        (Vec::new(), false, false)
    } else {
        let has_rules = match state.catalog.bucket_has_cors(&object.bucket).await {
            Ok(has_rules) => has_rules,
            Err(error) => {
                report_cors_lookup_failed(&object.bucket, &error);
                false
            }
        };
        (Vec::new(), false, has_rules)
    };
    // The fallback applies only once the bucket is known to have no rules (or not to exist).
    // After a failed lookup that is unknown, and granting every origin would fail open, so no
    // CORS field is sent at all.
    let cors = if has_origin && !lookup_failed {
        if found && rules.is_empty() && config.cors_fallback == CorsFallback::None {
            xml::warn_unconfigured_cors(state, &object.bucket, request.headers());
        }
        xml::cors(
            request.headers(),
            request.method(),
            &rules,
            kind,
            config.cors_fallback,
        )
    } else {
        hyper::HeaderMap::new()
    };
    // Cloud Storage sends no `Vary: Origin` for a bucket without CORS configuration, even
    // when the request has an Origin (observed 2026-10-31). Where rules or the local
    // fallback can add CORS fields, the response still varies by Origin when none match;
    // that case was not observed on Cloud Storage.
    let varies_by_origin = has_rules || config.cors_fallback != CorsFallback::None;
    let (mut response, vary) = match kind {
        CorsRequest::Preflight => (
            boxed(empty_response(StatusCode::OK)),
            "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
        ),
        CorsRequest::Actual => (respond_routed(request, ctx, selected).await, "Origin"),
    };
    apply_cors(
        response.headers_mut(),
        cors,
        varies_by_origin.then_some(vary),
    );
    response
}

/// CORS fields replace the routed response's, except `vary`: a representation that also
/// varies by `Accept-Encoding` keeps that field after the CORS `vary`, as Cloud Storage
/// sends both (observed 2026-10-31).
/// The fields of a failed CORS rule lookup. The cause passes through the same allow-list as
/// client-visible backend errors, because catalog messages may embed SQL values.
fn cors_lookup_failure(bucket: &str, error: &CatalogError) -> serde_json::Value {
    let cause = match error {
        CatalogError::Unavailable(cause) => json::safe_backend_cause(cause.message()).to_owned(),
        CatalogError::Io(cause) => format!("catalog I/O: {cause}"),
        CatalogError::Corrupt(_) => "catalog corrupt".to_owned(),
        other => json::safe_backend_cause(&other.to_string()).to_owned(),
    };
    serde_json::json!({"bucket": bucket, "cause": cause})
}

fn report_cors_lookup_failed(bucket: &str, error: &CatalogError) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "cors_lookup_failed",
        cors_lookup_failure(bucket, error),
    );
}

fn apply_cors(headers: &mut HeaderMap, cors: HeaderMap, default_vary: Option<&'static str>) {
    let (leading, representation): (Vec<HeaderValue>, Vec<HeaderValue>) = headers
        .get_all("vary")
        .iter()
        .cloned()
        .partition(|value| value == super::ALLOWED_RESOURCES_VARY);
    headers.remove("vary");
    headers.extend(cors);
    if let Some(vary) = default_vary
        && !headers.contains_key("vary")
    {
        headers.insert("vary", HeaderValue::from_static(vary));
    }
    // Cloud Storage lists the allowed-resources field ahead of the CORS fields.
    let cors_vary: Vec<HeaderValue> = headers.get_all("vary").iter().cloned().collect();
    headers.remove("vary");
    for value in leading.into_iter().chain(cors_vary) {
        headers.append("vary", value);
    }
    for value in representation {
        headers.append("vary", value);
    }
}

/// Preflight and CORS metadata do not authorize access; actual API dispatch authenticates
/// before operation validation, body reads and authorized catalog operations.
async fn respond_routed(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    selected: Option<xml::ObjectPath>,
) -> Response<ResponseBody> {
    if is_unsupported_protocol_upload(&request) {
        return boxed(
            authenticate(&request, ctx.state)
                .and_then(|_| Err(json::unsupported_upload_type()))
                .unwrap_or_else(JsonError::into_response),
        );
    }
    let (method, uri, profile) = (request.method(), request.uri(), ctx.config.compat_profile);
    if xml::buckets::matches(method, uri, selected.as_ref()) {
        return xml::buckets::respond(request, selected.as_ref(), ctx).await;
    }
    if selected.is_none()
        && let Some(parsed) = json::acl::route_acl(method, uri, profile)
    {
        return respond_acl(request, ctx, parsed).await;
    }
    if selected.is_none()
        && let Some(route) = unsupported::route(method, uri)
    {
        return respond_unsupported(&request, ctx.state, route);
    }
    // POST Object targets the bucket itself and authenticates from form fields in the body.
    if let Some(object) = selected.as_ref().filter(|object| {
        method == Method::POST && object.name().is_empty() && !xml::multipart::matches(uri)
    }) {
        let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
        let result = xml::post::respond(request, object, ctx, deadline).await;
        ctx.finish_object_request();
        return result.unwrap_or_else(|error| boxed(error.response(xml::ErrorBody::Include)));
    }
    if selected.is_none()
        && let Some(parsed) = json::notifications::route(method, uri, profile)
    {
        return respond_notifications(request, ctx, parsed).await;
    }
    let signature_resource = selected.as_ref().map_or(
        crate::auth::signature::Resource::PathStyle,
        xml::ObjectPath::signature_resource,
    );
    if let Some(object) = selected.as_ref()
        && let Some(route) = xml::resumable::route(method, uri, request.headers())
    {
        return respond_xml_resumable(request, ctx, object, route, &signature_resource).await;
    }
    if let Some(parsed) = xml::route(method, ctx.config, selected, request.headers()) {
        return respond_xml_object(request, ctx, parsed, signature_resource).await;
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
        return respond_project(request, ctx, parsed).await;
    }
    if let Some(parsed) = json::route_with_profile(method, uri, profile, &ctx.config.seed_project) {
        return respond_bucket(request, ctx, profile, parsed).await;
    }
    boxed(probe_or_unknown(&request, ctx.state))
}

fn authenticate<'a>(
    request: &Request<Incoming>,
    state: &'a State,
) -> Result<Option<Caller<'a>>, JsonError> {
    state
        .auth
        .authenticate_at(
            request.headers(),
            request.uri().query(),
            state.clock.as_ref(),
        )
        .map_err(JsonError::from)
}

async fn respond_acl(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<json::acl::AclRequest, JsonError>,
) -> Response<ResponseBody> {
    let state = ctx.state;
    let result =
        match authenticate(&request, state).and_then(|caller| parsed.map(|op| (op, caller))) {
            Ok((operation, caller)) => acl::respond(
                request,
                operation,
                caller,
                state,
                &ctx.endpoints.api_base_url,
            )
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
                ).with_kind(json::ErrorKind::Unsupported))
            })
            .unwrap_or_else(JsonError::into_response),
    )
}

async fn respond_notifications(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<(json::notifications::NotificationRequest, json::Fields), JsonError>,
) -> Response<ResponseBody> {
    let state = ctx.state;
    let result =
        match authenticate(&request, state).and_then(|caller| parsed.map(|op| (op, caller))) {
            Ok(((operation, fields), caller)) => notifications::respond(
                request,
                operation,
                fields,
                caller,
                state,
                &ctx.endpoints.api_base_url,
            )
            .await
            .map_err(|error| anonymous_error(error, caller)),
            Err(error) => Err(error),
        };
    boxed(result.unwrap_or_else(JsonError::into_response))
}

async fn respond_xml_object(
    mut request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<xml::ReadRequest, xml::Error>,
    resource: crate::auth::signature::Resource,
) -> Response<ResponseBody> {
    let head = request.method() == Method::HEAD;
    let error_body = xml::ErrorBody::for_method(request.method());
    let put = request.method() == Method::PUT;
    let delete = request.method() == Method::DELETE;
    let multipart = xml::multipart::matches(request.uri());
    // A read's or delete's missing-object error names the requested path; a PUT can miss a
    // different object, such as a copy source, and names that one itself.
    let target = parsed
        .as_ref()
        .ok()
        .filter(|_| head || delete || request.method() == Method::GET)
        .map(|read| (read.target.bucket.clone(), read.target.name.clone()));
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    let auth_inputs = xml::Authentication {
        auth: &ctx.state.auth,
        catalog: &ctx.state.catalog,
        method: request.method(),
        uri: request.uri(),
        headers: request.headers(),
        clock: ctx.state.clock.as_ref(),
        resource: &resource,
    };
    let authentication = if put && !multipart {
        xml::authenticate_upload_target(auth_inputs).await
    } else {
        xml::authenticate_target(auth_inputs)
            .await
            .map(|caller| (caller, None))
    };
    let result = match authentication {
        Ok((caller, streaming)) => {
            let object_ctx = ctx.object(caller, deadline);
            match xml::normalize_legacy_headers(request.headers_mut()).and(parsed) {
                Ok(operation) if multipart => {
                    xml::multipart::respond(request, operation, object_ctx).await
                }
                Ok(operation) if put => {
                    xml::put::respond(request, operation, object_ctx, streaming).await
                }
                Ok(operation) => xml_read::respond(request, operation, object_ctx).await,
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };
    // A successful GET keeps its deadline while the response body streams.
    if head || put || delete || multipart || result.is_err() {
        ctx.finish_object_request();
    }
    result.unwrap_or_else(|error| {
        let error = match &target {
            Some((bucket, name)) => error.naming_object(bucket, name),
            None => error,
        };
        boxed(error.response(error_body))
    })
}

/// Initiation authenticates like any XML request; the session URI is its own capability.
async fn respond_xml_resumable(
    mut request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    object: &xml::ObjectPath,
    route: xml::resumable::Route,
    resource: &crate::auth::signature::Resource,
) -> Response<ResponseBody> {
    let error_body = xml::ErrorBody::for_method(request.method());
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    let result = match route {
        xml::resumable::Route::Session => {
            xml::resumable::session(request, object, ctx.object(None, deadline)).await
        }
        xml::resumable::Route::Start => {
            match xml::authenticate_target(xml::Authentication {
                auth: &ctx.state.auth,
                catalog: &ctx.state.catalog,
                method: request.method(),
                uri: request.uri(),
                headers: request.headers(),
                clock: ctx.state.clock.as_ref(),
                resource,
            })
            .await
            {
                Ok(caller) => match xml::normalize_legacy_headers(request.headers_mut()) {
                    Ok(()) => {
                        xml::resumable::start(request, object, ctx.object(caller, deadline)).await
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            }
        }
    };
    ctx.finish_object_request();
    result.unwrap_or_else(|error| boxed(error.response(error_body)))
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
    // The object a missing-object error names: only operations whose path selects one.
    let target = match &parsed {
        Ok((
            ObjectRequest::Get { target, .. }
            | ObjectRequest::Update(json::ObjectUpdateParams { target, .. })
            | ObjectRequest::Delete { target, .. },
            _,
        )) => Some((target.bucket.clone(), target.name.clone())),
        _ => None,
    };
    let transfer = media
        || matches!(
            &parsed,
            Ok((
                ObjectRequest::Insert(_)
                    | ObjectRequest::Copy(_)
                    | ObjectRequest::Compose { .. }
                    | ObjectRequest::Multipart(_)
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
    result.unwrap_or_else(|error| {
        let error = match &target {
            Some((bucket, name)) => error.naming_object(bucket, name),
            None => error,
        };
        boxed(if media {
            error.into_media_response()
        } else {
            error.into_response()
        })
    })
}

async fn respond_project(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<(json::ProjectRequest, json::Fields), JsonError>,
) -> Response<ResponseBody> {
    let state = ctx.state;
    let result =
        match authenticate(&request, state).and_then(|caller| parsed.map(|op| (op, caller))) {
            Ok(((operation, fields), caller)) => project_resources::respond(
                request,
                operation,
                fields,
                caller,
                state,
                &ctx.endpoints.api_base_url,
            )
            .await
            .map_err(|error| anonymous_error(error, caller)),
            Err(error) => Err(error),
        };
    boxed(result.unwrap_or_else(JsonError::into_response))
}

async fn respond_bucket(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    profile: CompatProfile,
    parsed: Result<json::BucketRequest, JsonError>,
) -> Response<ResponseBody> {
    let state = ctx.state;
    let result =
        match authenticate(&request, state).and_then(|caller| parsed.map(|op| (op, caller))) {
            Ok((operation, caller)) => buckets::respond(
                request,
                operation,
                caller,
                state,
                profile,
                &ctx.endpoints.api_base_url,
            )
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
        && matches!(
            error.kind,
            json::ErrorKind::InvalidArgument | json::ErrorKind::ParseError
        )
    {
        error.reason = "invalid";
    }
    if profile == CompatProfile::LegacyEmulator && error.is_missing_bucket() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cors_lookup_failure_withholds_backend_details_outside_the_allow_list() {
        let leaky = CatalogError::Unavailable("UNIQUE failed: buckets.name = 'secret-name'".into());
        let fields = cors_lookup_failure("bucket", &leaky);
        assert_eq!(fields["bucket"], "bucket");
        assert_eq!(
            fields["cause"],
            "unrecognized backend failure (private details withheld)"
        );
        let known = CatalogError::Unavailable("database is locked".into());
        assert_eq!(
            cors_lookup_failure("bucket", &known)["cause"],
            "database is locked"
        );
    }
}
