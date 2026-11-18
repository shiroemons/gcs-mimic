//! How the XML API answers the errors of shared request parsing and of the domain. Each answer
//! keeps the code, message and details the XML adapter has always sent for that failure.

use super::{Error, XmlCode};
use crate::{
    auth::InvalidProjectId,
    bounded_body::BodyError,
    bucket::name::ProfileNameError,
    hashing::InvalidDigest,
    percent::{MalformedEncoding, QueryError},
    time_format::TimeOutOfRange,
};
use hyper::StatusCode;

/// A body over its bound keeps the 413 status under the generic code, as other statuses the XML
/// API has no code for do.
impl From<BodyError> for Error {
    fn from(error: BodyError) -> Self {
        let code = match error {
            BodyError::TooLarge => XmlCode::forwarded(StatusCode::PAYLOAD_TOO_LARGE),
            BodyError::OutOfMemory => XmlCode::ServiceUnavailable,
            BodyError::Unreadable => XmlCode::InvalidArgument,
        };
        Self::new(code, error.to_string())
    }
}

/// A stored time that cannot be rendered is a backend failure.
impl From<TimeOutOfRange> for Error {
    fn from(error: TimeOutOfRange) -> Self {
        Self::new(XmlCode::ServiceUnavailable, error.to_string())
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
