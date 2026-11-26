//! How the XML API answers the errors of shared request parsing and of the domain. Each answer
//! keeps the code, message and details the XML adapter has always sent for that failure.

use super::{Error, XmlCode};
use crate::multipart::MultipartError;
use crate::object::upload_headers::UploadHeaderError;

use crate::{
    auth::{AuthError, AuthFailure, Caller, Denied, InvalidProjectId},
    bounded_body::BodyError,
    bucket::{CatalogError, name::ProfileNameError},
    csek::CsekError,
    hashing::InvalidDigest,
    input::InputKind,
    metadata_error::MetadataError,
    object::{ComponentFailureKind, ObjectError, name::InvalidObjectName},
    percent::{MalformedEncoding, QueryError},
    service::ServiceError,
    storage::failure::{BackendCause, wording},
    time_format::TimeOutOfRange,
};
use hyper::StatusCode;

impl From<UploadHeaderError> for Error {
    fn from(error: UploadHeaderError) -> Self {
        ServiceError::from(error).into()
    }
}
impl From<crate::object::media_headers::StoredHeaderError> for Error {
    fn from(error: crate::object::media_headers::StoredHeaderError) -> Self {
        ServiceError::from(error).into()
    }
}

impl From<ServiceError> for Error {
    fn from(error: ServiceError) -> Self {
        match error {
            ServiceError::Input(error) => match error.kind {
                // Unsupported bucket input also had status 400, which XML maps to InvalidArgument.
                InputKind::Invalid | InputKind::Required | InputKind::Unsupported => {
                    Self::invalid(error.message)
                }
            },
            ServiceError::Project(error) => error.into(),
            ServiceError::Catalog(error) => error.into(),
            ServiceError::Multipart(error) => match error {
                MultipartError::Framing
                | MultipartError::BadContentType
                | MultipartError::MissingEnd
                | MultipartError::UnsupportedParameter(_) => Self::invalid(error.to_string()),
            },
            ServiceError::UploadHeader(error) => match error {
                UploadHeaderError::InvalidContentType
                | UploadHeaderError::DuplicateContentType
                | UploadHeaderError::UnsupportedAlgorithm => Self::invalid(error.to_string()),
                // XML PUT omits the algorithm from a malformed or repeated digest.
                UploadHeaderError::Digest(_)
                | UploadHeaderError::MalformedHash(_)
                | UploadHeaderError::Conflicting(_)
                | UploadHeaderError::DuplicateAlgorithm => {
                    Self::new(XmlCode::InvalidDigest, "Invalid x-goog-hash digest.")
                }
            },
            ServiceError::StoredHeader(error) => {
                Self::new(XmlCode::ServiceUnavailable, error.to_string())
            }
            ServiceError::Body(error) => {
                let code = match error {
                    BodyError::TooLarge => XmlCode::forwarded(StatusCode::PAYLOAD_TOO_LARGE),
                    BodyError::OutOfMemory => XmlCode::ServiceUnavailable,
                    BodyError::Unreadable => XmlCode::InvalidArgument,
                };
                Self::new(code, error.to_string())
            }
            ServiceError::Time(error) => error.into(),
        }
    }
}

impl Error {
    /// A refused permission, naming the principal and the permission as Cloud Storage does.
    fn denied(denied: Box<Denied>) -> Self {
        Self {
            code: XmlCode::AccessDenied,
            message: denied.to_string(),
            details: None,
            denied: Some(denied),
        }
    }

    /// A metadata refusal: a class the bucket's location cannot hold, a recognized setting
    /// gcs-mimic lacks (worded by `unsupported` for the resource that refused it) or an invalid
    /// value.
    fn metadata(error: &MetadataError, unsupported: fn(&str) -> String) -> Self {
        let message = error.to_string();
        if matches!(error, MetadataError::ClassLocation(_)) {
            Self::class_location(message)
        } else if error.unsupported() {
            Self::invalid(unsupported(&message))
        } else {
            Self::invalid(message)
        }
    }

    /// The answer an anonymous caller receives for a refusal: `AuthenticationRequired`, which
    /// keeps the message of a refused permission that names the anonymous caller and is the
    /// documented `Login Required` otherwise. Only the codes a refusal of the request's own
    /// access produces change; signature failures keep their codes.
    pub(crate) fn for_caller(self, caller: Option<Caller<'_>>) -> Self {
        let refusal = matches!(
            self.code,
            XmlCode::AccessDenied | XmlCode::RetentionPolicyNotMet | XmlCode::ObjectUnderActiveHold
        );
        if !refusal || !matches!(caller, Some(Caller::Anonymous)) {
            return self;
        }
        let message = match &self.denied {
            Some(denied) if denied.is_anonymous() => denied.to_string(),
            _ => wording::LOGIN_REQUIRED.to_owned(),
        };
        // A refusal can name a stored principal, such as the identity a resumable session
        // keeps; the logged event names the anonymous caller the response answers.
        Self {
            code: XmlCode::AuthenticationRequired,
            message,
            details: None,
            denied: self.denied.map(Denied::into_anonymous),
        }
    }
}

impl From<AuthFailure> for Error {
    fn from(failure: AuthFailure) -> Self {
        match failure {
            AuthFailure::LoginRequired => {
                Self::new(XmlCode::AuthenticationRequired, wording::LOGIN_REQUIRED)
            }
            AuthFailure::InvalidCredentials => {
                Self::new(XmlCode::InvalidAuthentication, wording::INVALID_CREDENTIALS)
            }
            AuthFailure::Denied(denied) => Self::denied(denied),
            AuthFailure::Forbidden => Self::new(XmlCode::AccessDenied, wording::FORBIDDEN),
        }
    }
}

impl From<AuthError> for Error {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::Failure(failure) => failure.into(),
            AuthError::Unavailable(cause) => Self::new(
                XmlCode::ServiceUnavailable,
                wording::object_backend(&cause).message,
            ),
        }
    }
}

/// Every bucket catalog failure picks its XML code here; the text it shares with the JSON API
/// and gRPC comes from [`wording`]. Statuses without an XML code of their own keep the generic
/// code, and every failed condition reads as the one XML precondition failure.
impl From<CatalogError> for Error {
    fn from(error: CatalogError) -> Self {
        let conflict = || XmlCode::forwarded(StatusCode::CONFLICT);
        match error {
            CatalogError::AlreadyExists => Self::new(conflict(), wording::BUCKET_ALREADY_OWNED),
            CatalogError::NotFound => Self::new(XmlCode::NoSuchBucket, wording::BUCKET_NOT_FOUND),
            CatalogError::RestoreConflict => Self::new(conflict(), wording::RESTORE_CONFLICT),
            CatalogError::NotEmpty => Self::new(conflict(), "Bucket is not empty"),
            CatalogError::PreconditionFailed
            | CatalogError::NotModified
            | CatalogError::PublicAccessPrevention => Self::precondition_failed(),
            CatalogError::Denied(denied) => Self::denied(denied),
            CatalogError::Forbidden => Self::new(XmlCode::AccessDenied, wording::FORBIDDEN),
            error @ (CatalogError::RetentionPolicyLocked(_)
            | CatalogError::RetentionPolicyNotSet) => Self::invalid(error.to_string()),
            CatalogError::InvalidMetadata(reason) => Self::invalid(reason),
            CatalogError::Metadata(error) => {
                Self::metadata(&error, wording::unsupported_bucket_setting)
            }
            CatalogError::UnsupportedMetadata(field) => {
                Self::invalid(wording::unsupported_bucket_setting(&field))
            }
            CatalogError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull => {
                Self::new(
                    XmlCode::ServiceUnavailable,
                    wording::storage_capacity(&cause),
                )
            }
            CatalogError::Unavailable(cause) | CatalogError::Corrupt(cause) => Self::new(
                XmlCode::ServiceUnavailable,
                wording::catalog_backend(&cause).message,
            ),
            error @ (CatalogError::AuthenticationAlreadyConfigured
            | CatalogError::Io(_)
            | CatalogError::NewerSchema { .. }) => Self::new(
                XmlCode::ServiceUnavailable,
                wording::catalog_backend(&BackendCause::from_error(error)).message,
            ),
        }
    }
}

/// Every object failure picks its XML code here, as [`CatalogError`] does. A missing object
/// keeps the fixed XML message, whose `Details` the caller adds by naming the object.
impl From<ObjectError> for Error {
    fn from(error: ObjectError) -> Self {
        let unavailable = |message: String| Self::new(XmlCode::ServiceUnavailable, message);
        match error {
            ObjectError::BucketNotFound => {
                Self::new(XmlCode::NoSuchBucket, wording::BUCKET_NOT_FOUND)
            }
            ObjectError::ObjectNotFound => Self::new(XmlCode::NoSuchKey, super::NO_SUCH_KEY),
            ObjectError::Component(failure) => match failure.kind {
                ComponentFailureKind::NotFound => Self::new(XmlCode::NoSuchKey, super::NO_SUCH_KEY),
                ComponentFailureKind::GenerationMismatch { .. } => Self::precondition_failed(),
            },
            ObjectError::DerivedTransferNotFound => {
                Self::new(XmlCode::NoSuchKey, wording::DERIVED_TRANSFER_NOT_FOUND)
            }
            ObjectError::DerivedTransferExpired => Self::new(
                XmlCode::forwarded(StatusCode::GONE),
                wording::DERIVED_TRANSFER_EXPIRED,
            ),
            ObjectError::NotModified | ObjectError::WriteNotModified => {
                Self::new(XmlCode::forwarded(StatusCode::NOT_MODIFIED), "Not modified")
            }
            ObjectError::PreconditionFailed
            | ObjectError::SourceNotModified
            | ObjectError::ObjectNotSoftDeleted
            | ObjectError::PublicAccessPrevention => Self::precondition_failed(),
            ObjectError::OverwriteForbidden(denied) | ObjectError::Denied(denied) => {
                Self::denied(denied)
            }
            ObjectError::Forbidden => Self::new(XmlCode::AccessDenied, wording::FORBIDDEN),
            ObjectError::UnlockedRetentionOverrideRequired => Self::new(
                XmlCode::AccessDenied,
                crate::object::UNLOCKED_RETENTION_OVERRIDE_MESSAGE,
            ),
            ObjectError::LockedRetention => Self::new(
                XmlCode::AccessDenied,
                crate::object::LOCKED_RETENTION_MESSAGE,
            ),
            ObjectError::ActiveHold(held) => {
                Self::new(XmlCode::ObjectUnderActiveHold, wording::held_object(&held))
            }
            ObjectError::ObjectRetentionNotMet(retained) => Self::new(
                XmlCode::RetentionPolicyNotMet,
                wording::retained_object(&retained),
            ),
            ObjectError::Metadata(error) => {
                Self::metadata(&error, wording::unsupported_object_setting)
            }
            ObjectError::UnsupportedMetadata(field) => {
                Self::invalid(wording::unsupported_object_setting(&field))
            }
            ObjectError::InvalidDerivedArgument(reason)
            | ObjectError::InvalidFixedMetadata(reason) => Self::invalid(reason),
            error @ ObjectError::ComposeClassMismatch { .. } => Self::invalid(error.to_string()),
            ObjectError::SoftDeletePolicyRequired(request) => {
                Self::invalid(request.policy_required_message())
            }
            ObjectError::DerivedTransferIncomplete => {
                Self::invalid(wording::DERIVED_TRANSFER_INCOMPLETE)
            }
            ObjectError::HashMismatch(mismatch) => Self::invalid(wording::hash_mismatch(&mismatch)),
            ObjectError::ChecksumMismatch => Self::invalid(wording::CHECKSUM_MISMATCH),
            ObjectError::BodyAborted(_) => Self::invalid(wording::BODY_ABORTED),
            ObjectError::UniformAccessPredefinedAcl | ObjectError::UniformAccessAclUpdate => {
                Self::uniform_access_acl()
            }
            ObjectError::CustomerEncryption(error) => error.into(),
            ObjectError::TooLarge => Self::new(
                XmlCode::forwarded(StatusCode::PAYLOAD_TOO_LARGE),
                wording::UPLOAD_TOO_LARGE,
            ),
            ObjectError::DerivedTransferBusy => unavailable(wording::DERIVED_TRANSFER_BUSY.into()),
            ObjectError::Unavailable(cause) => {
                unavailable(wording::object_backend(&cause).message.into())
            }
            ObjectError::Io(cause) => unavailable(wording::object_io(&cause).into()),
            ObjectError::Corrupt(cause) => {
                unavailable(wording::corrupt_object(&cause).message.into())
            }
        }
    }
}

/// A body over its bound keeps the 413 status under the generic code, as other statuses the XML
/// API has no code for do.
impl From<BodyError> for Error {
    fn from(error: BodyError) -> Self {
        ServiceError::from(error).into()
    }
}

/// A stored time that cannot be rendered is a backend failure.
impl From<TimeOutOfRange> for Error {
    fn from(error: TimeOutOfRange) -> Self {
        Self::new(XmlCode::ServiceUnavailable, error.to_string())
    }
}

/// Each customer-supplied key failure has its own XML code and the shared message.
impl From<CsekError> for Error {
    fn from(error: CsekError) -> Self {
        Self::new(XmlCode::Csek(error), error.message())
    }
}

/// The XML API names no defect of a rejected object name.
impl From<InvalidObjectName> for Error {
    fn from(_: InvalidObjectName) -> Self {
        Self::invalid("Invalid object name")
    }
}

impl From<InvalidDigest> for Error {
    fn from(error: InvalidDigest) -> Self {
        Self::invalid(error.to_string())
    }
}

impl From<InvalidProjectId> for Error {
    fn from(error: InvalidProjectId) -> Self {
        Self::invalid(error.to_string())
    }
}

impl From<ProfileNameError> for Error {
    fn from(error: ProfileNameError) -> Self {
        Self::invalid(error.to_string())
    }
}

impl From<MalformedEncoding> for Error {
    fn from(error: MalformedEncoding) -> Self {
        Self::invalid(error.to_string())
    }
}

impl From<QueryError> for Error {
    fn from(error: QueryError) -> Self {
        Self::invalid(error.to_string())
    }
}

#[cfg(test)]
mod golden_tests;
