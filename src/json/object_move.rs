use super::{
    JsonError, ObjectRequest, Projection,
    query::{Decoding, decode, finish, parse_query, projection},
};
use crate::{
    config::CompatProfile,
    object::ObjectConditions,
    storage::{MoveConditions, MoveNames},
};
use hyper::{Method, Uri};

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
    let path = uri.path().strip_prefix("/storage/v1/b/")?;
    let (source, destination) = path.split_once("/moveTo/o/")?;
    Some((|| {
        if method != Method::POST {
            return Err(JsonError::method_not_allowed(
                "Object move requires POST",
                Some("POST"),
            ));
        }
        let (bucket, source) = source
            .split_once("/o/")
            .ok_or_else(|| JsonError::invalid_parameter("Invalid move path"))?;
        if bucket.len() > 222 * 3
            || bucket.contains('/')
            || source.contains('/')
            || destination.contains('/')
            || source.len() > 1024 * 3
            || destination.len() > 1024 * 3
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
        super::Fields::take(&mut query, "Object")?;
        let projection = projection(&mut query)?;
        if projection == Projection::Full {
            return Err(JsonError::unsupported_object(
                "move projection=full; use projection=noAcl",
            ));
        }
        for key in [
            "userProject",
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
        // The routing layer applies the value; prettyPrint=false selects compact bodies.
        super::query::boolean(&mut query, "prettyPrint")?;
        if query.remove("alt").is_some_and(|alt| alt != "json") {
            return Err(JsonError::invalid_parameter("move requires alt=json"));
        }
        let mut conditions = |prefix: &str| -> Result<ObjectConditions, JsonError> {
            let conditions = ObjectConditions {
                if_generation_match: super::objects::integer(
                    &mut query,
                    &format!("if{prefix}GenerationMatch"),
                )?,
                if_generation_not_match: super::objects::integer(
                    &mut query,
                    &format!("if{prefix}GenerationNotMatch"),
                )?,
                if_metageneration_match: super::objects::integer(
                    &mut query,
                    &format!("if{prefix}MetagenerationMatch"),
                )?,
                if_metageneration_not_match: super::objects::integer(
                    &mut query,
                    &format!("if{prefix}MetagenerationNotMatch"),
                )?,
            };
            if conditions.if_generation_match.is_some()
                && conditions.if_generation_not_match.is_some()
                || conditions.if_metageneration_match.is_some()
                    && conditions.if_metageneration_not_match.is_some()
            {
                return Err(JsonError::invalid_argument(
                    "Match and NotMatch conditions are mutually exclusive; supply only one condition per generation kind",
                ));
            }
            Ok(conditions)
        };
        let source_conditions = conditions("Source")?;
        let destination_conditions = conditions("")?;
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
    })())
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
