//! The JSON API handlers: the dispatch from a parsed JSON route to its operation, and the
//! request checks and resource responses those operations share. `server::routing` decides
//! that the JSON API answers a request and parses its route; one submodule serves each family
//! of operations.

mod acl;
mod buckets;
mod notifications;
mod objects;
mod project_resources;
pub(crate) mod resumable;
mod rewrite;
pub(crate) mod route;
mod unsupported;

#[cfg(test)]
mod tests;

use crate::json::JsonErrorResponse;
use crate::service::{
    body::{RequestBody, RequestBody as Incoming, ResponseBody, boxed, empty_response, has_body},
    context::{DeadlineWatch, RequestCtx},
};
use crate::{
    auth::{self, Caller},
    bounded_body::RESOURCE_BODY_LIMIT,
    http_conditions::{EtagError, etag_not_modified},
    json::{self, ApiError, ObjectRequest},
    profile::CompatProfile,
};
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Bytes, header::HeaderValue};
use route::JsonRoute;

/// Dispatches a request to the operation of the JSON API `route` that claimed it.
pub(crate) async fn respond(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    route: JsonRoute,
) -> Response<ResponseBody> {
    match route {
        JsonRoute::Acl(parsed) => respond_acl(request, ctx, parsed).await,
        JsonRoute::Unsupported(route) => respond_unsupported(&request, ctx, route).await,
        JsonRoute::Notifications(parsed) => respond_notifications(request, ctx, parsed).await,
        JsonRoute::Session(parsed) => respond_session(request, ctx, parsed).await,
        JsonRoute::Rewrite(parsed) => respond_rewrite(request, ctx, parsed).await,
        JsonRoute::Object(parsed) => Box::pin(respond_json_object(request, ctx, parsed)).await,
        JsonRoute::Project(parsed) => respond_project(request, ctx, parsed).await,
        JsonRoute::Bucket(parsed) => {
            respond_bucket(request, ctx, ctx.config.compat_profile(), parsed).await
        }
    }
}

/// The session URI is its own capability, so the request carries no caller.
async fn respond_session(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<json::resumable::UploadSession, ApiError>,
) -> Response<ResponseBody> {
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    let result = match parsed {
        Ok(json::resumable::UploadSession { bucket, upload_id }) => {
            resumable::session_response(
                request,
                bucket.as_str(),
                &upload_id,
                ctx.object(None, deadline),
            )
            .await
        }
        Err(error) => Err(error),
    };
    ctx.finish_object_request();
    result.unwrap_or_else(|error| boxed(error.into_response()))
}

/// Authenticates the request, then takes its parsed operation. Authentication answers first,
/// so a caller without credentials learns nothing about which requests would parse.
async fn authenticated<'a, T>(
    request: &Request<Incoming>,
    ctx: RequestCtx<'a>,
    parsed: Result<T, ApiError>,
) -> Result<(T, Option<Caller<'a>>), ApiError> {
    let caller = ctx
        .authenticate(request.headers(), request.uri().query())
        .await?;
    Ok((parsed?, caller))
}

/// Runs an authenticated operation, reporting a refusal to an anonymous caller as Cloud
/// Storage does.
async fn dispatch<'a, T, R, F, Fut>(
    request: Request<Incoming>,
    ctx: RequestCtx<'a>,
    parsed: Result<T, ApiError>,
    run: F,
) -> Result<R, ApiError>
where
    F: FnOnce(Request<Incoming>, T, Option<Caller<'a>>) -> Fut,
    Fut: Future<Output = Result<R, ApiError>>,
{
    let (operation, caller) = authenticated(&request, ctx, parsed).await?;
    run(request, operation, caller)
        .await
        .map_err(|error| anonymous_error(error, caller))
}

/// Answers `refusal` once the request authenticates.
pub(crate) async fn refuse_authenticated(
    request: &Request<Incoming>,
    ctx: RequestCtx<'_>,
    refusal: ApiError,
) -> Response<ResponseBody> {
    let error = match ctx
        .authenticate(request.headers(), request.uri().query())
        .await
    {
        Ok(_) => refusal,
        Err(error) => ApiError::from(error),
    };
    boxed(error.into_response())
}

async fn respond_acl(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<json::acl::AclRequest, ApiError>,
) -> Response<ResponseBody> {
    let (state, base) = (ctx.state, ctx.endpoints.api_base_url());
    let result = dispatch(request, ctx, parsed, |request, operation, caller| {
        acl::respond(request, operation, caller, state, base)
    })
    .await;
    boxed(result.unwrap_or_else(ApiError::into_response))
}

async fn respond_unsupported(
    request: &Request<Incoming>,
    ctx: RequestCtx<'_>,
    route: &str,
) -> Response<ResponseBody> {
    let refusal = ApiError::unsupported_message(format!(
        "gcs-mimic does not support Google JSON API route {route}; remove this API call and use a supported operation (for example, GET /storage/v1/b/BUCKET/o)."
    ));
    refuse_authenticated(request, ctx, refusal).await
}

async fn respond_notifications(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<(json::notifications::NotificationRequest, json::Fields), ApiError>,
) -> Response<ResponseBody> {
    let (state, base) = (ctx.state, ctx.endpoints.api_base_url());
    let result = dispatch(
        request,
        ctx,
        parsed,
        |request, (operation, fields), caller| {
            notifications::respond(request, operation, fields, caller, state, base)
        },
    )
    .await;
    boxed(result.unwrap_or_else(ApiError::into_response))
}

async fn respond_rewrite(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<json::rewrite::RewriteRequest, ApiError>,
) -> Response<ResponseBody> {
    let parsed = authenticated(&request, ctx, parsed).await;
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
    parsed: Result<ObjectRequest, ApiError>,
) -> Response<ResponseBody> {
    let parsed = authenticated(&request, ctx, parsed).await;
    let operation = parsed.as_ref().ok().map(|(operation, _)| operation);
    let media = operation.is_some_and(ObjectRequest::is_media_read);
    let target = operation
        .and_then(ObjectRequest::named_object)
        .map(|target| (target.bucket.clone(), target.name.clone()));
    let transfer = operation.is_some_and(ObjectRequest::transfers_data);
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
            Some((bucket, name)) => error.naming_object(bucket.as_str(), name),
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
    parsed: Result<(json::ProjectRequest, json::Fields), ApiError>,
) -> Response<ResponseBody> {
    let (state, base) = (ctx.state, ctx.endpoints.api_base_url());
    let result = dispatch(
        request,
        ctx,
        parsed,
        |request, (operation, fields), caller| {
            project_resources::respond(request, operation, fields, caller, state, base)
        },
    )
    .await;
    boxed(result.unwrap_or_else(ApiError::into_response))
}

async fn respond_bucket(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    profile: CompatProfile,
    parsed: Result<json::BucketRequest, ApiError>,
) -> Response<ResponseBody> {
    let result = dispatch(request, ctx, parsed, |request, operation, caller| {
        buckets::respond(request, operation, caller, ctx)
    })
    .await;
    boxed(bucket_answer(result, profile).await)
}

/// What a bucket route answers for `result`, in fake-gcs-server's reasons and body layout under
/// its profile. The error goldens call it directly, since the route itself needs a request and a
/// catalog.
pub(crate) async fn bucket_answer(
    result: Result<Response<Full<Bytes>>, ApiError>,
    profile: CompatProfile,
) -> Response<Full<Bytes>> {
    let response = result
        .map_err(|error| {
            if profile == CompatProfile::FakeGcsServer {
                error.for_legacy_bucket()
            } else {
                error
            }
        })
        .unwrap_or_else(ApiError::into_response);
    if profile == CompatProfile::FakeGcsServer && response.status() != StatusCode::NOT_MODIFIED {
        return legacy_bucket_body(response).await;
    }
    response
}

/// fake-gcs-server ends bucket bodies with a newline and labels them plain JSON.
async fn legacy_bucket_body(response: Response<Full<Bytes>>) -> Response<Full<Bytes>> {
    let (mut parts, body) = response.into_parts();
    let Ok(collected) = body.collect().await;
    let bytes = collected.to_bytes();
    let mut bytes = bytes.try_into_mut().unwrap_or_else(bytes::BytesMut::from);
    if !bytes.ends_with(b"\n") {
        bytes.extend_from_slice(b"\n");
    }
    parts
        .headers
        .insert("content-type", HeaderValue::from_static("application/json"));
    Response::from_parts(parts, Full::new(bytes.freeze()))
}

/// Refuses a declared body on a JSON method that takes none, read as [`has_body`] reads it.
/// Cloud Storage accepted `Content-Length: 00` on a bucket read, an object read and an object
/// delete (recorded 2026-11-15).
fn require_empty_body(headers: &hyper::HeaderMap) -> Result<(), ApiError> {
    if has_body(headers) {
        return Err(ApiError::invalid_argument(
            "This method requires an empty body",
        ));
    }
    Ok(())
}

/// Moves the links of an object resource stored by an earlier request to this request's origin.
/// The stored links name the Host that finished the work, and a retry can arrive through another
/// address: a browser finishes on a published port, a peer asks through the service name. An
/// explicit base left them correct, so the stored response stays exactly as it was.
fn rebase_stored_links(
    endpoints: &crate::config::RequestEndpoints,
    resource: &mut serde_json::Value,
) {
    match endpoints.api_base_source() {
        crate::config::ApiBaseSource::Explicit => {}
        crate::config::ApiBaseSource::Derived => {
            json::rebase_object_links(resource, endpoints.api_base_url());
        }
    }
}

pub(crate) fn anonymous_error(error: ApiError, caller: Option<Caller<'_>>) -> ApiError {
    if matches!(caller, Some(Caller::Anonymous)) && error.status() == StatusCode::FORBIDDEN {
        error.for_anonymous_caller()
    } else {
        error
    }
}

/// Authorize a project-scoped collection request such as buckets.insert or buckets.list.
fn authorize_project(
    caller: Option<Caller<'_>>,
    permission: auth::Permission,
    project: &str,
) -> Result<(), ApiError> {
    match caller {
        Some(caller) => Ok(caller.authorize(permission, Some(project))?),
        None => Ok(()),
    }
}

/// Whether a POST that takes no body also accepts an empty JSON object as one.
#[derive(Clone, Copy)]
pub(crate) enum EmptyJsonBody {
    /// An official client sends `{}` here: google-cloud-cpp 3.13.0 does on objects.restore.
    Accepted,
    /// No pinned client sends one here, so it stays an unknown body.
    Refused,
}

/// The declared type of a body sent to a POST that takes none.
#[derive(Clone, Copy)]
enum BodylessPostType {
    Form,
    Json,
    Other,
}

async fn validate_bodyless_post(
    request: Request<RequestBody>,
    expected: &std::collections::BTreeMap<String, String>,
    empty_json: EmptyJsonBody,
) -> Result<(), ApiError> {
    // Ruby's generated client repeats URL query parameters in form-encoded POST bodies, and the
    // C++ client sends an empty JSON object to restore. Neither carries a value the URL query
    // lacks; any other body could, so it is still refused.
    let declared = match request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
    {
        Some("application/x-www-form-urlencoded") => BodylessPostType::Form,
        Some("application/json") => BodylessPostType::Json,
        _ => BodylessPostType::Other,
    };
    let body = crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    let accepted = body.is_empty()
        || match declared {
            BodylessPostType::Form => {
                std::str::from_utf8(&body)
                    .ok()
                    .and_then(|body| crate::percent::parse_query(body).ok())
                    .as_ref()
                    == Some(expected)
            }
            BodylessPostType::Json => match empty_json {
                EmptyJsonBody::Accepted => {
                    serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&body)
                        .is_ok_and(|members| members.is_empty())
                }
                EmptyJsonBody::Refused => false,
            },
            BodylessPostType::Other => false,
        };
    if !accepted {
        let example = expected
            .first_key_value()
            .map_or_default(|(key, value)| format!("{key}={value}"));
        let allowed = match empty_json {
            EmptyJsonBody::Accepted => "an empty body, an empty JSON object,",
            EmptyJsonBody::Refused => "an empty body,",
        };
        return Err(ApiError::invalid_argument(format!(
            "Use {allowed} or form parameters identical to the URL query, for example {example}"
        )));
    }
    Ok(())
}

/// Rejects headers that do not apply to a derived write (copy, compose or rewrite), naming the
/// first of `names` the request carries.
fn reject_headers(headers: &hyper::HeaderMap, names: &[&str]) -> Result<(), ApiError> {
    match names.iter().find(|name| headers.contains_key(**name)) {
        Some(name) => Err(ApiError::unsupported_object(name)),
        None => Ok(()),
    }
}

/// An `If-Match` or `If-None-Match` header that does not parse as entity tags.
fn invalid_etag_header() -> ApiError {
    ApiError::invalid_parameter("Invalid ETag precondition header")
}

fn not_modified_response(
    etag: Option<&hyper::header::HeaderValue>,
    cache_control: Option<&hyper::header::HeaderValue>,
) -> Response<Full<Bytes>> {
    let mut response = empty_response(StatusCode::NOT_MODIFIED);
    let headers = response.headers_mut();
    if let Some(etag) = etag {
        headers.insert("etag", etag.clone());
    }
    if let Some(cache_control) = cache_control {
        headers.insert("cache-control", cache_control.clone());
    }
    response
}

/// Whether a metadata read's query preconditions (`ifGenerationNotMatch`,
/// `ifMetagenerationNotMatch`) already select a `304 Not Modified` response.
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryPrecondition {
    NotModified,
    Unmatched,
}

/// Varied on first by a publicly readable JSON media response, ahead of the CORS fields.
pub(crate) const ALLOWED_RESOURCES_VARY: &str = "X-Goog-Allowed-Resources";

/// The `Cache-Control` of IAM policy reads, whose provider value has not been recorded.
const IAM_POLICY_CACHE_CONTROL: &str = "no-cache";

/// What a metadata read that evaluates `If-Match` and `If-None-Match` returns, which sets its
/// `Cache-Control` and the form of a failed `If-Match`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValidatedRead {
    Object,
    Bucket,
    Policy,
    /// A bucket, default object or object ACL listing, validated by its owner's entity tag.
    AclList,
}

impl ValidatedRead {
    fn cache_control(self) -> &'static str {
        match self {
            Self::Object | Self::Bucket | Self::AclList => json::RESOURCE_CACHE_CONTROL,
            Self::Policy => IAM_POLICY_CACHE_CONTROL,
        }
    }

    /// Whether a failed `If-Match` names the header and carries the ETag, as Cloud Storage
    /// answered an object metadata read (case obj-download-validators) and the ACL listings
    /// (case bkt-probe-collection-validators). No failing bucket or policy read was recorded.
    fn names_failed_if_match(self) -> bool {
        match self {
            Self::Object | Self::AclList => true,
            Self::Bucket | Self::Policy => false,
        }
    }
}

fn read_metadata_response(
    headers: &hyper::HeaderMap,
    etag: &str,
    mut value: serde_json::Value,
    fields: &json::Fields,
    query: QueryPrecondition,
    read: ValidatedRead,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let not_modified = match etag_not_modified(headers, etag) {
        Ok(not_modified) => not_modified || query == QueryPrecondition::NotModified,
        Err(EtagError::Invalid) => return Err(invalid_etag_header()),
        Err(EtagError::PreconditionFailed) => {
            // The header form of the failure carries a shorter message than the query form.
            let error = ApiError::new(
                StatusCode::PRECONDITION_FAILED,
                "conditionNotMet",
                "Precondition Failed",
            );
            if !read.names_failed_if_match() {
                return Err(error);
            }
            let mut response = error.at_if_match().into_response();
            if let Ok(etag) = etag.parse() {
                response.headers_mut().insert("etag", etag);
            }
            return Ok(response);
        }
    };
    // ETags are rendered locally from base64 (JSON) or a quoted policy revision, so the
    // conversion always succeeds; omitting the validator is safer than panicking if that ever
    // changes.
    let etag = hyper::header::HeaderValue::from_str(etag).ok();
    let mut response = if not_modified {
        empty_response(StatusCode::NOT_MODIFIED)
    } else {
        fields.apply(&mut value);
        json_response(&value)
    };
    let response_headers = response.headers_mut();
    if let Some(etag) = etag {
        response_headers.insert("etag", etag);
    }
    response_headers.insert(
        "cache-control",
        hyper::header::HeaderValue::from_static(read.cache_control()),
    );
    Ok(response)
}

/// The `fields` selection a response applies, read from the request query as the operation
/// says.
fn response_fields(
    uri: &crate::percent::RequestUri,
    selection: json::FieldSelection<'_>,
) -> Result<json::Fields, ApiError> {
    match selection {
        json::FieldSelection::Ignored => Ok(json::Fields::whole()),
        json::FieldSelection::Query(schema) => json::Fields::from_query(uri, schema),
        json::FieldSelection::Parsed(fields) => Ok(fields.clone()),
    }
}

/// A `200 OK` JSON API resource with the cache headers its operation records.
fn json_response_cached(
    value: &serde_json::Value,
    cache: json::ResponseCache,
) -> Response<Full<Bytes>> {
    let mut response = json_response(value);
    let headers = response.headers_mut();
    match cache {
        json::ResponseCache::Unrecorded => {}
        json::ResponseCache::Resource => {
            headers.insert(
                "cache-control",
                hyper::header::HeaderValue::from_static(json::RESOURCE_CACHE_CONTROL),
            );
        }
        json::ResponseCache::NoStore => {
            headers.insert(
                "cache-control",
                hyper::header::HeaderValue::from_static(
                    crate::object::media_headers::NO_STORE_CACHE_CONTROL,
                ),
            );
            headers.insert(
                "pragma",
                hyper::header::HeaderValue::from_static("no-cache"),
            );
        }
    }
    response
}

/// A resource response narrowed by `fields`. The `ETag` header still names the whole resource,
/// as Cloud Storage sends it with a field-selected body.
fn json_response_selected(
    value: &mut serde_json::Value,
    fields: &json::Fields,
    cache: json::ResponseCache,
) -> Response<Full<Bytes>> {
    let etag = json::etag_header(value);
    fields.apply(value);
    let mut response = json_response_cached(value, cache);
    if let Some(etag) = etag {
        response.headers_mut().insert("etag", etag);
    }
    response
}

/// A `200 OK` JSON API resource.
pub(crate) fn json_response(value: &serde_json::Value) -> Response<Full<Bytes>> {
    json_response_body(value, json::render_body(value))
}

/// `body` must be `value` rendered in the current request's JSON format.
fn json_response_body(value: &serde_json::Value, body: String) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    response.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/json; charset=UTF-8"),
    );
    if let Some(etag) = json::etag_header(value) {
        response.headers_mut().insert("etag", etag);
    }
    response
}
