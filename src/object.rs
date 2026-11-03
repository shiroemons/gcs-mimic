//! Transport-independent object state shared by the catalog and protocol adapters.

use crate::{
    bucket::StorageClass,
    storage::failure::{BackendCause, IoCause},
};
use std::{collections::BTreeMap, fmt, time::SystemTime};
use unix_nanos::UnixNanos;

pub(crate) mod owner;
pub(crate) mod retention;
pub(crate) mod unix_nanos;

/// Multipart uploads retain an internal digest without exposing an MD5 checksum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Md5Availability {
    #[default]
    Available,
    Unavailable,
}

/// Only a transport-confirmed EOF permits publication; a dropped producer is an abort.
pub enum UploadChunk {
    Data(bytes::Bytes),
    End,
    EndWithHashes(ExpectedHashes),
}

/// Immutable generation metadata persisted for live and noncurrent objects.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ObjectRecord {
    pub bucket: String,
    pub name: String,
    pub generation: i64,
    pub metageneration: i64,
    /// Older generations have no recoverable uploader identity.
    #[serde(default)]
    pub(crate) owner: Option<owner::ObjectOwner>,
    pub content_type: String,
    pub content_encoding: Option<String>,
    pub content_disposition: Option<String>,
    pub content_language: Option<String>,
    pub cache_control: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub size: u64,
    pub md5: [u8; 16],
    #[serde(default)]
    pub md5_availability: Md5Availability,
    pub crc32c: u32,
    pub component_count: Option<i64>,
    pub storage_class: StorageClass,
    /// Microsecond precision in the catalog; protocol adapters retain their wire precision.
    pub time_created: SystemTime,
    pub updated: SystemTime,
    pub time_deleted: Option<SystemTime>,
    pub soft_delete_time: Option<SystemTime>,
    pub hard_delete_time: Option<SystemTime>,
    /// Three states, as Cloud Storage reports them: `None` (never set, omitted on the wire),
    /// `Some(false)` (set before and released, reported as `false`) and `Some(true)`. Only
    /// `Some(true)` blocks deletion and overwrite; use `has_hold` instead of reading these.
    pub event_based_hold: Option<bool>,
    pub temporary_hold: Option<bool>,
    pub custom_time: Option<String>,
    /// Bucket retention policy expiration only; object retention is kept in `retention`.
    pub retention_expiration_time: Option<SystemTime>,
    pub(crate) retention: Option<retention::ObjectRetention>,
    /// SHA-256 of the customer-supplied AES256 key; the key itself is never stored.
    pub customer_key_sha256: Option<[u8; 32]>,
}

impl ObjectRecord {
    pub(crate) fn has_md5(&self) -> bool {
        self.md5_availability == Md5Availability::Available && self.component_count.is_none()
    }

    /// A PUT replaces the resource, so a hold it does not name returns to never set (recorded:
    /// a PUT of only `metadata` under a temporary hold answered without the hold, not `false`).
    pub(crate) fn clear_holds(&mut self) {
        self.event_based_hold = None;
        self.temporary_hold = None;
    }

    pub(crate) fn has_hold(&self) -> bool {
        self.event_based_hold == Some(true) || self.temporary_hold == Some(true)
    }

    /// Guards removing this generation permanently (or soft deleting it). With both a bucket
    /// policy and an object retention in force, the denial names the later deadline (an
    /// inference; Cloud Storage was only observed with one of them).
    pub(crate) fn check_retention(&self, now: SystemTime) -> Result<(), ObjectError> {
        match (
            self.check_policy_and_holds(now),
            self.check_object_retention(now),
        ) {
            (
                Err(ObjectError::ObjectRetentionNotMet(_)),
                Err(ObjectError::ObjectRetentionNotMet(_)),
            ) => Err(self.retained_until(
                self.earliest_deletion_time()
                    .unwrap_or_else(|| UnixNanos::from(now)),
            )),
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    /// Holds and the bucket policy also forbid making a live object noncurrent.
    pub(crate) fn check_policy_and_holds(&self, now: SystemTime) -> Result<(), ObjectError> {
        if self.has_hold() {
            return Err(ObjectError::ActiveHold(Box::new(HeldObject {
                bucket: self.bucket.clone(),
                name: self.name.clone(),
                kind: if self.temporary_hold == Some(true) {
                    HoldKind::Temporary
                } else {
                    HoldKind::EventBased
                },
            })));
        }
        if self
            .retention_expiration_time
            .is_some_and(|deadline| now < deadline)
        {
            return Err(self.retained_until(UnixNanos::from(
                self.retention_expiration_time.unwrap_or(now),
            )));
        }
        Ok(())
    }

    /// Bucket policy and object retention share one denial; the time keeps the stored
    /// precision (microseconds for a policy, nanoseconds for object retention).
    fn retained_until(&self, until: UnixNanos) -> ObjectError {
        ObjectError::ObjectRetentionNotMet(Box::new(RetainedObject {
            bucket: self.bucket.clone(),
            name: self.name.clone(),
            until,
        }))
    }

    /// The object-lock guide allows a retained live version to become noncurrent, so
    /// callers apply this only when the generation would be deleted or replaced.
    pub(crate) fn check_object_retention(&self, now: SystemTime) -> Result<(), ObjectError> {
        if self
            .retention
            .is_some_and(|retention| retention.active(now))
        {
            return Err(self.retained_until(
                self.retention
                    .map_or_else(|| UnixNanos::from(now), |retention| retention.retain_until),
            ));
        }
        Ok(())
    }

    /// The earliest deletion time across the bucket policy and the object's own retention.
    pub(crate) fn earliest_deletion_time(&self) -> Option<UnixNanos> {
        self.retention_expiration_time
            .map(UnixNanos::from)
            .into_iter()
            .chain(self.retention.map(|retention| retention.retain_until))
            .max()
    }
}

/// A custom time as Cloud Storage prints it in the decrease refusal: UTC with a `+00:00` offset
/// and microsecond fraction only when one is present.
fn custom_time_display((seconds, nanos): (i64, u32)) -> String {
    let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds.unsigned_abs());
    let rendered = crate::json::timestamp(time).unwrap_or_default();
    let fraction = if nanos == 0 {
        String::new()
    } else {
        format!(".{:06}", nanos / 1000)
    };
    format!("{}{fraction}+00:00", rendered.get(..19).unwrap_or_default())
}

/// Reuse the strict civil-date parser while preserving nanosecond precision on the wire.
pub(crate) fn custom_time_key(value: &str) -> Result<(i64, u32), ObjectError> {
    let invalid = || {
        ObjectError::InvalidFixedMetadata(
            "Parse Error: Invalid value for type.googleapis.com/google.protobuf.Timestamp field: 'Field 'customTime', Illegal timestamp format; timestamps must end with 'Z' or have a valid timezone offset.'.".into(),
        )
    };
    if !value.is_ascii() || value.len() < 20 || value.starts_with("0000") {
        return Err(invalid());
    }
    let normalized = value.replace('t', "T").replace('z', "Z");
    let (seconds, nanos) = if normalized.as_bytes().get(19) == Some(&b'.') {
        let end = normalized[20..]
            .find(|c: char| !c.is_ascii_digit())
            .map(|i| i + 20)
            .ok_or_else(invalid)?;
        let fraction = &normalized[20..end];
        if fraction.is_empty() || fraction.len() > 9 {
            return Err(invalid());
        }
        let nanos =
            fraction.parse::<u32>().map_err(|_| invalid())? * 10u32.pow(9 - fraction.len() as u32);
        (
            format!("{}{}", &normalized[..19], &normalized[end..]),
            nanos,
        )
    } else {
        (normalized, 0)
    };
    let micros = crate::import::codec::parse_timestamp(&seconds).map_err(|_| invalid())?;
    Ok((micros / 1_000_000, nanos))
}

/// Content fields reset on PUT; holds only change when explicitly supplied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectMetadataUpdate {
    pub(crate) acl: Option<Vec<crate::acl::AclEntry>>,
    /// Replaces the bucket's default object ACL for a newly published generation. Only
    /// publication reads it; the JSON API accepts it on uploads alone.
    pub(crate) predefined_acl: Option<crate::acl::PredefinedObjectAcl>,
    pub replace: bool,
    pub content_type: Option<Option<String>>,
    pub content_encoding: Option<Option<String>>,
    pub content_disposition: Option<Option<String>>,
    pub content_language: Option<Option<String>>,
    pub cache_control: Option<Option<String>>,
    pub metadata: Option<Option<BTreeMap<String, Option<String>>>>,
    pub event_based_hold: Option<bool>,
    pub temporary_hold: Option<bool>,
    pub custom_time: Option<Option<String>>,
    /// `Some(None)` removes the configuration; transitions are checked by the catalog.
    pub(crate) retention: Option<Option<retention::ObjectRetention>>,
    pub(crate) override_unlocked_retention: retention::UnlockedOverride,
    /// Set only by data writes that carry a customer-supplied key; metadata patches keep it.
    pub customer_key_sha256: Option<[u8; 32]>,
    /// Requested class for a new generation; creation resolves it against the bucket's
    /// default and Autoclass. Metadata updates never change the class (rewrite does).
    pub storage_class: Option<StorageClass>,
}

pub(crate) fn validate_object_storage_class(class: &str) -> Result<StorageClass, ObjectError> {
    crate::bucket::validate_storage_class(class).map_err(|error| match error {
        crate::bucket::CatalogError::UnsupportedMetadata(reason) => {
            ObjectError::UnsupportedMetadata(format!("storageClass={class}: {reason}"))
        }
        _ => ObjectError::InvalidFixedMetadata(
            "Invalid object storageClass; use STANDARD, NEARLINE, COLDLINE or ARCHIVE".into(),
        ),
    })
}

impl ObjectMetadataUpdate {
    pub(crate) fn validate(&self) -> Result<(), ObjectError> {
        if let Some(Some(value)) = &self.custom_time {
            custom_time_key(value)?;
        }
        for (field, value) in [
            ("contentType", &self.content_type),
            ("contentEncoding", &self.content_encoding),
            ("contentDisposition", &self.content_disposition),
            ("contentLanguage", &self.content_language),
            ("cacheControl", &self.cache_control),
        ] {
            if let Some(Some(value)) = value {
                validate_fixed_metadata(field, value)?;
            }
        }
        if self
            .metadata
            .as_ref()
            .and_then(Option::as_ref)
            .is_some_and(|entries| entries.keys().any(String::is_empty))
        {
            return Err(ObjectError::UnsupportedMetadata(
                "empty custom metadata key".into(),
            ));
        }
        Ok(())
    }

    /// Refuses a retain-until time the request itself sets that has already passed. Only a
    /// newly requested value is checked: an object whose retention has expired stays valid and
    /// must remain restorable and movable.
    pub(crate) fn reject_past_retention(&self, now: SystemTime) -> Result<(), ObjectError> {
        if let Some(Some(retention)) = &self.retention
            && retention.retain_until <= UnixNanos::from(now)
        {
            return Err(ObjectError::InvalidFixedMetadata(
                "Retain until time cannot be in the past.".into(),
            ));
        }
        Ok(())
    }

    /// Applies the update to a published generation, where a PUT also clears the holds it omits.
    pub(crate) fn apply_to_existing(self, record: &mut ObjectRecord) -> Result<(), ObjectError> {
        if self.replace {
            record.clear_holds();
        }
        self.apply(record)
    }

    pub(crate) fn apply(self, record: &mut ObjectRecord) -> Result<(), ObjectError> {
        self.validate()?;
        // Cloud Storage kept the current custom time for `null` and for a PUT that omits it
        // (both recorded as 200); only a time can change it, and never backwards.
        if let Some(Some(value)) = self.custom_time {
            let next = custom_time_key(&value)?;
            if let Some(previous) = &record.custom_time {
                let previous = custom_time_key(previous)?;
                if next < previous {
                    return Err(ObjectError::InvalidFixedMetadata(format!(
                        "Custom time cannot be decreased. Previously: {}. Attempting to set: {}.",
                        custom_time_display(previous),
                        custom_time_display(next)
                    )));
                }
            }
            record.custom_time = Some(value);
        }
        if let Some(hold) = self.event_based_hold {
            record.event_based_hold = Some(hold);
        }
        if let Some(hold) = self.temporary_hold {
            record.temporary_hold = Some(hold);
        }
        if let Some(retention) = self.retention {
            record.retention = retention;
        }
        if let Some(sha256) = self.customer_key_sha256 {
            record.customer_key_sha256 = Some(sha256);
        }
        if self.replace {
            record.content_type.clear();
            record.content_encoding = None;
            record.content_disposition = None;
            record.content_language = None;
            record.cache_control = None;
            record.metadata.clear();
        }
        if let Some(value) = self.content_type {
            if let Some(value) = value {
                if !value.is_empty() {
                    record.content_type = value;
                }
            } else {
                record.content_type.clear();
            }
        }
        for (field, value) in [
            (&mut record.content_encoding, self.content_encoding),
            (&mut record.content_disposition, self.content_disposition),
            (&mut record.content_language, self.content_language),
            (&mut record.cache_control, self.cache_control),
        ] {
            if let Some(value) = value {
                *field = value.filter(|value| !value.is_empty());
            }
        }
        match self.metadata {
            Some(Some(entries)) => {
                for (key, value) in entries {
                    if let Some(value) = value {
                        record.metadata.insert(key, value);
                    } else {
                        record.metadata.remove(&key);
                    }
                }
            }
            Some(None) => record.metadata.clear(),
            None => {}
        }
        if record
            .metadata
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 8192
        {
            return Err(ObjectError::InvalidMetadata);
        }
        for (field, value) in [
            ("contentType", Some(record.content_type.as_str())),
            ("contentEncoding", record.content_encoding.as_deref()),
            ("contentDisposition", record.content_disposition.as_deref()),
            ("contentLanguage", record.content_language.as_deref()),
            ("cacheControl", record.cache_control.as_deref()),
        ] {
            if let Some(value) = value {
                validate_fixed_metadata(field, value)?;
            }
        }
        Ok(())
    }
}

/// The object version a catalog call addresses: optional for live selection, concrete
/// for soft-deleted versions, appendable takeovers and newly assigned publications.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObjectTarget<G = Option<i64>> {
    pub(crate) bucket: String,
    pub(crate) name: String,
    pub(crate) generation: G,
}

impl<G> ObjectTarget<G> {
    pub(crate) fn new(bucket: impl Into<String>, name: impl Into<String>, generation: G) -> Self {
        Self {
            bucket: bucket.into(),
            name: name.into(),
            generation,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ObjectConditions {
    pub if_generation_match: Option<i64>,
    pub if_generation_not_match: Option<i64>,
    pub if_metageneration_match: Option<i64>,
    pub if_metageneration_not_match: Option<i64>,
}

/// Client-declared hashes validated after the body has been streamed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExpectedHashes {
    pub md5: Option<[u8; 16]>,
    pub crc32c: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposeSource {
    pub name: String,
    pub generation: Option<i64>,
    pub if_generation_match: Option<i64>,
}

/// The wording Cloud Storage used when it refused to shorten or remove an Unlocked retention
/// without the override (observed 2026-10-31, identical for both, over JSON and
/// gRPC); locking without the override was seen with it too.
pub(crate) const UNLOCKED_RETENTION_OVERRIDE_MESSAGE: &str = "The unlocked object retention cannot be removed and its retention period cannot be shortened without overriding unlocked retention intent and permission.";

/// The wording Cloud Storage used when it refused to change a Locked retention.
pub(crate) const LOCKED_RETENTION_MESSAGE: &str = "The locked object retention cannot be removed. Its retention mode cannot be changed and its retention period cannot be shortened.";

/// The kind of hold that denied a delete or overwrite; Cloud Storage names it in the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoldKind {
    Temporary,
    EventBased,
}

/// The held object named by a denied delete or overwrite. Boxed to keep [`ObjectError`] small.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HeldObject {
    pub bucket: String,
    pub name: String,
    pub kind: HoldKind,
}

/// The retained object named by a denied delete or overwrite. Boxed to keep [`ObjectError`] small.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RetainedObject {
    pub bucket: String,
    pub name: String,
    pub until: UnixNanos,
}

/// Which stored hash an upload's declared hash disagreed with, and both digests.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HashMismatch {
    pub algorithm: HashAlgorithm,
    pub provided: Vec<u8>,
    pub calculated: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HashAlgorithm {
    Crc32c,
    Md5,
}

/// A compose component that could not be used. Cloud Storage names the component in both
/// failures, which the generic missing-object and precondition errors cannot.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ComponentFailure {
    pub bucket: String,
    pub name: String,
    pub kind: ComponentFailureKind,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ComponentFailureKind {
    NotFound,
    /// The component's `ifGenerationMatch` named a generation other than the live one.
    GenerationMismatch {
        required: i64,
        actual: i64,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ObjectError {
    BucketNotFound,
    ObjectNotFound,
    PreconditionFailed,
    /// A compose component was missing or failed its generation precondition.
    Component(Box<ComponentFailure>),
    PublicAccessPrevention,
    /// Publication would replace a live object, but the caller may not delete it.
    OverwriteForbidden,
    ActiveHold(Box<HeldObject>),
    /// The bucket retention policy or the object's own retain-until time has not passed.
    ObjectRetentionNotMet(Box<RetainedObject>),
    /// An Unlocked retention was shortened or removed without `overrideUnlockedRetention`.
    UnlockedRetentionOverrideRequired,
    /// A Locked retention was shortened, removed or unlocked; the override never applies.
    LockedRetention,
    ObjectNotSoftDeleted,
    /// The bucket's current owning project is not one the caller was authorized for.
    Forbidden,
    NotModified,
    ChecksumMismatch,
    /// The streamed body disagreed with a declared hash; the same failure as
    /// [`Self::ChecksumMismatch`], with both digests for the JSON message.
    HashMismatch(Box<HashMismatch>),
    TooLarge,
    InvalidMetadata,
    InvalidFixedMetadata(String),
    InvalidDerivedArgument(String),
    DerivedTransferNotFound,
    DerivedTransferExpired,
    DerivedTransferBusy,
    DerivedTransferIncomplete,
    UnsupportedMetadata(String),
    /// A customer-supplied key is invalid, missing or does not match the object.
    CustomerEncryption(crate::csek::CsekError),
    /// The request body ended early or failed while streaming.
    BodyAborted(BackendCause),
    Io(IoCause),
    Corrupt(BackendCause),
    Unavailable(BackendCause),
}

impl fmt::Display for ObjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BucketNotFound => formatter.write_str("bucket not found"),
            Self::ObjectNotFound => formatter.write_str("object not found"),
            Self::PreconditionFailed => formatter.write_str("precondition failed"),
            Self::Component(_) => formatter.write_str("compose component unusable"),
            Self::PublicAccessPrevention => {
                formatter.write_str("public access prevention is enforced")
            }
            Self::OverwriteForbidden => formatter.write_str("overwrite forbidden"),
            Self::ActiveHold(_) => formatter.write_str("object under active hold"),
            Self::ObjectRetentionNotMet(_) => {
                formatter.write_str("object retention period has not elapsed")
            }
            Self::LockedRetention => formatter.write_str(LOCKED_RETENTION_MESSAGE),
            Self::UnlockedRetentionOverrideRequired => {
                formatter.write_str(UNLOCKED_RETENTION_OVERRIDE_MESSAGE)
            }
            Self::ObjectNotSoftDeleted => formatter.write_str(
                "object is not soft deleted; select a generation from softDeleted=true listing",
            ),
            Self::Forbidden => formatter.write_str("forbidden"),
            Self::NotModified => formatter.write_str("not modified"),
            Self::ChecksumMismatch | Self::HashMismatch(_) => {
                formatter.write_str("checksum mismatch")
            }
            Self::TooLarge => formatter.write_str("object too large"),
            Self::InvalidMetadata => formatter.write_str("custom metadata exceeds 8 KiB"),
            Self::InvalidFixedMetadata(reason) => formatter.write_str(reason),
            Self::InvalidDerivedArgument(reason) => formatter.write_str(reason),
            Self::DerivedTransferNotFound => formatter.write_str("derived transfer not found"),
            Self::DerivedTransferExpired => formatter.write_str("derived transfer expired"),
            Self::DerivedTransferBusy => formatter.write_str("derived transfer is already in use"),
            Self::DerivedTransferIncomplete => {
                formatter.write_str("derived transfer is incomplete")
            }
            Self::UnsupportedMetadata(field) => {
                write!(formatter, "unsupported metadata change: {field}")
            }
            Self::CustomerEncryption(error) => error.fmt(formatter),
            Self::BodyAborted(reason) => write!(formatter, "request body aborted: {reason}"),
            Self::Io(kind) => write!(formatter, "object I/O: {kind}"),
            Self::Corrupt(reason) => write!(formatter, "object storage corrupt: {reason}"),
            Self::Unavailable(reason) => write!(formatter, "catalog unavailable: {reason}"),
        }
    }
}

impl std::error::Error for ObjectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(cause) => cause.source(),
            Self::BodyAborted(cause) | Self::Corrupt(cause) | Self::Unavailable(cause) => {
                cause.source()
            }
            _ => None,
        }
    }
}

impl ObjectError {
    /// Wraps an adapter-side failure as unavailable, keeping it as the source.
    pub(crate) fn backend(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Unavailable(BackendCause::from_error(error))
    }

    /// The request body stream failed; the transport error stays the source.
    pub(crate) fn body_aborted(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::BodyAborted(BackendCause::from_error(error))
    }

    pub(crate) fn is_backend_failure(&self) -> bool {
        matches!(self, Self::Unavailable(_) | Self::Io(_) | Self::Corrupt(_))
    }
}

impl From<crate::bucket::CatalogError> for ObjectError {
    fn from(error: crate::bucket::CatalogError) -> Self {
        match error {
            crate::bucket::CatalogError::Io(kind) => Self::Io(kind),
            crate::bucket::CatalogError::Corrupt(reason) => Self::Corrupt(reason),
            error => Self::Unavailable(BackendCause::from_error(error)),
        }
    }
}

pub(crate) fn validate_fixed_metadata(key: &str, value: &str) -> Result<(), ObjectError> {
    if key == "contentLanguage" {
        if value.is_empty() {
            return Err(ObjectError::InvalidFixedMetadata(
                "contentLanguage must not be empty; use null to clear it".into(),
            ));
        }
        if value.chars().count() > 100 {
            return Err(ObjectError::InvalidFixedMetadata(
                "contentLanguage exceeds 100 characters".into(),
            ));
        }
    }
    if !value.is_ascii() {
        return Err(ObjectError::UnsupportedMetadata(
            "non-ASCII fixed metadata".into(),
        ));
    }
    // Fixed metadata becomes response headers; reject controls in every storage caller.
    if value.bytes().any(|byte| byte < 32 || byte == 127) {
        return Err(ObjectError::InvalidFixedMetadata(format!(
            "Invalid {key} header value"
        )));
    }
    Ok(())
}
