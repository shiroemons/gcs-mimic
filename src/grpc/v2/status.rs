use crate::{
    bucket::CatalogError,
    grpc::generated::google::rpc::{BadRequest, ErrorInfo, Status as RpcStatus, bad_request},
    metadata_error::MetadataError,
    object::{ComponentFailure, ComponentFailureKind, ObjectError},
    storage::failure::{BackendCause, wording},
};
use prost::Message;
use tonic::{Code, Status};

/// The gRPC message of a request whose `if_*_not_match` named the current value: a read
/// (recorded on `GetObject`) and, unlike the JSON API's 304, a write (recorded 2026-11-18 on
/// DeleteObject, UpdateObject, WriteObject and RewriteObject).
const NOT_MODIFIED: &str =
    "The resource you requested has not been modified according to the condition you specified.";

/// The failed precondition of a request whose not-match condition named the current value.
pub(super) fn not_modified() -> Status {
    Status::failed_precondition(NOT_MODIFIED)
}

/// A request for soft-deleted generations in a bucket without a soft delete policy. Cloud
/// Storage repeats the GetObject and RestoreObject refusals in a `BadRequest` (recorded
/// 2026-11-18); no ListObjects refusal was recorded over gRPC, so it keeps the bare message.
fn soft_delete_policy_required(request: crate::object::SoftDeletedRequest) -> Status {
    use crate::object::SoftDeletedRequest;
    let message = request.policy_required_message();
    match request {
        SoftDeletedRequest::List => Status::invalid_argument(message),
        SoftDeletedRequest::Get | SoftDeletedRequest::Restore => invalid_field(message),
    }
}

/// Cloud Storage's refusal to delete a bucket that still holds objects (recorded 2026-11-18).
const BUCKET_NOT_EMPTY: &str = "The bucket you tried to delete is not empty.";

/// Reader failures keep the storage classification and record the underlying I/O cause.
pub(super) fn read_failure(error: std::io::Error) -> Status {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Error,
        "grpc_object_read_failure",
        serde_json::json!({"cause": error.to_string()}),
    );
    super::object_io::object_status(crate::storage::failure::object_unavailable(error))
}

pub(super) fn list_query_status(error: crate::storage::ListQueryError) -> Status {
    use crate::storage::ListQueryError;
    Status::invalid_argument(match error {
        ListQueryError::SoftDeletedWithVersions => {
            "soft_deleted=true cannot be combined with versions=true; omit versions"
        }
        ListQueryError::FoldersAsPrefixesNeedSlash => {
            "include_folders_as_prefixes requires delimiter=/; use include_folders_as_prefixes=true and delimiter=/"
        }
        ListQueryError::GlobNeedsSlashDelimiter => {
            "match_glob requires delimiter=/ or no delimiter; use match_glob=**/*.txt and delimiter=/"
        }
    })
}

pub(super) fn glob_status(error: &crate::match_glob::GlobError) -> Status {
    Status::invalid_argument(error.message("match_glob"))
}

/// Every object failure picks its gRPC code here; the text it shares with the JSON API comes
/// from [`wording`].
impl From<ObjectError> for Status {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::BucketNotFound => Status::not_found(wording::BUCKET_NOT_FOUND),
            ObjectError::ObjectNotFound => Status::not_found(wording::OBJECT_NOT_FOUND),
            ObjectError::DerivedTransferNotFound => {
                Status::not_found(wording::DERIVED_TRANSFER_NOT_FOUND)
            }
            ObjectError::Component(failure) => component(&failure),
            ObjectError::PreconditionFailed => {
                Status::failed_precondition(wording::PRECONDITION_NOT_HELD)
            }
            // The JSON API answers a write's matching not-match condition with 304 and a copy
            // source's with 412; gRPC has one status and one wording for them and for a read's.
            ObjectError::NotModified
            | ObjectError::WriteNotModified
            | ObjectError::SourceNotModified => not_modified(),
            ObjectError::PublicAccessPrevention => {
                Status::failed_precondition(wording::PUBLIC_ACCESS_PREVENTED)
            }
            error @ ObjectError::ObjectNotSoftDeleted => {
                Status::failed_precondition(error.to_string())
            }
            ObjectError::DerivedTransferExpired => {
                Status::failed_precondition(wording::DERIVED_TRANSFER_EXPIRED)
            }
            ObjectError::OverwriteForbidden(denied) | ObjectError::Denied(denied) => {
                Status::permission_denied(denied.to_string())
            }
            ObjectError::Forbidden => Status::permission_denied(wording::FORBIDDEN),
            ObjectError::ActiveHold(held) => Status::permission_denied(wording::held_object(&held)),
            ObjectError::ObjectRetentionNotMet(retained) => {
                Status::permission_denied(wording::retained_object(&retained))
            }
            ObjectError::UnlockedRetentionOverrideRequired => {
                Status::permission_denied(crate::object::UNLOCKED_RETENTION_OVERRIDE_MESSAGE)
            }
            ObjectError::LockedRetention => {
                Status::permission_denied(crate::object::LOCKED_RETENTION_MESSAGE)
            }
            ObjectError::SoftDeletePolicyRequired(request) => soft_delete_policy_required(request),
            ObjectError::ChecksumMismatch => Status::invalid_argument(wording::CHECKSUM_MISMATCH),
            ObjectError::HashMismatch(mismatch) => {
                Status::invalid_argument(wording::hash_mismatch(&mismatch))
            }
            ObjectError::BodyAborted(_) => Status::invalid_argument(wording::BODY_ABORTED),
            ObjectError::InvalidFixedMetadata(reason)
            | ObjectError::InvalidDerivedArgument(reason) => Status::invalid_argument(reason),
            ObjectError::UniformAccessPredefinedAcl => {
                Status::invalid_argument(crate::acl::UNIFORM_ACCESS_UPLOAD_ACL)
            }
            ObjectError::UniformAccessAclUpdate => {
                Status::invalid_argument(crate::acl::UNIFORM_ACCESS_UPDATE_ACL)
            }
            // Cloud Storage repeats this refusal in a BadRequest (recorded 2026-11-18).
            error @ ObjectError::ComposeClassMismatch { .. } => invalid_field(error.to_string()),
            ObjectError::DerivedTransferIncomplete => {
                Status::invalid_argument(wording::DERIVED_TRANSFER_INCOMPLETE)
            }
            ObjectError::CustomerEncryption(error) => customer_encryption(error),
            ObjectError::Metadata(error) => metadata(&error, wording::unsupported_object_setting),
            ObjectError::UnsupportedMetadata(field) => {
                unsupported_message(wording::unsupported_object_setting(&field))
            }
            ObjectError::TooLarge => Status::resource_exhausted(wording::UPLOAD_TOO_LARGE),
            ObjectError::DerivedTransferBusy => Status::unavailable(wording::DERIVED_TRANSFER_BUSY),
            ObjectError::Io(cause) => Status::unavailable(wording::object_io(&cause)),
            ObjectError::Corrupt(cause) => {
                Status::unavailable(wording::corrupt_object(&cause).message)
            }
            ObjectError::Unavailable(cause) => {
                Status::unavailable(wording::object_backend(&cause).message)
            }
        }
    }
}

/// Every bucket catalog failure picks its gRPC code here; the text it shares with the JSON API
/// comes from [`wording`].
impl From<CatalogError> for Status {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::NotFound => Status::not_found(wording::BUCKET_NOT_FOUND),
            CatalogError::AlreadyExists => Status::already_exists(wording::BUCKET_ALREADY_OWNED),
            CatalogError::RestoreConflict => Status::already_exists(wording::RESTORE_CONFLICT),
            CatalogError::NotEmpty => Status::failed_precondition(BUCKET_NOT_EMPTY),
            CatalogError::PreconditionFailed => {
                Status::failed_precondition(wording::PRECONDITION_NOT_HELD)
            }
            // A not-match that alone fails is worded as a resource that has not been modified
            // (recorded on DeleteBucket, case grpc-probe-bucket-conditions, and on GetBucket and
            // UpdateBucket, case grpc-probe-inferred-bucket-masks). The legacy v1 service keeps
            // the failed-condition wording and maps this variant itself.
            CatalogError::NotModified => not_modified(),
            CatalogError::PublicAccessPrevention => {
                Status::failed_precondition(wording::PUBLIC_ACCESS_PREVENTED)
            }
            CatalogError::Denied(denied) => Status::permission_denied(denied.to_string()),
            CatalogError::Forbidden => Status::permission_denied(wording::FORBIDDEN),
            error @ (CatalogError::RetentionPolicyLocked(_)
            | CatalogError::RetentionPolicyNotSet) => Status::invalid_argument(error.to_string()),
            CatalogError::InvalidMetadata(reason) => Status::invalid_argument(reason),
            CatalogError::Metadata(error) => metadata(&error, wording::unsupported_bucket_setting),
            CatalogError::UnsupportedMetadata(field) => {
                unsupported_message(wording::unsupported_bucket_setting(&field))
            }
            CatalogError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull => {
                Status::unavailable(wording::storage_capacity(&cause))
            }
            CatalogError::Unavailable(cause) | CatalogError::Corrupt(cause) => {
                Status::unavailable(wording::catalog_backend(&cause).message)
            }
            // These hold no backend cause; wrapped in one, a fixed failure in their chain still
            // names its remedy and any other cause is logged, as the JSON API does.
            error @ (CatalogError::AuthenticationAlreadyConfigured
            | CatalogError::Io(_)
            | CatalogError::NewerSchema { .. }) => Status::unavailable(
                wording::catalog_backend(&BackendCause::from_error(error)).message,
            ),
        }
    }
}

/// A compose component's failure names the component, as Cloud Storage's wording does.
fn component(failure: &ComponentFailure) -> Status {
    match failure.kind {
        ComponentFailureKind::NotFound => Status::not_found(wording::missing_named_object(
            &failure.bucket,
            &failure.name,
        )),
        ComponentFailureKind::GenerationMismatch { required, actual } => {
            Status::failed_precondition(wording::component_generation_mismatch(
                &failure.bucket,
                &failure.name,
                required,
                actual,
            ))
        }
    }
}

/// A metadata refusal. A class the bucket's location cannot hold repeats its message in a
/// `BadRequest`, as Cloud Storage does (recorded 2026-11-17); a recognized setting gcs-mimic
/// lacks is unsupported in the wording of the API that refused it.
fn metadata(error: &MetadataError, unsupported: fn(&str) -> String) -> Status {
    let message = error.to_string();
    if matches!(error, MetadataError::ClassLocation(_)) {
        invalid_field(message)
    } else if error.unsupported() {
        unsupported_message(unsupported(&message))
    } else {
        Status::invalid_argument(message)
    }
}

/// A permission no supported operation checks is unsupported, worded as the JSON API words an
/// unsupported bucket setting; any other refusal is an invalid argument.
impl From<crate::auth::testable::UntestablePermission> for Status {
    fn from(error: crate::auth::testable::UntestablePermission) -> Self {
        use crate::auth::testable::UntestablePermission;
        match error {
            UntestablePermission::Unsupported(_) => {
                unsupported_message(wording::unsupported_bucket_setting(&error.to_string()))
            }
            UntestablePermission::Unknown(_) | UntestablePermission::ProjectScoped => {
                Status::invalid_argument(error.to_string())
            }
        }
    }
}

impl From<crate::page_token::PageTokenError> for Status {
    fn from(error: crate::page_token::PageTokenError) -> Self {
        Status::invalid_argument(error.to_string())
    }
}

impl From<crate::auth::InvalidProjectId> for Status {
    fn from(error: crate::auth::InvalidProjectId) -> Self {
        Status::invalid_argument(error.to_string())
    }
}

impl From<crate::bucket::name::ProfileNameError> for Status {
    fn from(error: crate::bucket::name::ProfileNameError) -> Self {
        Status::invalid_argument(error.to_string())
    }
}

/// A routing value or resource name whose percent-encoding cannot be decoded.
impl From<crate::percent::MalformedEncoding> for Status {
    fn from(error: crate::percent::MalformedEncoding) -> Self {
        Status::invalid_argument(error.to_string())
    }
}

/// A missing or unverifiable credential is unauthenticated and a refusal is permission denied,
/// with the messages the JSON API sends.
impl From<crate::auth::AuthFailure> for Status {
    fn from(error: crate::auth::AuthFailure) -> Self {
        use crate::auth::AuthFailure;
        match error {
            AuthFailure::LoginRequired => Status::unauthenticated(wording::LOGIN_REQUIRED),
            AuthFailure::InvalidCredentials => {
                Status::unauthenticated(wording::INVALID_CREDENTIALS)
            }
            AuthFailure::Denied(denied) => Status::permission_denied(denied.to_string()),
            AuthFailure::Forbidden => Status::permission_denied(wording::FORBIDDEN),
        }
    }
}

impl From<crate::auth::policy_input::PolicyInputError> for Status {
    fn from(error: crate::auth::policy_input::PolicyInputError) -> Self {
        match error {
            crate::auth::policy_input::PolicyInputError::InvalidInput(message) => {
                Status::invalid_argument(message)
            }
            crate::auth::policy_input::PolicyInputError::UnsupportedInput(message) => {
                unsupported_with_remedy(
                    format!(
                        "gcs-mimic does not support IAM policy {message}; omit it, for example use roles/storage.objectViewer with user: members"
                    ),
                    "Omit the unsupported IAM policy input; use roles/storage.objectViewer with user: members",
                )
            }
        }
    }
}

/// Cloud Storage answers every customer-supplied key failure over gRPC with `INVALID_ARGUMENT`
/// and one `BadRequest` field violation that repeats the message (recorded 2026-11-15).
pub(crate) fn customer_encryption(error: crate::csek::CsekError) -> Status {
    invalid_field(customer_encryption_message(error))
}

/// The gRPC wording, which differs from the JSON API's for four of the six failures
/// (recorded 2026-11-15).
fn customer_encryption_message(error: crate::csek::CsekError) -> &'static str {
    use crate::csek::CsekError;
    match error {
        CsekError::AlgorithmInvalid => {
            "Missing an encryption algorithm, or the provided algorithm is not \"AES256\"."
        }
        CsekError::KeyFormatInvalid => {
            "Missing an encryption key, or it is not base64 encoded, or it does not meet the required length of the encryption algorithm."
        }
        CsekError::KeySha256Invalid => {
            "Missing a SHA256 hash of the encryption key, or it is not base64 encoded, or it does not match the encryption key."
        }
        CsekError::KeyIncorrect => "The provided encryption key is incorrect.",
        CsekError::ResourceIsEncrypted => {
            "The target object is encrypted by a customer-supplied encryption key."
        }
        CsekError::ResourceNotEncrypted => {
            "The target object is not encrypted by a customer-supplied encryption key."
        }
    }
}

impl From<crate::csek::CsekError> for Status {
    fn from(error: crate::csek::CsekError) -> Self {
        customer_encryption(error)
    }
}

impl From<crate::api_error::ApiError> for Status {
    fn from(mut error: crate::api_error::ApiError) -> Self {
        // A storage error that a handler turned into the shared error, such as one that names its
        // object, must still report what the direct conversion above reports.
        if let crate::api_error::ErrorKind::Csek(error) = error.kind {
            return customer_encryption(error);
        }
        // The JSON API answers a write's matching not-match condition with 304; gRPC words it
        // as a read's.
        if error.kind == crate::api_error::ErrorKind::WriteNotModified {
            return not_modified();
        }
        if error.kind == crate::api_error::ErrorKind::NotModified {
            error.message = NOT_MODIFIED.into();
        }
        // Cloud Storage repeats this refusal in a BadRequest (recorded 2026-11-17).
        if error.kind == crate::api_error::ErrorKind::ClassLocation {
            return invalid_field(error.message);
        }
        if error.kind == crate::api_error::ErrorKind::Unsupported {
            return unsupported_message(error.message);
        }
        match error.status.as_u16() {
            400 => Status::invalid_argument(error.message),
            401 => Status::unauthenticated(error.message),
            403 => Status::permission_denied(error.message),
            404 => Status::not_found(error.message),
            409 => Status::already_exists(error.message),
            304 | 410 | 412 => Status::failed_precondition(error.message),
            413 | 429 => Status::resource_exhausted(error.message),
            416 => Status::out_of_range(error.message),
            499 => Status::cancelled(error.message),
            501 => unsupported_message(error.message),
            503 => Status::unavailable(error.message),
            504 => Status::deadline_exceeded(error.message),
            _ => Status::internal(error.message),
        }
    }
}

/// Object-level permissions are decided only after the catalog selects a generation and its
/// ACL, past the handlers' bucket-stage anonymous mapping; apply JSON's rule to the whole RPC.
pub(crate) fn anonymous(status: Status, caller: Option<crate::auth::Caller<'_>>) -> Status {
    if matches!(caller, Some(crate::auth::Caller::Anonymous))
        && status.code() == Code::PermissionDenied
    {
        crate::auth::AuthFailure::LoginRequired.into()
    } else {
        status
    }
}

pub(super) fn unsupported_for(method: &str, caller: Option<crate::auth::Caller<'_>>) -> Status {
    if matches!(caller, Some(crate::auth::Caller::Anonymous)) {
        return super::auth_error(crate::auth::AuthFailure::InvalidCredentials.into());
    }
    unsupported(method)
}

pub(crate) fn unsupported(method: &str) -> Status {
    let message = format!(
        "gcs-mimic does not support gRPC {method}; use the JSON API instead. Set STORAGE_EMULATOR_HOST to the server's api_base_url reported in its ready event."
    );
    unsupported_message(message)
}

/// JSON unsupported messages already include a remedy and must not be wrapped again.
pub(super) fn unsupported_message(message: String) -> Status {
    unsupported_with_remedy(
        message,
        "Use the JSON API; set STORAGE_EMULATOR_HOST to api_base_url from the server ready event",
    )
}

pub(super) fn unsupported_with_remedy(message: String, remedy: &str) -> Status {
    let info = ErrorInfo {
        // Preserve the existing unsupported JSON contract's reason and domain.
        reason: "badRequest".into(),
        domain: "global".into(),
        metadata: [
            ("service".into(), "gcs-mimic".into()),
            ("remedy".into(), remedy.into()),
        ]
        .into(),
    };
    with_detail(
        Code::Unimplemented,
        message,
        ERROR_INFO_TYPE,
        info.encode_to_vec(),
    )
}

const ERROR_INFO_TYPE: &str = "type.googleapis.com/google.rpc.ErrorInfo";

/// The domain Cloud Storage puts in the `ErrorInfo` of a rejected request. It names the service,
/// not the endpoint the client dialed, so an emulator on another host keeps the same value.
const ERROR_DOMAIN: &str = "storage.googleapis.com";

/// A status that carries one `google.rpc.ErrorInfo` naming `reason` in Cloud Storage's domain.
pub(super) fn with_reason(code: Code, message: impl Into<String>, reason: &str) -> Status {
    let info = ErrorInfo {
        reason: reason.into(),
        domain: ERROR_DOMAIN.into(),
        metadata: Default::default(),
    };
    with_detail(code, message.into(), ERROR_INFO_TYPE, info.encode_to_vec())
}

/// An `INVALID_ARGUMENT` status that carries one `google.rpc.ErrorInfo`, as Cloud Storage does
/// for malformed resource names.
pub(crate) fn invalid_with_reason(message: impl Into<String>, reason: &str) -> Status {
    with_reason(Code::InvalidArgument, message, reason)
}

/// The answer Cloud Storage gives where its own implementation fails, such as an upload ID it
/// cannot read (recorded 2026-11-18). The recorded message ends in an opaque
/// `Internal details:` value that differs for every request and is not reproduced.
pub(super) fn internal_error() -> Status {
    Status::internal("We encountered an internal error. Please try again.")
}

/// An `INVALID_ARGUMENT` status that carries a `google.rpc.BadRequest` with one field violation
/// whose description repeats the message, as Cloud Storage does for an empty object name.
pub(crate) fn invalid_field(message: impl Into<String>) -> Status {
    let message = message.into();
    let violation = BadRequest {
        field_violations: vec![bad_request::FieldViolation {
            description: message.clone(),
            ..Default::default()
        }],
    };
    with_detail(
        Code::InvalidArgument,
        message,
        "type.googleapis.com/google.rpc.BadRequest",
        violation.encode_to_vec(),
    )
}

pub(super) fn with_detail(code: Code, message: String, type_url: &str, value: Vec<u8>) -> Status {
    let details = RpcStatus {
        code: code as i32,
        message: message.clone(),
        details: vec![prost_types::Any {
            type_url: type_url.into(),
            value,
        }],
    };
    Status::with_details(code, message, details.encode_to_vec().into())
}

#[cfg(test)]
mod domain_tests;
#[cfg(test)]
mod golden_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::StatusCode;

    fn rpc_status(status: &Status) -> RpcStatus {
        RpcStatus::decode(status.details()).unwrap()
    }

    #[test]
    fn unsupported_iam_inputs_offer_supported_policy_members() {
        use crate::auth::policy_input::{parse_member, parse_role};
        for error in [
            parse_role("roles/owner").unwrap_err(),
            parse_member("group:team@example.invalid").unwrap_err(),
        ] {
            let status = Status::from(error);
            assert_eq!(status.code(), Code::Unimplemented);
            assert!(status.message().contains("omit it"));
            assert!(
                status
                    .message()
                    .contains("roles/storage.objectViewer with user: members")
            );
            assert!(!status.message().contains("JSON API"));
            let rpc = rpc_status(&status);
            assert_eq!(rpc.message, status.message());
            let info = ErrorInfo::decode(rpc.details[0].value.as_slice()).unwrap();
            assert!(!info.metadata["remedy"].contains("JSON API"));
            assert!(info.metadata["remedy"].contains("Omit"));
        }
    }

    #[test]
    fn unsupported_errors_keep_their_details() {
        let status = unsupported("example");
        assert_eq!(status.code(), Code::Unimplemented);
        assert_eq!(
            status.message(),
            "gcs-mimic does not support gRPC example; use the JSON API instead. Set STORAGE_EMULATOR_HOST to the server's api_base_url reported in its ready event."
        );
        let details = rpc_status(&status);
        assert_eq!(details.code, Code::Unimplemented as i32);
        assert_eq!(details.message, status.message());
        assert_eq!(details.details.len(), 1);
        let detail = &details.details[0];
        assert_eq!(detail.type_url, "type.googleapis.com/google.rpc.ErrorInfo");
        let info = ErrorInfo::decode(detail.value.as_slice()).unwrap();
        assert_eq!(info.reason, "badRequest");
        assert_eq!(info.domain, "global");
        assert_eq!(info.metadata["service"], "gcs-mimic");
        assert_eq!(
            info.metadata["remedy"],
            "Use the JSON API; set STORAGE_EMULATOR_HOST to api_base_url from the server ready event"
        );
    }

    #[test]
    fn unsupported_json_errors_keep_their_messages_and_details() {
        for (label, error, message) in [
            (
                "bucket",
                crate::api_error::ApiError::unsupported("storageClass requires a region or zone"),
                "gcs-mimic does not support bucket API setting storageClass requires a region or zone; omit this setting (for example, create a bucket with only name).",
            ),
            (
                "object",
                crate::api_error::ApiError::unsupported_object("metadata"),
                "gcs-mimic does not support object API setting metadata; omit this option or header (for example, use projection=noAcl).",
            ),
            (
                "custom",
                crate::api_error::ApiError::unsupported_message(
                    "gcs-mimic does not support this setting; omit it.",
                ),
                "gcs-mimic does not support this setting; omit it.",
            ),
            (
                "plain 501",
                crate::api_error::ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "notImplemented",
                    "Unsupported object option",
                ),
                "Unsupported object option",
            ),
        ] {
            let actual = Status::from(error);
            let expected = unsupported("example");
            assert_eq!(actual.code(), expected.code(), "{label}");
            assert_eq!(actual.message(), message, "{label}");
            let actual = rpc_status(&actual);
            assert_eq!(actual.message, message, "{label}");
            assert_eq!(actual.code, Code::Unimplemented as i32, "{label}");
            assert_eq!(actual.details.len(), 1, "{label}");
            let expected = rpc_status(&expected);
            assert_eq!(
                actual.details[0].type_url, expected.details[0].type_url,
                "{label}"
            );
            assert_eq!(
                ErrorInfo::decode(actual.details[0].value.as_slice()).unwrap(),
                ErrorInfo::decode(expected.details[0].value.as_slice()).unwrap(),
                "{label}"
            );
        }
    }

    #[test]
    fn precondition_failures_use_the_grpc_wording() {
        let not_empty = Status::from(crate::bucket::CatalogError::NotEmpty);
        assert_eq!(not_empty.code(), Code::FailedPrecondition);
        let failed = Status::from(crate::object::ObjectError::PreconditionFailed);
        assert_eq!(failed.code(), Code::FailedPrecondition);
        assert_eq!(failed.message(), wording::PRECONDITION_NOT_HELD);
        let unmodified = Status::from(crate::object::ObjectError::NotModified);
        assert_eq!(unmodified.message(), NOT_MODIFIED);
        // A write's matching not-match condition, 304 over JSON, is worded as a read's (recorded
        // 2026-11-18 on DeleteObject, UpdateObject, WriteObject and RewriteObject).
        let unchanged = Status::from(crate::object::ObjectError::WriteNotModified);
        assert_eq!(unchanged.code(), Code::FailedPrecondition);
        assert_eq!(unchanged.message(), NOT_MODIFIED);
        // A 412 that is not a failed condition keeps its own message.
        let pap = Status::from(crate::bucket::CatalogError::PublicAccessPrevention);
        assert_eq!(pap.code(), Code::FailedPrecondition);
        assert_eq!(pap.message(), "Public access prevention is enforced");
        let not_empty = Status::from(crate::api_error::ApiError::new(
            StatusCode::PRECONDITION_FAILED,
            "conditionNotMet",
            "bucket must be empty prior to deletion",
        ));
        assert_eq!(
            not_empty.message(),
            "bucket must be empty prior to deletion"
        );
    }

    #[test]
    fn customer_key_failures_use_the_recorded_grpc_contract() {
        use crate::csek::CsekError;
        // Recorded against Cloud Storage over gRPC on 2026-11-15.
        for (error, message) in [
            (
                CsekError::AlgorithmInvalid,
                "Missing an encryption algorithm, or the provided algorithm is not \"AES256\".",
            ),
            (
                CsekError::KeyFormatInvalid,
                "Missing an encryption key, or it is not base64 encoded, or it does not meet the required length of the encryption algorithm.",
            ),
            (
                CsekError::KeySha256Invalid,
                "Missing a SHA256 hash of the encryption key, or it is not base64 encoded, or it does not match the encryption key.",
            ),
            (
                CsekError::KeyIncorrect,
                "The provided encryption key is incorrect.",
            ),
            (
                CsekError::ResourceIsEncrypted,
                "The target object is encrypted by a customer-supplied encryption key.",
            ),
            (
                CsekError::ResourceNotEncrypted,
                "The target object is not encrypted by a customer-supplied encryption key.",
            ),
        ] {
            // Directly, and through the storage error every catalog call returns.
            for status in [
                Status::from(error),
                Status::from(crate::object::ObjectError::CustomerEncryption(error)),
            ] {
                assert_eq!(status.code(), Code::InvalidArgument, "{error:?}");
                assert_eq!(status.message(), message, "{error:?}");
                let rpc = rpc_status(&status);
                assert_eq!(rpc.details.len(), 1, "{error:?}");
                assert_eq!(
                    rpc.details[0].type_url,
                    "type.googleapis.com/google.rpc.BadRequest"
                );
                let violations = BadRequest::decode(rpc.details[0].value.as_slice()).unwrap();
                assert_eq!(violations.field_violations.len(), 1);
                assert_eq!(violations.field_violations[0].description, message);
                assert_eq!(violations.field_violations[0].field, "");
            }
        }
    }

    #[test]
    fn resource_name_errors_carry_their_details() {
        let bucket = crate::grpc::v2::names::bucket_name("x").unwrap_err();
        assert_eq!(bucket.code(), Code::InvalidArgument);
        assert!(bucket.message().starts_with("Invalid bucket name 'x'."));
        let detail = &rpc_status(&bucket).details[0];
        let info = ErrorInfo::decode(detail.value.as_slice()).unwrap();
        assert_eq!(info.reason, "GRPC_INVALID_BUCKET");
        assert_eq!(info.domain, ERROR_DOMAIN);

        let object =
            crate::grpc::v2::names::object_name("projects/_/buckets/bucket", "").unwrap_err();
        assert_eq!(object.message(), "No object name");
        let detail = &rpc_status(&object).details[0];
        let violations = BadRequest::decode(detail.value.as_slice()).unwrap();
        assert_eq!(violations.field_violations[0].description, "No object name");
    }
}
