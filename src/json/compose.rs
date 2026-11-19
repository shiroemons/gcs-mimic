use crate::{
    bucket::StorageClass,
    decimal::Grammar,
    json::{ApiError, WriteMode, object_metadata_from_map, validate_object_name},
    object::{ComposeSource, ObjectMetadataUpdate},
};
use serde_json::{Map, Value};

pub(crate) fn route(
    method: &hyper::Method,
    uri: &hyper::Uri,
    profile: crate::config::CompatProfile,
) -> Option<Result<crate::json::ObjectRequest, ApiError>> {
    let path = uri.path().strip_prefix(crate::json::query::BUCKET_PATH)?;
    let (bucket, rest) = path.split_once("/o/")?;
    let name = rest.strip_suffix("/compose")?;
    if bucket.contains('/') || name.contains('/') {
        return None;
    }
    Some(parse(method, bucket, name, uri, profile))
}

/// `bucket` and `name` are the raw path segments the route matched.
fn parse(
    method: &hyper::Method,
    bucket: &str,
    name: &str,
    uri: &hyper::Uri,
    profile: crate::config::CompatProfile,
) -> Result<crate::json::ObjectRequest, ApiError> {
    if method != hyper::Method::POST {
        return Err(ApiError::method_not_allowed(
            "Compose requires POST",
            Some("POST"),
        ));
    }
    let bucket = crate::percent::decode(bucket, crate::percent::Decoding::Path)?;
    crate::bucket::name::validate_for_profile(&bucket, profile)?;
    let name = crate::percent::decode(name, crate::percent::Decoding::Path)?;
    validate_object_name(&name)?;
    let mut query = crate::percent::parse_query(uri.query().unwrap_or_default())?;
    super::Fields::take(&mut query, super::Schema::Object)?;
    let predefined_acl =
        super::objects::take_predefined_acl(&mut query, "destinationPredefinedAcl")?;
    // objects.compose declares no `ifGenerationNotMatch` or `ifMetagenerationNotMatch`, so
    // they fall to the undeclared-parameter rule below: Cloud Storage composed over the
    // current generation with `ifGenerationNotMatch` naming it (case obj-mutation-not-match).
    super::query::standard_parameters(
        &mut query,
        &["kmsKeyName", "dropContextGroups"],
        ApiError::unsupported_object,
    )?;
    // The routing layer applies the value; prettyPrint=false selects compact bodies.
    super::query::boolean(&mut query, "prettyPrint")?;
    match query.remove("alt").as_deref() {
        None | Some("json") => {}
        Some(_) => return Err(ApiError::invalid_parameter("Compose requires alt=json")),
    }
    let mut condition = |key: &str| {
        query
            .remove(key)
            .map(|value| {
                Grammar::Digits
                    .parse::<i64>(&value)
                    .ok_or_else(|| ApiError::invalid_parameter(format!("Invalid {key}")))
            })
            .transpose()
    };
    let conditions = crate::object::ObjectConditions {
        if_generation_match: condition("ifGenerationMatch")?,
        if_metageneration_match: condition("ifMetagenerationMatch")?,
        ..Default::default()
    };
    // `projection` is a defined parameter this route does not implement.
    if query.contains_key("projection") {
        return Err(ApiError::invalid_parameter(
            "Unknown query parameter for this method: projection",
        ));
    }
    // Cloud Storage ignored a query parameter it does not define, such as the gRPC-only
    // `deleteSourceObjects`: the sources survived a recorded compose that carried it.
    Ok(crate::json::ObjectRequest::Compose(ComposeParams {
        bucket,
        name,
        conditions,
        predefined_acl,
    }))
}

/// The destination and options an objects.compose path and query name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposeParams {
    pub bucket: String,
    pub name: String,
    pub conditions: crate::object::ObjectConditions,
    /// Replaces the bucket's default object ACL on the composed object.
    pub predefined_acl: Option<crate::acl::PredefinedObjectAcl>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposeRequest {
    pub sources: Vec<ComposeSource>,
    pub destination: ObjectMetadataUpdate,
    /// The destination class the request names; it must match every component's class.
    pub storage_class: Option<StorageClass>,
    pub delete_sources: bool,
}

pub fn parse_compose(bytes: &[u8], bucket: &str) -> Result<ComposeRequest, ApiError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|cause| crate::json::parse_error(bytes, &cause))?;
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
        return Err(ApiError::invalid_argument("Invalid compose request kind"));
    }
    let delete_sources = body
        .get("deleteSourceObjects")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| ApiError::invalid_argument("deleteSourceObjects must be a boolean"))
        })
        .transpose()?
        .unwrap_or(false);
    if body
        .get("sourceObjects")
        .is_none_or(|sources| sources.as_array().is_some_and(Vec::is_empty))
    {
        return Err(ApiError::required(
            "You must provide at least one source component.",
        ));
    }
    let sources = body
        .get("sourceObjects")
        .and_then(Value::as_array)
        .filter(|sources| (1..=32).contains(&sources.len()))
        .ok_or_else(|| ApiError::invalid_argument("sourceObjects must contain 1 to 32 objects"))?;
    let sources = sources
        .iter()
        .map(parse_source)
        .collect::<Result<_, ApiError>>()?;
    let (destination, storage_class) = destination_metadata(body, bucket)?;
    Ok(ComposeRequest {
        sources,
        destination,
        storage_class,
        delete_sources,
    })
}

fn parse_source(value: &Value) -> Result<ComposeSource, ApiError> {
    let source = object(value, "Source object")?;
    known_fields(source, &["name", "generation", "objectPreconditions"])?;
    let name = source
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::invalid_argument("Source name must be a string"))?;
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
        generation: integer(
            source.get("generation").filter(|value| !value.is_null()),
            "generation",
        )?,
        if_generation_match: integer(
            conditions.and_then(|conditions| conditions.get("ifGenerationMatch")),
            "ifGenerationMatch",
        )?,
    })
}

/// The destination resource's metadata and requested class. Its bucket and kind must match
/// the request.
fn destination_metadata(
    body: &Map<String, Value>,
    bucket: &str,
) -> Result<(ObjectMetadataUpdate, Option<StorageClass>), ApiError> {
    let mut destination = body
        .get("destination")
        .filter(|value| !value.is_null())
        .map(|value| object(value, "Destination object").cloned())
        .transpose()?
        .unwrap_or_default();
    // The path names the destination: Cloud Storage composes at the path name even when the
    // destination resource names another object (recorded 2026-11-13).
    if destination
        .remove("name")
        .is_some_and(|value| !value.is_string())
    {
        return Err(ApiError::invalid_argument(
            "Destination name must be a string",
        ));
    }
    for (key, expected) in [("bucket", bucket), ("kind", "storage#object")] {
        if let Some(value) = destination.remove(key)
            && value.as_str() != Some(expected)
        {
            return Err(ApiError::invalid_argument(format!(
                "Destination {key} differs from request"
            )));
        }
    }
    // The JSON composite-object guide explicitly ignores supplied destination hashes.
    destination.remove("md5Hash");
    destination.remove("crc32c");
    // Cloud Storage passes over an output-only generation and size and judges the requested
    // class against the components instead (recorded 2026-11-13).
    destination.remove("generation");
    destination.remove("size");
    let storage_class = destination
        .remove("storageClass")
        .filter(|value| !value.is_null())
        .map(|value| {
            let class = value
                .as_str()
                .ok_or_else(|| ApiError::invalid_argument("storageClass must be a string"))?;
            crate::object::validate_object_storage_class(class).map_err(ApiError::from)
        })
        .transpose()?;
    let metadata = object_metadata_from_map(&destination, WriteMode::Replace)?;
    // Holds, a custom time and a retention apply as on an upload: Cloud Storage composed an
    // object held and with the requested custom time (recorded 2026-11-13, case
    // obj-unsupported-derived), and publication checks a retention against the bucket.
    metadata.update.validate().map_err(ApiError::from)?;
    if let Some(key) = metadata.unchanged.keys().next() {
        return Err(ApiError::unsupported_object(key));
    }
    Ok((metadata.update, storage_class))
}

fn object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>, ApiError> {
    value
        .as_object()
        .ok_or_else(|| ApiError::invalid_argument(format!("{label} must be a JSON object")))
}

fn known_fields(body: &Map<String, Value>, allowed: &[&str]) -> Result<(), ApiError> {
    if let Some(key) = body.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ApiError::unsupported_object(key));
    }
    Ok(())
}

fn integer(value: Option<&Value>, label: &str) -> Result<Option<i64>, ApiError> {
    value
        .map(|value| {
            value
                .as_str()
                .and_then(|value| Grammar::Digits.parse::<i64>(value))
                .or_else(|| value.as_i64())
                .filter(|value| *value >= 0)
                .ok_or_else(|| ApiError::invalid_argument(format!("Invalid {label}")))
        })
        .transpose()
}
