//! Request handling that the JSON, XML and gRPC adapters share: the server state, request
//! context and activity, request and response bodies, access checks, media negotiation, object
//! request header rules, upload driving and copies, and the errors it returns to the adapters.

use crate::{
    auth::InvalidProjectId,
    bounded_body::BodyError,
    bucket::{BucketRecord, CatalogError, resource},
    csek::CsekError,
    input::InputError,
    multipart::MultipartError,
    object::{ObjectError, media_headers::StoredHeaderError, upload_headers::UploadHeaderError},
    profile::CompatProfile,
    storage::ResumableError,
    time_format::TimeOutOfRange,
};
use serde_json::{Map, Value};
use std::time::SystemTime;

pub(crate) mod access;
pub(crate) mod activity;
pub(crate) mod body;
pub(crate) mod chunk_body;
pub(crate) mod context;
pub(crate) mod copy;
pub(crate) mod cors_warnings;
pub(crate) mod media;
pub(crate) mod object_body;
pub(crate) mod object_headers;
pub(crate) mod read_handles;
pub(crate) mod state;
pub(crate) mod upload_drive;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ServiceError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Project(#[from] InvalidProjectId),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Multipart(#[from] MultipartError),
    #[error(transparent)]
    Object(Box<ObjectError>),
    #[error(transparent)]
    Resumable(Box<ResumableError>),
    #[error(transparent)]
    Csek(#[from] CsekError),
    #[error(transparent)]
    UploadHeader(#[from] UploadHeaderError),
    #[error(transparent)]
    SessionHeader(UploadHeaderError),
    #[error(transparent)]
    Body(#[from] BodyError),
    #[error(transparent)]
    StoredHeader(#[from] StoredHeaderError),
    #[error(transparent)]
    Time(#[from] TimeOutOfRange),
}

impl From<ObjectError> for ServiceError {
    fn from(error: ObjectError) -> Self {
        Self::Object(Box::new(error))
    }
}

impl From<ResumableError> for ServiceError {
    fn from(error: ResumableError) -> Self {
        Self::Resumable(Box::new(error))
    }
}

impl From<resource::Error> for ServiceError {
    fn from(error: resource::Error) -> Self {
        match error {
            resource::Error::Input(error) => Self::Input(error),
            resource::Error::Project(error) => Self::Project(error),
            resource::Error::Time(error) => Self::Time(error),
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
        assert!(
            size_of::<InputError>() <= 48,
            "input {} bytes",
            size_of::<InputError>()
        );
        assert!(
            size_of::<ServiceError>() <= 56,
            "service {} bytes with object {} and resumable {} bytes",
            size_of::<ServiceError>(),
            size_of::<ObjectError>(),
            size_of::<ResumableError>()
        );
    }
}
