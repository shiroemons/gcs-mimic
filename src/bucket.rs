//! Transport-independent bucket state shared by the catalog and protocol adapters.

use crate::storage::failure::{BackendCause, IoCause};
use std::{
    collections::BTreeMap,
    fmt,
    str::FromStr,
    time::{Duration, SystemTime},
};

pub(crate) mod lifecycle;

pub(crate) mod autoclass;

#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(try_from = "String")]
pub enum StorageClass {
    Standard,
    Nearline,
    Coldline,
    Archive,
    MultiRegional,
    DurableReducedAvailability,
}

impl StorageClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "STANDARD",
            Self::Nearline => "NEARLINE",
            Self::Coldline => "COLDLINE",
            Self::Archive => "ARCHIVE",
            Self::MultiRegional => "MULTI_REGIONAL",
            Self::DurableReducedAvailability => "DURABLE_REDUCED_AVAILABILITY",
        }
    }
}

crate::serde_util::serialize_as_str!(StorageClass);

// Upload fingerprints include the former String's quoted Debug representation.
impl fmt::Debug for StorageClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl FromStr for StorageClass {
    type Err = CatalogError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "STANDARD" => Ok(Self::Standard),
            "NEARLINE" => Ok(Self::Nearline),
            "COLDLINE" => Ok(Self::Coldline),
            "ARCHIVE" => Ok(Self::Archive),
            "MULTI_REGIONAL" => Ok(Self::MultiRegional),
            "DURABLE_REDUCED_AVAILABILITY" => Ok(Self::DurableReducedAvailability),
            "REGIONAL" | "RAPID" => Err(CatalogError::UnsupportedMetadata(
                "storageClass requires a region or zone".into(),
            )),
            _ => Err(CatalogError::InvalidMetadata(
                "The storage class you specified is not valid.".into(),
            )),
        }
    }
}

impl TryFrom<String> for StorageClass {
    type Error = CatalogError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

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
        "The specified location constraint is not valid.".into(),
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
    pub storage_class: StorageClass,
    pub(crate) autoclass: Option<autoclass::Autoclass>,
    pub labels: BTreeMap<String, String>,
    pub(crate) cors: Vec<CorsRule>,
    pub lifecycle: Vec<serde_json::Value>,
    pub(crate) retention_policy: Option<RetentionPolicy>,
    pub(crate) soft_delete_policy: Option<SoftDeletePolicy>,
    /// Set only at creation (`enableObjectRetention=true`); it can never be disabled.
    pub object_retention_enabled: bool,
    pub versioning_enabled: bool,
    /// Explicit false is observable; an unset versioning configuration has no JSON field.
    pub(crate) versioning_configured: bool,
    pub default_event_based_hold: bool,
    pub public_access_prevention_enforced: bool,
    pub uniform_bucket_level_access_locked_time: Option<SystemTime>,
    pub metageneration: i64,
    /// Version of the bucket; the catalog assigns it at creation and keeps it across a
    /// soft delete and restore. Values supplied to an insert are replaced.
    pub(crate) generation: i64,
    /// Microsecond precision; the catalog floors sub-microsecond values.
    pub time_created: SystemTime,
    pub updated: SystemTime,
}

/// A soft-deleted bucket version: the metadata it had when deleted plus its deletion times.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SoftDeletedBucket {
    pub record: BucketRecord,
    pub soft_delete_time: SystemTime,
    /// Always at or after the latest hard delete time of an object retained with the bucket.
    pub hard_delete_time: SystemTime,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BucketMetadataUpdate {
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
    pub(crate) labels_mode: crate::object::MapMode,
    pub storage_class: Option<StorageClass>,
    pub autoclass: Option<autoclass::AutoclassUpdate>,
    pub versioning_enabled: Option<bool>,
    pub(crate) versioning_configured: Option<bool>,
    pub default_event_based_hold: Option<bool>,
    pub public_access_prevention_enforced: Option<bool>,
    pub uniform_bucket_level_access_enabled: Option<bool>,
    pub location: Option<String>,
    /// A recognised member was present even though it sets nothing, which still counts as a
    /// change and advances the metageneration.
    pub touched: bool,
    /// A replacing write left the retention policy out, which clears an unlocked policy; a
    /// locked policy stays, because omitting it is not a request to shorten it.
    pub omitted_retention: bool,
}

impl BucketMetadataUpdate {
    /// True when no writable setting is present, as for a PATCH that carried only ignored members.
    pub(crate) fn is_empty(&self) -> bool {
        let Self {
            acl,
            default_object_acl,
            cors,
            lifecycle,
            retention_period,
            soft_delete_retention,
            lock_retention_policy,
            object_retention_enabled,
            labels,
            labels_mode: _,
            storage_class,
            autoclass,
            versioning_enabled,
            versioning_configured,
            default_event_based_hold,
            public_access_prevention_enforced,
            uniform_bucket_level_access_enabled,
            location,
            touched,
            omitted_retention,
        } = self;
        acl.is_none()
            && default_object_acl.is_none()
            && cors.is_none()
            && lifecycle.is_none()
            && retention_period.is_none()
            && soft_delete_retention.is_none()
            && !lock_retention_policy
            && object_retention_enabled.is_none()
            && labels.is_none()
            && storage_class.is_none()
            && autoclass
                .as_ref()
                .is_none_or(|update| *update == autoclass::AutoclassUpdate::default())
            && versioning_enabled.is_none()
            && versioning_configured.is_none()
            && default_event_based_hold.is_none()
            && public_access_prevention_enforced.is_none()
            && uniform_bucket_level_access_enabled.is_none()
            && location.is_none()
            && !touched
            && !omitted_retention
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RetentionPolicy {
    pub retention_period: u64,
    pub effective_time: SystemTime,
    pub is_locked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SoftDeletePolicy {
    pub retention_duration_seconds: u64,
    pub effective_time: SystemTime,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CorsRule {
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

/// Which half of a label pair is validated; only keys must start with a letter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LabelPart {
    Key,
    Value,
}

pub(crate) fn valid_label(value: &str, part: LabelPart) -> bool {
    let key = part == LabelPart::Key;
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

pub(crate) fn validate_storage_class(storage_class: &str) -> Result<StorageClass, CatalogError> {
    storage_class.parse()
}

pub(crate) fn validate_update_storage_class(class: &str) -> Result<StorageClass, CatalogError> {
    validate_storage_class(class).map_err(|error| match error {
        CatalogError::InvalidMetadata(_) => {
            CatalogError::InvalidMetadata(format!("Invalid storage class \"{class}\""))
        }
        error => error,
    })
}

pub(crate) const SOFT_DELETE_RANGE_MESSAGE: &str =
    "Soft delete policy must have a retention duration between 3 days and 90 days.";
pub(crate) const RETENTION_RANGE_MESSAGE: &str =
    "Retention policy must have a retention period greater than 0 and less than 100 years.";

pub(crate) fn soft_delete_retention(seconds: i64) -> Result<u64, CatalogError> {
    u64::try_from(seconds)
        .ok()
        .filter(|value| *value == 0 || (604800..=7776000).contains(value))
        .ok_or_else(|| CatalogError::InvalidMetadata(SOFT_DELETE_RANGE_MESSAGE.into()))
}

pub(crate) fn retention_period(seconds: i64) -> Result<u64, CatalogError> {
    u64::try_from(seconds)
        .ok()
        .filter(|value| (1..3_155_760_000).contains(value))
        .ok_or_else(|| CatalogError::InvalidMetadata(RETENTION_RANGE_MESSAGE.into()))
}

pub(crate) fn public_access_prevention(value: &str) -> Result<bool, CatalogError> {
    match value {
        "enforced" => Ok(true),
        "inherited" => Ok(false),
        _ => Err(CatalogError::InvalidMetadata(format!(
            "Invalid value for: {value} is not a valid value"
        ))),
    }
}

#[test]
fn storage_class_validation_keeps_exact_errors() {
    for class in [
        "STANDARD",
        "NEARLINE",
        "COLDLINE",
        "ARCHIVE",
        "MULTI_REGIONAL",
        "DURABLE_REDUCED_AVAILABILITY",
    ] {
        assert!(validate_storage_class(class).is_ok(), "{class}");
    }
    for class in ["REGIONAL", "RAPID"] {
        assert_eq!(
            validate_storage_class(class),
            Err(CatalogError::UnsupportedMetadata(
                "storageClass requires a region or zone".into()
            )),
            "{class}"
        );
    }
    for class in ["standard", "", "UNKNOWN", " STANDARD"] {
        assert_eq!(
            validate_storage_class(class),
            Err(CatalogError::InvalidMetadata(
                "The storage class you specified is not valid.".into()
            )),
            "{class}"
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CatalogError {
    AuthenticationAlreadyConfigured,
    AlreadyExists,
    NotFound,
    NotEmpty,
    /// A soft-deleted bucket cannot be restored because a live bucket has its name.
    RestoreConflict,
    PreconditionFailed,
    PublicAccessPrevention,
    /// The bucket's current owning project is not one the caller was authorized for.
    Forbidden,
    RetentionPolicyLocked(u64),
    RetentionPolicyNotSet,
    InvalidMetadata(String),
    UnsupportedMetadata(String),
    Io(IoCause),
    Corrupt(BackendCause),
    /// A later release migrated the catalog; migrations are one-way.
    NewerSchema {
        version: i64,
        supported: i64,
    },
    Unavailable(BackendCause),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthenticationAlreadyConfigured => {
                formatter.write_str("Catalog authentication is already configured")
            }
            Self::AlreadyExists => formatter.write_str("bucket already exists"),
            Self::NotFound => formatter.write_str("bucket not found"),
            Self::NotEmpty => formatter.write_str("bucket is not empty"),
            Self::RestoreConflict => formatter
                .write_str("a bucket with this name exists, so the soft-deleted one cannot be restored"),
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

impl std::error::Error for CatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(cause) => cause.source(),
            Self::Corrupt(cause) | Self::Unavailable(cause) => cause.source(),
            _ => None,
        }
    }
}

/// Optional metageneration preconditions for a bucket request, separate from
/// [`crate::object::ObjectConditions`]. The default carries no condition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BucketConditions {
    pub if_metageneration_match: Option<i64>,
    pub if_metageneration_not_match: Option<i64>,
}

impl BucketConditions {
    /// Return `true` when both optional preconditions hold for `metageneration`.
    /// Callers answering a matching `if_metageneration_not_match` with 304 rather than 412
    /// read the fields instead.
    pub(crate) fn hold(self, metageneration: i64) -> bool {
        self.if_metageneration_match
            .is_none_or(|value| value == metageneration)
            && self.if_metageneration_not_match != Some(metageneration)
    }
}

/// The adapter's bucket authorization, re-validated by the catalog inside the same transaction
/// that reads or mutates the bucket.
///
/// Authorization resolves the owning project in an earlier catalog call. The bucket can be
/// deleted and recreated under another project before the operation runs, so the operation
/// itself must confirm that the project it observes is one the caller was authorized for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BucketAccess {
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
