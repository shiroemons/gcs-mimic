//! Protocol-independent access-control-list entries for buckets, default object ACLs and objects.
//!
//! Entries are stored exactly as validated here. A resource without stored entries uses the
//! `projectPrivate` canned ACL derived from the owning project, which is the documented default
//! for a bucket created without `predefinedAcl` and its default object ACL.

use crate::auth::Role;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The documented per-resource ACL entry limit.
pub(crate) const MAX_ENTRIES: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AclKind {
    Bucket,
    DefaultObject,
    Object,
}

impl AclKind {
    /// Whether an ACL of this kind may grant `role`: `WRITER` exists only on buckets.
    pub(crate) fn allows(self, role: AclRole) -> bool {
        match self {
            Self::Bucket => true,
            Self::DefaultObject | Self::Object => role != AclRole::Writer,
        }
    }

    /// The roles a message lists, in Cloud Storage's order.
    fn role_names(self) -> &'static str {
        match self {
            Self::Bucket => "OWNER, WRITER, READER",
            Self::DefaultObject | Self::Object => "OWNER, READER",
        }
    }
}

/// The resource whose ACL an operation reads or changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AclTarget {
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
    pub(crate) fn kind(&self) -> AclKind {
        match self {
            Self::Bucket(_) => AclKind::Bucket,
            Self::DefaultObject(_) => AclKind::DefaultObject,
            Self::Object { .. } => AclKind::Object,
        }
    }

    pub(crate) fn bucket(&self) -> &str {
        match self {
            Self::Bucket(bucket) | Self::DefaultObject(bucket) | Self::Object { bucket, .. } => {
                bucket
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AclEntry {
    pub entity: String,
    pub role: AclRole,
}

#[derive(Deserialize)]
pub(crate) struct AclEntryInput {
    pub entity: String,
    pub role: String,
}

pub(crate) fn parse_entries(
    kind: AclKind,
    input: Vec<AclEntryInput>,
) -> Result<Vec<AclEntry>, AclError> {
    if input.len() > MAX_ENTRIES {
        return Err(AclError::TooManyEntries);
    }
    let mut entities = std::collections::BTreeSet::new();
    let mut entries = Vec::with_capacity(input.len());
    for entry in input {
        Entity::parse(&entry.entity)?;
        let role = validate_role(kind, &entry.role)?;
        if !entities.insert(entry.entity.clone()) {
            return Err(AclError::DuplicateEntity(entry.entity));
        }
        entries.push(AclEntry {
            entity: entry.entity,
            role,
        });
    }
    Ok(entries)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub(crate) enum AclRole {
    Reader,
    Writer,
    Owner,
}

impl AclRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Reader => "READER",
            Self::Writer => "WRITER",
            Self::Owner => "OWNER",
        }
    }
}

crate::serde_util::serialize_as_str!(AclRole);

// Debug shows the wire spelling, as the String it replaced did.
impl fmt::Debug for AclRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

#[derive(Debug)]
pub(crate) struct InvalidAclRole;

impl fmt::Display for InvalidAclRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid ACL role")
    }
}

impl std::error::Error for InvalidAclRole {}

impl FromStr for AclRole {
    type Err = InvalidAclRole;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "READER" => Ok(Self::Reader),
            "WRITER" => Ok(Self::Writer),
            "OWNER" => Ok(Self::Owner),
            _ => Err(InvalidAclRole),
        }
    }
}

impl TryFrom<String> for AclRole {
    type Error = InvalidAclRole;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl AclRole {
    /// The legacy bucket IAM role that holds the same authority as this bucket ACL role.
    pub(crate) fn bucket_iam_role(self) -> Role {
        match self {
            Self::Reader => Role::LegacyBucketReader,
            Self::Writer => Role::LegacyBucketWriter,
            Self::Owner => Role::LegacyBucketOwner,
        }
    }

    /// The bucket ACL role a legacy bucket IAM role stands for; `None` for every other role.
    pub(crate) fn from_bucket_iam_role(role: Role) -> Option<Self> {
        [Self::Reader, Self::Writer, Self::Owner]
            .into_iter()
            .find(|acl| acl.bucket_iam_role() == role)
    }
}

pub(crate) fn object_permission(permission: crate::auth::Permission) -> bool {
    use crate::auth::Permission;
    matches!(
        permission,
        Permission::ObjectsGet
            | Permission::ObjectsUpdate
            | Permission::ObjectsGetIam
            | Permission::ObjectsSetIam
    )
}

pub(crate) fn object_role_allows(role: AclRole, permission: crate::auth::Permission) -> bool {
    object_permission(permission)
        && (role == AclRole::Owner
            || (role == AclRole::Reader && permission == crate::auth::Permission::ObjectsGet))
}

/// A rejected ACL entry or entry list. `Display` is the client-visible JSON error message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AclError {
    TooManyEntries,
    DuplicateEntity(String),
    InvalidEntity(String),
    /// Names no entity, so that a rejected value of any size is not repeated to the client.
    EntityTooLong,
    InvalidRole {
        kind: AclKind,
        role: String,
    },
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
            Self::EntityTooLong => {
                write!(f, "An ACL entity can hold at most {MAX_ENTITY_BYTES} bytes")
            }
            Self::InvalidRole { kind, role } => write!(
                f,
                "Invalid ACL role {role:?}; use one of {} (for example, READER)",
                kind.role_names()
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
        check_role(kind, entry.role)?;
        if !entities.insert(&entry.entity) {
            return Err(AclError::DuplicateEntity(entry.entity.clone()));
        }
    }
    Ok(())
}

/// Entity kinds from the pinned Discovery `entity` description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entity<'a> {
    AllUsers,
    AllAuthenticatedUsers,
    UserEmail(&'a str),
    UserId(&'a str),
    GroupEmail(&'a str),
    GroupId(&'a str),
    Domain(&'a str),
    ProjectTeam { team: &'a str, project: &'a str },
}

/// The longest entity: `project-viewers-` and a project ID of up to 256 bytes. Emails take at most
/// 254 bytes and domains 253, so each kind of entity fits, and ACLs and the policies derived from
/// them stay bounded by their entry counts.
const MAX_ENTITY_BYTES: usize = 272;

impl<'a> Entity<'a> {
    pub(crate) fn parse(entity: &'a str) -> Result<Self, AclError> {
        if entity.len() > MAX_ENTITY_BYTES {
            return Err(AclError::EntityTooLong);
        }
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

    pub(crate) fn is_public(self) -> bool {
        matches!(self, Self::AllUsers | Self::AllAuthenticatedUsers)
    }

    pub(crate) fn matches(
        self,
        identity: Option<&crate::auth::Identity>,
        public: crate::auth::PublicAccess,
    ) -> bool {
        match self {
            Self::AllUsers => public == crate::auth::PublicAccess::Allowed,
            Self::AllAuthenticatedUsers => {
                public == crate::auth::PublicAccess::Allowed && identity.is_some()
            }
            Self::UserEmail(user) | Self::UserId(user) => {
                identity.is_some_and(|id| id.matches_acl_user(user))
            }
            Self::GroupEmail(_) | Self::GroupId(_) | Self::Domain(_) | Self::ProjectTeam { .. } => {
                false
            }
        }
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

/// A client-supplied role name for an ACL of `kind`.
pub(crate) fn validate_role(kind: AclKind, role: &str) -> Result<AclRole, AclError> {
    role.parse::<AclRole>()
        .ok()
        .filter(|parsed| kind.allows(*parsed))
        .ok_or_else(|| AclError::InvalidRole {
            kind,
            role: role.to_owned(),
        })
}

/// A typed role checked against the ACL it is written to or read from.
pub(crate) fn check_role(kind: AclKind, role: AclRole) -> Result<(), AclError> {
    if kind.allows(role) {
        Ok(())
    } else {
        Err(AclError::InvalidRole {
            kind,
            role: role.as_str().to_owned(),
        })
    }
}

/// The project number rendered by bucket resources: numeric project IDs are used verbatim and
/// other local project IDs map to `0`, because the local registry has no project numbers.
pub(crate) fn project_number(project: &str) -> String {
    crate::decimal::Grammar::Digits
        .parse::<u64>(project)
        .unwrap_or(0)
        .to_string()
}

/// The `projectPrivate` canned ACL used for buckets and objects without stored entries.
pub(crate) fn project_private(project: &str) -> Vec<AclEntry> {
    let number = project_number(project);
    [
        ("owners", AclRole::Owner),
        ("editors", AclRole::Owner),
        ("viewers", AclRole::Reader),
    ]
    .into_iter()
    .map(|(team, role)| AclEntry {
        entity: format!("project-{team}-{number}"),
        role,
    })
    .collect()
}

/// The 400 message for an upload `predefinedAcl` under uniform access. It begins with the
/// sentence Cloud Storage returns, then adds the local remedy.
pub(crate) const UNIFORM_ACCESS_UPLOAD_ACL: &str = "Cannot insert legacy ACL for an object when uniform bucket-level access is enabled; remove predefinedAcl (for example, omit predefinedAcl=publicRead) and use bucket IAM permissions";

crate::config::string_enum! {
    no_default listed
    /// A JSON API object `predefinedAcl` value. `publicReadWrite` is bucket-only and is not one.
    pub(crate) enum PredefinedObjectAcl {
        AuthenticatedRead => "authenticatedRead",
        BucketOwnerFullControl => "bucketOwnerFullControl",
        BucketOwnerRead => "bucketOwnerRead",
        Private => "private",
        ProjectPrivate => "projectPrivate",
        PublicRead => "publicRead",
    }
    "predefinedAcl must be authenticatedRead, bucketOwnerFullControl, bucketOwnerRead, private, projectPrivate or publicRead"
}

impl PredefinedObjectAcl {
    /// The accepted spellings of a default object ACL, lowercased as Cloud Storage lists them.
    pub(crate) const ALLOWED_DEFAULT: &str = "[authenticatedread, bucketownerfullcontrol, bucketownerread, private, projectprivate, publicread]";

    /// Case-insensitive, as the service is (it reports its allowed values lowercased).
    pub(crate) fn parse_ignoring_case(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|acl| acl.as_str().eq_ignore_ascii_case(value))
    }

    /// The canned entries apart from the object owner's OWNER grant, which publication adds
    /// from the object's recorded owner. The bucket owner is the owning project's owners team.
    pub(crate) fn entries(self, project: &str) -> Vec<AclEntry> {
        let entry = |entity: String, role| AclEntry { entity, role };
        let owners = || format!("project-owners-{}", project_number(project));
        match self {
            Self::AuthenticatedRead => vec![entry("allAuthenticatedUsers".into(), AclRole::Reader)],
            Self::BucketOwnerFullControl => vec![entry(owners(), AclRole::Owner)],
            Self::BucketOwnerRead => vec![entry(owners(), AclRole::Reader)],
            Self::Private => Vec::new(),
            Self::ProjectPrivate => project_private(project),
            Self::PublicRead => vec![entry("allUsers".into(), AclRole::Reader)],
        }
    }
}

/// An `x-goog-acl` value the XML API does not define, kept for the error that names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidCannedAcl<'a>(pub(crate) &'a str);

impl std::fmt::Display for InvalidCannedAcl<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Invalid canned acl: {}", self.0)
    }
}

impl std::error::Error for InvalidCannedAcl<'_> {}

/// The XML API's hyphenated names. The JSON API's camelCase values, which `FromStr` reads, are
/// invalid in `x-goog-acl` (recorded 2026-11-09).
impl PredefinedObjectAcl {
    /// The XML API's `x-goog-acl` spelling.
    pub(crate) fn canned_name(self) -> &'static str {
        match self {
            Self::AuthenticatedRead => "authenticated-read",
            Self::BucketOwnerFullControl => "bucket-owner-full-control",
            Self::BucketOwnerRead => "bucket-owner-read",
            Self::Private => "private",
            Self::ProjectPrivate => "project-private",
            Self::PublicRead => "public-read",
        }
    }

    pub(crate) fn from_canned(value: &str) -> Result<Self, InvalidCannedAcl<'_>> {
        Self::ALL
            .iter()
            .copied()
            .find(|acl| acl.canned_name() == value)
            .ok_or(InvalidCannedAcl(value))
    }
}

crate::config::string_enum! {
    no_default listed
    /// A JSON API bucket `predefinedAcl` value.
    ///
    /// The entries follow the documented canned lists: the project owners team always holds
    /// OWNER, and the public values add a grant to `allUsers` or `allAuthenticatedUsers`. Cloud
    /// Storage applied `private` as the owners team alone and `projectPrivate` as the three
    /// project teams (recorded 2026-10-31).
    pub(crate) enum PredefinedBucketAcl {
        AuthenticatedRead => "authenticatedRead",
        Private => "private",
        ProjectPrivate => "projectPrivate",
        PublicRead => "publicRead",
        PublicReadWrite => "publicReadWrite",
    }
    "predefinedAcl must be authenticatedRead, private, projectPrivate, publicRead or publicReadWrite"
}

impl PredefinedBucketAcl {
    /// The accepted spellings, lowercased as Cloud Storage lists them in its error message.
    pub(crate) const ALLOWED: &str =
        "[authenticatedread, private, projectprivate, publicread, publicreadwrite]";

    /// Case-insensitive, as the service is (it reports its allowed values lowercased).
    pub(crate) fn parse_ignoring_case(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|acl| acl.as_str().eq_ignore_ascii_case(value))
    }

    pub(crate) fn entries(self, project: &str) -> Vec<AclEntry> {
        let entry = |entity: String, role| AclEntry { entity, role };
        let owners = entry(
            format!("project-owners-{}", project_number(project)),
            AclRole::Owner,
        );
        match self {
            Self::ProjectPrivate => project_private(project),
            Self::Private => vec![owners],
            Self::AuthenticatedRead => {
                vec![
                    owners,
                    entry("allAuthenticatedUsers".into(), AclRole::Reader),
                ]
            }
            Self::PublicRead => vec![owners, entry("allUsers".into(), AclRole::Reader)],
            Self::PublicReadWrite => vec![owners, entry("allUsers".into(), AclRole::Writer)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predefined_object_acls_round_trip_and_exclude_bucket_only_values() {
        for value in [
            "authenticatedRead",
            "bucketOwnerFullControl",
            "bucketOwnerRead",
            "private",
            "projectPrivate",
            "publicRead",
        ] {
            let parsed = value.parse::<PredefinedObjectAcl>().unwrap();
            assert_eq!(parsed.as_str(), value);
            validate_entries(AclKind::Object, &parsed.entries("123")).unwrap();
        }
        assert_eq!(PredefinedObjectAcl::ALL.len(), 6);
        for value in ["publicReadWrite", "", "PublicRead", "public-read"] {
            assert!(value.parse::<PredefinedObjectAcl>().is_err(), "{value}");
        }
        for (canned, json) in [
            ("authenticated-read", "authenticatedRead"),
            ("bucket-owner-full-control", "bucketOwnerFullControl"),
            ("bucket-owner-read", "bucketOwnerRead"),
            ("private", "private"),
            ("project-private", "projectPrivate"),
            ("public-read", "publicRead"),
        ] {
            assert_eq!(
                PredefinedObjectAcl::from_canned(canned).ok(),
                json.parse::<PredefinedObjectAcl>().ok(),
                "{canned}"
            );
        }
        for value in [
            "projectPrivate",
            "publicRead",
            "public-read-write",
            "PRIVATE",
            "",
        ] {
            assert_eq!(
                PredefinedObjectAcl::from_canned(value),
                Err(InvalidCannedAcl(value))
            );
        }
        assert_eq!(
            PredefinedObjectAcl::BucketOwnerRead.entries("local"),
            vec![AclEntry {
                entity: "project-owners-0".into(),
                role: crate::acl::AclRole::Reader
            }]
        );
        assert_eq!(
            PredefinedObjectAcl::ProjectPrivate.entries("123"),
            project_private("123")
        );
    }

    /// Query parameters name predefined ACLs in any case, as the service accepts them.
    #[test]
    fn predefined_acls_parse_ignoring_case() {
        assert_eq!(
            PredefinedObjectAcl::parse_ignoring_case("PUBLICREAD"),
            Some(PredefinedObjectAcl::PublicRead)
        );
        assert_eq!(
            PredefinedObjectAcl::parse_ignoring_case("publicReadWrite"),
            None
        );
        assert_eq!(
            PredefinedBucketAcl::parse_ignoring_case("PUBLICREAD"),
            Some(PredefinedBucketAcl::PublicRead)
        );
        assert_eq!(
            PredefinedBucketAcl::parse_ignoring_case("public-read"),
            None
        );
        assert_eq!(PredefinedBucketAcl::ALL.len(), 5);
        for &acl in PredefinedBucketAcl::ALL {
            assert_eq!(acl.as_str().parse::<PredefinedBucketAcl>(), Ok(acl));
            assert_eq!(
                PredefinedBucketAcl::parse_ignoring_case(&acl.as_str().to_ascii_uppercase()),
                Some(acl)
            );
        }
    }

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
        // Each kind's longest value fits, and only the length refuses what follows it.
        let email = format!("{}@example.com", "x".repeat(254 - "@example.com".len()));
        for entity in [
            format!("user-{email}"),
            format!("group-{email}"),
            format!("project-viewers-{}", "p".repeat(256)),
        ] {
            assert!(Entity::parse(&entity).is_ok(), "{entity}");
        }
        let longest = format!("project-viewers-{}", "p".repeat(256));
        assert_eq!(longest.len(), MAX_ENTITY_BYTES);
        assert_eq!(
            Entity::parse(&format!("{longest}p")),
            Err(AclError::EntityTooLong)
        );
        assert_eq!(
            Entity::parse(&format!("user-{}@example.com", "x".repeat(16 * 1024))),
            Err(AclError::EntityTooLong)
        );
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
        for kind in [AclKind::Bucket, AclKind::DefaultObject, AclKind::Object] {
            for role in [AclRole::Owner, AclRole::Writer, AclRole::Reader] {
                let checked = check_role(kind, role);
                assert_eq!(
                    checked.is_ok(),
                    kind == AclKind::Bucket || role != AclRole::Writer,
                    "{kind:?} {role:?}"
                );
                assert_eq!(
                    checked,
                    validate_role(kind, role.as_str()).map(|_| ()),
                    "{kind:?} {role:?}"
                );
            }
        }
    }

    #[test]
    fn project_private_uses_the_rendered_project_number() {
        assert_eq!(
            project_private("123"),
            vec![
                AclEntry {
                    entity: "project-owners-123".into(),
                    role: crate::acl::AclRole::Owner
                },
                AclEntry {
                    entity: "project-editors-123".into(),
                    role: crate::acl::AclRole::Owner
                },
                AclEntry {
                    entity: "project-viewers-123".into(),
                    role: crate::acl::AclRole::Reader
                },
            ]
        );
        assert_eq!(project_private("local")[0].entity, "project-owners-0");
        assert_eq!(project_number("99999999999999999999999"), "0");
    }
}
