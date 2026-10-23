use crate::{
    json::{JsonError, parse_object_metadata, validate_object_name},
    object::{ComposeSource, ObjectMetadataUpdate},
};
use hyper::StatusCode;
use serde_json::{Map, Value};

pub(crate) fn route(
    method: &hyper::Method,
    uri: &hyper::Uri,
    profile: crate::config::CompatProfile,
) -> Option<Result<crate::json::ObjectRequest, JsonError>> {
    let path = uri.path().strip_prefix("/storage/v1/b/")?;
    let (bucket, rest) = path.split_once("/o/")?;
    let name = rest.strip_suffix("/compose")?;
    if bucket.contains('/') || name.contains('/') {
        return None;
    }
    Some((|| {
        if method != hyper::Method::POST {
            let mut error = JsonError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "methodNotAllowed",
                "Compose requires POST",
            );
            error.allow = Some("POST");
            return Err(error);
        }
        let bucket = crate::json::decode(bucket, false)?;
        crate::json::validate_bucket_name_for_profile(&bucket, profile)?;
        let name = crate::json::decode(name, false)?;
        validate_object_name(&name)?;
        let mut query = crate::json::parse_query(uri.query().unwrap_or_default())?;
        crate::json::Fields::take(&mut query, "Object")?;
        crate::json::query::projection(&mut query)?;
        for key in [
            "destinationPredefinedAcl",
            "kmsKeyName",
            "userProject",
            "dropContextGroups",
            "ifGenerationNotMatch",
            "ifMetagenerationNotMatch",
            "key",
            "oauth_token",
            "quotaUser",
            "userIp",
            "callback",
        ] {
            if query.contains_key(key) {
                return Err(JsonError::unsupported_object(key));
            }
        }
        match query.remove("prettyPrint").as_deref() {
            None | Some("false") => {}
            Some("true") => return Err(JsonError::unsupported_object("prettyPrint")),
            Some(_) => {
                return Err(JsonError::invalid_parameter(
                    "prettyPrint must be a boolean",
                ));
            }
        }
        match query.remove("alt").as_deref() {
            None | Some("json") => {}
            Some(_) => return Err(JsonError::invalid_parameter("Compose requires alt=json")),
        }
        let mut condition = |key: &str| {
            query
                .remove(key)
                .map(|value| {
                    value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit())
                        .then(|| value.parse::<i64>().ok())
                        .flatten()
                        .filter(|value| *value >= 0)
                        .ok_or_else(|| JsonError::invalid_parameter(format!("Invalid {key}")))
                })
                .transpose()
        };
        let conditions = crate::object::ObjectConditions {
            if_generation_match: condition("ifGenerationMatch")?,
            if_metageneration_match: condition("ifMetagenerationMatch")?,
            ..Default::default()
        };
        crate::json::query::finish(query)?;
        Ok(crate::json::ObjectRequest::Compose {
            bucket,
            name,
            conditions,
        })
    })())
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
    if metadata
        .update
        .metadata
        .as_ref()
        .and_then(Option::as_ref)
        .is_some_and(|entries| {
            entries
                .iter()
                .filter_map(|(key, value)| value.as_ref().map(|value| key.len() + value.len()))
                .sum::<usize>()
                > 8192
        })
    {
        return Err(JsonError::invalid_argument("Custom metadata exceeds 8 KiB"));
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
