use super::JsonError;
use crate::config::ApiBaseSource;
use crate::object::{ObjectError, ObjectMetadataUpdate, ObjectRecord, ObjectTarget};
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
    pub(crate) fn select_target(&self, target: ObjectTarget) -> Result<ObjectTarget, JsonError> {
        for (key, path) in [("bucket", &target.bucket), ("name", &target.name)] {
            if let Some(content) = self.unchanged.get(key).and_then(Value::as_str)
                && content != path
            {
                return Err(JsonError::content_disagrees(content, path));
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
                _ => None,
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
                    .and_then(|owner| owner.entity())
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

/// Whether a metadata write merges into (PATCH) or replaces (PUT) the stored metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteMode {
    Patch,
    Replace,
}

/// Which write a metadata body describes. Only a mutation of an existing generation (PATCH or
/// PUT) may carry `acl`, keeps empty content fields and ignores output-only fields Cloud
/// Storage ignores there; uploads and derived writes reject `acl` and an empty
/// `contentLanguage`.
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
const IGNORED_ON_MUTATION: [&str; 6] = ["id", "selfLink", "timeCreated", "size", "md5Hash", "kind"];

/// Parse metadata mutations separately from upload bodies and server-managed fields.
pub(crate) fn parse_object_metadata(
    bytes: &[u8],
    mode: WriteMode,
) -> Result<ObjectMetadataRequest, JsonError> {
    parse_metadata(bytes, mode, BodyKind::Creation)
}

pub(crate) fn parse_object_metadata_mutation(
    bytes: &[u8],
    mode: WriteMode,
) -> Result<ObjectMetadataRequest, JsonError> {
    parse_metadata(bytes, mode, BodyKind::Mutation)
}

fn parse_metadata(
    bytes: &[u8],
    mode: WriteMode,
    kind: BodyKind,
) -> Result<ObjectMetadataRequest, JsonError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|cause| JsonError::parse_error(bytes, &cause))?;
    let body = value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument("Object resource must be a JSON object"))?;
    for field in ["acl", "predefinedAcl", "contexts", "kmsKeyName"] {
        if field == "acl" && kind == BodyKind::Mutation {
            continue;
        }
        if body.contains_key(field) {
            return Err(JsonError::unsupported_object(field));
        }
    }
    let mut unchanged = unchanged_fields(body)?;
    let metadata = custom_metadata(body)?;
    match kind {
        BodyKind::Mutation => {
            unchanged.retain(|key, _| !IGNORED_ON_MUTATION.contains(&key.as_str()))
        }
        BodyKind::Creation => {
            if body.get("contentLanguage").and_then(Value::as_str) == Some("") {
                return Err(JsonError::invalid_argument(
                    "contentLanguage must not be empty; use null to clear it",
                ));
            }
        }
    }
    let update = ObjectMetadataUpdate {
        acl: body
            .get("acl")
            .map(|value| super::acl::parse_entries(value, crate::acl::AclKind::Object))
            .transpose()?,
        predefined_acl: None,
        replace: mode == WriteMode::Replace,
        content_type: string(body, kind, "contentType")?,
        content_encoding: string(body, kind, "contentEncoding")?,
        content_disposition: string(body, kind, "contentDisposition")?,
        content_language: string(body, kind, "contentLanguage")?,
        cache_control: string(body, kind, "cacheControl")?,
        metadata,
        metadata_mode: crate::object::MapMode::Merge,
        temporary_hold: hold(body, "temporaryHold"),
        custom_time: string(body, kind, "customTime")?,
        event_based_hold: hold(body, "eventBasedHold"),
        retention: body
            .get("retention")
            .map(super::object_retention::parse)
            .transpose()?,
        override_unlocked_retention: crate::object::retention::UnlockedOverride::NotRequested,
        customer_key_sha256: None,
        storage_class: None,
    };
    // A resumable initiation stores this update before anything else validates it.
    update.check_encoded_size().map_err(JsonError::from)?;
    Ok(ObjectMetadataRequest { unchanged, update })
}

/// Collects server-managed fields, which a write may repeat only unchanged, and rejects
/// fields that are neither writable nor server-managed.
fn unchanged_fields(body: &Map<String, Value>) -> Result<Map<String, Value>, JsonError> {
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
            // Cloud Storage ignored a member that is not part of the object resource.
            continue;
        }
    }
    Ok(unchanged)
}

/// A hold flag. Cloud Storage applied a boolean and left the hold alone for `null` or a value
/// of another type (a recorded PATCH with `"yes"` answered 200 and changed nothing visible).
fn hold(body: &Map<String, Value>, key: &str) -> Option<bool> {
    body.get(key).and_then(Value::as_bool)
}

/// A custom metadata change: `None` keeps the entries, `Some(None)` clears them, and a `None`
/// value removes one key.
type CustomMetadataUpdate = Option<Option<BTreeMap<String, Option<String>>>>;

fn custom_metadata(body: &Map<String, Value>) -> Result<CustomMetadataUpdate, JsonError> {
    match body.get("metadata") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::Object(entries)) => custom_metadata_entries(entries).map(Some),
        // A value that is neither an object nor null was ignored (recorded for a string).
        Some(Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_)) => Ok(None),
    }
}

/// The entries of a `metadata` object. `None` clears the stored map: Cloud Storage did that for
/// an object with no entries and when it had dropped every supplied value (recorded 2026-11-09).
fn custom_metadata_entries(
    entries: &Map<String, Value>,
) -> Result<Option<BTreeMap<String, Option<String>>>, JsonError> {
    let mut changes = BTreeMap::new();
    for (key, value) in entries {
        if key.is_empty() {
            return Err(JsonError::unsupported_object("empty custom metadata key"));
        }
        let change = match CustomMetadataValue::from(value) {
            CustomMetadataValue::Store(text) => Some(text),
            CustomMetadataValue::Remove => None,
            CustomMetadataValue::Drop => continue,
        };
        changes.insert(key.clone(), change);
    }
    Ok((!changes.is_empty()).then_some(changes))
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
    /// A `null`, which removes the key.
    Remove,
    /// An object or array, discarded without failing the request.
    Drop,
}

impl From<&Value> for CustomMetadataValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => Self::Remove,
            Value::Bool(flag) => Self::Store(flag.to_string()),
            Value::Number(number) => Self::Store(custom_metadata_number(number)),
            Value::String(text) => Self::Store(text.clone()),
            Value::Array(_) | Value::Object(_) => Self::Drop,
        }
    }
}

fn string(
    body: &Map<String, Value>,
    kind: BodyKind,
    key: &str,
) -> Result<Option<Option<String>>, JsonError> {
    body.get(key)
        .map(|value| {
            if value.is_null() {
                return Ok(None);
            }
            let value = value.as_str().ok_or_else(|| {
                JsonError::invalid_argument(format!("{key} must be a string or null"))
            })?;
            // Cloud Storage stores the empty strings a PATCH sets (recorded 2026-11).
            if !(kind == BodyKind::Mutation && value.is_empty()) {
                validate_fixed_metadata(key, value)?;
            }
            Ok(Some(value.to_owned()))
        })
        .transpose()
}

pub(crate) fn validate_fixed_metadata(key: &str, value: &str) -> Result<(), JsonError> {
    crate::object::validate_fixed_metadata(key, value).map_err(JsonError::from)
}

/// Applies the objects.insert `contentEncoding` query parameter, which Discovery defines as
/// equivalent to the metadata property, on every upload type. A metadata body declaring a
/// different value is rejected rather than silently overridden.
pub(crate) fn declare_content_encoding(
    metadata: &mut ObjectMetadataUpdate,
    encoding: Option<String>,
) -> Result<(), JsonError> {
    let Some(encoding) = encoding else {
        return Ok(());
    };
    validate_fixed_metadata("contentEncoding", &encoding)?;
    if metadata
        .content_encoding
        .as_ref()
        .and_then(Option::as_ref)
        .is_some_and(|declared| declared != &encoding)
    {
        return Err(JsonError::invalid_parameter(
            "Conflicting contentEncoding declarations",
        ));
    }
    metadata.content_encoding = Some(Some(encoding));
    Ok(())
}
