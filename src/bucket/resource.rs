//! The bucket resource's members, as the JSON API names them, parsed for every protocol adapter.
//!
//! [`parse_insert`] creates a bucket record from a whole resource: the JSON API's parsed insert
//! body, or the members the XML and gRPC adapters build from their own create messages. The
//! member parsers it shares with the JSON adapter's PATCH and PUT live here too, so bucket
//! creation and update read a member the same way.

use super::{
    BucketRecord, CorsRule, LabelPart, Location, MAX_LABELS, PublicAccessPrevention,
    RETENTION_RANGE_MESSAGE, RetentionPeriod, RetentionPolicy, SOFT_DELETE_RANGE_MESSAGE,
    SoftDeletePolicy, SoftDeleteRetention, UNIFORM_ACCESS_LOCK_PERIOD, UniformAccessRequest,
    Versioning, cors_from_value, lifecycle::LifecycleRules, valid_label, validate_storage_class,
};
use crate::api_error::ApiError;
use crate::config::CompatProfile;
use crate::object::Patch;
use crate::time_format::timestamp;
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, time::SystemTime};

/// The location the fake-gcs-server profile reports for every bucket.
pub(crate) const LEGACY_LOCATION: &str = "US-CENTRAL1";

/// Bucket settings this emulator does not model. Insert and update refuse a member naming one
/// unless it restates the default or the adapter handles it separately.
pub(crate) const UNSUPPORTED_FIELDS: &[&str] = &[
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

/// The name of a bucket being created. Insert reports the offending value in the body error, and
/// a name below the minimum length has its own message (recorded 2026-10-31); a bucket named in
/// a path keeps the shorter `Invalid bucket name` error.
fn validate_new_bucket_name(name: &str) -> Result<(), ApiError> {
    if name.len() < 3 {
        return Err(ApiError::invalid(format!(
            "Bucket names must be at least 3 characters in length, got {}: '{name}'",
            name.len()
        )));
    }
    super::name::validate(name)
        .map_err(|_| ApiError::invalid(format!("Invalid bucket name: '{name}'")))
}

fn reject_unsupported_insert_fields(
    body: &Map<String, Value>,
    profile: CompatProfile,
) -> Result<(), ApiError> {
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
            return Err(ApiError::unsupported(field));
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
    body: &Map<String, Value>,
    field: &str,
    default: Option<&str>,
) -> Result<String, ApiError> {
    match body.get(field) {
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ApiError::invalid(format!("{field} must be a string"))),
        None => default
            .map(str::to_owned)
            .ok_or_else(|| ApiError::required(format!("Missing required {field}"))),
    }
}

/// A boolean field of an insert body; an absent field, or a legacy null, is false.
fn insert_bool(
    value: Option<&Value>,
    field: &str,
    profile: CompatProfile,
) -> Result<bool, ApiError> {
    match value {
        None => Ok(false),
        Some(Value::Null) if profile == CompatProfile::FakeGcsServer => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| ApiError::invalid(format!("{field} must be a boolean"))),
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
    body: &Map<String, Value>,
    profile: CompatProfile,
    default_location: &str,
) -> Result<String, ApiError> {
    if profile == CompatProfile::FakeGcsServer {
        if body
            .get("location")
            .is_some_and(|value| !value.as_str().is_some_and(fake_gcs_server_accepts_location))
        {
            return Err(ApiError::unsupported("location"));
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
    body: &Map<String, Value>,
    profile: CompatProfile,
) -> Result<BTreeMap<String, String>, ApiError> {
    let mut labels = BTreeMap::new();
    let Some(value) = body
        .get("labels")
        .filter(|_| profile == CompatProfile::Native)
    else {
        return Ok(labels);
    };
    let object = value
        .as_object()
        .ok_or_else(|| ApiError::invalid("labels must be a string map"))?;
    if object.len() > MAX_LABELS {
        return Err(ApiError::invalid("A bucket can have at most 64 labels"));
    }
    for (key, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| ApiError::invalid("Label values must be strings"))?;
        if !valid_label(key, LabelPart::Key) || !valid_label(value, LabelPart::Value) {
            return Err(ApiError::invalid("Invalid bucket label"));
        }
        labels.insert(key.clone(), value.to_owned());
    }
    Ok(labels)
}

/// The record a bucket insert creates from a resource the caller already holds as members: the
/// JSON API's parsed body, or one the XML API builds from its `CreateBucketConfiguration` or gRPC
/// from its `Bucket` message.
pub(crate) fn parse_insert(
    body: &Map<String, Value>,
    project: &str,
    now: SystemTime,
    profile: CompatProfile,
    default_location: &str,
) -> Result<BucketRecord, ApiError> {
    crate::auth::validate_project_id(project)?;
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
        super::name::validate_for_profile(&name, profile)
            .map_err(|_| ApiError::invalid("invalid bucket name"))?;
    }
    let versioning = match body.get("versioning") {
        None => Versioning::Unset,
        Some(Value::Null) if profile == CompatProfile::FakeGcsServer => Versioning::Unset,
        Some(value) => {
            let enabled = value
                .as_object()
                .ok_or_else(|| ApiError::invalid("versioning must be an object"))?
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
        .map(super::autoclass::parse_json)
        .transpose()?
        .map(|update| super::autoclass::apply(None, update, now))
        .transpose()?
        .flatten();
    let labels = insert_labels(body, profile)?;
    timestamp(now)?;
    Ok(BucketRecord {
        soft_delete_policy: parse_soft_delete_retention(body.get("softDeletePolicy"))?.map(
            |retention| SoftDeletePolicy {
                retention_duration_seconds: retention.seconds(),
                effective_time: now,
            },
        ),
        retention_policy: parse_retention_period(body.get("retentionPolicy"))?
            .value()
            .map(|period| RetentionPolicy {
                retention_period: period.seconds(),
                effective_time: now,
                is_locked: false,
            }),
        // The adapter reconciles an echoed value with enableObjectRetention.
        object_retention_enabled: parse_object_retention(body.get("objectRetention"))?
            .unwrap_or(false),
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
                ApiError::invalid("Uniform access deadline is outside the supported time range")
            })?)
        } else {
            None
        },
        ..BucketRecord::new(name, project, location, now)
    })
}

/// A lifecycle member. A JSON array is not a lifecycle message; the recording shows it accepted, but
/// it was sent to a bucket without rules, so it cannot tell "cleared" from "left alone". Leaving
/// the rules alone is the choice that cannot destroy data; the fake-gcs-server profile rejects it.
pub(crate) fn parse_lifecycle(
    value: Option<&Value>,
    profile: CompatProfile,
) -> Result<Option<LifecycleRules>, ApiError> {
    match value {
        Some(Value::Array(_)) if profile == CompatProfile::Native => Ok(None),
        Some(value) => Ok(Some(LifecycleRules::from_json(value)?)),
        None => Ok(None),
    }
}

/// Cloud Storage's rejection of a value that is not an int64: the offending JSON is quoted.
fn int64_parse_error(value: &Value) -> ApiError {
    ApiError::invalid(format!(
        "Parse Error: Invalid value for TYPE_INT64 field: '{value}'."
    ))
}

/// An int64 setting sent as a JSON number or a decimal string, as the Discovery document
/// declares it.
fn int64_setting(value: &Value) -> Result<i64, ApiError> {
    crate::serde_util::int64(value).ok_or_else(|| int64_parse_error(value))
}

/// The soft-delete retention of a policy member. An absent member, a null policy and an empty
/// policy all leave the setting alone: Cloud Storage answered a null policy with the unchanged
/// resource (recorded 2026-10-31).
pub(crate) fn parse_soft_delete_retention(
    value: Option<&Value>,
) -> Result<Option<SoftDeleteRetention>, ApiError> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let policy = value
        .as_object()
        .ok_or_else(|| ApiError::invalid(SOFT_DELETE_RANGE_MESSAGE))?;
    let Some(duration) = policy
        .get("retentionDurationSeconds")
        .filter(|v| !v.is_null())
    else {
        return Ok(None);
    };
    SoftDeleteRetention::try_from(int64_setting(duration)?)
        .map(Some)
        .map_err(Into::into)
}

/// The retention period of a bucket policy: an absent member or an empty policy (which Cloud
/// Storage treated as no change) keeps the policy; null or a policy without a period clears it.
pub(crate) fn parse_retention_period(
    value: Option<&Value>,
) -> Result<Patch<RetentionPeriod>, ApiError> {
    let Some(value) = value else {
        return Ok(Patch::Keep);
    };
    if value.is_null() {
        return Ok(Patch::Clear);
    }
    let policy = value
        .as_object()
        .ok_or_else(|| ApiError::invalid(RETENTION_RANGE_MESSAGE))?;
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

/// Parses an echoed bucket `objectRetention`: `Some(true)` for `{"mode":"Enabled"}`.
///
/// The value is output-only after creation; callers compare it with the stored state.
pub(crate) fn parse_object_retention(value: Option<&Value>) -> Result<Option<bool>, ApiError> {
    let Some(value) = value.filter(|value| !value.is_null() && *value != &json!({})) else {
        return Ok(None);
    };
    match value.get("mode").and_then(Value::as_str) {
        Some("Enabled") if value.as_object().is_some_and(|fields| fields.len() == 1) => {
            Ok(Some(true))
        }
        _ => Err(ApiError::invalid(
            "objectRetention only reports {\"mode\":\"Enabled\"}; enable object retention with the enableObjectRetention=true query parameter on bucket insert",
        )),
    }
}

pub(crate) fn parse_uniform_access(
    value: Option<&Value>,
) -> Result<Option<UniformAccessRequest>, ApiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let config = value
        .as_object()
        .ok_or_else(|| ApiError::invalid("iamConfiguration must be an object"))?;
    let mut enabled = None;
    for field in ["uniformBucketLevelAccess", "bucketPolicyOnly"] {
        if let Some(value) = config.get(field).filter(|value| !value.is_null()) {
            let setting = value.as_object().ok_or_else(|| {
                ApiError::invalid(format!(
                    "{field} must be an object; use {{\"enabled\":true}}"
                ))
            })?;
            if let Some(value) = setting.get("enabled").filter(|value| !value.is_null()) {
                let value = value.as_bool().ok_or_else(|| {
                    ApiError::invalid(format!(
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

pub(crate) fn parse_public_access_prevention(
    value: Option<&Value>,
) -> Result<Option<PublicAccessPrevention>, ApiError> {
    // A null `iamConfiguration` leaves the settings alone (recorded 2026-10-31).
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let config = value
        .as_object()
        .ok_or_else(|| ApiError::invalid("iamConfiguration must be an object"))?;
    config
        .get("publicAccessPrevention")
        .filter(|value| !value.is_null())
        .map(|value| match value.as_str() {
            Some(value) => value.parse::<PublicAccessPrevention>().map_err(Into::into),
            _ => Err(ApiError::invalid(format!(
                "Invalid value for: {} is not a valid value",
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned)
            ))),
        })
        .transpose()
}

pub(crate) fn parse_cors(value: Option<&Value>) -> Result<Option<Vec<CorsRule>>, ApiError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_bucket_defaults_match_a_native_insert() {
        use std::time::{Duration, UNIX_EPOCH};
        let name = super::super::name::BucketName::try_from("bucket").unwrap();
        let body = Map::from_iter([("name".into(), Value::from(name.as_str()))]);
        let insert = |project, now| parse_insert(&body, project, now, CompatProfile::Native, "US");
        assert_eq!(
            BucketRecord::default_for(&name, "project", UNIX_EPOCH).unwrap(),
            insert("project", UNIX_EPOCH).unwrap()
        );
        let outside = UNIX_EPOCH + Duration::from_secs(253_402_300_800);
        for (project, now) in [("bad project", outside), ("project", outside)] {
            let error = BucketRecord::default_for(&name, project, now).unwrap_err();
            crate::error::assert_distinct_causes(&error);
            assert_eq!(error.to_string(), insert(project, now).unwrap_err().message);
        }
    }

    #[test]
    fn bucket_object_retention_echo_values() {
        assert_eq!(parse_object_retention(None).unwrap(), None);
        assert_eq!(parse_object_retention(Some(&Value::Null)).unwrap(), None);
        assert_eq!(parse_object_retention(Some(&json!({}))).unwrap(), None);
        assert_eq!(
            parse_object_retention(Some(&json!({"mode":"Enabled"}))).unwrap(),
            Some(true)
        );
        for invalid in [
            json!({"mode":"Disabled"}),
            json!({"mode":"Enabled","extra":true}),
            json!(true),
        ] {
            assert!(parse_object_retention(Some(&invalid)).is_err(), "{invalid}");
        }
    }
}
