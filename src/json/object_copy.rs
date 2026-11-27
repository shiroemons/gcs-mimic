use super::{
    ApiError, ObjectRequest, Projection,
    integer::take_source_generation,
    objects::{ConditionKeys, take_conditions, take_predefined_acl},
    query::{
        BUCKET_PATH, MAX_BUCKET_SEGMENT_BYTES, MAX_OBJECT_SEGMENT_BYTES, projection,
        standard_parameters,
    },
};
use crate::{
    acl::PredefinedObjectAcl,
    bucket::name::{AdmittedBucket, BucketKey},
    config::CompatProfile,
    object::{ObjectConditions, generation::Generation},
    percent::{Decoding, RequestUri, decode},
};
use hyper::Method;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObjectCopyParams {
    pub source_bucket: BucketKey,
    pub source_name: String,
    pub bucket: BucketKey,
    pub name: String,
    pub source_generation: Option<Generation>,
    pub source_conditions: ObjectConditions,
    pub conditions: ObjectConditions,
    pub projection: Projection,
    /// Replaces the destination bucket's default object ACL on the copy.
    pub predefined_acl: Option<PredefinedObjectAcl>,
}

pub(super) fn route(
    method: &Method,
    uri: &RequestUri,
    profile: CompatProfile,
) -> Option<Result<ObjectRequest, ApiError>> {
    let path = uri.as_uri().path().strip_prefix(BUCKET_PATH)?;
    let (source, destination) = path.split_once("/copyTo/b/")?;
    Some(parse(method, source, destination, uri, profile))
}

/// `source` and `destination` are the raw `{bucket}/o/{name}` paths the route matched.
fn parse(
    method: &Method,
    source: &str,
    destination: &str,
    uri: &RequestUri,
    profile: CompatProfile,
) -> Result<ObjectRequest, ApiError> {
    if method != Method::POST {
        return Err(ApiError::method_not_allowed(
            "Object copy requires POST",
            Some("POST"),
        ));
    }
    let (source_bucket, source_name) = names(source, profile)?;
    let (bucket, name) = names(destination, profile)?;
    let mut query = uri.parameters()?.clone();
    super::Fields::take(&mut query, super::Schema::Object)?;
    let predefined_acl = take_predefined_acl(&mut query, "destinationPredefinedAcl")?;
    standard_parameters(
        &mut query,
        &["destinationKmsKeyName", "dropContextGroups"],
        ApiError::unsupported_object,
    )?;
    // The routing layer applies the value; prettyPrint=false selects compact bodies.
    super::query::boolean(&mut query, "prettyPrint")?;
    if let Some(alt) = query.remove("alt")
        && alt != "json"
    {
        return Err(ApiError::invalid_parameter("copy requires alt=json"));
    }
    let projection = projection(&mut query)?;
    let source_generation =
        take_source_generation(&mut query, "sourceGeneration must be positive")?;
    let source_conditions = take_conditions(&mut query, ConditionKeys::Source)?;
    let conditions = take_conditions(&mut query, ConditionKeys::Plain)?;
    // Cloud Storage copied an object whose request carried an undeclared `gcsmimicUnknown=1`
    // (case obj-undeclared-query-writes), and fake-gcs-server reads only the parameters it
    // knows, so what remains in `query` is ignored.
    Ok(ObjectRequest::Copy(ObjectCopyParams {
        source_bucket,
        source_name,
        bucket,
        name,
        source_generation,
        source_conditions,
        conditions,
        projection,
        predefined_acl,
    }))
}

fn names(path: &str, profile: CompatProfile) -> Result<(BucketKey, String), ApiError> {
    let (bucket, name) = path
        .split_once("/o/")
        .ok_or_else(|| ApiError::invalid_parameter("Invalid copy path"))?;
    if bucket.len() > MAX_BUCKET_SEGMENT_BYTES
        || name.len() > MAX_OBJECT_SEGMENT_BYTES
        || bucket.contains('/')
        || name.contains('/')
    {
        return Err(ApiError::invalid_parameter("Invalid copy path"));
    }
    let bucket = decode(bucket, Decoding::Path)?;
    let bucket = AdmittedBucket::for_profile(bucket, profile)?.into_key();
    let name = decode(name, Decoding::Path)?;
    super::validate_object_name(&name)?;
    Ok((bucket, name))
}
