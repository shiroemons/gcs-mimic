use super::{JsonError, Projection, WriteMode, query::validate_project};
use crate::bucket::{
    BucketMetadataUpdate, BucketRecord, CorsRule, Location, LocationKind, PublicAccessPrevention,
    RETENTION_RANGE_MESSAGE, RetentionPeriod, RetentionPolicyUpdate, SOFT_DELETE_RANGE_MESSAGE,
    SoftDeletedBucket, StorageClass, UNIFORM_ACCESS_LOCK_PERIOD, UniformAccessRequest, Versioning,
    VersioningUpdate, cors_from_value, cors_value, valid_label, validate_storage_class,
};
use crate::bucket::{autoclass::AutoclassState, lifecycle::LifecycleRules};
use crate::config::CompatProfile;
use crate::object::{MapPatch, Patch, unix_nanos::UnixNanos};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, time::SystemTime};

const LEGACY_LOCATION: &str = "US-CENTRAL1";

/// The project number the fake-gcs-server profile reports for every bucket.
const LEGACY_PROJECT_NUMBER: &str = "0";

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
    crate::bucket::name::validate(name)
        .map_err(|_| JsonError::invalid_parameter("Invalid bucket name"))
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

/// The fake-gcs-server profile keeps fake-gcs-server's wording; Cloud Storage reports a parser position.
fn bucket_parse_error(
    bytes: &[u8],
    error: &serde_json::Error,
    profile: CompatProfile,
) -> JsonError {
    if profile == CompatProfile::FakeGcsServer {
        return JsonError::legacy_parse_error("Invalid JSON bucket resource");
    }
    JsonError::parse_error(bytes, error)
}

/// fake-gcs-server reads only the first JSON value of an insert body and ignores the rest.
fn parse_insert_json(bytes: &[u8], profile: CompatProfile) -> Result<Value, JsonError> {
    let parsed = if profile == CompatProfile::FakeGcsServer {
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
        None => default
            .map(str::to_owned)
            .ok_or_else(|| JsonError::required(format!("Missing required {field}"))),
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
        Some(Value::Null) if profile == CompatProfile::FakeGcsServer => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| JsonError::invalid(format!("{field} must be a boolean"))),
    }
}

/// The create locations the fake-gcs-server profile accepts: the region it reports, and `US`,
/// which it stores and which official clients such as Go Storage send when the caller names no
/// location. Refusing `US` would refuse those clients' default create. Any other location is
/// refused rather than stored as `US`, so a caller never believes it got a placement it did not.
fn fake_gcs_server_accepts_location(location: &str) -> bool {
    location.eq_ignore_ascii_case(LEGACY_LOCATION) || location.eq_ignore_ascii_case("US")
}

fn insert_location(
    body: &serde_json::Map<String, Value>,
    profile: CompatProfile,
    default_location: &str,
) -> Result<String, JsonError> {
    if profile == CompatProfile::FakeGcsServer {
        if body
            .get("location")
            .is_some_and(|value| !value.as_str().is_some_and(fake_gcs_server_accepts_location))
        {
            return Err(JsonError::unsupported("location"));
        }
        return Ok("US".into());
    }
    // An empty string is not an absent location: Cloud Storage rejected `"location":""` with the
    // invalid-location error (recorded 2026-11) while an omitted location gets the default.
    if body.get("location").is_none_or(Value::is_null) {
        return Ok(default_location.to_owned());
    }
    insert_string(body, "location", Some(default_location))
}

/// The fake-gcs-server profile ignores labels on insert.
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
    if object.len() > crate::bucket::MAX_LABELS {
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
    // An invalid project is reported before a malformed body.
    validate_project(project)?;
    let value = parse_insert_json(bytes, profile)?;
    let body = value
        .as_object()
        .ok_or_else(|| JsonError::invalid("Bucket resource must be a JSON object"))?;
    insert_from_object(body, project, now, profile, default_location)
}

/// [`parse_insert_with_profile`] for a resource the caller already holds as members, such as
/// one the XML API builds from its `CreateBucketConfiguration` or gRPC from its `Bucket`
/// message.
pub(crate) fn insert_from_object(
    body: &Map<String, Value>,
    project: &str,
    now: SystemTime,
    profile: CompatProfile,
    default_location: &str,
) -> Result<BucketRecord, JsonError> {
    validate_project(project)?;
    reject_unsupported_insert_fields(body, profile)?;
    let name =
        if profile == CompatProfile::FakeGcsServer && body.get("name").is_none_or(Value::is_null) {
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
    let versioning = match body.get("versioning") {
        None => Versioning::Unset,
        Some(Value::Null) if profile == CompatProfile::FakeGcsServer => Versioning::Unset,
        Some(value) => {
            let enabled = value
                .as_object()
                .ok_or_else(|| JsonError::invalid("versioning must be an object"))?
                .get("enabled");
            // Only a boolean configures versioning; the fake-gcs-server profile reads a null as
            // no setting.
            insert_bool(enabled, "versioning.enabled", profile)?;
            Versioning::from(enabled.and_then(Value::as_bool))
        }
    };
    let default_event_based_hold = insert_bool(
        body.get("defaultEventBasedHold"),
        "defaultEventBasedHold",
        profile,
    )?;
    let location: Location = insert_location(body, profile, default_location)?.parse()?;
    let storage_class = if profile == CompatProfile::FakeGcsServer {
        "STANDARD".into()
    } else {
        insert_string(body, "storageClass", Some("STANDARD"))?
    };
    let storage_class = validate_storage_class(&storage_class)?;
    // Ruby sends disabled Autoclass even when no option was supplied; that stays unconfigured.
    let autoclass = body
        .get("autoclass")
        .map(crate::bucket::autoclass::parse_json)
        .transpose()?
        .map(|update| crate::bucket::autoclass::apply(None, &update, now))
        .transpose()?
        .flatten();
    let labels = insert_labels(body, profile)?;
    timestamp(now)?;
    Ok(BucketRecord {
        soft_delete_policy: parse_soft_delete_retention(body.get("softDeletePolicy"))?.map(
            |retention| crate::bucket::SoftDeletePolicy {
                retention_duration_seconds: retention.seconds(),
                effective_time: now,
            },
        ),
        retention_policy: parse_retention_period(body.get("retentionPolicy"))?
            .value()
            .map(|period| crate::bucket::RetentionPolicy {
                retention_period: period.seconds(),
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
        location,
        storage_class,
        autoclass,
        labels,
        cors: parse_cors(body.get("cors"))?.unwrap_or_default(),
        lifecycle: parse_lifecycle(body.get("lifecycle"), profile)?
            .unwrap_or_default()
            .into(),
        versioning,
        default_event_based_hold,
        public_access_prevention: parse_public_access_prevention(body.get("iamConfiguration"))?
            .unwrap_or_default(),
        uniform_bucket_level_access_locked_time: if parse_uniform_access(
            body.get("iamConfiguration"),
        )?
        .is_some_and(|request| request.enabled(None))
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
        Some(name) if name != bucket => Err(JsonError::content_disagrees(name, bucket)),
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

/// Labels of a bucket update. A patch merges them and removes a label with a null value; a
/// replacement requires string values and is limited to 64 labels.
fn update_labels(
    value: &Value,
    mode: WriteMode,
    profile: CompatProfile,
) -> Result<MapPatch, JsonError> {
    // A labels member that is not a map clears every label, like null. A string was confirmed in
    // the recording (`bkt-patch-errors`, own bucket: the harness label vanished with the rest);
    // an array or other non-map type is extrapolated from the string, which the recording cannot
    // distinguish.
    if value.is_null() {
        return Ok(MapPatch::Clear);
    }
    if profile == CompatProfile::Native && !value.is_object() {
        return Ok(MapPatch::Clear);
    }
    let labels = value
        .as_object()
        .ok_or_else(|| JsonError::invalid("labels must be an object or null"))?;
    match mode {
        WriteMode::Replace => {
            if labels.len() > crate::bucket::MAX_LABELS {
                return Err(JsonError::invalid("A bucket can have at most 64 labels"));
            }
            labels
                .iter()
                .map(|(key, value)| {
                    let value = value
                        .as_str()
                        .ok_or_else(|| JsonError::invalid("Label values must be strings"))?;
                    check_label(key, Some(value))?;
                    Ok((key.clone(), value.to_owned()))
                })
                .collect::<Result<_, JsonError>>()
                .map(MapPatch::Replace)
        }
        WriteMode::Patch => labels
            .iter()
            .map(|(key, value)| {
                let value = if value.is_null() {
                    None
                } else {
                    let value = value.as_str().ok_or_else(|| {
                        JsonError::invalid("Label values must be strings or null")
                    })?;
                    Some(value.to_owned())
                };
                check_label(key, value.as_deref())?;
                Ok((key.clone(), value))
            })
            .collect::<Result<_, JsonError>>()
            .map(MapPatch::Merge),
    }
}

/// Refuses a label key or value outside the label grammar.
fn check_label(key: &str, value: Option<&str>) -> Result<(), JsonError> {
    if !valid_label(key, crate::bucket::LabelPart::Key)
        || value.is_some_and(|value| !valid_label(value, crate::bucket::LabelPart::Value))
    {
        return Err(JsonError::invalid("Invalid bucket label"));
    }
    Ok(())
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
                if value.is_null() && profile == CompatProfile::FakeGcsServer {
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
        Some(value) => update_labels(value, mode, profile)?,
        None if omitted_clears => MapPatch::Clear,
        None => MapPatch::Keep,
    };
    let versioning = match body.get("versioning") {
        None => omitted_clears.then_some(VersioningUpdate::Set(Versioning::Unset)),
        Some(Value::Null) => Some(VersioningUpdate::Set(Versioning::Unset)),
        Some(value) => {
            let enabled = value
                .as_object()
                .ok_or_else(|| JsonError::invalid("versioning must be an object"))?
                .get("enabled");
            match boolean(enabled, "versioning.enabled")? {
                Some(on) if enabled.is_some_and(Value::is_boolean) => {
                    Some(VersioningUpdate::Set(Versioning::from(Some(on))))
                }
                // A null `enabled`, which only the fake-gcs-server profile reads as false.
                Some(_) => Some(VersioningUpdate::Disable),
                None if omitted_clears => Some(VersioningUpdate::Set(Versioning::Unset)),
                None if replace => Some(VersioningUpdate::Disable),
                None => None,
            }
        }
    };
    // The location of a bucket cannot change, and Cloud Storage ignored a different one in a
    // PATCH or PUT body like any other output-only member (recorded 2026-10-31).
    // The fake-gcs-server profile exposes a fixed location independently of the stored
    // placement, and refuses any other.
    if profile == CompatProfile::FakeGcsServer
        && string("location")?
            .is_some_and(|location| !location.eq_ignore_ascii_case(LEGACY_LOCATION))
    {
        return Err(JsonError::unsupported("location"));
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
        retention_policy: match parse_retention_period(body.get("retentionPolicy"))? {
            Patch::Set(period) => RetentionPolicyUpdate::Set(period),
            Patch::Clear => RetentionPolicyUpdate::Remove,
            // A PUT that leaves the policy out removes an unlocked one (recorded).
            Patch::Keep if omitted_clears && !body.contains_key("retentionPolicy") => {
                RetentionPolicyUpdate::RemoveUnlocked
            }
            // No recording covers an empty policy in a PUT (unverified); it keeps asking for
            // removal, so a locked policy refuses it.
            Patch::Keep if omitted_clears => RetentionPolicyUpdate::Remove,
            Patch::Keep => RetentionPolicyUpdate::Keep,
        },
        object_retention_enabled: super::object_retention::parse_bucket(
            body.get("objectRetention"),
        )?,
        cors: parse_cors(body.get("cors"))?.or_else(|| omitted_clears.then(Vec::new)),
        lifecycle: parse_lifecycle(body.get("lifecycle"), profile)?
            .or_else(|| omitted_clears.then(LifecycleRules::default)),
        labels,
        touched: update_touches_bucket(body),
        storage_class,
        autoclass: body
            .get("autoclass")
            .map(crate::bucket::autoclass::parse_json)
            .transpose()?,
        versioning,
        default_event_based_hold: boolean(
            body.get("defaultEventBasedHold"),
            "defaultEventBasedHold",
        )?
        .or(omitted_clears.then_some(false)),
        public_access_prevention: parse_public_access_prevention(body.get("iamConfiguration"))?,
        uniform_bucket_level_access_enabled: parse_uniform_access(body.get("iamConfiguration"))?,
        rpo: parse_rpo(body.get("rpo"))?,
    })
}

/// The recovery point objective of an update. Null restates the default and names none; a value
/// Cloud Storage does not define stays unsupported.
fn parse_rpo(value: Option<&Value>) -> Result<Option<crate::bucket::Rpo>, JsonError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    value
        .as_str()
        .and_then(|value| value.parse().ok())
        .map(Some)
        .ok_or_else(|| JsonError::unsupported("rpo"))
}

/// Rejects the bucket settings this emulator does not model. The ACL members and, in the native
/// profile, `rpo` have their own handling and pass.
fn reject_unsupported_update_fields(
    body: &serde_json::Map<String, Value>,
    profile: CompatProfile,
) -> Result<(), JsonError> {
    for field in UNSUPPORTED_FIELDS {
        if body.contains_key(*field) {
            if matches!(*field, "acl" | "defaultObjectAcl")
                || (*field == "rpo" && profile == CompatProfile::Native)
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
) -> Result<Option<StorageClass>, JsonError> {
    let class = match body.get("storageClass") {
        None => return Ok(None),
        Some(Value::Null) if profile == CompatProfile::Native => return Ok(None),
        Some(value) => value
            .as_str()
            .ok_or_else(|| JsonError::invalid("storageClass must be a string"))?,
    };
    let class = crate::bucket::validate_update_storage_class(class)?;
    Ok(Some(class))
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
/// the rules alone is the choice that cannot destroy data; the fake-gcs-server profile rejects it.
fn parse_lifecycle(
    value: Option<&Value>,
    profile: CompatProfile,
) -> Result<Option<LifecycleRules>, JsonError> {
    match value {
        Some(Value::Array(_)) if profile == CompatProfile::Native => Ok(None),
        Some(value) => Ok(Some(LifecycleRules::from_json(value)?)),
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
    crate::serde_util::int64(value).ok_or_else(|| int64_parse_error(value))
}

/// The soft-delete retention of a policy member. An absent member, a null policy and an empty
/// policy all leave the setting alone: Cloud Storage answered a null policy with the unchanged
/// resource (recorded 2026-10-31).
fn parse_soft_delete_retention(
    value: Option<&Value>,
) -> Result<Option<crate::bucket::SoftDeleteRetention>, JsonError> {
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
    crate::bucket::SoftDeleteRetention::try_from(int64_setting(duration)?)
        .map(Some)
        .map_err(Into::into)
}

/// The retention period of a bucket policy: an absent member or an empty policy (which Cloud
/// Storage treated as no change) keeps the policy; null or a policy without a period clears it.
fn parse_retention_period(value: Option<&Value>) -> Result<Patch<RetentionPeriod>, JsonError> {
    let Some(value) = value else {
        return Ok(Patch::Keep);
    };
    if value.is_null() {
        return Ok(Patch::Clear);
    }
    let policy = value
        .as_object()
        .ok_or_else(|| JsonError::invalid(RETENTION_RANGE_MESSAGE))?;
    if policy.is_empty() {
        return Ok(Patch::Keep);
    }
    let Some(period) = policy.get("retentionPeriod").filter(|v| !v.is_null()) else {
        return Ok(Patch::Clear);
    };
    RetentionPeriod::try_from(int64_setting(period)?)
        .map(Patch::Set)
        .map_err(Into::into)
}

fn parse_uniform_access(value: Option<&Value>) -> Result<Option<UniformAccessRequest>, JsonError> {
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
                enabled = match enabled {
                    None => Some(UniformAccessRequest::Set(value)),
                    Some(UniformAccessRequest::Set(first)) if first != value => {
                        Some(UniformAccessRequest::Conflicting)
                    }
                    agreed => agreed,
                };
            }
        }
    }
    Ok(enabled)
}

fn parse_public_access_prevention(
    value: Option<&Value>,
) -> Result<Option<PublicAccessPrevention>, JsonError> {
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
            Some(value) => value.parse::<PublicAccessPrevention>().map_err(Into::into),
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
    pub(crate) fn of(
        locked_time: Option<SystemTime>,
        requested: Option<UniformAccessRequest>,
    ) -> Self {
        if locked_time.is_some() || requested.is_some_and(|request| request.enabled(locked_time)) {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

/// The resource a predefined ACL query parameter would apply to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PredefinedAclTarget {
    /// An object method that applies no predefined ACL.
    Object,
    /// A media, multipart or resumable upload, which applies `predefinedAcl` outside UBLA.
    ObjectUpload,
    /// A copy, rewrite or compose, which applies `destinationPredefinedAcl` outside UBLA.
    DerivedObject,
    /// A metadata PATCH or PUT, which replaces the ACL with `predefinedAcl` outside UBLA.
    ObjectUpdate,
}

impl PredefinedAclTarget {
    /// The query parameter whose canned ACL this target applies.
    fn applied_key(self) -> Option<&'static str> {
        match self {
            Self::Object => None,
            Self::ObjectUpload | Self::ObjectUpdate => Some("predefinedAcl"),
            Self::DerivedObject => Some("destinationPredefinedAcl"),
        }
    }

    /// Whether the write publishes a new object, whose canned ACL it chooses.
    fn publishes(self) -> bool {
        match self {
            Self::ObjectUpload | Self::DerivedObject => true,
            Self::Object | Self::ObjectUpdate => false,
        }
    }
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
    // The fake-gcs-server profile never applied canned ACLs; it refuses them as before.
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
            crate::acl::PredefinedBucketAcl::parse_ignoring_case(value).ok_or_else(|| {
                invalid_value(value, crate::acl::PredefinedBucketAcl::ALLOWED)
                    .at_parameter("predefinedAcl")
            })
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

/// Refuses the predefined ACL query parameters an object write cannot apply for this caller and
/// bucket, before any body is read; publication rechecks the bucket in its transaction.
pub(crate) fn reject_predefined_acl(
    query: Option<&str>,
    uniform_access: UniformAccess,
    target: PredefinedAclTarget,
    access: &crate::bucket::BucketAccess,
) -> Result<(), JsonError> {
    let query = super::parse_query(query.unwrap_or_default())?;
    // Documented for uploads: an anonymous caller cannot name a canned ACL. Its object belongs
    // to the project owners team, so the caller would choose the ACL of an object it does not
    // own. A copy, rewrite or compose publishes the same kind of object, as an XML copy with
    // x-goog-acl does, so the rule covers them too. A metadata write changes an existing
    // object's ACL, which storage.objects.setIamPolicy governs instead.
    if let Some(key) = target.applied_key()
        && target.publishes()
        && query.contains_key(key)
        && access.is_anonymous()
    {
        return Err(JsonError::invalid_argument(format!(
            "Anonymous callers cannot specify {key}; authenticate or omit {key}"
        )));
    }
    let present = [
        "predefinedAcl",
        "predefinedDefaultObjectAcl",
        "destinationPredefinedAcl",
    ]
    .into_iter()
    .filter(|key| query.contains_key(*key));
    for key in present {
        let applies = target.applied_key() == Some(key);
        match uniform_access {
            UniformAccess::Disabled if applies => {}
            // The refusal the publishing or updating transaction gives (reason invalid,
            // recorded 2026-11-17), checked here before any body is read.
            UniformAccess::Enabled if applies => {
                return Err(match target {
                    PredefinedAclTarget::ObjectUpdate => {
                        crate::object::ObjectError::UniformAccessAclUpdate
                    }
                    PredefinedAclTarget::Object
                    | PredefinedAclTarget::ObjectUpload
                    | PredefinedAclTarget::DerivedObject => {
                        crate::object::ObjectError::UniformAccessPredefinedAcl
                    }
                }
                .into());
            }
            UniformAccess::Enabled => {
                return Err(JsonError::invalid_argument(format!(
                    "{key} cannot be used with uniform bucket-level access; remove {key} (for example, omit {key}=publicRead) and use bucket IAM permissions"
                )));
            }
            UniformAccess::Disabled => {
                return Err(JsonError::unsupported_object(key).at_parameter(key));
            }
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

/// The `BucketStorageLayout` resource. Its location fields match the profile's bucket resource,
/// so a client comparing the two sees one location. `customPlacementConfig` is never stored
/// (insert accepts only an empty one) and hierarchical namespace is not supported, so both
/// stay absent; clients read an absent `hierarchicalNamespace` as disabled.
pub(crate) fn storage_layout_resource(bucket: &BucketRecord, profile: CompatProfile) -> Value {
    let reported = ReportedLocation::new(bucket, profile);
    json!({
        "kind": "storage#storageLayout",
        "bucket": bucket.name,
        "location": reported.location,
        "locationType": reported.location_type,
    })
}

/// The location fields a profile reports for a bucket: the stored location, or for the
/// fake-gcs-server profile the one region that server always answers with.
#[derive(Clone, Copy)]
struct ReportedLocation {
    location: &'static str,
    location_type: &'static str,
}

impl ReportedLocation {
    fn new(bucket: &BucketRecord, profile: CompatProfile) -> Self {
        match profile {
            CompatProfile::Native => Self {
                location: bucket.location.as_str(),
                location_type: bucket.location.kind().as_str(),
            },
            CompatProfile::FakeGcsServer => Self {
                location: LEGACY_LOCATION,
                location_type: LocationKind::Region.as_str(),
            },
        }
    }
}

/// The bucket fields whose reported value depends on the compatibility profile. The
/// fake-gcs-server profile reports a fixed location and project number, and a bucket it never
/// updated (metageneration 1) as that server created it: `STANDARD`, its own ETag, the creation
/// time as the update time, and no labels.
struct ProfileFields<'a> {
    project_number: String,
    location: ReportedLocation,
    storage_class: StorageClass,
    etag: String,
    updated: String,
    /// `None` when the profile reports no labels, whatever the bucket stores.
    labels: Option<&'a BTreeMap<String, String>>,
}

fn profile_fields(
    bucket: &BucketRecord,
    profile: CompatProfile,
) -> Result<ProfileFields<'_>, JsonError> {
    // Both profiles render the stored update time, so one outside the RFC 3339 range fails the
    // fake-gcs-server profile too even when it reports the creation time instead.
    let updated = timestamp(bucket.updated)?;
    let location = ReportedLocation::new(bucket, profile);
    Ok(match profile {
        CompatProfile::Native => ProfileFields {
            project_number: crate::project::ProjectNumber::of(&bucket.project).to_string(),
            location,
            storage_class: bucket.storage_class,
            etag: bucket_etag(bucket),
            updated,
            labels: Some(&bucket.labels),
        },
        CompatProfile::FakeGcsServer if bucket.metageneration == 1 => ProfileFields {
            project_number: LEGACY_PROJECT_NUMBER.into(),
            location,
            storage_class: StorageClass::Standard,
            etag: "RVRhZw==".into(),
            updated: timestamp(bucket.time_created)?,
            labels: None,
        },
        CompatProfile::FakeGcsServer => ProfileFields {
            project_number: LEGACY_PROJECT_NUMBER.into(),
            location,
            storage_class: bucket.storage_class,
            etag: bucket_etag(bucket),
            updated,
            labels: Some(&bucket.labels),
        },
    })
}

pub(crate) fn bucket_resource_with_profile(
    bucket: &BucketRecord,
    base_url: &str,
    projection: Projection,
    profile: CompatProfile,
) -> Result<Value, JsonError> {
    // ACL and owner fields are not modeled, so both projections expose the same local subset.
    let (Projection::NoAcl | Projection::Full) = projection;
    let time_created = timestamp(bucket.time_created)?;
    let fields = profile_fields(bucket, profile)?;
    // Fields follow the order of a recorded Cloud Storage response. Fields it did not contain
    // follow `locationType`, in an order that has not been verified.
    let mut value = Map::from_iter([
        ("kind".into(), "storage#bucket".into()),
        (
            "selfLink".into(),
            format!(
                "{}/storage/v1/b/{}",
                base_url.trim_end_matches('/'),
                encode_segment(&bucket.name)
            )
            .into(),
        ),
        ("id".into(), bucket.name.as_str().into()),
        ("name".into(), bucket.name.as_str().into()),
        ("projectNumber".into(), fields.project_number.into()),
    ]);
    // The reference emulator's bucket shape has no generation; keep that profile unchanged.
    if profile == CompatProfile::Native {
        value.insert("generation".into(), bucket.generation.to_string().into());
    }
    value.insert(
        "metageneration".into(),
        bucket.metageneration.to_string().into(),
    );
    value.insert("location".into(), fields.location.location.into());
    value.insert("storageClass".into(), fields.storage_class.as_str().into());
    value.insert("etag".into(), fields.etag.into());
    value.insert("timeCreated".into(), time_created.into());
    value.insert("updated".into(), fields.updated.into());
    if let Some(policy) = &bucket.soft_delete_policy {
        let mut soft_delete = Map::from_iter([(
            "retentionDurationSeconds".into(),
            policy.retention_duration_seconds.to_string().into(),
        )]);
        if policy.is_enabled() {
            soft_delete.insert(
                "effectiveTime".into(),
                timestamp(policy.effective_time)?.into(),
            );
        }
        value.insert("softDeletePolicy".into(), soft_delete.into());
    }
    let mut uniform_access = Map::from_iter([(
        "enabled".into(),
        bucket
            .uniform_bucket_level_access_locked_time
            .is_some()
            .into(),
    )]);
    if let Some(locked_time) = bucket.uniform_bucket_level_access_locked_time {
        uniform_access.insert("lockedTime".into(), timestamp(locked_time)?.into());
    }
    value.insert(
        "iamConfiguration".into(),
        json!({
            "bucketPolicyOnly": uniform_access,
            "uniformBucketLevelAccess": uniform_access,
            "publicAccessPrevention": bucket.public_access_prevention.as_str(),
        }),
    );
    value.insert("locationType".into(), fields.location.location_type.into());
    // Cloud Storage returned `satisfiesPZI: true` for regional buckets (asia-northeast1 and
    // us-central1, 2026-10-31) and no such field for a multi-region bucket, which carries
    // `rpo: DEFAULT` instead. The reference emulator's bucket shape has neither field.
    if profile == CompatProfile::Native {
        match bucket.location.kind() {
            LocationKind::Region => value.insert("satisfiesPZI".into(), true.into()),
            LocationKind::DualRegion | LocationKind::MultiRegion => {
                value.insert("rpo".into(), "DEFAULT".into())
            }
        };
    }
    add_optional_bucket_fields(&mut value, bucket, fields.labels, profile)?;
    Ok(Value::Object(value))
}

fn add_optional_bucket_fields(
    value: &mut Map<String, Value>,
    bucket: &BucketRecord,
    labels: Option<&BTreeMap<String, String>>,
    profile: CompatProfile,
) -> Result<(), JsonError> {
    if let Some(config) = &bucket.autoclass {
        let mut autoclass = Map::from_iter([
            ("enabled".into(), config.is_enabled().into()),
            ("toggleTime".into(), timestamp(config.toggle_time)?.into()),
        ]);
        match config.state {
            AutoclassState::Enabled {
                terminal_storage_class,
                terminal_storage_class_update_time,
            } => {
                autoclass.insert(
                    "terminalStorageClass".into(),
                    terminal_storage_class.as_str().into(),
                );
                autoclass.insert(
                    "terminalStorageClassUpdateTime".into(),
                    timestamp(terminal_storage_class_update_time)?.into(),
                );
            }
            AutoclassState::Disabled => {}
        }
        value.insert("autoclass".into(), autoclass.into());
    }
    if let Some(labels) = labels.filter(|labels| !labels.is_empty()) {
        value.insert("labels".into(), crate::serde_util::string_map(labels));
    }
    if !bucket.cors.is_empty() {
        value.insert("cors".into(), cors_value(&bucket.cors));
    }
    // Unreadable stored rules are returned as stored, so the bucket stays repairable.
    if !bucket.lifecycle.is_empty() {
        let rules = serde_json::to_value(&bucket.lifecycle)
            .map_err(|error| JsonError::from(crate::storage::failure::unavailable(error)))?;
        value.insert(
            "lifecycle".into(),
            Value::Object(Map::from_iter([("rule".into(), rules)])),
        );
    }
    if bucket.versioning != Versioning::Unset || profile == CompatProfile::FakeGcsServer {
        value.insert(
            "versioning".into(),
            json!({"enabled": bucket.versioning.is_enabled()}),
        );
    }
    if let Some(policy) = &bucket.retention_policy {
        let mut retention = Map::from_iter([
            (
                "retentionPeriod".into(),
                policy.retention_period.to_string().into(),
            ),
            (
                "effectiveTime".into(),
                timestamp(policy.effective_time)?.into(),
            ),
        ]);
        // `isLocked` appears only once the policy is locked (recorded 2026-10-31).
        if policy.is_locked {
            retention.insert("isLocked".into(), true.into());
        }
        value.insert("retentionPolicy".into(), retention.into());
    }
    if let Some(object_retention) = super::object_retention::render_bucket(bucket) {
        value.insert("objectRetention".into(), object_retention);
    }
    if bucket.default_event_based_hold || profile == CompatProfile::FakeGcsServer {
        value.insert(
            "defaultEventBasedHold".into(),
            bucket.default_event_based_hold.into(),
        );
    }
    Ok(())
}

/// Cloud Storage encodes only the metageneration, so by inference from the encoding a recreated
/// bucket repeats its etag; that repetition itself has not been observed.
pub(crate) fn bucket_etag(bucket: &BucketRecord) -> String {
    super::objects::revision_etag(None, bucket.metageneration)
}

/// Render one already bounded, sorted catalog page; the caller decides whether a token is due.
pub(crate) fn bucket_list_with_profile(
    buckets: &[BucketRecord],
    next_page_token: Option<&str>,
    base_url: &str,
    projection: Projection,
    profile: CompatProfile,
) -> Result<Value, JsonError> {
    // The recorded object list orders kind, nextPageToken, items; a bucket list was not recorded
    // and follows the same layout.
    let mut value = Map::from_iter([("kind".into(), "storage#buckets".into())]);
    if let Some(token) = next_page_token {
        value.insert("nextPageToken".into(), token.into());
    }
    if !buckets.is_empty() {
        value.insert(
            "items".into(),
            buckets
                .iter()
                .map(|bucket| bucket_resource_with_profile(bucket, base_url, projection, profile))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        );
    }
    Ok(Value::Object(value))
}

/// A soft-deleted version renders as its bucket plus the two deletion times.
pub(crate) fn soft_deleted_bucket_resource(
    version: &SoftDeletedBucket,
    base_url: &str,
    projection: Projection,
    profile: CompatProfile,
) -> Result<Value, JsonError> {
    let mut value = bucket_resource_with_profile(&version.record, base_url, projection, profile)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| JsonError::service_unavailable("Bucket resource must be an object"))?;
    object.insert(
        "softDeleteTime".into(),
        timestamp(version.soft_delete_time)?.into(),
    );
    object.insert(
        "hardDeleteTime".into(),
        timestamp(version.hard_delete_time)?.into(),
    );
    Ok(value)
}

pub(crate) fn soft_deleted_bucket_list(
    versions: &[SoftDeletedBucket],
    next_page_token: Option<&str>,
    base_url: &str,
    projection: Projection,
    profile: CompatProfile,
) -> Result<Value, JsonError> {
    let mut value = Map::from_iter([("kind".into(), "storage#buckets".into())]);
    if let Some(token) = next_page_token {
        value.insert("nextPageToken".into(), token.into());
    }
    if !versions.is_empty() {
        value.insert(
            "items".into(),
            versions
                .iter()
                .map(|version| soft_deleted_bucket_resource(version, base_url, projection, profile))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        );
    }
    Ok(Value::Object(value))
}

pub(super) use crate::url::encode_unreserved as encode_segment;

fn timestamp_error() -> JsonError {
    JsonError::service_unavailable("Bucket timestamp is outside the supported RFC 3339 range")
}

/// Weekday names from 1970-01-01, a Thursday.
const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A UTC instant broken into the calendar fields that both RFC 3339 and HTTP dates render.
struct CivilTime {
    year: i64,
    /// 1 to 12.
    month: i64,
    day: i64,
    /// Seconds into the day.
    daytime: i64,
    /// Microseconds into the second.
    micros: i64,
    /// Days since 1970-01-01, which fixes the weekday.
    days: i64,
}

/// The calendar fields of `time`, computed without a platform calendar. Fails outside the
/// RFC 3339 range that every rendered timestamp shares.
fn civil_time(time: UnixNanos) -> Result<CivilTime, JsonError> {
    let micros = i128::from(time.secs()) * 1_000_000 + i128::from(time.subsec_nanos() / 1000);
    if !crate::clock::rfc3339_micros(micros) {
        return Err(timestamp_error());
    }
    let micros = i64::try_from(micros).map_err(|_| timestamp_error())?;
    let seconds = micros.div_euclid(1_000_000);
    let days = seconds.div_euclid(86400);
    // Gregorian 400-year cycles avoid platform-local timezone and time_t limits.
    let shifted = days + 719468;
    let era = shifted.div_euclid(146097);
    let day_of_era = shifted.rem_euclid(146097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = month_from_march + if month_from_march < 10 { 3 } else { -9 };
    Ok(CivilTime {
        year: year_of_era + era * 400 + i64::from(month <= 2),
        month,
        day,
        daytime: seconds.rem_euclid(86400),
        micros: micros.rem_euclid(1_000_000),
        days,
    })
}

/// The RFC 9110 date of a stored timestamp, as `Last-Modified` carries it.
pub(crate) fn http_date(time: SystemTime) -> Result<String, JsonError> {
    let CivilTime {
        year,
        month,
        day,
        daytime,
        days,
        ..
    } = civil_time(UnixNanos::from(time))?;
    // A negative day count is an instant before the epoch, which is never rendered as a date.
    let days = usize::try_from(days).map_err(|_| timestamp_error())?;
    let weekday = WEEKDAYS.get(days % 7).ok_or_else(timestamp_error)?;
    let month = usize::try_from(month - 1)
        .ok()
        .and_then(|index| MONTHS.get(index))
        .ok_or_else(timestamp_error)?;
    Ok(format!(
        "{weekday}, {day:02} {month} {year:04} {:02}:{:02}:{:02} GMT",
        daytime / 3600,
        daytime / 60 % 60,
        daytime % 60,
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
    rfc3339(time, Fraction::Millis)
}

/// Render a stored time as RFC 3339 UTC with exactly six fractional digits, as Cloud Storage
/// writes XML multipart `Initiated` and part `LastModified` times (recorded 2026-11-16).
pub(crate) fn timestamp_micros(time: SystemTime) -> Result<String, JsonError> {
    rfc3339(UnixNanos::from(time), Fraction::Micros)
}

/// The fractional-second digits of a rendered time; finer precision is floored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fraction {
    Millis,
    Micros,
}

fn rfc3339(time: UnixNanos, fraction: Fraction) -> Result<String, JsonError> {
    let CivilTime {
        year,
        month,
        day,
        daytime,
        micros,
        ..
    } = civil_time(time)?;
    let fraction = match fraction {
        Fraction::Millis => format!("{:03}", micros / 1000),
        Fraction::Micros => format!("{micros:06}"),
    };
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{fraction}Z",
        daytime / 3600,
        daytime / 60 % 60,
        daytime % 60,
    ))
}
