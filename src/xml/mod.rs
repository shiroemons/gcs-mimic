//! XML objects. Shared authorization, generation selection and bytes live in storage.

use crate::{
    config::{Config, EndpointSnapshot},
    json::{self, JsonError},
    object::{ObjectConditions, ObjectRecord, ObjectTarget},
};
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Method, Response, StatusCode, Uri,
    body::Bytes,
    header::{HeaderName, HeaderValue},
};

pub(crate) mod buckets;
mod code;
use code::XmlCode;
mod copy;
mod cors;
mod document;
pub(crate) mod envelope;
#[cfg(test)]
mod error_tests;
pub(crate) mod list;
pub(crate) mod multipart;
#[cfg(test)]
mod test_support;
pub use cors::CorsWarnings;
pub(crate) use cors::{cors, warn_unconfigured_cors};
pub(crate) mod post;
pub(crate) mod put;
pub(crate) mod resumable;

/// Inputs to XML authentication. The returned Caller borrows only auth, so the request
/// can be mutated or consumed afterwards.
pub(crate) struct Authentication<'auth, 'request> {
    pub(crate) auth: &'auth crate::auth::Auth,
    pub(crate) catalog: &'request crate::storage::Catalog,
    pub(crate) method: &'request Method,
    pub(crate) uri: &'request Uri,
    pub(crate) headers: &'request HeaderMap,
    pub(crate) clock: &'request dyn crate::clock::Clock,
    pub(crate) resource: &'request crate::auth::signature::Resource,
    /// Decides who an unsigned request without credentials acts as; signatures verify alike on
    /// every listener.
    pub(crate) listener: crate::auth::ListenerRole,
}

pub(crate) async fn authenticate_target<'a>(
    authentication: Authentication<'a, '_>,
) -> Result<Option<crate::auth::Caller<'a>>, Error> {
    let Authentication {
        auth,
        catalog,
        method,
        uri,
        headers,
        clock,
        resource,
        listener,
    } = authentication;
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
                        XmlCode::RequestTimeTooSkewed,
                        "The legacy signature timestamp is outside the allowed time window.",
                    )
                } else {
                    signature_error(error)
                }
            })
    } else {
        auth.authenticate_request(headers, uri.query(), clock, listener)
            .await
            .map_err(|e| Error::from(JsonError::from(e)))
    }
}

/// The InvalidAccessKeyId answer, naming where the profile looks for `key`s so that the client
/// knows what to register or enable.
fn unknown_signer(lookup: crate::auth::signature::KeyLookup, key: &str) -> Error {
    use crate::auth::signature::KeyLookup;
    let message = match lookup {
        KeyLookup::Registry => {
            format!("The {key} or its registered identity is unknown or inactive.")
        }
        KeyLookup::PermissiveKeys => format!(
            "The {key} is unknown or inactive. With signed_urls=verify, keys come from identity file public_keys, HMAC keys created through the JSON API and the built-in test signer."
        ),
        KeyLookup::DisabledTestSigner => format!(
            "{} is the built-in test signer, which is disabled; set test_signer=enabled.",
            crate::auth::test_signer::ID
        ),
        KeyLookup::Ignored => format!(
            "No {key} can verify this request because signatures are ignored; set signed_urls=verify, or send the request unsigned."
        ),
    };
    Error::new(XmlCode::InvalidAccessKeyId, message)
}

fn signature_error(failure: crate::auth::signature::VerificationError) -> Error {
    use crate::auth::signature::{Failure, InvalidExpires, MAX_EXPIRES, VerificationError};
    let failure = match failure {
        VerificationError::Signature(failure) => failure,
        VerificationError::Catalog(error) => return Error::from(JsonError::from(error)),
        VerificationError::Worker(_) => {
            return Error::new(
                XmlCode::ServiceUnavailable,
                "The signature verifier is unavailable.",
            );
        }
    };
    match failure {
        Failure::Malformed => Error::new(
            XmlCode::InvalidAuthentication,
            "Invalid V2/V4 signing parameters; use exactly one signing version (for example, GoogleAccessId, Expires=1793090000 and Signature for V2).",
        ),
        // Cloud Storage's answer to this case is not recorded, so only the message changes.
        Failure::ExpiresOutOfRange(parameter, value) => Error::new(
            XmlCode::InvalidAuthentication,
            match value {
                InvalidExpires::Zero => format!(
                    "{parameter} must be a whole number of seconds from 1 to {MAX_EXPIRES}; 0 usually means the signer's expiry was less than one second away."
                ),
                InvalidExpires::Other => format!(
                    "{parameter} must be a whole number of seconds from 1 to {MAX_EXPIRES}."
                ),
            },
        ),
        Failure::UnknownSigner(lookup) => unknown_signer(lookup, "signing key"),
        Failure::Mismatch => Error::new(
            XmlCode::SignatureDoesNotMatch,
            "The request signature does not match.",
        ),
        Failure::MismatchWithHostPort => Error::new(
            XmlCode::SignatureDoesNotMatch,
            "The request signature does not match. It matches this request without the port in Host, as Go storage signs; set signed_host_port=lenient to accept it, which is already the default where the built-in test signer is enabled.",
        ),
        Failure::Expired => Error::new(XmlCode::ExpiredToken, "The request signature has expired."),
        Failure::NotYetValid => Error::new(
            XmlCode::RequestTimeTooSkewed,
            "The request signature is not yet usable.",
        ),
    }
}

pub(crate) async fn authenticate_upload_target<'a>(
    authentication: Authentication<'a, '_>,
) -> Result<
    (
        Option<crate::auth::Caller<'a>>,
        Option<crate::auth::signature::Streaming>,
    ),
    Error,
> {
    let Authentication {
        auth,
        catalog,
        method,
        uri,
        headers,
        clock,
        ..
    } = authentication;
    if headers
        .get("x-amz-content-sha256")
        .is_some_and(|value| value == crate::auth::signature::STREAMING_PAYLOAD)
    {
        let (caller, streaming) = auth
            .authenticate_streaming(catalog, method, uri, headers, clock)
            .await
            .map_err(signature_error)?;
        Ok((caller, Some(streaming)))
    } else {
        authenticate_target(authentication)
            .await
            .map(|caller| (caller, None))
    }
}

pub(crate) fn normalize_legacy_headers(headers: &mut HeaderMap) -> Result<(), Error> {
    if !crate::auth::signature::legacy_aws(headers) {
        return Ok(());
    }
    let mut replacements = Vec::new();
    for (name, suffix) in headers.keys().filter_map(|name| {
        name.as_str()
            .strip_prefix("x-amz-")
            .map(|suffix| (name, suffix))
    }) {
        if suffix == "date" || suffix == "project-id" || suffix.starts_with("meta-") {
            let target = HeaderName::from_bytes(format!("x-goog-{suffix}").as_bytes())
                .map_err(|_| Error::unsupported(name.as_str()))?;
            if headers.contains_key(&target) {
                return Err(Error::invalid("Ambiguous legacy extension headers."));
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

/// An XML object request's path and response overrides; headers supply preconditions separately.
pub(crate) struct ReadRequest {
    pub target: ObjectTarget,
    pub response_overrides: HeaderMap,
}

#[derive(Debug)]
pub(crate) struct Error {
    code: XmlCode,
    message: String,
    /// The optional `Details` element, which Cloud Storage uses to name a missing object.
    details: Option<String>,
}

/// The XML declaration Cloud Storage writes, with single-quoted pseudo-attributes.
pub(crate) const DECLARATION: &str = "<?xml version='1.0' encoding='UTF-8'?>";
/// The fixed message of a missing object; `Details` names the object.
const NO_SUCH_KEY: &str = "The specified key does not exist.";
/// The fixed message of a failed precondition, whichever precondition failed.
const PRECONDITION_NOT_HELD: &str =
    "At least one of the pre-conditions you specified did not hold.";

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
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(XmlCode::InvalidArgument, message)
    }

    fn new(code: XmlCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub(crate) fn precondition_failed() -> Self {
        Self::new(XmlCode::PreconditionFailed, PRECONDITION_NOT_HELD)
    }

    /// The 400 `RequestTimeout` for a client whose socket stayed silent too long.
    pub(crate) fn request_timeout(message: impl Into<String>) -> Self {
        Self::new(XmlCode::RequestTimeout, message)
    }

    pub(crate) fn status(&self) -> StatusCode {
        self.code.status()
    }

    /// Names the requested object in a missing-object error, as Cloud Storage does.
    pub(crate) fn naming_object(mut self, bucket: &str, name: &str) -> Self {
        if self.code == XmlCode::NoSuchKey && self.message == NO_SUCH_KEY && self.details.is_none()
        {
            self.details = Some(format!("No such object: {bucket}/{name}"));
        }
        self
    }

    /// The 416 for a Range no byte satisfies; `Details` echoes the request's Range value as
    /// Cloud Storage does (observed 2026-10-31).
    pub(crate) fn unsatisfiable_range(requested: &str) -> Self {
        let mut error = Self::new(
            XmlCode::InvalidRange,
            "The requested range cannot be satisfied.",
        );
        error.details = Some(requested.to_owned());
        error
    }

    /// The 400 for a Range header with several ranges; `Details` echoes the header as
    /// Cloud Storage does (observed 2026-10-31).
    pub(crate) fn multiple_ranges(requested: &str) -> Self {
        let mut error = Self::invalid("Invalid argument.");
        error.details = Some(format!(
            "Multiple ranges are not supported; Range:{requested}"
        ));
        error
    }

    pub(crate) fn unsupported(name: &str) -> Self {
        Self::invalid(format!(
            "gcs-mimic does not support XML API {name}; omit this option or use a supported operation (for example, GET /BUCKET/OBJECT without extra query parameters)."
        ))
    }

    pub(crate) fn response(self, body_mode: ErrorBody) -> Response<Full<Bytes>> {
        let mut body = format!("{DECLARATION}<Error>");
        element(&mut body, "Code", self.code.as_str());
        element(&mut body, "Message", &self.message);
        if let Some(details) = &self.details {
            element(&mut body, "Details", details);
        }
        body.push_str("</Error>");
        let content_length = HeaderValue::from(body.len());
        let mut response = document_response(match body_mode {
            ErrorBody::Omit => Bytes::new(),
            ErrorBody::Include => Bytes::from(body),
        });
        *response.status_mut() = self.status();
        let headers = response.headers_mut();
        headers.insert("content-length", content_length);
        headers.insert(
            "cache-control",
            HeaderValue::from_static(PRIVATE_CACHE_CONTROL),
        );
        response
    }
}

fn document_response(body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(body.into()));
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/xml; charset=UTF-8"),
    );
    response
}

/// The `Cache-Control` Cloud Storage sends with XML API lists, errors and private objects.
pub(crate) const PRIVATE_CACHE_CONTROL: &str = "private, max-age=0";

impl From<JsonError> for Error {
    fn from(error: JsonError) -> Self {
        // XML reference-status pairs invalid/unsupported auth headers with 400; 401 covers expiry.
        let code = match error.status {
            StatusCode::UNAUTHORIZED if error.kind == json::ErrorKind::Required => {
                XmlCode::AuthenticationRequired
            }
            StatusCode::UNAUTHORIZED => XmlCode::InvalidAuthentication,
            StatusCode::FORBIDDEN if error.kind == json::ErrorKind::RetentionNotMet => {
                XmlCode::RetentionPolicyNotMet
            }
            StatusCode::FORBIDDEN if error.kind == json::ErrorKind::ActiveHold => {
                XmlCode::ObjectUnderActiveHold
            }
            StatusCode::FORBIDDEN => XmlCode::AccessDenied,
            StatusCode::NOT_FOUND if error.is_missing_bucket() => XmlCode::NoSuchBucket,
            StatusCode::NOT_FOUND => XmlCode::NoSuchKey,
            StatusCode::PRECONDITION_FAILED => XmlCode::PreconditionFailed,
            StatusCode::RANGE_NOT_SATISFIABLE => XmlCode::InvalidRange,
            StatusCode::REQUEST_TIMEOUT => XmlCode::UploadBrokenConnection,
            StatusCode::SERVICE_UNAVAILABLE => XmlCode::ServiceUnavailable,
            _ => match error.kind {
                json::ErrorKind::Csek(error) => XmlCode::Csek(error),
                _ => XmlCode::forwarded(error.status),
            },
        };
        let message = if error.is_missing_object() {
            NO_SUCH_KEY.to_owned()
        } else if error.status == StatusCode::PRECONDITION_FAILED {
            PRECONDITION_NOT_HELD.to_owned()
        } else {
            error.message
        };
        // shortcut: auth wording and error precedence are local choices until cloud differential evidence exists.
        Self::new(code, message)
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
    split_virtual_host(&authority, domains).map(|(bucket, _)| bucket)
}

/// The bucket a host selects, and the configured domain it selects that bucket under.
fn split_virtual_host<'d>(
    authority: &hyper::http::uri::Authority,
    domains: &'d [String],
) -> Option<(String, &'d str)> {
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
            return Some((bucket.to_owned(), domain));
        }
    }
    None
}

/// The API's authority behind a host that selects a virtual bucket: the configured domain, which
/// the configuration already lowercased, and the port the client reached it through.
pub(crate) fn virtual_host_api_authority(
    authority: &hyper::http::uri::Authority,
    domains: &[String],
) -> Option<hyper::http::uri::Authority> {
    let (_, domain) = split_virtual_host(authority, domains)?;
    let port = authority.as_str().strip_prefix(authority.host())?;
    format!("{domain}{port}").parse().ok()
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

pub(crate) fn request_authority(headers: &HeaderMap) -> Option<hyper::http::uri::Authority> {
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
    // Strip only for domain classification; trailing-dot hosts still route path-style.
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
    #[expect(
        clippy::string_slice,
        reason = "name_start is computed from a path suffix in select_object_with_endpoints, so it is in bounds at a UTF-8 boundary"
    )]
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
    let endpoints = EndpointSnapshot::explicit(
        &config.effective_api_base_url(config.listen),
        config.public_base_url.clone(),
        config.xml_virtual_host_domains.clone(),
    );
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
        Some(
            request_authority(headers)
                .ok_or_else(|| Error::invalid("Require one valid Host authority."))?,
        )
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
                    XmlCode::SignatureDoesNotMatch,
                    "The request authority does not match Host.",
                )
            } else {
                Error::invalid("The request authority does not match Host.")
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
    Some((|| {
        let name = uri
            .path()
            .get(name_start..)
            .ok_or_else(|| Error::invalid("Invalid object path"))?;
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
                crate::object::name::validate(&name)
                    .map_err(|_| json::JsonError::invalid_parameter("Invalid object name"))?;
            }
            return Ok(ReadRequest {
                target: ObjectTarget::new(bucket, name, None),
                response_overrides: HeaderMap::new(),
            });
        }
        if *method == Method::GET && name.is_empty() {
            return Ok(ReadRequest {
                target: ObjectTarget::new(bucket, String::new(), None),
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
        crate::object::name::validate(&name)
            .map_err(|_| json::JsonError::invalid_parameter("Invalid object name"))?;
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
                    return Err(Error::invalid("Invalid response header override."));
                }
                let value = HeaderValue::from_str(&value)
                    .ok()
                    .filter(|value| value.to_str().is_ok())
                    .ok_or_else(|| Error::unsupported(parameter))?;
                response_overrides.insert(header, value);
            }
        }
        // Authentication validates credential fields from the original URI before using this operation.
        // Cloud Storage answered a GET with an unknown query parameter as if it were absent
        // (recorded). A parameter that selects a different resource, such as `acl`, would give
        // a false success if ignored, so those stay rejected. HEAD and PUT share the GET and
        // PUT parsing; HEAD ignoring unknown parameters is assumed, not recorded.
        let ignores_unknown = matches!(*method, Method::GET | Method::HEAD);
        if let Some(key) = query.keys().find(|key| {
            !signing_parameter(key)
                && (!ignores_unknown
                    || SUBRESOURCES.contains(&key.as_str())
                    || RECOGNIZED_UNSUPPORTED_QUERY.contains(&key.as_str())
                    || key.starts_with("response-"))
        }) {
            return Err(Error::unsupported(key));
        }
        Ok(ReadRequest {
            target: ObjectTarget::new(bucket, name, generation),
            response_overrides,
        })
    })())
}

/// Query parameters that address a sub-resource of an object (its ACL, torrent, tags and so on)
/// rather than the object itself. Answering them with the object would be a false success.
const SUBRESOURCES: &[&str] = &[
    "acl",
    "attributes",
    "legal-hold",
    "partNumber",
    "retention",
    "tagging",
    "torrent",
    "uploadId",
    "uploads",
];

/// Query parameters with defined meaning (billing project, preconditions, soft-delete
/// selection) that this adapter does not implement on an object read. Ignoring them would answer
/// as if the billing project, condition or selection were absent, so they are rejected like
/// the `x-goog-user-project` header.
const RECOGNIZED_UNSUPPORTED_QUERY: &[&str] = &[
    "ifGenerationMatch",
    "ifGenerationNotMatch",
    "ifMetagenerationMatch",
    "ifMetagenerationNotMatch",
    "softDeleted",
    "userProject",
];

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
        .ok_or_else(|| Error::invalid("Invalid generation or metageneration."))
}

/// `x-goog-` headers with defined meaning that this adapter does not implement. An object read
/// that ignored them would answer as if the condition, billing or key were absent.
const RECOGNIZED_UNSUPPORTED_GOOG_HEADERS: &[&str] = &[
    "x-goog-encryption-kms-key-name",
    "x-goog-if-generation-not-match",
    "x-goog-if-metageneration-not-match",
    "x-goog-user-project",
];

/// Reads allow unknown x-goog headers and evaluate HTTP validators after authorization.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderPolicy {
    Strict,
    Read,
}

// Ignoring these HTTP conditions would serve an object without checking its preconditions.
const HTTP_PRECONDITION_HEADERS: &[&str] = &[
    "if-match",
    "if-none-match",
    "if-modified-since",
    "if-unmodified-since",
    "if-range",
];

// These headers have the same meaning on XML reads, PUT and resumable initiation.
fn is_common_object_header(name: &str) -> bool {
    crate::csek::is_key_header(name, crate::csek::DESTINATION_PREFIX)
        || matches!(
            name,
            "x-goog-if-generation-match"
                | "x-goog-if-metageneration-match"
                | "x-goog-api-client"
                | "x-goog-gcs-idempotency-token"
        )
}

/// Validates an XML object request's headers and returns their preconditions.
pub(crate) fn headers(
    headers: &HeaderMap,
    policy: HeaderPolicy,
) -> Result<ObjectConditions, Error> {
    if let Some(hash) = crate::auth::signature::payload_hash(headers)
        .map_err(|_| Error::new(XmlCode::InvalidDigest, "Invalid SHA256 declaration."))?
        && hash.as_slice() != ring::digest::digest(&ring::digest::SHA256, b"").as_ref()
    {
        return Err(Error::new(
            XmlCode::BadDigest,
            "The content hash does not match the empty read body.",
        ));
    }
    let mut tokens = headers.get_all("x-goog-gcs-idempotency-token").iter();
    if let Some(token) = tokens.next() {
        if tokens.next().is_some() {
            return Err(Error::new(
                XmlCode::ExcessHeaderValues,
                "Expected a single x-goog-gcs-idempotency-token header.",
            ));
        }
        if token.as_bytes().trim_ascii().is_empty() {
            return Err(Error::invalid(
                "Expected a non-empty x-goog-gcs-idempotency-token header.",
            ));
        }
    }
    // storage/v1.70.0 invoke.go:215-220 sends client telemetry and retry IDs;
    // http_client.go:1551-1587 uses them on idempotent reads, which need no token replay.
    for key in headers.keys() {
        let key = key.as_str();
        // Cloud Storage answered an object GET with an unknown `x-goog-` header as if it were
        // absent (recorded), so GET and HEAD ignore those (HEAD is assumed); DELETE was not
        // recorded and rejects them; the recognized ones are parsed by
        // their callers. Bucket operations give some `x-goog-` headers meaning (ACLs, project
        // billing) that this adapter does not implement, so they still reject unknown ones.
        // Headers of other protocols and the HTTP conditions carry semantics this adapter
        // does not implement, and ignoring them would be a false success.
        if (key.starts_with("x-goog-")
            && policy == HeaderPolicy::Read
            && RECOGNIZED_UNSUPPORTED_GOOG_HEADERS.contains(&key))
            || (key.starts_with("x-goog-")
                && policy == HeaderPolicy::Strict
                && !is_common_object_header(key)
                && !crate::auth::signature::authentication_header(key)
                && key != "x-goog-content-sha256")
            || (key.starts_with("x-amz-") && !crate::auth::signature::authentication_header(key))
            || (HTTP_PRECONDITION_HEADERS.contains(&key)
                && (policy == HeaderPolicy::Strict || key == "if-range"))
        {
            return Err(Error::unsupported(key));
        }
    }
    let mut conditions = ObjectConditions::default();
    for (key, target, min) in [
        (
            "x-goog-if-generation-match",
            &mut conditions.if_generation_match,
            MinValue::Zero,
        ),
        (
            "x-goog-if-metageneration-match",
            &mut conditions.if_metageneration_match,
            MinValue::One,
        ),
    ] {
        let mut values = headers.get_all(key).iter();
        if let Some(value) = values.next() {
            if values.next().is_some() {
                return Err(Error::new(
                    XmlCode::ExcessHeaderValues,
                    "Multiple precondition header values.",
                ));
            }
            *target = Some(number(
                value
                    .to_str()
                    .map_err(|_| Error::invalid("Invalid precondition header."))?,
                min,
            )?);
        }
    }
    Ok(conditions)
}

pub(crate) async fn empty_body<B>(mut body: B) -> Result<(), Error>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
{
    let unexpected = || {
        Error::new(
            XmlCode::UnexpectedContent,
            "This operation requires an empty body.",
        )
    };
    if body.size_hint().lower() > 0 {
        return Err(unexpected());
    }
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| Error::invalid("Could not read request body."))?;
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

pub(crate) fn object_etag(record: &ObjectRecord) -> String {
    // MD5 ETags apply only to objects without customer-supplied encryption. Other objects quote
    // the JSON validator, because XML entity tags are always quoted; Cloud Storage's XML ETag for
    // composite and encrypted objects has not been recorded.
    if !record.has_md5() || record.customer_key_sha256.is_some() {
        format!("\"{}\"", json::object_etag(record))
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
            // Character data needs no quote escapes; Cloud Storage writes `<ETag>"..."</ETag>`.
            _ => body.push(ch),
        }
    }
    body.push_str("</");
    body.push_str(name);
    body.push('>');
}

pub(crate) fn media_headers(record: &ObjectRecord, scope: CacheScope) -> Result<HeaderMap, Error> {
    let mut headers = json::object_media_headers(record)?;
    insert_hashes(&mut headers, record);
    // Only the XML API advertises ranges; the JSON media response omits the field.
    headers.insert("accept-ranges", HeaderValue::from_static("bytes"));
    let cache = record.cache_control.as_deref().unwrap_or(match scope {
        CacheScope::Public => "public, max-age=3600",
        CacheScope::Private => PRIVATE_CACHE_CONTROL,
    });
    let etag = object_etag(record);
    let date = json::http_date(record.updated)
        .map_err(|_| Error::new(XmlCode::ServiceUnavailable, "Invalid stored timestamp."))?;
    for (key, value) in [
        ("cache-control", cache),
        ("etag", &etag),
        ("last-modified", &date),
        ("x-goog-storage-class", record.storage_class.as_str()),
    ] {
        headers.insert(
            key,
            HeaderValue::from_str(value)
                .map_err(|_| Error::new(XmlCode::ServiceUnavailable, "Invalid stored header."))?,
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

/// Cloud Storage's XML API sends one `x-goog-hash` field per algorithm, always for the
/// whole object, including on a range (observed 2026-10-31). The JSON API instead combines
/// them in one field.
pub(crate) fn insert_hashes(headers: &mut HeaderMap, record: &ObjectRecord) {
    headers.remove("x-goog-hash");
    let crc32c = format!("crc32c={}", json::base64(&record.crc32c.to_be_bytes()));
    let md5 = record
        .has_md5()
        .then(|| format!("md5={}", json::base64(&record.md5)));
    // Base64 always forms a valid header value; omitting a hash is safer than panicking
    // if that ever changes.
    for value in std::iter::once(crc32c).chain(md5) {
        if let Ok(value) = HeaderValue::try_from(value) {
            headers.append("x-goog-hash", value);
        }
    }
}

pub(crate) fn not_modified(source: &HeaderMap) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    for name in ["cache-control", "etag", "expires", "vary", "last-modified"] {
        for value in source.get_all(name) {
            response.headers_mut().append(name, value.clone());
        }
    }
    response
}

/// Moves every value of the named fields, so a field sent once per algorithm keeps them all.
pub(crate) fn move_headers(from: &mut HeaderMap, to: &mut HeaderMap, names: &[&'static str]) {
    for name in names {
        if let hyper::header::Entry::Occupied(entry) = from.entry(*name) {
            let (key, values) = entry.remove_entry_mult();
            to.remove(&key);
            for value in values {
                to.append(key.clone(), value);
            }
        }
    }
}
