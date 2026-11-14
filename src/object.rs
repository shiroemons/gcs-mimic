//! Transport-independent object state shared by the catalog and protocol adapters.

use crate::{
    bucket::StorageClass,
    storage::failure::{BackendCause, IoCause, Link, TypedSource},
};
use std::{collections::BTreeMap, fmt, time::SystemTime};
use unix_nanos::UnixNanos;

pub(crate) mod generation;
pub(crate) mod name;
pub(crate) mod owner;
mod patch;
pub(crate) mod retention;
pub(crate) mod unix_nanos;

pub use patch::{MapPatch, Patch, WriteMode};

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

    /// The stored fixed metadata fields with their JSON names, in the order updates report them.
    fn fixed_fields(&self) -> [(&'static str, Option<&str>); 5] {
        [
            ("contentType", Some(self.content_type.as_str())),
            ("contentEncoding", self.content_encoding.as_deref()),
            ("contentDisposition", self.content_disposition.as_deref()),
            ("contentLanguage", self.content_language.as_deref()),
            ("cacheControl", self.cache_control.as_deref()),
        ]
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
fn custom_time_display(time: UnixNanos) -> String {
    let rendered = crate::json::timestamp_nanos(time).unwrap_or_default();
    let fraction = if time.subsec_nanos() == 0 {
        String::new()
    } else {
        format!(".{:06}", time.subsec_nanos() / 1000)
    };
    format!("{}{fraction}+00:00", rendered.get(..19).unwrap_or_default())
}

#[cfg(test)]
mod custom_time_tests {
    use super::*;

    #[test]
    fn custom_time_errors_preserve_dates_before_the_epoch() {
        for value in [
            "0001-01-01T00:00:00Z",
            "1600-02-29T23:59:59Z",
            "1960-01-01T00:00:00Z",
            "1969-12-31T23:59:59Z",
            "1980-01-01T00:00:00Z",
            "9999-12-31T00:00:00Z",
        ] {
            assert_eq!(
                custom_time_display(custom_time_key(value).unwrap()),
                value.replace('Z', "+00:00")
            );
        }
        assert_eq!(
            custom_time_display(UnixNanos::new(-1, 123_456_000).unwrap()),
            "1969-12-31T23:59:59.123456+00:00"
        );
        assert_eq!(
            custom_time_display(custom_time_key("0001-01-01T00:00:00.123456789Z").unwrap()),
            "0001-01-01T00:00:00.123456+00:00"
        );
    }
}

/// Reuse the strict civil-date parser while preserving nanosecond precision on the wire.
pub(crate) fn custom_time_key(value: &str) -> Result<UnixNanos, ObjectError> {
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
    UnixNanos::new(micros / 1_000_000, nanos).ok_or_else(invalid)
}

/// Content fields reset on PUT; holds only change when explicitly supplied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectMetadataUpdate {
    pub(crate) acl: Option<Vec<crate::acl::AclEntry>>,
    /// Replaces the bucket's default object ACL for a newly published generation. Only
    /// publication reads it; the JSON API accepts it on uploads alone.
    pub(crate) predefined_acl: Option<crate::acl::PredefinedObjectAcl>,
    /// A PUT resets the content fields, custom metadata and holds it does not name.
    pub write: WriteMode,
    pub content_type: Patch<String>,
    pub content_encoding: Patch<String>,
    pub content_disposition: Patch<String>,
    pub content_language: Patch<String>,
    pub cache_control: Patch<String>,
    pub metadata: MapPatch,
    pub event_based_hold: Option<bool>,
    pub temporary_hold: Option<bool>,
    /// Only a time changes the stored custom time; `Clear` keeps it (see `apply_with`).
    pub custom_time: Patch<String>,
    /// `Clear` removes the configuration; transitions are checked by the catalog.
    pub(crate) retention: Patch<retention::ObjectRetention>,
    pub(crate) override_unlocked_retention: retention::UnlockedOverride,
    /// Set only by data writes that carry a customer-supplied key; metadata patches keep it.
    pub customer_key_sha256: Option<[u8; 32]>,
    /// Requested class for a new generation; creation resolves it against the bucket's
    /// default and Autoclass. Metadata updates never change the class (rewrite does).
    pub storage_class: Option<StorageClass>,
}

pub(crate) fn validate_object_storage_class(class: &str) -> Result<StorageClass, ObjectError> {
    use crate::{bucket::InvalidStorageClass, metadata_error::MetadataError};
    StorageClass::from_name(class).map_err(|error| {
        ObjectError::Metadata(match error {
            InvalidStorageClass::RequiresLocation => {
                MetadataError::UnsupportedObjectStorageClass(class.into())
            }
            InvalidStorageClass::Unknown => MetadataError::ObjectStorageClass,
        })
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

/// Cloud Storage's limit on the summed bytes of custom metadata keys and values.
pub(crate) const CUSTOM_METADATA_LIMIT: usize = 8192;

/// Which entries of a custom metadata change count toward [`CUSTOM_METADATA_LIMIT`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemovedKeys {
    /// A removal stores nothing and counts nothing.
    Ignored,
    /// A removal counts its key, as the resumable completion check always has.
    Counted,
}

/// Whether the entries of a custom metadata change pass [`CUSTOM_METADATA_LIMIT`]. Each entry
/// counts its key and value bytes; `removed` says whether a removal (`None`) counts its key.
pub(crate) fn custom_metadata_exceeds(
    entries: &BTreeMap<String, Option<String>>,
    removed: RemovedKeys,
) -> bool {
    entries
        .iter()
        .filter_map(|(key, value)| match (value, removed) {
            (Some(value), RemovedKeys::Ignored | RemovedKeys::Counted) => {
                Some(key.len() + value.len())
            }
            (None, RemovedKeys::Counted) => Some(key.len()),
            (None, RemovedKeys::Ignored) => None,
        })
        .sum::<usize>()
        > CUSTOM_METADATA_LIMIT
}

#[cfg(test)]
mod custom_metadata_limit_tests {
    use super::*;

    #[test]
    fn the_limit_counts_keys_and_values_and_removals_only_when_asked() {
        let entries = |pairs: &[(&str, Option<usize>)]| {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.map(|len| "v".repeat(len))))
                .collect::<BTreeMap<_, _>>()
        };
        for removed in [RemovedKeys::Ignored, RemovedKeys::Counted] {
            // 8192 bytes exactly are allowed; one more byte is not.
            assert!(!custom_metadata_exceeds(
                &entries(&[("k", Some(8191))]),
                removed
            ));
            assert!(custom_metadata_exceeds(
                &entries(&[("k", Some(8192))]),
                removed
            ));
            assert!(!custom_metadata_exceeds(
                &entries(&[("a", Some(4095)), ("b", Some(4095))]),
                removed
            ));
            assert!(custom_metadata_exceeds(
                &entries(&[("a", Some(4095)), ("bb", Some(4095))]),
                removed
            ));
        }
        // A removal adds only its key, and only where removals count.
        let with_removal = entries(&[("k", Some(8188)), ("gone", None)]);
        assert!(!custom_metadata_exceeds(
            &with_removal,
            RemovedKeys::Ignored
        ));
        assert!(custom_metadata_exceeds(&with_removal, RemovedKeys::Counted));
        let at_limit = entries(&[("k", Some(8188)), ("one", None)]);
        assert!(!custom_metadata_exceeds(&at_limit, RemovedKeys::Counted));
    }
}

impl ObjectMetadataUpdate {
    /// The fixed metadata fields with their JSON names, in the order errors are reported.
    fn fixed_fields(&self) -> [(&'static str, &Patch<String>); 5] {
        [
            ("contentType", &self.content_type),
            ("contentEncoding", &self.content_encoding),
            ("contentDisposition", &self.content_disposition),
            ("contentLanguage", &self.content_language),
            ("cacheControl", &self.cache_control),
        ]
    }

    /// Options store the content type both as a session field and inside the metadata.
    fn encoded_size(&self) -> usize {
        let text = |patch: &Patch<String>| patch.value().map_or(0, encoded_json_len);
        let metadata = match &self.metadata {
            MapPatch::Keep | MapPatch::Clear => 0,
            MapPatch::Merge(entries) => encoded_json_len(entries),
            MapPatch::Replace(entries) => encoded_json_len(entries),
        };
        self.fixed_fields()
            .into_iter()
            .map(|(_, patch)| text(patch))
            .chain([text(&self.content_type), metadata])
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
        self.validate_for(EmptyContent::Unset)
    }

    fn validate_for(&self, empty: EmptyContent) -> Result<(), ObjectError> {
        self.check_encoded_size()?;
        if let Some(value) = self.custom_time.value() {
            custom_time_key(value)?;
        }
        for (field, patch) in self.fixed_fields() {
            if let Some(value) = patch.value()
                && !empty.is_stored(value)
            {
                validate_fixed_metadata(field, value)?;
            }
        }
        let names_empty_key = match &self.metadata {
            MapPatch::Keep | MapPatch::Clear => false,
            MapPatch::Merge(entries) => entries.contains_key(""),
            MapPatch::Replace(entries) => entries.contains_key(""),
        };
        if names_empty_key {
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
        if let Some(retention) = self.retention.value()
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
        if self.write == WriteMode::Replace {
            record.clear_holds();
        }
        self.apply_with(record, EmptyContent::Stored)?;
        Self::inheriting(record).check_encoded_size()
    }

    /// The update that gives a new object the editable metadata of `source`, except its holds.
    /// An empty content field a metadata update stored is left unset on the new generation.
    pub(crate) fn inheriting(source: &ObjectRecord) -> Self {
        let set = |value: &Option<String>| {
            Patch::nullable(value.clone().filter(|value| !value.is_empty()))
        };
        Self {
            content_type: Patch::Set(source.content_type.clone()),
            content_encoding: set(&source.content_encoding),
            content_disposition: set(&source.content_disposition),
            content_language: set(&source.content_language),
            cache_control: set(&source.cache_control),
            // A merge rather than a replacement: a rewrite session stores this update and must
            // read back an equal one.
            metadata: MapPatch::Merge(
                source
                    .metadata
                    .iter()
                    .map(|(key, value)| (key.clone(), Some(value.clone())))
                    .collect(),
            ),
            ..Default::default()
        }
    }

    pub(crate) fn apply(self, record: &mut ObjectRecord) -> Result<(), ObjectError> {
        self.apply_with(record, EmptyContent::Unset)
    }

    fn apply_with(self, record: &mut ObjectRecord, empty: EmptyContent) -> Result<(), ObjectError> {
        self.validate_for(empty)?;
        // Cloud Storage kept the current custom time for `null` and for a PUT that omits it
        // (both recorded as 200); only a time can change it, and never backwards.
        if let Patch::Set(value) = self.custom_time {
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
        match self.retention {
            Patch::Keep => {}
            Patch::Clear => record.retention = None,
            Patch::Set(retention) => record.retention = Some(retention),
        }
        if let Some(sha256) = self.customer_key_sha256 {
            record.customer_key_sha256 = Some(sha256);
        }
        if self.write == WriteMode::Replace {
            record.content_type.clear();
            record.content_encoding = None;
            record.content_disposition = None;
            record.content_language = None;
            record.cache_control = None;
            record.metadata.clear();
        }
        match self.content_type {
            Patch::Keep => {}
            Patch::Clear => record.content_type.clear(),
            // An empty type leaves the stored type (or the one a PUT cleared) as it is.
            Patch::Set(value) => {
                if !value.is_empty() {
                    record.content_type = value;
                }
            }
        }
        for (field, patch) in [
            (&mut record.content_encoding, self.content_encoding),
            (&mut record.content_disposition, self.content_disposition),
            (&mut record.content_language, self.content_language),
            (&mut record.cache_control, self.cache_control),
        ] {
            empty.apply(field, patch);
        }
        apply_custom_metadata(&mut record.metadata, self.metadata);
        if record
            .metadata
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > CUSTOM_METADATA_LIMIT
        {
            return Err(ObjectError::InvalidMetadata);
        }
        for (field, value) in record.fixed_fields() {
            if let Some(value) = value
                && !empty.is_stored(value)
            {
                validate_fixed_metadata(field, value)?;
            }
        }
        Ok(())
    }
}

/// How a metadata write treats an empty content field (`contentEncoding`,
/// `contentDisposition`, `contentLanguage` or `cacheControl`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyContent {
    /// A new generation leaves the field unset; an empty `contentLanguage` is refused.
    Unset,
    /// A metadata update of an existing generation stores the empty string: Cloud Storage
    /// returned `""` for each of these after a JSON PATCH set them so (recorded 2026-11).
    Stored,
}

impl EmptyContent {
    fn is_stored(self, value: &str) -> bool {
        self == Self::Stored && value.is_empty()
    }

    /// Stores an optional content field's patch; an empty value is unset on a new generation.
    fn apply(self, field: &mut Option<String>, patch: Patch<String>) {
        match patch {
            Patch::Keep => {}
            Patch::Clear => *field = None,
            Patch::Set(value) => {
                *field = (self.is_stored(&value) || !value.is_empty()).then_some(value);
            }
        }
    }
}

/// Applies a custom metadata change; a merge removes the keys it names without a value.
fn apply_custom_metadata(stored: &mut BTreeMap<String, String>, change: MapPatch) {
    match change {
        MapPatch::Keep => {}
        MapPatch::Clear => stored.clear(),
        MapPatch::Merge(entries) => {
            for (key, value) in entries {
                match value {
                    Some(value) => {
                        stored.insert(key, value);
                    }
                    None => {
                        stored.remove(&key);
                    }
                }
            }
        }
        MapPatch::Replace(entries) => *stored = entries,
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

/// A digest algorithm an upload can declare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HashAlgorithm {
    Crc32c,
    Md5,
}

impl HashAlgorithm {
    /// The algorithm's name as conflict messages spell it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Crc32c => "CRC32C",
            Self::Md5 => "MD5",
        }
    }
}

/// One declared digest of an object's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Checksum {
    Md5([u8; 16]),
    Crc32c(u32),
}

/// A second, different value for a digest already declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChecksumConflict(pub(crate) HashAlgorithm);

impl fmt::Display for ChecksumConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "conflicting {} declarations", self.0.name())
    }
}

impl std::error::Error for ChecksumConflict {}

impl ExpectedHashes {
    /// Whether a digest of `algorithm` is declared.
    pub(crate) fn declares(&self, algorithm: HashAlgorithm) -> bool {
        match algorithm {
            HashAlgorithm::Crc32c => self.crc32c.is_some(),
            HashAlgorithm::Md5 => self.md5.is_some(),
        }
    }

    /// Records `checksum`, replacing any earlier value of its algorithm.
    pub(crate) fn set(&mut self, checksum: Checksum) {
        match checksum {
            Checksum::Md5(digest) => self.md5 = Some(digest),
            Checksum::Crc32c(digest) => self.crc32c = Some(digest),
        }
    }

    /// Records `checksum`; an earlier value of its algorithm must be equal.
    pub(crate) fn agree(&mut self, checksum: Checksum) -> Result<(), ChecksumConflict> {
        let conflicts = match checksum {
            Checksum::Md5(digest) => self.md5.is_some_and(|declared| declared != digest),
            Checksum::Crc32c(digest) => self.crc32c.is_some_and(|declared| declared != digest),
        };
        if conflicts {
            return Err(ChecksumConflict(checksum.algorithm()));
        }
        self.set(checksum);
        Ok(())
    }
}

impl Checksum {
    pub(crate) const fn algorithm(self) -> HashAlgorithm {
        match self {
            Self::Md5(_) => HashAlgorithm::Md5,
            Self::Crc32c(_) => HashAlgorithm::Crc32c,
        }
    }
}

#[cfg(test)]
mod expected_hashes_tests {
    use super::*;

    #[test]
    fn a_declared_digest_is_replaced_by_set_and_must_agree_otherwise() {
        let mut hashes = ExpectedHashes::default();
        assert!(!hashes.declares(HashAlgorithm::Md5));
        assert!(!hashes.declares(HashAlgorithm::Crc32c));
        hashes.set(Checksum::Md5([1; 16]));
        assert!(hashes.declares(HashAlgorithm::Md5));
        assert!(!hashes.declares(HashAlgorithm::Crc32c));
        hashes.set(Checksum::Md5([2; 16]));
        assert_eq!(hashes.md5, Some([2; 16]));
        // An equal value agrees and a first value is recorded; neither touches the other.
        assert_eq!(hashes.agree(Checksum::Md5([2; 16])), Ok(()));
        assert_eq!(hashes.agree(Checksum::Crc32c(7)), Ok(()));
        assert_eq!(
            hashes,
            ExpectedHashes {
                md5: Some([2; 16]),
                crc32c: Some(7),
            }
        );
        for (different, algorithm) in [
            (Checksum::Md5([3; 16]), HashAlgorithm::Md5),
            (Checksum::Crc32c(8), HashAlgorithm::Crc32c),
        ] {
            assert_eq!(hashes.agree(different), Err(ChecksumConflict(algorithm)));
        }
        // A conflict leaves the declared values as they were.
        assert_eq!(hashes.md5, Some([2; 16]));
        assert_eq!(hashes.crc32c, Some(7));
        assert_eq!(
            ChecksumConflict(HashAlgorithm::Crc32c).to_string(),
            "conflicting CRC32C declarations"
        );
    }
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
    /// A write's `ifGenerationNotMatch` or `ifMetagenerationNotMatch` named the current value of
    /// the object it would change, delete or replace; nothing was written. Cloud Storage answers
    /// 304 over JSON (recorded 2026-11 for patch, update, delete, media upload and copy), while
    /// gRPC still reports a failed precondition.
    WriteNotModified,
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
    /// A compose destination named a storage class its components do not share.
    ComposeClassMismatch {
        requested: StorageClass,
        sources: StorageClass,
    },
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
            Self::NotModified | Self::WriteNotModified => formatter.write_str("not modified"),
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
            // Cloud Storage's wording (recorded 2026-11-13).
            Self::ComposeClassMismatch { requested, sources } => write!(
                formatter,
                "Cannot compose object with destination storage class {} from source components that have different storage classes ({}).",
                requested.as_str(),
                sources.as_str()
            ),
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
        self.typed_source().map(Link::as_error)
    }
}

impl TypedSource for ObjectError {
    fn typed_source(&self) -> Option<Link<'_>> {
        match self {
            Self::Io(cause) => cause.typed_source(),
            Self::Metadata(error) => Some(Link::Leaf(error)),
            Self::BodyAborted(cause) | Self::Corrupt(cause) | Self::Unavailable(cause) => {
                cause.typed_source()
            }
            Self::BucketNotFound
            | Self::ObjectNotFound
            | Self::PreconditionFailed
            | Self::Component(_)
            | Self::PublicAccessPrevention
            | Self::OverwriteForbidden(_)
            | Self::ActiveHold(_)
            | Self::ObjectRetentionNotMet(_)
            | Self::UnlockedRetentionOverrideRequired
            | Self::LockedRetention
            | Self::ObjectNotSoftDeleted
            | Self::Denied(_)
            | Self::Forbidden
            | Self::NotModified
            | Self::WriteNotModified
            | Self::ChecksumMismatch
            | Self::HashMismatch(_)
            | Self::TooLarge
            | Self::InvalidMetadata
            | Self::InvalidFixedMetadata(_)
            | Self::UniformAccessPredefinedAcl
            | Self::InvalidDerivedArgument(_)
            | Self::ComposeClassMismatch { .. }
            | Self::DerivedTransferNotFound
            | Self::DerivedTransferExpired
            | Self::DerivedTransferBusy
            | Self::DerivedTransferIncomplete
            | Self::UnsupportedMetadata(_)
            | Self::CustomerEncryption(_) => None,
        }
    }
}

impl ObjectError {
    /// Wraps an adapter-side failure as unavailable, keeping it as the source.
    pub(crate) fn backend(error: impl Into<crate::storage::failure::Source>) -> Self {
        Self::Unavailable(BackendCause::from_error(error))
    }

    /// The request body stream failed; the transport error stays the source.
    pub(crate) fn body_aborted(error: impl Into<crate::storage::failure::Source>) -> Self {
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
        use crate::bucket::CatalogError;
        match error {
            CatalogError::Denied(denied) => Self::Denied(denied),
            CatalogError::Forbidden => Self::Forbidden,
            CatalogError::PublicAccessPrevention => Self::PublicAccessPrevention,
            CatalogError::Io(kind) => Self::Io(kind),
            CatalogError::Corrupt(reason) => Self::Corrupt(reason),
            // The same mapping as `object_unavailable`, which keeps an unavailable cause as is.
            CatalogError::Unavailable(reason) => Self::Unavailable(reason),
            error @ (CatalogError::AuthenticationAlreadyConfigured
            | CatalogError::AlreadyExists
            | CatalogError::NotFound
            | CatalogError::NotEmpty
            | CatalogError::RestoreConflict
            | CatalogError::PreconditionFailed
            | CatalogError::RetentionPolicyLocked(_)
            | CatalogError::RetentionPolicyNotSet
            | CatalogError::InvalidMetadata(_)
            | CatalogError::Metadata(_)
            | CatalogError::UnsupportedMetadata(_)
            | CatalogError::NewerSchema { .. }) => {
                Self::Unavailable(BackendCause::from_error(error))
            }
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
