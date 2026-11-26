use super::{
    ApiError, Fields, Schema,
    integer::take_long,
    objects::{ConditionKeys, take_conditions, take_predefined_acl},
    query, validate_object_name,
};
use crate::percent::{Decoding, decode, parse_query};
use crate::{
    acl::PredefinedObjectAcl,
    config::CompatProfile,
    object::{ObjectConditions, ObjectTarget},
    storage::rewrite::{RewriteNames, RewriteProgress},
};
use hyper::{Method, Uri};
use serde_json::{Map, Value};
use std::num::NonZeroU64;

pub(crate) struct RewriteRequest {
    pub(crate) names: RewriteNames,
    pub(crate) token: Option<String>,
    /// `sourceGeneration`: positive when present.
    pub(crate) source_generation: Option<i64>,
    /// The `ifSource*` preconditions, which guard the source.
    pub(crate) source_conditions: ObjectConditions,
    /// The unprefixed preconditions, which guard the destination.
    pub(crate) conditions: ObjectConditions,
    /// `maxBytesRewrittenPerCall`: a positive multiple of 1 MiB.
    pub(crate) max_bytes_per_call: Option<NonZeroU64>,
    /// `destinationPredefinedAcl`, which replaces the destination bucket's default object ACL.
    pub(crate) predefined_acl: Option<PredefinedObjectAcl>,
    /// Whether the finished rewrite's resource carries `acl` and `owner`, as the Go SDK's Copier
    /// asks for.
    pub(crate) projection: query::Projection,
    /// The parameters a rewrite token binds, by their JSON API names, as the session stores
    /// them: numbers in canonical decimal, every other bound parameter as sent, keys in
    /// ascending order. The text and the order are part of the stored session bytes.
    pub(crate) parameters: Map<String, Value>,
}

pub(crate) fn route(
    method: &Method,
    uri: &Uri,
    profile: CompatProfile,
) -> Option<Result<RewriteRequest, ApiError>> {
    let path = uri.path().strip_prefix(query::BUCKET_PATH)?;
    let segments: Vec<_> = path.split('/').take(9).collect();
    let [
        source_bucket,
        "o",
        source_name,
        "rewriteTo",
        "b",
        destination_bucket,
        "o",
        destination_name,
    ] = segments.as_slice()
    else {
        return None;
    };
    let path = RewritePath {
        source_bucket,
        source_name,
        destination_bucket,
        destination_name,
    };
    Some(parse(method, path, uri, profile))
}

/// The raw path segments of a rewrite, before decoding.
#[derive(Clone, Copy)]
struct RewritePath<'a> {
    source_bucket: &'a str,
    source_name: &'a str,
    destination_bucket: &'a str,
    destination_name: &'a str,
}

fn parse(
    method: &Method,
    path: RewritePath<'_>,
    uri: &Uri,
    profile: CompatProfile,
) -> Result<RewriteRequest, ApiError> {
    if *method != Method::POST {
        return Err(ApiError::method_not_allowed(
            "Rewrite requires POST",
            Some("POST"),
        ));
    }
    let names = RewriteNames {
        source: ObjectTarget::new(
            decode(path.source_bucket, Decoding::Path)?,
            decode(path.source_name, Decoding::Path)?,
            (),
        ),
        destination: ObjectTarget::new(
            decode(path.destination_bucket, Decoding::Path)?,
            decode(path.destination_name, Decoding::Path)?,
            (),
        ),
    };
    for bucket in [&names.source.bucket, &names.destination.bucket] {
        crate::bucket::name::validate_for_profile(bucket, profile)?;
    }
    for object in [&names.source.name, &names.destination.name] {
        validate_object_name(object)?;
    }
    let mut parameters = parse_query(uri.query().unwrap_or_default())?;
    let token = parameters.remove("rewriteToken");
    // A rewrite token binds these parameters; an ignored API key or billing project must not
    // be one of them.
    query::ignore_accepted_parameters(&mut parameters);
    let mut remaining = parameters.clone();
    Fields::take(&mut remaining, Schema::RewriteResponse)?;
    query::common(&mut remaining)?;
    let projection = query::projection(&mut remaining)?;
    let predefined_acl = take_predefined_acl(&mut remaining, "destinationPredefinedAcl")?;
    for key in ["destinationKmsKeyName", "dropContextGroups"] {
        if remaining.contains_key(key) {
            return Err(ApiError::unsupported_object(key));
        }
    }
    let source_generation = take_long(&mut remaining, "sourceGeneration")?;
    if source_generation.is_some_and(|generation| generation <= 0) {
        return Err(ApiError::invalid_parameter("Invalid sourceGeneration"));
    }
    let source_conditions = take_conditions(&mut remaining, ConditionKeys::Source)?;
    let conditions = take_conditions(&mut remaining, ConditionKeys::Plain)?;
    let max_bytes = take_long(&mut remaining, "maxBytesRewrittenPerCall")?;
    let max_bytes_per_call = max_bytes.map(byte_budget).transpose()?;
    // The binding spells each number in canonical decimal, so `007` binds as `7`.
    for (key, number) in [
        ("sourceGeneration", source_generation),
        (
            "ifSourceGenerationMatch",
            source_conditions.if_generation_match,
        ),
        (
            "ifSourceGenerationNotMatch",
            source_conditions.if_generation_not_match,
        ),
        (
            "ifSourceMetagenerationMatch",
            source_conditions.if_metageneration_match,
        ),
        (
            "ifSourceMetagenerationNotMatch",
            source_conditions.if_metageneration_not_match,
        ),
        ("ifGenerationMatch", conditions.if_generation_match),
        ("ifGenerationNotMatch", conditions.if_generation_not_match),
        ("ifMetagenerationMatch", conditions.if_metageneration_match),
        (
            "ifMetagenerationNotMatch",
            conditions.if_metageneration_not_match,
        ),
        ("maxBytesRewrittenPerCall", max_bytes),
    ] {
        if let Some(number) = number {
            parameters.insert(key.into(), number.to_string());
        }
    }
    // Cloud Storage rewrote an object whose request carried an undeclared `gcsmimicUnknown=1`
    // (case obj-undeclared-query-writes), and fake-gcs-server reads only the parameters it
    // knows. What remains is ignored and kept out of the token binding, so a continuation
    // may add or drop such a parameter.
    parameters.retain(|key, _| !remaining.contains_key(key));
    Ok(RewriteRequest {
        names,
        token,
        source_generation,
        source_conditions,
        conditions,
        max_bytes_per_call,
        predefined_acl,
        projection,
        parameters: parameters
            .into_iter()
            .map(|(key, value)| (key, Value::String(value)))
            .collect(),
    })
}

/// `maxBytesRewrittenPerCall`, which must be a positive multiple of 1 MiB.
fn byte_budget(number: i64) -> Result<NonZeroU64, ApiError> {
    let Some(budget) = u64::try_from(number).ok().and_then(NonZeroU64::new) else {
        return Err(ApiError::invalid(
            "maxBytesRewrittenPerCall must be a positive value",
        ));
    };
    if budget.get() % 1_048_576 != 0 {
        return Err(ApiError::invalid(
            "maxBytesRewrittenPerCall must be an integral multiple of 1 MiB (1048576)",
        ));
    }
    Ok(budget)
}

pub(crate) fn response(progress: RewriteProgress) -> Result<Value, ApiError> {
    let mut value = Map::from_iter([
        ("kind".into(), "storage#rewriteResponse".into()),
        (
            "totalBytesRewritten".into(),
            progress.offset.to_string().into(),
        ),
        ("objectSize".into(), progress.total.to_string().into()),
        ("done".into(), progress.resource.is_some().into()),
    ]);
    if let Some(token) = progress.token {
        value.insert("rewriteToken".into(), token.into());
    }
    if let Some(resource) = progress.resource {
        value.insert("resource".into(), resource);
    }
    let mut value = Value::Object(value);
    if let Some(fields) = progress.fields {
        Fields::parse(&fields, Schema::RewriteResponse)?.apply(&mut value);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::StatusCode;
    #[test]
    fn rewrite_route_decodes_once_and_bounds_byte_budgets() {
        let base = "/storage/v1/b/source-bucket/o/a%2Fb%25/rewriteTo/b/destination-bucket/o/c%2Fd";
        let parsed = route(&Method::POST, &base.parse().unwrap(), CompatProfile::Native)
            .unwrap()
            .unwrap();
        assert_eq!(
            parsed.names,
            RewriteNames {
                source: ObjectTarget::new("source-bucket", "a/b%", ()),
                destination: ObjectTarget::new("destination-bucket", "c/d", ()),
            }
        );
        for maximum in [
            "0",
            "1",
            "1048575",
            "1048577",
            "-1048576",
            "9223372036854775808",
        ] {
            assert_eq!(
                route(
                    &Method::POST,
                    &format!("{base}?maxBytesRewrittenPerCall={maximum}")
                        .parse()
                        .unwrap(),
                    CompatProfile::Native
                )
                .unwrap()
                .err()
                .unwrap()
                .status(),
                StatusCode::BAD_REQUEST
            );
        }
        for maximum in ["1048576", "2097152"] {
            assert!(
                route(
                    &Method::POST,
                    &format!("{base}?maxBytesRewrittenPerCall={maximum}")
                        .parse()
                        .unwrap(),
                    CompatProfile::Native
                )
                .unwrap()
                .is_ok()
            );
        }
    }

    /// The numbers a rewrite reads keep their side: `ifSource*` guards the source.
    #[test]
    fn rewrite_numbers_are_parsed_once_into_typed_fields() {
        let uri = "/storage/v1/b/source-bucket/o/a/rewriteTo/b/destination-bucket/o/c?sourceGeneration=5&ifSourceGenerationMatch=6&ifSourceGenerationNotMatch=7&ifSourceMetagenerationMatch=8&ifSourceMetagenerationNotMatch=-0&ifGenerationMatch=007&ifGenerationNotMatch=-3&ifMetagenerationMatch=10&ifMetagenerationNotMatch=11&maxBytesRewrittenPerCall=2097152";
        let request = route(&Method::POST, &uri.parse().unwrap(), CompatProfile::Native)
            .unwrap()
            .unwrap();
        assert_eq!(request.source_generation, Some(5));
        assert_eq!(
            request.source_conditions,
            ObjectConditions {
                if_generation_match: Some(6),
                if_generation_not_match: Some(7),
                if_metageneration_match: Some(8),
                if_metageneration_not_match: Some(0),
            }
        );
        assert_eq!(
            request.conditions,
            ObjectConditions {
                if_generation_match: Some(7),
                if_generation_not_match: Some(-3),
                if_metageneration_match: Some(10),
                if_metageneration_not_match: Some(11),
            }
        );
        assert_eq!(request.max_bytes_per_call, NonZeroU64::new(2_097_152));
        let bare = route(
            &Method::POST,
            &"/storage/v1/b/source-bucket/o/a/rewriteTo/b/destination-bucket/o/c"
                .parse()
                .unwrap(),
            CompatProfile::Native,
        )
        .unwrap()
        .unwrap();
        assert_eq!(bare.source_generation, None);
        assert_eq!(bare.source_conditions, ObjectConditions::default());
        assert_eq!(bare.conditions, ObjectConditions::default());
        assert_eq!(bare.max_bytes_per_call, None);
        assert!(bare.parameters.is_empty());
    }

    #[test]
    fn undeclared_parameters_stay_out_of_the_token_binding() {
        let base = "/storage/v1/b/source-bucket/o/a/rewriteTo/b/destination-bucket/o/c?maxBytesRewrittenPerCall=1048576";
        let parse = |uri: &str| {
            route(&Method::POST, &uri.parse().unwrap(), CompatProfile::Native)
                .unwrap()
                .unwrap()
                .parameters
        };
        assert_eq!(parse(&format!("{base}&gcsmimicUnknown=1")), parse(base));
    }

    /// A rewrite session stores these parameters as JSON, key order included, and a continued
    /// call must bind the same values.
    #[test]
    fn rewrite_binding_parameters_keep_their_json_bytes() {
        let uri = "/storage/v1/b/source-bucket/o/a/rewriteTo/b/destination-bucket/o/c?sourceGeneration=5&ifGenerationMatch=007&ifSourceMetagenerationNotMatch=-0&fields=done&prettyPrint=False&projection=noAcl&destinationPredefinedAcl=private&maxBytesRewrittenPerCall=1048576&gcsmimicUnknown=1&key=k";
        let parameters = route(&Method::POST, &uri.parse().unwrap(), CompatProfile::Native)
            .unwrap()
            .unwrap()
            .parameters;
        // Numbers are respelled, `False` keeps its spelling, the undeclared parameter and the
        // API key are left out, and keys come in sorted order.
        assert_eq!(
            serde_json::to_string(&parameters).unwrap(),
            r#"{"destinationPredefinedAcl":"private","fields":"done","ifGenerationMatch":"7","ifSourceMetagenerationNotMatch":"0","maxBytesRewrittenPerCall":"1048576","prettyPrint":"False","projection":"noAcl","sourceGeneration":"5"}"#
        );
    }
}
