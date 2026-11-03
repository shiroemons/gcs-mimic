use super::{
    Binding, Policy,
    roles::{GrantLevel, PREDEFINED_ROLES},
};
use std::fmt;

/// Adapter-decoded policy fields, before shared semantic validation.
pub(crate) struct PolicyInput {
    pub version: i32,
    pub bindings: Vec<Binding>,
    pub etag: Option<String>,
}

#[derive(Debug)]
pub(crate) enum PolicyInputError {
    InvalidInput(String),
    UnsupportedInput(String),
}

impl fmt::Display for PolicyInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) | Self::UnsupportedInput(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for PolicyInputError {}

impl PolicyInput {
    pub(crate) fn into_policy(self) -> Result<Policy, PolicyInputError> {
        if !matches!(self.version, 0 | 1 | 3) {
            return Err(PolicyInputError::InvalidInput(
                "Policy version must be 0, 1 or 3".into(),
            ));
        }
        let mut count = 0;
        let mut bindings = Vec::new();
        for binding in self.bindings {
            validate_role(&binding.role)?;
            count += binding.members.len();
            validate_principal_count(count)?;
            for member in &binding.members {
                validate_member(member)?;
            }
            if !binding.members.is_empty() {
                bindings.push(binding);
            }
        }
        Ok(Policy {
            bindings,
            etag: self.etag,
        })
    }
}

pub(crate) fn validate_role(role: &str) -> Result<(), PolicyInputError> {
    if matches!(role, "roles/owner" | "roles/editor" | "roles/viewer") || custom_role(role) {
        return Err(PolicyInputError::UnsupportedInput(
            "basic or custom role".into(),
        ));
    }
    let Some((_, level, _)) = PREDEFINED_ROLES.iter().find(|(name, _, _)| *name == role) else {
        return Err(PolicyInputError::InvalidInput(format!(
            "Role {role} is not supported for this resource."
        )));
    };
    if *level == GrantLevel::Project {
        return Err(PolicyInputError::InvalidInput(
            "Role cannot be granted on a bucket".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_principal_count(count: usize) -> Result<(), PolicyInputError> {
    if count > 1500 {
        return Err(PolicyInputError::InvalidInput(
            "Policy exceeds 1500 principal occurrences".into(),
        ));
    }
    Ok(())
}

fn custom_role(role: &str) -> bool {
    let parts: Vec<_> = role.split('/').collect();
    matches!(parts.as_slice(), ["projects" | "organizations", owner, "roles", name]
        if !owner.is_empty() && !name.is_empty() && role.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte)))
}

pub(crate) fn validate_member(member: &str) -> Result<(), PolicyInputError> {
    if matches!(member, "allUsers" | "allAuthenticatedUsers") {
        return Ok(());
    }
    let unknown_type = || {
        PolicyInputError::InvalidInput(format!(
            "The member {member} is of an unknown type. Please set a valid type prefix for the member."
        ))
    };
    let (kind, id) = member.split_once(':').ok_or_else(unknown_type)?;
    if matches!(kind, "projectOwner" | "projectEditor" | "projectViewer") {
        return if !super::valid_project_id(id) {
            Err(PolicyInputError::InvalidInput(
                "Invalid convenience project".into(),
            ))
        } else {
            Ok(())
        };
    }
    if id.is_empty() || !id.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(PolicyInputError::InvalidInput(
            "Malformed policy member".into(),
        ));
    }
    match kind {
        "user" | "serviceAccount" if id.len() <= 254 => Ok(()),
        "group" | "domain" | "deleted" => {
            Err(PolicyInputError::UnsupportedInput("principal kind".into()))
        }
        "principal" | "principalSet" if id.starts_with("//") && id.len() > 2 => Err(
            PolicyInputError::UnsupportedInput("federated principal".into()),
        ),
        _ => Err(unknown_type()),
    }
}

#[test]
fn convenience_members_and_json_queries_share_project_identifier_rules() {
    for (project, valid) in [
        ("project".to_owned(), true),
        ("é".into(), true),
        ("a".repeat(256), true),
        ("".into(), false),
        ("a".repeat(257), false),
        ("has space".into(), false),
        ("has\tcontrol".into(), false),
    ] {
        assert_eq!(super::valid_project_id(&project), valid);
        assert_eq!(crate::json::validate_project(&project).is_ok(), valid);
        for prefix in ["projectOwner", "projectEditor", "projectViewer"] {
            assert_eq!(
                validate_member(&format!("{prefix}:{project}")).is_ok(),
                valid
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_policies_share_role_member_and_occurrence_validation() {
        let input = |version, role: &str, members: Vec<String>| PolicyInput {
            version,
            bindings: vec![Binding {
                role: role.into(),
                members,
            }],
            etag: Some("AA==".into()),
        };
        let role = "roles/storage.objectViewer";
        assert!(input(2, role, vec![]).into_policy().is_err());
        assert!(input(1, "roles/owner", vec![]).into_policy().is_err());
        assert!(
            input(1, role, vec!["allUsers".into(); 1501])
                .into_policy()
                .is_err()
        );
        assert!(
            input(1, role, vec!["unknown".into()])
                .into_policy()
                .is_err()
        );
        assert!(
            input(1, role, vec!["group:team@example.test".into()])
                .into_policy()
                .is_err()
        );
        let policy = input(3, role, vec![]).into_policy().unwrap();
        assert!(policy.bindings.is_empty());
        assert_eq!(policy.etag.as_deref(), Some("AA=="));
    }
}
