//! XML objects. Shared authorization, generation selection and bytes live in storage.

use crate::{
    config::{Config, EndpointSnapshot},
    json::{self, JsonError},
    object::{ObjectConditions, ObjectRecord},
};
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Method, Response, StatusCode, Uri,
    body::Bytes,
    header::{HeaderName, HeaderValue},
};
use std::time::UNIX_EPOCH;

pub(crate) mod buckets;
mod copy;
mod cors;
mod document;
pub(crate) mod list;
pub(crate) mod multipart;
pub(crate) use cors::{cors, warn_unconfigured_cors};
pub(crate) mod post;
pub(crate) mod put;

#[cfg(test)]
pub(crate) async fn authenticate<'a>(
    auth: &'a crate::auth::Auth,
    catalog: &crate::storage::Catalog,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    clock: &dyn crate::clock::Clock,
) -> Result<Option<crate::auth::Caller<'a>>, Error> {
    authenticate_target(
        auth,
        catalog,
        method,
        uri,
        headers,
        clock,
        &crate::auth::signature::Resource::PathStyle,
    )
    .await
}

pub(crate) async fn authenticate_target<'a>(
    auth: &'a crate::auth::Auth,
    catalog: &crate::storage::Catalog,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    clock: &dyn crate::clock::Clock,
    resource: &crate::auth::signature::Resource,
) -> Result<Option<crate::auth::Caller<'a>>, Error> {
    if crate::auth::signature::is_signed(uri.query()) || crate::auth::signature::has_header(headers)
    {
        auth.authenticate_xml_resource(catalog, method, uri, headers, clock, resource)
            .await
            .map_err(|error| {
                if crate::auth::signature::has_header(headers)
                    && !headers.get("authorization").is_some_and(|value| {
                        value.as_bytes().starts_with(b"GOOG4")
                            || value.as_bytes().starts_with(b"AWS4")
                    })
                    && matches!(
                        error,
                        crate::auth::signature::VerificationError::Signature(
                            crate::auth::signature::Failure::Expired
                                | crate::auth::signature::Failure::NotYetValid
                        )
                    )
                {
                    Error::new(
                        StatusCode::FORBIDDEN,
                        "RequestTimeTooSkewed",
                        "The legacy signature timestamp is outside the allowed time window.",
                    )
                } else {
                    signature_error(error)
                }
            })
    } else {
        auth.authenticate_at(headers, uri.query(), clock)
            .map_err(|e| Error::from(JsonError::from(e)))
    }
}

fn signature_error(failure: crate::auth::signature::VerificationError) -> Error {
    use crate::auth::signature::{Failure, VerificationError};
    let failure = match failure {
        VerificationError::Signature(failure) => failure,
        VerificationError::Catalog(error) => return Error::from(JsonError::from(error)),
        VerificationError::Worker(_) => {
            return Error::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "ServiceUnavailable",
                "The signature verifier is unavailable.",
            );
        }
    };
    let (status, code, message) = match failure {
        Failure::Malformed => (
            StatusCode::BAD_REQUEST,
            "InvalidAuthentication",
            "Invalid V2/V4 signing parameters; use exactly one signing version (for example, GoogleAccessId, Expires=1793090000 and Signature for V2).",
        ),
        Failure::UnknownSigner => (
            StatusCode::FORBIDDEN,
            "InvalidAccessKeyId",
            "The signing key or its registered identity is unknown or inactive.",
        ),
        Failure::Mismatch => (
            StatusCode::FORBIDDEN,
            "SignatureDoesNotMatch",
            "The request signature does not match.",
        ),
        Failure::Expired => (
            StatusCode::BAD_REQUEST,
            "ExpiredToken",
            "The request signature has expired.",
        ),
        Failure::NotYetValid => (
            StatusCode::FORBIDDEN,
            "RequestTimeTooSkewed",
            "The request signature is not yet usable.",
        ),
    };
    Error::new(status, code, message)
}

#[cfg(test)]
pub(crate) async fn authenticate_upload<'a>(
    auth: &'a crate::auth::Auth,
    catalog: &crate::storage::Catalog,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    clock: &dyn crate::clock::Clock,
) -> Result<
    (
        Option<crate::auth::Caller<'a>>,
        Option<crate::auth::signature::Streaming>,
    ),
    Error,
> {
    authenticate_upload_target(
        auth,
        catalog,
        method,
        uri,
        headers,
        clock,
        &crate::auth::signature::Resource::PathStyle,
    )
    .await
}

pub(crate) async fn authenticate_upload_target<'a>(
    auth: &'a crate::auth::Auth,
    catalog: &crate::storage::Catalog,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    clock: &dyn crate::clock::Clock,
    resource: &crate::auth::signature::Resource,
) -> Result<
    (
        Option<crate::auth::Caller<'a>>,
        Option<crate::auth::signature::Streaming>,
    ),
    Error,
> {
    if headers
        .get("x-amz-content-sha256")
        .is_some_and(|value| value == crate::auth::signature::STREAMING_PAYLOAD)
    {
        let (caller, streaming) = auth
            .authenticate_streaming(catalog, method, uri, headers, clock)
            .await
            .map_err(signature_error)?;
        Ok((Some(caller), Some(streaming)))
    } else {
        authenticate_target(auth, catalog, method, uri, headers, clock, resource)
            .await
            .map(|caller| (caller, None))
    }
}

pub(crate) fn normalize_legacy_headers(headers: &mut HeaderMap) -> Result<(), Error> {
    if !crate::auth::signature::legacy_aws(headers) {
        return Ok(());
    }
    let mut replacements = Vec::new();
    for name in headers
        .keys()
        .filter(|name| name.as_str().starts_with("x-amz-"))
    {
        let suffix = &name.as_str()[6..];
        if suffix == "date" || suffix == "project-id" || suffix.starts_with("meta-") {
            let target = HeaderName::from_bytes(format!("x-goog-{suffix}").as_bytes())
                .map_err(|_| Error::unsupported(name.as_str()))?;
            if headers.contains_key(&target) {
                return Err(Error::new(
                    StatusCode::BAD_REQUEST,
                    "InvalidArgument",
                    "Ambiguous legacy extension headers.",
                ));
            }
            replacements.push((
                name.clone(),
                target,
                headers.get_all(name).iter().cloned().collect::<Vec<_>>(),
            ));
        }
    }
    for (raw, target, values) in replacements {
        headers.remove(raw);
        for value in values {
            headers.append(target.clone(), value);
        }
    }
    Ok(())
}

pub(crate) struct ReadRequest {
    pub bucket: String,
    pub name: String,
    pub generation: Option<i64>,
    pub conditions: ObjectConditions,
    pub response_overrides: HeaderMap,
}

#[derive(Debug)]
pub(crate) struct Error {
    pub status: StatusCode,
    code: &'static str,
    message: String,
}

/// Whether an XML error response carries its body. A HEAD response sends only the headers,
/// including the Content-Length of the body it omits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorBody {
    Include,
    Omit,
}

impl ErrorBody {
    pub(crate) fn for_method(method: &Method) -> Self {
        if method == Method::HEAD {
            Self::Omit
        } else {
            Self::Include
        }
    }
}

impl Error {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub(crate) fn unsupported(name: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "InvalidArgument",
            format!(
                "gcs-mimic does not support XML API {name}; omit this option or use a supported operation (for example, GET /BUCKET/OBJECT without extra query parameters)."
            ),
        )
    }

    pub(crate) fn response(self, body_mode: ErrorBody) -> Response<Full<Bytes>> {
        let escaped = self
            .message
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{}</Code><Message>{escaped}</Message></Error>",
            self.code
        );
        let content_length = HeaderValue::from(body.len());
        let mut response = Response::new(Full::new(match body_mode {
            ErrorBody::Omit => Bytes::new(),
            ErrorBody::Include => Bytes::from(body),
        }));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        headers.insert(
            "content-type",
            HeaderValue::from_static("application/xml; charset=UTF-8"),
        );
        headers.insert("content-length", content_length);
        response
    }
}

impl From<JsonError> for Error {
    fn from(error: JsonError) -> Self {
        // XML reference-status pairs invalid/unsupported auth headers with 400; 401 covers expiry.
        let status = if error.status == StatusCode::UNAUTHORIZED && error.reason != "required" {
            StatusCode::BAD_REQUEST
        } else {
            error.status
        };
        let code = match error.status {
            StatusCode::UNAUTHORIZED if error.reason == "required" => "AuthenticationRequired",
            StatusCode::UNAUTHORIZED => "InvalidAuthentication",
            StatusCode::FORBIDDEN if error.reason == "retentionPolicyNotMet" => {
                "RetentionPolicyNotMet"
            }
            StatusCode::FORBIDDEN if error.reason == "objectUnderActiveHold" => {
                "ObjectUnderActiveHold"
            }
            StatusCode::FORBIDDEN => "AccessDenied",
            StatusCode::NOT_FOUND if error.message == "Bucket not found" => "NoSuchBucket",
            StatusCode::NOT_FOUND => "NoSuchKey",
            StatusCode::PRECONDITION_FAILED => "PreconditionFailed",
            StatusCode::RANGE_NOT_SATISFIABLE => "InvalidRange",
            StatusCode::REQUEST_TIMEOUT => "uploadBrokenConnection",
            StatusCode::SERVICE_UNAVAILABLE => "ServiceUnavailable",
            _ => crate::csek::CsekError::from_reason(error.reason)
                .map_or("InvalidArgument", crate::csek::CsekError::xml_code),
        };
        // shortcut: auth wording and error precedence are local choices until cloud differential evidence exists.
        Self::new(status, code, error.message)
    }
}

pub(crate) fn reserved_path(path: &str) -> bool {
    [
        "/storage/v1",
        "/upload/storage/v1",
        "/download/storage/v1",
        "/batch/storage/v1",
        "/_internal",
    ]
    .iter()
    .any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|tail| tail.starts_with('/'))
    })
}

pub(crate) fn virtual_bucket(headers: &HeaderMap, domains: &[String]) -> Option<String> {
    let mut hosts = headers.get_all("host").iter();
    let host = hosts.next()?.to_str().ok()?;
    if hosts.next().is_some() {
        return None;
    }
    bucket_for_authority(host, domains)
}

pub(crate) fn bucket_for_authority(host: &str, domains: &[String]) -> Option<String> {
    let authority: hyper::http::uri::Authority = host.parse().ok()?;
    if authority.as_str().contains('@')
        || (authority.as_str().contains(':') && authority.port_u16().is_none())
    {
        return None;
    }
    let host = authority.host().to_ascii_lowercase();
    for domain in domains {
        if host == *domain {
            return None;
        }
        if let Some(bucket) = host.strip_suffix(domain).and_then(|s| s.strip_suffix('.')) {
            return Some(bucket.to_owned());
        }
    }
    None
}

fn legacy_public_authority<'a>(
    config: &Config,
    endpoints: &'a EndpointSnapshot,
) -> Option<&'a hyper::http::uri::Authority> {
    if config.compat_profile != crate::config::CompatProfile::LegacyEmulator {
        return None;
    }
    endpoints
        .public_base_url
        .as_ref()
        .map(crate::url::PublicOrigin::authority)
}

fn request_authority(headers: &HeaderMap) -> Option<hyper::http::uri::Authority> {
    let mut hosts = headers.get_all("host").iter();
    let authority: hyper::http::uri::Authority = hosts.next()?.to_str().ok()?.parse().ok()?;
    if hosts.next().is_some()
        || authority.as_str().contains('@')
        || (authority.as_str() != authority.host() && authority.port_u16().is_none())
    {
        return None;
    }
    Some(authority)
}

fn public_authority_matches(request: &str, public: &str) -> bool {
    if public.contains(':') {
        request == public
    } else {
        request.split(':').next() == Some(public)
    }
}

/// Classify once so a failed public suffix cannot fall through to a shorter native suffix.
fn legacy_virtual_bucket(
    authority: &hyper::http::uri::Authority,
    public: &hyper::http::uri::Authority,
    endpoints: &EndpointSnapshot,
) -> (Option<String>, bool) {
    let host = authority.host().to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    let domain = endpoints.xml_virtual_host_domains.iter().find(|domain| {
        host == domain.as_str()
            || host
                .strip_suffix(domain.as_str())
                .is_some_and(|s| s.ends_with('.'))
    });
    if domain.is_some_and(|domain| domain == public.host()) {
        let bucket = authority
            .host()
            .strip_suffix(public.host())
            .and_then(|s| s.strip_suffix('.'))
            .filter(|bucket| !bucket.is_empty() && !bucket.contains('.'))
            .filter(|_| {
                public.port_u16().is_none()
                    || authority.as_str().strip_prefix(authority.host())
                        == public.as_str().strip_prefix(public.host())
            })
            .map(str::to_owned);
        (bucket, true)
    } else {
        (
            bucket_for_authority(authority.as_str(), &endpoints.xml_virtual_host_domains),
            false,
        )
    }
}

pub(crate) struct ObjectPath {
    pub bucket: String,
    uri: Uri,
    name_start: usize,
}

impl ObjectPath {
    pub(crate) fn signature_resource(&self) -> crate::auth::signature::Resource {
        if self.name_start == 1 {
            crate::auth::signature::Resource::VirtualHosted(self.bucket.clone())
        } else {
            crate::auth::signature::Resource::PathStyle
        }
    }
    pub(crate) fn name(&self) -> &str {
        &self.uri.path()[self.name_start..]
    }
}

/// Preserve the raw object path and reuse one routing decision for CORS and dispatch.
#[cfg(test)]
pub(crate) fn select_object(
    uri: &Uri,
    headers: &HeaderMap,
    config: &Config,
) -> Result<Option<ObjectPath>, Error> {
    let endpoints = EndpointSnapshot {
        api_base_url: config.effective_api_base_url(config.listen),
        public_base_url: config.public_base_url.clone(),
        xml_virtual_host_domains: config.xml_virtual_host_domains.clone(),
    };
    select_object_with_endpoints(uri, headers, config, &endpoints)
}

pub(crate) fn select_object_with_endpoints(
    uri: &Uri,
    headers: &HeaderMap,
    config: &Config,
    endpoints: &EndpointSnapshot,
) -> Result<Option<ObjectPath>, Error> {
    let public = legacy_public_authority(config, endpoints);
    let legacy_authority = if public.is_some() {
        Some(request_authority(headers).ok_or_else(|| {
            Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidArgument",
                "Require one valid Host authority.",
            )
        })?)
    } else {
        None
    };
    let (virtual_bucket, public_domain) = match (&legacy_authority, public) {
        (Some(authority), Some(public)) => legacy_virtual_bucket(authority, public, endpoints),
        _ => (
            virtual_bucket(headers, &endpoints.xml_virtual_host_domains),
            false,
        ),
    };
    if let Some(authority) = uri.authority()
        && (public.is_some()
            || virtual_bucket.is_some()
            || bucket_for_authority(authority.as_str(), &endpoints.xml_virtual_host_domains)
                .is_some())
    {
        let mut hosts = headers.get_all("host").iter();
        if !hosts
            .next()
            .is_some_and(|host| host.as_bytes() == authority.as_str().as_bytes())
            || hosts.next().is_some()
        {
            return Err(if crate::auth::signature::is_signed(uri.query()) {
                Error::new(
                    StatusCode::FORBIDDEN,
                    "SignatureDoesNotMatch",
                    "The request authority does not match Host.",
                )
            } else {
                Error::new(
                    StatusCode::BAD_REQUEST,
                    "InvalidArgument",
                    "The request authority does not match Host.",
                )
            });
        }
    }
    let path = uri.path();
    if reserved_path(path) && (public_domain || virtual_bucket.is_none()) {
        return Ok(None);
    }
    let Some(path) = path.strip_prefix('/') else {
        return Ok(None);
    };
    let is_virtual = virtual_bucket.is_some();
    let (bucket, name) = match (virtual_bucket, legacy_authority, public) {
        (Some(bucket), _, _) => (bucket, path),
        (None, Some(authority), Some(public))
            if !public_authority_matches(authority.as_str(), public.as_str()) =>
        {
            (authority.host().to_owned(), path)
        }
        _ => {
            let (bucket, name) = path.split_once('/').unwrap_or((path, ""));
            (json::decode(bucket, json::Decoding::Path)?, name)
        }
    };
    if public.is_none()
        && !is_virtual
        && json::validate_bucket_name_for_profile(&bucket, config.compat_profile).is_err()
    {
        return Ok(None);
    }
    Ok(Some(ObjectPath {
        bucket,
        uri: uri.clone(),
        name_start: uri.path().len() - name.len(),
    }))
}

pub(crate) fn route(
    method: &Method,
    config: &Config,
    selected: Option<ObjectPath>,
    headers: &HeaderMap,
) -> Option<Result<ReadRequest, Error>> {
    let ObjectPath {
        bucket,
        uri,
        name_start,
    } = selected?;
    let name = &uri.path()[name_start..];
    Some((|| {
        json::validate_bucket_name_for_profile(&bucket, config.compat_profile)?;
        let mut query =
            crate::auth::signature::xml_query(uri.query().unwrap_or_default(), headers)?;
        let v2 = query
            .keys()
            .any(|key| crate::auth::signature::V2_PARAMETERS.contains(&key.as_str()));
        if v2 && (query.contains_key("generation") || (name.is_empty() && name_start == 1)) {
            return Err(Error::unsupported(
                "V2 generation selection or virtual-host bucket query; use V4 signing",
            ));
        }
        if multipart::matches(&uri) {
            let name = json::decode(name, json::Decoding::Path)?;
            if !name.is_empty() {
                json::validate_object_name(&name)?;
            }
            return Ok(ReadRequest {
                bucket,
                name,
                generation: None,
                conditions: ObjectConditions::default(),
                response_overrides: HeaderMap::new(),
            });
        }
        if *method == Method::GET && name.is_empty() {
            return Ok(ReadRequest {
                bucket,
                name: String::new(),
                generation: None,
                conditions: ObjectConditions::default(),
                response_overrides: HeaderMap::new(),
            });
        }
        if !matches!(
            *method,
            Method::GET | Method::HEAD | Method::PUT | Method::DELETE
        ) || name.is_empty()
        {
            return Err(Error::unsupported(&format!(
                "{method} route; use GET, HEAD, PUT or DELETE on /BUCKET/OBJECT"
            )));
        }
        let name = json::decode(name, json::Decoding::Path)?;
        json::validate_object_name(&name)?;
        if *method == Method::PUT
            && let Some(key) = query.keys().find(|key| !signing_parameter(key))
        {
            return Err(Error::unsupported(key));
        }
        let generation = query
            .remove("generation")
            .map(|value| number(&value, MinValue::One))
            .transpose()?;
        if *method == Method::DELETE && query.keys().any(|key| !signing_parameter(key)) {
            return Err(Error::unsupported(
                "DELETE options; use DELETE /BUCKET/OBJECT with only an optional generation",
            ));
        }
        let mut response_overrides = HeaderMap::new();
        for (parameter, header) in [
            ("response-content-type", "content-type"),
            ("response-content-disposition", "content-disposition"),
        ] {
            if let Some(value) = query.remove(parameter) {
                if value.is_empty()
                    || value.trim() != value
                    || value
                        .bytes()
                        .any(|byte| byte.is_ascii_control() && byte != b'\t')
                {
                    return Err(Error::new(
                        StatusCode::BAD_REQUEST,
                        "InvalidArgument",
                        "Invalid response header override.",
                    ));
                }
                let value = HeaderValue::from_str(&value)
                    .ok()
                    .filter(|value| value.to_str().is_ok())
                    .ok_or_else(|| Error::unsupported(parameter))?;
                response_overrides.insert(header, value);
            }
        }
        // Authentication validates credential fields from the original URI before using this operation.
        if let Some(key) = query.keys().find(|key| !signing_parameter(key)) {
            return Err(Error::unsupported(key));
        }
        Ok(ReadRequest {
            bucket,
            name,
            generation,
            conditions: ObjectConditions::default(),
            response_overrides,
        })
    })())
}

fn signing_parameter(key: &str) -> bool {
    crate::auth::signature::PARAMETERS.contains(&key)
        || crate::auth::signature::AWS_PARAMETERS.contains(&key)
        || crate::auth::signature::V2_PARAMETERS.contains(&key)
}

/// The smallest generation or metageneration a field accepts. Generation 0 in a generation
/// precondition requires that no live object exists; every other field starts at 1.
#[derive(Clone, Copy)]
enum MinValue {
    Zero,
    One,
}

fn number(raw: &str, min: MinValue) -> Result<i64, Error> {
    let floor = match min {
        MinValue::Zero => 0,
        MinValue::One => 1,
    };
    raw.parse::<i64>()
        .ok()
        .filter(|n| *n >= floor)
        .filter(|_| !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| {
            Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidArgument",
                "Invalid generation or metageneration.",
            )
        })
}

pub(crate) fn headers(headers: &HeaderMap, request: &mut ReadRequest) -> Result<(), Error> {
    if let Some(hash) = crate::auth::signature::payload_hash(headers).map_err(|_| {
        Error::new(
            StatusCode::BAD_REQUEST,
            "InvalidDigest",
            "Invalid SHA256 declaration.",
        )
    })? && hash.as_slice() != ring::digest::digest(&ring::digest::SHA256, b"").as_ref()
    {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "BadDigest",
            "The content hash does not match the empty read body.",
        ));
    }
    let mut tokens = headers.get_all("x-goog-gcs-idempotency-token").iter();
    if let Some(token) = tokens.next() {
        if tokens.next().is_some() {
            return Err(Error::new(
                StatusCode::BAD_REQUEST,
                "ExcessHeaderValues",
                "Expected a single x-goog-gcs-idempotency-token header.",
            ));
        }
        if token.as_bytes().trim_ascii().is_empty() {
            return Err(Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidArgument",
                "Expected a non-empty x-goog-gcs-idempotency-token header.",
            ));
        }
    }
    // storage/v1.70.0 invoke.go:215-220 sends client telemetry and retry IDs;
    // http_client.go:1551-1587 uses them on idempotent reads, which need no token replay.
    for key in headers.keys() {
        let key = key.as_str();
        // Customer-supplied key headers are parsed by the GET/HEAD and DELETE callers.
        if (key.starts_with("x-goog-")
            && !crate::csek::is_key_header(key, crate::csek::DESTINATION_PREFIX)
            && !crate::auth::signature::authentication_header(key)
            && !matches!(
                key,
                "x-goog-if-generation-match"
                    | "x-goog-if-metageneration-match"
                    | "x-goog-api-client"
                    | "x-goog-gcs-idempotency-token"
                    | "x-goog-content-sha256"
            ))
            || (key.starts_with("x-amz-") && !crate::auth::signature::authentication_header(key))
            || matches!(
                key,
                "if-match"
                    | "if-none-match"
                    | "if-modified-since"
                    | "if-unmodified-since"
                    | "if-range"
            )
        {
            return Err(Error::unsupported(key));
        }
    }
    for (key, target, min) in [
        (
            "x-goog-if-generation-match",
            &mut request.conditions.if_generation_match,
            MinValue::Zero,
        ),
        (
            "x-goog-if-metageneration-match",
            &mut request.conditions.if_metageneration_match,
            MinValue::One,
        ),
    ] {
        let mut values = headers.get_all(key).iter();
        if let Some(value) = values.next() {
            if values.next().is_some() {
                return Err(Error::new(
                    StatusCode::BAD_REQUEST,
                    "ExcessHeaderValues",
                    "Multiple precondition header values.",
                ));
            }
            *target = Some(number(
                value.to_str().map_err(|_| {
                    Error::new(
                        StatusCode::BAD_REQUEST,
                        "InvalidArgument",
                        "Invalid precondition header.",
                    )
                })?,
                min,
            )?);
        }
    }
    Ok(())
}

pub(crate) async fn empty_body<B>(mut body: B) -> Result<(), Error>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
{
    let unexpected = || {
        Error::new(
            StatusCode::BAD_REQUEST,
            "UnexpectedContent",
            "This operation requires an empty body.",
        )
    };
    if body.size_hint().lower() > 0 {
        return Err(unexpected());
    }
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| {
            Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidArgument",
                "Could not read request body.",
            )
        })?;
        let data = frame
            .into_data()
            .map_err(|_| Error::unsupported("request trailers"))?;
        if !data.is_empty() {
            return Err(unexpected());
        }
    }
    Ok(())
}

/// The default `Cache-Control` scope for an object without its own `cacheControl`: shared caches
/// may keep publicly readable objects, while every other object stays private.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheScope {
    Public,
    Private,
}

fn object_etag(record: &ObjectRecord) -> String {
    // MD5 ETags apply only to objects without customer-supplied encryption.
    if !record.has_md5() || record.customer_key_sha256.is_some() {
        json::object_etag(record)
    } else {
        format!("\"{}\"", crate::hashing::encode_lower(&record.md5))
    }
}

fn element(body: &mut String, name: &str, value: &str) {
    body.push('<');
    body.push_str(name);
    body.push('>');
    for ch in value.chars() {
        match ch {
            '&' => body.push_str("&amp;"),
            '<' => body.push_str("&lt;"),
            '>' => body.push_str("&gt;"),
            '"' => body.push_str("&quot;"),
            '\'' => body.push_str("&apos;"),
            _ => body.push(ch),
        }
    }
    body.push_str("</");
    body.push_str(name);
    body.push('>');
}

pub(crate) fn media_headers(record: &ObjectRecord, scope: CacheScope) -> Result<HeaderMap, Error> {
    let mut headers = json::object_media_headers(record)?;
    let cache = record.cache_control.as_deref().unwrap_or(match scope {
        CacheScope::Public => "public, max-age=3600",
        CacheScope::Private => "private, max-age=0",
    });
    let etag = object_etag(record);
    let timestamp = json::timestamp(record.updated)?;
    let invalid_timestamp = || {
        Error::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ServiceUnavailable",
            "Invalid stored timestamp.",
        )
    };
    let days = record
        .updated
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid_timestamp())?
        .as_secs()
        / 86400;
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][(days % 7) as usize];
    // RFC 3339 timestamps always carry a two-digit month at bytes 5..7.
    let month = timestamp
        .get(5..7)
        .and_then(|month| month.parse::<usize>().ok())
        .and_then(|month| {
            [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ]
            .get(month.checked_sub(1)?)
            .copied()
        })
        .ok_or_else(invalid_timestamp)?;
    let date = format!(
        "{weekday}, {} {month} {} {} GMT",
        &timestamp[8..10],
        &timestamp[..4],
        &timestamp[11..19]
    );
    for (key, value) in [
        ("cache-control", cache),
        ("etag", &etag),
        ("last-modified", &date),
        ("x-goog-storage-class", &record.storage_class),
    ] {
        headers.insert(
            key,
            HeaderValue::from_str(value).map_err(|_| {
                Error::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "ServiceUnavailable",
                    "Invalid stored header.",
                )
            })?,
        );
    }
    if let Some(count) = record.component_count {
        headers.insert("x-goog-component-count", HeaderValue::from(count));
    }
    for (key, value) in &record.metadata {
        let key =
            HeaderName::from_bytes(format!("x-goog-meta-{key}").as_bytes()).map_err(|_| {
                Error::unsupported(&format!(
                    "custom metadata key {key}; use JSON metadata reads"
                ))
            })?;
        let value = HeaderValue::from_str(value).map_err(|_| {
            Error::unsupported(&format!(
                "custom metadata value for {key}; use JSON metadata reads"
            ))
        })?;
        if headers.contains_key(&key) {
            return Err(Error::unsupported(key.as_str()));
        }
        headers.insert(key, value);
    }
    Ok(headers)
}
