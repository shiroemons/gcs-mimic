use crate::{bucket::CatalogError, object::ObjectError};
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
}

impl JsonError {
    pub fn new(status: StatusCode, reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            reason,
            message: message.into(),
            allow: None,
        }
    }

    pub fn invalid_parameter(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalidParameter", message)
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalidArgument", message)
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
        json!({"error": {"code": self.status.as_u16(), "message": self.message,
            "errors": [{"message": self.message, "domain": "global", "reason": self.reason}]}})
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
            CatalogError::Unavailable(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "Bucket catalog is unavailable",
            ),
        }
    }
}

impl From<ObjectError> for JsonError {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::BucketNotFound => {
                Self::new(StatusCode::NOT_FOUND, "notFound", "Bucket not found")
            }
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
