use crate::{
    json::{JsonError, parse_object_metadata, validate_object_name},
    object::ObjectMetadataUpdate,
};
use hyper::StatusCode;
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposeSource {
    pub name: String,
    pub generation: Option<i64>,
    pub if_generation_match: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposeRequest {
    pub sources: Vec<ComposeSource>,
    pub destination: ObjectMetadataUpdate,
    pub delete_sources: bool,
}

pub fn parse_compose(bytes: &[u8], bucket: &str, name: &str) -> Result<ComposeRequest, JsonError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "parseError",
            "Invalid compose JSON",
        )
    })?;
    let body = object(&value, "Compose request")?;
    known_fields(
        body,
        &[
            "kind",
            "sourceObjects",
            "destination",
            "deleteSourceObjects",
        ],
    )?;
    if body
        .get("kind")
        .is_some_and(|value| value.as_str() != Some("storage#composeRequest"))
    {
        return Err(JsonError::invalid_argument("Invalid compose request kind"));
    }
    let delete_sources = body
        .get("deleteSourceObjects")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| JsonError::invalid_argument("deleteSourceObjects must be a boolean"))
        })
        .transpose()?
        .unwrap_or(false);
    let sources = body
        .get("sourceObjects")
        .and_then(Value::as_array)
        .filter(|sources| (1..=32).contains(&sources.len()))
        .ok_or_else(|| JsonError::invalid_argument("sourceObjects must contain 1 to 32 objects"))?;
    let sources = sources
        .iter()
        .map(|value| {
            let source = object(value, "Source object")?;
            known_fields(source, &["name", "generation", "objectPreconditions"])?;
            let name = source
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| JsonError::invalid_argument("Source name must be a string"))?;
            validate_object_name(name)?;
            let conditions = source
                .get("objectPreconditions")
                .map(|value| object(value, "Source preconditions"))
                .transpose()?;
            if let Some(conditions) = conditions {
                known_fields(conditions, &["ifGenerationMatch"])?;
            }
            Ok(ComposeSource {
                name: name.to_owned(),
                generation: integer(source.get("generation"), "generation")?,
                if_generation_match: integer(
                    conditions.and_then(|conditions| conditions.get("ifGenerationMatch")),
                    "ifGenerationMatch",
                )?,
            })
        })
        .collect::<Result<_, JsonError>>()?;
    let mut destination = body
        .get("destination")
        .map(|value| object(value, "Destination object").cloned())
        .transpose()?
        .unwrap_or_default();
    for (key, expected) in [
        ("bucket", bucket),
        ("name", name),
        ("kind", "storage#object"),
    ] {
        if let Some(value) = destination.remove(key)
            && value.as_str() != Some(expected)
        {
            return Err(JsonError::invalid_argument(format!(
                "Destination {key} differs from request"
            )));
        }
    }
    // The JSON composite-object guide explicitly ignores supplied destination hashes.
    destination.remove("md5Hash");
    destination.remove("crc32c");
    let metadata =
        parse_object_metadata(&Value::Object(destination).to_string().into_bytes(), true)?;
    if let Some(key) = metadata.unchanged.keys().next() {
        return Err(JsonError::unsupported_object(key));
    }
    Ok(ComposeRequest {
        sources,
        destination: metadata.update,
        delete_sources,
    })
}

fn object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>, JsonError> {
    value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument(format!("{label} must be a JSON object")))
}

fn known_fields(body: &Map<String, Value>, allowed: &[&str]) -> Result<(), JsonError> {
    if let Some(key) = body.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(JsonError::unsupported_object(key));
    }
    Ok(())
}

fn integer(value: Option<&Value>, label: &str) -> Result<Option<i64>, JsonError> {
    value
        .map(|value| {
            value
                .as_str()
                .and_then(|value| {
                    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                        .then(|| value.parse::<i64>().ok())
                        .flatten()
                })
                .or_else(|| value.as_i64())
                .filter(|value| *value >= 0)
                .ok_or_else(|| JsonError::invalid_argument(format!("Invalid {label}")))
        })
        .transpose()
}
