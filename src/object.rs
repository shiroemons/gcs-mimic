//! Transport-independent object state shared by the catalog and protocol adapters.

use std::{collections::BTreeMap, fmt, time::SystemTime};

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
    /// Millisecond precision, matching bucket timestamps.
    pub time_created: SystemTime,
    pub updated: SystemTime,
    pub time_deleted: Option<SystemTime>,
    pub event_based_hold: bool,
}

/// Content fields reset on PUT; holds only change when explicitly supplied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectMetadataUpdate {
    pub replace: bool,
    pub content_type: Option<Option<String>>,
    pub content_encoding: Option<Option<String>>,
    pub content_disposition: Option<Option<String>>,
    pub content_language: Option<Option<String>>,
    pub cache_control: Option<Option<String>>,
    pub metadata: Option<Option<BTreeMap<String, Option<String>>>>,
    pub event_based_hold: Option<bool>,
}

impl ObjectMetadataUpdate {
    pub(crate) fn apply(self, record: &mut ObjectRecord) -> Result<(), ObjectError> {
        if let Some(hold) = self.event_based_hold {
            record.event_based_hold = hold;
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

#[derive(Debug, PartialEq, Eq)]
pub enum ObjectError {
    BucketNotFound,
    ObjectNotFound,
    PreconditionFailed,
    /// Publication would replace a live object, but the caller may not delete it.
    OverwriteForbidden,
    ActiveHold,
    /// The bucket's current owning project is not one the caller was authorized for.
    Forbidden,
    NotModified,
    ChecksumMismatch,
    TooLarge,
    InvalidMetadata,
    InvalidDerivedArgument(String),
    DerivedTransferNotFound,
    DerivedTransferExpired,
    DerivedTransferBusy,
    DerivedTransferIncomplete,
    UnsupportedMetadata(String),
    /// The request body ended early or failed while streaming.
    BodyAborted(String),
    Unavailable(String),
}

impl fmt::Display for ObjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BucketNotFound => formatter.write_str("bucket not found"),
            Self::ObjectNotFound => formatter.write_str("object not found"),
            Self::PreconditionFailed => formatter.write_str("precondition failed"),
            Self::OverwriteForbidden => formatter.write_str("overwrite forbidden"),
            Self::ActiveHold => formatter.write_str("object under active hold"),
            Self::Forbidden => formatter.write_str("forbidden"),
            Self::NotModified => formatter.write_str("not modified"),
            Self::ChecksumMismatch => formatter.write_str("checksum mismatch"),
            Self::TooLarge => formatter.write_str("object too large"),
            Self::InvalidMetadata => formatter.write_str("custom metadata exceeds 8 KiB"),
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
            Self::BodyAborted(reason) => write!(formatter, "request body aborted: {reason}"),
            Self::Unavailable(reason) => write!(formatter, "catalog unavailable: {reason}"),
        }
    }
}

impl std::error::Error for ObjectError {}
