//! XML objects. Shared authorization, generation selection and bytes live in storage.

use crate::{
    config::Config,
    json::{self, JsonError},
    object::{ObjectConditions, ObjectRecord},
};
use http_body_util::Full;
use hyper::{
    HeaderMap, Method, Response, StatusCode, Uri,
    body::Bytes,
    header::{HeaderName, HeaderValue},
};
use std::time::UNIX_EPOCH;

mod cors;
pub(crate) use cors::{cors, warn_unconfigured_cors};
pub(crate) mod post;
pub(crate) mod put;

pub(crate) fn authenticate<'a>(
    auth: &'a crate::auth::Auth,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    clock: &dyn crate::clock::Clock,
) -> Result<Option<crate::auth::Caller<'a>>, Error> {
    if crate::auth::signature::is_signed(uri.query()) {
        auth.authenticate_signed(method, uri, headers, clock)
            .map_err(|failure| {
                use crate::auth::signature::Failure;
                let (status, code, message) = match failure {
                    Failure::Malformed => (
                        StatusCode::BAD_REQUEST,
                        "InvalidAuthentication",
                        "Invalid V2/V4 signing parameters; use exactly one signing version (for example, GoogleAccessId, Expires=1793090000 and Signature for V2).",
                    ),
                    Failure::UnknownSigner => (
                        StatusCode::FORBIDDEN,
                        "InvalidAccessKeyId",
                        "The signing identity or RSA public key is unknown; register GoogleAccessId and its RSA public_keys in the identity file (for example, local-admin@example.invalid). HMAC/GOOG1 keys are unsupported.",
                    ),
                    Failure::Mismatch => (
                        StatusCode::FORBIDDEN,
                        "SignatureDoesNotMatch",
                        "The request signature does not match.",
                    ),
                    Failure::Expired => (
                        StatusCode::BAD_REQUEST,
                        "ExpiredToken",
                        "The signed URL has expired.",
                    ),
                    Failure::NotYetValid => (
                        StatusCode::FORBIDDEN,
                        "RequestTimeTooSkewed",
                        "The signed URL is not yet usable.",
                    ),
                };
                Error::new(status, code, message)
            })
    } else {
        auth.authenticate(headers, uri.query())
            .map_err(|e| Error::from(JsonError::from(e)))
    }
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

impl Error {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn unsupported(name: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "InvalidArgument",
            format!(
                "gcs-mimic does not support XML API {name}; omit this option or use a supported operation (for example, GET /BUCKET/OBJECT without extra query parameters)."
            ),
        )
    }

    pub fn response(self, head: bool) -> Response<Full<Bytes>> {
        let escaped = self
            .message
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{}</Code><Message>{escaped}</Message></Error>",
            self.code
        );
        Response::builder()
            .status(self.status)
            .header("content-type", "application/xml; charset=UTF-8")
            .header("content-length", body.len())
            .body(Full::new(if head {
                Bytes::new()
            } else {
                Bytes::from(body)
            }))
            .unwrap()
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

fn legacy_public_authority(config: &Config) -> Option<&hyper::http::uri::Authority> {
    if config.compat_profile != crate::config::CompatProfile::LegacyEmulator {
        return None;
    }
    config
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
    config: &Config,
) -> (Option<String>, bool) {
    let host = authority.host().to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    let domain = config.xml_virtual_host_domains.iter().find(|domain| {
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
            bucket_for_authority(authority.as_str(), &config.xml_virtual_host_domains),
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
    pub fn name(&self) -> &str {
        &self.uri.path()[self.name_start..]
    }
}

/// Preserve the raw object path and reuse one routing decision for CORS and dispatch.
pub(crate) fn select_object(
    uri: &Uri,
    headers: &HeaderMap,
    config: &Config,
) -> Result<Option<ObjectPath>, Error> {
    let public = legacy_public_authority(config);
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
        (Some(authority), Some(public)) => legacy_virtual_bucket(authority, public, config),
        _ => (
            virtual_bucket(headers, &config.xml_virtual_host_domains),
            false,
        ),
    };
    if let Some(authority) = uri.authority()
        && (public.is_some()
            || virtual_bucket.is_some()
            || bucket_for_authority(authority.as_str(), &config.xml_virtual_host_domains).is_some())
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
            (bucket.to_owned(), name)
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
) -> Option<Result<ReadRequest, Error>> {
    let ObjectPath {
        bucket,
        uri,
        name_start,
    } = selected?;
    let name = &uri.path()[name_start..];
    Some((|| {
        json::validate_bucket_name_for_profile(&bucket, config.compat_profile)?;
        if !matches!(
            *method,
            Method::GET | Method::HEAD | Method::PUT | Method::DELETE
        ) || name.is_empty()
        {
            return Err(Error::unsupported(&format!(
                "{method} route; use GET, HEAD, PUT or DELETE on /BUCKET/OBJECT"
            )));
        }
        let name = json::decode(name, false)?;
        json::validate_object_name(&name)?;
        let mut query = json::parse_query(uri.query().unwrap_or_default())?;
        let v2 = query
            .keys()
            .any(|key| crate::auth::signature::V2_PARAMETERS.contains(&key.as_str()));
        if v2 && (name_start == 1 || *method == Method::HEAD || query.contains_key("generation")) {
            return Err(Error::unsupported(
                "V2 virtual-host, HEAD or generation selection; use path-style GET/PUT /BUCKET/OBJECT",
            ));
        }
        if *method == Method::PUT
            && let Some(key) = query.keys().find(|key| !signing_parameter(key))
        {
            return Err(Error::unsupported(key));
        }
        let generation = query
            .remove("generation")
            .map(|value| number(&value, false))
            .transpose()?;
        if *method == Method::DELETE && !query.is_empty() {
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
        || crate::auth::signature::V2_PARAMETERS.contains(&key)
}

fn number(raw: &str, zero: bool) -> Result<i64, Error> {
    raw.parse::<i64>()
        .ok()
        .filter(|n| *n >= i64::from(!zero))
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
    if let Some(hash) = headers.get("x-goog-content-sha256") {
        if headers.get_all("x-goog-content-sha256").iter().count() != 1 {
            return Err(Error::new(
                StatusCode::BAD_REQUEST,
                "ExcessHeaderValues",
                "Expected a single content hash.",
            ));
        }
        if hash != "UNSIGNED-PAYLOAD"
            && hash != "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        {
            return Err(Error::new(
                StatusCode::BAD_REQUEST,
                "BadDigest",
                "The content hash does not match the empty read body.",
            ));
        }
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
            && !matches!(
                key,
                "x-goog-if-generation-match"
                    | "x-goog-if-metageneration-match"
                    | "x-goog-api-client"
                    | "x-goog-gcs-idempotency-token"
                    | "x-goog-content-sha256"
            ))
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
    for (key, target, zero) in [
        (
            "x-goog-if-generation-match",
            &mut request.conditions.if_generation_match,
            true,
        ),
        (
            "x-goog-if-metageneration-match",
            &mut request.conditions.if_metageneration_match,
            false,
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
                zero,
            )?);
        }
    }
    if headers.contains_key("transfer-encoding")
        || headers.get("content-length").is_some_and(|v| v != "0")
    {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "UnexpectedContent",
            "This method requires an empty body.",
        ));
    }
    Ok(())
}

pub(crate) fn media_headers(
    record: &ObjectRecord,
    publicly_readable: bool,
) -> Result<HeaderMap, Error> {
    let mut headers = json::object_media_headers(record)?;
    let cache = record
        .cache_control
        .as_deref()
        .unwrap_or(if publicly_readable {
            "public, max-age=3600"
        } else {
            "private, max-age=0"
        });
    // MD5 ETags apply only to objects without customer-supplied encryption.
    let etag = if record.component_count.is_some() || record.customer_key_sha256.is_some() {
        json::object_etag(record)
    } else {
        format!(
            "\"{}\"",
            record
                .md5
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    };
    let timestamp = json::timestamp(record.updated)?;
    let days = record
        .updated
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            Error::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "ServiceUnavailable",
                "Invalid stored timestamp.",
            )
        })?
        .as_secs()
        / 86400;
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][(days % 7) as usize];
    let month: usize = timestamp[5..7].parse().expect("validated timestamp month");
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][month - 1];
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
