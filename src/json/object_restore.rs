use super::{
    JsonError, ObjectRequest,
    objects::integer,
    query::{Decoding, decode, finish, parse_query, projection},
};
use crate::{
    config::CompatProfile,
    object::{ObjectConditions, ObjectTarget},
};
use hyper::{Method, Uri};

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
            return Err(JsonError::method_not_allowed(
                "Object restore requires POST",
                Some("POST"),
            ));
        }
        if bucket.len() > 222 * 3
            || name.len() > 1024 * 3
            || bucket.contains('/')
            || name.contains('/')
        {
            return Err(JsonError::invalid_parameter("Invalid object restore path"));
        }
        let bucket = decode(bucket, Decoding::Path)?;
        let name = decode(name, Decoding::Path)?;
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
        if super::query::boolean(&mut query, "copySourceAcl")? {
            return Err(JsonError::unsupported_object("copySourceAcl"));
        }
        // The routing layer applies the value; prettyPrint=false selects compact bodies.
        super::query::boolean(&mut query, "prettyPrint")?;
        if query.remove("alt").is_some_and(|alt| alt != "json") {
            return Err(JsonError::invalid_parameter("restore requires alt=json"));
        }
        // Both documented values are accepted (the Java SDK always sends projection=full), but
        // Cloud Storage was observed to omit acl and owner from the restore response even for
        // full, so the value is validated and then has no effect on the rendered resource.
        projection(&mut query)?;
        let generation = integer(&mut query, "generation")?.ok_or_else(|| {
            JsonError::required(
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
        finish(&query)?;
        Ok(ObjectRequest::Restore {
            target: ObjectTarget::new(bucket, name, generation),
            conditions,
        })
    })())
}
