//! Dispatch from a request to the API family that serves it.
//!
//! The order of the checks in `respond_routed` is part of the wire contract: earlier routes
//! claim overlapping paths, so the routes are tried in a fixed sequence.

use super::RequestBody as Incoming;
use super::admin::AdminRoute;
use super::{
    DeadlineWatch, RequestCtx, ResponseBody, acl, anonymous_error, boxed, buckets, empty_response,
    notifications, objects, probe, project_resources, resumable, rewrite, unknown_route,
    unsupported, xml_read,
};
use crate::{
    auth::Caller,
    bucket::{BucketAccess, CatalogError, CorsRule},
    config::{CompatProfile, Config, CorsFallback, CorsFallbackChoice, EndpointSnapshot},
    json::{self, JsonError, ObjectRequest, cors::CorsRequest},
    xml,
};
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Method, Request, Response, StatusCode, body::Bytes, header::HeaderValue};

/// How the server reads one request target. Decided once, before the timeout reply and the
/// router look at it, so the two can never disagree about which API answers.
pub(super) enum Target {
    /// The authority fails XML addressing: a Host the configured domains require is missing, or
    /// disagrees with an absolute-form target. Only the XML API addresses by Host, so it answers.
    Unaddressable(xml::Error),
    /// A management route under `/_internal`; it carries no API envelope.
    Admin(AdminRoute),
    /// `/batch/storage/v1`; each part is classified on its own.
    Batch,
    /// An XML bucket operation without an object path: `GET /`, or `PUT /{name}` with a name
    /// the profile rejects, which the XML API refuses itself.
    XmlBucket,
    /// An XML path that selects a bucket and object name.
    XmlObject(xml::ObjectPath),
    /// Everything else: the JSON API, probes and unknown routes.
    Json,
}

/// Which API's envelope wraps a routed response.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Json,
    Xml,
}

impl Target {
    /// Rewrites the Discovery resumable path in place, then classifies the final target.
    ///
    /// Whether to rewrite is decided from the selection of the original target. A selection
    /// fails only on the Host, the authority and the signed query, never on the path, so an
    /// unaddressable target is answered as such without a rewrite: rewriting would select the
    /// same error, and the error response reads no part of the target.
    pub(super) fn classify<B>(
        request: &mut Request<B>,
        config: &Config,
        endpoints: &EndpointSnapshot,
    ) -> Self {
        let selected = match xml::select_object_with_endpoints(
            request.uri(),
            request.headers(),
            config,
            endpoints,
        ) {
            Ok(selected) => selected,
            Err(error) => return Self::Unaddressable(error),
        };
        let selected = match protocol_upload(request, selected.as_ref()) {
            Some((uri, kind)) => {
                *request.uri_mut() = uri;
                request.extensions_mut().insert(kind);
                // The rewritten path is reserved for the JSON API, so this selects nothing.
                match xml::select_object_with_endpoints(
                    request.uri(),
                    request.headers(),
                    config,
                    endpoints,
                ) {
                    Ok(selected) => selected,
                    Err(error) => return Self::Unaddressable(error),
                }
            }
            None => selected,
        };
        if let Some(object) = selected {
            return Self::XmlObject(object);
        }
        let path = request.uri().path();
        if let Some(route) = AdminRoute::parse(path) {
            Self::Admin(route)
        } else if path == "/batch/storage/v1" {
            Self::Batch
        } else if xml::buckets::matches(request.method(), request.uri(), None) {
            Self::XmlBucket
        } else {
            Self::Json
        }
    }

    /// `None` for management and batch responses, which carry no API envelope.
    fn envelope(&self) -> Option<Family> {
        match self {
            Self::Unaddressable(_) | Self::XmlBucket | Self::XmlObject(_) => Some(Family::Xml),
            Self::Json => Some(Family::Json),
            Self::Admin(_) | Self::Batch => None,
        }
    }

    /// Whether a timeout reply uses the XML error document. Management and batch routes, like
    /// the JSON API, answer JSON.
    pub(super) fn answers_xml(&self) -> bool {
        self.envelope() == Some(Family::Xml)
    }
}

/// Classifies and answers one request, as a batch part is. The connection classifies the
/// request itself before its timeout reply is chosen, and then calls `respond_classified`.
pub(super) async fn respond(
    mut request: Request<Incoming>,
    ctx: RequestCtx<'_>,
) -> Response<ResponseBody> {
    let target = Target::classify(&mut request, ctx.config, ctx.endpoints.snapshot());
    respond_classified(request, target, ctx).await
}

/// The JSON API fields apply only to requests the JSON router claimed: a virtual-host or
/// path-style XML request can share the `/storage/v1/` path prefix with an object name.
pub(super) async fn respond_classified(
    request: Request<Incoming>,
    target: Target,
    ctx: RequestCtx<'_>,
) -> Response<ResponseBody> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let call = json::EnvelopeCall::of_request(&request);
    let family = target.envelope();
    let mut response = route(request, target, ctx).await;
    match family {
        Some(Family::Json) => {
            json::apply_envelope(&method, &path, call, ctx.state.clock.now(), &mut response);
        }
        Some(Family::Xml) => xml::envelope::apply(&method, ctx.state.clock.now(), &mut response),
        None => {}
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
/// `selected` is what the XML API would select at the original target.
fn protocol_upload<B>(
    request: &Request<B>,
    selected: Option<&xml::ObjectPath>,
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
    if selected.is_some_and(|object| {
        object.bucket != "resumable"
            || Some(object.name()) != uri.path().strip_prefix("/resumable/")
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

async fn route(
    request: Request<Incoming>,
    target: Target,
    ctx: RequestCtx<'_>,
) -> Response<ResponseBody> {
    match target {
        Target::Unaddressable(error) => {
            boxed(error.response(xml::ErrorBody::for_method(request.method())))
        }
        Target::Admin(route) => super::admin::respond(route, request, ctx).await,
        Target::Batch => super::batch::respond(request, ctx).await,
        Target::XmlBucket => route_xml(request, ctx, None).await,
        Target::XmlObject(object) => route_xml(request, ctx, Some(object)).await,
        Target::Json => route_json(request, ctx).await,
    }
}

/// Wraps CORS headers around the routed XML response. CORS never authorizes a request.
async fn route_xml(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    selected: Option<xml::ObjectPath>,
) -> Response<ResponseBody> {
    if request.headers().contains_key("x-http-method-override") {
        return boxed(
            xml::Error::from(JsonError::invalid_argument(
                "Method override is supported only by the JSON API",
            ))
            .response(xml::ErrorBody::for_method(request.method())),
        );
    }
    // CORS must also wrap invalid object names and signed queries without authorizing them.
    if selected.as_ref().is_some_and(|object| {
        json::validate_bucket_name_for_profile(&object.bucket, ctx.config.compat_profile).is_ok()
    }) {
        return respond_xml_cors(request, ctx, selected).await;
    }
    respond_routed(request, ctx, selected).await
}

/// Wraps CORS headers around the routed JSON response. CORS never authorizes a request.
async fn route_json(mut request: Request<Incoming>, ctx: RequestCtx<'_>) -> Response<ResponseBody> {
    // Only resumable initiation is served on the Discovery protocol path. Other upload types
    // answer before a method override or upload Content-Encoding is read, as JSON with CORS.
    if is_unsupported_protocol_upload(&request) {
        return respond_json_cors(request, ctx).await;
    }
    if let Err(error) = super::ingress::override_method(&mut request) {
        return boxed(error.into_response());
    }
    let request = match super::ingress::decode_metadata(request, ctx).await {
        Ok(request) => request,
        Err(error) => return boxed(error.into_response()),
    };
    if json::cors::is_route(request.uri()) {
        return respond_json_cors(request, ctx).await;
    }
    respond_routed(request, ctx, None).await
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
    let choice = config.effective_cors_fallback();
    let fallback = choice.fallback();
    let has_origin = request.headers().contains_key("origin");
    let kind = if request.method() == Method::OPTIONS && has_origin {
        CorsRequest::Preflight
    } else {
        CorsRequest::Actual
    };
    // The rules and the existence of the bucket matter only to a request with an Origin.
    // Every other request needs just whether `Vary: Origin` applies: the configured fallback
    // always adds it, so only a fallback-less bucket needs a (light) lookup.
    let lookup = if has_origin {
        origin_bucket_cors(state, &object.bucket).await
    } else {
        BucketCors::WithoutOrigin {
            has_rules: fallback == CorsFallback::None
                && bucket_has_cors(state, &object.bucket).await,
        }
    };
    // The fallback applies only once the bucket is known to have no rules (or not to exist).
    let cors = match &lookup {
        BucketCors::Found(rules) => {
            let cors = xml::cors(request.headers(), request.method(), rules, kind, fallback);
            if rules.is_empty() {
                note_bucket_without_rules(state, &object.bucket, request.headers(), choice, &cors);
            }
            cors
        }
        BucketCors::Missing => xml::cors(request.headers(), request.method(), &[], kind, fallback),
        BucketCors::Failed | BucketCors::WithoutOrigin { .. } => HeaderMap::new(),
    };
    // Cloud Storage sends no `Vary: Origin` for a bucket without CORS configuration, even
    // when the request has an Origin (observed 2026-10-31). Where rules or the local
    // fallback can add CORS fields, the response still varies by Origin when none match;
    // that case was not observed on Cloud Storage.
    let varies_by_origin = lookup.has_rules() || fallback != CorsFallback::None;
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

/// What the CORS wrapper learned about the rules of the bucket an XML request names.
///
/// A failed lookup never answers 503 here: this wrapper runs before authentication and
/// signature checks, so an unauthenticated request must not learn of catalog trouble ahead of
/// the 401/403 it would otherwise get. The routed handler reports a real catalog failure after
/// authorizing.
enum BucketCors {
    /// An Origin-bearing request on an existing bucket, with its rules (possibly none).
    Found(Vec<CorsRule>),
    /// An Origin-bearing request on a bucket that does not exist. It has no stored rules, and
    /// the fallback lets browsers read the 404.
    Missing,
    /// The lookup for an Origin-bearing request failed. Whether the fallback applies is then
    /// unknown, and granting every origin would fail open, so no CORS field is sent: without
    /// rules no CORS access is granted, and the failure fails closed.
    Failed,
    /// A request without Origin needs only whether `Vary: Origin` applies; a failed lookup
    /// counts as no rules.
    WithoutOrigin { has_rules: bool },
}

impl BucketCors {
    /// Whether the bucket is known to have CORS rules.
    fn has_rules(&self) -> bool {
        match self {
            Self::Found(rules) => !rules.is_empty(),
            Self::Missing | Self::Failed => false,
            Self::WithoutOrigin { has_rules } => *has_rules,
        }
    }
}

/// The rules of the bucket an Origin-bearing request names, and whether it exists.
async fn origin_bucket_cors(state: &super::State, bucket: &str) -> BucketCors {
    match state
        .catalog
        .get_bucket(bucket, &BucketAccess::Permissive)
        .await
    {
        Ok(Some(record)) => BucketCors::Found(record.cors),
        Ok(None) => BucketCors::Missing,
        Err(error) => {
            report_cors_lookup_failed(bucket, &error);
            BucketCors::Failed
        }
    }
}

/// Whether a bucket has CORS rules, without reading them; a failed lookup counts as none.
async fn bucket_has_cors(state: &super::State, bucket: &str) -> bool {
    match state.catalog.bucket_has_cors(bucket).await {
        Ok(has_rules) => has_rules,
        Err(error) => {
            report_cors_lookup_failed(bucket, &error);
            false
        }
    }
}

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

/// Logs once per bucket what its lack of rules did to an Origin-bearing request: a warning
/// where no fallback applies, or a note where only the permissive default let it through.
fn note_bucket_without_rules(
    state: &super::State,
    bucket: &str,
    headers: &HeaderMap,
    fallback: CorsFallbackChoice,
    granted: &HeaderMap,
) {
    match fallback {
        CorsFallbackChoice::Explicit(CorsFallback::None)
        | CorsFallbackChoice::Default(CorsFallback::None) => {
            xml::warn_unconfigured_cors(state, bucket, headers);
        }
        CorsFallbackChoice::Default(CorsFallback::AllowAll) if !granted.is_empty() => {
            xml::note_default_cors_fallback(state, bucket);
        }
        CorsFallbackChoice::Default(CorsFallback::AllowAll)
        | CorsFallbackChoice::Explicit(CorsFallback::AllowAll) => {}
    }
}

fn report_cors_lookup_failed(bucket: &str, error: &CatalogError) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "cors_lookup_failed",
        cors_lookup_failure(bucket, error),
    );
}

/// CORS fields replace the routed response's, except `vary`: a representation that also
/// varies by `Accept-Encoding` keeps that field after the CORS `vary`, as Cloud Storage
/// sends both (observed 2026-10-31).
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
        return refuse_authenticated(&request, ctx, json::unsupported_upload_type()).await;
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
        return respond_unsupported(&request, ctx, route).await;
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
            Ok(json::resumable::UploadSession { bucket, upload_id }) => {
                resumable::session_response(
                    request,
                    &bucket,
                    &upload_id,
                    ctx.object(None, deadline),
                )
                .await
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
    boxed(probe(&request, ctx.state).unwrap_or_else(|| unknown_route().into_response()))
}

/// Authenticates the request, then takes its parsed operation. Authentication answers first,
/// so a caller without credentials learns nothing about which requests would parse.
async fn authenticated<'a, T>(
    request: &Request<Incoming>,
    ctx: RequestCtx<'a>,
    parsed: Result<T, JsonError>,
) -> Result<(T, Option<Caller<'a>>), JsonError> {
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
    parsed: Result<T, JsonError>,
    run: F,
) -> Result<R, JsonError>
where
    F: FnOnce(Request<Incoming>, T, Option<Caller<'a>>) -> Fut,
    Fut: Future<Output = Result<R, JsonError>>,
{
    let (operation, caller) = authenticated(&request, ctx, parsed).await?;
    run(request, operation, caller)
        .await
        .map_err(|error| anonymous_error(error, caller))
}

/// Answers `refusal` once the request authenticates.
async fn refuse_authenticated(
    request: &Request<Incoming>,
    ctx: RequestCtx<'_>,
    refusal: JsonError,
) -> Response<ResponseBody> {
    let error = match ctx
        .authenticate(request.headers(), request.uri().query())
        .await
    {
        Ok(_) => refusal,
        Err(error) => error,
    };
    boxed(error.into_response())
}

async fn respond_acl(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<json::acl::AclRequest, JsonError>,
) -> Response<ResponseBody> {
    let (state, base) = (ctx.state, ctx.endpoints.api_base_url());
    let result = dispatch(request, ctx, parsed, |request, operation, caller| {
        acl::respond(request, operation, caller, state, base)
    })
    .await;
    boxed(result.unwrap_or_else(JsonError::into_response))
}

async fn respond_unsupported(
    request: &Request<Incoming>,
    ctx: RequestCtx<'_>,
    route: &str,
) -> Response<ResponseBody> {
    let refusal = JsonError::unsupported_message(format!(
        "gcs-mimic does not support Google JSON API route {route}; remove this API call and use a supported operation (for example, GET /storage/v1/b/BUCKET/o)."
    ));
    refuse_authenticated(request, ctx, refusal).await
}

async fn respond_notifications(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    parsed: Result<(json::notifications::NotificationRequest, json::Fields), JsonError>,
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
    let auth_inputs = ctx.xml_authentication(&request, &resource);
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
            let normalized = xml::extension_headers::normalize(
                &mut request,
                xml::extension_headers::Route::Object,
            );
            match normalized.and(parsed) {
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
            match xml::authenticate_target(ctx.xml_authentication(&request, resource)).await {
                Ok(caller) => match xml::extension_headers::normalize(
                    &mut request,
                    xml::extension_headers::Route::Object,
                ) {
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
    parsed: Result<ObjectRequest, JsonError>,
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
    boxed(result.unwrap_or_else(JsonError::into_response))
}

async fn respond_bucket(
    request: Request<Incoming>,
    ctx: RequestCtx<'_>,
    profile: CompatProfile,
    parsed: Result<json::BucketRequest, JsonError>,
) -> Response<ResponseBody> {
    let (state, base) = (ctx.state, ctx.endpoints.api_base_url());
    let result = dispatch(request, ctx, parsed, |request, operation, caller| {
        buckets::respond(request, operation, caller, state, profile, base)
    })
    .await;
    let response = result
        .map_err(|error| legacy_bucket_error(error, profile))
        .unwrap_or_else(JsonError::into_response);
    if profile == CompatProfile::FakeGcsServer && response.status() != StatusCode::NOT_MODIFIED {
        return boxed(legacy_bucket_body(response).await);
    }
    boxed(response)
}

/// fake-gcs-server reports these bucket errors with its own reasons and messages.
fn legacy_bucket_error(mut error: JsonError, profile: CompatProfile) -> JsonError {
    if profile == CompatProfile::FakeGcsServer
        && error.status == StatusCode::BAD_REQUEST
        && matches!(
            error.kind,
            json::ErrorKind::InvalidArgument | json::ErrorKind::LegacyParseError
        )
    {
        error.reason = "invalid";
    }
    if profile == CompatProfile::FakeGcsServer && error.is_missing_bucket() {
        error.message = "Not Found".into();
    }
    error
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VirtualHostDomains;

    fn classify(
        config: &Config,
        method: Method,
        target: &str,
        host: &str,
    ) -> (Target, Request<()>) {
        let endpoints = EndpointSnapshot::explicit(
            "http://localhost:4443",
            config.public_base_url.clone(),
            config.xml_virtual_host_domains.clone(),
        );
        let mut request = Request::builder()
            .method(method)
            .uri(target)
            .header("host", host)
            .body(())
            .unwrap();
        let target = Target::classify(&mut request, config, &endpoints);
        (target, request)
    }

    #[test]
    fn discovery_resumable_path_is_rewritten_and_answered_by_the_json_api() {
        let (target, request) = classify(
            &Config::default(),
            Method::POST,
            "/resumable/upload/storage/v1/b/bucket/o?uploadType=resumable",
            "localhost:4443",
        );
        assert!(matches!(target, Target::Json));
        assert_eq!(
            request.uri(),
            "/upload/storage/v1/b/bucket/o?uploadType=resumable"
        );
        assert!(request.extensions().get::<ProtocolUpload>() == Some(&ProtocolUpload::Resumable));
    }

    #[test]
    fn a_bucket_named_resumable_keeps_its_xml_object_path() {
        // Without `uploadType` the path names an object in the bucket `resumable`.
        let (target, request) = classify(
            &Config::default(),
            Method::POST,
            "/resumable/upload/storage/v1/b/bucket/o",
            "localhost:4443",
        );
        let Target::XmlObject(object) = target else {
            panic!("expected an XML object path");
        };
        assert_eq!(object.bucket, "resumable");
        assert_eq!(object.name(), "upload/storage/v1/b/bucket/o");
        assert_eq!(request.uri(), "/resumable/upload/storage/v1/b/bucket/o");
        assert!(request.extensions().get::<ProtocolUpload>().is_none());
    }

    #[test]
    fn targets_without_an_object_path_are_classified_by_route() {
        let config = Config::default();
        let host = "localhost:4443";
        let (root, _) = classify(&config, Method::GET, "/", host);
        assert!(matches!(root, Target::XmlBucket));
        let (admin, _) = classify(&config, Method::POST, "/_internal/reset", host);
        assert!(matches!(admin, Target::Admin(AdminRoute::Reset)));
        let (batch, _) = classify(&config, Method::POST, "/batch/storage/v1", host);
        assert!(matches!(batch, Target::Batch));
        let (json, _) = classify(&config, Method::GET, "/storage/v1/b/bucket/o", host);
        assert!(matches!(json, Target::Json));
        assert!(root.envelope() == Some(Family::Xml));
        assert!(json.envelope() == Some(Family::Json));
        assert!(admin.envelope().is_none() && batch.envelope().is_none());
        // A timeout reply follows the API that serves the request.
        assert!(root.answers_xml());
        assert!(!json.answers_xml() && !admin.answers_xml() && !batch.answers_xml());
    }

    #[test]
    fn an_authority_that_disagrees_with_host_is_unaddressable_before_any_rewrite() {
        let config = Config {
            xml_virtual_host_domains: VirtualHostDomains::of(&["storage.example.test"]),
            ..Config::default()
        };
        let target = "http://bucket.storage.example.test/resumable/upload/storage/v1/b/b/o?uploadType=resumable";
        let (classified, request) = classify(&config, Method::POST, target, "other.example.test");
        let Target::Unaddressable(error) = classified else {
            panic!("expected an unaddressable target");
        };
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(request.uri(), target);
        assert!(request.extensions().get::<ProtocolUpload>().is_none());
    }

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
        let leaky = CatalogError::Io(std::io::Error::other("secret catalog filename").into());
        let fields = cors_lookup_failure("bucket", &leaky);
        assert!(!fields["cause"].as_str().unwrap().contains("secret"));
    }
}
