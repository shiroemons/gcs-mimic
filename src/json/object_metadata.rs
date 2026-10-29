use super::JsonError;
use crate::object::{ObjectError, ObjectMetadataUpdate, ObjectRecord};
use hyper::StatusCode;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

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

/// Whether a metadata write merges into (PATCH) or replaces (PUT) the stored metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    Patch,
    Replace,
}

/// Whether a metadata body may carry an `acl` field. Only object metadata mutations change
/// the ACL; uploads and derived writes reject it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AclField {
    Accepted,
    Rejected,
}

/// Parse metadata mutations separately from upload bodies and server-managed fields.
pub fn parse_object_metadata(
    bytes: &[u8],
    mode: WriteMode,
) -> Result<ObjectMetadataRequest, JsonError> {
    parse_metadata(bytes, mode, AclField::Rejected)
}

pub(crate) fn parse_object_metadata_mutation(
    bytes: &[u8],
    mode: WriteMode,
) -> Result<ObjectMetadataRequest, JsonError> {
    parse_metadata(bytes, mode, AclField::Accepted)
}

fn parse_metadata(
    bytes: &[u8],
    mode: WriteMode,
    acl: AclField,
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
    for field in ["acl", "predefinedAcl", "contexts", "kmsKeyName"] {
        if field == "acl" && acl == AclField::Accepted {
            continue;
        }
        if body.contains_key(field) {
            return Err(JsonError::unsupported_object(field));
        }
    }
    let unchanged = unchanged_fields(body)?;
    let metadata = custom_metadata(body)?;
    if body.get("contentLanguage").and_then(Value::as_str) == Some("") {
        return Err(JsonError::invalid_argument(
            "contentLanguage must not be empty; use null to clear it",
        ));
    }
    Ok(ObjectMetadataRequest {
        unchanged,
        update: ObjectMetadataUpdate {
            acl: body
                .get("acl")
                .map(|value| super::acl::parse_entries(value, crate::acl::AclKind::Object))
                .transpose()?,
            replace: mode == WriteMode::Replace,
            content_type: string(body, "contentType")?,
            content_encoding: string(body, "contentEncoding")?,
            content_disposition: string(body, "contentDisposition")?,
            content_language: string(body, "contentLanguage")?,
            cache_control: string(body, "cacheControl")?,
            metadata,
            temporary_hold: body
                .get("temporaryHold")
                .filter(|v| !v.is_null())
                .map(|value| {
                    value.as_bool().ok_or_else(|| {
                        JsonError::invalid_argument(
                            "temporaryHold must be a boolean, for example true or false",
                        )
                    })
                })
                .transpose()?,
            custom_time: string(body, "customTime")?,
            event_based_hold: body
                .get("eventBasedHold")
                .map(|value| {
                    value.as_bool().ok_or_else(|| {
                        JsonError::invalid_argument("eventBasedHold must be a boolean")
                    })
                })
                .transpose()?,
            retention: body
                .get("retention")
                .map(super::object_retention::parse)
                .transpose()?,
            override_unlocked_retention: false,
            customer_key_sha256: None,
            storage_class: None,
        },
    })
}

/// Collects server-managed fields, which a write may repeat only unchanged, and rejects
/// fields that are neither writable nor server-managed.
fn unchanged_fields(body: &Map<String, Value>) -> Result<Map<String, Value>, JsonError> {
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
                | "customerEncryption"
        ) {
            unchanged.insert(key.clone(), body[key].clone());
            continue;
        }
        if !matches!(
            key.as_str(),
            "contentType"
                | "acl"
                | "contentEncoding"
                | "contentDisposition"
                | "contentLanguage"
                | "cacheControl"
                | "metadata"
                | "eventBasedHold"
                | "temporaryHold"
                | "customTime"
                | "retention"
        ) {
            return Err(JsonError::unsupported_object(key));
        }
    }
    Ok(unchanged)
}

/// A custom metadata change: `None` keeps the entries, `Some(None)` clears them, and a `None`
/// value removes one key.
type CustomMetadataUpdate = Option<Option<BTreeMap<String, Option<String>>>>;

fn custom_metadata(body: &Map<String, Value>) -> Result<CustomMetadataUpdate, JsonError> {
    body.get("metadata")
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
        .transpose()
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
            validate_fixed_metadata(key, value)?;
            Ok(Some(value.to_owned()))
        })
        .transpose()
}

pub(crate) fn validate_fixed_metadata(key: &str, value: &str) -> Result<(), JsonError> {
    crate::object::validate_fixed_metadata(key, value).map_err(JsonError::from)
}
