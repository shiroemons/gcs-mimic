use crate::{auth::AuthFailure, bucket::CatalogError, object::ObjectError};
use http_body_util::Full;
use hyper::{Response, StatusCode, body::Bytes, header};
use serde_json::{Value, json};
use std::fmt;

/// The `Cache-Control` Cloud Storage sends with JSON API errors and media downloads.
pub(crate) const NO_STORE_CACHE_CONTROL: &str = "no-cache, no-store, max-age=0, must-revalidate";

/// The `Cache-Control` of a media download that Cloud Storage answers with a plain error; its
/// `Expires` is the response time.
pub(crate) const MEDIA_ERROR_CACHE_CONTROL: &str = "private, max-age=0";

/// The missing-object message for operations that do not name one object, such as a copy source.
const OBJECT_NOT_FOUND: &str = "Object not found";
/// Cloud Storage's message for a bucket that does not exist, shared by every bucket-scoped route.
pub(crate) const BUCKET_NOT_FOUND: &str = "The specified bucket does not exist.";
/// The message of a failed metageneration or entity-tag precondition on a bucket or policy.
pub(crate) const PRECONDITION_NOT_HELD: &str =
    "At least one of the pre-conditions you specified did not hold.";
/// The prefix of Cloud Storage's missing-object message, followed by `<bucket>/<object>`.
const NO_SUCH_OBJECT: &str = "No such object: ";

/// Where Cloud Storage's parser expected the `:` after the first key and what it said.
/// The first key may be bare, as in the recorded `{not json` and `oops` inputs.
fn missing_colon(text: &str) -> Option<(String, String, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let skip_space = |mut index: usize| {
        while chars.get(index).is_some_and(|c| c.is_whitespace()) {
            index += 1;
        }
        index
    };
    let mut index = skip_space(0);
    if chars.get(index) == Some(&'{') {
        index = skip_space(index + 1);
    }
    let key_start = index;
    if chars.get(index) == Some(&'"') {
        index += 1;
        while chars.get(index).is_some_and(|c| *c != '"') {
            index += 1;
        }
        index += 1;
    } else {
        while chars
            .get(index)
            .is_some_and(|c| !c.is_whitespace() && *c != ':')
        {
            index += 1;
        }
    }
    let index = skip_space(index);
    match chars.get(index) {
        Some(':') => None,
        Some(_) => Some((
            "Expected : between key:value pair.".to_owned(),
            text.to_owned(),
            index,
        )),
        // At the end of the input the quoted context starts at the key (recorded for `{oops`).
        None => Some((
            "Unexpected end of string. Expected : between key:value pair.".to_owned(),
            chars[key_start..].iter().collect(),
            chars.len() - key_start,
        )),
    }
}

/// Escapes text for an HTML document. The object name in a media 404 is client-controlled and
/// the response is `text/html`, so every markup-significant character must be neutralized.
fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    pub(crate) kind: ErrorKind,
    pub status: StatusCode,
    pub reason: &'static str,
    pub message: String,
    pub allow: Option<&'static str>,
    /// The `locationType`/`location` pair of the error item, if any. A reference keeps the error
    /// small, which `Result` returns throughout the crate depend on.
    pub location: Option<&'static ErrorLocation>,
    /// Cloud Storage answered some failures with a bare text page instead of a JSON envelope.
    pub(crate) plain: Option<&'static PlainText>,
    /// Cloud Storage's upload front end sent some JSON error envelopes as `text/html`.
    pub(crate) html_typed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    MissingBucket,
    MissingObject,
    NamedMissingObject,
    Required,
    RetentionNotMet,
    ActiveHold,
    Csek(crate::csek::CsekError),
    Unsupported,
    CancelledUpload,
    IncompleteBody,
    InvalidArgument,
    /// Parse failures whose wire reason the legacy bucket adapter changes to `invalid`.
    LegacyParseError,
    MalformedJson,
    FieldMaskSyntax,
    PreconditionFailed,
    NotModified,
    Other,
}

/// A bare text error page: the front end's answer to a request no JSON handler claims.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PlainText {
    pub(crate) content_type: &'static str,
    /// The page text; `None` sends the error's own message.
    pub(crate) body: Option<&'static str>,
    /// Whether the page carries the JSON API's no-store cache headers.
    pub(crate) cache: PlainCache,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PlainCache {
    NoStore,
    Unspecified,
}

/// `POST` to an object resource, which has no such method.
pub(crate) const NOT_FOUND_HTML: PlainText = PlainText {
    content_type: "text/html; charset=UTF-8",
    body: Some("Not Found"),
    cache: PlainCache::NoStore,
};

/// An upload request that names no session the server knows.
pub(crate) const NOT_FOUND_TEXT: PlainText = PlainText {
    content_type: "text/plain; charset=utf-8",
    body: Some("Not Found"),
    cache: PlainCache::Unspecified,
};

/// A multipart upload whose `Content-Type` carries no boundary.
pub(crate) const BAD_CONTENT_TYPE_TEXT: PlainText = PlainText {
    content_type: "text/plain; charset=utf-8",
    body: Some("Bad content type.  Please use multipart."),
    cache: PlainCache::Unspecified,
};

/// A resumable chunk whose `Content-Range` Cloud Storage could not use.
pub(crate) const CONTENT_RANGE_TEXT: PlainText = PlainText {
    content_type: "text/plain; charset=utf-8",
    body: Some("Failed to parse Content-Range header."),
    cache: PlainCache::Unspecified,
};

/// A session-protocol failure that has no recorded text of its own. The recorded failures
/// of a continuation (an unusable `Content-Range`, an unknown session) are bare `text/plain`
/// pages without cache fields, so the other protocol errors (an inconsistent total, a gap in
/// the offsets, a finished or cancelled session) use the same form with the message they
/// already had. The form is inferred; only the two wordings above were recorded.
pub(crate) const SESSION_MESSAGE_TEXT: PlainText = PlainText {
    content_type: "text/plain; charset=utf-8",
    body: None,
    cache: PlainCache::Unspecified,
};

/// An upload request whose `uploadType` is not one of the three.
pub(crate) const BAD_REQUEST_TEXT: PlainText = PlainText {
    content_type: "text/plain; charset=utf-8",
    body: Some("Bad Request"),
    cache: PlainCache::Unspecified,
};

/// Where a rejected value was sent: `locationType` and `location` of an error item.
#[derive(Debug, PartialEq, Eq)]
pub struct ErrorLocation {
    pub kind: &'static str,
    pub name: &'static str,
}

pub(crate) const AUTHORIZATION_HEADER: ErrorLocation = ErrorLocation {
    kind: "header",
    name: "Authorization",
};
pub(crate) const IF_MATCH_HEADER: ErrorLocation = ErrorLocation {
    kind: "header",
    name: "If-Match",
};
pub(crate) const FIELDS_PARAMETER: ErrorLocation = ErrorLocation {
    kind: "parameter",
    name: "fields",
};

/// Const constructor for the query-parameter entries of [`PARAMETERS`].
const fn parameter_location(name: &'static str) -> ErrorLocation {
    ErrorLocation {
        kind: "parameter",
        name,
    }
}

/// The JSON API query parameters whose rejection Cloud Storage reports with a `parameter`
/// location. A fixed table lets the error keep a `&'static` reference to the location instead of
/// owning the client-supplied name.
static PARAMETERS: [ErrorLocation; 30] = [
    parameter_location("alt"),
    parameter_location("fields"),
    parameter_location("generation"),
    parameter_location("ifGenerationMatch"),
    parameter_location("ifGenerationNotMatch"),
    parameter_location("ifMetagenerationMatch"),
    parameter_location("ifMetagenerationNotMatch"),
    parameter_location("ifSourceGenerationMatch"),
    parameter_location("ifSourceGenerationNotMatch"),
    parameter_location("ifSourceMetagenerationMatch"),
    parameter_location("ifSourceMetagenerationNotMatch"),
    parameter_location("maxResults"),
    parameter_location("pageToken"),
    parameter_location("permissions"),
    parameter_location("predefinedAcl"),
    parameter_location("destinationPredefinedAcl"),
    parameter_location("predefinedDefaultObjectAcl"),
    parameter_location("project"),
    parameter_location("projection"),
    parameter_location("serviceAccountEmail"),
    parameter_location("showDeletedKeys"),
    parameter_location("softDeleted"),
    parameter_location("sourceGeneration"),
    parameter_location("userProject"),
    parameter_location("delimiter"),
    parameter_location("prefix"),
    parameter_location("startOffset"),
    parameter_location("endOffset"),
    parameter_location("versions"),
    parameter_location("kmsKeyName"),
];

impl ErrorLocation {
    /// The `parameter` location for a query parameter name, if it is one the API names.
    pub(crate) fn parameter(name: &str) -> Option<&'static ErrorLocation> {
        PARAMETERS.iter().find(|location| location.name == name)
    }
}

impl JsonError {
    fn storage_capacity() -> Self {
        Self::service_unavailable(
            "Storage capacity exhausted; for backend=memory release objects/readers or restart with larger memory_max_bytes and memory_catalog_max_bytes (restart loses RAM data), and for backend=disk free disk space; data kept by soft delete also uses capacity, so lower default_soft_delete_retention_seconds to 0 or set softDeletePolicy.retentionDurationSeconds to 0 on the bucket to free deleted data at once; retry with generation preconditions after capacity is available",
        )
    }

    fn backend(message: &str) -> Self {
        // Only fixed diagnostics cross the HTTP boundary; arbitrary backend text can contain paths or secrets.
        let cause = message
            .strip_prefix("catalog unavailable: ")
            .unwrap_or(message);
        let message = match cause {
            "catalog is closed" => {
                "Object storage is unavailable: catalog is closed; wait for the server to restart before retrying"
            }
            "catalog operation limit reached" => {
                "Object storage is unavailable: catalog operation limit reached; retry with fewer concurrent requests (for example, 16)"
            }
            "upload catalog wait timed out" => {
                "Object storage is unavailable: upload catalog wait timed out; retry with generation preconditions or increase catalog_wait_timeout_ms (for example, 60000); a completed upload may still commit"
            }
            "retained upload session limit reached" => {
                "Object storage is unavailable: retained upload session limit reached; wait for expired sessions to be reclaimed or use single-request media uploads"
            }
            "notification outbox limit reached" => {
                "Object storage is unavailable: notification outbox limit reached; start or repair the configured local Pub/Sub emulator, or set pubsub_emulator_host to resume delivery, then retry with generation preconditions"
            }
            "upload finalization timed out" => {
                "Object storage is unavailable: upload finalization timed out; read object state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000)"
            }
            "copy request deadline exceeded" => {
                "Object storage is unavailable: copy request deadline exceeded; read destination state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000)"
            }
            "upload session is busy" => {
                "Object storage is unavailable: upload session is busy; wait for the current request to finish before sending another request for this session"
            }
            _ => {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Error,
                    "object_backend_failure",
                    json!({"cause": safe_backend_cause(cause)}),
                );
                "Object storage is unavailable: internal storage failure (private backend details withheld)"
            }
        };
        Self::service_unavailable(message)
    }

    /// An unclassified protocol error; use a named constructor for a semantic error kind.
    pub(crate) fn new(
        status: StatusCode,
        reason: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: ErrorKind::Other,
            status,
            reason,
            message: message.into(),
            allow: None,
            location: None,
            plain: None,
            html_typed: false,
        }
    }

    fn with_kind(mut self, kind: ErrorKind) -> Self {
        self.kind = kind;
        self
    }

    fn missing_bucket() -> Self {
        Self::new(StatusCode::NOT_FOUND, "notFound", BUCKET_NOT_FOUND)
            .with_kind(ErrorKind::MissingBucket)
    }

    fn missing_object() -> Self {
        Self::new(StatusCode::NOT_FOUND, "notFound", OBJECT_NOT_FOUND)
            .with_kind(ErrorKind::MissingObject)
    }

    fn named_missing_object(bucket: &str, name: &str) -> Self {
        Self::missing_object().naming_object(bucket, name)
    }

    fn precondition_not_held() -> Self {
        Self::new(
            StatusCode::PRECONDITION_FAILED,
            "conditionNotMet",
            PRECONDITION_NOT_HELD,
        )
        .with_kind(ErrorKind::PreconditionFailed)
    }

    pub(crate) fn not_modified() -> Self {
        Self::new(StatusCode::NOT_MODIFIED, "notModified", "Not modified")
            .with_kind(ErrorKind::NotModified)
    }

    pub(crate) fn legacy_parse_error(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "parseError", message)
            .with_kind(ErrorKind::LegacyParseError)
    }

    pub(crate) fn unsupported_message(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "badRequest", message).with_kind(ErrorKind::Unsupported)
    }

    pub(crate) fn field_mask_syntax(message: impl Into<String>) -> Self {
        Self::invalid_parameter(message)
            .with_kind(ErrorKind::FieldMaskSyntax)
            .at_parameter("fields")
    }

    /// Names the rejected query parameter in the error item (`locationType: parameter`). A name
    /// outside the API's parameter table leaves the error without a location.
    pub(crate) fn at_parameter(mut self, name: &str) -> Self {
        self.location = ErrorLocation::parameter(name);
        self
    }

    /// Names the `If-Match` header: Cloud Storage reports every failed precondition there, even
    /// when the condition was a query parameter.
    pub(crate) fn at_if_match(mut self) -> Self {
        self.location = Some(&IF_MATCH_HEADER);
        self
    }

    pub(crate) fn invalid_parameter(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalidParameter", message)
    }

    pub(crate) fn required(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "required", message).with_kind(ErrorKind::Required)
    }

    pub(crate) fn method_not_allowed(
        message: impl Into<String>,
        allow: Option<&'static str>,
    ) -> Self {
        let mut error = Self::new(StatusCode::METHOD_NOT_ALLOWED, "methodNotAllowed", message);
        error.allow = allow;
        error
    }

    pub(crate) fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "backendError", message)
    }

    pub(crate) fn invalid_stored_header() -> Self {
        Self::service_unavailable("Invalid stored object response header")
    }

    /// A numeric query parameter Cloud Storage could not parse as a signed 64-bit integer.
    pub(crate) fn invalid_long(value: &str) -> Self {
        Self::invalid_parameter(format!("Invalid long value: '{value}'."))
    }

    /// A numeric query parameter Cloud Storage could not parse as an unsigned integer.
    pub(crate) fn invalid_unsigned(value: &str) -> Self {
        Self::invalid_parameter(format!("Invalid unsigned integer value: '{value}'."))
    }

    /// A request body that is not JSON. Cloud Storage quotes the offending input with a caret
    /// under the position where its lenient parser stopped, which the recorded object-resource
    /// failures show for a missing `:` after the first key.
    pub(crate) fn parse_error(input: &[u8], cause: &serde_json::Error) -> Self {
        let text = String::from_utf8_lossy(input);
        let (reason, shown, column) = missing_colon(&text).unwrap_or_else(|| {
            // A body that ends inside a value is reported with an empty context line (recorded
            // 2026-10-31 for bucket bodies truncated after a key).
            if cause.is_eof() {
                return (
                    "Unexpected end of string. Expected a value.".to_owned(),
                    String::new(),
                    0,
                );
            }
            (
                cause
                    .to_string()
                    .split(" at line ")
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                text.to_string(),
                cause.column().saturating_sub(1),
            )
        });
        Self::invalid(format!(
            "Parse Error: {reason}\n{shown}\n{}^",
            " ".repeat(column)
        ))
        .with_kind(ErrorKind::MalformedJson)
    }

    /// The same JSON envelope, labelled `text/html` as the upload front end did for request
    /// problems it found before reaching the storage handler.
    pub(crate) fn html_typed(mut self) -> Self {
        self.html_typed = true;
        self
    }

    /// A session-protocol failure (a `Content-Range` that cannot be used, a gap or total that
    /// disagrees with the session): the upload front end answered these with a bare `text/plain`
    /// page, so the message leaves as that page with no error `reason`. Failures of other kinds
    /// (checksums, headers, preconditions) keep their JSON envelope, because SDKs read the
    /// `reason` from it.
    pub(crate) fn session_protocol(message: impl Into<String>) -> Self {
        let mut error = Self::invalid_parameter(message);
        error.plain = Some(&SESSION_MESSAGE_TEXT);
        error
    }

    /// An upload that names no object.
    pub(crate) fn missing_object_name() -> Self {
        Self::required("You must specify an object name.").html_typed()
    }

    /// A rejected value in the request body or a bucket setting. Cloud Storage reports these as
    /// `invalid` (recorded 2026-10-31), where `invalidArgument` is a gRPC-style name.
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid", message)
    }

    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalidArgument", message)
            .with_kind(ErrorKind::InvalidArgument)
    }

    pub(crate) fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", "Forbidden")
    }

    pub fn unsupported(name: &str) -> Self {
        Self::unsupported_message(format!(
            "gcs-mimic does not support bucket API setting {name}; omit this setting (for example, create a bucket with only name)."
        ))
    }

    pub fn unsupported_object(name: &str) -> Self {
        Self::unsupported_message(format!(
            "gcs-mimic does not support object API setting {name}; omit this option or header (for example, use projection=noAcl)."
        ))
    }

    pub(crate) fn value(&self) -> Value {
        let mut item = json!({"message": self.message, "domain": "global", "reason": self.reason});
        if let Some(location) = self.location {
            item["locationType"] = location.kind.into();
            item["location"] = location.name.into();
        }
        json!({"error": {"code": self.status.as_u16(), "message": self.message, "errors": [item]}})
    }

    /// Whether this is a missing-bucket error, in either the bucket or the object wording.
    pub(crate) fn is_missing_bucket(&self) -> bool {
        self.kind == ErrorKind::MissingBucket
    }

    /// Whether this is the catalog's missing-object error, before or after it names the object.
    pub(crate) fn is_missing_object(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::MissingObject | ErrorKind::NamedMissingObject
        )
    }

    /// Cloud Storage names the requested object in its not-found message, which the catalog
    /// error cannot do because it does not know which request selected the object.
    pub(crate) fn naming_object(mut self, bucket: &str, name: &str) -> Self {
        // An error that already names its object, such as a copy source, keeps that name.
        if self.kind == ErrorKind::MissingObject {
            self.message = format!("{NO_SUCH_OBJECT}{bucket}/{name}");
            self.kind = ErrorKind::NamedMissingObject;
        }
        self
    }

    /// A media download's error. Cloud Storage answers a missing object and an unsatisfiable
    /// range with an HTML-typed plain message and `private, max-age=0`; only these two were
    /// recorded, so other media errors keep the JSON form.
    pub(crate) fn into_media_response(self) -> Response<Full<Bytes>> {
        if !self.is_missing_object() && self.status != StatusCode::RANGE_NOT_SATISFIABLE {
            return self.into_response();
        }
        // The range refusal has its own short wording, unlike the JSON form.
        let text = if self.status == StatusCode::RANGE_NOT_SATISFIABLE {
            "Request range not satisfiable".to_owned()
        } else {
            escape_html(&self.message)
        };
        let mut response = Response::new(Full::new(Bytes::from(text)));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/html; charset=UTF-8"),
        );
        headers.insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static(MEDIA_ERROR_CACHE_CONTROL),
        );
        response
    }

    pub(crate) fn into_response(self) -> Response<Full<Bytes>> {
        if self.status == StatusCode::NOT_MODIFIED {
            let mut response = Response::new(Full::new(Bytes::new()));
            *response.status_mut() = self.status;
            return response;
        }
        if let Some(plain) = self.plain {
            let body = plain.body.map_or_else(
                || Bytes::from(self.message.clone()),
                |text| Bytes::from_static(text.as_bytes()),
            );
            let mut response = Response::new(Full::new(body));
            *response.status_mut() = self.status;
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static(plain.content_type),
            );
            if plain.cache == PlainCache::NoStore {
                headers.insert(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static(NO_STORE_CACHE_CONTROL),
                );
                headers.insert(header::PRAGMA, header::HeaderValue::from_static("no-cache"));
            }
            return response;
        }
        let mut text = super::render_body(&self.value());
        // The upload front end ended its `text/html` envelopes with a newline (recorded).
        if self.html_typed {
            // The upload front end sends no nosniff header; JSON escapes keep request input out of HTML markup.
            text = text
                .replace('&', "\\u0026")
                .replace('<', "\\u003c")
                .replace('>', "\\u003e");
            text.push('\n');
        }
        let mut response = Response::new(Full::new(Bytes::from(text)));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static(if self.html_typed {
                "text/html; charset=UTF-8"
            } else {
                "application/json; charset=UTF-8"
            }),
        );
        // Recorded Cloud Storage JSON errors forbid every cache, HTTP/1.0 ones included.
        headers.insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static(NO_STORE_CACHE_CONTROL),
        );
        headers.insert(header::PRAGMA, header::HeaderValue::from_static("no-cache"));
        if let Some(allow) = self.allow {
            response
                .headers_mut()
                .insert(header::ALLOW, header::HeaderValue::from_static(allow));
        }
        response
    }
}

/// A refused `matchGlob` pattern is a malformed request; the pattern defect is the message.
impl From<crate::match_glob::GlobError> for JsonError {
    fn from(error: crate::match_glob::GlobError) -> Self {
        Self::invalid(error.to_string())
    }
}

/// A URL that cannot be built becomes the error body text; `UrlError` names the defect and
/// has no underlying cause to keep.
impl From<crate::url::UrlError> for JsonError {
    fn from(error: crate::url::UrlError) -> Self {
        Self::invalid_parameter(error.to_string())
    }
}

impl From<AuthFailure> for JsonError {
    fn from(failure: AuthFailure) -> Self {
        match failure {
            // Documented status-code example for a missing credential.
            AuthFailure::LoginRequired => {
                let mut error = Self::new(StatusCode::UNAUTHORIZED, "required", "Login Required")
                    .with_kind(ErrorKind::Required);
                error.location = Some(&AUTHORIZATION_HEADER);
                error
            }
            // The reason is documented; message and location are a local choice.
            AuthFailure::InvalidCredentials => {
                let mut error =
                    Self::new(StatusCode::UNAUTHORIZED, "authError", "Invalid Credentials");
                error.location = Some(&AUTHORIZATION_HEADER);
                error
            }
            // The service's longer "<principal> does not have <permission> access" text is
            // undocumented, so the documented example message is kept.
            AuthFailure::Forbidden => Self::forbidden(),
        }
    }
}

impl From<crate::csek::CsekError> for JsonError {
    fn from(error: crate::csek::CsekError) -> Self {
        Self::new(StatusCode::BAD_REQUEST, error.reason(), error.message())
            .with_kind(ErrorKind::Csek(error))
    }
}

impl From<CatalogError> for JsonError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::AlreadyExists => Self::new(
                StatusCode::CONFLICT,
                "conflict",
                "Your previous request to create the named bucket succeeded and you already own it.",
            ),
            CatalogError::NotFound => Self::missing_bucket(),
            // The wording is a local choice; only the "you already own it" text of a create
            // conflict was recorded.
            CatalogError::RestoreConflict => Self::new(
                StatusCode::CONFLICT,
                "conflict",
                "A bucket with this name already exists, so the soft-deleted bucket cannot be restored.",
            ),
            CatalogError::NotEmpty => {
                Self::new(StatusCode::CONFLICT, "conflict", "Bucket is not empty")
            }
            CatalogError::PreconditionFailed => Self::precondition_not_held(),
            CatalogError::Forbidden => Self::forbidden(),
            error @ CatalogError::RetentionPolicyLocked(_) => Self::new(
                StatusCode::BAD_REQUEST,
                "badRequestException",
                error.to_string(),
            ),
            error @ CatalogError::RetentionPolicyNotSet => {
                Self::new(StatusCode::BAD_REQUEST, "badRequest", error.to_string())
            }
            CatalogError::PublicAccessPrevention => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "conditionNotMet",
                "Public access prevention is enforced",
            ),
            CatalogError::InvalidMetadata(reason) => Self::invalid(reason),
            CatalogError::UnsupportedMetadata(field) => Self::unsupported(&field),
            CatalogError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull => {
                Self::storage_capacity()
            }
            CatalogError::Unavailable(_)
            | CatalogError::AuthenticationAlreadyConfigured
            | CatalogError::Io(_)
            | CatalogError::Corrupt(_)
            | CatalogError::NewerSchema { .. } => {
                Self::service_unavailable("Bucket catalog is unavailable")
            }
        }
    }
}

impl From<crate::storage::ResumableError> for JsonError {
    fn from(error: crate::storage::ResumableError) -> Self {
        use crate::storage::ResumableError;
        match error {
            ResumableError::NotFound => {
                let mut error = Self::new(
                    StatusCode::NOT_FOUND,
                    "notFound",
                    "Upload session not found",
                );
                error.plain = Some(&NOT_FOUND_TEXT);
                error
            }
            // Unrecorded terminal validation failures keep the existing local 410 response.
            ResumableError::Gone => {
                let mut error = Self::new(
                    StatusCode::GONE,
                    "gone",
                    "Upload session is no longer active",
                );
                error.plain = Some(&SESSION_MESSAGE_TEXT);
                error
            }
            ResumableError::Invalid(message) => Self::invalid_parameter(message),
            ResumableError::Cancelled => Self::cancelled_upload(),
            ResumableError::InvalidContentRange => {
                let mut error = Self::invalid_parameter("Failed to parse Content-Range header.");
                error.plain = Some(&CONTENT_RANGE_TEXT);
                error
            }
            ResumableError::Object(error) => error.into(),
        }
    }
}

impl JsonError {
    #[expect(
        clippy::expect_used,
        reason = "499 lies within the 100..=999 range StatusCode accepts, and from_u16 is not const"
    )]
    fn cancelled_upload() -> Self {
        Self::new(
            StatusCode::from_u16(499).expect("499 is a valid status code"),
            "clientClosedRequest",
            "clientClosedRequest",
        )
        .with_kind(ErrorKind::CancelledUpload)
    }
}

/// A compose component's failure, in the wording Cloud Storage used for it.
impl From<Box<crate::object::ComponentFailure>> for JsonError {
    fn from(failure: Box<crate::object::ComponentFailure>) -> Self {
        use crate::object::ComponentFailureKind;
        match failure.kind {
            ComponentFailureKind::NotFound => {
                Self::named_missing_object(&failure.bucket, &failure.name)
            }
            ComponentFailureKind::GenerationMismatch { required, actual } => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "conditionNotMet",
                format!(
                    "Generation {required} required for component object ({}/{}), but actual generation is {actual}.",
                    failure.bucket, failure.name
                ),
            ),
        }
    }
}

impl From<ObjectError> for JsonError {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::BucketNotFound => Self::missing_bucket(),
            ObjectError::Component(failure) => failure.into(),
            ObjectError::InvalidMetadata => Self::invalid_argument("Custom metadata exceeds 8 KiB"),
            ObjectError::InvalidDerivedArgument(reason) => Self::invalid_argument(reason),
            ObjectError::DerivedTransferIncomplete => {
                Self::invalid_argument("Derived transfer is incomplete")
            }
            ObjectError::DerivedTransferNotFound => Self::new(
                StatusCode::NOT_FOUND,
                "notFound",
                "Derived transfer not found",
            ),
            ObjectError::DerivedTransferExpired => {
                Self::new(StatusCode::GONE, "gone", "Derived transfer expired")
            }
            ObjectError::DerivedTransferBusy => {
                Self::service_unavailable("Derived transfer is already in use")
            }
            ObjectError::UnsupportedMetadata(field) => Self::unsupported_object(&field),
            ObjectError::CustomerEncryption(error) => error.into(),
            ObjectError::NotModified => Self::not_modified(),
            ObjectError::ObjectNotFound => Self::missing_object(),
            ObjectError::ObjectNotSoftDeleted => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "objectNotSoftDeleted",
                error.to_string(),
            ),
            ObjectError::PreconditionFailed => Self::precondition_not_held().at_if_match(),
            ObjectError::OverwriteForbidden | ObjectError::Forbidden => Self::forbidden(),
            ObjectError::UnlockedRetentionOverrideRequired => Self::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                crate::object::UNLOCKED_RETENTION_OVERRIDE_MESSAGE,
            ),
            ObjectError::LockedRetention => Self::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                crate::object::LOCKED_RETENTION_MESSAGE,
            ),
            ObjectError::ActiveHold(held) => {
                let kind = match held.kind {
                    crate::object::HoldKind::Temporary => "Temporary",
                    crate::object::HoldKind::EventBased => "Event-Based",
                };
                Self::new(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    format!(
                        "Object '{}/{}' is under active {kind} hold and cannot be deleted, overwritten or archived until hold is removed.",
                        held.bucket, held.name
                    ),
                ).with_kind(ErrorKind::ActiveHold)
            }
            // Bucket retention policies and object retention share the reason and wording
            // (observed 2026-10-31, including the refused overwrite).
            ObjectError::ObjectRetentionNotMet(retained) => Self::new(
                StatusCode::FORBIDDEN,
                "retentionPolicyNotMet",
                format!(
                    "Object '{}/{}' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until {}",
                    retained.bucket,
                    retained.name,
                    crate::object::retention::pacific_rfc3339(retained.until)
                ),
            ).with_kind(ErrorKind::RetentionNotMet),
            ObjectError::HashMismatch(mismatch) => {
                let (label, noun) = match mismatch.algorithm {
                    crate::object::HashAlgorithm::Crc32c => ("CRC32C", "CRC32C"),
                    crate::object::HashAlgorithm::Md5 => ("MD5 hash", "MD5 hash"),
                };
                Self::invalid(format!(
                    "Provided {label} \"{}\" doesn't match calculated {noun} \"{}\".",
                    super::base64(&mismatch.provided),
                    super::base64(&mismatch.calculated)
                ))
            }
            ObjectError::ChecksumMismatch => Self::new(
                StatusCode::BAD_REQUEST,
                "invalid",
                "The supplied checksum does not match the uploaded data",
            ),
            ObjectError::TooLarge => Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "uploadTooLarge",
                "Object exceeds the upload size limit",
            ),
            ObjectError::BodyAborted(_) => Self::new(
                StatusCode::BAD_REQUEST,
                "badRequest",
                "The upload request body did not complete",
            ).with_kind(ErrorKind::IncompleteBody),
            ObjectError::InvalidFixedMetadata(reason) => Self::invalid(reason),
            ObjectError::PublicAccessPrevention => CatalogError::PublicAccessPrevention.into(),
            ObjectError::Unavailable(cause) => Self::backend(cause.message()),
            ObjectError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull => {
                Self::storage_capacity()
            }
            ObjectError::Io(kind) => Self::service_unavailable(format!(
                "Object storage is unavailable: storage I/O error ({kind}); check data directory permissions and available disk space"
            )),
            ObjectError::Corrupt(_) => Self::service_unavailable(
                "Object storage is unavailable: stored data is corrupt; restore a valid data directory",
            ),
        }
    }
}

pub(crate) fn safe_backend_cause(cause: &str) -> &'static str {
    // Only known fixed causes are safe; backend strings may embed credentials or SQL values.
    const SAFE_CAUSES: &[&str] = &[
        "database is locked",
        "database is busy",
        "database or disk is full",
        "disk I/O error",
        "attempt to write a readonly database",
        "Permission denied",
        "No space left on device",
        "No such file or directory",
        "Acknowledged upload prefix is missing",
        "invalid resumable catalog state",
        "invalid opaque blob ID",
        "deletion journal references a live blob",
        "generation exhausted",
        "metageneration exhausted",
        "bucket metageneration exhausted",
        "derived transfer limit reached",
        "derived publication catalog admission timed out",
        "rewrite session limit reached",
        "catalog is closing",
    ];
    SAFE_CAUSES
        .iter()
        .copied()
        .find(|safe| {
            cause == *safe
                || cause
                    .strip_prefix(safe)
                    .is_some_and(|tail| tail.starts_with(": ") || tail.starts_with(" (os error "))
        })
        .unwrap_or("unrecognized backend failure (private details withheld)")
}

impl fmt::Display for JsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.status, self.message)
    }
}

impl std::error::Error for JsonError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_classification_does_not_depend_on_the_public_wording() {
        let mut bucket = JsonError::from(CatalogError::NotFound);
        bucket.message = "different bucket wording".into();
        bucket.reason = "differentReason";
        assert!(bucket.is_missing_bucket());
        let mut object = JsonError::from(ObjectError::ObjectNotFound).naming_object("b", "o");
        assert_eq!(object.message, "No such object: b/o");
        object.message = "different object wording".into();
        assert!(object.is_missing_object());
        assert_eq!(
            object.naming_object("other", "other").message,
            "different object wording"
        );
        let unrelated = JsonError::invalid_parameter(OBJECT_NOT_FOUND);
        assert!(!unrelated.is_missing_object());
    }

    #[test]
    fn json_parse_errors_quote_the_input_with_a_caret_like_cloud_storage() {
        let parse = |input: &str| {
            let cause = serde_json::from_slice::<Value>(input.as_bytes()).unwrap_err();
            JsonError::parse_error(input.as_bytes(), &cause)
        };
        let error = parse("{not json");
        assert_eq!(error.kind, ErrorKind::MalformedJson);
        assert_eq!((error.status.as_u16(), error.reason), (400, "invalid"));
        assert_eq!(
            error.message,
            "Parse Error: Expected : between key:value pair.\n{not json\n     ^"
        );
        assert_eq!(
            parse("{oops").message,
            "Parse Error: Unexpected end of string. Expected : between key:value pair.\noops\n    ^"
        );
    }

    #[test]
    fn holds_and_compose_components_are_named_in_their_errors() {
        use crate::object::{ComponentFailure, ComponentFailureKind, HeldObject, HoldKind};
        let held = |kind| {
            JsonError::from(ObjectError::ActiveHold(Box::new(HeldObject {
                bucket: "b".into(),
                name: "o".into(),
                kind,
            })))
        };
        let error = held(HoldKind::EventBased);
        assert_eq!((error.status.as_u16(), error.reason), (403, "forbidden"));
        assert_eq!(
            error.message,
            "Object 'b/o' is under active Event-Based hold and cannot be deleted, overwritten or archived until hold is removed."
        );
        assert_eq!(error.kind, ErrorKind::ActiveHold);
        let component = |kind| {
            JsonError::from(ObjectError::Component(Box::new(ComponentFailure {
                bucket: "b".into(),
                name: "a".into(),
                kind,
            })))
        };
        assert_eq!(
            component(ComponentFailureKind::NotFound).message,
            "No such object: b/a"
        );
        let mismatch = component(ComponentFailureKind::GenerationMismatch {
            required: 1,
            actual: 7,
        });
        assert_eq!(mismatch.status.as_u16(), 412);
        assert_eq!(
            mismatch.message,
            "Generation 1 required for component object (b/a), but actual generation is 7."
        );
    }

    #[test]
    fn backend_causes_are_actionable_and_private_details_stay_private() {
        if std::env::var_os("GCS_MIMIC_TEST_BACKEND_LOG").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "json::error::tests::backend_causes_are_actionable_and_private_details_stay_private",
                    "--nocapture",
                ])
                .env("GCS_MIMIC_TEST_BACKEND_LOG", "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(!stderr.contains("/secret"));
            assert!(!stderr.contains("token="));
            let events: Vec<Value> = stderr
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(events.len(), 6);
            assert!(events.iter().all(|event| {
                event["event"] == "object_backend_failure" && event["level"] == "error"
            }));
            assert_eq!(events[1]["cause"], "database is locked");
            assert_eq!(events[2]["cause"], "Permission denied");
            return;
        }
        for cause in [
            "catalog is closed",
            "catalog operation limit reached",
            "upload catalog wait timed out",
            "retained upload session limit reached",
            "upload finalization timed out",
            "copy request deadline exceeded",
            "upload session is busy",
        ] {
            let error = JsonError::from(ObjectError::Unavailable(cause.into()));
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(error.message.contains(cause));
        }
        let error = JsonError::from(ObjectError::Unavailable(
            "failure at /secret/catalog with token=private".into(),
        ));
        assert!(!error.message.contains("/secret"));
        assert!(!error.message.contains("token="));
        for (cause, expected) in [
            (
                "catalog unavailable: database is locked",
                "database is locked",
            ),
            (
                "Permission denied (os error 13): token=private",
                "Permission denied",
            ),
            (
                "database is locked: /secret/catalog token=private",
                "database is locked",
            ),
            (
                "database is locked token=private",
                "unrecognized backend failure (private details withheld)",
            ),
            (
                "failure at /secret/catalog with token=private",
                "unrecognized backend failure (private details withheld)",
            ),
        ] {
            let cause = cause.strip_prefix("catalog unavailable: ").unwrap_or(cause);
            assert_eq!(safe_backend_cause(cause), expected);
            let error = JsonError::backend(cause);
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                error.message,
                "Object storage is unavailable: internal storage failure (private backend details withheld)"
            );
        }
    }
}
