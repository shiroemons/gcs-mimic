use super::{BucketRequest, Fields, JsonError, objects::base64, query};
use crate::auth::{
    Binding, Policy,
    roles::{GrantLevel, PREDEFINED_ROLES},
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
        let bucket = query::decode(segment, false)?;
        super::buckets::validate_bucket_name_for_profile(&bucket, profile)?;
        if *method != Method::GET {
            let mut error = JsonError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "methodNotAllowed",
                "Use GET for bucket testIamPermissions",
            );
            error.allow = Some("GET");
            return Err(error);
        }
        let (permissions, _) = test_permissions_query(uri.query())?;
        Ok(BucketRequest::TestIam {
            bucket,
            permissions,
        })
    })())
}

pub fn test_permissions_query(raw: Option<&str>) -> Result<(Vec<String>, Fields), JsonError> {
    let raw = raw.unwrap_or_default();
    if raw.len() > 16 * 1024 {
        return Err(JsonError::invalid_parameter("Query exceeds 16 KiB"));
    }
    let mut permissions = Vec::new();
    let mut rest = Vec::new();
    for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if query::decode(key, true)? == "permissions" {
            let permission = query::decode(value, true)?;
            if matches!(
                permission.as_str(),
                crate::auth::BUCKETS_CREATE | crate::auth::BUCKETS_LIST
            ) {
                return Err(invalid(
                    "Project-only permission; test a bucket permission such as storage.buckets.get",
                ));
            }
            if crate::auth::Permission::try_from(permission.as_str()).is_err() {
                if PREDEFINED_ROLES
                    .iter()
                    .any(|(_, _, names)| names.contains(&permission.as_str()))
                {
                    return Err(JsonError::unsupported(&format!(
                        "permission {permission}; use a supported operation permission such as storage.objects.get"
                    )));
                }
                return Err(invalid(
                    "Invalid permission; use a supported name such as storage.objects.get",
                ));
            }
            if !permissions.contains(&permission) {
                permissions.push(permission);
            }
        } else {
            rest.push(pair);
        }
    }
    if permissions.is_empty() {
        return Err(JsonError::new(
            StatusCode::BAD_REQUEST,
            "required",
            "Missing permissions; supply permissions=storage.objects.get",
        ));
    }
    let mut query = query::parse_query(&rest.join("&"))?;
    if query.get("userProject").is_some_and(String::is_empty) {
        query.remove("userProject");
    }
    let fields = Fields::take(&mut query, "TestIamPermissionsResponse")?;
    query::common(&mut query)?;
    query::finish(query)?;
    Ok((permissions, fields))
}

fn invalid(message: &str) -> JsonError {
    JsonError::new(StatusCode::BAD_REQUEST, "invalid", message)
}

pub fn parse_policy(bytes: &[u8]) -> Result<Policy, JsonError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        JsonError::new(StatusCode::BAD_REQUEST, "parseError", "Invalid JSON policy")
    })?;
    let body = value
        .as_object()
        .ok_or_else(|| invalid("Policy must be an object"))?;
    if let Some(version) = body.get("version")
        && !matches!(version.as_u64(), Some(0 | 1 | 3))
    {
        return Err(invalid("Policy version must be 0, 1 or 3"));
    }
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
            if matches!(role, "roles/owner" | "roles/editor" | "roles/viewer") || custom_role(role)
            {
                return Err(JsonError::unsupported("basic or custom role"));
            }
            let Some((_, level, _)) = PREDEFINED_ROLES.iter().find(|(name, _, _)| *name == role)
            else {
                return Err(invalid("Unknown policy role"));
            };
            if *level == GrantLevel::Project {
                return Err(invalid("Role cannot be granted on a bucket"));
            }
            let values = binding
                .get("members")
                .and_then(Value::as_array)
                .filter(|values| !values.is_empty())
                .ok_or_else(|| invalid("binding members must be a nonempty array"))?;
            principals += values.len();
            if principals > 1500 {
                return Err(invalid("Policy exceeds 1500 principal occurrences"));
            }
            let members = values
                .iter()
                .map(|value| {
                    let member = value
                        .as_str()
                        .ok_or_else(|| invalid("member must be a string"))?;
                    validate_member(member)?;
                    Ok(member.to_owned())
                })
                .collect::<Result<_, JsonError>>()?;
            bindings.push(Binding {
                role: role.into(),
                members,
            });
        }
    }
    Ok(Policy { bindings, etag })
}

fn parse_etag(raw: &str) -> Result<Option<String>, JsonError> {
    if raw.is_empty() {
        return Ok(None);
    }
    let invalid = || invalid("etag must be valid base64");
    let data = raw.trim_end_matches('=');
    let padding = raw.len() - data.len();
    if data.len() % 4 == 1
        || padding > 2
        || (padding > 0 && (!raw.len().is_multiple_of(4) || padding != (4 - data.len() % 4) % 4))
    {
        return Err(invalid());
    }
    let mut decoded = Vec::with_capacity(data.len() * 3 / 4);
    let mut bits = 0u32;
    let mut count = 0;
    for byte in data.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err(invalid()),
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            decoded.push((bits >> count) as u8);
        }
    }
    if bits & ((1 << count) - 1) != 0 {
        return Err(invalid());
    }
    Ok(Some(base64(&decoded)))
}

fn custom_role(role: &str) -> bool {
    let parts: Vec<_> = role.split('/').collect();
    matches!(parts.as_slice(), ["projects" | "organizations", owner, "roles", name]
        if !owner.is_empty() && !name.is_empty() && role.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b)))
}

fn validate_member(member: &str) -> Result<(), JsonError> {
    if matches!(member, "allUsers" | "allAuthenticatedUsers") {
        return Ok(());
    }
    let (kind, id) = member
        .split_once(':')
        .ok_or_else(|| invalid("Malformed policy member"))?;
    if matches!(kind, "projectOwner" | "projectEditor" | "projectViewer") {
        return super::validate_project(id).map_err(|_| invalid("Invalid convenience project"));
    }
    if id.is_empty() || !id.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(invalid("Malformed policy member"));
    }
    match kind {
        "user" | "serviceAccount" if id.len() <= 254 => Ok(()),
        "group" | "domain" | "deleted" => Err(JsonError::unsupported("principal kind")),
        "principal" | "principalSet" if id.starts_with("//") && id.len() > 2 => {
            Err(JsonError::unsupported("federated principal"))
        }
        _ => Err(invalid("Malformed or unknown policy member")),
    }
}

pub fn policy_resource(policy: &Policy, bucket: &str) -> Value {
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
    fn etag_bytes_accept_protojson_encodings_and_normalize_for_comparison() {
        assert_eq!(parse_etag("").unwrap(), None);
        for raw in ["+/8=", "+/8", "-_8=", "-_8"] {
            assert_eq!(parse_etag(raw).unwrap().as_deref(), Some("+/8="));
        }
        for bytes in [vec![0], vec![0, 1], vec![0, 1, 2], vec![255; 16]] {
            let encoded = base64(&bytes);
            assert_eq!(parse_etag(&encoded).unwrap(), Some(encoded));
        }
    }
}
