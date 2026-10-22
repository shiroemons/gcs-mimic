use super::JsonError;
use crate::object::{ObjectError, ObjectMetadataUpdate, ObjectRecord};
use hyper::{StatusCode, header::HeaderValue};
use serde_json::{Map, Value};

pub struct ObjectMetadataRequest {
    pub update: ObjectMetadataUpdate,
    pub unchanged: Map<String, Value>,
}

impl ObjectMetadataRequest {
    pub fn validate_unchanged(
        unchanged: &Map<String, Value>,
        record: &ObjectRecord,
        base: &str,
    ) -> Result<(), ObjectError> {
        let resource = super::object_resource(record, base, super::Projection::Full)
            .map_err(|error| ObjectError::Unavailable(error.to_string()))?;
        for (key, value) in unchanged {
            let same_integer = matches!(key.as_str(), "generation" | "metageneration" | "size")
                && value.as_u64().is_some_and(|number| {
                    resource[key].as_str() == Some(number.to_string().as_str())
                });
            if resource.get(key) != Some(value) && !same_integer {
                return Err(ObjectError::UnsupportedMetadata(key.clone()));
            }
        }
        Ok(())
    }
}

/// Parse metadata mutations separately from upload bodies and server-managed fields.
pub fn parse_object_metadata(
    bytes: &[u8],
    replace: bool,
) -> Result<ObjectMetadataRequest, JsonError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "parseError",
            "Invalid JSON object resource",
        )
    })?;
    let body = value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument("Object resource must be a JSON object"))?;
    for field in [
        "acl",
        "predefinedAcl",
        "temporaryHold",
        "retention",
        "contexts",
        "customTime",
        "kmsKeyName",
        "customerEncryption",
    ] {
        if body.contains_key(field) {
            return Err(JsonError::unsupported_object(field));
        }
    }
    let mut unchanged = Map::new();
    // shortcut: changed output-only and unknown inputs need legacy service evidence; accept only identical resource values.
    for key in body.keys() {
        if matches!(
            key.as_str(),
            "kind"
                | "id"
                | "selfLink"
                | "mediaLink"
                | "name"
                | "bucket"
                | "generation"
                | "metageneration"
                | "size"
                | "md5Hash"
                | "crc32c"
                | "etag"
                | "timeCreated"
                | "timeFinalized"
                | "updated"
                | "storageClass"
                | "timeStorageClassUpdated"
                | "componentCount"
                | "owner"
                | "timeDeleted"
                | "softDeleteTime"
                | "hardDeleteTime"
                | "retentionExpirationTime"
                | "restoreToken"
        ) {
            unchanged.insert(key.clone(), body[key].clone());
            continue;
        }
        if !matches!(
            key.as_str(),
            "contentType"
                | "contentEncoding"
                | "contentDisposition"
                | "contentLanguage"
                | "cacheControl"
                | "metadata"
                | "eventBasedHold"
        ) {
            return Err(JsonError::unsupported_object(key));
        }
    }
    let metadata = body
        .get("metadata")
        .map(|value| {
            if value.is_null() {
                return Ok(None);
            }
            let entries = value
                .as_object()
                .ok_or_else(|| JsonError::invalid_argument("metadata must be an object or null"))?;
            entries
                .iter()
                .map(|(key, value)| {
                    if key.is_empty() {
                        return Err(JsonError::unsupported_object("empty custom metadata key"));
                    }
                    let value = if value.is_null() {
                        None
                    } else {
                        Some(
                            value
                                .as_str()
                                .ok_or_else(|| {
                                    JsonError::invalid_argument(
                                        "Metadata values must be strings or null",
                                    )
                                })?
                                .to_owned(),
                        )
                    };
                    Ok((key.clone(), value))
                })
                .collect::<Result<_, JsonError>>()
                .map(Some)
        })
        .transpose()?;
    if body.get("contentLanguage").and_then(Value::as_str) == Some("") {
        return Err(JsonError::invalid_argument(
            "contentLanguage must not be empty; use null to clear it",
        ));
    }
    Ok(ObjectMetadataRequest {
        unchanged,
        update: ObjectMetadataUpdate {
            replace,
            content_type: string(body, "contentType")?,
            content_encoding: string(body, "contentEncoding")?,
            content_disposition: string(body, "contentDisposition")?,
            content_language: string(body, "contentLanguage")?,
            cache_control: string(body, "cacheControl")?,
            metadata,
            event_based_hold: body
                .get("eventBasedHold")
                .map(|value| {
                    value.as_bool().ok_or_else(|| {
                        JsonError::invalid_argument("eventBasedHold must be a boolean")
                    })
                })
                .transpose()?,
        },
    })
}

fn string(body: &Map<String, Value>, key: &str) -> Result<Option<Option<String>>, JsonError> {
    body.get(key)
        .map(|value| {
            if value.is_null() {
                return Ok(None);
            }
            let value = value.as_str().ok_or_else(|| {
                JsonError::invalid_argument(format!("{key} must be a string or null"))
            })?;
            if key == "contentLanguage" && value.chars().count() > 100 {
                return Err(JsonError::invalid_argument(
                    "contentLanguage exceeds 100 characters",
                ));
            }
            if !value.is_ascii() {
                return Err(JsonError::unsupported_object("non-ASCII fixed metadata"));
            }
            // These fields become HTTP headers; reject control characters before committing state.
            if value.bytes().any(|byte| byte < 32 || byte == 127)
                || HeaderValue::from_str(value).is_err()
            {
                return Err(JsonError::invalid_argument(format!(
                    "Invalid {key} header value"
                )));
            }
            Ok(Some(value.to_owned()))
        })
        .transpose()
}
