//! Transport-independent object state shared by the catalog and protocol adapters.

use crate::{
    bucket::StorageClass,
    cause::{BackendCause, IoCause, Link, TypedSource},
    hashing::Hashes,
};
use std::{collections::BTreeMap, fmt, time::SystemTime};
use unix_nanos::UnixNanos;

pub(crate) mod content_type;
mod custom_time;
pub(crate) mod generation;
mod idempotency;
pub(crate) mod media_headers;
pub(crate) mod name;
pub(crate) mod owner;
mod patch;
pub(crate) mod retention;
pub(crate) mod unix_nanos;
pub(crate) mod upload_headers;
pub(crate) use idempotency::{IdempotencyToken, OpaqueTokenError, TextTokenError};

pub use custom_time::CustomTime;
pub use generation::{Generation, InvalidGeneration, InvalidMetageneration, Metageneration};
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

    /// Whether changing this state to `next` releases a hold in force. Only this transition
    /// restarts the bucket retention period of an event-based hold: setting a hold, or releasing
    /// one that was not held, leaves the period where it was.
    pub(crate) fn released_by(self, next: Self) -> bool {
        self.is_held() && !next.is_held()
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
    pub generation: Generation,
    pub metageneration: Metageneration,
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
    /// When the event-based hold last went from held to released, at the catalog's microsecond
    /// precision. Older serialized records and generations never released have none.
    #[serde(default)]
    pub(crate) event_based_hold_release_time: Option<SystemTime>,
    pub custom_time: Option<CustomTime>,
    /// Bucket retention policy expiration only, counted from [`Self::retention_period_start`];
    /// object retention is kept in `retention`.
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

    /// The digests of the stored bytes, which a copy of them must reproduce.
    pub(crate) const fn hashes(&self) -> Hashes {
        Hashes {
            md5: self.md5,
            crc32c: self.crc32c,
        }
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

    /// When the bucket retention period of this generation starts: at creation, or when its
    /// event-based hold was last released, since "retention duration of the objects begins from
    /// the moment event-based hold transitioned from true to false" (Objects resource,
    /// `eventBasedHold`). `None` while that hold is held: "retention expiration is unknown until
    /// the hold is removed" (`retention_expire_time` in the pinned v2 and v1 protos). A release
    /// recorded under a clock set back before creation still counts from creation.
    pub(crate) fn retention_period_start(&self) -> Option<SystemTime> {
        if self.event_based_hold.is_held() {
            return None;
        }
        Some(
            self.event_based_hold_release_time
                .map_or(self.time_created, |released| {
                    released.max(self.time_created)
                }),
        )
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
    let rendered = crate::time_format::timestamp_nanos(time).unwrap_or_default();
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
    let invalid = invalid_custom_time;
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

fn invalid_custom_time() -> ObjectError {
    ObjectError::Metadata(crate::metadata_error::MetadataError::CustomTime)
}

/// Content fields reset on PUT; holds only change when explicitly supplied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectMetadataUpdate {
    pub(crate) acl: crate::acl::AclRequest,
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
    pub custom_time: Patch<CustomTime>,
    /// `Clear` removes the configuration; transitions are checked by the catalog.
    pub(crate) retention: Patch<retention::ObjectRetention>,
    pub(crate) override_unlocked_retention: retention::UnlockedOverride,
    /// Set only by data writes that carry a customer-supplied key; metadata patches keep it.
    pub customer_key_sha256: Option<[u8; 32]>,
    /// Requested class for a new generation; creation resolves it against the bucket's
    /// default and Autoclass. Metadata updates never change the class (rewrite does).
    pub storage_class: Option<StorageClass>,
}

impl From<crate::bucket::ClassLocationConflict> for ObjectError {
    fn from(conflict: crate::bucket::ClassLocationConflict) -> Self {
        Self::Metadata(crate::metadata_error::MetadataError::ClassLocation(
            conflict,
        ))
    }
}

pub(crate) fn validate_object_storage_class(class: &str) -> Result<StorageClass, ObjectError> {
    use crate::{bucket::InvalidStorageClass, metadata_error::MetadataError};
    StorageClass::from_name(class).map_err(|error| {
        ObjectError::Metadata(match error {
            InvalidStorageClass::RequiresZone => {
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
/// a retryable server error after the client has sent its data. This local bound is the only
/// size limit on custom metadata: Cloud Storage accepted values far past its documented 8 KiB,
/// up to a 64 KiB value (recorded 2026-11-15). Fixed metadata has no documented size but becomes
/// response headers, whose documented XML API total is 16 KiB.
const ENCODED_METADATA_LIMIT: usize = 40 * 1024;

/// The refusal of metadata past [`ENCODED_METADATA_LIMIT`].
fn encoded_metadata_too_large() -> ObjectError {
    ObjectError::InvalidFixedMetadata(format!(
        "Object metadata exceeds {} KiB once encoded as JSON; shorten its fixed and custom metadata.",
        ENCODED_METADATA_LIMIT / 1024
    ))
}

pub(crate) fn check_custom_metadata_size<T: serde::Serialize>(
    entries: &BTreeMap<String, T>,
) -> Result<(), ObjectError> {
    if encoded_json_len(entries) > ENCODED_METADATA_LIMIT {
        return Err(encoded_metadata_too_large());
    }
    Ok(())
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
            return Err(encoded_metadata_too_large());
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), ObjectError> {
        self.check_encoded_size()?;
        if let Some(value) = self.custom_time.value() {
            value.instant()?;
        }
        for (field, patch) in self.fixed_fields() {
            if let Some(value) = patch.value() {
                validate_fixed_metadata(field, value)?;
            }
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

    /// Applies the update to a published generation at `now`, where a PUT also clears the holds
    /// it omits. Releasing a held event-based hold, by either, records `now` as the start of the
    /// bucket retention period. Edits of one field at a time may not add up past what a later
    /// copy could inherit.
    pub(crate) fn apply_to_existing(
        self,
        record: &mut ObjectRecord,
        now: SystemTime,
    ) -> Result<(), ObjectError> {
        let event_based_hold = record.event_based_hold;
        if self.write == WriteMode::Replace {
            record.clear_holds();
        }
        self.apply(record)?;
        if event_based_hold.released_by(record.event_based_hold) {
            record.event_based_hold_release_time = Some(now);
        }
        Self::inheriting(record).check_encoded_size()
    }

    /// The update that gives a new object the editable metadata of `source`, except its holds.
    /// Empty content fields carry over: a copy kept the empty strings its source stored
    /// (recorded 2026-11-17, case obj-gaps-json).
    pub(crate) fn inheriting(source: &ObjectRecord) -> Self {
        let set = |value: &Option<String>| Patch::nullable(value.clone());
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

    /// Applies the update to a new generation. Empty content fields are stored as on an
    /// existing one: an upload kept them (recorded 2026-11-17, case obj-gaps-json).
    pub(crate) fn apply(self, record: &mut ObjectRecord) -> Result<(), ObjectError> {
        self.validate()?;
        // Cloud Storage kept the current custom time for `null` and for a PUT that omits it
        // (both recorded as 200); only a time can change it, and never backwards.
        if let Patch::Set(value) = self.custom_time {
            let next = value.instant()?;
            if let Some(previous) = &record.custom_time {
                let previous = previous.instant()?;
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
        self.retention.apply_to(&mut record.retention);
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
            patch.apply_to(field);
        }
        apply_custom_metadata(&mut record.metadata, self.metadata);
        // Merges can grow the stored custom metadata past what any one request carried. Fixed
        // fields replace rather than accumulate, so only the merged map needs the bound again.
        check_custom_metadata_size(&record.metadata)?;
        for (field, value) in record.fixed_fields() {
            if let Some(value) = value {
                validate_fixed_metadata(field, value)?;
            }
        }
        Ok(())
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

impl ObjectTarget<Option<Generation>> {
    pub(crate) fn into_signed_target(self) -> ObjectTarget {
        ObjectTarget::new(self.bucket, self.name, self.generation.map(Generation::get))
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

/// Digests a write's bytes must have, checked once they are all known: declared by a client,
/// or recorded for the bytes a copy reproduces.
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

/// A digest the bytes were expected to have and the one computed from them, at the width of its
/// algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HashMismatch {
    #[error("CRC32C {expected:08x} expected, {computed:08x} computed")]
    Crc32c { expected: u32, computed: u32 },
    #[error("MD5 {} expected, {} computed", crate::hashing::encode_lower(.expected), crate::hashing::encode_lower(.computed))]
    Md5 {
        expected: [u8; 16],
        computed: [u8; 16],
    },
}

impl HashMismatch {
    pub(crate) const fn algorithm(&self) -> HashAlgorithm {
        match self {
            Self::Crc32c { .. } => HashAlgorithm::Crc32c,
            Self::Md5 { .. } => HashAlgorithm::Md5,
        }
    }
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

    /// Cloud Storage's message for a declared digest of this algorithm that the bytes did not
    /// match, worded alike by an XML PUT and by Storage v2 ComposeObject and RewriteObject
    /// (recorded 2026-11-13 and 2026-11-17).
    pub(crate) const fn mismatch_message(self) -> &'static str {
        match self {
            Self::Crc32c => "The CRC32C you specified did not match what we computed.",
            Self::Md5 => {
                "The MD5 you specified in Content-MD5 or x-goog-hash did not match what we computed."
            }
        }
    }
}

/// Both digests of known bytes, declared so that a copy of them is verified.
impl From<Hashes> for ExpectedHashes {
    fn from(Hashes { md5, crc32c }: Hashes) -> Self {
        Self {
            md5: Some(md5),
            crc32c: Some(crc32c),
        }
    }
}

impl From<HashMismatch> for ObjectError {
    fn from(mismatch: HashMismatch) -> Self {
        Self::HashMismatch(Box::new(mismatch))
    }
}

/// One declared digest of an object's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Checksum {
    Md5([u8; 16]),
    Crc32c(u32),
}

/// A second, different value for a digest already declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("conflicting {} declarations", .0.name())]
pub(crate) struct ChecksumConflict(pub(crate) HashAlgorithm);

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

    /// Checks the declared digests against the ones computed from the bytes, CRC32C first, and
    /// reports the first that disagrees, with both values.
    pub(crate) fn verify(&self, computed: &Hashes) -> Result<(), HashMismatch> {
        self.verify_crc32c(computed.crc32c)?;
        match self.md5.filter(|expected| *expected != computed.md5) {
            Some(expected) => Err(HashMismatch::Md5 {
                expected,
                computed: computed.md5,
            }),
            None => Ok(()),
        }
    }

    /// Checks a declared CRC32C alone, for bytes whose MD5 is never computed, such as a
    /// composite object's.
    pub(crate) fn verify_crc32c(&self, computed: u32) -> Result<(), HashMismatch> {
        match self.crc32c.filter(|expected| *expected != computed) {
            Some(expected) => Err(HashMismatch::Crc32c { expected, computed }),
            None => Ok(()),
        }
    }

    /// Combines two declarations of the same bytes; a digest both name must agree.
    pub(crate) fn merge(mut self, other: Self) -> Result<Self, ChecksumConflict> {
        let declared = other
            .md5
            .map(Checksum::Md5)
            .into_iter()
            .chain(other.crc32c.map(Checksum::Crc32c));
        for checksum in declared {
            self.agree(checksum)?;
        }
        Ok(self)
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
    fn digest_errors_name_both_values_and_have_no_source() {
        let crc = HashMismatch::Crc32c {
            expected: 1,
            computed: 0xff,
        };
        let md5 = HashMismatch::Md5 {
            expected: [0x12; 16],
            computed: [0xab; 16],
        };
        let conflict = ChecksumConflict(HashAlgorithm::Md5);
        let cases: &[(&dyn std::error::Error, &str)] = &[
            (&crc, "CRC32C 00000001 expected, 000000ff computed"),
            (
                &md5,
                "MD5 12121212121212121212121212121212 expected, abababababababababababababababab computed",
            ),
            (&conflict, "conflicting MD5 declarations"),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), *expected);
            assert!(error.source().is_none());
        }
    }

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

/// What a request asked of a bucket's soft-deleted generations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SoftDeletedRequest {
    List,
    Get,
    Restore,
}

impl SoftDeletedRequest {
    /// Cloud Storage's refusal when the bucket has no soft delete policy (recorded
    /// 2026-11-17, case obj-gaps-json); each request kind has its own wording.
    pub(crate) const fn policy_required_message(self) -> &'static str {
        match self {
            Self::List => "Soft delete policy is required to list soft-deleted versions",
            Self::Get => "Soft delete policy is required to request a soft-deleted version.",
            Self::Restore => "bucket soft delete policy must be set.",
        }
    }
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
    /// The bucket has no positive soft delete policy, so it keeps no soft-deleted generation
    /// to list, read or restore.
    SoftDeletePolicyRequired(SoftDeletedRequest),
    /// Strict authorization refused a Cloud Storage permission, including when the bucket's
    /// current owning project is not one the caller was authorized for.
    Denied(Box<crate::auth::Denied>),
    /// Refused for a reason no Cloud Storage permission names: an upload session whose bucket
    /// was recreated, administrative access through upload replay, or a bucket refusal of that
    /// kind ([`CatalogError::Forbidden`](crate::bucket::CatalogError::Forbidden)).
    Forbidden,
    NotModified,
    /// A write's `ifGenerationNotMatch` or `ifMetagenerationNotMatch` named the current value of
    /// the object it would change, delete or replace; nothing was written. Cloud Storage answers
    /// 304 over JSON (recorded 2026-11 for patch, update, delete, media upload and copy), while
    /// gRPC still reports a failed precondition.
    WriteNotModified,
    /// A copy or rewrite source's `ifSourceGenerationNotMatch` or
    /// `ifSourceMetagenerationNotMatch` named its current value while its match conditions
    /// held; nothing was written. The JSON and XML APIs answer it as any failed source
    /// condition, while Storage v2 RewriteObject words it as a resource that has not been
    /// modified (recorded 2026-11-18).
    SourceNotModified,
    /// Two declarations of a digest disagreed, a digest without a typed algorithm (a signed
    /// payload's SHA-256) did not match, or a mirrored file's fingerprint no longer matches the
    /// record it was published as.
    ChecksumMismatch,
    /// The bytes disagreed with a digest the client declared, named with both values for the
    /// JSON message. Bytes that fail a digest recorded for their source are [`Self::Corrupt`].
    /// Boxed to keep [`ObjectError`] small.
    HashMismatch(Box<HashMismatch>),
    TooLarge,
    InvalidFixedMetadata(String),
    /// A predefined ACL reached publication on a bucket with uniform access enabled. Typed
    /// because each protocol words this refusal differently.
    UniformAccessPredefinedAcl,
    /// A metadata write named a predefined ACL on a bucket with uniform access enabled, which
    /// Cloud Storage words apart from the publication refusal.
    UniformAccessAclUpdate,
    Metadata(crate::metadata_error::MetadataError),
    InvalidDerivedArgument(String),
    /// Compose components whose classes are not all the destination's: the class the request
    /// named or, without one, the first component's.
    ComposeClassMismatch {
        destination: StorageClass,
        /// The other component classes, each once in component order; never empty.
        sources: Vec<StorageClass>,
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
            Self::SoftDeletePolicyRequired(request) => {
                formatter.write_str(request.policy_required_message())
            }
            Self::Denied(denied) => denied.fmt(formatter),
            Self::Forbidden => formatter.write_str("forbidden"),
            Self::NotModified | Self::WriteNotModified | Self::SourceNotModified => {
                formatter.write_str("not modified")
            }
            Self::ChecksumMismatch | Self::HashMismatch(_) => {
                formatter.write_str("checksum mismatch")
            }
            Self::TooLarge => formatter.write_str("object too large"),
            Self::InvalidFixedMetadata(reason) | Self::InvalidDerivedArgument(reason) => {
                formatter.write_str(reason)
            }
            Self::UniformAccessPredefinedAcl => {
                formatter.write_str(crate::acl::UNIFORM_ACCESS_UPLOAD_ACL)
            }
            Self::UniformAccessAclUpdate => {
                formatter.write_str(crate::acl::UNIFORM_ACCESS_UPDATE_OBJECT_ACL)
            }
            Self::Metadata(error) => error.fmt(formatter),
            // Cloud Storage's wording (recorded 2026-11-13 for a named class and 2026-11-17 for
            // mixed components); the separator between several classes is not recorded.
            Self::ComposeClassMismatch {
                destination,
                sources,
            } => write!(
                formatter,
                "Cannot compose object with destination storage class {} from source components that have different storage classes ({}).",
                destination.as_str(),
                sources
                    .iter()
                    .map(|class| class.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
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
            Self::BodyAborted(cause) | Self::Corrupt(cause) | Self::Unavailable(cause) => {
                cause.typed_source()
            }
            Self::Metadata(error) => Link::Leaf(error).source(),
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
            | Self::SoftDeletePolicyRequired(_)
            | Self::Denied(_)
            | Self::Forbidden
            | Self::NotModified
            | Self::WriteNotModified
            | Self::SourceNotModified
            | Self::ChecksumMismatch
            | Self::HashMismatch(_)
            | Self::TooLarge
            | Self::InvalidFixedMetadata(_)
            | Self::UniformAccessPredefinedAcl
            | Self::UniformAccessAclUpdate
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
    pub(crate) fn backend(error: impl Into<crate::cause::Source>) -> Self {
        Self::Unavailable(BackendCause::from_error(error))
    }

    /// The request body stream failed; the transport error stays the source.
    pub(crate) fn body_aborted(error: impl Into<crate::cause::Source>) -> Self {
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
            error @ (CatalogError::AlreadyExists
            | CatalogError::NotFound
            | CatalogError::NotEmpty
            | CatalogError::RestoreConflict
            | CatalogError::PreconditionFailed
            | CatalogError::NotModified
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

#[derive(Debug, thiserror::Error)]
pub(crate) enum FixedMetadataError {
    #[error(transparent)]
    Invalid(#[from] crate::metadata_error::MetadataError),
    #[error("{}", crate::input::unsupported_object_setting(Self::NON_ASCII))]
    NonAscii,
}

impl FixedMetadataError {
    const NON_ASCII: &'static str = "non-ASCII fixed metadata";
}

impl From<FixedMetadataError> for ObjectError {
    fn from(error: FixedMetadataError) -> Self {
        match error {
            FixedMetadataError::Invalid(error) => Self::Metadata(error),
            FixedMetadataError::NonAscii => {
                Self::UnsupportedMetadata(FixedMetadataError::NON_ASCII.into())
            }
        }
    }
}

pub(crate) fn validate_fixed_metadata(key: &str, value: &str) -> Result<(), FixedMetadataError> {
    if key == "contentLanguage" && value.chars().count() > 100 {
        return Err(crate::metadata_error::MetadataError::LongLanguage.into());
    }
    if !value.is_ascii() {
        return Err(FixedMetadataError::NonAscii);
    }
    // Fixed metadata becomes response headers; reject controls in every storage caller.
    if value.bytes().any(|byte| byte < 32 || byte == 127) {
        return Err(crate::metadata_error::MetadataError::FixedHeader(key.into()).into());
    }
    Ok(())
}
