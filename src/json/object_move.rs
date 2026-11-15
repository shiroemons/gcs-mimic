use super::{
    JsonError, ObjectRequest, Projection,
    objects::{ConditionKeys, match_and_not_match, take_conditions},
    query::{
        BUCKET_PATH, Decoding, MAX_BUCKET_SEGMENT_BYTES, MAX_OBJECT_SEGMENT_BYTES, decode, finish,
        parse_query, projection, standard_parameters,
    },
};
use crate::{
    config::CompatProfile,
    object::ObjectConditions,
    storage::{MoveConditions, MoveNames},
};
use hyper::{Method, Uri};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObjectMoveParams {
    pub names: MoveNames,
    pub conditions: MoveConditions,
    pub projection: Projection,
}

pub(super) fn route(
    method: &Method,
    uri: &Uri,
    profile: CompatProfile,
) -> Option<Result<ObjectRequest, JsonError>> {
    let path = uri.path().strip_prefix(BUCKET_PATH)?;
    let (source, destination) = path.split_once("/moveTo/o/")?;
    Some(parse(method, source, destination, uri, profile))
}

/// `source` is the raw `{bucket}/o/{name}` the route matched, which this checks for a move
/// path's shape, and `destination` the raw destination name.
fn parse(
    method: &Method,
    source: &str,
    destination: &str,
    uri: &Uri,
    profile: CompatProfile,
) -> Result<ObjectRequest, JsonError> {
    if method != Method::POST {
        return Err(JsonError::method_not_allowed(
            "Object move requires POST",
            Some("POST"),
        ));
    }
    let (bucket, source) = source
        .split_once("/o/")
        .ok_or_else(|| JsonError::invalid_parameter("Invalid move path"))?;
    if bucket.len() > MAX_BUCKET_SEGMENT_BYTES
        || bucket.contains('/')
        || source.contains('/')
        || destination.contains('/')
        || source.len() > MAX_OBJECT_SEGMENT_BYTES
        || destination.len() > MAX_OBJECT_SEGMENT_BYTES
    {
        return Err(JsonError::invalid_parameter("Invalid move path"));
    }
    let bucket = decode(bucket, Decoding::Path)?;
    super::validate_bucket_name_for_profile(&bucket, profile)?;
    let source = decode(source, Decoding::Path)?;
    let destination = decode(destination, Decoding::Path)?;
    super::validate_object_name(&source)?;
    super::validate_object_name(&destination)?;
    if source == destination {
        return Err(JsonError::invalid(
            "Source and destination object names must be different.",
        ));
    }
    let mut query = parse_query(uri.query().unwrap_or_default())?;
    super::Fields::take(&mut query, super::Schema::Object)?;
    let projection = projection(&mut query)?;
    if projection == Projection::Full {
        return Err(JsonError::unsupported_object(
            "move projection=full; use projection=noAcl",
        ));
    }
    standard_parameters(&mut query, &[], JsonError::unsupported_object)?;
    // The routing layer applies the value; prettyPrint=false selects compact bodies.
    super::query::boolean(&mut query, "prettyPrint")?;
    if query.remove("alt").is_some_and(|alt| alt != "json") {
        return Err(JsonError::invalid_parameter("move requires alt=json"));
    }
    let source_conditions = exclusive_conditions(&mut query, ConditionKeys::Source)?;
    let destination_conditions = exclusive_conditions(&mut query, ConditionKeys::Plain)?;
    finish(&query)?;
    Ok(ObjectRequest::Move(ObjectMoveParams {
        names: MoveNames {
            bucket,
            source,
            destination,
        },
        conditions: MoveConditions {
            source: source_conditions,
            destination: destination_conditions,
        },
        projection,
    }))
}

/// One side's preconditions. `storage.proto` makes each Match and NotMatch pair of
/// MoveObjectRequest mutually exclusive, and Cloud Storage refuses such a pair as a conflict.
fn exclusive_conditions(
    query: &mut BTreeMap<String, String>,
    keys: ConditionKeys,
) -> Result<ObjectConditions, JsonError> {
    let conditions = take_conditions(query, keys)?;
    if match_and_not_match(&conditions) {
        return Err(JsonError::conflicting_preconditions());
    }
    Ok(conditions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::StatusCode;

    #[test]
    fn move_route_decodes_names_once_and_keeps_conditions_separate() {
        let uri = "/storage/v1/b/bucket/o/source%2F%252F/moveTo/o/target%2F%252F?ifSourceGenerationMatch=7&ifGenerationMatch=0&ifSourceMetagenerationNotMatch=3&ifMetagenerationMatch=4&prettyPrint=false&alt=json".parse().unwrap();
        let ObjectRequest::Move(params) = route(&Method::POST, &uri, CompatProfile::Native)
            .unwrap()
            .unwrap()
        else {
            panic!("expected move route")
        };
        assert_eq!(params.names.bucket, "bucket");
        assert_eq!(params.names.source, "source/%2F");
        assert_eq!(params.names.destination, "target/%2F");
        assert_eq!(params.conditions.source.if_generation_match, Some(7));
        assert_eq!(params.conditions.destination.if_generation_match, Some(0));
        assert_eq!(
            params.conditions.source.if_metageneration_not_match,
            Some(3)
        );
        assert_eq!(
            params.conditions.destination.if_metageneration_match,
            Some(4)
        );
        let error = route(&Method::GET, &uri, CompatProfile::Native)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(error.allow, Some("POST"));
    }
}
