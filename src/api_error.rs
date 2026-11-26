//! The error the request handlers share: an HTTP status with Cloud Storage's `reason` and
//! message, the kind an adapter switches on, and the conversions from the domain errors.
//!
//! The shared server logic and the request parsers return it. Each adapter answers it in its
//! own protocol: the JSON adapter renders the error envelope (`crate::json::error`), and the
//! XML and gRPC adapters map it to their codes and statuses. It lives outside the adapters so
//! that none of them has to name another to read a failure.

use crate::{
    auth::{AuthFailure, Denied},
    bucket::CatalogError,
    object::ObjectError,
    storage::failure::{
        BackendCause, BackendFailure,
        wording::{self, BUCKET_NOT_FOUND, BackendNotice, OBJECT_NOT_FOUND, PRECONDITION_NOT_HELD},
    },
};
use hyper::StatusCode;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
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
    /// Keeps the typed timeout cause available to classification tests after conversion.
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
    /// Lets the XML adapter give this refusal Cloud Storage's own XML wording.
    UniformAccessAcl,
    /// A storage class the bucket's location cannot hold, which the XML and Storage v2 adapters
    /// answer with Cloud Storage's own code and details.
    ClassLocation,
    /// Parse failures whose wire reason the legacy bucket adapter changes to `invalid`.
    LegacyParseError,
    MalformedJson,
    FieldMaskSyntax,
    PreconditionFailed,
    NotModified,
    WriteNotModified,
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
        .with_kind(ErrorKind::PreconditionFailed)
    }

    pub(crate) fn not_modified() -> Self {
        Self::new(StatusCode::NOT_MODIFIED, "notModified", "Not modified")
            .with_kind(ErrorKind::NotModified)
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

    /// The JSON API's 304 for a write whose not-match condition named the current value. Its
    /// own kind lets the gRPC adapter keep reporting the failed precondition it always has.
    fn write_not_modified() -> Self {
        Self::not_modified().with_kind(ErrorKind::WriteNotModified)
    }

    /// A request body that is not JSON, with the wording the JSON adapter quotes for it.
    pub(crate) fn malformed_json(message: impl Into<String>) -> Self {
        Self::invalid(message).with_kind(ErrorKind::MalformedJson)
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
        Self::new(StatusCode::BAD_REQUEST, "required", message).with_kind(ErrorKind::Required)
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
            self.message = wording::missing_named_object(bucket, name);
            self.kind = ErrorKind::NamedMissingObject;
        }
        self
    }
}

impl From<crate::percent::MalformedEncoding> for ApiError {
    fn from(error: crate::percent::MalformedEncoding) -> Self {
        Self::invalid_parameter(error.to_string())
    }
}

impl From<crate::percent::QueryError> for ApiError {
    fn from(error: crate::percent::QueryError) -> Self {
        Self::invalid_parameter(error.to_string())
    }
}

/// A bucket named in a path or parameter: the native refusal is a malformed parameter, while the
/// fake-gcs-server profile reports an invalid value.
impl From<crate::bucket::name::ProfileNameError> for ApiError {
    fn from(error: crate::bucket::name::ProfileNameError) -> Self {
        use crate::bucket::name::ProfileNameError;
        match error {
            ProfileNameError::Native(_) => Self::invalid_parameter(error.to_string()),
            ProfileNameError::FakeGcsServer => Self::invalid(error.to_string()),
        }
    }
}

/// A refused bucket token is a malformed parameter and a refused object token an invalid value,
/// as Cloud Storage reported them.
impl From<crate::page_token::PageTokenError> for ApiError {
    fn from(error: crate::page_token::PageTokenError) -> Self {
        use crate::page_token::PageTokenError;
        match error {
            PageTokenError::BucketCursor => Self::invalid_parameter(error.to_string()),
            PageTokenError::ObjectCursor(_) => Self::invalid(error.to_string()),
            PageTokenError::Project(error) => error.into(),
            PageTokenError::BucketName(error) => error.into(),
        }
    }
}

impl From<crate::hashing::InvalidDigest> for ApiError {
    fn from(error: crate::hashing::InvalidDigest) -> Self {
        Self::invalid_parameter(error.to_string())
    }
}

/// A stored time that cannot be rendered is a backend failure.
impl From<crate::time_format::TimeOutOfRange> for ApiError {
    fn from(error: crate::time_format::TimeOutOfRange) -> Self {
        Self::service_unavailable(error.to_string())
    }
}

/// A project path segment outside the identifier rules is a malformed parameter.
impl From<crate::auth::InvalidProjectId> for ApiError {
    fn from(error: crate::auth::InvalidProjectId) -> Self {
        Self::invalid_parameter(error.to_string())
    }
}

/// The JSON API names the canned ACL parameter `predefinedAcl` on a metadata write.
impl From<crate::acl::ConflictingAcl> for ApiError {
    fn from(_: crate::acl::ConflictingAcl) -> Self {
        Self::unsupported_object("acl together with predefinedAcl").at_parameter("predefinedAcl")
    }
}

/// A refused `matchGlob` pattern is a malformed request; the pattern defect is the message.
impl From<crate::match_glob::GlobError> for ApiError {
    fn from(error: crate::match_glob::GlobError) -> Self {
        Self::invalid(error.to_string())
    }
}

/// A URL that cannot be built becomes the error body text; `UrlError` names the defect and
/// has no underlying cause to keep.
impl From<crate::url::UrlError> for ApiError {
    fn from(error: crate::url::UrlError) -> Self {
        Self::invalid_parameter(error.to_string())
    }
}

impl From<crate::auth::AuthError> for ApiError {
    fn from(error: crate::auth::AuthError) -> Self {
        match error {
            crate::auth::AuthError::Failure(failure) => failure.into(),
            crate::auth::AuthError::Unavailable(cause) => Self::backend(&cause),
        }
    }
}

impl From<AuthFailure> for ApiError {
    fn from(failure: AuthFailure) -> Self {
        match failure {
            // Documented status-code example for a missing credential.
            AuthFailure::LoginRequired => Self::new(
                StatusCode::UNAUTHORIZED,
                "required",
                wording::LOGIN_REQUIRED,
            )
            .with_kind(ErrorKind::Required)
            .at(&AUTHORIZATION_HEADER),
            // The reason is documented; message and location are a local choice.
            AuthFailure::InvalidCredentials => Self::new(
                StatusCode::UNAUTHORIZED,
                "authError",
                wording::INVALID_CREDENTIALS,
            )
            .at(&AUTHORIZATION_HEADER),
            AuthFailure::Denied(denied) => denied.into(),
            AuthFailure::Forbidden => Self::forbidden(),
        }
    }
}

/// The documented 403 `forbidden` reason with Cloud Storage's longer message naming the
/// principal and the refused permission.
impl From<Box<Denied>> for ApiError {
    fn from(denied: Box<Denied>) -> Self {
        let mut error = Self::new(StatusCode::FORBIDDEN, "forbidden", denied.to_string());
        error.denied = Some(denied);
        error
    }
}

impl From<crate::csek::CsekError> for ApiError {
    fn from(error: crate::csek::CsekError) -> Self {
        Self::new(StatusCode::BAD_REQUEST, error.reason(), error.message())
            .with_kind(ErrorKind::Csek(error))
    }
}

impl From<CatalogError> for ApiError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::AlreadyExists => Self::new(
                StatusCode::CONFLICT,
                "conflict",
                wording::BUCKET_ALREADY_OWNED,
            ),
            CatalogError::NotFound => Self::missing_bucket(),
            CatalogError::RestoreConflict => {
                Self::new(StatusCode::CONFLICT, "conflict", wording::RESTORE_CONFLICT)
            }
            CatalogError::NotEmpty => {
                Self::new(StatusCode::CONFLICT, "conflict", "Bucket is not empty")
            }
            CatalogError::PreconditionFailed | CatalogError::NotModified => {
                Self::precondition_not_held()
            }
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
                wording::PUBLIC_ACCESS_PREVENTED,
            ),
            CatalogError::InvalidMetadata(reason) => Self::invalid(reason),
            CatalogError::Metadata(error) => Self::metadata(&error, Self::unsupported),
            CatalogError::UnsupportedMetadata(field) => Self::unsupported(&field),
            CatalogError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull => {
                Self::service_unavailable(wording::storage_capacity(&cause))
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

impl From<crate::storage::ResumableError> for ApiError {
    fn from(error: crate::storage::ResumableError) -> Self {
        use crate::storage::ResumableError;
        match error {
            ResumableError::NotFound => Self::new(
                StatusCode::NOT_FOUND,
                "notFound",
                "Upload session not found",
            )
            .with_plain(&NOT_FOUND_TEXT),
            // Unrecorded terminal validation failures keep the existing local 410 response.
            ResumableError::Gone => Self::new(
                StatusCode::GONE,
                "gone",
                "Upload session is no longer active",
            )
            .with_plain(&SESSION_MESSAGE_TEXT),
            ResumableError::Invalid(message) => Self::invalid_parameter(message),
            ResumableError::Cancelled => Self::cancelled_upload(),
            ResumableError::InvalidContentRange => {
                Self::invalid_parameter("Failed to parse Content-Range header.")
                    .with_plain(&CONTENT_RANGE_TEXT)
            }
            // The recorded refusal is a bare text page without cache fields.
            ResumableError::Overrun(overrun) => Self::session_protocol(overrun.to_string()),
            ResumableError::Object(error) => error.into(),
        }
    }
}

impl ApiError {
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
impl From<Box<crate::object::ComponentFailure>> for ApiError {
    fn from(failure: Box<crate::object::ComponentFailure>) -> Self {
        use crate::object::ComponentFailureKind;
        match failure.kind {
            ComponentFailureKind::NotFound => {
                Self::named_missing_object(&failure.bucket, &failure.name)
            }
            ComponentFailureKind::GenerationMismatch { required, actual } => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "conditionNotMet",
                wording::component_generation_mismatch(
                    &failure.bucket,
                    &failure.name,
                    required,
                    actual,
                ),
            ),
        }
    }
}

impl From<Box<crate::object::HeldObject>> for ApiError {
    fn from(held: Box<crate::object::HeldObject>) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            wording::held_object(&held),
        )
        .with_kind(ErrorKind::ActiveHold)
    }
}

/// Bucket retention policies and object retention share the reason and wording (observed
/// 2026-10-31, including the refused overwrite).
impl From<Box<crate::object::RetainedObject>> for ApiError {
    fn from(retained: Box<crate::object::RetainedObject>) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "retentionPolicyNotMet",
            wording::retained_object(&retained),
        )
        .with_kind(ErrorKind::RetentionNotMet)
    }
}

/// An unavailable or corrupt backend, keeping the fixed failure it reports for classification.
impl From<BackendNotice> for ApiError {
    fn from(notice: BackendNotice) -> Self {
        let error = Self::service_unavailable(notice.message);
        match notice.failure {
            Some(failure) => error.with_kind(ErrorKind::Backend(failure)),
            None => error,
        }
    }
}

impl From<crate::object::FixedMetadataError> for ApiError {
    fn from(error: crate::object::FixedMetadataError) -> Self {
        ObjectError::from(error).into()
    }
}

impl From<ObjectError> for ApiError {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::BucketNotFound => Self::missing_bucket(),
            ObjectError::Component(failure) => failure.into(),
            ObjectError::Metadata(error) => Self::metadata(&error, Self::unsupported_object),
            ObjectError::InvalidDerivedArgument(reason) => Self::invalid_argument(reason),
            ObjectError::ComposeClassMismatch { .. } => Self::invalid(error.to_string()),
            ObjectError::DerivedTransferIncomplete => {
                Self::invalid_argument(wording::DERIVED_TRANSFER_INCOMPLETE)
            }
            ObjectError::DerivedTransferNotFound => Self::new(
                StatusCode::NOT_FOUND,
                "notFound",
                wording::DERIVED_TRANSFER_NOT_FOUND,
            ),
            ObjectError::DerivedTransferExpired => {
                Self::new(StatusCode::GONE, "gone", wording::DERIVED_TRANSFER_EXPIRED)
            }
            ObjectError::DerivedTransferBusy => {
                Self::service_unavailable(wording::DERIVED_TRANSFER_BUSY)
            }
            ObjectError::UnsupportedMetadata(field) => Self::unsupported_object(&field),
            ObjectError::CustomerEncryption(error) => error.into(),
            ObjectError::NotModified => Self::not_modified(),
            ObjectError::WriteNotModified => Self::write_not_modified(),
            ObjectError::ObjectNotFound => Self::missing_object(),
            ObjectError::ObjectNotSoftDeleted => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "objectNotSoftDeleted",
                error.to_string(),
            ),
            ObjectError::SoftDeletePolicyRequired(request) => {
                Self::invalid(request.policy_required_message())
            }
            // A copy source's not-match is a failed source condition, not a conditional read.
            ObjectError::PreconditionFailed | ObjectError::SourceNotModified => {
                Self::precondition_not_held().at_if_match()
            }
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
            ObjectError::ActiveHold(held) => held.into(),
            ObjectError::ObjectRetentionNotMet(retained) => retained.into(),
            ObjectError::HashMismatch(mismatch) => Self::invalid(wording::hash_mismatch(&mismatch)),
            ObjectError::ChecksumMismatch => Self::new(
                StatusCode::BAD_REQUEST,
                "invalid",
                wording::CHECKSUM_MISMATCH,
            ),
            ObjectError::TooLarge => Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "uploadTooLarge",
                wording::UPLOAD_TOO_LARGE,
            ),
            ObjectError::BodyAborted(_) => {
                Self::new(StatusCode::BAD_REQUEST, "badRequest", wording::BODY_ABORTED)
                    .with_kind(ErrorKind::IncompleteBody)
            }
            ObjectError::InvalidFixedMetadata(reason) => Self::invalid(reason),
            ObjectError::UniformAccessPredefinedAcl => {
                Self::invalid(crate::acl::UNIFORM_ACCESS_UPLOAD_ACL)
                    .with_kind(ErrorKind::UniformAccessAcl)
            }
            ObjectError::UniformAccessAclUpdate => {
                Self::invalid(crate::acl::UNIFORM_ACCESS_UPDATE_OBJECT_ACL)
                    .with_kind(ErrorKind::UniformAccessAcl)
            }
            ObjectError::PublicAccessPrevention => CatalogError::PublicAccessPrevention.into(),
            ObjectError::Unavailable(cause) => Self::backend(&cause),
            ObjectError::Io(cause) => Self::service_unavailable(wording::object_io(&cause)),
            ObjectError::Corrupt(cause) => wording::corrupt_object(&cause).into(),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.status, self.message)
    }
}

impl std::error::Error for ApiError {}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::failure::{test_support::denied, wording::safe_backend_cause};
    use serde_json::{Value, json};

    /// A tester whose Docker disk filled under `backend=disk` was told about RAM budgets first;
    /// each storage that can run out now names only its own remedy.
    #[test]
    fn storage_capacity_names_only_the_remedy_of_the_storage_that_ran_out() {
        use crate::storage::failure::{IoCause, MemoryBudget, MemoryExhausted};
        let memory = |budget| IoCause::from(MemoryExhausted::new(budget));
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
            let object = ApiError::from(ObjectError::Io(cause.clone()));
            let catalog = ApiError::from(CatalogError::Io(cause));
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
        let mut bucket = ApiError::from(CatalogError::NotFound);
        bucket.message = "different bucket wording".into();
        bucket.reason = "differentReason";
        assert!(bucket.is_missing_bucket());
        let mut object = ApiError::from(ObjectError::ObjectNotFound).naming_object("b", "o");
        assert_eq!(object.message, "No such object: b/o");
        object.message = "different object wording".into();
        assert!(object.is_missing_object());
        assert_eq!(
            object.naming_object("other", "other").message,
            "different object wording"
        );
        let unrelated = ApiError::invalid_parameter(OBJECT_NOT_FOUND);
        assert!(!unrelated.is_missing_object());
    }

    /// The logged event names the anonymous caller the response answers, with the refused
    /// permission and resource, even when the refusal named a stored principal.
    #[test]
    fn an_anonymous_callers_refusal_logs_the_anonymous_caller() {
        let event = |error: ApiError| {
            error
                .for_anonymous_caller()
                .denied
                .map(|denied| denied.event_value())
        };
        let anonymous = json!({"anonymous": true, "permission": "storage.buckets.get",
            "resource": "bucket", "bucket": "bucket"});
        assert_eq!(
            event(ApiError::from(CatalogError::Denied(denied(None)))),
            Some(anonymous.clone())
        );
        assert_eq!(
            event(ApiError::from(ObjectError::Denied(denied(Some(
                "snapshot@example.invalid"
            ))))),
            Some(anonymous)
        );
        assert_eq!(event(ApiError::from(ObjectError::LockedRetention)), None);
    }

    #[test]
    fn holds_and_compose_components_are_named_in_their_errors() {
        use crate::object::{ComponentFailure, ComponentFailureKind, HeldObject, HoldKind};
        let held = |kind| {
            ApiError::from(ObjectError::ActiveHold(Box::new(HeldObject {
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
            ApiError::from(ObjectError::Component(Box::new(ComponentFailure {
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
        if std::env::var_os("GCSM_TEST_BACKEND_LOG").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "api_error::tests::backend_causes_are_actionable_and_private_details_stay_private",
                    "--nocapture",
                ])
                .env("GCSM_TEST_BACKEND_LOG", "1")
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
            let error = ApiError::from(ObjectError::Unavailable(cause.into()));
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(error.message.contains(cause.message()));
        }
        let error = ApiError::from(ObjectError::Unavailable(
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
            let cause = cause.trim_prefix("catalog unavailable: ");
            assert_eq!(safe_backend_cause(cause), expected);
            let error = ApiError::backend(&cause.into());
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
                ApiError::from(CatalogError::Unavailable(
                    "database is locked: /secret token=private".into(),
                )),
                catalog,
            ),
            (
                ApiError::from(CatalogError::Corrupt(
                    "invalid opaque blob ID: /secret token=private".into(),
                )),
                catalog,
            ),
            (
                ApiError::from(CatalogError::Io(
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "Permission denied: /secret token=private",
                    )
                    .into(),
                )),
                catalog,
            ),
            (
                ApiError::from(CatalogError::AuthenticationAlreadyConfigured),
                catalog,
            ),
            (
                ApiError::from(CatalogError::NewerSchema {
                    version: 99,
                    supported: 50,
                }),
                catalog,
            ),
            (
                ApiError::from(ObjectError::Corrupt(
                    "Acknowledged upload prefix is missing: /secret token=private".into(),
                )),
                corrupt,
            ),
            (
                ApiError::from(ObjectError::Corrupt("/secret token=private".into())),
                corrupt,
            ),
            (
                ApiError::from(HmacKeyError::Catalog(CatalogError::Unavailable(
                    "database is locked: /secret token=private".into(),
                ))),
                catalog,
            ),
            (
                ApiError::from(HmacKeyError::Catalog(CatalogError::Corrupt(
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

    /// Each fixed failure keeps its remedy word for word, after the object or the bucket subject.
    #[test]
    fn backend_remedies_keep_their_object_and_bucket_wording() {
        for (failure, remedy) in [
            (
                BackendFailure::CatalogClosed,
                "catalog is closed; wait for the server to restart before retrying",
            ),
            (
                BackendFailure::UploadWaitTimeout,
                "upload catalog wait timed out; retry with generation preconditions or increase catalog_wait_timeout_ms (for example, 300000; 0 disables it); a completed upload may still commit",
            ),
            (
                BackendFailure::RetainedSessionLimit,
                "retained upload session limit reached; raise max_resumable_sessions (0 is unlimited), wait for expired sessions to be reclaimed or use single-request media uploads",
            ),
            (
                BackendFailure::RewriteSessionLimit,
                "rewrite session limit reached; raise max_rewrite_sessions (0 is unlimited) or wait for unfinished rewrites to expire",
            ),
            (
                BackendFailure::FinalizationTimeout,
                "upload finalization timed out; read object state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)",
            ),
            (
                BackendFailure::CopyDeadline,
                "copy request deadline exceeded; read destination state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)",
            ),
            (
                BackendFailure::SessionBusy,
                "upload session is busy; wait for the current request to finish before sending another request for this session, or increase catalog_wait_timeout_ms (for example, 60000)",
            ),
        ] {
            let object = ApiError::from(ObjectError::Unavailable(failure.into()));
            assert_eq!(
                object.message,
                format!("Object storage is unavailable: {remedy}")
            );
            assert_eq!(object.kind, ErrorKind::Backend(failure));
            let bucket = ApiError::from(CatalogError::Unavailable(failure.into()));
            assert_eq!(
                bucket.message,
                format!("Bucket catalog is unavailable: {remedy}")
            );
            assert_eq!(bucket.kind, ErrorKind::Backend(failure));
            let corrupt = ApiError::from(ObjectError::Corrupt(failure.into()));
            assert_eq!(
                corrupt.message,
                "Object storage is unavailable: stored data is corrupt; restore a valid data directory"
            );
            assert_eq!(corrupt.kind, ErrorKind::Backend(failure));
        }
    }
}
