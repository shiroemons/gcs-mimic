use super::{
    Binding, Policy, Role,
    policy::{Member, MemberRef},
    roles::GrantLevel,
};
use crate::acl::ProjectTeam;
use std::fmt;

/// Adapter-decoded policy fields, before shared semantic validation.
pub(crate) struct PolicyInput {
    pub version: i32,
    pub bindings: Vec<BindingInput>,
    pub etag: Option<String>,
}

/// One binding as an adapter decoded it, before validation.
pub(crate) struct BindingInput {
    pub role: String,
    pub members: Vec<String>,
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
        let mut bindings = Vec::with_capacity(self.bindings.len());
        for binding in self.bindings {
            let role = parse_role(&binding.role)?;
            count += binding.members.len();
            validate_principal_count(count)?;
            let members = binding
                .members
                .iter()
                .map(|member| parse_member(member))
                .collect::<Result<_, _>>()?;
            bindings.push(Binding { role, members });
        }
        Ok(Policy::normalized(bindings, self.etag))
    }
}

/// A role a bucket policy may grant.
pub(crate) fn parse_role(role: &str) -> Result<Role, PolicyInputError> {
    if matches!(role, "roles/owner" | "roles/editor" | "roles/viewer") || custom_role(role) {
        return Err(PolicyInputError::UnsupportedInput(
            "basic or custom role".into(),
        ));
    }
    let Ok(parsed) = role.parse::<Role>() else {
        return Err(PolicyInputError::InvalidInput(format!(
            "Role {role} is not supported for this resource."
        )));
    };
    if parsed.level() == GrantLevel::Project {
        return Err(PolicyInputError::InvalidInput(
            "Role cannot be granted on a bucket".into(),
        ));
    }
    Ok(parsed)
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

/// A member a policy request may name.
pub(crate) fn parse_member(member: &str) -> Result<Member, PolicyInputError> {
    match member {
        "allUsers" => return Ok(Member::from(MemberRef::AllUsers)),
        "allAuthenticatedUsers" => return Ok(Member::from(MemberRef::AllAuthenticatedUsers)),
        _ => {}
    }
    let unknown_type = || {
        PolicyInputError::InvalidInput(format!(
            "The member {member} is of an unknown type. Please set a valid type prefix for the member."
        ))
    };
    let (kind, id) = member.split_once(':').ok_or_else(unknown_type)?;
    if let Some(team) = ProjectTeam::from_member_kind(kind) {
        return if !super::valid_project_id(id) {
            Err(PolicyInputError::InvalidInput(
                "Invalid convenience project".into(),
            ))
        } else {
            Ok(Member::from(MemberRef::Project { team, project: id }))
        };
    }
    if id.is_empty() || !id.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(PolicyInputError::InvalidInput(
            "Malformed policy member".into(),
        ));
    }
    match kind {
        "user" if id.len() <= 254 => Ok(Member::from(MemberRef::User(id))),
        "serviceAccount" if id.len() <= 254 => Ok(Member::from(MemberRef::ServiceAccount(id))),
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
            assert_eq!(parse_member(&format!("{prefix}:{project}")).is_ok(), valid);
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
            bindings: vec![BindingInput {
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

    #[test]
    fn policies_merge_roles_and_sort_and_deduplicate_members() {
        let input = |role: &str, members: &[&str]| BindingInput {
            role: role.into(),
            members: members.iter().map(|member| (*member).to_owned()).collect(),
        };
        let stored = |role, members: &[&str]| Binding {
            role,
            members: members
                .iter()
                .map(|member| member.parse().unwrap())
                .collect(),
        };
        let owner = "roles/storage.legacyBucketOwner";
        let viewer = "roles/storage.objectViewer";
        let policy = PolicyInput {
            version: 1,
            bindings: vec![
                input(
                    owner,
                    &["projectOwner:p", "projectEditor:p", "projectOwner:p"],
                ),
                input(viewer, &["projectViewer:p"]),
                input("roles/storage.legacyBucketReader", &[]),
                input(viewer, &["projectEditor:p", "projectViewer:p"]),
            ],
            etag: None,
        }
        .into_policy()
        .unwrap();
        assert_eq!(
            policy.bindings,
            [
                stored(
                    Role::LegacyBucketOwner,
                    &["projectEditor:p", "projectOwner:p"]
                ),
                stored(Role::ObjectViewer, &["projectEditor:p", "projectViewer:p"]),
            ]
        );
    }
}
