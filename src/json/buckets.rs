use super::{JsonError, Projection, query::validate_project};
use crate::bucket::BucketRecord;
use hyper::StatusCode;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) const UNSUPPORTED_FIELDS: &[&str] = &[
    "acl",
    "autoclass",
    "billing",
    "cors",
    "customPlacementConfig",
    "defaultEventBasedHold",
    "defaultObjectAcl",
    "encryption",
    "hierarchicalNamespace",
    "iamConfiguration",
    "ipFilter",
    "lifecycle",
    "logging",
    "objectRetention",
    "retentionPolicy",
    "rpo",
    "satisfiesPZI",
    "satisfiesPZS",
    "softDeletePolicy",
    "versioning",
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

/// Parse only the documented bucket metadata subset; unsupported semantics never round-trip.
pub fn parse_insert(
    bytes: &[u8],
    project: &str,
    now: SystemTime,
) -> Result<BucketRecord, JsonError> {
    validate_project(project)?;
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
    let name = string("name", None)?;
    validate_bucket_name(&name).map_err(|_| JsonError::invalid_argument("Invalid bucket name"))?;
    let location = string("location", Some("US"))?;
    if !matches!(location.as_str(), "US" | "EU" | "ASIA") {
        return Err(JsonError::unsupported("location (supported: US, EU, ASIA)"));
    }
    let storage_class = string("storageClass", Some("STANDARD"))?;
    if matches!(storage_class.as_str(), "REGIONAL" | "RAPID") {
        return Err(JsonError::unsupported(
            "storageClass requires a region or zone",
        ));
    }
    if !matches!(
        storage_class.as_str(),
        "STANDARD"
            | "NEARLINE"
            | "COLDLINE"
            | "ARCHIVE"
            | "MULTI_REGIONAL"
            | "DURABLE_REDUCED_AVAILABILITY"
    ) {
        return Err(JsonError::invalid_argument("Invalid bucket storageClass"));
    }
    let mut labels = BTreeMap::new();
    if let Some(value) = body.get("labels") {
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
    let millis = epoch_millis(now);
    timestamp(now)?;
    let duration =
        Duration::from_millis(u64::try_from(millis.abs()).map_err(|_| timestamp_error())?);
    let now = if millis < 0 {
        UNIX_EPOCH.checked_sub(duration)
    } else {
        UNIX_EPOCH.checked_add(duration)
    }
    .ok_or_else(timestamp_error)?;
    Ok(BucketRecord {
        name,
        project: project.to_owned(),
        location,
        location_type: "multi-region".to_owned(),
        storage_class,
        labels,
        metageneration: 1,
        time_created: now,
        updated: now,
    })
}

fn valid_label(value: &str, key: bool) -> bool {
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

/// Render supported metadata using a caller-validated advertised origin/base path.
pub fn bucket_resource(
    bucket: &BucketRecord,
    base_url: &str,
    projection: Projection,
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
        "etag": format!("\"b-{}-{}-{}\"", epoch_millis(bucket.time_created), epoch_millis(bucket.updated), bucket.metageneration),
        "location": bucket.location, "locationType": bucket.location_type, "storageClass": bucket.storage_class,
    });
    if !bucket.labels.is_empty() {
        value["labels"] = json!(bucket.labels);
    }
    Ok(value)
}

/// Render one already bounded, sorted catalog page; the caller decides whether a token is due.
pub fn bucket_list(
    buckets: &[BucketRecord],
    next_page_token: Option<&str>,
    base_url: &str,
    projection: Projection,
) -> Result<Value, JsonError> {
    let mut value = json!({"kind": "storage#buckets"});
    if !buckets.is_empty() {
        value["items"] = buckets
            .iter()
            .map(|bucket| bucket_resource(bucket, base_url, projection))
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

pub(super) fn timestamp(time: SystemTime) -> Result<String, JsonError> {
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
