use super::{Identity, Permission, Role};
use crate::acl::ProjectTeam;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    str::FromStr,
};

#[derive(Debug)]
pub(crate) enum PolicyDecodeError {
    Json(serde_json::Error),
    Shape(&'static str),
}

impl std::fmt::Display for PolicyDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(f, "invalid stored policy: {error}"),
            Self::Shape(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for PolicyDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Shape(_) => None,
        }
    }
}

impl From<PolicyDecodeError> for crate::storage::failure::BackendCause {
    fn from(error: PolicyDecodeError) -> Self {
        Self::from_error(error)
    }
}

/// Whether `allUsers` and `allAuthenticatedUsers` bindings may grant access to a bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicAccess {
    Allowed,
    /// Public access prevention is enforced on the bucket or its parent project.
    Prevented,
}

/// What a member names, found once when it was parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MemberKind {
    AllUsers,
    AllAuthenticatedUsers,
    /// `KIND:ID`.
    Prefixed(MemberPrefix),
}

/// The `KIND` of a `KIND:ID` member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MemberPrefix {
    User,
    ServiceAccount,
    Group,
    Domain,
    Project(ProjectTeam),
}

impl MemberPrefix {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::ServiceAccount => "serviceAccount",
            Self::Group => "group",
            Self::Domain => "domain",
            Self::Project(team) => team.member_kind(),
        }
    }

    fn parse(prefix: &str) -> Option<Self> {
        [Self::User, Self::ServiceAccount, Self::Group, Self::Domain]
            .into_iter()
            .find(|candidate| candidate.as_str() == prefix)
            .or_else(|| ProjectTeam::from_member_kind(prefix).map(Self::Project))
    }
}

/// An IAM member of a form a stored bucket policy holds: `allUsers`, `allAuthenticatedUsers`, or
/// a known kind and an ID. Input rules (`policy_input::parse_member`) are stricter; the ACL bridge
/// writes `group:` and `domain:` members and longer IDs that input refuses. Equality and order
/// are the text's, because `text` is the first field and the kind is a function of it: stored
/// member order depends on them.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Member {
    text: String,
    kind: MemberKind,
}

crate::serde_util::serialize_as_str!(Member);

/// A member's parts, borrowed from its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemberRef<'a> {
    AllUsers,
    AllAuthenticatedUsers,
    User(&'a str),
    ServiceAccount(&'a str),
    Group(&'a str),
    Domain(&'a str),
    Project { team: ProjectTeam, project: &'a str },
}

impl Member {
    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    pub(crate) fn into_string(self) -> String {
        self.text
    }

    pub(crate) fn parsed(&self) -> MemberRef<'_> {
        let prefix = match self.kind {
            MemberKind::AllUsers => return MemberRef::AllUsers,
            MemberKind::AllAuthenticatedUsers => return MemberRef::AllAuthenticatedUsers,
            MemberKind::Prefixed(prefix) => prefix,
        };
        // Both constructors write `KIND:` before the ID.
        let id = self
            .text
            .get(prefix.as_str().len() + 1..)
            .unwrap_or_default();
        match prefix {
            MemberPrefix::User => MemberRef::User(id),
            MemberPrefix::ServiceAccount => MemberRef::ServiceAccount(id),
            MemberPrefix::Group => MemberRef::Group(id),
            MemberPrefix::Domain => MemberRef::Domain(id),
            MemberPrefix::Project(team) => MemberRef::Project { team, project: id },
        }
    }

    pub(crate) fn is_public(&self) -> bool {
        matches!(
            self.kind,
            MemberKind::AllUsers | MemberKind::AllAuthenticatedUsers
        )
    }
}

/// Total: the stored form accepts any ID after a known kind, so every built member reads back.
impl From<MemberRef<'_>> for Member {
    fn from(member: MemberRef<'_>) -> Self {
        let (prefix, id) = match member {
            MemberRef::AllUsers => {
                return Self {
                    text: "allUsers".into(),
                    kind: MemberKind::AllUsers,
                };
            }
            MemberRef::AllAuthenticatedUsers => {
                return Self {
                    text: "allAuthenticatedUsers".into(),
                    kind: MemberKind::AllAuthenticatedUsers,
                };
            }
            MemberRef::User(id) => (MemberPrefix::User, id),
            MemberRef::ServiceAccount(id) => (MemberPrefix::ServiceAccount, id),
            MemberRef::Group(id) => (MemberPrefix::Group, id),
            MemberRef::Domain(id) => (MemberPrefix::Domain, id),
            MemberRef::Project { team, project } => (MemberPrefix::Project(team), project),
        };
        Self {
            text: format!("{}:{id}", prefix.as_str()),
            kind: MemberKind::Prefixed(prefix),
        }
    }
}

/// A stored member of neither public name nor a known `KIND:`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidMember;

impl fmt::Display for InvalidMember {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a stored policy member")
    }
}

impl std::error::Error for InvalidMember {}

/// The stored form: the two public names, or `KIND:ID` with a known KIND and any ID.
impl FromStr for Member {
    type Err = InvalidMember;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let kind = match text {
            "allUsers" => MemberKind::AllUsers,
            "allAuthenticatedUsers" => MemberKind::AllAuthenticatedUsers,
            _ => {
                let (prefix, _) = text.split_once(':').ok_or(InvalidMember)?;
                MemberKind::Prefixed(MemberPrefix::parse(prefix).ok_or(InvalidMember)?)
            }
        };
        Ok(Self {
            text: text.to_owned(),
            kind,
        })
    }
}

impl fmt::Display for Member {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Debug shows the spelling, as the `String` it replaced did.
impl fmt::Debug for Member {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.text, f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub role: Role,
    pub members: Vec<Member>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Policy {
    pub bindings: Vec<Binding>,
    pub etag: Option<String>,
}

impl Policy {
    pub(crate) fn default_for(project: &str) -> Self {
        let team = |team| Member::from(MemberRef::Project { team, project });
        Self {
            bindings: vec![
                Binding {
                    role: Role::LegacyBucketOwner,
                    members: vec![team(ProjectTeam::Editors), team(ProjectTeam::Owners)],
                },
                Binding {
                    role: Role::LegacyBucketReader,
                    members: vec![team(ProjectTeam::Viewers)],
                },
            ],
            etag: None,
        }
    }

    /// What Cloud Storage stores for a validated policy (recorded 2026-11): bindings of one role
    /// merge into the first one's position, members are sorted and de-duplicated, and a binding
    /// without members is dropped.
    pub(crate) fn normalized(bindings: Vec<Binding>, etag: Option<String>) -> Self {
        let mut positions: HashMap<Role, usize> = HashMap::new();
        let mut roles: Vec<(Role, BTreeSet<Member>)> = Vec::with_capacity(bindings.len());
        for binding in bindings {
            if let Some(&position) = positions.get(&binding.role)
                && let Some((_, members)) = roles.get_mut(position)
            {
                members.extend(binding.members);
            } else {
                positions.insert(binding.role, roles.len());
                roles.push((binding.role, binding.members.into_iter().collect()));
            }
        }
        let bindings = roles
            .into_iter()
            .filter(|(_, members)| !members.is_empty())
            .map(|(role, members)| Binding {
                role,
                members: members.into_iter().collect(),
            })
            .collect();
        Self { bindings, etag }
    }

    pub(crate) fn allows(
        &self,
        identity: Option<&Identity>,
        permission: Permission,
        public_access: PublicAccess,
    ) -> bool {
        let public = public_access == PublicAccess::Allowed;
        self.bindings.iter().any(|binding| {
            binding.role.grants(permission)
                && binding.members.iter().any(|member| match member.parsed() {
                    MemberRef::AllUsers => public,
                    MemberRef::AllAuthenticatedUsers => public && identity.is_some(),
                    MemberRef::User(user) | MemberRef::ServiceAccount(user) => {
                        identity.is_some_and(|identity| identity.matches_acl_user(user))
                    }
                    MemberRef::Group(_) | MemberRef::Domain(_) | MemberRef::Project { .. } => false,
                })
        })
    }

    /// Whether any binding grants `allUsers` or `allAuthenticatedUsers`, which public access
    /// prevention refuses to store.
    pub(crate) fn names_public_principal(&self) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.members.iter().any(Member::is_public))
    }

    pub(crate) fn bindings_json(&self) -> Value {
        Value::Array(
            self.bindings
                .iter()
                .map(|binding| json!({"role": binding.role.as_str(), "members": binding.members}))
                .collect(),
        )
    }

    /// A stored role outside the pinned snapshot or a member of an unknown kind reads as
    /// corruption; no writer ever stored one.
    pub(crate) fn from_stored(bindings: &str, etag: String) -> Result<Self, PolicyDecodeError> {
        use PolicyDecodeError::Shape;
        let value: Value = serde_json::from_str(bindings).map_err(PolicyDecodeError::Json)?;
        let bindings = value
            .as_array()
            .ok_or(Shape("invalid stored bindings"))?
            .iter()
            .map(|binding| -> Result<Binding, PolicyDecodeError> {
                Ok(Binding {
                    role: binding["role"]
                        .as_str()
                        .and_then(|role| role.parse().ok())
                        .ok_or(Shape("invalid stored role"))?,
                    members: binding["members"]
                        .as_array()
                        .ok_or(Shape("invalid stored members"))?
                        .iter()
                        .map(|member| {
                            member
                                .as_str()
                                .and_then(|member| member.parse().ok())
                                .ok_or(Shape("invalid stored member"))
                        })
                        .collect::<Result<_, _>>()?,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            bindings,
            etag: Some(etag),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_policy_retains_the_json_cause_through_catalog_conversion() {
        let error = Policy::from_stored("{", "etag".into()).unwrap_err();
        let cause: crate::storage::failure::BackendCause = error.into();
        let error = crate::bucket::CatalogError::Corrupt(cause);
        let mut source: &(dyn std::error::Error + 'static) = &error;
        while !source.is::<serde_json::Error>() {
            source = source
                .source()
                .expect("JSON cause must remain in the chain");
        }
    }

    fn member_samples() -> [MemberRef<'static>; 9] {
        [
            MemberRef::AllUsers,
            MemberRef::AllAuthenticatedUsers,
            MemberRef::User("reader@example.test"),
            MemberRef::ServiceAccount("local:app"),
            MemberRef::Group("team@example.test"),
            MemberRef::Domain("example.test"),
            MemberRef::Project {
                team: ProjectTeam::Owners,
                project: "p",
            },
            MemberRef::Project {
                team: ProjectTeam::Editors,
                project: "p",
            },
            MemberRef::Project {
                team: ProjectTeam::Viewers,
                project: "p",
            },
        ]
    }

    /// `bucket_policies.bindings` keeps its text: role then members, each member its spelling.
    #[test]
    fn bindings_keep_their_stored_json() {
        let policy = Policy {
            bindings: vec![
                Binding {
                    role: Role::LegacyBucketOwner,
                    members: member_samples().into_iter().map(Member::from).collect(),
                },
                Binding {
                    role: Role::ObjectViewer,
                    members: vec!["user:".parse().unwrap()],
                },
            ],
            etag: None,
        };
        let stored = policy.bindings_json().to_string();
        assert_eq!(
            stored,
            r#"[{"role":"roles/storage.legacyBucketOwner","members":["allUsers","allAuthenticatedUsers","user:reader@example.test","serviceAccount:local:app","group:team@example.test","domain:example.test","projectOwner:p","projectEditor:p","projectViewer:p"]},{"role":"roles/storage.objectViewer","members":["user:"]}]"#
        );
        let read = Policy::from_stored(&stored, "CAE=".into()).unwrap();
        assert_eq!(read.bindings, policy.bindings);
    }

    /// No writer stored a role outside the snapshot or a member of an unknown kind, so one reads
    /// as corruption.
    #[test]
    fn unknown_stored_roles_and_member_kinds_are_corruption() {
        for (bindings, expected) in [
            (
                r#"[{"role":"roles/owner","members":["allUsers"]}]"#,
                "invalid stored role",
            ),
            (
                r#"[{"role":"roles/storage.objectViewer","members":["deleted:user:x"]}]"#,
                "invalid stored member",
            ),
            (
                r#"[{"role":"roles/storage.objectViewer","members":["allusers"]}]"#,
                "invalid stored member",
            ),
        ] {
            assert!(
                matches!(
                    Policy::from_stored(bindings, "CAE=".into()),
                    Err(PolicyDecodeError::Shape(message)) if message == expected
                ),
                "{bindings}"
            );
        }
    }

    /// Stored member order is text order, so the typed order must be the text's.
    #[test]
    fn members_order_by_their_text() {
        let members: Vec<Member> = member_samples().into_iter().map(Member::from).collect();
        let mut by_text: Vec<String> = members.iter().map(|member| member.to_string()).collect();
        by_text.sort_unstable();
        let mut sorted = members;
        sorted.sort();
        let sorted: Vec<&str> = sorted.iter().map(Member::as_str).collect();
        assert_eq!(sorted, by_text);
    }

    /// Every built member reads back as the same parts, including one with an empty ID.
    #[test]
    fn built_members_read_back() {
        let empty = [
            MemberRef::User(""),
            MemberRef::Domain(""),
            MemberRef::Project {
                team: ProjectTeam::Viewers,
                project: "",
            },
        ];
        for parts in member_samples().into_iter().chain(empty) {
            let built = Member::from(parts);
            let read: Member = built.as_str().parse().unwrap();
            assert_eq!(read, built);
            assert_eq!(read.parsed(), parts, "{built}");
        }
        assert!(Member::from(MemberRef::AllUsers).is_public());
        assert!(!Member::from(MemberRef::Domain("example.test")).is_public());
    }
}
