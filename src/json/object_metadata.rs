use super::ApiError;
use crate::config::ApiBaseSource;
use crate::object::{
    MapPatch, ObjectError, ObjectMetadataUpdate, ObjectRecord, ObjectTarget, Patch, WriteMode,
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub(crate) struct ObjectMetadataRequest {
    pub update: ObjectMetadataUpdate,
    pub unchanged: Map<String, Value>,
}

impl ObjectMetadataRequest {
    /// Checks the body's `bucket` and `name` against the path and lets its `generation` select
    /// the generation when the query names none: Cloud Storage answered a PATCH whose body
    /// named a generation that did not exist with 404 (recorded 2026-11). A query generation
    /// stays the selector, and the body must then repeat it unchanged.
    pub(crate) fn select_target(&self, target: ObjectTarget) -> Result<ObjectTarget, ApiError> {
        for (key, path) in [("bucket", &target.bucket), ("name", &target.name)] {
            if let Some(content) = self.unchanged.get(key).and_then(Value::as_str)
                && content != path
            {
                return Err(ApiError::content_disagrees(content, path));
            }
        }
        if target.generation.is_some() {
            return Ok(target);
        }
        let generation = self
            .unchanged
            .get("generation")
            .and_then(|value| match value {
                Value::String(text) => text.parse::<i64>().ok(),
                Value::Number(number) => number.as_i64(),
                Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
            });
        Ok(ObjectTarget {
            generation,
            ..target
        })
    }

    pub(crate) fn validate_unchanged(
        unchanged: &Map<String, Value>,
        record: &ObjectRecord,
        base: &str,
        source: ApiBaseSource,
    ) -> Result<(), ObjectError> {
        let resource = super::object_resource(record, base, super::Projection::Full)
            .map_err(ObjectError::backend)?;
        for (key, value) in unchanged {
            // The Java SDK uses generation:null for an unspecified generation.
            if key == "generation" && value.is_null() {
                continue;
            }
            let same_integer = matches!(key.as_str(), "generation" | "metageneration" | "size")
                && value.as_u64().is_some_and(|number| {
                    resource.get(key).and_then(Value::as_str) == Some(number.to_string().as_str())
                });
            let same_owner = key == "owner"
                && record
                    .owner
                    .as_ref()
                    .and_then(crate::object::owner::ObjectOwner::entity)
                    .is_some_and(|entity| *value == serde_json::json!({"entity":entity}));
            let same_link = match source {
                ApiBaseSource::Explicit => false,
                // A derived link names whichever Host read the resource, so a client may repeat
                // one it read through another address.
                ApiBaseSource::Derived => {
                    matches!(key.as_str(), "selfLink" | "mediaLink")
                        && value
                            .as_str()
                            .zip(resource.get(key).and_then(Value::as_str))
                            .is_some_and(|(sent, rendered)| {
                                crate::url::same_below_origin(sent, rendered)
                            })
                }
            };
            if resource.get(key) != Some(value) && !same_integer && !same_owner && !same_link {
                return Err(ObjectError::UnsupportedMetadata(key.clone()));
            }
        }
        Ok(())
    }
}

/// Which write a metadata body describes. Only a mutation of an existing generation (PATCH or
/// PUT) may carry `acl` and ignores output-only fields Cloud Storage ignores there; uploads and
/// derived writes reject `acl`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    Mutation,
    Creation,
}

/// Output-only fields a PATCH or PUT may carry with any value: Cloud Storage answered a PUT
/// whose `size` and `md5Hash` disagreed with the object with 200 and ignored them (recorded
/// 2026-11). That `id`, `selfLink` and `timeCreated` are ignored the same way is an inference
/// from a PATCH carrying them that failed only on its `generation`. A PATCH whose `kind` named
/// another resource was answered with 200 and `storage#object` (case obj-unsupported-inputs).
/// A PUT repeating a superseded read whole, as the .NET client's UpdateObject sends it, and a
/// PATCH naming a superseded `etag` both applied, ignoring the stale `etag`, `metageneration`
/// and `updated` (recorded 2026-11-17, case obj-stale-body-update). A body `generation` still
/// selects a generation, so it is not among them.
const IGNORED_ON_MUTATION: &[&str] = &[
    "id",
    "selfLink",
    "timeCreated",
    "size",
    "md5Hash",
    "kind",
    "etag",
    "metageneration",
    "updated",
];

/// [`object_metadata_from_map`] for a body that is still JSON text. Every production caller
/// already holds the resource as members.
#[cfg(test)]
pub(crate) fn parse_object_metadata(
    bytes: &[u8],
    mode: WriteMode,
) -> Result<ObjectMetadataRequest, ApiError> {
    parse_metadata(bytes, mode, BodyKind::Creation)
}

/// Parse a PATCH or PUT body, which may carry `acl` and keeps empty content fields.
pub(crate) fn parse_object_metadata_mutation(
    bytes: &[u8],
    mode: WriteMode,
) -> Result<ObjectMetadataRequest, ApiError> {
    parse_metadata(bytes, mode, BodyKind::Mutation)
}

/// Parse the metadata of a new object (an upload or a derived write) separately from upload
/// bodies and server-managed fields.
pub(crate) fn object_metadata_from_map(
    body: &Map<String, Value>,
    mode: WriteMode,
) -> Result<ObjectMetadataRequest, ApiError> {
    metadata_from_map(body, mode, BodyKind::Creation)
}

/// The members of a body that must be an object resource.
pub(crate) fn resource_object(value: &Value) -> Result<&Map<String, Value>, ApiError> {
    value
        .as_object()
        .ok_or_else(|| ApiError::invalid_argument("Object resource must be a JSON object"))
}

fn parse_metadata(
    bytes: &[u8],
    mode: WriteMode,
    kind: BodyKind,
) -> Result<ObjectMetadataRequest, ApiError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|cause| crate::json::parse_error(bytes, &cause))?;
    metadata_from_map(resource_object(&value)?, mode, kind)
}

fn metadata_from_map(
    body: &Map<String, Value>,
    mode: WriteMode,
    kind: BodyKind,
) -> Result<ObjectMetadataRequest, ApiError> {
    for field in ["acl", "predefinedAcl", "contexts", "kmsKeyName"] {
        if field == "acl" && kind == BodyKind::Mutation {
            continue;
        }
        if body.contains_key(field) {
            return Err(ApiError::unsupported_object(field));
        }
    }
    let mut unchanged = unchanged_fields(body);
    // Cloud Storage removed a key named with null only on a PATCH. An insert, a resumable start,
    // a copy, a rewrite, a compose destination and a PUT stored the empty string instead
    // (recorded 2026-11-15).
    let null = if kind == BodyKind::Mutation && mode == WriteMode::Patch {
        NullValue::Removes
    } else {
        NullValue::StoresEmpty
    };
    let metadata = custom_metadata(body, null);
    if kind == BodyKind::Mutation {
        unchanged.retain(|key, _| !IGNORED_ON_MUTATION.contains(&key.as_str()));
    }
    let update = ObjectMetadataUpdate {
        acl: body
            .get("acl")
            .map(|value| super::acl::parse_entries(value, crate::acl::AclKind::Object))
            .transpose()?
            .into(),
        write: mode,
        content_type: string(body, "contentType")?,
        content_encoding: string(body, "contentEncoding")?,
        content_disposition: string(body, "contentDisposition")?,
        content_language: string(body, "contentLanguage")?,
        cache_control: string(body, "cacheControl")?,
        metadata,
        temporary_hold: hold(body, "temporaryHold"),
        custom_time: string(body, "customTime")?,
        event_based_hold: hold(body, "eventBasedHold"),
        retention: body
            .get("retention")
            .map(super::object_retention::parse)
            .transpose()?
            .map_or(Patch::Keep, Patch::nullable),
        override_unlocked_retention: crate::object::retention::UnlockedOverride::NotRequested,
        customer_key_sha256: None,
        storage_class: None,
    };
    // A resumable initiation stores this update before anything else validates it.
    update.check_encoded_size().map_err(ApiError::from)?;
    Ok(ObjectMetadataRequest { unchanged, update })
}

/// Collects server-managed fields, which a write may repeat only unchanged. Cloud Storage
/// ignored a member that is not part of the object resource, so the writable fields and any
/// unknown member pass through.
fn unchanged_fields(body: &Map<String, Value>) -> Map<String, Value> {
    let mut unchanged = Map::new();
    // shortcut: changed output-only and unknown inputs need legacy service evidence; accept only identical resource values.
    for (key, value) in body {
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
            unchanged.insert(key.clone(), value.clone());
        }
    }
    unchanged
}

/// A hold flag. Cloud Storage applied a boolean and left the hold alone for `null` or a value
/// of another type (a recorded PATCH with `"yes"` answered 200 and changed nothing visible).
fn hold(body: &Map<String, Value>, key: &str) -> Option<bool> {
    body.get(key).and_then(Value::as_bool)
}

/// What a null custom metadata value means to a write.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NullValue {
    /// A PATCH removes the key.
    Removes,
    /// Every other write stores the key with an empty value.
    StoresEmpty,
}

/// A custom metadata change; a JSON body merges its entries, and `null` says what a null value
/// does to its key.
fn custom_metadata(body: &Map<String, Value>, null: NullValue) -> MapPatch {
    match body.get("metadata") {
        None => MapPatch::Keep,
        Some(Value::Null) => MapPatch::Clear,
        Some(Value::Object(entries)) => {
            custom_metadata_entries(entries, null).map_or(MapPatch::Clear, MapPatch::Merge)
        }
        // A value that is neither an object nor null was ignored (recorded for a string).
        Some(Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_)) => {
            MapPatch::Keep
        }
    }
}

/// The entries of a `metadata` object. `None` clears the stored map: Cloud Storage did that for
/// an object with no entries and when it had dropped every supplied value (recorded 2026-11-09).
fn custom_metadata_entries(
    entries: &Map<String, Value>,
    null: NullValue,
) -> Option<BTreeMap<String, Option<String>>> {
    let mut changes = BTreeMap::new();
    for (key, value) in entries {
        let change = match (CustomMetadataValue::from(value), null) {
            (CustomMetadataValue::Store(text), NullValue::Removes | NullValue::StoresEmpty) => {
                Some(text)
            }
            (CustomMetadataValue::Null, NullValue::Removes) => None,
            (CustomMetadataValue::Null, NullValue::StoresEmpty) => Some(String::new()),
            (CustomMetadataValue::Drop, NullValue::Removes | NullValue::StoresEmpty) => continue,
        };
        changes.insert(key.clone(), change);
    }
    (!changes.is_empty()).then_some(changes)
}

/// The text Cloud Storage stored for a JSON number on 2026-11-09. Integers within 64 bits keep
/// their digits. Anything else is a double, written like Java's `Double.toString`: plain from
/// 0.001 up to 10^7 and scientific outside that.
fn custom_metadata_number(number: &serde_json::Number) -> String {
    match number.as_f64() {
        Some(value) if number.is_f64() => custom_metadata_float(value),
        Some(_) | None => number.to_string(),
    }
}

fn custom_metadata_float(value: f64) -> String {
    /// From here up Cloud Storage kept 16 significant digits instead of the shortest text.
    const SIXTEEN_DIGITS_FROM: f64 = 18_446_744_073_709_551_616.0;
    if value.classify() == std::num::FpCategory::Zero {
        // Cloud Storage's own text for a zero, for every spelling recorded.
        return if value.is_sign_negative() {
            "-0.0E"
        } else {
            "0.0E"
        }
        .to_owned();
    }
    let magnitude = value.abs();
    if (1e-3..1e7).contains(&magnitude) {
        return format!("{value:?}");
    }
    let scientific = if magnitude >= SIXTEEN_DIGITS_FROM {
        format!("{value:.15E}")
    } else {
        format!("{value:E}")
    };
    let Some((mantissa, exponent)) = scientific.split_once('E') else {
        return scientific;
    };
    let digits = mantissa.trim_end_matches('0').trim_end_matches('.');
    let fraction = if digits.contains('.') { "" } else { ".0" };
    format!("{digits}{fraction}E{exponent}")
}

/// What one supplied custom metadata value does to its key, as recorded from Cloud Storage on
/// 2026-11-09.
enum CustomMetadataValue {
    /// A scalar, kept as its text.
    Store(String),
    /// A `null`, whose meaning depends on the write ([`NullValue`]).
    Null,
    /// An object or array, discarded without failing the request.
    Drop,
}

impl From<&Value> for CustomMetadataValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(flag) => Self::Store(flag.to_string()),
            Value::Number(number) => Self::Store(custom_metadata_number(number)),
            Value::String(text) => Self::Store(text.clone()),
            Value::Array(_) | Value::Object(_) => Self::Drop,
        }
    }
}

/// A string field of the resource. An empty string is a value: Cloud Storage stored the empty
/// content fields a PATCH or an upload set (recorded 2026-11, cases obj-metadata-output-only and
/// obj-gaps-json).
fn string(body: &Map<String, Value>, key: &str) -> Result<Patch<String>, ApiError> {
    let Some(value) = body.get(key) else {
        return Ok(Patch::Keep);
    };
    if value.is_null() {
        return Ok(Patch::Clear);
    }
    let value = value
        .as_str()
        .ok_or_else(|| ApiError::invalid_argument(format!("{key} must be a string or null")))?;
    validate_fixed_metadata(key, value)?;
    Ok(Patch::Set(value.to_owned()))
}

pub(crate) fn validate_fixed_metadata(key: &str, value: &str) -> Result<(), ApiError> {
    crate::object::validate_fixed_metadata(key, value).map_err(ApiError::from)
}

/// Applies the objects.insert `contentEncoding` query parameter, which Discovery defines as
/// equivalent to the metadata property, on every upload type. A metadata body declaring a
/// different value is rejected rather than silently overridden.
pub(crate) fn declare_content_encoding(
    metadata: &mut ObjectMetadataUpdate,
    encoding: Option<String>,
) -> Result<(), ApiError> {
    let Some(encoding) = encoding else {
        return Ok(());
    };
    validate_fixed_metadata("contentEncoding", &encoding)?;
    if metadata
        .content_encoding
        .value()
        .is_some_and(|declared| declared != &encoding)
    {
        return Err(ApiError::invalid_parameter(
            "Conflicting contentEncoding declarations",
        ));
    }
    metadata.content_encoding = Patch::Set(encoding);
    Ok(())
}
