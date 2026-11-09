use super::{
    Decoding, Fields, JsonError, decode, parse_query, query, validate_bucket_name_for_profile,
    validate_object_name,
};
use crate::{
    config::CompatProfile,
    object::{ObjectConditions, ObjectTarget},
    storage::rewrite::{RewriteNames, RewriteProgress},
};
use hyper::{Method, Uri};
use serde_json::{Map, Value, json};

/// Which object a rewrite precondition applies to: `ifSource*` parameters guard the source and
/// the unprefixed ones guard the destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RewriteSide {
    Source,
    Destination,
}

pub(crate) struct RewriteRequest {
    pub(crate) names: RewriteNames,
    pub(crate) token: Option<String>,
    pub(crate) parameters: Map<String, Value>,
}

impl RewriteRequest {
    pub(crate) fn number(&self, key: &str) -> Option<i64> {
        self.parameters
            .get(key)
            .and_then(Value::as_str)
            .and_then(|value| value.parse().ok())
    }
    pub(crate) fn conditions(&self, side: RewriteSide) -> ObjectConditions {
        let [
            generation_match,
            generation_not_match,
            metageneration_match,
            metageneration_not_match,
        ] = if side == RewriteSide::Source {
            [
                "ifSourceGenerationMatch",
                "ifSourceGenerationNotMatch",
                "ifSourceMetagenerationMatch",
                "ifSourceMetagenerationNotMatch",
            ]
        } else {
            [
                "ifGenerationMatch",
                "ifGenerationNotMatch",
                "ifMetagenerationMatch",
                "ifMetagenerationNotMatch",
            ]
        };
        ObjectConditions {
            if_generation_match: self.number(generation_match),
            if_generation_not_match: self.number(generation_not_match),
            if_metageneration_match: self.number(metageneration_match),
            if_metageneration_not_match: self.number(metageneration_not_match),
        }
    }
}

pub(crate) fn route(
    method: &Method,
    uri: &Uri,
    profile: CompatProfile,
) -> Option<Result<RewriteRequest, JsonError>> {
    let path = uri.path().strip_prefix("/storage/v1/b/")?;
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
    Some((|| {
        if *method != Method::POST {
            return Err(JsonError::method_not_allowed(
                "Rewrite requires POST",
                Some("POST"),
            ));
        }
        let names = RewriteNames {
            source: ObjectTarget::new(
                decode(source_bucket, Decoding::Path)?,
                decode(source_name, Decoding::Path)?,
                (),
            ),
            destination: ObjectTarget::new(
                decode(destination_bucket, Decoding::Path)?,
                decode(destination_name, Decoding::Path)?,
                (),
            ),
        };
        for bucket in [&names.source.bucket, &names.destination.bucket] {
            validate_bucket_name_for_profile(bucket, profile)?;
        }
        for object in [&names.source.name, &names.destination.name] {
            validate_object_name(object)?;
        }
        let mut parameters = parse_query(uri.query().unwrap_or_default())?;
        let token = parameters.remove("rewriteToken");
        // A rewrite token binds these parameters; an ignored API key must not be one of them.
        query::ignore_api_key(&mut parameters);
        let mut remaining = parameters.clone();
        Fields::take(&mut remaining, "RewriteResponse")?;
        query::common(&mut remaining)?;
        query::projection(&mut remaining)?;
        if let Some(acl) = remaining.get("destinationPredefinedAcl")
            && !matches!(
                acl.as_str(),
                "authenticatedRead"
                    | "bucketOwnerFullControl"
                    | "bucketOwnerRead"
                    | "private"
                    | "projectPrivate"
                    | "publicRead"
            )
        {
            return Err(JsonError::invalid_parameter(
                "Invalid destinationPredefinedAcl",
            ));
        }
        remaining.remove("destinationPredefinedAcl");
        for key in ["destinationKmsKeyName", "dropContextGroups"] {
            if remaining.contains_key(key) {
                return Err(JsonError::unsupported_object(key));
            }
        }
        for key in [
            "sourceGeneration",
            "ifSourceGenerationMatch",
            "ifSourceGenerationNotMatch",
            "ifSourceMetagenerationMatch",
            "ifSourceMetagenerationNotMatch",
            "ifGenerationMatch",
            "ifGenerationNotMatch",
            "ifMetagenerationMatch",
            "ifMetagenerationNotMatch",
            "maxBytesRewrittenPerCall",
        ] {
            if let Some(value) = remaining.remove(key) {
                if value.is_empty() || value.starts_with('+') {
                    return Err(JsonError::invalid_long(&value).at_parameter(key));
                }
                let number = value
                    .parse::<i64>()
                    .map_err(|_| JsonError::invalid_long(&value).at_parameter(key))?;
                if key == "sourceGeneration" && number <= 0 {
                    return Err(JsonError::invalid_parameter(format!("Invalid {key}")));
                }
                if key == "maxBytesRewrittenPerCall" {
                    if number <= 0 {
                        return Err(JsonError::invalid(
                            "maxBytesRewrittenPerCall must be a positive value",
                        ));
                    }
                    if number % 1_048_576 != 0 {
                        return Err(JsonError::invalid(
                            "maxBytesRewrittenPerCall must be an integral multiple of 1 MiB (1048576)",
                        ));
                    }
                }
                parameters.insert(key.into(), number.to_string());
            }
        }
        query::finish(&remaining)?;
        Ok(RewriteRequest {
            names,
            token,
            parameters: parameters
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect(),
        })
    })())
}

pub(crate) fn response(progress: RewriteProgress) -> Result<Value, JsonError> {
    let mut value = Map::from_iter([
        ("kind".into(), json!("storage#rewriteResponse")),
        (
            "totalBytesRewritten".into(),
            json!(progress.offset.to_string()),
        ),
        ("objectSize".into(), json!(progress.total.to_string())),
        ("done".into(), json!(progress.resource.is_some())),
    ]);
    if let Some(token) = progress.token {
        value.insert("rewriteToken".into(), json!(token));
    }
    if let Some(resource) = progress.resource {
        value.insert("resource".into(), resource);
    }
    let mut value = Value::Object(value);
    if let Some(fields) = progress.fields {
        Fields::parse(&fields, "RewriteResponse")?.apply(&mut value);
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
                .status,
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
}
