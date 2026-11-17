use super::{
    JsonError, ObjectRequest,
    integer::take_long,
    objects::{ConditionKeys, take_conditions},
    query::{
        BUCKET_PATH, Decoding, MAX_BUCKET_SEGMENT_BYTES, MAX_OBJECT_SEGMENT_BYTES, decode,
        finish_for_profile, parse_query, projection, standard_parameters,
    },
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
        .strip_prefix(BUCKET_PATH)?
        .strip_suffix("/restore")?;
    let (bucket, name) = path.split_once("/o/")?;
    Some(parse(method, bucket, name, uri, profile))
}

/// `bucket` and `name` are the raw path segments the route matched.
fn parse(
    method: &Method,
    bucket: &str,
    name: &str,
    uri: &Uri,
    profile: CompatProfile,
) -> Result<ObjectRequest, JsonError> {
    if method != Method::POST {
        return Err(JsonError::method_not_allowed(
            "Object restore requires POST",
            Some("POST"),
        ));
    }
    if bucket.len() > MAX_BUCKET_SEGMENT_BYTES
        || name.len() > MAX_OBJECT_SEGMENT_BYTES
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
    // Unlike a move, Cloud Storage evaluates a Match and NotMatch pair on a restore instead of
    // refusing it (recorded 2026-11-15).
    let conditions = take_conditions(&mut query, ConditionKeys::Plain)?;
    // Cloud Storage restored an object whose request carried an undeclared `gcsmimicUnknown=1`
    // (case obj-probe-undeclared-query-move-restore). fake-gcs-server has no restore route, so
    // its profile keeps rejecting one.
    finish_for_profile(&query, profile)?;
    Ok(ObjectRequest::Restore {
        target: ObjectTarget::new(bucket, name, generation),
        conditions,
    })
}
