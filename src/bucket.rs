//! Transport-independent bucket state shared by the catalog and protocol adapters.

use crate::cause::{BackendCause, IoCause, Link, TypedSource};
use std::{
    collections::BTreeMap,
    fmt,
    str::FromStr,
    time::{Duration, SystemTime},
};

pub(crate) mod lifecycle;

pub(crate) mod autoclass;
pub(crate) mod cors;
pub(crate) mod location;
pub(crate) mod name;
pub(crate) mod resource;

pub(crate) use location::{Location, LocationKind};

crate::choice::string_enum! {
    table
    /// A storage class with a canonical API name. `from_name` keeps the reason a name was
    /// refused, and `FromStr` returns the `CatalogError` bucket requests use.
    #[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize)]
    #[serde(try_from = "String")]
    pub enum StorageClass as pub as_str {
        Standard => "STANDARD",
        Nearline => "NEARLINE",
        Coldline => "COLDLINE",
        Archive => "ARCHIVE",
        MultiRegional => "MULTI_REGIONAL",
        Regional => "REGIONAL",
        DurableReducedAvailability => "DURABLE_REDUCED_AVAILABILITY",
    };
    listed(#[cfg(test)] pub(crate));
    parse(pub(crate) from_name, InvalidStorageClass,
        "RAPID" => Err(InvalidStorageClass::RequiresZone),
        _ => Err(InvalidStorageClass::Unknown));
    serde(try_from_string)
}

impl StorageClass {
    /// Whether a bucket of `kind` can hold the class. Cloud Storage keeps the legacy REGIONAL
    /// class to regions and MULTI_REGIONAL to multi-regions (recorded 2026-11-17); a dual-region
    /// refused REGIONAL and is assumed to accept MULTI_REGIONAL, as the documentation says.
    pub(crate) const fn fits(self, kind: LocationKind) -> bool {
        match self {
            Self::Regional => matches!(kind, LocationKind::Region),
            Self::MultiRegional => !matches!(kind, LocationKind::Region),
            Self::Standard
            | Self::Nearline
            | Self::Coldline
            | Self::Archive
            | Self::DurableReducedAvailability => true,
        }
    }
}

/// A storage class that a bucket's location cannot hold. Cloud Storage gives the same refusal
/// wherever a request names the class: a bucket's default, an object's class and a lifecycle
/// action or condition (recorded 2026-11-17).
#[derive(Debug, PartialEq, Eq)]
pub struct ClassLocationConflict {
    project_number: crate::project::ProjectNumber,
    class: StorageClass,
    location: Location,
}

impl ClassLocationConflict {
    /// Checks `class` against a bucket of `project` in `location`.
    pub(crate) fn check(
        class: StorageClass,
        location: Location,
        project: &str,
    ) -> Result<(), Self> {
        if class.fits(location.kind()) {
            return Ok(());
        }
        Err(Self {
            project_number: crate::project::ProjectNumber::of(project),
            class,
            location,
        })
    }
}

impl fmt::Display for ClassLocationConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Project {} may not create storageClass {} buckets with locationConstraint {}.",
            self.project_number,
            self.class.as_str(),
            self.location.as_str()
        )
    }
}

impl std::error::Error for ClassLocationConflict {}

impl From<ClassLocationConflict> for CatalogError {
    fn from(conflict: ClassLocationConflict) -> Self {
        Self::Metadata(crate::metadata_error::MetadataError::ClassLocation(
            conflict,
        ))
    }
}

// Upload fingerprints include the former String's quoted Debug representation.
impl fmt::Debug for StorageClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Why a name is not a storage class the emulator stores; each caller words its own refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InvalidStorageClass {
    /// `RAPID` is a real class, but it needs a zonal bucket.
    RequiresZone,
    Unknown,
}

impl From<InvalidStorageClass> for CatalogError {
    fn from(error: InvalidStorageClass) -> Self {
        match error {
            InvalidStorageClass::RequiresZone => {
                Self::Metadata(crate::metadata_error::MetadataError::StorageClassRequiresZone)
            }
            InvalidStorageClass::Unknown => {
                Self::InvalidMetadata("The storage class you specified is not valid.".into())
            }
        }
    }
}

impl FromStr for StorageClass {
    type Err = CatalogError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_name(value)?)
    }
}

pub(crate) const UNIFORM_ACCESS_LOCK_PERIOD: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// Bucket metadata persisted by the catalog. Protocol adapters derive wire fields from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BucketRecord {
    pub name: String,
    /// Project identifier exactly as supplied by the creating request.
    pub project: String,
    /// The location; its type (`locationType`) follows from the name.
    pub(crate) location: Location,
    pub storage_class: StorageClass,
    pub(crate) autoclass: Option<autoclass::Autoclass>,
    pub labels: BTreeMap<String, String>,
    pub(crate) cors: Vec<CorsRule>,
    pub(crate) lifecycle: lifecycle::BucketLifecycle,
    pub(crate) retention_policy: Option<RetentionPolicy>,
    pub(crate) soft_delete_policy: Option<SoftDeletePolicy>,
    /// Set only at creation (`enableObjectRetention=true`); it can never be disabled.
    pub object_retention_enabled: bool,
    pub versioning: Versioning,
    pub default_event_based_hold: bool,
    pub(crate) public_access_prevention: PublicAccessPrevention,
    pub uniform_bucket_level_access_locked_time: Option<SystemTime>,
    pub metageneration: i64,
    /// Version of the bucket; the catalog assigns it at creation and keeps it across a
    /// soft delete and restore. Values supplied to an insert are replaced.
    pub(crate) generation: i64,
    /// Microsecond precision; the catalog floors sub-microsecond values.
    pub time_created: SystemTime,
    pub updated: SystemTime,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DefaultBucketError {
    #[error(transparent)]
    Project(#[from] crate::auth::InvalidProjectId),
    #[error(transparent)]
    Timestamp(#[from] crate::time_format::TimeOutOfRange),
}

impl BucketRecord {
    /// Canned ACL prechecks honor both the stored policy and a request to enable uniform access.
    pub(crate) fn uniform_access(&self, requested: Option<UniformAccessRequest>) -> UniformAccess {
        let locked_time = self.uniform_bucket_level_access_locked_time;
        if locked_time.is_some() || requested.is_some_and(|request| request.enabled(locked_time)) {
            UniformAccess::Enabled
        } else {
            UniformAccess::Disabled
        }
    }

    /// Native insert defaults for local callers that already validated the bucket name.
    pub(crate) fn default_for(
        name: &name::BucketName,
        project: &str,
        now: SystemTime,
    ) -> Result<Self, DefaultBucketError> {
        crate::auth::validate_project_id(project)?;
        crate::time_format::timestamp(now)?;
        Ok(Self::new(
            name.as_str().to_owned(),
            project,
            Location::US,
            now,
        ))
    }

    fn new(name: String, project: &str, location: Location, now: SystemTime) -> Self {
        Self {
            name,
            project: project.to_owned(),
            location,
            storage_class: StorageClass::Standard,
            autoclass: None,
            labels: BTreeMap::new(),
            cors: Vec::new(),
            lifecycle: Default::default(),
            retention_policy: None,
            soft_delete_policy: None,
            object_retention_enabled: false,
            versioning: Versioning::Unset,
            default_event_based_hold: false,
            public_access_prevention: Default::default(),
            uniform_bucket_level_access_locked_time: None,
            metageneration: 1,
            generation: 0,
            time_created: now,
            updated: now,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(name: &str, project: &str, now: SystemTime) -> Self {
        let name = name.parse().unwrap();
        Self::default_for(&name, project, now).unwrap()
    }
}

/// A soft-deleted bucket version: the metadata it had when deleted plus its deletion times.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SoftDeletedBucket {
    pub record: BucketRecord,
    pub soft_delete_time: SystemTime,
    /// Always at or after the latest hard delete time of an object retained with the bucket.
    pub hard_delete_time: SystemTime,
}

/// A requested uniform bucket-level access setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UniformAccessRequest {
    Set(bool),
    /// `uniformBucketLevelAccess` and its `bucketPolicyOnly` alias disagree, so the one that
    /// changes the stored setting wins. Cloud Storage enabled access on a disabled bucket when
    /// only the alias said true (recorded 2026-11), and Ruby's setter disables access while it
    /// resends the alias's old true value from GET; this is the one rule that fits both.
    Conflicting,
}

impl UniformAccessRequest {
    /// Whether this request leaves access enabled on a bucket whose stored lock deadline is
    /// `locked_time`; a bucket has one exactly while uniform access is enabled.
    pub(crate) fn enabled(self, locked_time: Option<SystemTime>) -> bool {
        match self {
            Self::Set(enabled) => enabled,
            Self::Conflicting => locked_time.is_none(),
        }
    }
}

impl From<bool> for UniformAccessRequest {
    fn from(enabled: bool) -> Self {
        Self::Set(enabled)
    }
}

/// Whether uniform bucket-level access governs the bucket a request targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UniformAccess {
    Enabled,
    Disabled,
}

/// What a bucket write does to the retention policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RetentionPolicyUpdate {
    /// The write leaves the policy as it is.
    #[default]
    Keep,
    /// Sets the period in seconds; a locked policy may only grow.
    Set(RetentionPeriod),
    /// Removes the policy (null, a policy without a period, or an empty policy in a PUT); a
    /// locked policy refuses.
    Remove,
    /// A PUT left the policy out: an unlocked policy is removed and a locked one stays, because
    /// omitting it is not a request to shorten it.
    RemoveUnlocked,
    /// Locks the current policy (`lockRetentionPolicy`); the request must name the
    /// metageneration it read.
    Lock,
}

/// A bucket's versioning setting. `Unset` has no `versioning` member in a JSON resource; an
/// explicit `{"enabled":false}` is `Disabled`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Versioning {
    #[default]
    Unset,
    Disabled,
    Enabled,
}

impl Versioning {
    pub fn is_enabled(self) -> bool {
        match self {
            Self::Enabled => true,
            Self::Unset | Self::Disabled => false,
        }
    }

    /// The stored `(versioning_enabled, versioning_configured)` columns.
    pub(crate) fn columns(self) -> (bool, bool) {
        match self {
            Self::Unset => (false, false),
            Self::Disabled => (false, true),
            Self::Enabled => (true, true),
        }
    }

    /// Decodes the stored `(versioning_enabled, versioning_configured)` columns. Buckets that
    /// enabled versioning before schema 49 kept `versioning_configured = 0`; every reader
    /// already treated them as enabled.
    pub(crate) fn from_columns(columns: (bool, bool)) -> Self {
        match columns {
            (true, _) => Self::Enabled,
            (false, true) => Self::Disabled,
            (false, false) => Self::Unset,
        }
    }
}

/// The setting a request's or import source's `versioning.enabled` names; absent is unset.
impl From<Option<bool>> for Versioning {
    fn from(enabled: Option<bool>) -> Self {
        match enabled {
            None => Self::Unset,
            Some(false) => Self::Disabled,
            Some(true) => Self::Enabled,
        }
    }
}

/// A requested versioning change; `None` in the update leaves the setting alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VersioningUpdate {
    Set(Versioning),
    /// Turns versioning off without making an unset setting visible: the fake-gcs-server
    /// profile's `{"enabled":null}` and its PUT of `versioning: {}`.
    Disable,
}

impl VersioningUpdate {
    pub(crate) fn apply(self, current: Versioning) -> Versioning {
        match self {
            Self::Set(versioning) => versioning,
            Self::Disable => match current {
                Versioning::Unset => Versioning::Unset,
                Versioning::Disabled | Versioning::Enabled => Versioning::Disabled,
            },
        }
    }
}

crate::choice::string_enum! {
    table
    /// A recovery point objective a bucket update names.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Rpo as pub(crate) as_str {
        Default => "DEFAULT",
        AsyncTurbo => "ASYNC_TURBO",
    };
    from_str(UnknownRpo, _ => Err(UnknownRpo))
}

/// A recovery point objective Cloud Storage does not define.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UnknownRpo;

#[derive(Clone, Debug, Default)]
pub(crate) struct BucketMetadataUpdate {
    pub acl: Option<Vec<crate::acl::AclEntry>>,
    pub default_object_acl: Option<Vec<crate::acl::AclEntry>>,
    pub cors: Option<Vec<CorsRule>>,
    pub(crate) lifecycle: Option<lifecycle::LifecycleRules>,
    pub(crate) retention_policy: RetentionPolicyUpdate,
    pub(crate) soft_delete_retention: Option<SoftDeleteRetention>,
    /// An echoed `objectRetention` value; it must match the stored state.
    pub object_retention_enabled: Option<bool>,
    pub labels: crate::object::MapPatch,
    pub storage_class: Option<StorageClass>,
    pub autoclass: Option<autoclass::AutoclassUpdate>,
    pub(crate) versioning: Option<VersioningUpdate>,
    pub default_event_based_hold: Option<bool>,
    pub(crate) public_access_prevention: Option<PublicAccessPrevention>,
    pub uniform_bucket_level_access_enabled: Option<UniformAccessRequest>,
    /// A named recovery point objective. It is checked against the stored location and never
    /// stored: `DEFAULT` is the only objective a dual- or multi-region bucket has here.
    pub(crate) rpo: Option<Rpo>,
    /// A recognised member was present even though it sets nothing, which still counts as a
    /// change and advances the metageneration.
    pub touched: bool,
}

impl BucketMetadataUpdate {
    /// True when no writable setting is present, as for a PATCH that carried only ignored members.
    pub(crate) fn is_empty(&self) -> bool {
        let Self {
            acl,
            default_object_acl,
            cors,
            lifecycle,
            retention_policy,
            soft_delete_retention,
            object_retention_enabled,
            labels,
            storage_class,
            autoclass,
            versioning,
            default_event_based_hold,
            public_access_prevention,
            uniform_bucket_level_access_enabled,
            // Checked before emptiness matters, and an accepted objective stores nothing.
            rpo: _,
            touched,
        } = self;
        acl.is_none()
            && default_object_acl.is_none()
            && cors.is_none()
            && lifecycle.is_none()
            && *retention_policy == RetentionPolicyUpdate::Keep
            && soft_delete_retention.is_none()
            && object_retention_enabled.is_none()
            && *labels == crate::object::MapPatch::Keep
            && storage_class.is_none()
            && autoclass
                .as_ref()
                .is_none_or(|update| *update == autoclass::AutoclassUpdate::default())
            && versioning.is_none()
            && default_event_based_hold.is_none()
            && public_access_prevention.is_none()
            && uniform_bucket_level_access_enabled.is_none()
            && !touched
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

impl SoftDeletePolicy {
    /// A zero duration turns soft delete off. Cloud Storage then reports the policy without an
    /// effective time, in JSON (recorded 2026-10-31) and Storage v2 (recorded 2026-11-17).
    pub(crate) fn is_enabled(&self) -> bool {
        self.retention_duration_seconds > 0
    }
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
                        value.insert(field.into(), strings.as_slice().into());
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

/// Cloud Storage documents no limit on CORS rules, so only the input's own bound (a request body
/// or a gRPC message) limits how many arrive.
pub(crate) fn validate_cors(rules: &[CorsRule]) -> Result<(), CatalogError> {
    for rule in rules {
        if !rule.origin.iter().all(|origin| valid_cors_origin(origin))
            || !rule.method.iter().all(|method| valid_cors_method(method))
            || !rule
                .response_header
                .iter()
                .all(|name| valid_cors_response_header(name))
            || rule.max_age_seconds.is_some_and(|seconds| seconds < 0)
        {
            return Err(CatalogError::InvalidMetadata(
                "Invalid bucket CORS configuration".into(),
            ));
        }
    }
    Ok(())
}

/// An origin a CORS rule may list: visible ASCII that a response header value can carry.
pub(crate) fn valid_cors_origin(origin: &str) -> bool {
    !origin.is_empty()
        && origin.is_ascii()
        && !origin
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        && hyper::header::HeaderValue::from_str(origin).is_ok()
}

pub(crate) fn valid_cors_method(method: &str) -> bool {
    method == "*" || hyper::Method::from_bytes(method.as_bytes()).is_ok()
}

pub(crate) fn valid_cors_response_header(name: &str) -> bool {
    name == "*" || hyper::header::HeaderName::from_bytes(name.as_bytes()).is_ok()
}

/// The most labels a bucket can have, as Cloud Storage documents.
pub(crate) const MAX_LABELS: usize = 64;

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
    StorageClass::from_name(class).map_err(|error| match error {
        InvalidStorageClass::Unknown => CatalogError::Metadata(
            crate::metadata_error::MetadataError::InvalidUpdateStorageClass(class.into()),
        ),
        InvalidStorageClass::RequiresZone => error.into(),
    })
}

/// Cloud Storage's rejection of a soft delete retention outside its range (recorded 2026-11). The
/// lower bound it names is not enforced locally; see [`SoftDeleteRetention`].
pub(crate) const SOFT_DELETE_RANGE_MESSAGE: &str =
    "Soft delete policy must have a retention duration between 3 days and 90 days.";
/// The longest soft delete retention, 90 days in seconds, which Cloud Storage accepts inclusively.
pub(crate) const MAX_SOFT_DELETE_RETENTION_SECONDS: u64 = 7_776_000;
pub(crate) const RETENTION_RANGE_MESSAGE: &str =
    "Retention policy must have a retention period greater than 0 and less than 100 years.";

/// A soft delete retention the emulator accepts, in seconds. Zero is a setting of its own: the
/// API stores and returns it, and it means deletions are permanent.
///
/// Cloud Storage accepts only 0 or 7 to 90 days. The emulator deliberately accepts any shorter
/// duration too, so local tests can watch deleted data expire without waiting a week. The 90-day
/// ceiling matches Cloud Storage, which rejected 7776001 seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SoftDeleteRetention(u64);

impl SoftDeleteRetention {
    pub(crate) fn seconds(self) -> u64 {
        self.0
    }

    /// The retention to keep deletions for, or `None` when they are permanent.
    pub(crate) fn enabled(self) -> Option<std::num::NonZeroU64> {
        std::num::NonZeroU64::new(self.0)
    }
}

impl TryFrom<i64> for SoftDeleteRetention {
    type Error = CatalogError;

    fn try_from(seconds: i64) -> Result<Self, CatalogError> {
        u64::try_from(seconds)
            .ok()
            .filter(|seconds| *seconds <= MAX_SOFT_DELETE_RETENTION_SECONDS)
            .map(Self)
            .ok_or_else(|| CatalogError::InvalidMetadata(SOFT_DELETE_RANGE_MESSAGE.into()))
    }
}

/// A bucket retention period Cloud Storage accepts, in seconds: more than zero and less than
/// 100 years of 365.25 days.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RetentionPeriod(std::num::NonZeroU64);

impl RetentionPeriod {
    /// The exclusive upper bound, 100 years of 365.25 days.
    pub(crate) const LIMIT_SECONDS: u64 = 3_155_760_000;

    pub(crate) fn seconds(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<i64> for RetentionPeriod {
    type Error = CatalogError;

    fn try_from(seconds: i64) -> Result<Self, CatalogError> {
        u64::try_from(seconds)
            .ok()
            .filter(|seconds| *seconds < Self::LIMIT_SECONDS)
            .and_then(std::num::NonZeroU64::new)
            .map(Self)
            .ok_or_else(|| CatalogError::InvalidMetadata(RETENTION_RANGE_MESSAGE.into()))
    }
}

crate::choice::string_enum! {
    table
    /// A bucket's own public access prevention setting. The parent project can still prevent public
    /// access when this is `Inherited`; `crate::auth::PublicAccess` is the effective result.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) enum PublicAccessPrevention as pub(crate) as_str {
        #[default]
        Inherited => "inherited",
        Enforced => "enforced",
    };
    from_str(CatalogError, value => Err(CatalogError::InvalidMetadata(format!(
        "Invalid value for: {value} is not a valid value"
    ))))
}

#[derive(Debug, PartialEq, Eq)]
pub enum CatalogError {
    AlreadyExists,
    NotFound,
    NotEmpty,
    /// A soft-deleted bucket cannot be restored because a live bucket has its name.
    RestoreConflict,
    PreconditionFailed,
    /// A deletion's `ifMetagenerationNotMatch` named the bucket's current metageneration while
    /// its match condition held; nothing was deleted. The JSON, XML and legacy gRPC APIs answer
    /// it as [`Self::PreconditionFailed`], while Storage v2 DeleteBucket words it as a resource
    /// that has not been modified (recorded 2026-11-18).
    NotModified,
    PublicAccessPrevention,
    /// Strict authorization refused a Cloud Storage permission, including when the bucket's
    /// current owning project is not one the caller was authorized for.
    Denied(Box<crate::auth::Denied>),
    /// Refused for a reason no Cloud Storage permission names: an access authorized for another
    /// permission than the operation needs, or an object refusal of that kind, such as an upload
    /// session whose bucket was recreated.
    Forbidden,
    RetentionPolicyLocked(u64),
    RetentionPolicyNotSet,
    InvalidMetadata(String),
    Metadata(crate::metadata_error::MetadataError),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_reports_other_for_a_full_catalog() {
        let close = CatalogError::from(std::io::Error::from(std::io::ErrorKind::StorageFull));
        let diagnostic =
            crate::diagnostics::combine_shutdown_results(Ok(()), Err(close)).unwrap_err();
        assert_eq!(
            diagnostic.errors[0].fix,
            "Cannot close catalog (Other): catalog I/O: no storage space; inspect persistence before restarting."
        );
    }

    #[test]
    fn storage_class_spellings_and_json_are_stable() {
        let spellings = [
            "STANDARD",
            "NEARLINE",
            "COLDLINE",
            "ARCHIVE",
            "MULTI_REGIONAL",
            "REGIONAL",
            "DURABLE_REDUCED_AVAILABILITY",
        ];
        assert_eq!(StorageClass::ALL.len(), spellings.len());
        for (&class, spelling) in StorageClass::ALL.iter().zip(spellings) {
            assert_eq!(spelling.parse::<StorageClass>().unwrap(), class);
            let json = format!("{spelling:?}");
            assert_eq!(serde_json::to_string(&class).unwrap(), json);
            assert_eq!(serde_json::from_str::<StorageClass>(&json).unwrap(), class);
            assert_eq!(
                spelling
                    .to_ascii_lowercase()
                    .parse::<StorageClass>()
                    .unwrap_err()
                    .to_string(),
                "The storage class you specified is not valid."
            );
        }
        assert_eq!(
            "RAPID".parse::<StorageClass>().unwrap_err().to_string(),
            "storageClass RAPID requires a zonal bucket"
        );
    }

    #[test]
    fn bucket_setting_spellings_are_exact() {
        for (setting, spelling) in [
            (PublicAccessPrevention::Inherited, "inherited"),
            (PublicAccessPrevention::Enforced, "enforced"),
        ] {
            assert_eq!(setting.as_str(), spelling);
            assert_eq!(spelling.parse::<PublicAccessPrevention>().unwrap(), setting);
        }
        assert_eq!(
            "ENFORCED"
                .parse::<PublicAccessPrevention>()
                .unwrap_err()
                .to_string(),
            "Invalid value for: ENFORCED is not a valid value"
        );
        for (rpo, spelling) in [(Rpo::Default, "DEFAULT"), (Rpo::AsyncTurbo, "ASYNC_TURBO")] {
            assert_eq!(rpo.as_str(), spelling);
            assert_eq!(spelling.parse::<Rpo>().unwrap(), rpo);
        }
        assert_eq!("default".parse::<Rpo>(), Err(UnknownRpo));
    }

    #[test]
    fn uniform_access_precheck_honors_stored_and_requested_policy() {
        let now = SystemTime::UNIX_EPOCH;
        let mut bucket = BucketRecord::new("bucket".into(), "project", Location::US, now);
        for (requested, disabled_bucket_result) in [
            (None, UniformAccess::Disabled),
            (
                Some(UniformAccessRequest::Set(false)),
                UniformAccess::Disabled,
            ),
            (
                Some(UniformAccessRequest::Set(true)),
                UniformAccess::Enabled,
            ),
            (
                Some(UniformAccessRequest::Conflicting),
                UniformAccess::Enabled,
            ),
        ] {
            bucket.uniform_bucket_level_access_locked_time = None;
            assert_eq!(bucket.uniform_access(requested), disabled_bucket_result);
            bucket.uniform_bucket_level_access_locked_time = Some(now);
            assert_eq!(bucket.uniform_access(requested), UniformAccess::Enabled);
        }
    }

    /// Both enums count the same two refusals, and the conversion between them keeps that.
    #[test]
    fn refusals_are_permission_denials_and_unnamed_forbidden_errors() {
        use crate::object::ObjectError;
        let denied = || {
            crate::auth::Denied::new(
                None,
                crate::auth::OBJECTS_GET,
                crate::auth::denied::Resource::in_bucket(crate::auth::OBJECTS_GET, "bucket"),
            )
        };
        for refusal in [CatalogError::Denied(denied()), CatalogError::Forbidden] {
            assert!(refusal.is_refusal(), "{refusal:?}");
            assert!(ObjectError::from(refusal).is_refusal());
        }
        for other in [
            CatalogError::PublicAccessPrevention,
            CatalogError::NotFound,
            CatalogError::PreconditionFailed,
        ] {
            assert!(!other.is_refusal(), "{other:?}");
            assert!(!ObjectError::from(other).is_refusal());
        }
        assert!(!ObjectError::OverwriteForbidden(denied()).is_refusal());
    }

    #[test]
    fn retention_period_accepts_one_second_to_just_under_a_century() {
        let limit = i64::try_from(RetentionPeriod::LIMIT_SECONDS).unwrap();
        for seconds in [1, 3600, limit - 1] {
            assert_eq!(
                RetentionPeriod::try_from(seconds).unwrap().seconds(),
                u64::try_from(seconds).unwrap()
            );
        }
        for seconds in [i64::MIN, -1, 0, limit, i64::MAX] {
            assert_eq!(
                RetentionPeriod::try_from(seconds),
                Err(CatalogError::InvalidMetadata(
                    RETENTION_RANGE_MESSAGE.into()
                )),
                "{seconds}"
            );
        }
    }

    /// Each setting keeps its own pair of stored columns. A bucket that enabled versioning before
    /// the presence column existed stored `(1, 0)` and reads as enabled.
    #[test]
    fn versioning_columns_round_trip_and_updates_follow_the_current_setting() {
        use Versioning::{Disabled, Enabled, Unset};
        for (versioning, columns) in [
            (Unset, (false, false)),
            (Disabled, (false, true)),
            (Enabled, (true, true)),
        ] {
            assert_eq!(versioning.columns(), columns);
            assert_eq!(Versioning::from_columns(columns), versioning);
        }
        assert_eq!(Versioning::from_columns((true, false)), Enabled);
        assert_eq!(Versioning::from(None), Unset);
        assert_eq!(Versioning::from(Some(false)), Disabled);
        assert_eq!(Versioning::from(Some(true)), Enabled);
        assert!(Enabled.is_enabled() && !Disabled.is_enabled() && !Unset.is_enabled());
        for (current, disabled) in [(Unset, Unset), (Disabled, Disabled), (Enabled, Disabled)] {
            assert_eq!(VersioningUpdate::Disable.apply(current), disabled);
            for wanted in [Unset, Disabled, Enabled] {
                assert_eq!(VersioningUpdate::Set(wanted).apply(current), wanted);
            }
        }
    }

    /// Re-scoping changes only the permission a policy access checks; it never gives
    /// permissive or administrative access a permission to check.
    #[test]
    fn with_permission_rescopes_only_policy_access() {
        use crate::auth::Permission;
        let identity = crate::auth::Identity::from_upload_snapshot(
            &serde_json::json!({"id": "writer@example.invalid", "projects": {}}),
        )
        .unwrap();
        for (identity, project) in [(Some(identity), Some("project".to_owned())), (None, None)] {
            let access = BucketAccess::Policy {
                identity: identity.clone(),
                permission: Permission::ObjectsCreate,
                project: project.clone(),
            };
            assert_eq!(
                access.with_permission(Permission::ObjectsDelete),
                BucketAccess::Policy {
                    identity,
                    permission: Permission::ObjectsDelete,
                    project,
                }
            );
        }
        let admin = BucketAccess::Admin(crate::storage::admin::AdminGrant::seed());
        for access in [BucketAccess::Permissive, admin] {
            assert_eq!(access.with_permission(Permission::BucketsSetIam), access);
        }
    }

    #[test]
    fn cors_accepts_rule_counts_and_sizes_above_the_former_64_kib_limit() {
        let long_values = CorsRule {
            origin: vec!["x".repeat(128 * 1024)],
            method: vec!["GET".into(); 33 * 1024],
            response_header: vec!["x-test".into(); 33 * 1024],
            ..Default::default()
        };
        assert!(validate_cors(&[long_values]).is_ok());
        let small = CorsRule {
            origin: vec!["*".into()],
            ..Default::default()
        };
        assert!(validate_cors(&vec![small; 64 * 1024]).is_ok());
        // Removing the size checks leaves each value's own rules in force.
        let invalid = CorsRule {
            origin: vec!["bad origin".into()],
            ..Default::default()
        };
        assert!(validate_cors(&[invalid]).is_err());
    }

    #[test]
    fn storage_class_validation_keeps_exact_errors() {
        for class in [
            "STANDARD",
            "NEARLINE",
            "COLDLINE",
            "ARCHIVE",
            "MULTI_REGIONAL",
            "REGIONAL",
            "DURABLE_REDUCED_AVAILABILITY",
        ] {
            assert!(validate_storage_class(class).is_ok(), "{class}");
        }
        assert_eq!(
            validate_storage_class("RAPID"),
            Err(CatalogError::Metadata(
                crate::metadata_error::MetadataError::StorageClassRequiresZone
            ))
        );
    }

    #[test]
    fn legacy_classes_fit_only_their_location_type() {
        let [region, dual, multi] = ["US-CENTRAL1", "NAM4", "US"]
            .map(|name| name.parse::<Location>().expect("an official location"));
        for (class, fitting) in [
            (StorageClass::Regional, vec![region]),
            (StorageClass::MultiRegional, vec![dual, multi]),
            (StorageClass::Standard, vec![region, dual, multi]),
            (
                StorageClass::DurableReducedAvailability,
                vec![region, dual, multi],
            ),
        ] {
            for location in [region, dual, multi] {
                assert_eq!(
                    ClassLocationConflict::check(class, location, "local").is_ok(),
                    fitting.contains(&location),
                    "{class:?} in {}",
                    location.as_str()
                );
            }
        }
        // A numeric project ID is its own number; any other ID has a derived one.
        for (project, number) in [("local", "902648055030"), ("123456", "123456")] {
            assert_eq!(
                ClassLocationConflict::check(StorageClass::Regional, multi, project)
                    .unwrap_err()
                    .to_string(),
                format!(
                    "Project {number} may not create storageClass REGIONAL buckets with locationConstraint US."
                )
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
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists => formatter.write_str("bucket already exists"),
            Self::NotFound => formatter.write_str("bucket not found"),
            Self::NotEmpty => formatter.write_str("bucket is not empty"),
            Self::RestoreConflict => formatter
                .write_str("a bucket with this name exists, so the soft-deleted one cannot be restored"),
            Self::PreconditionFailed => formatter.write_str("precondition failed"),
            Self::NotModified => formatter.write_str("not modified"),
            Self::PublicAccessPrevention => {
                formatter.write_str("public access prevention is enforced")
            }
            Self::Denied(denied) => denied.fmt(formatter),
            Self::Forbidden => formatter.write_str("forbidden"),
            Self::RetentionPolicyLocked(period) => write!(
                formatter,
                "Bucket retention policy is locked and cannot be removed or shortened; keep retentionPeriod at least {period} seconds or extend it, for example {{\"retentionPolicy\":{{\"retentionPeriod\":\"{}\"}}}}",
                period.saturating_add(1).min(RetentionPeriod::LIMIT_SECONDS - 1)
            ),
            Self::RetentionPolicyNotSet => formatter.write_str("No retention policy to lock; set retentionPolicy.retentionPeriod first, for example 3600"),
            Self::Io(kind) => write!(formatter, "catalog I/O: {kind}"),
            Self::Corrupt(reason) => write!(formatter, "catalog corrupt: {reason}"),
            Self::NewerSchema { version, supported } => write!(
                formatter,
                "unsupported catalog schema version {version}; this release supports schema {supported} and older"
            ),
            Self::Metadata(error) => error.fmt(formatter),
            Self::InvalidMetadata(reason) | Self::UnsupportedMetadata(reason) => {
                formatter.write_str(reason)
            }
            Self::Unavailable(reason) => write!(formatter, "catalog unavailable: {reason}"),
        }
    }
}

impl CatalogError {
    /// An authorization refusal, whether or not it names a Cloud Storage permission. A
    /// caller that turns a refusal into a hidden result tests this, so it cannot handle one
    /// kind and let the other escape as an error.
    pub(crate) fn is_refusal(&self) -> bool {
        matches!(self, Self::Denied(_) | Self::Forbidden)
    }
}

// Startup and shutdown report catalog operations as Other even when the underlying I/O
// has a more specific kind. Changing it would change the operator diagnostic.
impl crate::diagnostics::RuntimeFailure for CatalogError {
    fn kind(&self) -> std::io::ErrorKind {
        std::io::ErrorKind::Other
    }
}

impl std::error::Error for CatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.typed_source().map(Link::as_error)
    }
}

impl TypedSource for CatalogError {
    fn typed_source(&self) -> Option<Link<'_>> {
        match self {
            Self::Io(cause) => cause.typed_source(),
            Self::Corrupt(cause) | Self::Unavailable(cause) => cause.typed_source(),
            Self::Metadata(error) => Link::Leaf(error).source(),
            Self::AlreadyExists
            | Self::NotFound
            | Self::NotEmpty
            | Self::RestoreConflict
            | Self::PreconditionFailed
            | Self::NotModified
            | Self::PublicAccessPrevention
            | Self::Denied(_)
            | Self::Forbidden
            | Self::RetentionPolicyLocked(_)
            | Self::RetentionPolicyNotSet
            | Self::InvalidMetadata(_)
            | Self::UnsupportedMetadata(_)
            | Self::NewerSchema { .. } => None,
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
        self.check(metageneration).is_ok()
    }

    /// Compares both optional preconditions with `metageneration`. A failing match is a failed
    /// precondition even beside a failing not-match; a not-match that alone fails, naming
    /// `metageneration`, is [`CatalogError::NotModified`].
    pub(crate) fn check(self, metageneration: i64) -> Result<(), CatalogError> {
        if self
            .if_metageneration_match
            .is_some_and(|value| value != metageneration)
        {
            return Err(CatalogError::PreconditionFailed);
        }
        if self.if_metageneration_not_match == Some(metageneration) {
            return Err(CatalogError::NotModified);
        }
        Ok(())
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

impl BucketAccess {
    /// The caller and optional observed project to check inside the catalog snapshot.
    pub(crate) fn for_caller(
        caller: Option<crate::auth::Caller<'_>>,
        permission: crate::auth::Permission,
        project: Option<&str>,
    ) -> Self {
        match caller {
            None => Self::Permissive,
            Some(caller) => Self::Policy {
                identity: match caller {
                    crate::auth::Caller::Principal(identity) => Some(identity.clone()),
                    crate::auth::Caller::Anonymous => None,
                },
                permission,
                project: project.map(str::to_owned),
            },
        }
    }

    /// This access re-scoped to `permission`: a policy access checks `permission` for the same
    /// principal and observed project, while permissive and administrative access stay as they
    /// are, because no Cloud Storage permission applies to them.
    pub(crate) fn with_permission(&self, permission: crate::auth::Permission) -> Self {
        match self {
            Self::Policy {
                identity, project, ..
            } => Self::Policy {
                identity: identity.clone(),
                permission,
                project: project.clone(),
            },
            Self::Permissive | Self::Admin(_) => self.clone(),
        }
    }

    /// A strict-profile caller without credentials. Permissive and administrative access are
    /// not anonymous: they bypass authorization instead of lacking an identity.
    pub(crate) fn is_anonymous(&self) -> bool {
        match self {
            Self::Policy { identity, .. } => identity.is_none(),
            Self::Permissive | Self::Admin(_) => false,
        }
    }
}

#[cfg(test)]
impl BucketAccess {
    /// The refusal of this access's own permission in `bucket`.
    #[track_caller]
    pub(crate) fn denied(&self, bucket: &str) -> Box<crate::auth::Denied> {
        let Self::Policy { permission, .. } = self else {
            panic!("only a policy access names a refused permission: {self:?}");
        };
        self.denied_for(*permission, bucket)
    }

    /// The refusal of `permission` in `bucket` for this access's principal.
    #[track_caller]
    pub(crate) fn denied_for(
        &self,
        permission: crate::auth::Permission,
        bucket: &str,
    ) -> Box<crate::auth::Denied> {
        let Self::Policy { identity, .. } = self else {
            panic!("only a policy access names a refused permission: {self:?}");
        };
        crate::auth::Denied::new(
            identity.as_ref(),
            permission,
            crate::auth::denied::Resource::in_bucket(permission, bucket),
        )
    }
}
