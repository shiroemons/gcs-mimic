//! Transport-independent object state shared by the catalog and protocol adapters.

use crate::{
    bucket::StorageClass,
    storage::failure::{BackendCause, IoCause},
};
use std::{collections::BTreeMap, fmt, time::SystemTime};
use unix_nanos::UnixNanos;

pub(crate) mod name;
pub(crate) mod owner;
pub(crate) mod retention;
pub(crate) mod unix_nanos;

/// Unset holds are omitted from JSON; a released hold keeps its explicit false value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "Option<bool>", into = "Option<bool>")]
pub enum Hold {
    #[default]
    Unset,
    Released,
    Held,
}

impl Hold {
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Self::Unset => None,
            Self::Released => Some(false),
            Self::Held => Some(true),
        }
    }

    pub(crate) fn is_held(self) -> bool {
        self == Self::Held
    }
}

impl From<Option<bool>> for Hold {
    fn from(value: Option<bool>) -> Self {
        match value {
            None => Self::Unset,
            Some(false) => Self::Released,
            Some(true) => Self::Held,
        }
    }
}

impl From<Hold> for Option<bool> {
    fn from(value: Hold) -> Self {
        value.as_bool()
    }
}

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
    /// Older serialized records have no class-change timestamp.
    #[serde(default)]
    pub time_storage_class_updated: Option<SystemTime>,
    pub updated: SystemTime,
    pub time_deleted: Option<SystemTime>,
    pub soft_delete_time: Option<SystemTime>,
    pub hard_delete_time: Option<SystemTime>,
    #[serde(default)]
    pub event_based_hold: Hold,
    #[serde(default)]
    pub temporary_hold: Hold,
    pub custom_time: Option<String>,
    /// Bucket retention policy expiration only; object retention is kept in `retention`.
    pub retention_expiration_time: Option<SystemTime>,
    pub(crate) retention: Option<retention::ObjectRetention>,
    /// SHA-256 of the customer-supplied AES256 key; the key itself is never stored.
    pub customer_key_sha256: Option<[u8; 32]>,
}

impl ObjectRecord {
    pub(crate) fn storage_class_updated(&self) -> SystemTime {
        self.time_storage_class_updated.unwrap_or(self.time_created)
    }

    pub(crate) fn has_md5(&self) -> bool {
        self.md5_availability == Md5Availability::Available && self.component_count.is_none()
    }

    /// A PUT replaces the resource, so a hold it does not name returns to never set (recorded:
    /// a PUT of only `metadata` under a temporary hold answered without the hold, not `false`).
    pub(crate) fn clear_holds(&mut self) {
        self.event_based_hold = Hold::Unset;
        self.temporary_hold = Hold::Unset;
    }

    pub(crate) fn has_hold(&self) -> bool {
        self.event_based_hold.is_held() || self.temporary_hold.is_held()
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
                kind: if self.temporary_hold.is_held() {
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
    let rendered = crate::clock::system_time_from_unix_seconds(seconds)
        .and_then(|time| crate::json::timestamp(time).ok())
        .unwrap_or_default();
    let fraction = if nanos == 0 {
        String::new()
    } else {
        format!(".{:06}", nanos / 1000)
    };
    format!("{}{fraction}+00:00", rendered.get(..19).unwrap_or_default())
}

#[cfg(test)]
mod custom_time_tests {
    use super::*;

    #[test]
    fn custom_time_errors_preserve_dates_before_the_epoch() {
        for value in [
            "1960-01-01T00:00:00Z",
            "1969-12-31T23:59:59Z",
            "1980-01-01T00:00:00Z",
        ] {
            assert_eq!(
                custom_time_display(custom_time_key(value).unwrap()),
                value.replace('Z', "+00:00")
            );
        }
        assert_eq!(
            custom_time_display((-1, 123_456_000)),
            "1969-12-31T23:59:59.123456+00:00"
        );
    }
}

/// Reuse the strict civil-date parser while preserving nanosecond precision on the wire.
pub(crate) fn custom_time_key(value: &str) -> Result<(i64, u32), ObjectError> {
    let invalid = || ObjectError::Metadata(crate::metadata_error::MetadataError::CustomTime);
    if !value.is_ascii() || value.len() < 20 || value.starts_with("0000") {
        return Err(invalid());
    }
    let normalized = value.replace('t', "T").replace('z', "Z");
    let (seconds, nanos) = if normalized.as_bytes().get(19) == Some(&b'.') {
        let remainder = normalized.get(20..).ok_or_else(invalid)?;
        let end = remainder
            .find(|c: char| !c.is_ascii_digit())
            .ok_or_else(invalid)?;
        let (fraction, zone) = remainder.split_at_checked(end).ok_or_else(invalid)?;
        if fraction.is_empty() || fraction.len() > 9 {
            return Err(invalid());
        }
        let nanos = fraction.parse::<u32>().map_err(|_| invalid())?
            * 10u32.pow(9 - u32::try_from(fraction.len()).map_err(|_| invalid())?);
        (
            format!("{}{}", normalized.get(..19).ok_or_else(invalid)?, zone),
            nanos,
        )
    } else {
        (normalized, 0)
    };
    let micros = crate::import::codec::parse_timestamp(&seconds).map_err(|_| invalid())?;
    Ok((micros / 1_000_000, nanos))
}

/// How a present custom-metadata or label map changes its stored entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MapMode {
    #[default]
    Merge,
    Replace,
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
    pub(crate) metadata_mode: MapMode,
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
        crate::bucket::CatalogError::Metadata(error) if error.unsupported() => {
            ObjectError::Metadata(
                crate::metadata_error::MetadataError::UnsupportedObjectStorageClass(class.into()),
            )
        }
        _ => ObjectError::Metadata(crate::metadata_error::MetadataError::ObjectStorageClass),
    })
}

/// Text and maps always serialize; a failure counts as too large rather than as fitting.
fn encoded_json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |encoded| encoded.len())
}

/// What an object's fixed and custom metadata may take once encoded as JSON.
///
/// A session's options, a stored completion and a stored rewrite response keep a copy in columns
/// checked at 64 KiB, next to the object name in several escaped forms and a `fields` selection
/// of up to 16 KiB. Refusing here keeps an oversized value from failing those rows' constraints as
/// a retryable server error after the client has sent its data. Custom metadata stays well inside
/// this at its documented 8 KiB, unless nearly every byte needs a six-byte JSON escape, and fixed
/// metadata has no documented size but becomes response headers, whose documented XML API total
/// is 16 KiB.
const ENCODED_METADATA_LIMIT: usize = 40 * 1024;

impl ObjectMetadataUpdate {
    /// Options store the content type both as a session field and inside the metadata.
    fn encoded_size(&self) -> usize {
        let text = |value: &Option<Option<String>>| {
            value
                .as_ref()
                .and_then(Option::as_ref)
                .map_or(0, encoded_json_len)
        };
        [
            text(&self.content_type),
            text(&self.content_type),
            text(&self.content_encoding),
            text(&self.content_disposition),
            text(&self.content_language),
            text(&self.cache_control),
            self.metadata
                .as_ref()
                .and_then(Option::as_ref)
                .map_or(0, encoded_json_len),
        ]
        .into_iter()
        .fold(0, usize::saturating_add)
    }

    /// Fails an update too large for the rows that keep a copy of it.
    pub(crate) fn check_encoded_size(&self) -> Result<(), ObjectError> {
        if self.encoded_size() > ENCODED_METADATA_LIMIT {
            return Err(ObjectError::InvalidFixedMetadata(format!(
                "Object metadata exceeds {} KiB once encoded as JSON; shorten its fixed and custom metadata.",
                ENCODED_METADATA_LIMIT / 1024
            )));
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), ObjectError> {
        self.check_encoded_size()?;
        if self.metadata_mode == MapMode::Replace
            && self
                .metadata
                .as_ref()
                .and_then(Option::as_ref)
                .is_some_and(|entries| entries.values().any(Option::is_none))
        {
            return Err(ObjectError::InvalidFixedMetadata(
                "Custom metadata replacement cannot remove individual keys".into(),
            ));
        }
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
    /// Edits of one field at a time may not add up past what a later copy could inherit.
    pub(crate) fn apply_to_existing(self, record: &mut ObjectRecord) -> Result<(), ObjectError> {
        if self.replace {
            record.clear_holds();
        }
        self.apply(record)?;
        Self::inheriting(record).check_encoded_size()
    }

    /// The update that gives a new object the editable metadata of `source`, except its holds.
    pub(crate) fn inheriting(source: &ObjectRecord) -> Self {
        Self {
            content_type: Some(Some(source.content_type.clone())),
            content_encoding: Some(source.content_encoding.clone()),
            content_disposition: Some(source.content_disposition.clone()),
            content_language: Some(source.content_language.clone()),
            cache_control: Some(source.cache_control.clone()),
            metadata: Some(Some(
                source
                    .metadata
                    .iter()
                    .map(|(key, value)| (key.clone(), Some(value.clone())))
                    .collect(),
            )),
            ..Default::default()
        }
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
            record.event_based_hold = Hold::from(Some(hold));
        }
        if let Some(hold) = self.temporary_hold {
            record.temporary_hold = Hold::from(Some(hold));
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
        if self.metadata_mode == MapMode::Replace && self.metadata.is_some() {
            record.metadata.clear();
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

/// Whether opening an object should also fetch the start of its body. A plain download sends
/// those bytes anyway; a ranged, conditional or HEAD request often does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadAhead {
    None,
    Body,
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
    OverwriteForbidden(Box<crate::auth::Denied>),
    ActiveHold(Box<HeldObject>),
    /// The bucket retention policy or the object's own retain-until time has not passed.
    ObjectRetentionNotMet(Box<RetainedObject>),
    /// An Unlocked retention was shortened or removed without `overrideUnlockedRetention`.
    UnlockedRetentionOverrideRequired,
    /// A Locked retention was shortened, removed or unlocked; the override never applies.
    LockedRetention,
    ObjectNotSoftDeleted,
    /// Strict authorization refused a Cloud Storage permission, including when the bucket's
    /// current owning project is not one the caller was authorized for.
    Denied(Box<crate::auth::Denied>),
    /// Refused for a reason no Cloud Storage permission names: an upload session whose bucket
    /// was recreated, administrative access through upload replay, or a refused sync control.
    Forbidden,
    NotModified,
    ChecksumMismatch,
    /// The streamed body disagreed with a declared hash; the same failure as
    /// [`Self::ChecksumMismatch`], with both digests for the JSON message.
    HashMismatch(Box<HashMismatch>),
    TooLarge,
    InvalidMetadata,
    InvalidFixedMetadata(String),
    /// A predefined ACL reached publication on a bucket with uniform access enabled. Typed
    /// because each protocol words this refusal differently.
    UniformAccessPredefinedAcl,
    Metadata(crate::metadata_error::MetadataError),
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
            Self::OverwriteForbidden(denied) => write!(formatter, "overwrite forbidden: {denied}"),
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
            Self::Denied(denied) => denied.fmt(formatter),
            Self::Forbidden => formatter.write_str("forbidden"),
            Self::NotModified => formatter.write_str("not modified"),
            Self::ChecksumMismatch | Self::HashMismatch(_) => {
                formatter.write_str("checksum mismatch")
            }
            Self::TooLarge => formatter.write_str("object too large"),
            Self::InvalidMetadata => formatter.write_str("custom metadata exceeds 8 KiB"),
            Self::InvalidFixedMetadata(reason) => formatter.write_str(reason),
            Self::UniformAccessPredefinedAcl => {
                formatter.write_str(crate::acl::UNIFORM_ACCESS_UPLOAD_ACL)
            }
            Self::Metadata(error) => error.fmt(formatter),
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
            Self::Metadata(error) => Some(error),
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

    /// An authorization refusal of the access itself, whether or not it names a Cloud Storage
    /// permission. A caller that turns a refusal into a hidden result tests this, so it cannot
    /// handle one kind and let the other escape as an error. [`Self::OverwriteForbidden`] is
    /// not one: it refuses only the replacement a publication would make.
    pub(crate) fn is_refusal(&self) -> bool {
        matches!(self, Self::Denied(_) | Self::Forbidden)
    }
}

impl From<crate::bucket::CatalogError> for ObjectError {
    fn from(error: crate::bucket::CatalogError) -> Self {
        match error {
            crate::bucket::CatalogError::Denied(denied) => Self::Denied(denied),
            crate::bucket::CatalogError::Forbidden => Self::Forbidden,
            crate::bucket::CatalogError::PublicAccessPrevention => Self::PublicAccessPrevention,
            crate::bucket::CatalogError::Io(kind) => Self::Io(kind),
            crate::bucket::CatalogError::Corrupt(reason) => Self::Corrupt(reason),
            error => Self::Unavailable(BackendCause::from_error(error)),
        }
    }
}

pub(crate) fn validate_fixed_metadata(key: &str, value: &str) -> Result<(), ObjectError> {
    if key == "contentLanguage" {
        if value.is_empty() {
            return Err(ObjectError::Metadata(
                crate::metadata_error::MetadataError::EmptyLanguage,
            ));
        }
        if value.chars().count() > 100 {
            return Err(ObjectError::Metadata(
                crate::metadata_error::MetadataError::LongLanguage,
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
        return Err(ObjectError::Metadata(
            crate::metadata_error::MetadataError::FixedHeader(key.into()),
        ));
    }
    Ok(())
}
