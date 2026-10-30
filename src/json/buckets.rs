use super::{JsonError, Projection, WriteMode, query::validate_project};
use crate::bucket::{
    BucketMetadataUpdate, BucketRecord, CorsRule, UNIFORM_ACCESS_LOCK_PERIOD, bucket_location,
    cors_from_value, cors_value, valid_label, validate_storage_class,
};
use crate::config::CompatProfile;
use hyper::StatusCode;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

const LEGACY_LOCATION: &str = "US-CENTRAL1";

pub(super) const UNSUPPORTED_FIELDS: &[&str] = &[
    "acl",
    "billing",
    "customPlacementConfig",
    "defaultObjectAcl",
    "encryption",
    "hierarchicalNamespace",
    "ipFilter",
    "logging",
    "rpo",
    "satisfiesPZI",
    "satisfiesPZS",
    "website",
];

pub(crate) fn validate_bucket_name(name: &str) -> Result<(), JsonError> {
    let invalid = || JsonError::invalid_parameter("Invalid bucket name");
    let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if name.len() < 3
        || name.len() > 222
        || (!name.contains('.') && name.len() > 63)
        || !name
            .bytes()
            .all(|byte| alphanumeric(byte) || matches!(byte, b'-' | b'_' | b'.'))
        || !name.bytes().next().is_some_and(alphanumeric)
        || !name.bytes().last().is_some_and(alphanumeric)
        || name
            .split('.')
            .any(|component| component.is_empty() || component.len() > 63)
        || name.starts_with("goog")
        || name.replace('0', "o").contains("google")
    {
        return Err(invalid());
    }
    let parts: Vec<_> = name.split('.').collect();
    if parts.len() == 4
        && parts.iter().all(|part| {
            part.bytes().all(|byte| byte.is_ascii_digit()) && part.parse::<u8>().is_ok()
        })
    {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn validate_bucket_name_for_profile(
    name: &str,
    profile: CompatProfile,
) -> Result<(), JsonError> {
    if profile == CompatProfile::Native {
        return validate_bucket_name(name);
    }
    let edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if (2..=666).contains(&name.len())
        && name.bytes().next().is_some_and(edge)
        && name.bytes().last().is_some_and(edge)
        && name
            .bytes()
            .all(|byte| edge(byte) || b"._-".contains(&byte))
    {
        Ok(())
    } else {
        Err(JsonError::invalid_argument("invalid bucket name"))
    }
}

/// Parse the native bucket metadata subset.
pub fn parse_insert(
    bytes: &[u8],
    project: &str,
    now: SystemTime,
) -> Result<BucketRecord, JsonError> {
    parse_insert_with_profile(bytes, project, now, CompatProfile::Native, "US")
}

/// fake-gcs-server reads only the first JSON value of an insert body and ignores the rest.
fn parse_insert_json(bytes: &[u8], profile: CompatProfile) -> Result<Value, JsonError> {
    let parsed = if profile == CompatProfile::LegacyEmulator {
        serde_json::Deserializer::from_slice(bytes)
            .into_iter::<Value>()
            .next()
            .unwrap_or_else(|| serde_json::from_slice(bytes))
    } else {
        serde_json::from_slice(bytes)
    };
    parsed.map_err(|_| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "parseError",
            "Invalid JSON bucket resource",
        )
    })
}

fn reject_unsupported_insert_fields(
    body: &serde_json::Map<String, Value>,
    profile: CompatProfile,
) -> Result<(), JsonError> {
    for field in UNSUPPORTED_FIELDS {
        if body.contains_key(*field) {
            if profile == CompatProfile::Native
                && *field == "customPlacementConfig"
                && body.get(*field).is_some_and(|value| {
                    value.is_null() || value == &json!({}) || value == &json!({"dataLocations": []})
                })
            {
                continue;
            }
            return Err(JsonError::unsupported(field));
        }
    }
    Ok(())
}

/// A string field of an insert body; `default` makes the field optional.
fn insert_string(
    body: &serde_json::Map<String, Value>,
    field: &str,
    default: Option<&str>,
) -> Result<String, JsonError> {
    match body.get(field) {
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| JsonError::invalid_argument(format!("{field} must be a string"))),
        None => default.map(str::to_owned).ok_or_else(|| {
            JsonError::new(
                StatusCode::BAD_REQUEST,
                "required",
                format!("Missing required {field}"),
            )
        }),
    }
}

/// A boolean field of an insert body; an absent field, or a legacy null, is false.
fn insert_bool(
    value: Option<&Value>,
    field: &str,
    profile: CompatProfile,
) -> Result<bool, JsonError> {
    match value {
        None => Ok(false),
        Some(Value::Null) if profile == CompatProfile::LegacyEmulator => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| JsonError::invalid_argument(format!("{field} must be a boolean"))),
    }
}

fn insert_location(
    body: &serde_json::Map<String, Value>,
    profile: CompatProfile,
    default_location: &str,
) -> Result<String, JsonError> {
    if profile == CompatProfile::LegacyEmulator {
        if body.get("location").is_some_and(|value| {
            !value
                .as_str()
                .is_some_and(|location| location.eq_ignore_ascii_case(LEGACY_LOCATION))
        }) {
            return Err(JsonError::unsupported("location"));
        }
        return Ok("US".into());
    }
    if body
        .get("location")
        .is_none_or(|value| value.is_null() || value == "")
    {
        return Ok(default_location.to_owned());
    }
    insert_string(body, "location", Some(default_location))
}

/// The legacy profile ignores labels on insert.
fn insert_labels(
    body: &serde_json::Map<String, Value>,
    profile: CompatProfile,
) -> Result<BTreeMap<String, String>, JsonError> {
    let mut labels = BTreeMap::new();
    let Some(value) = body
        .get("labels")
        .filter(|_| profile == CompatProfile::Native)
    else {
        return Ok(labels);
    };
    let object = value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument("labels must be a string map"))?;
    if object.len() > 64 {
        return Err(JsonError::invalid_argument(
            "A bucket can have at most 64 labels",
        ));
    }
    for (key, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| JsonError::invalid_argument("Label values must be strings"))?;
        if !valid_label(key, crate::bucket::LabelPart::Key)
            || !valid_label(value, crate::bucket::LabelPart::Value)
        {
            return Err(JsonError::invalid_argument("Invalid bucket label"));
        }
        labels.insert(key.clone(), value.to_owned());
    }
    Ok(labels)
}

pub(crate) fn parse_insert_with_profile(
    bytes: &[u8],
    project: &str,
    now: SystemTime,
    profile: CompatProfile,
    default_location: &str,
) -> Result<BucketRecord, JsonError> {
    validate_project(project)?;
    let value = parse_insert_json(bytes, profile)?;
    let body = value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument("Bucket resource must be a JSON object"))?;
    reject_unsupported_insert_fields(body, profile)?;
    let name = if profile == CompatProfile::LegacyEmulator
        && body.get("name").is_none_or(Value::is_null)
    {
        String::new()
    } else {
        insert_string(body, "name", None)?
    };
    validate_bucket_name_for_profile(&name, profile).map_err(|_| {
        JsonError::invalid_argument(if profile == CompatProfile::Native {
            "Invalid bucket name"
        } else {
            "invalid bucket name"
        })
    })?;
    let versioning_enabled = match body.get("versioning") {
        None => false,
        Some(Value::Null) if profile == CompatProfile::LegacyEmulator => false,
        Some(value) => {
            let versioning = value
                .as_object()
                .ok_or_else(|| JsonError::invalid_argument("versioning must be an object"))?;
            insert_bool(versioning.get("enabled"), "versioning.enabled", profile)?
        }
    };
    let default_event_based_hold = insert_bool(
        body.get("defaultEventBasedHold"),
        "defaultEventBasedHold",
        profile,
    )?;
    let location = insert_location(body, profile, default_location)?;
    let (location, location_type) = bucket_location(&location)?;
    let storage_class = if profile == CompatProfile::LegacyEmulator {
        "STANDARD".into()
    } else {
        insert_string(body, "storageClass", Some("STANDARD"))?
    };
    validate_storage_class(&storage_class)?;
    // Ruby sends disabled Autoclass even when no option was supplied; that stays unconfigured.
    let autoclass = body
        .get("autoclass")
        .map(crate::bucket::autoclass::parse_json)
        .transpose()?
        .map(|update| crate::bucket::autoclass::apply(None, &update, now))
        .transpose()?
        .and_then(|(config, _)| config);
    let labels = insert_labels(body, profile)?;
    timestamp(now)?;
    Ok(BucketRecord {
        soft_delete_policy: parse_soft_delete_retention(body.get("softDeletePolicy"))?.map(
            |retention_duration_seconds| crate::bucket::SoftDeletePolicy {
                retention_duration_seconds,
                effective_time: now,
            },
        ),
        retention_policy: parse_retention_period(body.get("retentionPolicy"))?
            .flatten()
            .map(|retention_period| crate::bucket::RetentionPolicy {
                retention_period,
                effective_time: now,
                is_locked: false,
            }),
        // The adapter reconciles an echoed value with enableObjectRetention.
        object_retention_enabled: super::object_retention::parse_bucket(
            body.get("objectRetention"),
        )?
        .unwrap_or(false),
        name,
        project: project.to_owned(),
        location: location.to_owned(),
        location_type: location_type.to_owned(),
        storage_class,
        autoclass,
        labels,
        cors: parse_cors(body.get("cors"))?.unwrap_or_default(),
        lifecycle: body
            .get("lifecycle")
            .map(crate::bucket::lifecycle::parse)
            .transpose()?
            .unwrap_or_default(),
        versioning_enabled,
        default_event_based_hold,
        public_access_prevention_enforced: parse_public_access_prevention(
            body.get("iamConfiguration"),
        )?
        .unwrap_or(false),
        uniform_bucket_level_access_locked_time: if parse_uniform_access(
            body.get("iamConfiguration"),
        )?
        .unwrap_or(false)
        {
            Some(now.checked_add(UNIFORM_ACCESS_LOCK_PERIOD).ok_or_else(|| {
                JsonError::invalid_argument(
                    "Uniform access deadline is outside the supported time range",
                )
            })?)
        } else {
            None
        },
        metageneration: 1,
        time_created: now,
        updated: now,
    })
}

pub(crate) fn parse_bucket_patch(
    bytes: &[u8],
    profile: CompatProfile,
) -> Result<BucketMetadataUpdate, JsonError> {
    parse_bucket_update(bytes, profile, WriteMode::Patch)
}

pub(crate) fn parse_bucket_replacement(
    bytes: &[u8],
    profile: CompatProfile,
) -> Result<BucketMetadataUpdate, JsonError> {
    parse_bucket_update(bytes, profile, WriteMode::Replace)
}

/// Labels of a bucket update, where `None` clears every label. A patch removes a label with a
/// null value; a replacement requires string values and is limited to 64 labels.
fn update_labels(
    value: &Value,
    mode: WriteMode,
) -> Result<Option<BTreeMap<String, Option<String>>>, JsonError> {
    if value.is_null() {
        return Ok(None);
    }
    let replace = mode == WriteMode::Replace;
    let labels = value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument("labels must be an object or null"))?;
    if replace && labels.len() > 64 {
        return Err(JsonError::invalid_argument(
            "A bucket can have at most 64 labels",
        ));
    }
    labels
        .iter()
        .map(|(key, value)| {
            let value = if value.is_null() && !replace {
                None
            } else {
                Some(
                    value
                        .as_str()
                        .ok_or_else(|| {
                            JsonError::invalid_argument(if replace {
                                "Label values must be strings"
                            } else {
                                "Label values must be strings or null"
                            })
                        })?
                        .to_owned(),
                )
            };
            if !valid_label(key, crate::bucket::LabelPart::Key)
                || value
                    .as_deref()
                    .is_some_and(|value| !valid_label(value, crate::bucket::LabelPart::Value))
            {
                return Err(JsonError::invalid_argument("Invalid bucket label"));
            }
            Ok((key.clone(), value))
        })
        .collect::<Result<_, JsonError>>()
        .map(Some)
}

fn parse_bucket_update(
    bytes: &[u8],
    profile: CompatProfile,
    mode: WriteMode,
) -> Result<BucketMetadataUpdate, JsonError> {
    let replace = mode == WriteMode::Replace;
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "parseError",
            "Invalid JSON bucket resource",
        )
    })?;
    let body = value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument("Bucket resource must be a JSON object"))?;
    for field in UNSUPPORTED_FIELDS {
        if body.contains_key(*field) {
            if matches!(*field, "acl" | "defaultObjectAcl") {
                continue;
            }
            return Err(JsonError::unsupported(field));
        }
    }
    // shortcut: legacy JSON unknown/output-only handling needs service evidence; use insert's ignored-input rule.
    let string = |field: &str| {
        body.get(field)
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| JsonError::invalid_argument(format!("{field} must be a string")))
            })
            .transpose()
    };
    let boolean = |value: Option<&Value>, field: &str| {
        value
            .map(|value| {
                if value.is_null() && profile == CompatProfile::LegacyEmulator {
                    return Ok(false);
                }
                value.as_bool().ok_or_else(|| {
                    JsonError::invalid_argument(format!("{field} must be a boolean"))
                })
            })
            .transpose()
    };
    let storage_class = string("storageClass")?;
    if let Some(class) = &storage_class {
        validate_storage_class(class)?;
    }
    let labels = body
        .get("labels")
        .map(|value| update_labels(value, mode))
        .transpose()?;
    let versioning_enabled = body
        .get("versioning")
        .map(|value| {
            if value.is_null() && profile == CompatProfile::LegacyEmulator {
                return Ok(Some(false));
            }
            let versioning = value
                .as_object()
                .ok_or_else(|| JsonError::invalid_argument("versioning must be an object"))?;
            boolean(versioning.get("enabled"), "versioning.enabled")
                .map(|enabled| enabled.or(replace.then_some(false)))
        })
        .transpose()?
        .flatten();
    let mut location = string("location")?;
    if profile == CompatProfile::LegacyEmulator {
        if location
            .as_ref()
            .is_some_and(|location| !location.eq_ignore_ascii_case(LEGACY_LOCATION))
        {
            return Err(JsonError::unsupported("location"));
        }
        // The profile exposes a fixed location independently of the stored placement.
        location = None;
    }
    Ok(BucketMetadataUpdate {
        acl: body
            .get("acl")
            .map(|value| super::acl::parse_entries(value, crate::acl::AclKind::Bucket))
            .transpose()?,
        default_object_acl: body
            .get("defaultObjectAcl")
            .map(|value| super::acl::parse_entries(value, crate::acl::AclKind::DefaultObject))
            .transpose()?,
        soft_delete_retention: parse_soft_delete_retention(body.get("softDeletePolicy"))?,
        retention_period: parse_retention_period(body.get("retentionPolicy"))?,
        lock_retention_policy: false,
        object_retention_enabled: super::object_retention::parse_bucket(
            body.get("objectRetention"),
        )?,
        cors: parse_cors(body.get("cors"))?,
        lifecycle: body
            .get("lifecycle")
            .map(crate::bucket::lifecycle::parse)
            .transpose()?,
        labels,
        storage_class,
        autoclass: body
            .get("autoclass")
            .map(crate::bucket::autoclass::parse_json)
            .transpose()?,
        versioning_enabled,
        default_event_based_hold: boolean(
            body.get("defaultEventBasedHold"),
            "defaultEventBasedHold",
        )?,
        location,
        public_access_prevention_enforced: parse_public_access_prevention(
            body.get("iamConfiguration"),
        )?,
        uniform_bucket_level_access_enabled: parse_uniform_access(body.get("iamConfiguration"))?,
    })
}

fn parse_soft_delete_retention(value: Option<&Value>) -> Result<Option<u64>, JsonError> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let invalid = || {
        JsonError::invalid_argument(
            "softDeletePolicy.retentionDurationSeconds must be 0 or integral seconds from 604800 to 7776000; use {\"retentionDurationSeconds\":\"604800\"}",
        )
    };
    let policy = value.as_object().ok_or_else(invalid)?;
    let Some(duration) = policy
        .get("retentionDurationSeconds")
        .filter(|v| !v.is_null())
    else {
        return Ok(None);
    };
    duration
        .as_u64()
        .or_else(|| duration.as_str().and_then(|v| v.parse().ok()))
        .filter(|v| *v == 0 || (604800..=7776000).contains(v))
        .map(Some)
        .ok_or_else(invalid)
}

fn parse_retention_period(value: Option<&Value>) -> Result<Option<Option<u64>>, JsonError> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() || value == &json!({}) {
        return Ok(Some(None));
    }
    let policy = value.as_object().ok_or_else(|| {
        JsonError::invalid_argument(
            "retentionPolicy must be an object or null; use {\"retentionPeriod\":\"3600\"}",
        )
    })?;
    let Some(period) = policy.get("retentionPeriod").filter(|v| !v.is_null()) else {
        return Ok(Some(None));
    };
    let period = period.as_u64().or_else(|| period.as_str().and_then(|v| v.parse().ok()))
        .filter(|v| (1..3_155_760_000).contains(v))
        .ok_or_else(|| JsonError::invalid_argument("retentionPeriod must be integral seconds greater than 0 and less than 3155760000; use \"3600\""))?;
    Ok(Some(Some(period)))
}

fn parse_uniform_access(value: Option<&Value>) -> Result<Option<bool>, JsonError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let config = value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument("iamConfiguration must be an object"))?;
    let mut enabled = None;
    for field in ["uniformBucketLevelAccess", "bucketPolicyOnly"] {
        if let Some(value) = config.get(field).filter(|value| !value.is_null()) {
            let setting = value.as_object().ok_or_else(|| {
                JsonError::invalid_argument(format!(
                    "{field} must be an object; use {{\"enabled\":true}}"
                ))
            })?;
            if let Some(value) = setting.get("enabled").filter(|value| !value.is_null()) {
                let value = value.as_bool().ok_or_else(|| {
                    JsonError::invalid_argument(format!(
                        "{field}.enabled must be a boolean; use true or false"
                    ))
                })?;
                // Ruby patches the modern field while retaining the response's legacy alias.
                enabled = enabled.or(Some(value));
            }
        }
    }
    Ok(enabled)
}

fn parse_public_access_prevention(value: Option<&Value>) -> Result<Option<bool>, JsonError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let config = value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument("iamConfiguration must be an object"))?;
    config
        .get("publicAccessPrevention")
        .map(|value| match value.as_str() {
            Some("enforced") => Ok(true),
            Some("inherited") => Ok(false),
            _ => Err(JsonError::invalid_argument(
                "publicAccessPrevention must be inherited or enforced",
            )),
        })
        .transpose()
}

fn parse_cors(value: Option<&Value>) -> Result<Option<Vec<CorsRule>>, JsonError> {
    value
        .map(|value| {
            let rules = if value.is_null() {
                Vec::new()
            } else {
                cors_from_value(value)?
            };
            Ok(rules)
        })
        .transpose()
}

/// Whether uniform bucket-level access governs the bucket a request targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UniformAccess {
    Enabled,
    Disabled,
}

impl UniformAccess {
    /// Enabled once the bucket records a lock time, or when the request itself enables it.
    pub(crate) fn of(locked_time: Option<SystemTime>, requested: Option<bool>) -> Self {
        if locked_time.is_some() || requested == Some(true) {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

/// The resource a predefined ACL query parameter would apply to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PredefinedAclTarget {
    Bucket,
    Object,
}

pub(crate) fn reject_predefined_acl(
    query: Option<&str>,
    uniform_access: UniformAccess,
    target: PredefinedAclTarget,
) -> Result<(), JsonError> {
    let query = super::parse_query(query.unwrap_or_default())?;
    for key in [
        "predefinedAcl",
        "predefinedDefaultObjectAcl",
        "destinationPredefinedAcl",
    ] {
        if query.contains_key(key) {
            return Err(match (uniform_access, target) {
                (UniformAccess::Enabled, _) => JsonError::invalid_argument(format!(
                    "{key} cannot be used with uniform bucket-level access; remove {key} (for example, omit {key}=publicRead) and use bucket IAM permissions"
                )),
                (UniformAccess::Disabled, PredefinedAclTarget::Object) => {
                    JsonError::unsupported_object(key)
                }
                (UniformAccess::Disabled, PredefinedAclTarget::Bucket) => {
                    JsonError::unsupported(key)
                }
            });
        }
    }
    Ok(())
}

/// Render supported metadata using a caller-validated advertised origin/base path.
#[cfg(test)]
pub(crate) fn bucket_resource(
    bucket: &BucketRecord,
    base_url: &str,
    projection: Projection,
) -> Result<Value, JsonError> {
    bucket_resource_with_profile(bucket, base_url, projection, CompatProfile::Native)
}

pub(crate) fn bucket_resource_with_profile(
    bucket: &BucketRecord,
    base_url: &str,
    projection: Projection,
    profile: CompatProfile,
) -> Result<Value, JsonError> {
    // ACL and owner fields are not modeled, so both projections expose the same local subset.
    let (Projection::NoAcl | Projection::Full) = projection;
    let project_number = if bucket.project.bytes().all(|byte| byte.is_ascii_digit()) {
        bucket.project.parse::<u64>().unwrap_or(0).to_string()
    } else {
        "0".to_owned()
    };
    let mut value = json!({
        "kind": "storage#bucket", "id": bucket.name, "name": bucket.name,
        "selfLink": format!("{}/storage/v1/b/{}", base_url.trim_end_matches('/'), encode_segment(&bucket.name)),
        "projectNumber": project_number, "metageneration": bucket.metageneration.to_string(),
        "timeCreated": timestamp(bucket.time_created)?, "updated": timestamp(bucket.updated)?,
        "etag": bucket_etag(bucket),
        "location": bucket.location, "locationType": bucket.location_type, "storageClass": bucket.storage_class,
        "iamConfiguration": {"publicAccessPrevention": if bucket.public_access_prevention_enforced { "enforced" } else { "inherited" }},
    });
    let mut uniform_access =
        json!({"enabled": bucket.uniform_bucket_level_access_locked_time.is_some()});
    if let Some(locked_time) = bucket.uniform_bucket_level_access_locked_time {
        uniform_access["lockedTime"] = timestamp(locked_time)?.into();
    }
    value["iamConfiguration"]["uniformBucketLevelAccess"] = uniform_access.clone();
    value["iamConfiguration"]["bucketPolicyOnly"] = uniform_access;
    if let Some(config) = &bucket.autoclass {
        let mut autoclass =
            json!({"enabled": config.enabled, "toggleTime": timestamp(config.toggle_time)?});
        if let Some(class) = &config.terminal_storage_class {
            autoclass["terminalStorageClass"] = class.as_str().into();
        }
        if let Some(time) = config.terminal_storage_class_update_time {
            autoclass["terminalStorageClassUpdateTime"] = timestamp(time)?.into();
        }
        value["autoclass"] = autoclass;
    }
    if !bucket.labels.is_empty() {
        value["labels"] = json!(bucket.labels);
    }
    if !bucket.cors.is_empty() {
        value["cors"] = cors_value(&bucket.cors);
    }
    if !bucket.lifecycle.is_empty() {
        value["lifecycle"] = json!({"rule": bucket.lifecycle});
    }
    if bucket.versioning_enabled || profile == CompatProfile::LegacyEmulator {
        value["versioning"] = json!({"enabled": bucket.versioning_enabled});
    }
    if let Some(policy) = &bucket.retention_policy {
        value["retentionPolicy"] = json!({"retentionPeriod":policy.retention_period.to_string(), "effectiveTime":timestamp(policy.effective_time)?, "isLocked":policy.is_locked});
    }
    if let Some(object_retention) = super::object_retention::render_bucket(bucket) {
        value["objectRetention"] = object_retention;
    }
    if let Some(policy) = &bucket.soft_delete_policy {
        value["softDeletePolicy"] = json!({"retentionDurationSeconds": policy.retention_duration_seconds.to_string(), "effectiveTime": timestamp(policy.effective_time)?});
    }
    if bucket.default_event_based_hold || profile == CompatProfile::LegacyEmulator {
        value["defaultEventBasedHold"] = bucket.default_event_based_hold.into();
    }
    if profile == CompatProfile::LegacyEmulator {
        value["location"] = LEGACY_LOCATION.into();
        value["locationType"] = "region".into();
        value["projectNumber"] = "0".into();
        if bucket.metageneration == 1 {
            value["storageClass"] = "STANDARD".into();
            value["etag"] = "RVRhZw==".into();
            value["updated"] = value["timeCreated"].clone();
            if let Some(object) = value.as_object_mut() {
                object.remove("labels");
            }
        }
    }
    Ok(value)
}

pub(crate) fn bucket_etag(bucket: &BucketRecord) -> String {
    format!(
        "\"b-{}-{}-{}\"",
        epoch_millis(bucket.time_created),
        epoch_millis(bucket.updated),
        bucket.metageneration
    )
}

/// Render one already bounded, sorted catalog page; the caller decides whether a token is due.
#[cfg(test)]
pub(crate) fn bucket_list(
    buckets: &[BucketRecord],
    next_page_token: Option<&str>,
    base_url: &str,
    projection: Projection,
) -> Result<Value, JsonError> {
    bucket_list_with_profile(
        buckets,
        next_page_token,
        base_url,
        projection,
        CompatProfile::Native,
    )
}

pub(crate) fn bucket_list_with_profile(
    buckets: &[BucketRecord],
    next_page_token: Option<&str>,
    base_url: &str,
    projection: Projection,
    profile: CompatProfile,
) -> Result<Value, JsonError> {
    let mut value = json!({"kind": "storage#buckets"});
    if !buckets.is_empty() {
        value["items"] = buckets
            .iter()
            .map(|bucket| bucket_resource_with_profile(bucket, base_url, projection, profile))
            .collect::<Result<Vec<_>, _>>()?
            .into();
    }
    if let Some(token) = next_page_token {
        value["nextPageToken"] = token.into();
    }
    Ok(value)
}

pub(super) fn encode_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn epoch_millis(time: SystemTime) -> i128 {
    let nanos = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as i128,
        Err(error) => -(error.duration().as_nanos() as i128),
    };
    nanos.div_euclid(1_000_000)
}

fn timestamp_error() -> JsonError {
    JsonError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "backendError",
        "Bucket timestamp is outside the supported RFC 3339 range",
    )
}

pub(crate) fn timestamp(time: SystemTime) -> Result<String, JsonError> {
    let millis = epoch_millis(time);
    if !(-62_167_219_200_000..=253_402_300_799_999).contains(&millis) {
        return Err(timestamp_error());
    }
    let seconds = millis.div_euclid(1000) as i64;
    let daytime = seconds.rem_euclid(86400);
    // Gregorian 400-year cycles avoid platform-local timezone and time_t limits.
    let days = seconds.div_euclid(86400) + 719468;
    let era = days.div_euclid(146097);
    let day_of_era = days.rem_euclid(146097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = month_from_march + if month_from_march < 10 { 3 } else { -9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        daytime / 3600,
        daytime / 60 % 60,
        daytime % 60,
        millis.rem_euclid(1000)
    ))
}
