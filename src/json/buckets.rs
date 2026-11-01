use super::{JsonError, Projection, WriteMode, query::validate_project};
use crate::bucket::{
    BucketMetadataUpdate, BucketRecord, CatalogError, CorsRule, SoftDeletedBucket,
    UNIFORM_ACCESS_LOCK_PERIOD, bucket_location, cors_from_value, cors_value, valid_label,
    validate_storage_class,
};
use crate::config::CompatProfile;
use crate::object::unix_nanos::UnixNanos;
use hyper::StatusCode;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::SystemTime};

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

/// The name of a bucket being created. Insert reports the offending value in the body error, and
/// a name below the minimum length has its own message (recorded 2026-10-31); a bucket named in
/// a path keeps the shorter `validate_bucket_name` error.
fn validate_new_bucket_name(name: &str) -> Result<(), JsonError> {
    if name.len() < 3 {
        return Err(JsonError::invalid(format!(
            "Bucket names must be at least 3 characters in length, got {}: '{name}'",
            name.len()
        )));
    }
    validate_bucket_name(name)
        .map_err(|_| JsonError::invalid(format!("Invalid bucket name: '{name}'")))
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
        Err(JsonError::invalid("invalid bucket name"))
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

/// The legacy profile keeps fake-gcs-server's wording; Cloud Storage reports a parser position.
fn bucket_parse_error(
    bytes: &[u8],
    error: &serde_json::Error,
    profile: CompatProfile,
) -> JsonError {
    if profile == CompatProfile::LegacyEmulator {
        return JsonError::new(
            StatusCode::BAD_REQUEST,
            "parseError",
            "Invalid JSON bucket resource",
        );
    }
    JsonError::parse_error(bytes, error)
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
    parsed.map_err(|error| bucket_parse_error(bytes, &error, profile))
}

fn reject_unsupported_insert_fields(
    body: &serde_json::Map<String, Value>,
    profile: CompatProfile,
) -> Result<(), JsonError> {
    for field in UNSUPPORTED_FIELDS {
        if body.contains_key(*field) {
            if *field == "rpo"
                && profile == CompatProfile::Native
                && body.get("rpo").is_some_and(is_default_rpo)
            {
                continue;
            }
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

/// `rpo` is `DEFAULT` for every non-regional bucket and the only value this emulator keeps, so
/// echoing it (or clearing it with null) changes nothing; other values need replication that
/// does not exist locally.
fn is_default_rpo(value: &Value) -> bool {
    value.is_null() || value == "DEFAULT"
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
            .ok_or_else(|| JsonError::invalid(format!("{field} must be a string"))),
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
            .ok_or_else(|| JsonError::invalid(format!("{field} must be a boolean"))),
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
        .ok_or_else(|| JsonError::invalid("labels must be a string map"))?;
    if object.len() > 64 {
        return Err(JsonError::invalid("A bucket can have at most 64 labels"));
    }
    for (key, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| JsonError::invalid("Label values must be strings"))?;
        if !valid_label(key, crate::bucket::LabelPart::Key)
            || !valid_label(value, crate::bucket::LabelPart::Value)
        {
            return Err(JsonError::invalid("Invalid bucket label"));
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
        .ok_or_else(|| JsonError::invalid("Bucket resource must be a JSON object"))?;
    reject_unsupported_insert_fields(body, profile)?;
    let name = if profile == CompatProfile::LegacyEmulator
        && body.get("name").is_none_or(Value::is_null)
    {
        String::new()
    } else {
        insert_string(body, "name", None)?
    };
    if profile == CompatProfile::Native {
        validate_new_bucket_name(&name)?;
    } else {
        validate_bucket_name_for_profile(&name, profile)
            .map_err(|_| JsonError::invalid("invalid bucket name"))?;
    }
    let versioning_enabled = match body.get("versioning") {
        None => false,
        Some(Value::Null) if profile == CompatProfile::LegacyEmulator => false,
        Some(value) => {
            let versioning = value
                .as_object()
                .ok_or_else(|| JsonError::invalid("versioning must be an object"))?;
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
        lifecycle: parse_lifecycle(body.get("lifecycle"), profile)?.unwrap_or_default(),
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
                JsonError::invalid("Uniform access deadline is outside the supported time range")
            })?)
        } else {
            None
        },
        metageneration: 1,
        generation: 0,
        time_created: now,
        updated: now,
    })
}

/// Rejects a body whose `name` disagrees with the bucket named in the path, as Cloud Storage does
/// for PATCH and PUT. An absent or non-string name is left to the ordinary field handling.
pub(crate) fn require_matching_bucket_name(bytes: &[u8], bucket: &str) -> Result<(), JsonError> {
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(bytes) else {
        return Ok(());
    };
    match body.get("name").and_then(Value::as_str) {
        Some(name) if name != bucket => Err(JsonError::invalid_parameter(format!(
            "Value '{name}' in content does not agree with value '{bucket}'. This can happen when a value set through a parameter is inconsistent with a value set in the request."
        ))),
        _ => Ok(()),
    }
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
    profile: CompatProfile,
) -> Result<Option<BTreeMap<String, Option<String>>>, JsonError> {
    // A labels member that is not a map (a string or an array) changes nothing here. The recording
    // sent a string to a bucket that had no labels, so it cannot tell "cleared" from "left alone";
    // keeping the labels is the choice that cannot destroy data.
    if value.is_null() {
        return Ok(None);
    }
    if profile == CompatProfile::Native && !value.is_object() {
        return Ok(Some(BTreeMap::new()));
    }
    let replace = mode == WriteMode::Replace;
    let labels = value
        .as_object()
        .ok_or_else(|| JsonError::invalid("labels must be an object or null"))?;
    if replace && labels.len() > 64 {
        return Err(JsonError::invalid("A bucket can have at most 64 labels"));
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
                            JsonError::invalid(if replace {
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
                return Err(JsonError::invalid("Invalid bucket label"));
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
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| bucket_parse_error(bytes, &error, profile))?;
    let body = value
        .as_object()
        .ok_or_else(|| JsonError::invalid("Bucket resource must be a JSON object"))?;
    reject_unsupported_update_fields(body, profile)?;
    // shortcut: legacy JSON unknown/output-only handling needs service evidence; use insert's ignored-input rule.
    let string = |field: &str| {
        body.get(field)
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| JsonError::invalid(format!("{field} must be a string")))
            })
            .transpose()
    };
    let boolean = |value: Option<&Value>, field: &str| {
        value
            .map(|value| {
                if value.is_null() && profile == CompatProfile::LegacyEmulator {
                    return Ok(false);
                }
                value
                    .as_bool()
                    .ok_or_else(|| JsonError::invalid(format!("{field} must be a boolean")))
            })
            .transpose()
    };
    let native = profile == CompatProfile::Native;
    // A null storageClass leaves the class alone. The recording sent it to a bucket already in the
    // default class, so this is an unverified choice, not an observation.
    let storage_class = update_storage_class(body, profile)?;
    // A PUT replaces the whole resource, so what it omits is cleared. Verified against the
    // recording: labels, cors, lifecycle, versioning and the retention policy are cleared, while the
    // storage class, PAP and the ACLs are kept. Unverified choices: defaultEventBasedHold is
    // cleared, UBLA and autoclass are kept, and a locked retention policy is kept. The legacy
    // profile keeps merging.
    let omitted_clears = replace && native;
    let labels = match body.get("labels") {
        Some(value) => Some(update_labels(value, mode, profile)?),
        None => omitted_clears.then_some(None),
    };
    let versioning_enabled = body
        .get("versioning")
        .map(|value| {
            if value.is_null() {
                return Ok(Some(false));
            }
            let versioning = value
                .as_object()
                .ok_or_else(|| JsonError::invalid("versioning must be an object"))?;
            boolean(versioning.get("enabled"), "versioning.enabled")
                .map(|enabled| enabled.or(replace.then_some(false)))
        })
        .transpose()?
        .flatten()
        .or(omitted_clears.then_some(false));
    // The location of a bucket cannot change, and Cloud Storage ignored a different one in a
    // PATCH or PUT body like any other output-only member (recorded 2026-10-31).
    let mut location = if native { None } else { string("location")? };
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
        retention_period: parse_retention_period(body.get("retentionPolicy"))?
            .or(omitted_clears.then_some(None)),
        lock_retention_policy: false,
        object_retention_enabled: super::object_retention::parse_bucket(
            body.get("objectRetention"),
        )?,
        cors: parse_cors(body.get("cors"))?.or_else(|| omitted_clears.then(Vec::new)),
        lifecycle: parse_lifecycle(body.get("lifecycle"), profile)?
            .or_else(|| omitted_clears.then(Vec::new)),
        labels,
        touched: update_touches_bucket(body),
        omitted_retention: omitted_clears && !body.contains_key("retentionPolicy"),
        storage_class,
        autoclass: body
            .get("autoclass")
            .map(crate::bucket::autoclass::parse_json)
            .transpose()?,
        versioning_enabled,
        default_event_based_hold: boolean(
            body.get("defaultEventBasedHold"),
            "defaultEventBasedHold",
        )?
        .or(omitted_clears.then_some(false)),
        location,
        public_access_prevention_enforced: parse_public_access_prevention(
            body.get("iamConfiguration"),
        )?,
        uniform_bucket_level_access_enabled: parse_uniform_access(body.get("iamConfiguration"))?,
    })
}

/// Rejects the bucket settings this emulator does not model. A restated default `rpo` and the
/// ACL members, which have their own handling, pass.
fn reject_unsupported_update_fields(
    body: &serde_json::Map<String, Value>,
    profile: CompatProfile,
) -> Result<(), JsonError> {
    for field in UNSUPPORTED_FIELDS {
        if body.contains_key(*field) {
            if matches!(*field, "acl" | "defaultObjectAcl")
                || (*field == "rpo"
                    && profile == CompatProfile::Native
                    && body.get("rpo").is_some_and(is_default_rpo))
            {
                continue;
            }
            return Err(JsonError::unsupported(field));
        }
    }
    Ok(())
}

/// The storage class of an update. A null class leaves the class alone (an unverified choice, see
/// `parse_bucket_update`), and an update names a rejected class, unlike an insert (recorded
/// 2026-10-31).
fn update_storage_class(
    body: &serde_json::Map<String, Value>,
    profile: CompatProfile,
) -> Result<Option<String>, JsonError> {
    let class = match body.get("storageClass") {
        None => return Ok(None),
        Some(Value::Null) if profile == CompatProfile::Native => return Ok(None),
        Some(value) => value
            .as_str()
            .ok_or_else(|| JsonError::invalid("storageClass must be a string"))?,
    };
    validate_storage_class(class).map_err(|error| match error {
        CatalogError::InvalidMetadata(_) => {
            JsonError::invalid(format!("Invalid storage class \"{class}\""))
        }
        error => error.into(),
    })?;
    Ok(Some(class.to_owned()))
}

/// Whether a PATCH or PUT member counts as a change even when it changes no stored value. Cloud
/// Storage advanced the metageneration for every recognised member except an empty message such
/// as `versioning: {}`, and for `labels`, `cors` and `lifecycle` even when empty (recorded
/// 2026-10-31); unknown and output-only members never counted.
fn update_touches_bucket(body: &serde_json::Map<String, Value>) -> bool {
    const TOUCHING: &[&str] = &[
        "acl",
        "autoclass",
        "billing",
        "cors",
        "defaultEventBasedHold",
        "defaultObjectAcl",
        "encryption",
        "hierarchicalNamespace",
        "iamConfiguration",
        "ipFilter",
        "labels",
        "lifecycle",
        "logging",
        "objectRetention",
        "retentionPolicy",
        "rpo",
        "softDeletePolicy",
        "storageClass",
        "versioning",
        "website",
    ];
    const EMPTY_MESSAGE_IS_NO_CHANGE: &[&str] = &[
        "autoclass",
        "iamConfiguration",
        "objectRetention",
        "retentionPolicy",
        "softDeletePolicy",
        "versioning",
    ];
    body.iter().any(|(key, value)| {
        TOUCHING.contains(&key.as_str())
            && !(EMPTY_MESSAGE_IS_NO_CHANGE.contains(&key.as_str()) && value == &json!({}))
    })
}

/// A lifecycle member. A JSON array is not a lifecycle message; the recording shows it accepted, but
/// it was sent to a bucket without rules, so it cannot tell "cleared" from "left alone". Leaving
/// the rules alone is the choice that cannot destroy data; the legacy profile rejects it.
fn parse_lifecycle(
    value: Option<&Value>,
    profile: CompatProfile,
) -> Result<Option<Vec<Value>>, JsonError> {
    match value {
        Some(Value::Array(_)) if profile == CompatProfile::Native => Ok(None),
        Some(value) => Ok(Some(crate::bucket::lifecycle::parse(value)?)),
        None => Ok(None),
    }
}

/// Cloud Storage's rejection of a value that is not an int64: the offending JSON is quoted.
fn int64_parse_error(value: &Value) -> JsonError {
    JsonError::invalid(format!(
        "Parse Error: Invalid value for TYPE_INT64 field: '{value}'."
    ))
}

/// An int64 setting sent as a JSON number or a decimal string, as the Discovery document
/// declares it.
fn int64_setting(value: &Value) -> Result<i64, JsonError> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .ok_or_else(|| int64_parse_error(value))
}

const SOFT_DELETE_RANGE_MESSAGE: &str =
    "Soft delete policy must have a retention duration between 3 days and 90 days.";
const RETENTION_RANGE_MESSAGE: &str =
    "Retention policy must have a retention period greater than 0 and less than 100 years.";

/// The soft-delete retention of a policy member. An absent member, a null policy and an empty
/// policy all leave the setting alone: Cloud Storage answered a null policy with the unchanged
/// resource (recorded 2026-10-31).
fn parse_soft_delete_retention(value: Option<&Value>) -> Result<Option<u64>, JsonError> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let policy = value
        .as_object()
        .ok_or_else(|| JsonError::invalid(SOFT_DELETE_RANGE_MESSAGE))?;
    let Some(duration) = policy
        .get("retentionDurationSeconds")
        .filter(|v| !v.is_null())
    else {
        return Ok(None);
    };
    u64::try_from(int64_setting(duration)?)
        .ok()
        .filter(|v| *v == 0 || (604800..=7776000).contains(v))
        .map(Some)
        .ok_or_else(|| JsonError::invalid(SOFT_DELETE_RANGE_MESSAGE))
}

/// The retention period of a bucket policy: `None` leaves the policy alone (absent member or an
/// empty policy, which Cloud Storage treated as no change), `Some(None)` removes it.
fn parse_retention_period(value: Option<&Value>) -> Result<Option<Option<u64>>, JsonError> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(Some(None));
    }
    let policy = value
        .as_object()
        .ok_or_else(|| JsonError::invalid(RETENTION_RANGE_MESSAGE))?;
    if policy.is_empty() {
        return Ok(None);
    }
    let Some(period) = policy.get("retentionPeriod").filter(|v| !v.is_null()) else {
        return Ok(Some(None));
    };
    u64::try_from(int64_setting(period)?)
        .ok()
        .filter(|v| (1..3_155_760_000).contains(v))
        .map(|period| Some(Some(period)))
        .ok_or_else(|| JsonError::invalid(RETENTION_RANGE_MESSAGE))
}

fn parse_uniform_access(value: Option<&Value>) -> Result<Option<bool>, JsonError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let config = value
        .as_object()
        .ok_or_else(|| JsonError::invalid("iamConfiguration must be an object"))?;
    let mut enabled = None;
    for field in ["uniformBucketLevelAccess", "bucketPolicyOnly"] {
        if let Some(value) = config.get(field).filter(|value| !value.is_null()) {
            let setting = value.as_object().ok_or_else(|| {
                JsonError::invalid(format!(
                    "{field} must be an object; use {{\"enabled\":true}}"
                ))
            })?;
            if let Some(value) = setting.get("enabled").filter(|value| !value.is_null()) {
                let value = value.as_bool().ok_or_else(|| {
                    JsonError::invalid(format!(
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
    // A null `iamConfiguration` leaves the settings alone (recorded 2026-10-31).
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let config = value
        .as_object()
        .ok_or_else(|| JsonError::invalid("iamConfiguration must be an object"))?;
    config
        .get("publicAccessPrevention")
        .filter(|value| !value.is_null())
        .map(|value| match value.as_str() {
            Some("enforced") => Ok(true),
            Some("inherited") => Ok(false),
            _ => Err(JsonError::invalid(format!(
                "Invalid value for: {} is not a valid value",
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned)
            ))),
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
    Object,
    /// A media, multipart or resumable upload, which applies `predefinedAcl` outside UBLA.
    ObjectUpload,
}

/// What a bucket write does with its predefined ACL query parameters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BucketAclWrite {
    Insert,
    Update,
}

/// The canned ACLs a bucket write asked for.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct BucketPredefinedAcls {
    pub acl: Option<crate::acl::PredefinedBucketAcl>,
    pub default_object_acl: Option<crate::acl::PredefinedObjectAcl>,
}

const UBLA_INSERT_ACL: &str = "Cannot use ACL API to set bucket policy when object policies are disabled, must use IAM API instead.";
const UBLA_UPDATE_ACL: &str = "Cannot use ACL API to update bucket policy when uniform bucket-level access is enabled. Read more at https://cloud.google.com/storage/docs/uniform-bucket-level-access";

/// Reads `predefinedAcl` and `predefinedDefaultObjectAcl` of a bucket write. Cloud Storage
/// rejects them under uniform bucket-level access and names the accepted values when one is
/// unknown. A PATCH or PUT applies them; an insert does not keep ACL entries yet, so it refuses
/// them rather than accept and ignore.
pub(crate) fn bucket_predefined_acls(
    query: Option<&str>,
    uniform_access: UniformAccess,
    write: BucketAclWrite,
    profile: CompatProfile,
) -> Result<BucketPredefinedAcls, JsonError> {
    let query = super::parse_query(query.unwrap_or_default())?;
    // The legacy profile never applied canned ACLs; it refuses them as before.
    if profile != CompatProfile::Native {
        for key in ["predefinedAcl", "predefinedDefaultObjectAcl"] {
            if query.contains_key(key) {
                return Err(match uniform_access {
                    UniformAccess::Enabled => JsonError::invalid(format!(
                        "{key} cannot be used with uniform bucket-level access; remove {key} (for example, omit {key}=publicRead) and use bucket IAM permissions"
                    )),
                    UniformAccess::Disabled => JsonError::unsupported(key),
                });
            }
        }
        return Ok(BucketPredefinedAcls::default());
    }
    let invalid_value = |value: &str, allowed: &str| {
        JsonError::invalid_parameter(format!(
            "Invalid string value: '{value}'. Allowed values: {allowed}"
        ))
    };
    let acl = query
        .get("predefinedAcl")
        .map(|value| {
            crate::acl::PredefinedBucketAcl::parse(value)
                .ok_or_else(|| invalid_value(value, crate::acl::PredefinedBucketAcl::ALLOWED))
        })
        .transpose()?;
    let default_object_acl = query
        .get("predefinedDefaultObjectAcl")
        .map(|value| {
            crate::acl::PredefinedObjectAcl::parse_ignoring_case(value).ok_or_else(|| {
                invalid_value(value, crate::acl::PredefinedObjectAcl::ALLOWED_DEFAULT)
            })
        })
        .transpose()?;
    if acl.is_none() && default_object_acl.is_none() {
        return Ok(BucketPredefinedAcls::default());
    }
    if uniform_access == UniformAccess::Enabled {
        return Err(JsonError::invalid(match write {
            BucketAclWrite::Insert => UBLA_INSERT_ACL,
            BucketAclWrite::Update => UBLA_UPDATE_ACL,
        }));
    }
    if write == BucketAclWrite::Insert {
        let key = if acl.is_some() {
            "predefinedAcl"
        } else {
            "predefinedDefaultObjectAcl"
        };
        return Err(JsonError::unsupported(key));
    }
    Ok(BucketPredefinedAcls {
        acl,
        default_object_acl,
    })
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
        let applied = target == PredefinedAclTarget::ObjectUpload
            && key == "predefinedAcl"
            && uniform_access == UniformAccess::Disabled;
        if query.contains_key(key) && !applied {
            return Err(match (uniform_access, target) {
                (UniformAccess::Enabled, PredefinedAclTarget::ObjectUpload)
                    if key == "predefinedAcl" =>
                {
                    JsonError::invalid_argument(crate::acl::UNIFORM_ACCESS_UPLOAD_ACL)
                }
                (UniformAccess::Enabled, _) => JsonError::invalid_argument(format!(
                    "{key} cannot be used with uniform bucket-level access; remove {key} (for example, omit {key}=publicRead) and use bucket IAM permissions"
                )),
                (
                    UniformAccess::Disabled,
                    PredefinedAclTarget::Object | PredefinedAclTarget::ObjectUpload,
                ) => JsonError::unsupported_object(key).at_parameter(key),
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
    // Fields follow the order of a recorded Cloud Storage response. Fields it did not contain
    // follow `locationType`, in an order that has not been verified.
    let mut value = json!({
        "kind": "storage#bucket",
        "selfLink": format!("{}/storage/v1/b/{}", base_url.trim_end_matches('/'), encode_segment(&bucket.name)),
        "id": bucket.name, "name": bucket.name,
        "projectNumber": project_number,
    });
    // The reference emulator's bucket shape has no generation; keep that profile unchanged.
    if profile == CompatProfile::Native {
        value["generation"] = bucket.generation.to_string().into();
    }
    value["metageneration"] = bucket.metageneration.to_string().into();
    value["location"] = json!(bucket.location);
    value["storageClass"] = json!(bucket.storage_class);
    value["etag"] = bucket_etag(bucket).into();
    value["timeCreated"] = timestamp(bucket.time_created)?.into();
    value["updated"] = timestamp(bucket.updated)?.into();
    if let Some(policy) = &bucket.soft_delete_policy {
        let mut soft_delete =
            json!({"retentionDurationSeconds": policy.retention_duration_seconds.to_string()});
        // Cloud Storage reports no effective time for a disabled policy (recorded 2026-10-31).
        if policy.retention_duration_seconds > 0 {
            soft_delete["effectiveTime"] = timestamp(policy.effective_time)?.into();
        }
        value["softDeletePolicy"] = soft_delete;
    }
    let mut uniform_access =
        json!({"enabled": bucket.uniform_bucket_level_access_locked_time.is_some()});
    if let Some(locked_time) = bucket.uniform_bucket_level_access_locked_time {
        uniform_access["lockedTime"] = timestamp(locked_time)?.into();
    }
    value["iamConfiguration"] = json!({
        "bucketPolicyOnly": uniform_access,
        "uniformBucketLevelAccess": uniform_access,
        "publicAccessPrevention": if bucket.public_access_prevention_enforced { "enforced" } else { "inherited" },
    });
    value["locationType"] = json!(bucket.location_type);
    // Cloud Storage returned `satisfiesPZI: true` for regional buckets (asia-northeast1 and
    // us-central1, 2026-10-31) and no such field for a multi-region bucket, which carries
    // `rpo: DEFAULT` instead. The reference emulator's bucket shape has neither field.
    if profile == CompatProfile::Native {
        if bucket.location_type == "region" {
            value["satisfiesPZI"] = true.into();
        } else {
            value["rpo"] = "DEFAULT".into();
        }
    }
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
        let mut retention = json!({"retentionPeriod":policy.retention_period.to_string(), "effectiveTime":timestamp(policy.effective_time)?});
        // `isLocked` appears only once the policy is locked (recorded 2026-10-31).
        if policy.is_locked {
            retention["isLocked"] = true.into();
        }
        value["retentionPolicy"] = retention;
    }
    if let Some(object_retention) = super::object_retention::render_bucket(bucket) {
        value["objectRetention"] = object_retention;
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
                object.shift_remove("labels");
            }
        }
    }
    Ok(value)
}

/// Cloud Storage encodes only the metageneration, so by inference from the encoding a recreated
/// bucket repeats its etag; that repetition itself has not been observed.
pub(crate) fn bucket_etag(bucket: &BucketRecord) -> String {
    super::objects::revision_etag(None, bucket.metageneration)
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
    // The recorded object list orders kind, nextPageToken, items; a bucket list was not recorded
    // and follows the same layout.
    let mut value = json!({"kind": "storage#buckets"});
    if let Some(token) = next_page_token {
        value["nextPageToken"] = token.into();
    }
    if !buckets.is_empty() {
        value["items"] = buckets
            .iter()
            .map(|bucket| bucket_resource_with_profile(bucket, base_url, projection, profile))
            .collect::<Result<Vec<_>, _>>()?
            .into();
    }
    Ok(value)
}

/// A soft-deleted version renders as its bucket plus the two deletion times.
pub(crate) fn soft_deleted_bucket_resource(
    version: &SoftDeletedBucket,
    base_url: &str,
    projection: Projection,
    profile: CompatProfile,
) -> Result<Value, JsonError> {
    let mut value = bucket_resource_with_profile(&version.record, base_url, projection, profile)?;
    value["softDeleteTime"] = timestamp(version.soft_delete_time)?.into();
    value["hardDeleteTime"] = timestamp(version.hard_delete_time)?.into();
    Ok(value)
}

pub(crate) fn soft_deleted_bucket_list(
    versions: &[SoftDeletedBucket],
    next_page_token: Option<&str>,
    base_url: &str,
    projection: Projection,
    profile: CompatProfile,
) -> Result<Value, JsonError> {
    let mut value = json!({"kind": "storage#buckets"});
    if let Some(token) = next_page_token {
        value["nextPageToken"] = token.into();
    }
    if !versions.is_empty() {
        value["items"] = versions
            .iter()
            .map(|version| soft_deleted_bucket_resource(version, base_url, projection, profile))
            .collect::<Result<Vec<_>, _>>()?
            .into();
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

fn timestamp_error() -> JsonError {
    JsonError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "backendError",
        "Bucket timestamp is outside the supported RFC 3339 range",
    )
}

/// The RFC 9110 date of a stored timestamp, as `Last-Modified` carries it. The weekday and month
/// come from the validated RFC 3339 rendering so no platform calendar is involved.
pub(crate) fn http_date(time: SystemTime) -> Result<String, JsonError> {
    let timestamp = timestamp(time)?;
    let days = time
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| timestamp_error())?
        .as_secs()
        / 86400;
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][(days % 7) as usize];
    // RFC 3339 timestamps always carry a two-digit month at bytes 5..7.
    let month = timestamp
        .get(5..7)
        .and_then(|month| month.parse::<usize>().ok())
        .and_then(|month| {
            [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ]
            .get(month.checked_sub(1)?)
            .copied()
        })
        .ok_or_else(timestamp_error)?;
    Ok(format!(
        "{weekday}, {} {month} {} {} GMT",
        &timestamp[8..10],
        &timestamp[..4],
        &timestamp[11..19]
    ))
}

/// Render a stored time as RFC 3339 UTC with exactly three fractional digits, the precision
/// Cloud Storage emits (observed 2026-10-31). Stored times keep their finer precision; only
/// the rendering floors, so ordering and stored deadlines are unaffected.
pub(crate) fn timestamp(time: SystemTime) -> Result<String, JsonError> {
    timestamp_nanos(UnixNanos::from(time))
}

/// [`timestamp`] for a time that is already seconds and nanoseconds, such as a retention time.
pub(crate) fn timestamp_nanos(time: UnixNanos) -> Result<String, JsonError> {
    let micros = i128::from(time.secs()) * 1_000_000 + i128::from(time.subsec_nanos() / 1000);
    if !crate::clock::rfc3339_micros(micros) {
        return Err(timestamp_error());
    }
    let seconds = micros.div_euclid(1_000_000) as i64;
    let fraction = format!("{:03}", micros.div_euclid(1000).rem_euclid(1000));
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
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{fraction}Z",
        daytime / 3600,
        daytime / 60 % 60,
        daytime % 60,
    ))
}
