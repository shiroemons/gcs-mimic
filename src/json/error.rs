use crate::{auth::AuthFailure, bucket::CatalogError, object::ObjectError};
use http_body_util::Full;
use hyper::{Response, StatusCode, body::Bytes, header};
use serde_json::{Value, json};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    pub status: StatusCode,
    pub reason: &'static str,
    pub message: String,
    pub allow: Option<&'static str>,
    /// Request header named by `locationType`/`location` in the error item, if any.
    pub header_location: Option<&'static str>,
}

impl JsonError {
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

    pub fn new(status: StatusCode, reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            reason,
            message: message.into(),
            allow: None,
            header_location: None,
        }
    }

    pub fn invalid_parameter(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalidParameter", message)
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalidArgument", message)
    }

    pub fn forbidden() -> Self {
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

    pub fn value(&self) -> Value {
        let mut item = json!({"message": self.message, "domain": "global", "reason": self.reason});
        if let Some(location) = self.header_location {
            item["locationType"] = "header".into();
            item["location"] = location.into();
        }
        json!({"error": {"code": self.status.as_u16(), "message": self.message, "errors": [item]}})
    }

    pub fn into_response(self) -> Response<Full<Bytes>> {
        if self.status == StatusCode::NOT_MODIFIED {
            return Response::builder()
                .status(self.status)
                .body(Full::new(Bytes::new()))
                .unwrap();
        }
        let mut response = Response::new(Full::new(Bytes::from(self.value().to_string())));
        *response.status_mut() = self.status;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json; charset=UTF-8"),
        );
        if let Some(allow) = self.allow {
            response
                .headers_mut()
                .insert(header::ALLOW, header::HeaderValue::from_static(allow));
        }
        response
    }
}

impl From<AuthFailure> for JsonError {
    fn from(failure: AuthFailure) -> Self {
        match failure {
            // Documented status-code example for a missing credential.
            AuthFailure::LoginRequired => {
                let mut error = Self::new(StatusCode::UNAUTHORIZED, "required", "Login Required");
                error.header_location = Some("Authorization");
                error
            }
            // The reason is documented; message and location are a local choice.
            AuthFailure::InvalidCredentials => {
                let mut error =
                    Self::new(StatusCode::UNAUTHORIZED, "authError", "Invalid Credentials");
                error.header_location = Some("Authorization");
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
            CatalogError::Unavailable(_)
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
                Self::new(StatusCode::NOT_FOUND, "notFound", "Object not found")
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
            ),
            ObjectError::OverwriteForbidden | ObjectError::Forbidden => Self::forbidden(),
            ObjectError::ActiveHold => Self::new(
                StatusCode::FORBIDDEN,
                "objectUnderActiveHold",
                "Object replacement or deletion is not allowed due to an active hold on the object.",
            ),
            ObjectError::RetentionPolicyNotMet => Self::new(
                StatusCode::FORBIDDEN,
                "retentionPolicyNotMet",
                "Object retention period has not elapsed; wait until retentionExpirationTime or remove an unlocked bucket retention policy.",
            ),
            // The object-lock guide documents denial of retained deletes and replacements;
            // the reason string is shared with bucket retention as a local choice.
            ObjectError::ObjectRetentionNotMet => Self::new(
                StatusCode::FORBIDDEN,
                "retentionPolicyNotMet",
                "Object is retained until retention.retainUntilTime; wait until then, or shorten or remove an Unlocked configuration with overrideUnlockedRetention=true.",
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
            ObjectError::Unavailable(message) => Self::backend(&message),
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

fn safe_backend_cause(cause: &str) -> &'static str {
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
