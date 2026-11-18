//! How the XML API answers the errors of shared request parsing and of the domain. Each answer
//! keeps the code, message and details the XML adapter has always sent for that failure.

use super::Error;
use crate::{
    auth::InvalidProjectId,
    bucket::name::ProfileNameError,
    percent::{MalformedEncoding, QueryError},
};

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
