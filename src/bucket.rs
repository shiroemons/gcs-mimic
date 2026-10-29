//! Transport-independent bucket state shared by the catalog and protocol adapters.

use std::{
    collections::BTreeMap,
    fmt,
    time::{Duration, SystemTime},
};

pub mod lifecycle;

pub mod autoclass;

pub(crate) const UNIFORM_ACCESS_LOCK_PERIOD: Duration = Duration::from_secs(90 * 24 * 60 * 60);

// Pinned to the official locations page checked in sdk-regional-locations.md.
const REGIONS: &[&str] = &[
    "AFRICA-SOUTH1",
    "ASIA-EAST1",
    "ASIA-EAST2",
    "ASIA-NORTHEAST1",
    "ASIA-NORTHEAST2",
    "ASIA-NORTHEAST3",
    "ASIA-SOUTH1",
    "ASIA-SOUTH2",
    "ASIA-SOUTHEAST1",
    "ASIA-SOUTHEAST2",
    "ASIA-SOUTHEAST3",
    "AUSTRALIA-SOUTHEAST1",
    "AUSTRALIA-SOUTHEAST2",
    "EUROPE-CENTRAL2",
    "EUROPE-NORTH1",
    "EUROPE-NORTH2",
    "EUROPE-SOUTHWEST1",
    "EUROPE-WEST1",
    "EUROPE-WEST2",
    "EUROPE-WEST3",
    "EUROPE-WEST4",
    "EUROPE-WEST6",
    "EUROPE-WEST8",
    "EUROPE-WEST9",
    "EUROPE-WEST10",
    "EUROPE-WEST12",
    "ME-CENTRAL1",
    "ME-CENTRAL2",
    "ME-WEST1",
    "NORTHAMERICA-NORTHEAST1",
    "NORTHAMERICA-NORTHEAST2",
    "NORTHAMERICA-SOUTH1",
    "SOUTHAMERICA-EAST1",
    "SOUTHAMERICA-WEST1",
    "US-CENTRAL1",
    "US-EAST1",
    "US-EAST4",
    "US-EAST5",
    "US-SOUTH1",
    "US-WEST1",
    "US-WEST2",
    "US-WEST3",
    "US-WEST4",
];
const DUAL_REGIONS: &[&str] = &["ASIA1", "EUR4", "EUR5", "EUR7", "EUR8", "NAM4"];
const MULTI_REGIONS: &[&str] = &["US", "EU", "ASIA"];

pub(crate) fn bucket_location(value: &str) -> Result<(&'static str, &'static str), CatalogError> {
    for (names, kind) in [
        (REGIONS, "region"),
        (DUAL_REGIONS, "dual-region"),
        (MULTI_REGIONS, "multi-region"),
    ] {
        if let Some(name) = names.iter().find(|name| name.eq_ignore_ascii_case(value)) {
            return Ok((name, kind));
        }
    }
    Err(CatalogError::InvalidMetadata(
        "Unknown bucket location; use a supported location, for example US or ASIA-NORTHEAST1"
            .into(),
    ))
}

#[test]
fn official_bucket_location_names_and_types() {
    for (names, kind) in [
        (REGIONS, "region"),
        (DUAL_REGIONS, "dual-region"),
        (MULTI_REGIONS, "multi-region"),
    ] {
        for name in names {
            assert_eq!(
                bucket_location(&name.to_ascii_lowercase()).ok(),
                Some((*name, kind))
            );
        }
    }
    for invalid in ["MARS-CENTRAL1", "US-CENTRAL1-A", "AU", "", " US"] {
        assert!(bucket_location(invalid).is_err());
    }
}

/// Bucket metadata persisted by the catalog. Protocol adapters derive wire fields from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BucketRecord {
    pub name: String,
    /// Project identifier exactly as supplied by the creating request.
    pub project: String,
    pub location: String,
    pub location_type: String,
    pub storage_class: String,
    pub autoclass: Option<autoclass::Autoclass>,
    pub labels: BTreeMap<String, String>,
    pub cors: Vec<CorsRule>,
    pub lifecycle: Vec<serde_json::Value>,
    pub retention_policy: Option<RetentionPolicy>,
    pub soft_delete_policy: Option<SoftDeletePolicy>,
    /// Set only at creation (`enableObjectRetention=true`); it can never be disabled.
    pub object_retention_enabled: bool,
    pub versioning_enabled: bool,
    pub default_event_based_hold: bool,
    pub public_access_prevention_enforced: bool,
    pub uniform_bucket_level_access_locked_time: Option<SystemTime>,
    pub metageneration: i64,
    /// Microsecond precision; the catalog floors sub-microsecond values.
    pub time_created: SystemTime,
    pub updated: SystemTime,
}

#[derive(Clone, Debug, Default)]
pub struct BucketMetadataUpdate {
    pub acl: Option<Vec<crate::acl::AclEntry>>,
    pub default_object_acl: Option<Vec<crate::acl::AclEntry>>,
    pub cors: Option<Vec<CorsRule>>,
    pub lifecycle: Option<Vec<serde_json::Value>>,
    pub retention_period: Option<Option<u64>>,
    pub soft_delete_retention: Option<u64>,
    pub lock_retention_policy: bool,
    /// An echoed `objectRetention` value; it must match the stored state.
    pub object_retention_enabled: Option<bool>,
    pub labels: Option<Option<BTreeMap<String, Option<String>>>>,
    pub storage_class: Option<String>,
    pub autoclass: Option<autoclass::AutoclassUpdate>,
    pub versioning_enabled: Option<bool>,
    pub default_event_based_hold: Option<bool>,
    pub public_access_prevention_enforced: Option<bool>,
    pub uniform_bucket_level_access_enabled: Option<bool>,
    pub location: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetentionPolicy {
    pub retention_period: u64,
    pub effective_time: SystemTime,
    pub is_locked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SoftDeletePolicy {
    pub retention_duration_seconds: u64,
    pub effective_time: SystemTime,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CorsRule {
    pub origin: Vec<String>,
    pub method: Vec<String>,
    pub response_header: Vec<String>,
    pub max_age_seconds: Option<i32>,
}

pub(crate) fn cors_from_value(value: &serde_json::Value) -> Result<Vec<CorsRule>, CatalogError> {
    let invalid = || CatalogError::InvalidMetadata("Invalid bucket CORS configuration".into());
    let values = value.as_array().ok_or_else(invalid)?;
    let rules = values
        .iter()
        .map(|value| {
            let value = value.as_object().ok_or_else(invalid)?;
            let strings = |field| -> Result<Vec<String>, CatalogError> {
                match value.get(field) {
                    None => Ok(Vec::new()),
                    Some(value) => value
                        .as_array()
                        .ok_or_else(invalid)?
                        .iter()
                        .map(|value| value.as_str().map(str::to_owned).ok_or_else(invalid))
                        .collect(),
                }
            };
            Ok(CorsRule {
                origin: strings("origin")?,
                method: strings("method")?,
                response_header: strings("responseHeader")?,
                max_age_seconds: value
                    .get("maxAgeSeconds")
                    .map(|value| {
                        value
                            .as_i64()
                            .and_then(|value| i32::try_from(value).ok())
                            .ok_or_else(invalid)
                    })
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>, CatalogError>>()?;
    validate_cors(&rules)?;
    Ok(rules)
}

pub(crate) fn cors_value(rules: &[CorsRule]) -> serde_json::Value {
    serde_json::Value::Array(
        rules
            .iter()
            .map(|rule| {
                let mut value = serde_json::Map::new();
                for (field, strings) in [
                    ("origin", &rule.origin),
                    ("method", &rule.method),
                    ("responseHeader", &rule.response_header),
                ] {
                    if !strings.is_empty() {
                        value.insert(field.into(), serde_json::json!(strings));
                    }
                }
                if let Some(seconds) = rule.max_age_seconds {
                    value.insert("maxAgeSeconds".into(), seconds.into());
                }
                value.into()
            })
            .collect(),
    )
}

pub(crate) fn validate_cors(rules: &[CorsRule]) -> Result<(), CatalogError> {
    let mut bytes = 2usize;
    for rule in rules {
        bytes = bytes.saturating_add(3);
        for value in rule
            .origin
            .iter()
            .chain(&rule.method)
            .chain(&rule.response_header)
        {
            bytes = bytes.saturating_add(value.len()).saturating_add(3);
        }
        if rule.max_age_seconds.is_some() {
            bytes = bytes.saturating_add(18);
        }
        if bytes > 64 * 1024 {
            return Err(CatalogError::InvalidMetadata(
                "Bucket CORS exceeds 64 KiB".into(),
            ));
        }
        if rule.origin.iter().any(|origin| {
            origin.is_empty()
                || !origin.is_ascii()
                || origin
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
                || hyper::header::HeaderValue::from_str(origin).is_err()
        }) || rule
            .method
            .iter()
            .any(|method| method != "*" && hyper::Method::from_bytes(method.as_bytes()).is_err())
            || rule.response_header.iter().any(|name| {
                name != "*" && hyper::header::HeaderName::from_bytes(name.as_bytes()).is_err()
            })
            || rule.max_age_seconds.is_some_and(|seconds| seconds < 0)
        {
            return Err(CatalogError::InvalidMetadata(
                "Invalid bucket CORS configuration".into(),
            ));
        }
    }
    if cors_value(rules).to_string().len() > 64 * 1024 {
        return Err(CatalogError::InvalidMetadata(
            "Bucket CORS exceeds 64 KiB".into(),
        ));
    }
    Ok(())
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
    PublicAccessPrevention,
    /// The bucket's current owning project is not one the caller was authorized for.
    Forbidden,
    RetentionPolicyLocked(u64),
    RetentionPolicyNotSet,
    InvalidMetadata(String),
    UnsupportedMetadata(String),
    Io(std::io::ErrorKind),
    Corrupt(String),
    /// A later release migrated the catalog; migrations are one-way.
    NewerSchema {
        version: i64,
        supported: i64,
    },
    Unavailable(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists => formatter.write_str("bucket already exists"),
            Self::NotFound => formatter.write_str("bucket not found"),
            Self::NotEmpty => formatter.write_str("bucket is not empty"),
            Self::PreconditionFailed => formatter.write_str("precondition failed"),
            Self::PublicAccessPrevention => {
                formatter.write_str("public access prevention is enforced")
            }
            Self::Forbidden => formatter.write_str("forbidden"),
            Self::RetentionPolicyLocked(period) => write!(
                formatter,
                "Bucket retention policy is locked and cannot be removed or shortened; keep retentionPeriod at least {period} seconds or extend it, for example {{\"retentionPolicy\":{{\"retentionPeriod\":\"{}\"}}}}",
                period.saturating_add(1).min(3_155_759_999)
            ),
            Self::RetentionPolicyNotSet => formatter.write_str("No retention policy to lock; set retentionPolicy.retentionPeriod first, for example 3600"),
            Self::Io(kind) => write!(formatter, "catalog I/O: {kind}"),
            Self::Corrupt(reason) => write!(formatter, "catalog corrupt: {reason}"),
            Self::NewerSchema { version, supported } => write!(
                formatter,
                "unsupported catalog schema version {version}; this release supports schema {supported} and older"
            ),
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BucketAccess {
    /// Permissive profile: no authorization applies and overwrites are allowed.
    Permissive,
    Admin(crate::storage::admin::AdminGrant),
    /// Re-evaluate effective grants against the policy selected by the operation transaction.
    Policy {
        identity: Option<crate::auth::Identity>,
        permission: crate::auth::Permission,
        project: Option<String>,
    },
}
