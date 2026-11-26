//! Cloud Storage JSON errors, their domain mappings and their HTTP responses.

use crate::{
    auth::{AuthFailure, Denied},
    bounded_body::BodyError,
    bucket::CatalogError,
    input::{InputError, InputKind},
    multipart::MultipartError,
    object::{
        ObjectError,
        media_headers::{NO_STORE_CACHE_CONTROL, StoredHeaderError},
        upload_headers::UploadHeaderError,
    },
    service::ServiceError,
    storage::failure::{
        BackendCause, BackendFailure,
        wording::{self, BUCKET_NOT_FOUND, BackendNotice, OBJECT_NOT_FOUND, PRECONDITION_NOT_HELD},
    },
};
use http_body_util::Full;
use hyper::{Response, StatusCode, body::Bytes, header};
use serde_json::{Map, Value, json};

/// A refusal rendered by a Cloud Storage JSON handler.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{status}: {message}")]
pub struct ApiError {
    kind: ErrorKind,
    status: StatusCode,
    reason: &'static str,
    message: String,
    allow: Option<&'static str>,
    /// The `locationType`/`location` pair of the error item, if any. A reference keeps the error
    /// small, which `Result` returns throughout the crate depend on.
    location: Option<&'static ErrorLocation>,
    /// Cloud Storage answered some failures with a bare text page instead of a JSON envelope.
    plain: Option<&'static PlainText>,
    /// Cloud Storage's upload front end sent some JSON error envelopes as `text/html`.
    html_typed: bool,
    /// The refused permission behind a denial, logged when the response leaves so that a
    /// probe which discards the denial logs nothing.
    denied: Option<Box<Denied>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErrorKind {
    Backend(BackendFailure),
    MissingBucket,
    MissingObject,
    NamedMissingObject,
    InvalidArgument,
    LegacyParseError,
    ClassLocation,
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
    content_type: crate::mime::TEXT_PLAIN_UTF8,
    body: Some("Not Found"),
    cache: PlainCache::Unspecified,
};

/// A multipart upload whose `Content-Type` carries no boundary.
pub(crate) const BAD_CONTENT_TYPE_TEXT: PlainText = PlainText {
    content_type: crate::mime::TEXT_PLAIN_UTF8,
    body: Some("Bad content type.  Please use multipart."),
    cache: PlainCache::Unspecified,
};

/// A resumable chunk whose `Content-Range` Cloud Storage could not use.
pub(crate) const CONTENT_RANGE_TEXT: PlainText = PlainText {
    content_type: crate::mime::TEXT_PLAIN_UTF8,
    body: Some("Failed to parse Content-Range header."),
    cache: PlainCache::Unspecified,
};

/// A multipart upload body that ends before its closing delimiter (recorded 2026-11-13).
pub(crate) const MISSING_END_BOUNDARY_TEXT: PlainText = PlainText {
    content_type: crate::mime::TEXT_PLAIN_UTF8,
    body: Some("Missing end boundary in multipart body."),
    cache: PlainCache::Unspecified,
};

/// A transport-compressed request body that does not decode, which Cloud Storage's front end
/// refuses before routing or authenticating the request (recorded 2026-11-25).
pub(crate) const DECOMPRESS_TEXT: PlainText = PlainText {
    content_type: crate::mime::TEXT_PLAIN_UTF8,
    body: Some("Could not decompress request body."),
    cache: PlainCache::Unspecified,
};

/// A session-protocol failure that has no recorded text of its own. The recorded failures
/// of a continuation (an unusable `Content-Range`, an unknown session) are bare `text/plain`
/// pages without cache fields, so the other protocol errors (an inconsistent total, a gap in
/// the offsets, a finished or cancelled session) use the same form with the message they
/// already had. The form is inferred; only the two wordings above were recorded.
pub(crate) const SESSION_MESSAGE_TEXT: PlainText = PlainText {
    content_type: crate::mime::TEXT_PLAIN_UTF8,
    body: None,
    cache: PlainCache::Unspecified,
};

/// An upload request whose `uploadType` is not one of the three.
pub(crate) const BAD_REQUEST_TEXT: PlainText = PlainText {
    content_type: crate::mime::TEXT_PLAIN_UTF8,
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
/// An IAM policy body's `etag` member, which Cloud Storage reported as `locationType: other`.
pub(crate) const POLICY_ETAG_FIELD: ErrorLocation = ErrorLocation {
    kind: "other",
    name: "etag",
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

impl ApiError {
    /// The HTTP status sent by the JSON handler.
    pub fn status(&self) -> StatusCode {
        self.status
    }
    /// The reason in the JSON error item.
    pub fn reason(&self) -> &'static str {
        self.reason
    }
    /// The message associated with the rejected request.
    pub fn message(&self) -> &str {
        &self.message
    }
    /// The location of the rejected input, when the response identifies it.
    pub fn location(&self) -> Option<&'static ErrorLocation> {
        self.location
    }
    /// The allowed methods sent with a method refusal, when present.
    pub fn allow(&self) -> Option<&'static str> {
        self.allow
    }

    #[cfg(test)]
    pub(crate) fn backend_failure(&self) -> Option<BackendFailure> {
        if let ErrorKind::Backend(failure) = self.kind {
            Some(failure)
        } else {
            None
        }
    }

    pub(crate) fn for_legacy_bucket(mut self) -> Self {
        if matches!(
            self.kind,
            ErrorKind::InvalidArgument | ErrorKind::LegacyParseError
        ) {
            self.reason = "invalid";
        }
        if self.is_missing_bucket() {
            self.message = "Not Found".into();
        }
        self
    }

    fn backend(backend: &BackendCause) -> Self {
        wording::object_backend(backend).into()
    }

    fn catalog_backend(backend: &BackendCause) -> Self {
        wording::catalog_backend(backend).into()
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

    /// A semantic metadata refusal; `unsupported` words a recognized setting gcs-mimic lacks in
    /// the API that refused it.
    fn metadata(
        error: &crate::metadata_error::MetadataError,
        unsupported: fn(&str) -> Self,
    ) -> Self {
        let message = error.to_string();
        if matches!(
            error,
            crate::metadata_error::MetadataError::ClassLocation(_)
        ) {
            Self::invalid(message).with_kind(ErrorKind::ClassLocation)
        } else if error.unsupported() {
            unsupported(&message)
        } else {
            Self::invalid(message)
        }
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
    }

    pub(crate) fn not_modified() -> Self {
        Self::new(StatusCode::NOT_MODIFIED, "notModified", "Not modified")
    }

    /// A Match and NotMatch condition on the same value of a move, which Cloud Storage refuses
    /// as a conflict before evaluating either (recorded 2026-11-15).
    pub(crate) fn conflicting_preconditions() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "conflict",
            "Found conflicting object preconditions. Mutually exclusive fields cannot be set simultaneously.",
        )
    }

    /// A request body that is not JSON, with the wording the JSON adapter quotes for it.
    pub(crate) fn malformed_json(message: impl Into<String>) -> Self {
        Self::invalid(message)
    }

    pub(crate) fn legacy_parse_error(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "parseError", message)
            .with_kind(ErrorKind::LegacyParseError)
    }

    pub(crate) fn unsupported_message(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "badRequest", message)
    }

    pub(crate) fn field_mask_syntax(message: impl Into<String>) -> Self {
        Self::invalid_parameter(message).at_parameter("fields")
    }

    /// Names the rejected query parameter in the error item (`locationType: parameter`). A name
    /// outside the API's parameter table leaves the error without a location.
    pub(crate) fn at_parameter(mut self, name: &str) -> Self {
        self.location = ErrorLocation::parameter(name);
        self
    }

    /// Names where the rejected value was sent (the error item's `locationType`/`location`).
    pub(crate) fn at(mut self, location: &'static ErrorLocation) -> Self {
        self.location = Some(location);
        self
    }

    /// Names the `If-Match` header: Cloud Storage reports every failed precondition there, even
    /// when the condition was a query parameter.
    pub(crate) fn at_if_match(self) -> Self {
        self.at(&IF_MATCH_HEADER)
    }

    /// Sends the error as `page`, the bare text page Cloud Storage's front end answered with.
    pub(crate) fn with_plain(mut self, page: &'static PlainText) -> Self {
        self.plain = Some(page);
        self
    }

    pub(crate) fn invalid_parameter(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalidParameter", message)
    }

    /// A transport-compressed request body that does not decode; see [`DECOMPRESS_TEXT`].
    pub(crate) fn undecodable_body() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "badRequest",
            "Could not decompress request body.",
        )
        .with_plain(&DECOMPRESS_TEXT)
    }

    /// A body `name` or `bucket` that disagrees with the resource the path names, worded as
    /// Cloud Storage words it for bucket and object PATCH and PUT.
    pub(crate) fn content_disagrees(content: &str, parameter: &str) -> Self {
        Self::invalid_parameter(format!(
            "Value '{content}' in content does not agree with value '{parameter}'. This can happen when a value set through a parameter is inconsistent with a value set in the request."
        ))
    }

    pub(crate) fn required(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "required", message)
    }

    pub(crate) fn method_not_allowed(
        message: impl Into<String>,
        allow: Option<&'static str>,
    ) -> Self {
        Self {
            allow,
            ..Self::new(StatusCode::METHOD_NOT_ALLOWED, "methodNotAllowed", message)
        }
    }

    pub(crate) fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "backendError", message)
    }

    pub(crate) fn invalid_stored_header() -> Self {
        Self::service_unavailable(crate::object::media_headers::StoredHeaderError::MESSAGE)
    }

    /// A numeric query parameter Cloud Storage could not parse as a signed 64-bit integer.
    pub(crate) fn invalid_long(value: &str) -> Self {
        Self::invalid_parameter(format!("Invalid long value: '{value}'."))
    }

    /// A numeric query parameter Cloud Storage could not parse as an unsigned integer.
    pub(crate) fn invalid_unsigned(value: &str) -> Self {
        Self::invalid_parameter(format!("Invalid unsigned integer value: '{value}'."))
    }

    /// The same JSON envelope, labelled `text/html` as the upload front end did for request
    /// problems it found before reaching the storage handler.
    pub(crate) fn html_typed(mut self) -> Self {
        self.html_typed = true;
        self
    }

    /// An upload's refusal as the upload front end labels it: a class the bucket's location
    /// cannot hold left as `text/html` too (recorded 2026-11-17 on a multipart insert), although
    /// gcs-mimic finds it in the catalog.
    pub(crate) fn for_upload(self) -> Self {
        if self.kind == ErrorKind::ClassLocation {
            self.html_typed()
        } else {
            self
        }
    }

    /// A session-protocol failure (a `Content-Range` that cannot be used, a gap or total that
    /// disagrees with the session): the upload front end answered these with a bare `text/plain`
    /// page, so the message leaves as that page with no error `reason`. Failures of other kinds
    /// (checksums, headers, preconditions) keep their JSON envelope, because SDKs read the
    /// `reason` from it.
    pub(crate) fn session_protocol(message: impl Into<String>) -> Self {
        Self::invalid_parameter(message).with_plain(&SESSION_MESSAGE_TEXT)
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
        Self::new(StatusCode::FORBIDDEN, "forbidden", wording::FORBIDDEN)
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
            // A refusal can name a stored principal, such as the identity a resumable session
            // keeps. The response answers the anonymous caller, so the logged event names that
            // caller too rather than an identity the request never presented.
            error.denied = Some(denied.into_anonymous());
        }
        error
    }

    pub fn unsupported(name: &str) -> Self {
        Self::unsupported_message(wording::unsupported_bucket_setting(name))
    }

    pub fn unsupported_object(name: &str) -> Self {
        Self::unsupported_message(wording::unsupported_object_setting(name))
    }

    /// Whether this is a missing-bucket error, in either the bucket or the object wording.
    fn is_missing_bucket(&self) -> bool {
        self.kind == ErrorKind::MissingBucket
    }

    /// Whether this is the catalog's missing-object error, before or after it names the object.
    fn is_missing_object(&self) -> bool {
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
            self.message = wording::missing_named_object(bucket, name);
            self.kind = ErrorKind::NamedMissingObject;
        }
        self
    }
}

mod domain;
#[cfg(test)]
mod domain_tests;

impl From<InputError> for ApiError {
    fn from(input: InputError) -> Self {
        let error = match input.kind {
            InputKind::Parameter => Self::invalid_parameter(input.message),
            InputKind::FieldMaskSyntax => Self::field_mask_syntax(input.message),
            InputKind::SessionProtocol => Self::session_protocol(input.message),
            InputKind::Invalid => Self::invalid(input.message),
            InputKind::Required => Self::required(input.message),
            InputKind::Unsupported => Self::unsupported_message(input.message),
        };
        match input.field {
            Some(field) => error.at_parameter(field),
            None => error,
        }
    }
}

impl From<BodyError> for ApiError {
    fn from(error: BodyError) -> Self {
        ServiceError::from(error).into()
    }
}

impl From<MultipartError> for ApiError {
    fn from(error: MultipartError) -> Self {
        ServiceError::from(error).into()
    }
}

impl From<UploadHeaderError> for ApiError {
    fn from(error: UploadHeaderError) -> Self {
        ServiceError::from(error).into()
    }
}

impl From<StoredHeaderError> for ApiError {
    fn from(error: StoredHeaderError) -> Self {
        ServiceError::from(error).into()
    }
}

impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> Self {
        match error {
            ServiceError::Input(error) => error.into(),
            ServiceError::Project(error) => error.into(),
            ServiceError::Catalog(error) => error.into(),
            ServiceError::Object(error) => (*error).into(),
            ServiceError::Resumable(error) => (*error).into(),
            ServiceError::Csek(error) => error.into(),
            ServiceError::UploadHeader(error) | ServiceError::SessionHeader(error) => match error {
                UploadHeaderError::UnsupportedAlgorithm => {
                    Self::unsupported_message(error.to_string())
                }
                UploadHeaderError::CustomMetadata { .. }
                | UploadHeaderError::InvalidContentType
                | UploadHeaderError::DuplicateContentType
                | UploadHeaderError::DuplicateAlgorithm
                | UploadHeaderError::MalformedHash(_)
                | UploadHeaderError::Digest(_)
                | UploadHeaderError::Conflicting(_) => Self::invalid_parameter(error.to_string()),
            },
            ServiceError::Multipart(error) => match error {
                MultipartError::Framing => Self::invalid_parameter(error.to_string()),
                MultipartError::BadContentType => {
                    Self::invalid_parameter(error.to_string()).with_plain(&BAD_CONTENT_TYPE_TEXT)
                }
                MultipartError::MissingEnd => Self::invalid_parameter(error.to_string())
                    .with_plain(&MISSING_END_BOUNDARY_TEXT),
                MultipartError::UnsupportedParameter(_) => {
                    Self::unsupported_message(error.to_string())
                }
            },
            ServiceError::StoredHeader(_) => Self::invalid_stored_header(),
            ServiceError::Body(error) => match error {
                BodyError::TooLarge => Self::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "uploadTooLarge",
                    error.to_string(),
                ),
                BodyError::OutOfMemory => Self::service_unavailable(error.to_string()),
                BodyError::Unreadable => {
                    Self::new(StatusCode::BAD_REQUEST, "badRequest", error.to_string())
                }
            },
            ServiceError::Time(error) => error.into(),
        }
    }
}

// PATCH and PUT read individual members of the same resource as an insert.
impl From<crate::bucket::resource::Error> for ApiError {
    fn from(error: crate::bucket::resource::Error) -> Self {
        ServiceError::from(error).into()
    }
}

/// The `Cache-Control` of a media download that Cloud Storage answers with a plain error; its
/// `Expires` is the response time.
pub(crate) const MEDIA_ERROR_CACHE_CONTROL: &str = "private, max-age=0";

/// How long a malformed body an error quotes. A resource body may be far longer, and the quote
/// appears twice in the response, so past this it would make the answer larger than the request.
const QUOTED_BODY_LIMIT: usize = 64 * 1024;

/// What a JSON parse error quotes: the parser's reason, the input it shows and the column of
/// the caret under it.
struct ParseContext {
    reason: String,
    shown: String,
    column: usize,
}

/// Where Cloud Storage's parser expected the `:` after the first key and what it said.
/// The first key may be bare, as in the recorded `{not json` and `oops` inputs.
fn missing_colon(text: &str) -> Option<ParseContext> {
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
        // An object that ends right after its brace has no key yet (recorded for `{`).
        if index == chars.len() {
            return Some(ParseContext {
                reason: "Unexpected end of string. Expected an object key or }.".to_owned(),
                shown: String::new(),
                column: 0,
            });
        }
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
        Some(_) => Some(ParseContext {
            reason: "Expected : between key:value pair.".to_owned(),
            shown: text.to_owned(),
            column: index,
        }),
        // At the end of the input the quoted context starts at the key (recorded for `{oops`).
        None => Some(ParseContext {
            reason: "Unexpected end of string. Expected : between key:value pair.".to_owned(),
            shown: chars.iter().skip(key_start).collect(),
            column: chars.len() - key_start,
        }),
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

/// A request body that is not JSON. Cloud Storage quotes the offending input with a caret
/// under the position where its lenient parser stopped, which the recorded object-resource
/// failures show for a missing `:` after the first key.
pub(crate) fn parse_error(input: &[u8], cause: &serde_json::Error) -> ApiError {
    if input.len() > QUOTED_BODY_LIMIT {
        return ApiError::malformed_json(format!("Parse Error: {cause}"));
    }
    let text = String::from_utf8_lossy(input);
    let ParseContext {
        reason,
        shown,
        column,
    } = missing_colon(&text).unwrap_or_else(|| {
        // A body that ends inside a value is reported with an empty context line (recorded
        // 2026-10-31 for bucket bodies truncated after a key).
        if cause.is_eof() {
            return ParseContext {
                reason: "Unexpected end of string. Expected a value.".to_owned(),
                shown: String::new(),
                column: 0,
            };
        }
        ParseContext {
            reason: cause
                .to_string()
                .split(" at line ")
                .next()
                .unwrap_or_default()
                .to_owned(),
            shown: text.to_string(),
            column: cause.column().saturating_sub(1),
        }
    });
    ApiError::malformed_json(format!(
        "Parse Error: {reason}\n{shown}\n{}^",
        " ".repeat(column)
    ))
}

/// HTTP renderings used by JSON handlers and their media download endpoints.
pub(crate) trait JsonErrorResponse {
    /// The error envelope: `{"error": {"code", "message", "errors": [..]}}`.
    fn value(&self) -> Value;

    /// A media download's error. Cloud Storage answers a missing object and an unsatisfiable
    /// range with an HTML-typed plain message and `private, max-age=0`; only these two were
    /// recorded, so other media errors keep the JSON form.
    fn into_media_response(self) -> Response<Full<Bytes>>;

    fn into_response(self) -> Response<Full<Bytes>>;
}

impl JsonErrorResponse for ApiError {
    fn value(&self) -> Value {
        let mut item = Map::from_iter([
            ("message".into(), self.message.as_str().into()),
            ("domain".into(), "global".into()),
            ("reason".into(), self.reason.into()),
        ]);
        if let Some(location) = self.location {
            item.insert("locationType".into(), location.kind.into());
            item.insert("location".into(), location.name.into());
        }
        json!({"error": {"code": self.status.as_u16(), "message": self.message, "errors": [item]}})
    }

    fn into_media_response(self) -> Response<Full<Bytes>> {
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

    fn into_response(self) -> Response<Full<Bytes>> {
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

#[cfg(test)]
mod golden_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::AuthFailure,
        bucket::CatalogError,
        object::ObjectError,
        storage::failure::{BackendFailure, test_support::denied},
    };

    #[test]
    fn a_long_malformed_body_is_not_quoted_back() {
        let parse = |input: &[u8]| {
            let cause = serde_json::from_slice::<serde_json::Value>(input).unwrap_err();
            parse_error(input, &cause)
        };
        assert!(parse(b"{\"a\":}").message.contains("{\"a\":}\n"));
        let mut long = vec![b' '; QUOTED_BODY_LIMIT];
        long.push(b'x');
        let error = parse(&long);
        assert!(error.message.len() < 1024, "{}", error.message.len());
        // The bare-key form walks the body by character before it can report anything.
        let mut bare = vec![b'x'; QUOTED_BODY_LIMIT];
        bare.push(b'x');
        assert!(parse(&bare).message.len() < 1024);
    }

    /// Every error enum renders a refused permission as the documented 403 `forbidden` reason
    /// with the message naming the principal and the permission.
    #[test]
    fn denials_keep_the_forbidden_reason_and_name_the_principal_and_permission() {
        let message = "viewer@example.invalid does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist).";
        let viewer = || denied(Some("viewer@example.invalid"));
        for error in [
            ApiError::from(AuthFailure::Denied(viewer())),
            ApiError::from(CatalogError::Denied(viewer())),
            ApiError::from(ObjectError::Denied(viewer())),
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
            ApiError::from(AuthFailure::Forbidden),
            ApiError::from(CatalogError::Forbidden),
            ApiError::from(ObjectError::Forbidden),
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
        let anonymous = ApiError::from(CatalogError::Denied(denied(None))).for_anonymous_caller();
        let message = "Anonymous caller does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist).";
        assert_eq!(
            anonymous.value(),
            json!({"error": {"code": 401, "message": message, "errors": [{"message": message,
                "domain": "global", "reason": "required", "locationType": "header",
                "location": "Authorization"}]}})
        );
        // Another 403, or a refusal naming a stored principal, keeps the documented example.
        let login_required = ApiError::from(AuthFailure::LoginRequired);
        for error in [
            ApiError::from(ObjectError::LockedRetention),
            ApiError::from(ObjectError::Denied(denied(Some(
                "snapshot@example.invalid",
            )))),
        ] {
            let error = error.for_anonymous_caller();
            assert_eq!(error.value(), login_required.value());
        }
    }

    #[test]
    fn json_parse_errors_quote_the_input_with_a_caret_like_cloud_storage() {
        let parse = |input: &str| {
            let cause = serde_json::from_slice::<Value>(input.as_bytes()).unwrap_err();
            parse_error(input.as_bytes(), &cause)
        };
        let error = parse("{not json");
        assert_eq!((error.status.as_u16(), error.reason), (400, "invalid"));
        assert_eq!(
            error.message,
            "Parse Error: Expected : between key:value pair.\n{not json\n     ^"
        );
        assert_eq!(
            parse("{oops").message,
            "Parse Error: Unexpected end of string. Expected : between key:value pair.\noops\n    ^"
        );
        assert_eq!(
            parse("{ ").message,
            "Parse Error: Unexpected end of string. Expected an object key or }.\n\n^"
        );
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
            (
                CatalogError::Corrupt("/secret token=private".into()),
                generic,
            ),
        ] {
            let error = ApiError::from(cause);
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
