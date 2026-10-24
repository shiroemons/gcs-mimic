use super::{JsonError, Projection, query::validate_project};
use crate::bucket::{
    BucketMetadataUpdate, BucketRecord, CorsRule, cors_from_value, cors_value, valid_label,
    validate_storage_class,
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
    "autoclass",
    "billing",
    "customPlacementConfig",
    "defaultObjectAcl",
    "encryption",
    "hierarchicalNamespace",
    "ipFilter",
    "lifecycle",
    "logging",
    "objectRetention",
    "retentionPolicy",
    "rpo",
    "satisfiesPZI",
    "satisfiesPZS",
    "softDeletePolicy",
    "website",
];

pub fn validate_bucket_name(name: &str) -> Result<(), JsonError> {
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
    parse_insert_with_profile(bytes, project, now, CompatProfile::Native)
}

pub fn parse_insert_with_profile(
    bytes: &[u8],
    project: &str,
    now: SystemTime,
    profile: CompatProfile,
) -> Result<BucketRecord, JsonError> {
    validate_project(project)?;
    let parsed = if profile == CompatProfile::LegacyEmulator {
        serde_json::Deserializer::from_slice(bytes)
            .into_iter::<Value>()
            .next()
            .unwrap_or_else(|| serde_json::from_slice(bytes))
    } else {
        serde_json::from_slice(bytes)
    };
    let value: Value = parsed.map_err(|_| {
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
            // The pinned Go SDK sends an empty rule list for any nonnil BucketAttrs.
            if *field == "lifecycle" && body.get(*field) == Some(&json!({"rule": []})) {
                continue;
            }
            return Err(JsonError::unsupported(field));
        }
    }
    let string = |field, default: Option<&str>| -> Result<String, JsonError> {
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
    };
    let name = if profile == CompatProfile::LegacyEmulator
        && body.get("name").is_none_or(Value::is_null)
    {
        String::new()
    } else {
        string("name", None)?
    };
    validate_bucket_name_for_profile(&name, profile).map_err(|_| {
        JsonError::invalid_argument(if profile == CompatProfile::Native {
            "Invalid bucket name"
        } else {
            "invalid bucket name"
        })
    })?;
    let boolean = |value: Option<&Value>, field| -> Result<bool, JsonError> {
        match value {
            None => Ok(false),
            Some(Value::Null) if profile == CompatProfile::LegacyEmulator => Ok(false),
            Some(value) => value
                .as_bool()
                .ok_or_else(|| JsonError::invalid_argument(format!("{field} must be a boolean"))),
        }
    };
    let versioning_enabled = match body.get("versioning") {
        None => false,
        Some(Value::Null) if profile == CompatProfile::LegacyEmulator => false,
        Some(value) => {
            let versioning = value
                .as_object()
                .ok_or_else(|| JsonError::invalid_argument("versioning must be an object"))?;
            boolean(versioning.get("enabled"), "versioning.enabled")?
        }
    };
    let default_event_based_hold =
        boolean(body.get("defaultEventBasedHold"), "defaultEventBasedHold")?;
    let location = if profile == CompatProfile::LegacyEmulator {
        "US".into()
    } else {
        string("location", Some("US"))?
    };
    if !matches!(location.as_str(), "US" | "EU" | "ASIA") {
        return Err(JsonError::unsupported("location (supported: US, EU, ASIA)"));
    }
    let storage_class = if profile == CompatProfile::LegacyEmulator {
        "STANDARD".into()
    } else {
        string("storageClass", Some("STANDARD"))?
    };
    validate_storage_class(&storage_class)?;
    let mut labels = BTreeMap::new();
    if let Some(value) = body
        .get("labels")
        .filter(|_| profile == CompatProfile::Native)
    {
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
            if !valid_label(key, true) || !valid_label(value, false) {
                return Err(JsonError::invalid_argument("Invalid bucket label"));
            }
            labels.insert(key.clone(), value.to_owned());
        }
    }
    timestamp(now)?;
    Ok(BucketRecord {
        name,
        project: project.to_owned(),
        location,
        location_type: "multi-region".to_owned(),
        storage_class,
        labels,
        cors: parse_cors(body.get("cors"))?.unwrap_or_default(),
        versioning_enabled,
        default_event_based_hold,
        public_access_prevention_enforced: parse_public_access_prevention(
            body.get("iamConfiguration"),
        )?
        .unwrap_or(false),
        metageneration: 1,
        time_created: now,
        updated: now,
    })
}

pub fn parse_bucket_patch(
    bytes: &[u8],
    profile: CompatProfile,
) -> Result<BucketMetadataUpdate, JsonError> {
    parse_bucket_update(bytes, profile, false)
}

pub fn parse_bucket_replacement(
    bytes: &[u8],
    profile: CompatProfile,
) -> Result<BucketMetadataUpdate, JsonError> {
    parse_bucket_update(bytes, profile, true)
}

fn parse_bucket_update(
    bytes: &[u8],
    profile: CompatProfile,
    replace: bool,
) -> Result<BucketMetadataUpdate, JsonError> {
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
            if replace && *field == "acl" && body.get(*field) == Some(&json!([])) {
                continue;
            }
            if *field == "lifecycle" && body.get(*field) == Some(&json!({"rule": []})) {
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
        .map(|value| {
            if value.is_null() {
                return Ok(None);
            }
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
                    if !valid_label(key, true)
                        || value
                            .as_deref()
                            .is_some_and(|value| !valid_label(value, false))
                    {
                        return Err(JsonError::invalid_argument("Invalid bucket label"));
                    }
                    Ok((key.clone(), value))
                })
                .collect::<Result<_, JsonError>>()
                .map(Some)
        })
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
        cors: parse_cors(body.get("cors"))?,
        labels,
        storage_class,
        versioning_enabled,
        default_event_based_hold: boolean(
            body.get("defaultEventBasedHold"),
            "defaultEventBasedHold",
        )?,
        location,
        public_access_prevention_enforced: parse_public_access_prevention(
            body.get("iamConfiguration"),
        )?,
    })
}

fn parse_public_access_prevention(value: Option<&Value>) -> Result<Option<bool>, JsonError> {
    let Some(value) = value else { return Ok(None) };
    let config = value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument("iamConfiguration must be an object"))?;
    for field in ["uniformBucketLevelAccess", "bucketPolicyOnly"] {
        if let Some(value) = config.get(field)
            && !value.as_object().is_some_and(|setting| {
                setting.get("enabled") == Some(&Value::Bool(false))
                    && !setting.contains_key("lockedTime")
            })
        {
            return Err(JsonError::unsupported(&format!("iamConfiguration.{field}")));
        }
    }
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

/// Render supported metadata using a caller-validated advertised origin/base path.
pub fn bucket_resource(
    bucket: &BucketRecord,
    base_url: &str,
    projection: Projection,
) -> Result<Value, JsonError> {
    bucket_resource_with_profile(bucket, base_url, projection, CompatProfile::Native)
}

pub fn bucket_resource_with_profile(
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
    if !bucket.labels.is_empty() {
        value["labels"] = json!(bucket.labels);
    }
    if !bucket.cors.is_empty() {
        value["cors"] = cors_value(&bucket.cors);
    }
    if bucket.versioning_enabled || profile == CompatProfile::LegacyEmulator {
        value["versioning"] = json!({"enabled": bucket.versioning_enabled});
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
            value.as_object_mut().unwrap().remove("labels");
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
pub fn bucket_list(
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

pub fn bucket_list_with_profile(
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
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("writing to String cannot fail");
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
