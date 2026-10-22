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
            StatusCode::NOT_IMPLEMENTED,
            "notImplemented",
            format!("Unsupported bucket API semantics: {name}"),
        )
    }

    pub fn unsupported_object(name: &str) -> Self {
        Self::new(
            StatusCode::NOT_IMPLEMENTED,
            "notImplemented",
            format!("Unsupported object API semantics: {name}"),
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
            CatalogError::Unavailable(_) => Self::new(
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
            ObjectError::UnsupportedMetadata(field) => Self::unsupported_object(&field),
            ObjectError::NotModified => {
                Self::new(StatusCode::NOT_MODIFIED, "notModified", "Not modified")
            }
            ObjectError::ObjectNotFound => {
                Self::new(StatusCode::NOT_FOUND, "notFound", "Object not found")
            }
            ObjectError::PreconditionFailed => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "conditionNotMet",
                "Precondition failed",
            ),
            ObjectError::OverwriteForbidden | ObjectError::Forbidden => Self::forbidden(),
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
            ObjectError::Unavailable(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "Object storage is unavailable",
            ),
        }
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.status, self.message)
    }
}

impl std::error::Error for JsonError {}
