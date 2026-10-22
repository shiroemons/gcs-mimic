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
    /// The bucket's current owning project is not one the caller was authorized for.
    Forbidden,
    Unavailable(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists => formatter.write_str("bucket already exists"),
            Self::NotFound => formatter.write_str("bucket not found"),
            Self::NotEmpty => formatter.write_str("bucket is not empty"),
            Self::PreconditionFailed => formatter.write_str("precondition failed"),
            Self::Forbidden => formatter.write_str("forbidden"),
            Self::Unavailable(reason) => write!(formatter, "catalog unavailable: {reason}"),
        }
    }
}

impl std::error::Error for CatalogError {}

/// The adapter's bucket authorization, re-validated by the catalog inside the same transaction
/// that reads or mutates the bucket.
///
/// Authorization resolves the owning project in an earlier catalog call. The bucket can be
/// deleted and recreated under another project before the operation runs, so the operation
/// itself must confirm that the project it observes is one the caller was authorized for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BucketAccess {
    /// Permissive profile: no authorization applies and overwrites are allowed.
    #[default]
    Unchecked,
    /// Re-evaluate effective grants against the policy selected by the operation transaction.
    Policy {
        identity: crate::auth::Identity,
        permission: String,
        project: Option<String>,
    },
}
