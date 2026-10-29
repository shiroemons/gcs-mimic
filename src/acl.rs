//! Protocol-independent access-control-list entries for buckets, default object ACLs and objects.
//!
//! Entries are stored exactly as validated here. A resource without stored entries uses the
//! `projectPrivate` canned ACL derived from the owning project, which is the documented default
//! for a bucket created without `predefinedAcl` and its default object ACL.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The documented per-resource ACL entry limit.
pub const MAX_ENTRIES: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AclKind {
    Bucket,
    DefaultObject,
    Object,
}

impl AclKind {
    fn roles(self) -> &'static [&'static str] {
        match self {
            Self::Bucket => &["OWNER", "WRITER", "READER"],
            Self::DefaultObject | Self::Object => &["OWNER", "READER"],
        }
    }
}

/// The resource whose ACL an operation reads or changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AclTarget {
    Bucket(String),
    DefaultObject(String),
    Object {
        bucket: String,
        name: String,
        /// `None` selects the live generation.
        generation: Option<i64>,
    },
}

impl AclTarget {
    pub fn kind(&self) -> AclKind {
        match self {
            Self::Bucket(_) => AclKind::Bucket,
            Self::DefaultObject(_) => AclKind::DefaultObject,
            Self::Object { .. } => AclKind::Object,
        }
    }

    pub fn bucket(&self) -> &str {
        match self {
            Self::Bucket(bucket) | Self::DefaultObject(bucket) | Self::Object { bucket, .. } => {
                bucket
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclEntry {
    pub entity: String,
    pub role: String,
}

/// A rejected ACL entry or entry list. `Display` is the client-visible JSON error message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AclError {
    TooManyEntries,
    DuplicateEntity(String),
    InvalidEntity(String),
    InvalidRole { kind: AclKind, role: String },
}

impl fmt::Display for AclError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyEntries => write!(f, "An ACL can hold at most {MAX_ENTRIES} entries"),
            Self::DuplicateEntity(entity) => write!(f, "Duplicate ACL entity {entity:?}"),
            Self::InvalidEntity(entity) => write!(
                f,
                "Invalid ACL entity {entity:?}; use user-EMAIL, group-EMAIL, domain-DOMAIN, project-owners-PROJECT, allUsers or allAuthenticatedUsers (for example, user-liz@example.com)"
            ),
            Self::InvalidRole { kind, role } => write!(
                f,
                "Invalid ACL role {role:?}; use one of {} (for example, READER)",
                kind.roles().join(", ")
            ),
        }
    }
}

impl std::error::Error for AclError {}

pub(crate) fn validate_entries(kind: AclKind, entries: &[AclEntry]) -> Result<(), AclError> {
    if entries.len() > MAX_ENTRIES {
        return Err(AclError::TooManyEntries);
    }
    let mut entities = std::collections::BTreeSet::new();
    for entry in entries {
        Entity::parse(&entry.entity)?;
        validate_role(kind, &entry.role)?;
        if !entities.insert(&entry.entity) {
            return Err(AclError::DuplicateEntity(entry.entity.clone()));
        }
    }
    Ok(())
}

/// Entity kinds from the pinned Discovery `entity` description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entity<'a> {
    AllUsers,
    AllAuthenticatedUsers,
    UserEmail(&'a str),
    UserId(&'a str),
    GroupEmail(&'a str),
    GroupId(&'a str),
    Domain(&'a str),
    ProjectTeam { team: &'a str, project: &'a str },
}

impl<'a> Entity<'a> {
    pub fn parse(entity: &'a str) -> Result<Self, AclError> {
        let invalid = || AclError::InvalidEntity(entity.to_owned());
        let email = |value: &str| {
            value.split_once('@').is_some_and(|(local, domain)| {
                !local.is_empty()
                    && valid_domain(domain)
                    && local
                        .bytes()
                        .all(|byte| byte.is_ascii_graphic() && byte != b'@')
            })
        };
        let id =
            |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric());
        Ok(match entity {
            "allUsers" => Self::AllUsers,
            "allAuthenticatedUsers" => Self::AllAuthenticatedUsers,
            _ => {
                if let Some(value) = entity.strip_prefix("user-") {
                    if email(value) {
                        Self::UserEmail(value)
                    } else if id(value) {
                        Self::UserId(value)
                    } else {
                        return Err(invalid());
                    }
                } else if let Some(value) = entity.strip_prefix("group-") {
                    if email(value) {
                        Self::GroupEmail(value)
                    } else if id(value) {
                        Self::GroupId(value)
                    } else {
                        return Err(invalid());
                    }
                } else if let Some(value) = entity.strip_prefix("domain-") {
                    if !valid_domain(value) {
                        return Err(invalid());
                    }
                    Self::Domain(value)
                } else if let Some(value) = entity.strip_prefix("project-") {
                    let (team, project) = value.split_once('-').ok_or_else(invalid)?;
                    if !matches!(team, "owners" | "editors" | "viewers")
                        || project.is_empty()
                        || !project
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    {
                        return Err(invalid());
                    }
                    Self::ProjectTeam { team, project }
                } else {
                    return Err(invalid());
                }
            }
        })
    }

    pub fn is_public(self) -> bool {
        matches!(self, Self::AllUsers | Self::AllAuthenticatedUsers)
    }
}

fn valid_domain(domain: &str) -> bool {
    domain.len() <= 253
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

pub fn validate_role(kind: AclKind, role: &str) -> Result<(), AclError> {
    if kind.roles().contains(&role) {
        return Ok(());
    }
    Err(AclError::InvalidRole {
        kind,
        role: role.to_owned(),
    })
}

/// The project number rendered by bucket resources: numeric project IDs are used verbatim and
/// other local project IDs map to `0`, because the local registry has no project numbers.
pub fn project_number(project: &str) -> String {
    if project.bytes().all(|byte| byte.is_ascii_digit()) {
        project.parse::<u64>().unwrap_or(0).to_string()
    } else {
        "0".to_owned()
    }
}

/// The `projectPrivate` canned ACL used for buckets and objects without stored entries.
pub fn project_private(project: &str) -> Vec<AclEntry> {
    let number = project_number(project);
    [
        ("owners", "OWNER"),
        ("editors", "OWNER"),
        ("viewers", "READER"),
    ]
    .into_iter()
    .map(|(team, role)| AclEntry {
        entity: format!("project-{team}-{number}"),
        role: role.to_owned(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_follow_the_discovery_forms() {
        for (entity, parsed) in [
            ("allUsers", Entity::AllUsers),
            ("allAuthenticatedUsers", Entity::AllAuthenticatedUsers),
            ("user-liz@example.com", Entity::UserEmail("liz@example.com")),
            ("user-00b4903a97", Entity::UserId("00b4903a97")),
            (
                "group-team@example.com",
                Entity::GroupEmail("team@example.com"),
            ),
            ("group-00b4903a97", Entity::GroupId("00b4903a97")),
            ("domain-example.com", Entity::Domain("example.com")),
            (
                "project-owners-123",
                Entity::ProjectTeam {
                    team: "owners",
                    project: "123",
                },
            ),
            (
                "project-viewers-local-project",
                Entity::ProjectTeam {
                    team: "viewers",
                    project: "local-project",
                },
            ),
        ] {
            assert_eq!(Entity::parse(entity), Ok(parsed), "{entity}");
        }
        for entity in [
            "",
            "allusers",
            "user-",
            "user-a b@example.com",
            "user-liz@",
            "user-liz@localhost",
            "group-",
            "domain-",
            "domain-localhost",
            "domain-exa mple.com",
            "project-owners-",
            "project-admins-123",
            "project-owners",
            "serviceAccount:sa@example.com",
        ] {
            assert!(Entity::parse(entity).is_err(), "{entity}");
        }
        assert!(Entity::AllUsers.is_public() && Entity::AllAuthenticatedUsers.is_public());
        assert!(!Entity::Domain("example.com").is_public());
    }

    #[test]
    fn roles_depend_on_the_resource_kind() {
        for role in ["OWNER", "WRITER", "READER"] {
            assert!(validate_role(AclKind::Bucket, role).is_ok());
        }
        for kind in [AclKind::DefaultObject, AclKind::Object] {
            assert!(validate_role(kind, "OWNER").is_ok());
            assert!(validate_role(kind, "READER").is_ok());
            assert!(validate_role(kind, "WRITER").is_err());
        }
        for role in ["reader", "", "FULL_CONTROL"] {
            assert!(validate_role(AclKind::Bucket, role).is_err(), "{role}");
        }
    }

    #[test]
    fn project_private_uses_the_rendered_project_number() {
        assert_eq!(
            project_private("123"),
            vec![
                AclEntry {
                    entity: "project-owners-123".into(),
                    role: "OWNER".into()
                },
                AclEntry {
                    entity: "project-editors-123".into(),
                    role: "OWNER".into()
                },
                AclEntry {
                    entity: "project-viewers-123".into(),
                    role: "READER".into()
                },
            ]
        );
        assert_eq!(project_private("local")[0].entity, "project-owners-0");
        assert_eq!(project_number("99999999999999999999999"), "0");
    }
}
