use super::*;

impl From<crate::percent::MalformedEncoding> for ApiError {
    fn from(error: crate::percent::MalformedEncoding) -> Self {
        Self::invalid_parameter(error.to_string())
    }
}

impl From<crate::percent::QueryError> for ApiError {
    fn from(error: crate::percent::QueryError) -> Self {
        Self::invalid_parameter(error.to_string())
    }
}

/// A bucket named in a path or parameter: the native refusal is a malformed parameter, while the
/// fake-gcs-server profile reports an invalid value.
impl From<crate::bucket::name::ProfileNameError> for ApiError {
    fn from(error: crate::bucket::name::ProfileNameError) -> Self {
        use crate::bucket::name::ProfileNameError;
        match error {
            ProfileNameError::Native(_) => Self::invalid_parameter(error.to_string()),
            ProfileNameError::FakeGcsServer => Self::invalid(error.to_string()),
        }
    }
}

/// A refused bucket token is a malformed parameter and a refused object token an invalid value,
/// as Cloud Storage reported them.
impl From<crate::page_token::PageTokenError> for ApiError {
    fn from(error: crate::page_token::PageTokenError) -> Self {
        use crate::page_token::PageTokenError;
        match error {
            PageTokenError::BucketCursor => Self::invalid_parameter(error.to_string()),
            PageTokenError::ObjectCursor(_) => Self::invalid(error.to_string()),
            PageTokenError::Project(error) => error.into(),
            PageTokenError::BucketName(error) => error.into(),
        }
    }
}

impl From<crate::hashing::InvalidDigest> for ApiError {
    fn from(error: crate::hashing::InvalidDigest) -> Self {
        Self::invalid_parameter(error.to_string())
    }
}

/// A stored time that cannot be rendered is a backend failure.
impl From<crate::time_format::TimeOutOfRange> for ApiError {
    fn from(error: crate::time_format::TimeOutOfRange) -> Self {
        Self::service_unavailable(error.to_string())
    }
}

/// A project path segment outside the identifier rules is a malformed parameter.
impl From<crate::auth::InvalidProjectId> for ApiError {
    fn from(error: crate::auth::InvalidProjectId) -> Self {
        Self::invalid_parameter(error.to_string())
    }
}

/// The JSON API names the canned ACL parameter `predefinedAcl` on a metadata write.
impl From<crate::acl::ConflictingAcl> for ApiError {
    fn from(_: crate::acl::ConflictingAcl) -> Self {
        Self::unsupported_object("acl together with predefinedAcl").at_parameter("predefinedAcl")
    }
}

/// A refused `matchGlob` pattern is a malformed request; the pattern defect is the message.
impl From<crate::match_glob::GlobError> for ApiError {
    fn from(error: crate::match_glob::GlobError) -> Self {
        Self::invalid(error.to_string())
    }
}

/// A URL that cannot be built becomes the error body text; `UrlError` names the defect and
/// has no underlying cause to keep.
impl From<crate::url::UrlError> for ApiError {
    fn from(error: crate::url::UrlError) -> Self {
        Self::invalid_parameter(error.to_string())
    }
}

impl From<crate::auth::AuthError> for ApiError {
    fn from(error: crate::auth::AuthError) -> Self {
        match error {
            crate::auth::AuthError::Failure(failure) => failure.into(),
            crate::auth::AuthError::Unavailable(cause) => Self::backend(&cause),
        }
    }
}

impl From<AuthFailure> for ApiError {
    fn from(failure: AuthFailure) -> Self {
        match failure {
            // Documented status-code example for a missing credential.
            AuthFailure::LoginRequired => Self::new(
                StatusCode::UNAUTHORIZED,
                "required",
                wording::LOGIN_REQUIRED,
            )
            .with_kind(ErrorKind::Required)
            .at(&AUTHORIZATION_HEADER),
            // The reason is documented; message and location are a local choice.
            AuthFailure::InvalidCredentials => Self::new(
                StatusCode::UNAUTHORIZED,
                "authError",
                wording::INVALID_CREDENTIALS,
            )
            .at(&AUTHORIZATION_HEADER),
            AuthFailure::Denied(denied) => denied.into(),
            AuthFailure::Forbidden => Self::forbidden(),
        }
    }
}

/// The documented 403 `forbidden` reason with Cloud Storage's longer message naming the
/// principal and the refused permission.
impl From<Box<Denied>> for ApiError {
    fn from(denied: Box<Denied>) -> Self {
        let mut error = Self::new(StatusCode::FORBIDDEN, "forbidden", denied.to_string());
        error.denied = Some(denied);
        error
    }
}

impl From<crate::csek::CsekError> for ApiError {
    fn from(error: crate::csek::CsekError) -> Self {
        Self::new(StatusCode::BAD_REQUEST, error.reason(), error.message())
            .with_kind(ErrorKind::Csek(error))
    }
}

impl From<CatalogError> for ApiError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::AlreadyExists => Self::new(
                StatusCode::CONFLICT,
                "conflict",
                wording::BUCKET_ALREADY_OWNED,
            ),
            CatalogError::NotFound => Self::missing_bucket(),
            CatalogError::RestoreConflict => {
                Self::new(StatusCode::CONFLICT, "conflict", wording::RESTORE_CONFLICT)
            }
            CatalogError::NotEmpty => {
                Self::new(StatusCode::CONFLICT, "conflict", "Bucket is not empty")
            }
            CatalogError::PreconditionFailed | CatalogError::NotModified => {
                Self::precondition_not_held()
            }
            CatalogError::Denied(denied) => denied.into(),
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
                wording::PUBLIC_ACCESS_PREVENTED,
            ),
            CatalogError::InvalidMetadata(reason) => Self::invalid(reason),
            CatalogError::Metadata(error) => Self::metadata(&error, Self::unsupported),
            CatalogError::UnsupportedMetadata(field) => Self::unsupported(&field),
            CatalogError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull => {
                Self::service_unavailable(wording::storage_capacity(&cause))
            }
            CatalogError::Unavailable(cause) | CatalogError::Corrupt(cause) => {
                Self::catalog_backend(&cause)
            }
            error @ (CatalogError::AuthenticationAlreadyConfigured
            | CatalogError::Io(_)
            | CatalogError::NewerSchema { .. }) => {
                Self::catalog_backend(&BackendCause::from_error(error))
            }
        }
    }
}

impl From<crate::storage::ResumableError> for ApiError {
    fn from(error: crate::storage::ResumableError) -> Self {
        use crate::storage::ResumableError;
        match error {
            ResumableError::NotFound => Self::new(
                StatusCode::NOT_FOUND,
                "notFound",
                "Upload session not found",
            )
            .with_plain(&NOT_FOUND_TEXT),
            // Unrecorded terminal validation failures keep the existing local 410 response.
            ResumableError::Gone => Self::new(
                StatusCode::GONE,
                "gone",
                "Upload session is no longer active",
            )
            .with_plain(&SESSION_MESSAGE_TEXT),
            ResumableError::Invalid(message) => Self::invalid_parameter(message),
            ResumableError::Cancelled => Self::cancelled_upload(),
            ResumableError::InvalidContentRange => {
                Self::invalid_parameter("Failed to parse Content-Range header.")
                    .with_plain(&CONTENT_RANGE_TEXT)
            }
            // The recorded refusal is a bare text page without cache fields.
            ResumableError::Overrun(overrun) => Self::session_protocol(overrun.to_string()),
            ResumableError::Object(error) => error.into(),
        }
    }
}

impl ApiError {
    #[expect(
        clippy::expect_used,
        reason = "499 lies within the 100..=999 range StatusCode accepts, and from_u16 is not const"
    )]
    pub(super) fn cancelled_upload() -> Self {
        Self::new(
            StatusCode::from_u16(499).expect("499 is a valid status code"),
            "clientClosedRequest",
            "clientClosedRequest",
        )
        .with_kind(ErrorKind::CancelledUpload)
    }
}

/// A compose component's failure, in the wording Cloud Storage used for it.
impl From<Box<crate::object::ComponentFailure>> for ApiError {
    fn from(failure: Box<crate::object::ComponentFailure>) -> Self {
        use crate::object::ComponentFailureKind;
        match failure.kind {
            ComponentFailureKind::NotFound => {
                Self::named_missing_object(&failure.bucket, &failure.name)
            }
            ComponentFailureKind::GenerationMismatch { required, actual } => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "conditionNotMet",
                wording::component_generation_mismatch(
                    &failure.bucket,
                    &failure.name,
                    required,
                    actual,
                ),
            ),
        }
    }
}

impl From<Box<crate::object::HeldObject>> for ApiError {
    fn from(held: Box<crate::object::HeldObject>) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            wording::held_object(&held),
        )
        .with_kind(ErrorKind::ActiveHold)
    }
}

/// Bucket retention policies and object retention share the reason and wording (observed
/// 2026-10-31, including the refused overwrite).
impl From<Box<crate::object::RetainedObject>> for ApiError {
    fn from(retained: Box<crate::object::RetainedObject>) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "retentionPolicyNotMet",
            wording::retained_object(&retained),
        )
        .with_kind(ErrorKind::RetentionNotMet)
    }
}

/// An unavailable or corrupt backend, keeping the fixed failure it reports for classification.
impl From<BackendNotice> for ApiError {
    fn from(notice: BackendNotice) -> Self {
        let error = Self::service_unavailable(notice.message);
        match notice.failure {
            Some(failure) => error.with_kind(ErrorKind::Backend(failure)),
            None => error,
        }
    }
}

impl From<crate::object::FixedMetadataError> for ApiError {
    fn from(error: crate::object::FixedMetadataError) -> Self {
        ObjectError::from(error).into()
    }
}

impl From<ObjectError> for ApiError {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::BucketNotFound => Self::missing_bucket(),
            ObjectError::Component(failure) => failure.into(),
            ObjectError::Metadata(error) => Self::metadata(&error, Self::unsupported_object),
            ObjectError::InvalidDerivedArgument(reason) => Self::invalid_argument(reason),
            ObjectError::ComposeClassMismatch { .. } => Self::invalid(error.to_string()),
            ObjectError::DerivedTransferIncomplete => {
                Self::invalid_argument(wording::DERIVED_TRANSFER_INCOMPLETE)
            }
            ObjectError::DerivedTransferNotFound => Self::new(
                StatusCode::NOT_FOUND,
                "notFound",
                wording::DERIVED_TRANSFER_NOT_FOUND,
            ),
            ObjectError::DerivedTransferExpired => {
                Self::new(StatusCode::GONE, "gone", wording::DERIVED_TRANSFER_EXPIRED)
            }
            ObjectError::DerivedTransferBusy => {
                Self::service_unavailable(wording::DERIVED_TRANSFER_BUSY)
            }
            ObjectError::UnsupportedMetadata(field) => Self::unsupported_object(&field),
            ObjectError::CustomerEncryption(error) => error.into(),
            ObjectError::NotModified => Self::not_modified(),
            ObjectError::WriteNotModified => Self::write_not_modified(),
            ObjectError::ObjectNotFound => Self::missing_object(),
            ObjectError::ObjectNotSoftDeleted => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "objectNotSoftDeleted",
                error.to_string(),
            ),
            ObjectError::SoftDeletePolicyRequired(request) => {
                Self::invalid(request.policy_required_message())
            }
            // A copy source's not-match is a failed source condition, not a conditional read.
            ObjectError::PreconditionFailed | ObjectError::SourceNotModified => {
                Self::precondition_not_held().at_if_match()
            }
            ObjectError::OverwriteForbidden(denied) | ObjectError::Denied(denied) => denied.into(),
            ObjectError::Forbidden => Self::forbidden(),
            ObjectError::UnlockedRetentionOverrideRequired => Self::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                crate::object::UNLOCKED_RETENTION_OVERRIDE_MESSAGE,
            ),
            ObjectError::LockedRetention => Self::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                crate::object::LOCKED_RETENTION_MESSAGE,
            ),
            ObjectError::ActiveHold(held) => held.into(),
            ObjectError::ObjectRetentionNotMet(retained) => retained.into(),
            ObjectError::HashMismatch(mismatch) => Self::invalid(wording::hash_mismatch(&mismatch)),
            ObjectError::ChecksumMismatch => Self::new(
                StatusCode::BAD_REQUEST,
                "invalid",
                wording::CHECKSUM_MISMATCH,
            ),
            ObjectError::TooLarge => Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "uploadTooLarge",
                wording::UPLOAD_TOO_LARGE,
            ),
            ObjectError::BodyAborted(_) => {
                Self::new(StatusCode::BAD_REQUEST, "badRequest", wording::BODY_ABORTED)
                    .with_kind(ErrorKind::IncompleteBody)
            }
            ObjectError::InvalidFixedMetadata(reason) => Self::invalid(reason),
            ObjectError::UniformAccessPredefinedAcl => {
                Self::invalid(crate::acl::UNIFORM_ACCESS_UPLOAD_ACL)
                    .with_kind(ErrorKind::UniformAccessAcl)
            }
            ObjectError::UniformAccessAclUpdate => {
                Self::invalid(crate::acl::UNIFORM_ACCESS_UPDATE_OBJECT_ACL)
                    .with_kind(ErrorKind::UniformAccessAcl)
            }
            ObjectError::PublicAccessPrevention => CatalogError::PublicAccessPrevention.into(),
            ObjectError::Unavailable(cause) => Self::backend(&cause),
            ObjectError::Io(cause) => Self::service_unavailable(wording::object_io(&cause)),
            ObjectError::Corrupt(cause) => wording::corrupt_object(&cause).into(),
        }
    }
}
