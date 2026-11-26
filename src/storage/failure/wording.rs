//! Client-visible wording of storage failures that more than one protocol adapter reports.
//!
//! Each adapter matches the storage errors itself and chooses its own status; the text they
//! report alike lives here, so that one adapter's wording cannot drift from another's.

use super::{BackendCause, BackendFailure, IoCause, MemoryBudget};
use crate::object::{HashMismatch, HeldObject, HoldKind, RetainedObject};
use std::{borrow::Cow, io};

/// Cloud Storage's message for a bucket that does not exist, shared by every bucket-scoped route.
pub(crate) const BUCKET_NOT_FOUND: &str = "The specified bucket does not exist.";
/// The missing-object message for operations that do not name one object, such as a copy source.
pub(crate) const OBJECT_NOT_FOUND: &str = "Object not found";
/// The message of a failed metageneration or entity-tag precondition on a bucket or policy.
pub(crate) const PRECONDITION_NOT_HELD: &str =
    "At least one of the pre-conditions you specified did not hold.";
/// The documented example message, for a refusal that no Cloud Storage permission names.
pub(crate) const FORBIDDEN: &str = "Forbidden";
/// The documented status-code example for a missing credential.
pub(crate) const LOGIN_REQUIRED: &str = "Login Required";
/// The message of a credential that does not verify; only its reason is documented.
pub(crate) const INVALID_CREDENTIALS: &str = "Invalid Credentials";
pub(crate) const PUBLIC_ACCESS_PREVENTED: &str = "Public access prevention is enforced";
pub(crate) const BUCKET_ALREADY_OWNED: &str =
    "Your previous request to create the named bucket succeeded and you already own it.";
/// The wording is a local choice; only the "you already own it" text of a create conflict was
/// recorded.
pub(crate) const RESTORE_CONFLICT: &str =
    "A bucket with this name already exists, so the soft-deleted bucket cannot be restored.";
pub(crate) const CHECKSUM_MISMATCH: &str = "The supplied checksum does not match the uploaded data";
pub(crate) const UPLOAD_TOO_LARGE: &str = "Object exceeds the upload size limit";
pub(crate) const BODY_ABORTED: &str = "The upload request body did not complete";
pub(crate) const DERIVED_TRANSFER_NOT_FOUND: &str = "Derived transfer not found";
pub(crate) const DERIVED_TRANSFER_EXPIRED: &str = "Derived transfer expired";
pub(crate) const DERIVED_TRANSFER_BUSY: &str = "Derived transfer is already in use";
pub(crate) const DERIVED_TRANSFER_INCOMPLETE: &str = "Derived transfer is incomplete";

/// The prefix of Cloud Storage's missing-object message, followed by `<bucket>/<object>`.
const NO_SUCH_OBJECT: &str = "No such object: ";

/// Cloud Storage's missing-object message, which names the object.
pub(crate) fn missing_named_object(bucket: &str, name: &str) -> String {
    format!("{NO_SUCH_OBJECT}{bucket}/{name}")
}

/// A compose component whose `ifGenerationMatch` named a generation other than the live one.
pub(crate) fn component_generation_mismatch(
    bucket: &str,
    name: &str,
    required: i64,
    actual: i64,
) -> String {
    format!(
        "Generation {required} required for component object ({bucket}/{name}), but actual generation is {actual}."
    )
}

pub(crate) fn held_object(held: &HeldObject) -> String {
    let kind = match held.kind {
        HoldKind::Temporary => "Temporary",
        HoldKind::EventBased => "Event-Based",
    };
    format!(
        "Object '{}/{}' is under active {kind} hold and cannot be deleted, overwritten or archived until hold is removed.",
        held.bucket, held.name
    )
}

/// Bucket retention policies and object retention share the wording (observed 2026-10-31,
/// including the refused overwrite).
pub(crate) fn retained_object(retained: &RetainedObject) -> String {
    format!(
        "Object '{}/{}' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until {}",
        retained.bucket,
        retained.name,
        crate::object::retention::pacific_rfc3339(retained.until)
    )
}

/// Names both digests in base64, at the width of their algorithm.
pub(crate) fn hash_mismatch(mismatch: &HashMismatch) -> String {
    let (name, expected, computed) = match *mismatch {
        HashMismatch::Crc32c { expected, computed } => (
            "CRC32C",
            crate::base64::encode(&expected.to_be_bytes()),
            crate::base64::encode(&computed.to_be_bytes()),
        ),
        HashMismatch::Md5 { expected, computed } => (
            "MD5 hash",
            crate::base64::encode(&expected),
            crate::base64::encode(&computed),
        ),
    };
    format!("Provided {name} \"{expected}\" doesn't match calculated {name} \"{computed}\".")
}

/// A recognized bucket setting gcs-mimic does not implement, with the request that avoids it.
pub(crate) fn unsupported_bucket_setting(name: &str) -> String {
    format!(
        "gcs-mimic does not support bucket API setting {name}; omit this setting (for example, create a bucket with only name)."
    )
}

/// A rejected ACL entity, role or entry list. Cloud Storage's wording covers the two most common
/// mistakes (recorded 2026-10-31); the others keep the error's own description.
pub(crate) fn acl_refusal(error: &crate::acl::AclError) -> String {
    use crate::acl::AclError;
    match error {
        AclError::InvalidEntity(entity) => format!("Scope text \"{entity}\" is not valid."),
        AclError::InvalidRole { role, .. } => {
            format!("Invalid value for: {role} is not a valid value")
        }
        AclError::TooManyEntries | AclError::DuplicateEntity(_) | AclError::EntityTooLong => {
            error.to_string()
        }
    }
}

/// A recognized object setting gcs-mimic does not implement, with the request that avoids it.
pub(crate) fn unsupported_object_setting(name: &str) -> String {
    format!(
        "gcs-mimic does not support object API setting {name}; omit this option or header (for example, use projection=noAcl)."
    )
}

/// Names only the remedy for the storage that ran out: a disk backend never has a RAM budget
/// to raise, and a memory backend has no data directory to free.
pub(crate) fn storage_capacity(cause: &IoCause) -> &'static str {
    match cause.memory_budget() {
        None => {
            "Storage capacity exhausted; free space in the data directory, where data kept by soft delete uses space until it expires (set softDeletePolicy.retentionDurationSeconds to 0 on buckets that retain deletions), then retry with generation preconditions"
        }
        // Deleting does not free what soft delete retains, which can be what filled RAM.
        Some(MemoryBudget::Blobs) => {
            "Storage capacity exhausted; delete objects, though data kept by soft delete keeps its bytes until it expires (set softDeletePolicy.retentionDurationSeconds to 0 on buckets that retain deletions), or restart with a larger memory_max_bytes (a restart loses RAM data)"
        }
        Some(MemoryBudget::Catalog) => {
            "Storage capacity exhausted; restart with a larger memory_catalog_max_bytes (a restart loses RAM data)"
        }
    }
}

/// An object storage I/O failure: full storage names its remedy, and any other failure names
/// its kind, or the entry it hit when the failure carries one.
pub(crate) fn object_io(cause: &IoCause) -> Cow<'static, str> {
    if cause.kind() == io::ErrorKind::StorageFull {
        return storage_capacity(cause).into();
    }
    format!(
        "Object storage is unavailable: storage I/O error ({cause}); check data directory permissions and available disk space"
    )
    .into()
}

/// What a reply says about an unavailable or corrupt backend, and the fixed failure the message
/// reports, which adapters keep for classification.
pub(crate) struct BackendNotice {
    pub(crate) message: Cow<'static, str>,
    pub(crate) failure: Option<BackendFailure>,
}

pub(crate) fn object_backend(backend: &BackendCause) -> BackendNotice {
    match guidance(backend) {
        Some((failure, remedy)) => BackendNotice {
            message: format!("Object storage is unavailable: {remedy}").into(),
            failure: Some(failure),
        },
        None => BackendNotice {
            message: "Object storage is unavailable: internal storage failure (private backend details withheld)".into(),
            failure: None,
        },
    }
}

pub(crate) fn catalog_backend(backend: &BackendCause) -> BackendNotice {
    match guidance(backend) {
        Some((failure, remedy)) => BackendNotice {
            message: format!("Bucket catalog is unavailable: {remedy}").into(),
            failure: Some(failure),
        },
        None => BackendNotice {
            message: "Bucket catalog is unavailable".into(),
            failure: None,
        },
    }
}

/// Corrupt stored data has one remedy whatever the failure, which is still kept.
pub(crate) fn corrupt_object(cause: &BackendCause) -> BackendNotice {
    let failure = guidance(cause).map(|(failure, _)| failure);
    BackendNotice {
        message:
            "Object storage is unavailable: stored data is corrupt; restore a valid data directory"
                .into(),
        failure,
    }
}

/// A failure with fixed guidance keeps its kind for classification and returns its remedy.
/// Any other cause is logged with only the allowlisted text and has no remedy to show: arbitrary
/// backend text can contain paths or secrets, so only fixed diagnostics cross the protocol
/// boundary.
fn guidance(backend: &BackendCause) -> Option<(BackendFailure, &'static str)> {
    if let Some(failure) = backend.failure()
        && let Some(remedy) = failure.remedy()
    {
        return Some((failure, remedy));
    }
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Error,
        "object_backend_failure",
        BackendFailureEvent::from(backend),
    );
    None
}

/// The fields of an `object_backend_failure` event, and of other events that log a backend
/// failure.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct BackendFailureEvent {
    /// The allowlisted cause, or a fixed text that withholds it.
    cause: &'static str,
    /// One message such as `disk I/O error` stands for many failures (no usable temporary
    /// directory, a failed fsync, a short read); SQLite's extended code tells them apart without
    /// exposing paths or values.
    #[serde(skip_serializing_if = "Option::is_none")]
    sqlite_extended_code: Option<std::ffi::c_int>,
}

impl From<&BackendCause> for BackendFailureEvent {
    fn from(backend: &BackendCause) -> Self {
        let message = backend.message();
        let cause = message
            .strip_prefix("catalog unavailable: ")
            .unwrap_or(message);
        Self {
            cause: safe_backend_cause(cause),
            sqlite_extended_code: backend.sqlite_extended_code(),
        }
    }
}

pub(crate) fn safe_backend_cause(cause: &str) -> &'static str {
    // Only known fixed causes are safe; backend strings may embed credentials or SQL values.
    const SAFE_CAUSES: &[&str] = &[
        "database is locked",
        "database is busy",
        "database or disk is full",
        "disk I/O error",
        "attempt to write a readonly database",
        "Permission denied",
        "No space left on device",
        "No such file or directory",
        "Acknowledged upload prefix is missing",
        "invalid resumable catalog state",
        "invalid opaque blob ID",
        "deletion journal references a live blob",
        "generation exhausted",
        "metageneration exhausted",
        "bucket metageneration exhausted",
        "derived publication catalog admission timed out",
        "catalog is closing",
    ];
    SAFE_CAUSES
        .iter()
        .copied()
        .find(|safe| {
            cause == *safe
                || cause
                    .strip_prefix(safe)
                    .is_some_and(|tail| tail.starts_with(": ") || tail.starts_with(" (os error "))
        })
        .unwrap_or("unrecognized backend failure (private details withheld)")
}
