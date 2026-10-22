use super::{JsonError, objects::base64};
use crate::auth::{
    Binding, Policy,
    roles::{GrantLevel, PREDEFINED_ROLES},
};
use hyper::StatusCode;
use serde_json::{Value, json};

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
