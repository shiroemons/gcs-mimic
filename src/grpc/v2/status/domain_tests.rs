//! Pins the Storage v2 status of every storage domain error: its code, its message, its details
//! and whether it logs a backend cause. Each case names one branch of the mapping; the inputs are
//! the shared cases in [`crate::storage::failure::test_support`].

use super::*;
use crate::{
    bucket::CatalogError,
    object::ObjectError,
    storage::failure::test_support::{
        self as domain, CATALOG_VARIANTS, OBJECT_VARIANTS, catalog_variant, object_variant,
    },
};
use std::collections::{BTreeMap, BTreeSet};

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

/// Pairs each shared case with the reply listed under its label. A case without a reply, a
/// reply without a case and a label listed twice each fail.
fn with_replies<E>(
    cases: Vec<domain::Case<E>>,
    replies: Vec<(&'static str, Reply)>,
) -> Vec<Case<E>> {
    let listed = replies.len();
    let mut replies: BTreeMap<&str, Reply> = replies.into_iter().collect();
    assert_eq!(replies.len(), listed, "a label has two replies");
    let cases = cases
        .into_iter()
        .map(|case| Case {
            reply: replies
                .remove(case.label)
                .unwrap_or_else(|| panic!("no reply for {}", case.label)),
            error: case.error,
            logged: case.logged,
        })
        .collect();
    assert!(
        replies.is_empty(),
        "replies without a case: {:?}",
        replies.keys().collect::<Vec<_>>()
    );
    cases
}

const PRECONDITION: &str = "At least one of the pre-conditions you specified did not hold.";
const DENIED: &str = "viewer@example.invalid does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist).";
const ANONYMOUS_DENIED: &str = "Anonymous caller does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist).";
const DISK_FULL: &str = "Storage capacity exhausted; free space in the data directory, where data kept by soft delete uses space until it expires (set softDeletePolicy.retentionDurationSeconds to 0 on buckets that retain deletions), then retry with generation preconditions";
const BLOBS_FULL: &str = "Storage capacity exhausted; delete objects, though data kept by soft delete keeps its bytes until it expires (set softDeletePolicy.retentionDurationSeconds to 0 on buckets that retain deletions), or restart with a larger memory_max_bytes (a restart loses RAM data)";
const CATALOG_PAGES_FULL: &str = "Storage capacity exhausted; restart with a larger memory_catalog_max_bytes (a restart loses RAM data)";
const CATALOG_CLOSED: &str = "catalog is closed; wait for the server to restart before retrying";
const CORRUPT: &str =
    "Object storage is unavailable: stored data is corrupt; restore a valid data directory";
const CATALOG_UNAVAILABLE: &str = "Bucket catalog is unavailable";

const CLASS_LOCATION: &str =
    "Project 123456 may not create storageClass REGIONAL buckets with locationConstraint US.";

fn object_cases() -> Vec<Case<ObjectError>> {
    with_replies(
        domain::object_cases(),
        vec![
            (
                "BucketNotFound",
                plain(Code::NotFound, "The specified bucket does not exist."),
            ),
            ("ObjectNotFound", plain(Code::NotFound, "Object not found")),
            (
                "PreconditionFailed",
                plain(Code::FailedPrecondition, PRECONDITION),
            ),
            (
                "Component(NotFound)",
                plain(Code::NotFound, "No such object: b/a"),
            ),
            (
                "Component(GenerationMismatch)",
                plain(
                    Code::FailedPrecondition,
                    "Generation 1 required for component object (b/a), but actual generation is 7.",
                ),
            ),
            (
                "PublicAccessPrevention",
                plain(
                    Code::FailedPrecondition,
                    "Public access prevention is enforced",
                ),
            ),
            ("OverwriteForbidden", plain(Code::PermissionDenied, DENIED)),
            (
                "ActiveHold(Temporary)",
                plain(
                    Code::PermissionDenied,
                    "Object 'b/o' is under active Temporary hold and cannot be deleted, overwritten or archived until hold is removed.",
                ),
            ),
            (
                "ActiveHold(EventBased)",
                plain(
                    Code::PermissionDenied,
                    "Object 'b/o' is under active Event-Based hold and cannot be deleted, overwritten or archived until hold is removed.",
                ),
            ),
            (
                "ObjectRetentionNotMet",
                plain(
                    Code::PermissionDenied,
                    "Object 'b/o' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until 2023-11-14T14:13:20.5-08:00",
                ),
            ),
            (
                "UnlockedRetentionOverrideRequired",
                plain(
                    Code::PermissionDenied,
                    "The unlocked object retention cannot be removed and its retention period cannot be shortened without overriding unlocked retention intent and permission.",
                ),
            ),
            (
                "LockedRetention",
                plain(
                    Code::PermissionDenied,
                    "The locked object retention cannot be removed. Its retention mode cannot be changed and its retention period cannot be shortened.",
                ),
            ),
            (
                "ObjectNotSoftDeleted",
                plain(
                    Code::FailedPrecondition,
                    "object is not soft deleted; select a generation from softDeleted=true listing",
                ),
            ),
            (
                "SoftDeletePolicyRequired(List)",
                plain(
                    Code::InvalidArgument,
                    "Soft delete policy is required to list soft-deleted versions",
                ),
            ),
            // Recorded 2026-11-18 on GetObject and RestoreObject.
            (
                "SoftDeletePolicyRequired(Get)",
                field_violation(
                    "Soft delete policy is required to request a soft-deleted version.",
                ),
            ),
            (
                "SoftDeletePolicyRequired(Restore)",
                field_violation("bucket soft delete policy must be set."),
            ),
            ("Denied", plain(Code::PermissionDenied, DENIED)),
            (
                "Denied(anonymous)",
                plain(Code::PermissionDenied, ANONYMOUS_DENIED),
            ),
            ("Forbidden", plain(Code::PermissionDenied, "Forbidden")),
            (
                "NotModified",
                plain(
                    Code::FailedPrecondition,
                    "The resource you requested has not been modified according to the condition you specified.",
                ),
            ),
            // Recorded 2026-11-18 on DeleteObject, UpdateObject, WriteObject and RewriteObject.
            (
                "WriteNotModified",
                plain(
                    Code::FailedPrecondition,
                    "The resource you requested has not been modified according to the condition you specified.",
                ),
            ),
            // Recorded 2026-11-18 for a RewriteObject source.
            (
                "SourceNotModified",
                plain(
                    Code::FailedPrecondition,
                    "The resource you requested has not been modified according to the condition you specified.",
                ),
            ),
            (
                "ChecksumMismatch",
                plain(
                    Code::InvalidArgument,
                    "The supplied checksum does not match the uploaded data",
                ),
            ),
            (
                "HashMismatch(Crc32c)",
                plain(
                    Code::InvalidArgument,
                    "Provided CRC32C \"AAAAAQ==\" doesn't match calculated CRC32C \"AAAAAg==\".",
                ),
            ),
            (
                "HashMismatch(Md5)",
                plain(
                    Code::InvalidArgument,
                    "Provided MD5 hash \"AAAAAAAAAAAAAAAAAAAAAA==\" doesn't match calculated MD5 hash \"AQEBAQEBAQEBAQEBAQEBAQ==\".",
                ),
            ),
            (
                "TooLarge",
                plain(
                    Code::ResourceExhausted,
                    "Object exceeds the upload size limit",
                ),
            ),
            (
                "InvalidFixedMetadata",
                plain(Code::InvalidArgument, "Invalid contentType header value"),
            ),
            (
                "UniformAccessPredefinedAcl",
                plain(
                    Code::InvalidArgument,
                    "Cannot insert legacy ACL for an object when uniform bucket-level access is enabled. Read more at https://cloud.google.com/storage/docs/uniform-bucket-level-access",
                ),
            ),
            (
                "UniformAccessAclUpdate",
                plain(
                    Code::InvalidArgument,
                    "Cannot update access control for an object when uniform bucket-level access is enabled. Read more at https://cloud.google.com/storage/docs/uniform-bucket-level-access",
                ),
            ),
            ("Metadata(ClassLocation)", field_violation(CLASS_LOCATION)),
            (
                "Metadata(StorageClassRequiresZone)",
                unimplemented(
                    "gcs-mimic does not support object API setting storageClass RAPID requires a zonal bucket; omit this option or header (for example, use projection=noAcl).",
                ),
            ),
            (
                "Metadata(LongLanguage)",
                plain(
                    Code::InvalidArgument,
                    "contentLanguage exceeds 100 characters",
                ),
            ),
            (
                "InvalidDerivedArgument",
                plain(Code::InvalidArgument, "sourceObjects must not be empty"),
            ),
            // The BadRequest was recorded 2026-11-18 with one differing class.
            (
                "ComposeClassMismatch",
                field_violation(
                    "Cannot compose object with destination storage class STANDARD from source components that have different storage classes (NEARLINE, COLDLINE).",
                ),
            ),
            (
                "DerivedTransferNotFound",
                plain(Code::NotFound, "Derived transfer not found"),
            ),
            (
                "DerivedTransferExpired",
                plain(Code::FailedPrecondition, "Derived transfer expired"),
            ),
            (
                "DerivedTransferBusy",
                plain(Code::Unavailable, "Derived transfer is already in use"),
            ),
            (
                "DerivedTransferIncomplete",
                plain(Code::InvalidArgument, "Derived transfer is incomplete"),
            ),
            (
                "UnsupportedMetadata",
                unimplemented(
                    "gcs-mimic does not support object API setting metadata; omit this option or header (for example, use projection=noAcl).",
                ),
            ),
            (
                "CustomerEncryption(KeyIncorrect)",
                field_violation("The provided encryption key is incorrect."),
            ),
            (
                "BodyAborted",
                plain(
                    Code::InvalidArgument,
                    "The upload request body did not complete",
                ),
            ),
            ("Io(StorageFull)", plain(Code::Unavailable, DISK_FULL)),
            ("Io(memory blobs)", plain(Code::Unavailable, BLOBS_FULL)),
            (
                "Io(memory catalog)",
                plain(Code::Unavailable, CATALOG_PAGES_FULL),
            ),
            (
                "Io(PermissionDenied)",
                plain(
                    Code::Unavailable,
                    "Object storage is unavailable: storage I/O error (permission denied); check data directory permissions and available disk space",
                ),
            ),
            (
                "Io(with context)",
                plain(
                    Code::Unavailable,
                    "Object storage is unavailable: storage I/O error (blob 7: permission denied); check data directory permissions and available disk space",
                ),
            ),
            ("Corrupt(CatalogClosed)", plain(Code::Unavailable, CORRUPT)),
            ("Corrupt(private cause)", plain(Code::Unavailable, CORRUPT)),
            (
                "Unavailable(CatalogClosed)",
                plain(
                    Code::Unavailable,
                    &format!("Object storage is unavailable: {CATALOG_CLOSED}"),
                ),
            ),
            (
                "Unavailable(MissingCompletion)",
                plain(
                    Code::Unavailable,
                    "Object storage is unavailable: internal storage failure (private backend details withheld)",
                ),
            ),
            (
                "Unavailable(private cause)",
                plain(
                    Code::Unavailable,
                    "Object storage is unavailable: internal storage failure (private backend details withheld)",
                ),
            ),
        ],
    )
}

fn catalog_cases() -> Vec<Case<CatalogError>> {
    with_replies(
        domain::catalog_cases(),
        vec![
            (
                "AuthenticationAlreadyConfigured",
                plain(Code::Unavailable, CATALOG_UNAVAILABLE),
            ),
            (
                "AlreadyExists",
                plain(
                    Code::AlreadyExists,
                    "Your previous request to create the named bucket succeeded and you already own it.",
                ),
            ),
            (
                "NotFound",
                plain(Code::NotFound, "The specified bucket does not exist."),
            ),
            (
                "NotEmpty",
                plain(
                    Code::FailedPrecondition,
                    "The bucket you tried to delete is not empty.",
                ),
            ),
            (
                "RestoreConflict",
                plain(
                    Code::AlreadyExists,
                    "A bucket with this name already exists, so the soft-deleted bucket cannot be restored.",
                ),
            ),
            (
                "PreconditionFailed",
                plain(Code::FailedPrecondition, PRECONDITION),
            ),
            // Storage v2 words a not-match that alone fails apart (recorded on DeleteBucket,
            // GetBucket and UpdateBucket); the legacy v1 service maps it itself.
            (
                "NotModified",
                plain(
                    Code::FailedPrecondition,
                    "The resource you requested has not been modified according to the condition you specified.",
                ),
            ),
            (
                "PublicAccessPrevention",
                plain(
                    Code::FailedPrecondition,
                    "Public access prevention is enforced",
                ),
            ),
            ("Denied", plain(Code::PermissionDenied, DENIED)),
            (
                "Denied(anonymous)",
                plain(Code::PermissionDenied, ANONYMOUS_DENIED),
            ),
            ("Forbidden", plain(Code::PermissionDenied, "Forbidden")),
            (
                "RetentionPolicyLocked",
                plain(
                    Code::InvalidArgument,
                    "Bucket retention policy is locked and cannot be removed or shortened; keep retentionPeriod at least 3600 seconds or extend it, for example {\"retentionPolicy\":{\"retentionPeriod\":\"3601\"}}",
                ),
            ),
            (
                "RetentionPolicyNotSet",
                plain(
                    Code::InvalidArgument,
                    "No retention policy to lock; set retentionPolicy.retentionPeriod first, for example 3600",
                ),
            ),
            (
                "InvalidMetadata",
                plain(
                    Code::InvalidArgument,
                    "Invalid value for: x is not a valid value",
                ),
            ),
            ("Metadata(ClassLocation)", field_violation(CLASS_LOCATION)),
            (
                "Metadata(StorageClassRequiresZone)",
                unimplemented(
                    "gcs-mimic does not support bucket API setting storageClass RAPID requires a zonal bucket; omit this setting (for example, create a bucket with only name).",
                ),
            ),
            (
                "Metadata(AutoclassTerminal)",
                plain(
                    Code::InvalidArgument,
                    "Invalid value for terminalStorageClass field in Autoclass config. Valid values are NEARLINE and ARCHIVE",
                ),
            ),
            (
                "UnsupportedMetadata",
                unimplemented(
                    "gcs-mimic does not support bucket API setting cors; omit this setting (for example, create a bucket with only name).",
                ),
            ),
            ("Io(StorageFull)", plain(Code::Unavailable, DISK_FULL)),
            (
                "Io(memory catalog)",
                plain(Code::Unavailable, CATALOG_PAGES_FULL),
            ),
            (
                "Io(PermissionDenied)",
                plain(Code::Unavailable, CATALOG_UNAVAILABLE),
            ),
            (
                "Corrupt(CatalogClosed)",
                plain(
                    Code::Unavailable,
                    &format!("Bucket catalog is unavailable: {CATALOG_CLOSED}"),
                ),
            ),
            (
                "Corrupt(private cause)",
                plain(Code::Unavailable, CATALOG_UNAVAILABLE),
            ),
            ("NewerSchema", plain(Code::Unavailable, CATALOG_UNAVAILABLE)),
            (
                "Unavailable(CatalogClosed)",
                plain(
                    Code::Unavailable,
                    &format!("Bucket catalog is unavailable: {CATALOG_CLOSED}"),
                ),
            ),
            (
                "Unavailable(private cause)",
                plain(Code::Unavailable, CATALOG_UNAVAILABLE),
            ),
        ],
    )
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

/// Authentication failures map to statuses directly; each status stays the one the JSON error
/// conversion produced.
#[test]
fn authentication_failures_answer_as_their_json_conversion_did() {
    use crate::auth::AuthFailure;
    let denied = || {
        crate::auth::Denied::new(
            None,
            crate::auth::OBJECTS_GET,
            crate::auth::denied::Resource::in_bucket(crate::auth::OBJECTS_GET, "bucket"),
        )
    };
    let failures = || {
        [
            AuthFailure::LoginRequired,
            AuthFailure::InvalidCredentials,
            AuthFailure::Denied(denied()),
            AuthFailure::Forbidden,
        ]
    };
    for (direct, converted) in failures().into_iter().zip(failures()) {
        assert_eq!(
            Reply::of(&Status::from(direct)),
            Reply::of(&Status::from(crate::api_error::ApiError::from(converted)))
        );
    }
}
