//! Errors returned where the protocol adapters call shared bucket parsing.

use crate::{
    auth::InvalidProjectId,
    bucket::{BucketRecord, CatalogError, DefaultBucketError, resource},
    config::CompatProfile,
    input::InputError,
    time_format::TimeOutOfRange,
};
use serde_json::{Map, Value};
use std::time::SystemTime;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ServiceError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Project(#[from] InvalidProjectId),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Time(#[from] TimeOutOfRange),
}

impl From<resource::Error> for ServiceError {
    fn from(error: resource::Error) -> Self {
        match error {
            resource::Error::Input(error) => Self::Input(error),
            resource::Error::Defaults(DefaultBucketError::Project(error)) => Self::Project(error),
            resource::Error::Defaults(DefaultBucketError::Timestamp(error)) => Self::Time(error),
            resource::Error::Catalog(error) => Self::Catalog(error),
        }
    }
}

pub(crate) fn parse_bucket(
    body: &Map<String, Value>,
    project: &str,
    now: SystemTime,
    profile: CompatProfile,
    default_location: &str,
) -> Result<BucketRecord, ServiceError> {
    resource::parse_insert(body, project, now, profile, default_location).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn service_errors_fit_an_inline_input_and_a_discriminant() {
        println!(
            "service {} bytes, input {}, catalog {}",
            size_of::<ServiceError>(),
            size_of::<InputError>(),
            size_of::<CatalogError>()
        );
        assert!(
            size_of::<ServiceError>() <= size_of::<InputError>() + size_of::<usize>(),
            "service {} bytes, input {}, catalog {}",
            size_of::<ServiceError>(),
            size_of::<InputError>(),
            size_of::<CatalogError>()
        );
    }
}
