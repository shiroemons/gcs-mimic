use super::{
    JsonError, ObjectRequest, Projection,
    query::{Decoding, decode, finish, parse_query, projection},
};
use crate::{config::CompatProfile, object::ObjectConditions};
use hyper::{Method, StatusCode, Uri};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectMoveParams {
    pub bucket: String,
    pub source: String,
    pub destination: String,
    pub source_conditions: ObjectConditions,
    pub conditions: ObjectConditions,
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
            let mut error = JsonError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "methodNotAllowed",
                "Object move requires POST",
            );
            error.allow = Some("POST");
            return Err(error);
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
            return Err(JsonError::invalid_argument(
                "Source and destination must differ; for example, move source.txt to destination.txt",
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
        if super::query::boolean(&mut query, "prettyPrint")? {
            return Err(JsonError::unsupported_object("prettyPrint"));
        }
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
        let conditions = conditions("")?;
        finish(&query)?;
        Ok(ObjectRequest::Move(ObjectMoveParams {
            bucket,
            source,
            destination,
            source_conditions,
            conditions,
            projection,
        }))
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_route_decodes_names_once_and_keeps_conditions_separate() {
        let uri = "/storage/v1/b/bucket/o/source%2F%252F/moveTo/o/target%2F%252F?ifSourceGenerationMatch=7&ifGenerationMatch=0&prettyPrint=false&alt=json".parse().unwrap();
        let ObjectRequest::Move(params) = route(&Method::POST, &uri, CompatProfile::Native)
            .unwrap()
            .unwrap()
        else {
            panic!("expected move route")
        };
        assert_eq!(params.source, "source/%2F");
        assert_eq!(params.destination, "target/%2F");
        assert_eq!(params.source_conditions.if_generation_match, Some(7));
        assert_eq!(params.conditions.if_generation_match, Some(0));
        let error = route(&Method::GET, &uri, CompatProfile::Native)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(error.allow, Some("POST"));
    }
}
