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
    pub versioning_enabled: bool,
    pub default_event_based_hold: bool,
    pub metageneration: i64,
    /// Millisecond precision; the catalog truncates sub-millisecond values.
    pub time_created: SystemTime,
    pub updated: SystemTime,
}

#[derive(Clone, Debug, Default)]
pub struct BucketMetadataUpdate {
    pub labels: Option<Option<BTreeMap<String, Option<String>>>>,
    pub storage_class: Option<String>,
    pub versioning_enabled: Option<bool>,
    pub default_event_based_hold: Option<bool>,
    pub location: Option<String>,
}

pub(crate) fn valid_label(value: &str, key: bool) -> bool {
    let initial = |character: char| {
        character.is_ascii_lowercase()
            || (!character.is_ascii() && character.is_alphabetic() && !character.is_uppercase())
    };
    value.chars().count() <= 63
        && (!key || value.chars().next().is_some_and(initial))
        && value.chars().all(|character| {
            initial(character) || character.is_numeric() || matches!(character, '_' | '-')
        })
}

pub(crate) fn validate_storage_class(storage_class: &str) -> Result<(), CatalogError> {
    match storage_class {
        "STANDARD"
        | "NEARLINE"
        | "COLDLINE"
        | "ARCHIVE"
        | "MULTI_REGIONAL"
        | "DURABLE_REDUCED_AVAILABILITY" => Ok(()),
        "REGIONAL" | "RAPID" => Err(CatalogError::UnsupportedMetadata(
            "storageClass requires a region or zone".into(),
        )),
        _ => Err(CatalogError::InvalidMetadata(
            "Invalid bucket storageClass".into(),
        )),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CatalogError {
    AlreadyExists,
    NotFound,
    NotEmpty,
    PreconditionFailed,
    /// The bucket's current owning project is not one the caller was authorized for.
    Forbidden,
    InvalidMetadata(String),
    UnsupportedMetadata(String),
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
            Self::InvalidMetadata(reason) | Self::UnsupportedMetadata(reason) => {
                formatter.write_str(reason)
            }
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
        identity: Option<crate::auth::Identity>,
        permission: String,
        project: Option<String>,
    },
}
