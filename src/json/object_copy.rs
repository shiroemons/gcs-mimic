use super::{
    JsonError, ObjectRequest, Projection,
    query::{Decoding, decode, finish, parse_query, projection},
};
use crate::{config::CompatProfile, object::ObjectConditions};
use hyper::{Method, StatusCode, Uri};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectCopyParams {
    pub source_bucket: String,
    pub source_name: String,
    pub bucket: String,
    pub name: String,
    pub source_generation: Option<i64>,
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
    let (source, destination) = path.split_once("/copyTo/b/")?;
    Some((|| {
        if method != Method::POST {
            let mut error = JsonError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "methodNotAllowed",
                "Object copy requires POST",
            );
            error.allow = Some("POST");
            return Err(error);
        }
        let (source_bucket, source_name) = names(source, profile)?;
        let (bucket, name) = names(destination, profile)?;
        let mut query = parse_query(uri.query().unwrap_or_default())?;
        super::Fields::take(&mut query, "Object")?;
        query.remove("destinationPredefinedAcl");
        for key in [
            "destinationKmsKeyName",
            "userProject",
            "dropContextGroups",
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
        if let Some(alt) = query.remove("alt")
            && alt != "json"
        {
            return Err(JsonError::invalid_parameter("copy requires alt=json"));
        }
        let projection = projection(&mut query)?;
        let source_generation = super::objects::integer(&mut query, "sourceGeneration")?;
        if source_generation.is_some_and(|generation| generation <= 0) {
            return Err(JsonError::invalid_parameter(
                "sourceGeneration must be positive",
            ));
        }
        let mut conditions = |prefix: &str| -> Result<ObjectConditions, JsonError> {
            Ok(ObjectConditions {
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
            })
        };
        let source_conditions = conditions("Source")?;
        let conditions = conditions("")?;
        finish(&query)?;
        Ok(ObjectRequest::Copy(ObjectCopyParams {
            source_bucket,
            source_name,
            bucket,
            name,
            source_generation,
            source_conditions,
            conditions,
            projection,
        }))
    })())
}

fn names(path: &str, profile: CompatProfile) -> Result<(String, String), JsonError> {
    let (bucket, name) = path
        .split_once("/o/")
        .ok_or_else(|| JsonError::invalid_parameter("Invalid copy path"))?;
    if bucket.len() > 222 * 3 || name.len() > 1024 * 3 || bucket.contains('/') || name.contains('/')
    {
        return Err(JsonError::invalid_parameter("Invalid copy path"));
    }
    let bucket = decode(bucket, Decoding::Path)?;
    super::validate_bucket_name_for_profile(&bucket, profile)?;
    let name = decode(name, Decoding::Path)?;
    super::validate_object_name(&name)?;
    Ok((bucket, name))
}
