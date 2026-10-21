//! Transport-independent bucket state shared by the catalog and protocol adapters.

use std::{collections::BTreeMap, fmt, time::SystemTime};

/// Bucket metadata persisted by the catalog. Protocol adapters derive wire fields from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BucketRecord {
    pub name: String,
    /// Project identifier exactly as supplied by the creating request.
    pub project: String,
    pub location: String,
    pub location_type: String,
    pub storage_class: String,
    pub labels: BTreeMap<String, String>,
    pub metageneration: i64,
    /// Millisecond precision; the catalog truncates sub-millisecond values.
    pub time_created: SystemTime,
    pub updated: SystemTime,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CatalogError {
    AlreadyExists,
    NotFound,
    NotEmpty,
    PreconditionFailed,
    Unavailable(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists => formatter.write_str("bucket already exists"),
            Self::NotFound => formatter.write_str("bucket not found"),
            Self::NotEmpty => formatter.write_str("bucket is not empty"),
            Self::PreconditionFailed => formatter.write_str("precondition failed"),
            Self::Unavailable(reason) => write!(formatter, "catalog unavailable: {reason}"),
        }
    }
}

impl std::error::Error for CatalogError {}
