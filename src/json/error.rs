use crate::{auth::AuthFailure, bucket::CatalogError, object::ObjectError};
use http_body_util::Full;
use hyper::{Response, StatusCode, body::Bytes, header};
use serde_json::{Value, json};
use std::fmt;

/// The `Cache-Control` Cloud Storage sends with JSON API errors and media downloads.
pub(crate) const NO_STORE_CACHE_CONTROL: &str = "no-cache, no-store, max-age=0, must-revalidate";

/// The `Cache-Control` of a media download that Cloud Storage answers with a plain error.
const MEDIA_ERROR_CACHE_CONTROL: &str = "private, max-age=0";

/// The missing-object message for operations that do not name one object, such as a copy source.
const OBJECT_NOT_FOUND: &str = "Object not found";
/// The prefix of Cloud Storage's missing-object message, followed by `<bucket>/<object>`.
const NO_SUCH_OBJECT: &str = "No such object: ";

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
    pub status: StatusCode,
    pub reason: &'static str,
    pub message: String,
    pub allow: Option<&'static str>,
    /// The `locationType`/`location` pair of the error item, if any. A reference keeps the error
    /// small, which `Result` returns throughout the crate depend on.
    pub location: Option<&'static ErrorLocation>,
}

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
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "backendError",
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
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "backendError", message)
    }

    pub(crate) fn new(
        status: StatusCode,
        reason: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            reason,
            message: message.into(),
            allow: None,
            location: None,
        }
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

    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalidArgument", message)
    }

    pub(crate) fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", "Forbidden")
    }

    pub fn unsupported(name: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "badRequest",
            format!(
                "gcs-mimic does not support bucket API setting {name}; omit this setting (for example, create a bucket with only name)."
            ),
        )
    }

    pub fn unsupported_object(name: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "badRequest",
            format!(
                "gcs-mimic does not support object API setting {name}; omit this option or header (for example, use projection=noAcl)."
            ),
        )
    }

    pub(crate) fn value(&self) -> Value {
        let mut item = json!({"message": self.message, "domain": "global", "reason": self.reason});
        if let Some(location) = self.location {
            item["locationType"] = location.kind.into();
            item["location"] = location.name.into();
        }
        json!({"error": {"code": self.status.as_u16(), "message": self.message, "errors": [item]}})
    }

    /// Whether this is the catalog's missing-object error, before or after it names the object.
    pub(crate) fn is_missing_object(&self) -> bool {
        self.status == StatusCode::NOT_FOUND
            && self.reason == "notFound"
            && (self.message == OBJECT_NOT_FOUND || self.message.starts_with(NO_SUCH_OBJECT))
    }

    /// Cloud Storage names the requested object in its not-found message, which the catalog
    /// error cannot do because it does not know which request selected the object.
    pub(crate) fn naming_object(mut self, bucket: &str, name: &str) -> Self {
        if self.is_missing_object() {
            self.message = format!("{NO_SUCH_OBJECT}{bucket}/{name}");
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
        let mut response = Response::new(Full::new(Bytes::from(escape_html(&self.message))));
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
        let mut response = Response::new(Full::new(Bytes::from(super::render_body(&self.value()))));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json; charset=UTF-8"),
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
                let mut error = Self::new(StatusCode::UNAUTHORIZED, "required", "Login Required");
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

impl From<CatalogError> for JsonError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::AlreadyExists => {
                Self::new(StatusCode::CONFLICT, "conflict", "Bucket already exists")
            }
            CatalogError::NotFound => {
                Self::new(StatusCode::NOT_FOUND, "notFound", "Bucket not found")
            }
            CatalogError::NotEmpty => {
                Self::new(StatusCode::CONFLICT, "conflict", "Bucket is not empty")
            }
            CatalogError::PreconditionFailed => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "conditionNotMet",
                "Precondition failed",
            ),
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
            CatalogError::InvalidMetadata(reason) => Self::invalid_argument(reason),
            CatalogError::UnsupportedMetadata(field) => Self::unsupported(&field),
            CatalogError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull => {
                Self::storage_capacity()
            }
            CatalogError::Unavailable(_)
            | CatalogError::AuthenticationAlreadyConfigured
            | CatalogError::Io(_)
            | CatalogError::Corrupt(_)
            | CatalogError::NewerSchema { .. } => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "Bucket catalog is unavailable",
            ),
        }
    }
}

impl From<crate::storage::ResumableError> for JsonError {
    fn from(error: crate::storage::ResumableError) -> Self {
        use crate::storage::ResumableError;
        match error {
            ResumableError::NotFound => Self::new(
                StatusCode::NOT_FOUND,
                "notFound",
                "Upload session not found",
            ),
            ResumableError::Gone => Self::new(
                StatusCode::GONE,
                "gone",
                "Upload session is no longer active",
            ),
            ResumableError::Invalid(message) => Self::invalid_parameter(message),
            ResumableError::Object(error) => error.into(),
        }
    }
}

impl From<ObjectError> for JsonError {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::BucketNotFound => {
                Self::new(StatusCode::NOT_FOUND, "notFound", "Bucket not found")
            }
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
            ObjectError::DerivedTransferBusy => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "Derived transfer is already in use",
            ),
            ObjectError::UnsupportedMetadata(field) => Self::unsupported_object(&field),
            ObjectError::CustomerEncryption(error) => error.into(),
            ObjectError::NotModified => {
                Self::new(StatusCode::NOT_MODIFIED, "notModified", "Not modified")
            }
            ObjectError::ObjectNotFound => {
                Self::new(StatusCode::NOT_FOUND, "notFound", OBJECT_NOT_FOUND)
            }
            ObjectError::ObjectNotSoftDeleted => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "objectNotSoftDeleted",
                error.to_string(),
            ),
            ObjectError::PreconditionFailed => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "conditionNotMet",
                "Precondition failed",
            )
            .at_if_match(),
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
            ObjectError::ActiveHold => Self::new(
                StatusCode::FORBIDDEN,
                "objectUnderActiveHold",
                "Object replacement or deletion is not allowed due to an active hold on the object.",
            ),
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
            ),
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
            ),
            ObjectError::InvalidFixedMetadata(reason) => Self::invalid_argument(reason),
            ObjectError::PublicAccessPrevention => CatalogError::PublicAccessPrevention.into(),
            ObjectError::Unavailable(cause) => Self::backend(cause.message()),
            ObjectError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull => {
                Self::storage_capacity()
            }
            ObjectError::Io(kind) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                format!(
                    "Object storage is unavailable: storage I/O error ({kind}); check data directory permissions and available disk space"
                ),
            ),
            ObjectError::Corrupt(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
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
