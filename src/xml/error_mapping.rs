//! How the XML API answers the errors of shared request parsing and of the domain. Each answer
//! keeps the code, message and details the XML adapter has always sent for that failure.

use super::Error;
use crate::percent::{MalformedEncoding, QueryError};

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
