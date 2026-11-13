use super::{BucketRequest, Fields, JsonError, objects::base64, query};
use crate::auth::{
    Binding, Policy,
    policy_input::{self, PolicyInput, PolicyInputError},
    roles::PREDEFINED_ROLES,
};
use hyper::{Method, StatusCode, Uri};
use serde_json::{Value, json};

pub(super) fn route_test_permissions(
    method: &Method,
    uri: &Uri,
    profile: crate::config::CompatProfile,
) -> Option<Result<BucketRequest, JsonError>> {
    let segment = uri
        .path()
        .strip_prefix("/storage/v1/b/")?
        .strip_suffix("/iam/testPermissions")?;
    if segment.is_empty() || segment.contains('/') {
        return None;
    }
    Some((|| {
        if segment.len() > 222 * 3 {
            return Err(JsonError::invalid_parameter("Invalid bucket name"));
        }
        let bucket = query::decode(segment, query::Decoding::Path)?;
        super::buckets::validate_bucket_name_for_profile(&bucket, profile)?;
        if *method != Method::GET {
            return Err(JsonError::method_not_allowed(
                "Use GET for bucket testIamPermissions",
                Some("GET"),
            ));
        }
        let (permissions, _) = test_permissions_query(uri.query())?;
        Ok(BucketRequest::TestIam {
            bucket,
            permissions,
        })
    })())
}

pub(crate) fn test_permissions_query(
    raw: Option<&str>,
) -> Result<(Vec<crate::auth::Permission>, Fields), JsonError> {
    let raw = raw.unwrap_or_default();
    if raw.len() > 16 * 1024 {
        return Err(JsonError::invalid_parameter("Query exceeds 16 KiB"));
    }
    let mut permissions = Vec::new();
    let mut rest = Vec::new();
    for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if query::decode(key, query::Decoding::Form)? == "permissions" {
            let permission = query::decode(value, query::Decoding::Form)?;
            let permission = check_testable_permission(&permission)?;
            if !permissions.contains(&permission) {
                permissions.push(permission);
            }
        } else {
            rest.push(pair);
        }
    }
    if permissions.is_empty() {
        return Err(
            JsonError::required("Required parameter: permissions").at_parameter("permissions")
        );
    }
    let mut query = query::parse_query(&rest.join("&"))?;
    let fields = Fields::take(&mut query, "TestIamPermissionsResponse")?;
    query::common(&mut query)?;
    query::finish(&query)?;
    Ok((permissions, fields))
}

fn invalid(message: impl Into<String>) -> JsonError {
    JsonError::new(StatusCode::BAD_REQUEST, "invalid", message)
}

pub(crate) fn check_testable_permission(
    permission: &str,
) -> Result<crate::auth::Permission, JsonError> {
    let Ok(parsed) = crate::auth::Permission::try_from(permission) else {
        if PREDEFINED_ROLES
            .iter()
            .any(|(_, _, names)| names.contains(&permission))
        {
            return Err(JsonError::unsupported(&format!(
                "permission {permission}; use a supported operation permission such as storage.objects.get"
            )));
        }
        return Err(invalid(format!(
            "{permission} is not a valid Google Cloud Storage permission."
        )));
    };
    if matches!(
        parsed,
        crate::auth::Permission::BucketsCreate | crate::auth::Permission::BucketsList
    ) {
        return Err(invalid("Invalid argument."));
    }
    Ok(parsed)
}

impl From<PolicyInputError> for JsonError {
    fn from(error: PolicyInputError) -> Self {
        match error {
            PolicyInputError::InvalidInput(message) => invalid(message),
            PolicyInputError::UnsupportedInput(message) => JsonError::unsupported(&message),
        }
    }
}

pub(crate) fn parse_policy(bytes: &[u8]) -> Result<Policy, JsonError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| JsonError::parse_error(bytes, &error))?;
    let body = value
        .as_object()
        .ok_or_else(|| invalid("Policy must be an object"))?;
    let version = match body.get("version") {
        None => 0,
        Some(value) => match value.as_i64() {
            Some(0) => 0,
            Some(1) => 1,
            Some(3) => 3,
            _ => return Err(invalid("Policy version must be 0, 1 or 3")),
        },
    };
    if body.contains_key("auditConfigs") {
        return Err(JsonError::unsupported("auditConfigs"));
    }
    let etag = match body.get("etag") {
        None => None,
        Some(value) => parse_etag(
            value
                .as_str()
                .ok_or_else(|| invalid("etag must be a string"))?,
        )?,
    };
    let mut bindings = Vec::new();
    let mut principals = 0;
    {
        let values = body
            .get("bindings")
            .ok_or_else(|| invalid("bindings is required"))?;
        for value in values
            .as_array()
            .ok_or_else(|| invalid("bindings must be an array"))?
        {
            let binding = value
                .as_object()
                .ok_or_else(|| invalid("binding must be an object"))?;
            if binding.contains_key("condition") {
                return Err(JsonError::unsupported("binding condition"));
            }
            let role = binding
                .get("role")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("binding role must be a string"))?;
            policy_input::validate_role(role)?;
            // A binding without members grants nothing, and Cloud Storage dropped it from the
            // stored policy without complaint (recorded 2026-10-31).
            let values = binding
                .get("members")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("binding members must be an array"))?;
            if values.is_empty() {
                continue;
            }
            principals += values.len();
            policy_input::validate_principal_count(principals)?;
            let members = values
                .iter()
                .map(|value| {
                    let member = value
                        .as_str()
                        .ok_or_else(|| invalid("member must be a string"))?;
                    policy_input::validate_member(member)?;
                    Ok(member.to_owned())
                })
                .collect::<Result<_, JsonError>>()?;
            bindings.push(Binding {
                role: role.into(),
                members,
            });
        }
    }
    // Validate while parsing too, so mixed shape/semantic errors retain their JSON precedence.
    PolicyInput {
        version,
        bindings,
        etag,
    }
    .into_policy()
    .map_err(Into::into)
}

fn parse_etag(raw: &str) -> Result<Option<String>, JsonError> {
    // Google.Apis replaces a fetched policy's body etag with the response's HTTP ETag, which
    // getIamPolicy sends quoted, and sends that value back; one pair of quotes names the same tag.
    let tag = raw
        .strip_prefix('"')
        .and_then(|tag| tag.strip_suffix('"'))
        .filter(|tag| !tag.is_empty())
        .unwrap_or(raw);
    if tag.is_empty() {
        return Ok(None);
    }
    // Cloud Storage decoded "CAF=", whose unused bits are not zero, and then refused the write
    // with 412 because the tag did not match (recorded 2026-11).
    let decoded = crate::base64::decode(
        tag,
        crate::base64::Alphabet::Either,
        crate::base64::Padding::OptionalUncheckedBits,
    )
    .ok_or_else(|| {
        let mut error = invalid(format!("Invalid value for ByteString: {raw}"));
        error.location = Some(&super::error::POLICY_ETAG_FIELD);
        error
    })?;
    Ok(Some(base64(&decoded)))
}

pub(crate) fn policy_resource(policy: &Policy, bucket: &str) -> Value {
    json!({"kind": "storage#policy", "resourceId": format!("projects/_/buckets/{bucket}"),
        "version": 1, "etag": policy.etag, "bindings": policy.bindings_json()})
}

pub(crate) fn policy_etag(sequence: i64) -> String {
    base64(&sequence.to_be_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_validation_preserves_json_error_precedence_and_opaque_etags() {
        for (body, status, message) in [
            (
                json!({"version":2,"auditConfigs":[]}),
                400,
                "Policy version must be 0, 1 or 3",
            ),
            (
                json!({"bindings":[{"role":"unknown","members":false}]}),
                400,
                "Role unknown is not supported for this resource.",
            ),
            (
                json!({"bindings":[{"role":"roles/storage.objectViewer","members":vec![Value::Null;1501]}]}),
                400,
                "Policy exceeds 1500 principal occurrences",
            ),
            (
                json!({"bindings":[{"role":"roles/storage.objectViewer","members":[false]},{"role":"unknown","members":[]}]}),
                400,
                "member must be a string",
            ),
        ] {
            let error = parse_policy(body.to_string().as_bytes()).unwrap_err();
            assert_eq!(error.status.as_u16(), status);
            assert_eq!(error.message, message);
        }
        let policy = parse_policy(br#"{"version":3,"bindings":[],"etag":"AA=="}"#).unwrap();
        assert_eq!(policy.etag.as_deref(), Some("AA=="));
    }

    #[test]
    fn etag_bytes_accept_protojson_encodings_and_normalize_for_comparison() {
        assert_eq!(parse_etag("").unwrap(), None);
        for raw in ["+/8=", "+/8", "-_8=", "-_8"] {
            assert_eq!(parse_etag(raw).unwrap().as_deref(), Some("+/8="));
        }
        for bytes in [vec![0], vec![0, 1], vec![0, 1, 2], vec![255; 16]] {
            let encoded = base64(&bytes);
            assert_eq!(parse_etag(&encoded).unwrap(), Some(encoded));
        }
        // Nonzero unused bits decode to the same bytes, which then simply fail to match.
        assert_eq!(parse_etag("CAF=").unwrap().as_deref(), Some("CAE="));
        assert_eq!(parse_etag("CAE").unwrap().as_deref(), Some("CAE="));
        let error = parse_etag("!!!").unwrap_err();
        assert_eq!(error.message, "Invalid value for ByteString: !!!");
        assert_eq!(
            error.location,
            Some(&super::super::error::POLICY_ETAG_FIELD)
        );
    }

    #[test]
    fn etag_accepts_one_pair_of_http_quotes_as_the_same_tag() {
        assert_eq!(parse_etag("\"+/8=\"").unwrap().as_deref(), Some("+/8="));
        for raw in [
            "\"\"",
            "\"",
            "\"+/8=",
            "+/8=\"",
            "\"\"+/8=\"\"",
            "W/\"+/8=\"",
        ] {
            assert!(parse_etag(raw).is_err(), "{raw}");
        }
    }
}
