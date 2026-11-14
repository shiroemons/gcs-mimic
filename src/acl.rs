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

/// One ACL grant. Stored rows decode the entity's form; the size cap and the role per kind are
/// the write policy that [`validate_entries`] applies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AclEntry {
    pub entity: Entity,
    pub role: AclRole,
}

/// Client input keeps its strings, so a malformed entity is reported in Cloud Storage's words
/// rather than as a malformed list.

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
        let entity = parse_entity(&entry.entity)?;
        let role = validate_role(kind, &entry.role)?;
        if !entities.insert(entity.clone()) {
            return Err(AclError::DuplicateEntity(entry.entity));
        }
        entries.push(AclEntry { entity, role });
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
        // A typed entity has a valid form; only one stored before the cap can be too long.
        if entry.entity.as_str().len() > MAX_ENTITY_BYTES {
            return Err(AclError::EntityTooLong);
        }
        check_role(kind, entry.role)?;
        if !entities.insert(&entry.entity) {
            return Err(AclError::DuplicateEntity(entry.entity.as_str().to_owned()));
        }
    }
    Ok(())
}

/// The three project teams that ACL entities and IAM convenience members name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ProjectTeam {
    Owners,
    Editors,
    Viewers,
}

impl ProjectTeam {
    const ALL: [Self; 3] = [Self::Owners, Self::Editors, Self::Viewers];

    /// The entity spelling, as in `project-owners-123`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Owners => "owners",
            Self::Editors => "editors",
            Self::Viewers => "viewers",
        }
    }

    /// The IAM convenience member kind, as in `projectOwner:example-project`.
    pub(crate) fn member_kind(self) -> &'static str {
        match self {
            Self::Owners => "projectOwner",
            Self::Editors => "projectEditor",
            Self::Viewers => "projectViewer",
        }
    }

    fn from_team(team: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == team)
    }
}

/// What an entity names, found once when it was parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
    AllUsers,
    AllAuthenticatedUsers,
    UserEmail,
    UserId,
    GroupEmail,
    GroupId,
    Domain,
    ProjectTeam(ProjectTeam),
}

impl Scope {
    /// The length of the ASCII prefix before the value, such as `user-` or `project-owners-`.
    fn prefix_len(self) -> usize {
        match self {
            Self::AllUsers | Self::AllAuthenticatedUsers => 0,
            Self::UserEmail | Self::UserId => "user-".len(),
            Self::GroupEmail | Self::GroupId => "group-".len(),
            Self::Domain => "domain-".len(),
            Self::ProjectTeam(team) => "project-".len() + team.as_str().len() + 1,
        }
    }
}

/// A parsed entity's parts, borrowed from its text: the entity kinds from the pinned Discovery
/// `entity` description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntityRef<'a> {
    AllUsers,
    AllAuthenticatedUsers,
    UserEmail(&'a str),
    UserId(&'a str),
    GroupEmail(&'a str),
    GroupId(&'a str),
    Domain(&'a str),
    ProjectTeam { team: ProjectTeam, project: &'a str },
}

/// An ACL entity of a form the pinned Discovery `entity` description allows, kept in the exact
/// spelling it was given. Equality and order are the text's, because `text` is the first field
/// and the scope is a function of it: stored ACL order, duplicate detection and the IAM bridge
/// all compare spellings.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct Entity {
    text: String,
    scope: Scope,
}

crate::serde_util::serialize_as_str!(Entity);

/// The longest entity: `project-viewers-` and a project ID of up to 256 bytes. Emails take at most
/// 254 bytes and domains 253, so each kind of entity fits, and ACLs and the policies derived from
/// them stay bounded by their entry counts.
const MAX_ENTITY_BYTES: usize = 272;

impl Entity {
    pub(crate) fn all_users() -> Self {
        Self {
            text: "allUsers".into(),
            scope: Scope::AllUsers,
        }
    }

    pub(crate) fn all_authenticated_users() -> Self {
        Self {
            text: "allAuthenticatedUsers".into(),
            scope: Scope::AllAuthenticatedUsers,
        }
    }

    /// `project-TEAM-NUMBER` for `project`, spelled with the number bucket resources render.
    pub(crate) fn project_team(team: ProjectTeam, project: &str) -> Self {
        Self {
            text: format!("project-{}-{}", team.as_str(), project_number(project)),
            scope: Scope::ProjectTeam(team),
        }
    }

    /// The natural `user-ID` entity of a principal ID, when the ID forms one within the size cap.
    pub(crate) fn from_user_id(id: &str) -> Option<Self> {
        parse_entity(&format!("user-{id}")).ok()
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    pub(crate) fn into_string(self) -> String {
        self.text
    }

    pub(crate) fn parsed(&self) -> EntityRef<'_> {
        // `classify` checked this prefix, so the value always starts at a character boundary.
        let value = self.text.get(self.scope.prefix_len()..).unwrap_or_default();
        match self.scope {
            Scope::AllUsers => EntityRef::AllUsers,
            Scope::AllAuthenticatedUsers => EntityRef::AllAuthenticatedUsers,
            Scope::UserEmail => EntityRef::UserEmail(value),
            Scope::UserId => EntityRef::UserId(value),
            Scope::GroupEmail => EntityRef::GroupEmail(value),
            Scope::GroupId => EntityRef::GroupId(value),
            Scope::Domain => EntityRef::Domain(value),
            Scope::ProjectTeam(team) => EntityRef::ProjectTeam {
                team,
                project: value,
            },
        }
    }

    pub(crate) fn is_public(&self) -> bool {
        matches!(self.scope, Scope::AllUsers | Scope::AllAuthenticatedUsers)
    }

    /// The email or ID of a `user-` entity.
    pub(crate) fn user_id(&self) -> Option<&str> {
        match self.parsed() {
            EntityRef::UserEmail(user) | EntityRef::UserId(user) => Some(user),
            EntityRef::AllUsers
            | EntityRef::AllAuthenticatedUsers
            | EntityRef::GroupEmail(_)
            | EntityRef::GroupId(_)
            | EntityRef::Domain(_)
            | EntityRef::ProjectTeam { .. } => None,
        }
    }

    /// The email of a `user-` or `group-` entity: the resource's derived `email` field.
    pub(crate) fn email(&self) -> Option<&str> {
        match self.parsed() {
            EntityRef::UserEmail(email) | EntityRef::GroupEmail(email) => Some(email),
            EntityRef::AllUsers
            | EntityRef::AllAuthenticatedUsers
            | EntityRef::UserId(_)
            | EntityRef::GroupId(_)
            | EntityRef::Domain(_)
            | EntityRef::ProjectTeam { .. } => None,
        }
    }

    /// The ID of a `user-` or `group-` entity: the resource's derived `entityId` field.
    pub(crate) fn entity_id(&self) -> Option<&str> {
        match self.parsed() {
            EntityRef::UserId(id) | EntityRef::GroupId(id) => Some(id),
            EntityRef::AllUsers
            | EntityRef::AllAuthenticatedUsers
            | EntityRef::UserEmail(_)
            | EntityRef::GroupEmail(_)
            | EntityRef::Domain(_)
            | EntityRef::ProjectTeam { .. } => None,
        }
    }

    /// The domain of a `domain-` entity: the resource's derived `domain` field.
    pub(crate) fn domain(&self) -> Option<&str> {
        match self.parsed() {
            EntityRef::Domain(domain) => Some(domain),
            EntityRef::AllUsers
            | EntityRef::AllAuthenticatedUsers
            | EntityRef::UserEmail(_)
            | EntityRef::UserId(_)
            | EntityRef::GroupEmail(_)
            | EntityRef::GroupId(_)
            | EntityRef::ProjectTeam { .. } => None,
        }
    }

    /// The team and project of a `project-` entity: the resource's derived `projectTeam` field.
    pub(crate) fn team(&self) -> Option<(ProjectTeam, &str)> {
        match self.parsed() {
            EntityRef::ProjectTeam { team, project } => Some((team, project)),
            EntityRef::AllUsers
            | EntityRef::AllAuthenticatedUsers
            | EntityRef::UserEmail(_)
            | EntityRef::UserId(_)
            | EntityRef::GroupEmail(_)
            | EntityRef::GroupId(_)
            | EntityRef::Domain(_) => None,
        }
    }

    pub(crate) fn matches(
        &self,
        identity: Option<&crate::auth::Identity>,
        public: crate::auth::PublicAccess,
    ) -> bool {
        match self.parsed() {
            EntityRef::AllUsers => public == crate::auth::PublicAccess::Allowed,
            EntityRef::AllAuthenticatedUsers => {
                public == crate::auth::PublicAccess::Allowed && identity.is_some()
            }
            EntityRef::UserEmail(user) | EntityRef::UserId(user) => {
                identity.is_some_and(|id| id.matches_acl_user(user))
            }
            EntityRef::GroupEmail(_)
            | EntityRef::GroupId(_)
            | EntityRef::Domain(_)
            | EntityRef::ProjectTeam { .. } => false,
        }
    }
}

/// The entity's form only, without the size cap: a stored entity parses here, so one a release
/// before 0.12.0 stored past [`MAX_ENTITY_BYTES`] stays readable, while [`validate_entries`]
/// refuses to write it back.
impl FromStr for Entity {
    type Err = AclError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let scope = classify(text)?;
        Ok(Self {
            text: text.to_owned(),
            scope,
        })
    }
}

impl TryFrom<String> for Entity {
    type Error = AclError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let scope = classify(&text)?;
        Ok(Self { text, scope })
    }
}

impl fmt::Display for Entity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Debug shows the spelling, as the `String` it replaced did, so test failures and logs stay
/// readable.
impl fmt::Debug for Entity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.text, f)
    }
}

/// A client-supplied or derived entity: refuses one past [`MAX_ENTITY_BYTES`] before naming it,
/// then parses its form.
pub(crate) fn parse_entity(text: &str) -> Result<Entity, AclError> {
    if text.len() > MAX_ENTITY_BYTES {
        return Err(AclError::EntityTooLong);
    }
    text.parse()
}

/// The scope of `entity`. `user`, `group`, `domain` and `project` contain no `-`, so the first
/// segment is the prefix.
fn classify(entity: &str) -> Result<Scope, AclError> {
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
    let id = |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric());
    match entity {
        "allUsers" => return Ok(Scope::AllUsers),
        "allAuthenticatedUsers" => return Ok(Scope::AllAuthenticatedUsers),
        _ => {}
    }
    let (prefix, value) = entity.split_once('-').ok_or_else(invalid)?;
    match prefix {
        "user" if email(value) => Ok(Scope::UserEmail),
        "user" if id(value) => Ok(Scope::UserId),
        "group" if email(value) => Ok(Scope::GroupEmail),
        "group" if id(value) => Ok(Scope::GroupId),
        "domain" if valid_domain(value) => Ok(Scope::Domain),
        "project" => {
            let (team, project) = value.split_once('-').ok_or_else(invalid)?;
            let team = ProjectTeam::from_team(team).ok_or_else(invalid)?;
            if project.is_empty()
                || !project
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return Err(invalid());
            }
            Ok(Scope::ProjectTeam(team))
        }
        _ => Err(invalid()),
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
    [
        (ProjectTeam::Owners, AclRole::Owner),
        (ProjectTeam::Editors, AclRole::Owner),
        (ProjectTeam::Viewers, AclRole::Reader),
    ]
    .into_iter()
    .map(|(team, role)| AclEntry {
        entity: Entity::project_team(team, project),
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
        let entry = |entity, role| AclEntry { entity, role };
        let owners = || Entity::project_team(ProjectTeam::Owners, project);
        match self {
            Self::AuthenticatedRead => {
                vec![entry(Entity::all_authenticated_users(), AclRole::Reader)]
            }
            Self::BucketOwnerFullControl => vec![entry(owners(), AclRole::Owner)],
            Self::BucketOwnerRead => vec![entry(owners(), AclRole::Reader)],
            Self::Private => Vec::new(),
            Self::ProjectPrivate => project_private(project),
            Self::PublicRead => vec![entry(Entity::all_users(), AclRole::Reader)],
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
        let entry = |entity, role| AclEntry { entity, role };
        let owners = entry(
            Entity::project_team(ProjectTeam::Owners, project),
            AclRole::Owner,
        );
        match self {
            Self::ProjectPrivate => project_private(project),
            Self::Private => vec![owners],
            Self::AuthenticatedRead => {
                vec![
                    owners,
                    entry(Entity::all_authenticated_users(), AclRole::Reader),
                ]
            }
            Self::PublicRead => vec![owners, entry(Entity::all_users(), AclRole::Reader)],
            Self::PublicReadWrite => vec![owners, entry(Entity::all_users(), AclRole::Writer)],
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
                entity: "project-owners-0".parse().unwrap(),
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

    fn sample_entities() -> [(&'static str, EntityRef<'static>); 10] {
        [
            ("allUsers", EntityRef::AllUsers),
            ("allAuthenticatedUsers", EntityRef::AllAuthenticatedUsers),
            (
                "user-liz@example.com",
                EntityRef::UserEmail("liz@example.com"),
            ),
            ("user-00b4903a97", EntityRef::UserId("00b4903a97")),
            (
                "group-team@example.com",
                EntityRef::GroupEmail("team@example.com"),
            ),
            ("group-00b4903a97", EntityRef::GroupId("00b4903a97")),
            ("domain-example.com", EntityRef::Domain("example.com")),
            (
                "project-owners-123",
                EntityRef::ProjectTeam {
                    team: ProjectTeam::Owners,
                    project: "123",
                },
            ),
            (
                "project-editors-123",
                EntityRef::ProjectTeam {
                    team: ProjectTeam::Editors,
                    project: "123",
                },
            ),
            (
                "project-viewers-local-project",
                EntityRef::ProjectTeam {
                    team: ProjectTeam::Viewers,
                    project: "local-project",
                },
            ),
        ]
    }

    #[test]
    fn entities_follow_the_discovery_forms() {
        for (entity, parsed) in sample_entities() {
            let parsed_entity = parse_entity(entity).unwrap();
            assert_eq!(parsed_entity.parsed(), parsed, "{entity}");
            assert_eq!(parsed_entity.as_str(), entity);
            assert_eq!(entity.parse::<Entity>().as_ref(), Ok(&parsed_entity));
        }
        // Each kind's longest value fits, and only the length refuses what follows it.
        let email = format!("{}@example.com", "x".repeat(254 - "@example.com".len()));
        for entity in [
            format!("user-{email}"),
            format!("group-{email}"),
            format!("project-viewers-{}", "p".repeat(256)),
        ] {
            assert!(parse_entity(&entity).is_ok(), "{entity}");
        }
        let longest = format!("project-viewers-{}", "p".repeat(256));
        assert_eq!(longest.len(), MAX_ENTITY_BYTES);
        assert_eq!(
            parse_entity(&format!("{longest}p")),
            Err(AclError::EntityTooLong)
        );
        assert_eq!(
            parse_entity(&format!("user-{}@example.com", "x".repeat(16 * 1024))),
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
            assert!(parse_entity(entity).is_err(), "{entity}");
            assert!(entity.parse::<Entity>().is_err(), "{entity}");
        }
        let public = |entity: &str| parse_entity(entity).unwrap().is_public();
        assert!(public("allUsers") && public("allAuthenticatedUsers"));
        assert!(!public("domain-example.com"));
    }

    /// Stored ACL order and the IAM bridge sort entities by spelling, so their order must be
    /// the text's, whatever their kinds.
    #[test]
    fn entities_order_by_their_text() {
        let texts: Vec<_> = sample_entities()
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        let mut by_text = texts.clone();
        by_text.sort_unstable();
        let mut entities: Vec<Entity> = texts.iter().map(|text| text.parse().unwrap()).collect();
        entities.sort();
        let sorted: Vec<_> = entities.iter().map(Entity::as_str).collect();
        assert_eq!(sorted, by_text);
    }

    /// The constructors spell what parsing reads, so a built entity equals a parsed one.
    #[test]
    fn built_entities_match_parsed_ones() {
        for (team, number) in [
            (ProjectTeam::Owners, "123"),
            (ProjectTeam::Editors, "123"),
            (ProjectTeam::Viewers, "0"),
        ] {
            let project = if number == "0" {
                "local-project"
            } else {
                number
            };
            let built = Entity::project_team(team, project);
            let text = format!("project-{}-{number}", team.as_str());
            assert_eq!(built, text.parse().unwrap());
            assert_eq!(
                built.parsed(),
                EntityRef::ProjectTeam {
                    team,
                    project: number
                }
            );
        }
        assert_eq!(
            Entity::from_user_id("liz@example.com").map(Entity::into_string),
            Some("user-liz@example.com".into())
        );
        assert_eq!(Entity::from_user_id("local:admin"), None);
    }

    /// A release before 0.12.0 stored entities without the size cap; such an entity still reads
    /// back, while a request or a write refuses it.
    #[test]
    fn oversized_entities_parse_only_without_the_size_cap() {
        let oversized = format!("project-viewers-{}", "p".repeat(257));
        assert_eq!(oversized.len(), MAX_ENTITY_BYTES + 1);
        let stored: Entity = oversized.parse().unwrap();
        assert_eq!(stored.as_str(), oversized);
        assert_eq!(
            serde_json::from_value::<Entity>(serde_json::Value::String(oversized.clone())).unwrap(),
            stored
        );
        assert_eq!(parse_entity(&oversized), Err(AclError::EntityTooLong));
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
                    entity: "project-owners-123".parse().unwrap(),
                    role: crate::acl::AclRole::Owner
                },
                AclEntry {
                    entity: "project-editors-123".parse().unwrap(),
                    role: crate::acl::AclRole::Owner
                },
                AclEntry {
                    entity: "project-viewers-123".parse().unwrap(),
                    role: crate::acl::AclRole::Reader
                },
            ]
        );
        assert_eq!(
            project_private("local")[0].entity.as_str(),
            "project-owners-0"
        );
        assert_eq!(project_number("99999999999999999999999"), "0");
    }
}
