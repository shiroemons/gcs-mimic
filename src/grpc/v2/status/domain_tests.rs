//! Pins the Storage v2 status of every storage domain error: its code, its message, its details
//! and whether it logs a backend cause. Each case names one branch of the mapping.

use super::*;
use crate::{
    bucket::{CatalogError, ClassLocationConflict, StorageClass},
    metadata_error::MetadataError,
    object::ObjectError,
    storage::failure::{BackendCause, BackendFailure, IoCause, MemoryBudget, MemoryExhausted},
};
use std::collections::BTreeSet;

#[derive(Debug, PartialEq)]
enum Detail {
    None,
    BadRequest(BadRequest),
    ErrorInfo(ErrorInfo),
}

/// What a client sees of a status.
#[derive(Debug, PartialEq)]
struct Reply {
    code: Code,
    message: String,
    detail: Detail,
}

impl Reply {
    fn of(status: &Status) -> Self {
        let detail = if status.details().is_empty() {
            Detail::None
        } else {
            let rpc = RpcStatus::decode(status.details()).unwrap();
            assert_eq!(rpc.code, status.code() as i32);
            assert_eq!(rpc.message, status.message());
            let [detail] = rpc.details.as_slice() else {
                panic!("expected one detail: {rpc:?}");
            };
            let value = detail.value.as_slice();
            match detail.type_url.as_str() {
                "type.googleapis.com/google.rpc.BadRequest" => {
                    Detail::BadRequest(BadRequest::decode(value).unwrap())
                }
                "type.googleapis.com/google.rpc.ErrorInfo" => {
                    Detail::ErrorInfo(ErrorInfo::decode(value).unwrap())
                }
                other => panic!("unexpected detail type {other}"),
            }
        };
        Self {
            code: status.code(),
            message: status.message().to_owned(),
            detail,
        }
    }
}

fn plain(code: Code, message: &str) -> Reply {
    Reply {
        code,
        message: message.to_owned(),
        detail: Detail::None,
    }
}

/// `INVALID_ARGUMENT` with one field violation that names no field and repeats the message.
fn field_violation(message: &str) -> Reply {
    Reply {
        code: Code::InvalidArgument,
        message: message.to_owned(),
        detail: Detail::BadRequest(BadRequest {
            field_violations: vec![bad_request::FieldViolation {
                description: message.to_owned(),
                ..Default::default()
            }],
        }),
    }
}

/// `UNIMPLEMENTED` with the unsupported-setting `ErrorInfo` and its JSON API remedy.
fn unimplemented(message: &str) -> Reply {
    Reply {
        code: Code::Unimplemented,
        message: message.to_owned(),
        detail: Detail::ErrorInfo(ErrorInfo {
            reason: "badRequest".into(),
            domain: "global".into(),
            metadata: [
                ("service".into(), "gcs-mimic".into()),
                (
                    "remedy".into(),
                    "Use the JSON API; set STORAGE_EMULATOR_HOST to api_base_url from the server ready event".into(),
                ),
            ]
            .into(),
        }),
    }
}

struct Case<E> {
    error: E,
    reply: Reply,
    /// The cause an `object_backend_failure` event logs, if the conversion logs one.
    logged: Option<&'static str>,
}

fn case<E>(error: E, reply: Reply) -> Case<E> {
    Case {
        error,
        reply,
        logged: None,
    }
}

impl<E> Case<E> {
    fn logging(mut self, cause: &'static str) -> Self {
        self.logged = Some(cause);
        self
    }
}

const PRECONDITION: &str = "At least one of the pre-conditions you specified did not hold.";
const DENIED: &str = "viewer@example.invalid does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist).";
const UNCLASSIFIED: &str = "unrecognized backend failure (private details withheld)";
const DISK_FULL: &str = "Storage capacity exhausted; free space in the data directory, where data kept by soft delete uses space until it expires (set softDeletePolicy.retentionDurationSeconds to 0 on buckets that retain deletions), then retry with generation preconditions";
const BLOBS_FULL: &str = "Storage capacity exhausted; delete objects, though data kept by soft delete keeps its bytes until it expires (set softDeletePolicy.retentionDurationSeconds to 0 on buckets that retain deletions), or restart with a larger memory_max_bytes (a restart loses RAM data)";
const CATALOG_PAGES_FULL: &str = "Storage capacity exhausted; restart with a larger memory_catalog_max_bytes (a restart loses RAM data)";
const CATALOG_CLOSED: &str = "catalog is closed; wait for the server to restart before retrying";
const CORRUPT: &str =
    "Object storage is unavailable: stored data is corrupt; restore a valid data directory";
const CATALOG_UNAVAILABLE: &str = "Bucket catalog is unavailable";

fn denied() -> Box<crate::auth::Denied> {
    let identity = crate::auth::Identity::from_upload_snapshot(
        &serde_json::json!({"id": "viewer@example.invalid", "projects": {}}),
    )
    .unwrap();
    crate::auth::Denied::new(
        Some(&identity),
        crate::auth::BUCKETS_GET,
        crate::auth::denied::Resource::in_bucket(crate::auth::BUCKETS_GET, "bucket"),
    )
}

fn class_location() -> MetadataError {
    let multi = "US".parse::<crate::bucket::Location>().unwrap();
    MetadataError::ClassLocation(
        ClassLocationConflict::check(StorageClass::Regional, multi, "123456").unwrap_err(),
    )
}

const CLASS_LOCATION: &str =
    "Project 123456 may not create storageClass REGIONAL buckets with locationConstraint US.";

fn memory_full(budget: MemoryBudget) -> IoCause {
    MemoryExhausted::new(budget).into()
}

/// Names each variant. A new variant fails to compile here: give it a case in
/// [`object_cases`] and its name in [`OBJECT_VARIANTS`].
fn object_variant(error: &ObjectError) -> &'static str {
    match error {
        ObjectError::BucketNotFound => "BucketNotFound",
        ObjectError::ObjectNotFound => "ObjectNotFound",
        ObjectError::PreconditionFailed => "PreconditionFailed",
        ObjectError::Component(_) => "Component",
        ObjectError::PublicAccessPrevention => "PublicAccessPrevention",
        ObjectError::OverwriteForbidden(_) => "OverwriteForbidden",
        ObjectError::ActiveHold(_) => "ActiveHold",
        ObjectError::ObjectRetentionNotMet(_) => "ObjectRetentionNotMet",
        ObjectError::UnlockedRetentionOverrideRequired => "UnlockedRetentionOverrideRequired",
        ObjectError::LockedRetention => "LockedRetention",
        ObjectError::ObjectNotSoftDeleted => "ObjectNotSoftDeleted",
        ObjectError::SoftDeletePolicyRequired(_) => "SoftDeletePolicyRequired",
        ObjectError::Denied(_) => "Denied",
        ObjectError::Forbidden => "Forbidden",
        ObjectError::NotModified => "NotModified",
        ObjectError::WriteNotModified => "WriteNotModified",
        ObjectError::SourceNotModified => "SourceNotModified",
        ObjectError::ChecksumMismatch => "ChecksumMismatch",
        ObjectError::HashMismatch(_) => "HashMismatch",
        ObjectError::TooLarge => "TooLarge",
        ObjectError::InvalidFixedMetadata(_) => "InvalidFixedMetadata",
        ObjectError::UniformAccessPredefinedAcl => "UniformAccessPredefinedAcl",
        ObjectError::UniformAccessAclUpdate => "UniformAccessAclUpdate",
        ObjectError::Metadata(_) => "Metadata",
        ObjectError::InvalidDerivedArgument(_) => "InvalidDerivedArgument",
        ObjectError::ComposeClassMismatch { .. } => "ComposeClassMismatch",
        ObjectError::DerivedTransferNotFound => "DerivedTransferNotFound",
        ObjectError::DerivedTransferExpired => "DerivedTransferExpired",
        ObjectError::DerivedTransferBusy => "DerivedTransferBusy",
        ObjectError::DerivedTransferIncomplete => "DerivedTransferIncomplete",
        ObjectError::UnsupportedMetadata(_) => "UnsupportedMetadata",
        ObjectError::CustomerEncryption(_) => "CustomerEncryption",
        ObjectError::BodyAborted(_) => "BodyAborted",
        ObjectError::Io(_) => "Io",
        ObjectError::Corrupt(_) => "Corrupt",
        ObjectError::Unavailable(_) => "Unavailable",
    }
}

const OBJECT_VARIANTS: [&str; 36] = [
    "BucketNotFound",
    "ObjectNotFound",
    "PreconditionFailed",
    "Component",
    "PublicAccessPrevention",
    "OverwriteForbidden",
    "ActiveHold",
    "ObjectRetentionNotMet",
    "UnlockedRetentionOverrideRequired",
    "LockedRetention",
    "ObjectNotSoftDeleted",
    "SoftDeletePolicyRequired",
    "Denied",
    "Forbidden",
    "NotModified",
    "WriteNotModified",
    "SourceNotModified",
    "ChecksumMismatch",
    "HashMismatch",
    "TooLarge",
    "InvalidFixedMetadata",
    "UniformAccessPredefinedAcl",
    "UniformAccessAclUpdate",
    "Metadata",
    "InvalidDerivedArgument",
    "ComposeClassMismatch",
    "DerivedTransferNotFound",
    "DerivedTransferExpired",
    "DerivedTransferBusy",
    "DerivedTransferIncomplete",
    "UnsupportedMetadata",
    "CustomerEncryption",
    "BodyAborted",
    "Io",
    "Corrupt",
    "Unavailable",
];

fn object_cases() -> Vec<Case<ObjectError>> {
    use crate::object::{
        ComponentFailure, ComponentFailureKind, HashMismatch, HeldObject, HoldKind, RetainedObject,
        SoftDeletedRequest, unix_nanos::UnixNanos,
    };
    let component = |kind| {
        ObjectError::Component(Box::new(ComponentFailure {
            bucket: "b".into(),
            name: "a".into(),
            kind,
        }))
    };
    let held = |kind| {
        ObjectError::ActiveHold(Box::new(HeldObject {
            bucket: "b".into(),
            name: "o".into(),
            kind,
        }))
    };
    vec![
        case(
            ObjectError::BucketNotFound,
            plain(Code::NotFound, "The specified bucket does not exist."),
        ),
        case(
            ObjectError::ObjectNotFound,
            plain(Code::NotFound, "Object not found"),
        ),
        case(
            ObjectError::PreconditionFailed,
            plain(Code::FailedPrecondition, PRECONDITION),
        ),
        case(
            component(ComponentFailureKind::NotFound),
            plain(Code::NotFound, "No such object: b/a"),
        ),
        case(
            component(ComponentFailureKind::GenerationMismatch {
                required: 1,
                actual: 7,
            }),
            plain(
                Code::FailedPrecondition,
                "Generation 1 required for component object (b/a), but actual generation is 7.",
            ),
        ),
        case(
            ObjectError::PublicAccessPrevention,
            plain(
                Code::FailedPrecondition,
                "Public access prevention is enforced",
            ),
        ),
        case(
            ObjectError::OverwriteForbidden(denied()),
            plain(Code::PermissionDenied, DENIED),
        ),
        case(
            held(HoldKind::Temporary),
            plain(
                Code::PermissionDenied,
                "Object 'b/o' is under active Temporary hold and cannot be deleted, overwritten or archived until hold is removed.",
            ),
        ),
        case(
            held(HoldKind::EventBased),
            plain(
                Code::PermissionDenied,
                "Object 'b/o' is under active Event-Based hold and cannot be deleted, overwritten or archived until hold is removed.",
            ),
        ),
        case(
            ObjectError::ObjectRetentionNotMet(Box::new(RetainedObject {
                bucket: "b".into(),
                name: "o".into(),
                until: UnixNanos::new(1_700_000_000, 500_000_000).unwrap(),
            })),
            plain(
                Code::PermissionDenied,
                "Object 'b/o' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until 2023-11-14T14:13:20.5-08:00",
            ),
        ),
        case(
            ObjectError::UnlockedRetentionOverrideRequired,
            plain(
                Code::PermissionDenied,
                "The unlocked object retention cannot be removed and its retention period cannot be shortened without overriding unlocked retention intent and permission.",
            ),
        ),
        case(
            ObjectError::LockedRetention,
            plain(
                Code::PermissionDenied,
                "The locked object retention cannot be removed. Its retention mode cannot be changed and its retention period cannot be shortened.",
            ),
        ),
        case(
            ObjectError::ObjectNotSoftDeleted,
            plain(
                Code::FailedPrecondition,
                "object is not soft deleted; select a generation from softDeleted=true listing",
            ),
        ),
        case(
            ObjectError::SoftDeletePolicyRequired(SoftDeletedRequest::List),
            plain(
                Code::InvalidArgument,
                "Soft delete policy is required to list soft-deleted versions",
            ),
        ),
        // Recorded 2026-11-18 on GetObject and RestoreObject.
        case(
            ObjectError::SoftDeletePolicyRequired(SoftDeletedRequest::Get),
            field_violation("Soft delete policy is required to request a soft-deleted version."),
        ),
        case(
            ObjectError::SoftDeletePolicyRequired(SoftDeletedRequest::Restore),
            field_violation("bucket soft delete policy must be set."),
        ),
        case(
            ObjectError::Denied(denied()),
            plain(Code::PermissionDenied, DENIED),
        ),
        case(
            ObjectError::Forbidden,
            plain(Code::PermissionDenied, "Forbidden"),
        ),
        case(
            ObjectError::NotModified,
            plain(
                Code::FailedPrecondition,
                "The resource you requested has not been modified according to the condition you specified.",
            ),
        ),
        // Recorded 2026-11-18 on DeleteObject, UpdateObject, WriteObject and RewriteObject.
        case(
            ObjectError::WriteNotModified,
            plain(
                Code::FailedPrecondition,
                "The resource you requested has not been modified according to the condition you specified.",
            ),
        ),
        // Recorded 2026-11-18 for a RewriteObject source.
        case(
            ObjectError::SourceNotModified,
            plain(
                Code::FailedPrecondition,
                "The resource you requested has not been modified according to the condition you specified.",
            ),
        ),
        case(
            ObjectError::ChecksumMismatch,
            plain(
                Code::InvalidArgument,
                "The supplied checksum does not match the uploaded data",
            ),
        ),
        case(
            ObjectError::HashMismatch(Box::new(HashMismatch::Crc32c {
                expected: 1,
                computed: 2,
            })),
            plain(
                Code::InvalidArgument,
                "Provided CRC32C \"AAAAAQ==\" doesn't match calculated CRC32C \"AAAAAg==\".",
            ),
        ),
        case(
            ObjectError::HashMismatch(Box::new(HashMismatch::Md5 {
                expected: [0; 16],
                computed: [1; 16],
            })),
            plain(
                Code::InvalidArgument,
                "Provided MD5 hash \"AAAAAAAAAAAAAAAAAAAAAA==\" doesn't match calculated MD5 hash \"AQEBAQEBAQEBAQEBAQEBAQ==\".",
            ),
        ),
        case(
            ObjectError::TooLarge,
            plain(
                Code::ResourceExhausted,
                "Object exceeds the upload size limit",
            ),
        ),
        case(
            ObjectError::InvalidFixedMetadata("Invalid contentType header value".into()),
            plain(Code::InvalidArgument, "Invalid contentType header value"),
        ),
        case(
            ObjectError::UniformAccessPredefinedAcl,
            plain(
                Code::InvalidArgument,
                "Cannot insert legacy ACL for an object when uniform bucket-level access is enabled. Read more at https://cloud.google.com/storage/docs/uniform-bucket-level-access",
            ),
        ),
        case(
            ObjectError::UniformAccessAclUpdate,
            plain(
                Code::InvalidArgument,
                "Cannot update access control for an object when uniform bucket-level access is enabled. Read more at https://cloud.google.com/storage/docs/uniform-bucket-level-access",
            ),
        ),
        case(
            ObjectError::Metadata(class_location()),
            field_violation(CLASS_LOCATION),
        ),
        case(
            ObjectError::Metadata(MetadataError::StorageClassRequiresZone),
            unimplemented(
                "gcs-mimic does not support object API setting storageClass RAPID requires a zonal bucket; omit this option or header (for example, use projection=noAcl).",
            ),
        ),
        case(
            ObjectError::Metadata(MetadataError::LongLanguage),
            plain(
                Code::InvalidArgument,
                "contentLanguage exceeds 100 characters",
            ),
        ),
        case(
            ObjectError::InvalidDerivedArgument("sourceObjects must not be empty".into()),
            plain(Code::InvalidArgument, "sourceObjects must not be empty"),
        ),
        case(
            ObjectError::ComposeClassMismatch {
                destination: StorageClass::Standard,
                sources: vec![StorageClass::Nearline, StorageClass::Coldline],
            },
            // The BadRequest was recorded 2026-11-18 with one differing class.
            field_violation(
                "Cannot compose object with destination storage class STANDARD from source components that have different storage classes (NEARLINE, COLDLINE).",
            ),
        ),
        case(
            ObjectError::DerivedTransferNotFound,
            plain(Code::NotFound, "Derived transfer not found"),
        ),
        case(
            ObjectError::DerivedTransferExpired,
            plain(Code::FailedPrecondition, "Derived transfer expired"),
        ),
        case(
            ObjectError::DerivedTransferBusy,
            plain(Code::Unavailable, "Derived transfer is already in use"),
        ),
        case(
            ObjectError::DerivedTransferIncomplete,
            plain(Code::InvalidArgument, "Derived transfer is incomplete"),
        ),
        case(
            ObjectError::UnsupportedMetadata("metadata".into()),
            unimplemented(
                "gcs-mimic does not support object API setting metadata; omit this option or header (for example, use projection=noAcl).",
            ),
        ),
        case(
            ObjectError::CustomerEncryption(crate::csek::CsekError::KeyIncorrect),
            field_violation("The provided encryption key is incorrect."),
        ),
        case(
            ObjectError::BodyAborted(BackendCause::new("client disconnected")),
            plain(
                Code::InvalidArgument,
                "The upload request body did not complete",
            ),
        ),
        case(
            ObjectError::Io(std::io::ErrorKind::StorageFull.into()),
            plain(Code::Unavailable, DISK_FULL),
        ),
        case(
            ObjectError::Io(memory_full(MemoryBudget::Blobs)),
            plain(Code::Unavailable, BLOBS_FULL),
        ),
        case(
            ObjectError::Io(memory_full(MemoryBudget::Catalog)),
            plain(Code::Unavailable, CATALOG_PAGES_FULL),
        ),
        case(
            ObjectError::Io(std::io::ErrorKind::PermissionDenied.into()),
            plain(
                Code::Unavailable,
                "Object storage is unavailable: storage I/O error (permission denied); check data directory permissions and available disk space",
            ),
        ),
        case(
            crate::storage::failure::object_context(
                "blob 7",
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ),
            plain(
                Code::Unavailable,
                "Object storage is unavailable: storage I/O error (blob 7: permission denied); check data directory permissions and available disk space",
            ),
        ),
        case(
            ObjectError::Corrupt(BackendFailure::CatalogClosed.into()),
            plain(Code::Unavailable, CORRUPT),
        ),
        case(
            ObjectError::Corrupt("invalid opaque blob ID: /secret token=private".into()),
            plain(Code::Unavailable, CORRUPT),
        )
        .logging("invalid opaque blob ID"),
        case(
            ObjectError::Unavailable(BackendFailure::CatalogClosed.into()),
            plain(
                Code::Unavailable,
                &format!("Object storage is unavailable: {CATALOG_CLOSED}"),
            ),
        ),
        case(
            ObjectError::Unavailable(BackendFailure::MissingCompletion.into()),
            plain(
                Code::Unavailable,
                "Object storage is unavailable: internal storage failure (private backend details withheld)",
            ),
        )
        .logging(UNCLASSIFIED),
        case(
            ObjectError::Unavailable("database is locked: /secret token=private".into()),
            plain(
                Code::Unavailable,
                "Object storage is unavailable: internal storage failure (private backend details withheld)",
            ),
        )
        .logging("database is locked"),
    ]
}

/// Names each variant. A new variant fails to compile here: give it a case in
/// [`catalog_cases`] and its name in [`CATALOG_VARIANTS`].
fn catalog_variant(error: &CatalogError) -> &'static str {
    match error {
        CatalogError::AuthenticationAlreadyConfigured => "AuthenticationAlreadyConfigured",
        CatalogError::AlreadyExists => "AlreadyExists",
        CatalogError::NotFound => "NotFound",
        CatalogError::NotEmpty => "NotEmpty",
        CatalogError::RestoreConflict => "RestoreConflict",
        CatalogError::PreconditionFailed => "PreconditionFailed",
        CatalogError::NotModified => "NotModified",
        CatalogError::PublicAccessPrevention => "PublicAccessPrevention",
        CatalogError::Denied(_) => "Denied",
        CatalogError::Forbidden => "Forbidden",
        CatalogError::RetentionPolicyLocked(_) => "RetentionPolicyLocked",
        CatalogError::RetentionPolicyNotSet => "RetentionPolicyNotSet",
        CatalogError::InvalidMetadata(_) => "InvalidMetadata",
        CatalogError::Metadata(_) => "Metadata",
        CatalogError::UnsupportedMetadata(_) => "UnsupportedMetadata",
        CatalogError::Io(_) => "Io",
        CatalogError::Corrupt(_) => "Corrupt",
        CatalogError::NewerSchema { .. } => "NewerSchema",
        CatalogError::Unavailable(_) => "Unavailable",
    }
}

const CATALOG_VARIANTS: [&str; 19] = [
    "AuthenticationAlreadyConfigured",
    "AlreadyExists",
    "NotFound",
    "NotEmpty",
    "RestoreConflict",
    "PreconditionFailed",
    "NotModified",
    "PublicAccessPrevention",
    "Denied",
    "Forbidden",
    "RetentionPolicyLocked",
    "RetentionPolicyNotSet",
    "InvalidMetadata",
    "Metadata",
    "UnsupportedMetadata",
    "Io",
    "Corrupt",
    "NewerSchema",
    "Unavailable",
];

fn catalog_cases() -> Vec<Case<CatalogError>> {
    vec![
        case(
            CatalogError::AuthenticationAlreadyConfigured,
            plain(Code::Unavailable, CATALOG_UNAVAILABLE),
        )
        .logging(UNCLASSIFIED),
        case(
            CatalogError::AlreadyExists,
            plain(
                Code::AlreadyExists,
                "Your previous request to create the named bucket succeeded and you already own it.",
            ),
        ),
        case(
            CatalogError::NotFound,
            plain(Code::NotFound, "The specified bucket does not exist."),
        ),
        case(
            CatalogError::NotEmpty,
            plain(
                Code::FailedPrecondition,
                "The bucket you tried to delete is not empty.",
            ),
        ),
        case(
            CatalogError::RestoreConflict,
            plain(
                Code::AlreadyExists,
                "A bucket with this name already exists, so the soft-deleted bucket cannot be restored.",
            ),
        ),
        case(
            CatalogError::PreconditionFailed,
            plain(Code::FailedPrecondition, PRECONDITION),
        ),
        // Storage v2 words a not-match that alone fails apart (recorded on DeleteBucket,
        // GetBucket and UpdateBucket); the legacy v1 service maps it itself.
        case(
            CatalogError::NotModified,
            plain(
                Code::FailedPrecondition,
                "The resource you requested has not been modified according to the condition you specified.",
            ),
        ),
        case(
            CatalogError::PublicAccessPrevention,
            plain(
                Code::FailedPrecondition,
                "Public access prevention is enforced",
            ),
        ),
        case(
            CatalogError::Denied(denied()),
            plain(Code::PermissionDenied, DENIED),
        ),
        case(
            CatalogError::Forbidden,
            plain(Code::PermissionDenied, "Forbidden"),
        ),
        case(
            CatalogError::RetentionPolicyLocked(3600),
            plain(
                Code::InvalidArgument,
                "Bucket retention policy is locked and cannot be removed or shortened; keep retentionPeriod at least 3600 seconds or extend it, for example {\"retentionPolicy\":{\"retentionPeriod\":\"3601\"}}",
            ),
        ),
        case(
            CatalogError::RetentionPolicyNotSet,
            plain(
                Code::InvalidArgument,
                "No retention policy to lock; set retentionPolicy.retentionPeriod first, for example 3600",
            ),
        ),
        case(
            CatalogError::InvalidMetadata("Invalid value for: x is not a valid value".into()),
            plain(
                Code::InvalidArgument,
                "Invalid value for: x is not a valid value",
            ),
        ),
        case(
            CatalogError::Metadata(class_location()),
            field_violation(CLASS_LOCATION),
        ),
        case(
            CatalogError::Metadata(MetadataError::StorageClassRequiresZone),
            unimplemented(
                "gcs-mimic does not support bucket API setting storageClass RAPID requires a zonal bucket; omit this setting (for example, create a bucket with only name).",
            ),
        ),
        case(
            CatalogError::Metadata(MetadataError::AutoclassTerminal),
            plain(
                Code::InvalidArgument,
                "Invalid value for terminalStorageClass field in Autoclass config. Valid values are NEARLINE and ARCHIVE",
            ),
        ),
        case(
            CatalogError::UnsupportedMetadata("cors".into()),
            unimplemented(
                "gcs-mimic does not support bucket API setting cors; omit this setting (for example, create a bucket with only name).",
            ),
        ),
        case(
            CatalogError::Io(std::io::ErrorKind::StorageFull.into()),
            plain(Code::Unavailable, DISK_FULL),
        ),
        case(
            CatalogError::Io(memory_full(MemoryBudget::Catalog)),
            plain(Code::Unavailable, CATALOG_PAGES_FULL),
        ),
        case(
            CatalogError::Io(std::io::ErrorKind::PermissionDenied.into()),
            plain(Code::Unavailable, CATALOG_UNAVAILABLE),
        )
        .logging(UNCLASSIFIED),
        case(
            CatalogError::Corrupt(BackendFailure::CatalogClosed.into()),
            plain(
                Code::Unavailable,
                &format!("Bucket catalog is unavailable: {CATALOG_CLOSED}"),
            ),
        ),
        case(
            CatalogError::Corrupt("invalid opaque blob ID: /secret token=private".into()),
            plain(Code::Unavailable, CATALOG_UNAVAILABLE),
        )
        .logging("invalid opaque blob ID"),
        case(
            CatalogError::NewerSchema {
                version: 99,
                supported: 50,
            },
            plain(Code::Unavailable, CATALOG_UNAVAILABLE),
        )
        .logging(UNCLASSIFIED),
        case(
            CatalogError::Unavailable(BackendFailure::CatalogClosed.into()),
            plain(
                Code::Unavailable,
                &format!("Bucket catalog is unavailable: {CATALOG_CLOSED}"),
            ),
        ),
        case(
            CatalogError::Unavailable("database is locked: /secret token=private".into()),
            plain(Code::Unavailable, CATALOG_UNAVAILABLE),
        )
        .logging("database is locked"),
    ]
}

fn variants<E>(cases: &[Case<E>], name: fn(&E) -> &'static str) -> BTreeSet<&'static str> {
    cases.iter().map(|case| name(&case.error)).collect()
}

#[test]
fn every_object_error_has_its_storage_v2_status() {
    let cases = object_cases();
    assert_eq!(
        variants(&cases, object_variant),
        BTreeSet::from(OBJECT_VARIANTS)
    );
    for Case { error, reply, .. } in cases {
        let label = format!("{error:?}");
        assert_eq!(Reply::of(&Status::from(error)), reply, "{label}");
    }
}

/// Handlers that name the object they act on reword a missing object and must report every
/// other failure, with its details, as the direct conversion does.
#[test]
fn the_object_naming_path_reports_object_errors_alike() {
    let named = crate::grpc::v2::object_io::naming_object("bucket", "object");
    for Case { error, reply, .. } in object_cases() {
        let label = format!("{error:?}");
        let expected = if error == ObjectError::ObjectNotFound {
            plain(Code::NotFound, "No such object: bucket/object")
        } else {
            reply
        };
        assert_eq!(Reply::of(&named(error)), expected, "{label}");
    }
}

#[test]
fn every_catalog_error_has_its_storage_v2_status() {
    let cases = catalog_cases();
    assert_eq!(
        variants(&cases, catalog_variant),
        BTreeSet::from(CATALOG_VARIANTS)
    );
    for Case { error, reply, .. } in cases {
        let label = format!("{error:?}");
        assert_eq!(Reply::of(&Status::from(error)), reply, "{label}");
    }
}

/// A backend cause without fixed guidance is logged with only its allowlisted text, once per
/// conversion; a cause with guidance, a full disk and every domain refusal log nothing.
#[test]
fn unguided_backend_failures_log_only_their_allowlisted_cause() {
    const CHILD: &str = "GCSM_TEST_GRPC_STATUS_LOG";
    if std::env::var_os(CHILD).is_some() {
        for case in object_cases() {
            let _status = Status::from(case.error);
        }
        for case in catalog_cases() {
            let _status = Status::from(case.error);
        }
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "grpc::v2::status::domain_tests::unguided_backend_failures_log_only_their_allowlisted_cause",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("/secret"), "{stderr}");
    assert!(!stderr.contains("token="), "{stderr}");
    let logged: Vec<String> = stderr
        .lines()
        .map(|line| {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(event["event"], "object_backend_failure", "{line}");
            assert_eq!(event["level"], "error", "{line}");
            event["cause"].as_str().unwrap().to_owned()
        })
        .collect();
    let expected: Vec<&str> = object_cases()
        .iter()
        .filter_map(|case| case.logged)
        .chain(catalog_cases().iter().filter_map(|case| case.logged))
        .collect();
    assert_eq!(logged, expected);
}
