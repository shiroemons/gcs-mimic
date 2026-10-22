use super::{JsonError, objects::base64};
use crate::auth::{Binding, Policy, roles::PREDEFINED_ROLES};
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
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| invalid("etag must be a string"))?
                .to_owned(),
        ),
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
            if !PREDEFINED_ROLES.iter().any(|(name, _)| *name == role) {
                return Err(invalid("Unknown policy role"));
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

fn custom_role(role: &str) -> bool {
    let parts: Vec<_> = role.split('/').collect();
    matches!(parts.as_slice(), ["projects" | "organizations", owner, "roles", name]
        if !owner.is_empty() && !name.is_empty() && role.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b)))
}

fn validate_member(member: &str) -> Result<(), JsonError> {
    if matches!(member, "allUsers" | "allAuthenticatedUsers") {
        return Err(JsonError::unsupported("public principal"));
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
