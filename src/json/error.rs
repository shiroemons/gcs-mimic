use crate::{
    auth::{AuthFailure, Denied},
    bucket::CatalogError,
    object::ObjectError,
    storage::failure::{BackendCause, BackendFailure, IoCause, MemoryBudget},
};
use http_body_util::Full;
use hyper::{Response, StatusCode, body::Bytes, header};
use serde_json::{Map, Value, json};
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

/// How long a malformed body an error quotes. A resource body may be far longer, and the quote
/// appears twice in the response, so past this it would make the answer larger than the request.
const QUOTED_BODY_LIMIT: usize = 64 * 1024;

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
            chars.iter().skip(key_start).collect(),
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
    /// The refused permission behind a denial, logged when the response leaves so that a
    /// probe which discards the denial logs nothing.
    pub(crate) denied: Option<Box<Denied>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    /// Keeps the typed timeout cause available to classification tests after JSON adaptation.
    Backend(BackendFailure),
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
static PARAMETERS: [ErrorLocation; 37] = [
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
    parameter_location("enableObjectRetention"),
    parameter_location("returnPartialSuccess"),
    parameter_location("prettyPrint"),
    parameter_location("includeFoldersAsPrefixes"),
    parameter_location("includeTrailingDelimiter"),
    parameter_location("overrideUnlockedRetention"),
    parameter_location("copySourceAcl"),
];

impl ErrorLocation {
    /// The `parameter` location for a query parameter name, if it is one the API names.
    pub(crate) fn parameter(name: &str) -> Option<&'static ErrorLocation> {
        PARAMETERS.iter().find(|location| location.name == name)
    }
}

impl JsonError {
    /// Names only the remedy for the storage that ran out: a disk backend never has a RAM budget
    /// to raise, and a memory backend has no data directory to free.
    fn storage_capacity(cause: &IoCause) -> Self {
        Self::service_unavailable(match cause.memory_budget() {
            None => {
                "Storage capacity exhausted; free space in the data directory, where data kept by soft delete uses space until it expires (set softDeletePolicy.retentionDurationSeconds to 0 on buckets that retain deletions), then retry with generation preconditions"
            }
            // Deleting does not free what soft delete retains, which can be what filled RAM.
            Some(MemoryBudget::Blobs) => {
                "Storage capacity exhausted; delete objects, though data kept by soft delete keeps its bytes until it expires (set softDeletePolicy.retentionDurationSeconds to 0 on buckets that retain deletions), or restart with a larger memory_max_bytes (a restart loses RAM data)"
            }
            Some(MemoryBudget::Catalog) => {
                "Storage capacity exhausted; restart with a larger memory_catalog_max_bytes (a restart loses RAM data)"
            }
        })
    }

    fn backend(backend: &BackendCause) -> Self {
        // Only fixed diagnostics cross the HTTP boundary; arbitrary backend text can contain paths or secrets.
        if let Some(failure) = backend.failure()
            && let Some(message) = failure.guidance()
        {
            return Self::service_unavailable(message).with_kind(ErrorKind::Backend(failure));
        }
        let cause = backend
            .message()
            .strip_prefix("catalog unavailable: ")
            .unwrap_or(backend.message());
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Error,
            "object_backend_failure",
            json!({"cause": safe_backend_cause(cause)}),
        );
        Self::service_unavailable(
            "Object storage is unavailable: internal storage failure (private backend details withheld)",
        )
    }

    fn catalog_backend(backend: &BackendCause) -> Self {
        let mut error = Self::backend(backend);
        error.message = match error.kind {
            ErrorKind::Backend(_) => error
                .message
                .replacen("Object storage", "Bucket catalog", 1),
            _ => "Bucket catalog is unavailable".into(),
        };
        error
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
            denied: None,
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
        if input.len() > QUOTED_BODY_LIMIT {
            return Self::invalid(format!("Parse Error: {cause}"))
                .with_kind(ErrorKind::MalformedJson);
        }
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

    /// A refusal that no Cloud Storage permission names keeps the documented example message.
    pub(crate) fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", "Forbidden")
    }

    /// The 401 `required` an anonymous caller receives in place of a 403. A refused
    /// permission keeps its message, which names the anonymous caller; any other refusal
    /// becomes the documented `Login Required` example.
    pub(crate) fn for_anonymous_caller(self) -> Self {
        let mut error = Self::from(AuthFailure::LoginRequired);
        if let Some(denied) = self.denied {
            if denied.is_anonymous() {
                error.message = denied.to_string();
            }
            error.denied = Some(denied);
        }
        error
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
        let mut item = Map::from_iter([
            ("message".into(), json!(self.message)),
            ("domain".into(), "global".into()),
            ("reason".into(), self.reason.into()),
        ]);
        if let Some(location) = self.location {
            item.insert("locationType".into(), location.kind.into());
            item.insert("location".into(), location.name.into());
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
        if let Some(denied) = &self.denied {
            denied.log();
        }
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

impl From<crate::auth::AuthError> for JsonError {
    fn from(error: crate::auth::AuthError) -> Self {
        match error {
            crate::auth::AuthError::Failure(failure) => failure.into(),
            crate::auth::AuthError::Unavailable(cause) => Self::backend(&cause),
        }
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
            AuthFailure::Denied(denied) => denied.into(),
            AuthFailure::Forbidden => Self::forbidden(),
        }
    }
}

/// The documented 403 `forbidden` reason with Cloud Storage's longer message naming the
/// principal and the refused permission.
impl From<Box<Denied>> for JsonError {
    fn from(denied: Box<Denied>) -> Self {
        let mut error = Self::new(StatusCode::FORBIDDEN, "forbidden", denied.to_string());
        error.denied = Some(denied);
        error
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
            CatalogError::Denied(denied) => denied.into(),
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
            CatalogError::Metadata(error) => {
                if error.unsupported() {
                    Self::unsupported(&error.to_string())
                } else {
                    Self::invalid(error.to_string())
                }
            }
            CatalogError::UnsupportedMetadata(field) => Self::unsupported(&field),
            CatalogError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull => {
                Self::storage_capacity(&cause)
            }
            CatalogError::Unavailable(cause) | CatalogError::Corrupt(cause) => {
                Self::catalog_backend(&cause)
            }
            error @ (CatalogError::AuthenticationAlreadyConfigured
            | CatalogError::Io(_)
            | CatalogError::NewerSchema { .. }) => {
                Self::catalog_backend(&BackendCause::from_error(error))
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
            ObjectError::Metadata(error) => {
                if error.unsupported() { Self::unsupported_object(&error.to_string()) }
                else { Self::invalid(error.to_string()) }
            }
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
            ObjectError::OverwriteForbidden(denied) | ObjectError::Denied(denied) => denied.into(),
            ObjectError::Forbidden => Self::forbidden(),
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
            ObjectError::Unavailable(cause) => Self::backend(&cause),
            ObjectError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull => {
                Self::storage_capacity(&cause)
            }
            ObjectError::Io(kind) => Self::service_unavailable(format!(
                "Object storage is unavailable: storage I/O error ({kind}); check data directory permissions and available disk space"
            )),
            ObjectError::Corrupt(cause) => {
                let mut error = Self::backend(&cause);
                error.message = "Object storage is unavailable: stored data is corrupt; restore a valid data directory".into();
                error
            },
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
        "derived publication catalog admission timed out",
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
    fn a_long_malformed_body_is_not_quoted_back() {
        let parse = |input: &[u8]| {
            let cause = serde_json::from_slice::<serde_json::Value>(input).unwrap_err();
            JsonError::parse_error(input, &cause)
        };
        assert!(parse(b"{\"a\":}").message.contains("{\"a\":}\n"));
        let mut long = vec![b' '; QUOTED_BODY_LIMIT];
        long.push(b'x');
        let error = parse(&long);
        assert!(error.message.len() < 1024, "{}", error.message.len());
        assert_eq!(error.kind, ErrorKind::MalformedJson);
        // The bare-key form walks the body by character before it can report anything.
        let mut bare = vec![b'x'; QUOTED_BODY_LIMIT];
        bare.push(b'x');
        assert!(parse(&bare).message.len() < 1024);
    }

    /// A tester whose Docker disk filled under `backend=disk` was told about RAM budgets first;
    /// each storage that can run out now names only its own remedy.
    #[test]
    fn storage_capacity_names_only_the_remedy_of_the_storage_that_ran_out() {
        use crate::storage::failure::MemoryExhausted;
        let memory = |budget| {
            IoCause::from(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                MemoryExhausted::new(budget),
            ))
        };
        let disk = || IoCause::from(std::io::ErrorKind::StorageFull);
        for (cause, names, omits) in [
            (
                disk(),
                &[
                    "data directory",
                    "softDeletePolicy.retentionDurationSeconds",
                    "generation preconditions",
                ][..],
                &[
                    "memory_max_bytes",
                    "memory_catalog_max_bytes",
                    "backend=",
                    "default_soft_delete_retention_seconds",
                ][..],
            ),
            (
                memory(MemoryBudget::Blobs),
                &[
                    "delete objects",
                    "softDeletePolicy.retentionDurationSeconds",
                    "memory_max_bytes",
                    "restart loses RAM data",
                ][..],
                &[
                    "memory_catalog_max_bytes",
                    "data directory",
                    "default_soft_delete_retention_seconds",
                ][..],
            ),
            (
                memory(MemoryBudget::Catalog),
                &["memory_catalog_max_bytes", "restart loses RAM data"][..],
                &["memory_max_bytes", "data directory", "softDeletePolicy"][..],
            ),
        ] {
            let object = JsonError::from(ObjectError::Io(cause.clone()));
            let catalog = JsonError::from(CatalogError::Io(cause));
            assert_eq!(object.message, catalog.message);
            assert_eq!(object.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(object.reason, "backendError");
            let message = &object.message;
            assert!(
                message.starts_with("Storage capacity exhausted;"),
                "{message}"
            );
            for name in names {
                assert!(message.contains(name), "{name}: {message}");
            }
            for name in omits {
                assert!(!message.contains(name), "{name}: {message}");
            }
        }
    }

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

    fn denied(id: Option<&str>) -> Box<Denied> {
        let identity = id.map(|id| {
            crate::auth::Identity::from_upload_snapshot(&json!({"id": id, "projects": {}})).unwrap()
        });
        Denied::new(
            identity.as_ref(),
            crate::auth::BUCKETS_GET,
            crate::auth::denied::Resource::in_bucket(crate::auth::BUCKETS_GET, "bucket"),
        )
    }

    /// Every error enum renders a refused permission as the documented 403 `forbidden` reason
    /// with the message naming the principal and the permission.
    #[test]
    fn denials_keep_the_forbidden_reason_and_name_the_principal_and_permission() {
        let message = "viewer@example.invalid does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist).";
        let viewer = || denied(Some("viewer@example.invalid"));
        for error in [
            JsonError::from(AuthFailure::Denied(viewer())),
            JsonError::from(CatalogError::Denied(viewer())),
            JsonError::from(ObjectError::Denied(viewer())),
        ] {
            assert_eq!(
                error.value(),
                json!({"error": {"code": 403, "message": message,
                    "errors": [{"message": message, "domain": "global", "reason": "forbidden"}]}})
            );
            assert_eq!(error.denied, Some(viewer()));
        }
        // Refusals that no permission names keep the documented example message.
        for error in [
            JsonError::from(AuthFailure::Forbidden),
            JsonError::from(CatalogError::Forbidden),
            JsonError::from(ObjectError::Forbidden),
        ] {
            assert_eq!(
                (error.status, error.reason),
                (StatusCode::FORBIDDEN, "forbidden")
            );
            assert_eq!(error.message, "Forbidden");
            assert_eq!(error.denied, None);
        }
    }

    #[test]
    fn an_anonymous_caller_gets_401_required_naming_the_permission_it_lacks() {
        let anonymous = JsonError::from(CatalogError::Denied(denied(None))).for_anonymous_caller();
        let message = "Anonymous caller does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist).";
        assert_eq!(
            anonymous.value(),
            json!({"error": {"code": 401, "message": message, "errors": [{"message": message,
                "domain": "global", "reason": "required", "locationType": "header",
                "location": "Authorization"}]}})
        );
        assert_eq!(anonymous.kind, ErrorKind::Required);
        // Another 403, or a refusal naming a stored principal, keeps the documented example.
        let login_required = JsonError::from(AuthFailure::LoginRequired);
        for error in [
            JsonError::from(ObjectError::LockedRetention),
            JsonError::from(ObjectError::Denied(denied(Some(
                "snapshot@example.invalid",
            )))),
        ] {
            let error = error.for_anonymous_caller();
            assert_eq!(error.value(), login_required.value());
            assert_eq!(error.kind, ErrorKind::Required);
        }
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
            assert_eq!(events.len(), 15);
            assert!(events.iter().all(|event| {
                event["event"] == "object_backend_failure" && event["level"] == "error"
            }));
            assert_eq!(events[1]["cause"], "database is locked");
            assert_eq!(events[2]["cause"], "Permission denied");
            assert_eq!(events[6]["cause"], "database is locked");
            assert_eq!(events[7]["cause"], "invalid opaque blob ID");
            assert_eq!(events[11]["cause"], "Acknowledged upload prefix is missing");
            assert_eq!(events[13]["cause"], "database is locked");
            return;
        }
        for cause in [
            BackendFailure::CatalogClosed,
            BackendFailure::UploadWaitTimeout,
            BackendFailure::RetainedSessionLimit,
            BackendFailure::RewriteSessionLimit,
            BackendFailure::FinalizationTimeout,
            BackendFailure::CopyDeadline,
            BackendFailure::SessionBusy,
        ] {
            let error = JsonError::from(ObjectError::Unavailable(cause.into()));
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(error.message.contains(cause.message()));
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
            let error = JsonError::backend(&cause.into());
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                error.message,
                "Object storage is unavailable: internal storage failure (private backend details withheld)"
            );
        }
        use crate::storage::hmac_keys::HmacKeyError;
        let catalog = "Bucket catalog is unavailable";
        let corrupt =
            "Object storage is unavailable: stored data is corrupt; restore a valid data directory";
        for (error, expected) in [
            (
                JsonError::from(CatalogError::Unavailable(
                    "database is locked: /secret token=private".into(),
                )),
                catalog,
            ),
            (
                JsonError::from(CatalogError::Corrupt(
                    "invalid opaque blob ID: /secret token=private".into(),
                )),
                catalog,
            ),
            (
                JsonError::from(CatalogError::Io(
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "Permission denied: /secret token=private",
                    )
                    .into(),
                )),
                catalog,
            ),
            (
                JsonError::from(CatalogError::AuthenticationAlreadyConfigured),
                catalog,
            ),
            (
                JsonError::from(CatalogError::NewerSchema {
                    version: 99,
                    supported: 50,
                }),
                catalog,
            ),
            (
                JsonError::from(ObjectError::Corrupt(
                    "Acknowledged upload prefix is missing: /secret token=private".into(),
                )),
                corrupt,
            ),
            (
                JsonError::from(ObjectError::Corrupt("/secret token=private".into())),
                corrupt,
            ),
            (
                JsonError::from(HmacKeyError::Catalog(CatalogError::Unavailable(
                    "database is locked: /secret token=private".into(),
                ))),
                catalog,
            ),
            (
                JsonError::from(HmacKeyError::Catalog(CatalogError::Corrupt(
                    "/secret token=private".into(),
                ))),
                catalog,
            ),
        ] {
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(error.reason, "backendError");
            assert_eq!(error.message, expected);
        }
    }

    #[tokio::test]
    async fn catalog_backend_errors_keep_bucket_guidance_and_safe_envelope() {
        use http_body_util::BodyExt;

        let generic = "Bucket catalog is unavailable";
        for (cause, message) in [
            (
                CatalogError::Unavailable("catalog is closed".into()),
                generic,
            ),
            (
                CatalogError::Unavailable(BackendFailure::CatalogClosed.into()),
                "Bucket catalog is unavailable: catalog is closed; wait for the server to restart before retrying",
            ),
            (
                CatalogError::Io(std::io::ErrorKind::PermissionDenied.into()),
                generic,
            ),
            (
                CatalogError::NewerSchema {
                    version: 99,
                    supported: 50,
                },
                generic,
            ),
            (CatalogError::AuthenticationAlreadyConfigured, generic),
            (
                CatalogError::Corrupt("/secret token=private".into()),
                generic,
            ),
        ] {
            let error = JsonError::from(cause);
            assert!(!error.message.contains("Object storage"));
            assert_eq!(error.message, message);
            let response = error.into_response();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/json; charset=UTF-8"
            );
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                NO_STORE_CACHE_CONTROL
            );
            assert_eq!(response.headers()[header::PRAGMA], "no-cache");
            assert_eq!(response.headers().len(), 3);
            let expected = json!({"error": {
                "code": 503,
                "message": message,
                "errors": [{"message": message, "domain": "global", "reason": "backendError"}]
            }});
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                serde_json::to_string_pretty(&expected).unwrap()
            );
        }
    }
}
