//! Transport-independent object state shared by the catalog and protocol adapters.

use std::{fmt, time::SystemTime};

/// Only a transport-confirmed EOF permits publication; a dropped producer is an abort.
pub enum UploadChunk {
    Data(bytes::Bytes),
    End,
}

/// Live object generation metadata persisted by the catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectRecord {
    pub bucket: String,
    pub name: String,
    pub generation: i64,
    pub metageneration: i64,
    pub content_type: String,
    pub size: u64,
    pub md5: [u8; 16],
    pub crc32c: u32,
    pub storage_class: String,
    /// Millisecond precision, matching bucket timestamps.
    pub time_created: SystemTime,
    pub updated: SystemTime,
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
    NotModified,
    ChecksumMismatch,
    TooLarge,
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
            Self::NotModified => formatter.write_str("not modified"),
            Self::ChecksumMismatch => formatter.write_str("checksum mismatch"),
            Self::TooLarge => formatter.write_str("object too large"),
            Self::BodyAborted(reason) => write!(formatter, "request body aborted: {reason}"),
            Self::Unavailable(reason) => write!(formatter, "catalog unavailable: {reason}"),
        }
    }
}

impl std::error::Error for ObjectError {}
