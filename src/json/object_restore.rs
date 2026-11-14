use super::{
    JsonError, ObjectRequest,
    integer::take_long,
    objects::{ConditionKeys, match_and_not_match, take_conditions},
    query::{Decoding, decode, finish, parse_query, projection, standard_parameters},
};
use crate::{config::CompatProfile, object::ObjectTarget};
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
        super::Fields::take(&mut query, super::Schema::Object)?;
        standard_parameters(&mut query, &["restoreToken"], JsonError::unsupported_object)?;
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
        let generation = take_long(&mut query, "generation")?.ok_or_else(|| {
            JsonError::required(
                "Missing required generation; select a generation from softDeleted=true listing",
            )
        })?;
        let conditions = take_conditions(&mut query, ConditionKeys::Plain)?;
        if match_and_not_match(&conditions) {
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
