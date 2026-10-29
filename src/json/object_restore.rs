use super::{
    JsonError, ObjectRequest, Projection,
    objects::integer,
    query::{decode, finish, parse_query, projection},
};
use crate::{config::CompatProfile, object::ObjectConditions};
use hyper::{Method, StatusCode, Uri};

pub(super) fn route(
    method: &Method,
    uri: &Uri,
    profile: CompatProfile,
) -> Option<Result<ObjectRequest, JsonError>> {
    let path = uri
        .path()
        .strip_prefix("/storage/v1/b/")?
        .strip_suffix("/restore")?;
    let (bucket, name) = path.split_once("/o/")?;
    Some((|| {
        if method != Method::POST {
            let mut error = JsonError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "methodNotAllowed",
                "Object restore requires POST",
            );
            error.allow = Some("POST");
            return Err(error);
        }
        if bucket.len() > 222 * 3
            || name.len() > 1024 * 3
            || bucket.contains('/')
            || name.contains('/')
        {
            return Err(JsonError::invalid_parameter("Invalid object restore path"));
        }
        let bucket = decode(bucket, false)?;
        let name = decode(name, false)?;
        super::validate_bucket_name_for_profile(&bucket, profile)?;
        super::validate_object_name(&name)?;
        let mut query = parse_query(uri.query().unwrap_or_default())?;
        super::Fields::take(&mut query, "Object")?;
        for key in [
            "restoreToken",
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
        for key in ["copySourceAcl", "prettyPrint"] {
            if super::query::boolean(&mut query, key)? {
                return Err(JsonError::unsupported_object(key));
            }
        }
        if query.remove("alt").is_some_and(|alt| alt != "json") {
            return Err(JsonError::invalid_parameter("restore requires alt=json"));
        }
        let projection = projection(&mut query)?;
        if projection == Projection::Full {
            return Err(JsonError::unsupported_object(
                "restore projection=full; use projection=noAcl",
            ));
        }
        let generation = integer(&mut query, "generation")?.ok_or_else(|| {
            JsonError::new(
                StatusCode::BAD_REQUEST,
                "required",
                "Missing required generation; select a generation from softDeleted=true listing",
            )
        })?;
        let conditions = ObjectConditions {
            if_generation_match: integer(&mut query, "ifGenerationMatch")?,
            if_generation_not_match: integer(&mut query, "ifGenerationNotMatch")?,
            if_metageneration_match: integer(&mut query, "ifMetagenerationMatch")?,
            if_metageneration_not_match: integer(&mut query, "ifMetagenerationNotMatch")?,
        };
        if conditions.if_generation_match.is_some() && conditions.if_generation_not_match.is_some()
            || conditions.if_metageneration_match.is_some()
                && conditions.if_metageneration_not_match.is_some()
        {
            return Err(JsonError::invalid_argument(
                "Match and NotMatch conditions are mutually exclusive; supply one condition per generation kind",
            ));
        }
        finish(query)?;
        Ok(ObjectRequest::Restore {
            bucket,
            name,
            generation,
            conditions,
            projection,
        })
    })())
}
