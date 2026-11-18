//! How the XML API answers the errors of shared request parsing and of the domain. Each answer
//! keeps the code, message and details the XML adapter has always sent for that failure.

use super::{Error, XmlCode};
use crate::{
    auth::InvalidProjectId,
    bucket::name::ProfileNameError,
    percent::{MalformedEncoding, QueryError},
    time_format::TimeOutOfRange,
};

/// A stored time that cannot be rendered is a backend failure.
impl From<TimeOutOfRange> for Error {
    fn from(error: TimeOutOfRange) -> Self {
        Self::new(XmlCode::ServiceUnavailable, error.to_string())
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
