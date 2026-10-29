//! Transport-independent object state shared by the catalog and protocol adapters.

use std::{collections::BTreeMap, fmt, time::SystemTime};

#[path = "object_retention.rs"]
pub mod retention;

/// Only a transport-confirmed EOF permits publication; a dropped producer is an abort.
pub enum UploadChunk {
    Data(bytes::Bytes),
    End,
    EndWithHashes(ExpectedHashes),
}

/// Immutable generation metadata persisted for live and noncurrent objects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectRecord {
    pub bucket: String,
    pub name: String,
    pub generation: i64,
    pub metageneration: i64,
    pub content_type: String,
    pub content_encoding: Option<String>,
    pub content_disposition: Option<String>,
    pub content_language: Option<String>,
    pub cache_control: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub size: u64,
    pub md5: [u8; 16],
    pub crc32c: u32,
    pub component_count: Option<i64>,
    pub storage_class: String,
    /// Microsecond precision in the catalog; protocol adapters retain their wire precision.
    pub time_created: SystemTime,
    pub updated: SystemTime,
    pub time_deleted: Option<SystemTime>,
    pub soft_delete_time: Option<SystemTime>,
    pub hard_delete_time: Option<SystemTime>,
    pub event_based_hold: bool,
    pub temporary_hold: bool,
    pub custom_time: Option<String>,
    /// Bucket retention policy expiration only; object retention is kept in `retention`.
    pub retention_expiration_time: Option<SystemTime>,
    pub retention: Option<retention::ObjectRetention>,
    /// SHA-256 of the customer-supplied AES256 key; the key itself is never stored.
    pub customer_key_sha256: Option<[u8; 32]>,
}

impl ObjectRecord {
    pub(crate) fn has_hold(&self) -> bool {
        self.event_based_hold || self.temporary_hold
    }

    /// Guards removing this generation permanently (or soft deleting it).
    pub(crate) fn check_retention(&self, now: SystemTime) -> Result<(), ObjectError> {
        self.check_policy_and_holds(now)?;
        self.check_object_retention(now)
    }

    /// Holds and the bucket policy also forbid making a live object noncurrent.
    pub(crate) fn check_policy_and_holds(&self, now: SystemTime) -> Result<(), ObjectError> {
        if self.has_hold() {
            return Err(ObjectError::ActiveHold);
        }
        if self
            .retention_expiration_time
            .is_some_and(|deadline| now < deadline)
        {
            return Err(ObjectError::RetentionPolicyNotMet);
        }
        Ok(())
    }

    /// The object-lock guide allows a retained live version to become noncurrent, so
    /// callers apply this only when the generation would be deleted or replaced.
    pub(crate) fn check_object_retention(&self, now: SystemTime) -> Result<(), ObjectError> {
        if self
            .retention
            .is_some_and(|retention| retention.active(now))
        {
            return Err(ObjectError::ObjectRetentionNotMet);
        }
        Ok(())
    }

    /// The earliest deletion time across the bucket policy and the object's own retention.
    pub(crate) fn earliest_deletion_time(&self) -> Option<SystemTime> {
        self.retention_expiration_time
            .into_iter()
            .chain(self.retention.map(|retention| retention.retain_until))
            .max()
    }
}

/// Reuse the strict civil-date parser while preserving nanosecond precision on the wire.
pub(crate) fn custom_time_key(value: &str) -> Result<(i64, u32), ObjectError> {
    let invalid = || {
        ObjectError::InvalidFixedMetadata(
            "customTime must be RFC 3339, for example 2026-10-20T01:02:03Z".into(),
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
    pub acl: Option<Vec<crate::acl::AclEntry>>,
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
    pub retention: Option<Option<retention::ObjectRetention>>,
    pub override_unlocked_retention: bool,
    /// Set only by data writes that carry a customer-supplied key; metadata patches keep it.
    pub customer_key_sha256: Option<[u8; 32]>,
    /// Requested class for a new generation; creation resolves it against the bucket's
    /// default and Autoclass. Metadata updates never change the class (rewrite does).
    pub storage_class: Option<String>,
}

pub(crate) fn validate_object_storage_class(class: &str) -> Result<(), ObjectError> {
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
        if let Some(class) = &self.storage_class {
            validate_object_storage_class(class)?;
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

    pub(crate) fn apply(self, record: &mut ObjectRecord) -> Result<(), ObjectError> {
        self.validate()?;
        if let Some(value) = self.custom_time.or_else(|| self.replace.then_some(None)) {
            let next = value.as_deref().map(custom_time_key).transpose()?;
            if let Some(previous) = &record.custom_time {
                let previous = custom_time_key(previous)?;
                if next.is_none_or(|v| v < previous) {
                    return Err(ObjectError::InvalidFixedMetadata(
                        "customTime cannot be removed or decreased; supply the current or a later RFC 3339 time".into(),
                    ));
                }
            }
            record.custom_time = value;
        }
        if let Some(hold) = self.event_based_hold {
            record.event_based_hold = hold;
        }
        if let Some(hold) = self.temporary_hold {
            record.temporary_hold = hold;
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObjectConditions {
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

#[derive(Debug, PartialEq, Eq)]
pub enum ObjectError {
    BucketNotFound,
    ObjectNotFound,
    PreconditionFailed,
    PublicAccessPrevention,
    /// Publication would replace a live object, but the caller may not delete it.
    OverwriteForbidden,
    ActiveHold,
    RetentionPolicyNotMet,
    /// The object's own retain-until time has not passed.
    ObjectRetentionNotMet,
    ObjectNotSoftDeleted,
    /// The bucket's current owning project is not one the caller was authorized for.
    Forbidden,
    NotModified,
    ChecksumMismatch,
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
    BodyAborted(String),
    Io(std::io::ErrorKind),
    Corrupt(String),
    Unavailable(String),
}

impl fmt::Display for ObjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BucketNotFound => formatter.write_str("bucket not found"),
            Self::ObjectNotFound => formatter.write_str("object not found"),
            Self::PreconditionFailed => formatter.write_str("precondition failed"),
            Self::PublicAccessPrevention => {
                formatter.write_str("public access prevention is enforced")
            }
            Self::OverwriteForbidden => formatter.write_str("overwrite forbidden"),
            Self::ActiveHold => formatter.write_str("object under active hold"),
            Self::RetentionPolicyNotMet => {
                formatter.write_str("object retention period has not elapsed")
            }
            Self::ObjectRetentionNotMet => {
                formatter.write_str("object retain-until time has not elapsed")
            }
            Self::ObjectNotSoftDeleted => formatter.write_str(
                "object is not soft deleted; select a generation from softDeleted=true listing",
            ),
            Self::Forbidden => formatter.write_str("forbidden"),
            Self::NotModified => formatter.write_str("not modified"),
            Self::ChecksumMismatch => formatter.write_str("checksum mismatch"),
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

impl std::error::Error for ObjectError {}

impl ObjectError {
    pub(crate) fn is_backend_failure(&self) -> bool {
        matches!(self, Self::Unavailable(_) | Self::Io(_) | Self::Corrupt(_))
    }
}

impl From<crate::bucket::CatalogError> for ObjectError {
    fn from(error: crate::bucket::CatalogError) -> Self {
        match error {
            crate::bucket::CatalogError::Io(kind) => Self::Io(kind),
            crate::bucket::CatalogError::Corrupt(reason) => Self::Corrupt(reason),
            error => Self::Unavailable(error.to_string()),
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
