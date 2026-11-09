//! Stored bucket, default object and object ACLs.
//!
//! Missing legacy entries retain the owning project's `projectPrivate` default. New
//! publications snapshot the current default and keep immutable ownership separately;
//! its OWNER grant is a view, never an independently persisted canonical alias grant.
//! Later default changes never rewrite existing generations.
//! Foreign keys remove ACL rows together with their bucket or object generation.

use super::{
    Catalog, change_journal,
    objects::{object_unavailable, selected},
    policies::{check_access, public_access_prevention},
    unavailable, unix_micros,
};
use crate::storage::catalog::{bucket_incarnation, sql};
use crate::{
    acl::{AclEntry, AclKind, AclRole, AclTarget, Entity, MAX_ENTRIES, project_private},
    bucket::{BucketAccess, BucketConditions, CatalogError},
    object::{ObjectConditions, ObjectError},
};
use rusqlite::{Connection, OptionalExtension, params};

mod bucket_policy;
#[cfg(test)]
mod snapshot_tests;
pub(super) use bucket_policy::reconcile as reconcile_bucket_policy;
pub(super) use bucket_policy::replace_from_policy;

pub(super) const SCHEMA: &str = "CREATE TABLE acl_bucket_entries (
    bucket TEXT NOT NULL COLLATE BINARY REFERENCES buckets(name) ON DELETE CASCADE,
    scope TEXT NOT NULL CHECK(scope IN ('bucket', 'defaultObject')),
    entries TEXT NOT NULL CHECK(json_valid(entries) AND json_type(entries) = 'array'),
    PRIMARY KEY(bucket, scope)
) WITHOUT ROWID;
CREATE TABLE acl_object_entries (
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    generation INTEGER NOT NULL,
    entries TEXT NOT NULL CHECK(json_valid(entries) AND json_type(entries) = 'array'),
    PRIMARY KEY(bucket, name, generation),
    FOREIGN KEY(bucket, name, generation) REFERENCES objects(bucket, name, generation) ON DELETE CASCADE
) WITHOUT ROWID;
PRAGMA user_version = 33;";

pub(super) const POLICY_BINDING_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN acl_policy_bound INTEGER NOT NULL DEFAULT 0 CHECK(acl_policy_bound IN (0,1)); PRAGMA user_version=43;";

// Shared by metadata replacement and entry changes so both hit one cached statement.
const UPSERT_BUCKET_ENTRIES: &str =
    "INSERT INTO acl_bucket_entries (bucket, scope, entries) VALUES (?1, ?2, ?3)
     ON CONFLICT(bucket, scope) DO UPDATE SET entries = excluded.entries";
const UPSERT_OBJECT_ENTRIES: &str =
    "INSERT INTO acl_object_entries (bucket, name, generation, entries) VALUES (?1, ?2, ?3, ?4)
     ON CONFLICT(bucket, name, generation) DO UPDATE SET entries = excluded.entries";

/// An owner without a canonical alias still consumes one logical ACL grant.
fn validate_effective_entries(
    kind: AclKind,
    entries: &[AclEntry],
    owner: Option<&crate::object::owner::ObjectOwner>,
) -> Result<(), CatalogError> {
    crate::acl::validate_entries(kind, entries)?;
    if entries.len() >= MAX_ENTRIES && owner.is_some_and(|owner| owner.entity().is_none()) {
        return Err(CatalogError::from(crate::acl::AclError::TooManyEntries));
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) enum PublicationAcl<'a> {
    InheritDefault,
    Preserve(&'a [AclEntry]),
    /// A request's `predefinedAcl`, checked against uniform access and public access
    /// prevention in the publishing transaction so a concurrent bucket change cannot slip past.
    Predefined(crate::acl::PredefinedObjectAcl),
}

impl PublicationAcl<'_> {
    pub(super) fn requested(predefined: Option<crate::acl::PredefinedObjectAcl>) -> Self {
        predefined.map_or(Self::InheritDefault, Self::Predefined)
    }
}

pub(super) fn effective_object_entries(
    connection: &Connection,
    record: &crate::object::ObjectRecord,
) -> Result<Vec<AclEntry>, CatalogError> {
    let row = bucket_row(connection, &record.bucket)?.ok_or(CatalogError::NotFound)?;
    let mut entries =
        stored_object_entries(connection, &record.bucket, &record.name, record.generation)?
            .unwrap_or_else(|| project_private(&row.project));
    if let Some(owner) = &record.owner {
        owner.preserve_grant(&mut entries);
    }
    validate_effective_entries(AclKind::Object, &entries, record.owner.as_ref())?;
    Ok(entries)
}

/// A preserved source ACL never observes the destination's current default ACL.
pub(super) fn publish_object_acl(
    connection: &Connection,
    record: &crate::object::ObjectRecord,
    mode: PublicationAcl<'_>,
) -> Result<(), ObjectError> {
    let bucket = &record.bucket;
    let row = bucket_row(connection, bucket)?.ok_or(ObjectError::BucketNotFound)?;
    let mut entries = match mode {
        PublicationAcl::InheritDefault => {
            let stored = stored_bucket_entries(connection, bucket, AclKind::DefaultObject)?;
            if stored.is_none() && record.owner.is_none() {
                return Ok(());
            }
            stored.unwrap_or_else(|| project_private(&row.project))
        }
        PublicationAcl::Preserve(entries) => entries.to_vec(),
        PublicationAcl::Predefined(predefined) => {
            if row.uniform_access {
                return Err(ObjectError::UniformAccessPredefinedAcl);
            }
            let entries = predefined.entries(&row.project);
            if entries
                .iter()
                .any(|entry| Entity::parse(&entry.entity).is_ok_and(Entity::is_public))
                && public_access_prevention(connection, bucket)?
            {
                return Err(ObjectError::PublicAccessPrevention);
            }
            entries
        }
    };
    if let Some(owner) = &record.owner {
        owner.preserve_grant(&mut entries);
    }
    validate_effective_entries(AclKind::Object, &entries, record.owner.as_ref())
        .map_err(super::objects::acl_update_error)?;
    if let Some(owner) = &record.owner {
        // A read re-adds an implicit grant at its usual place, so a predefined ACL stores the
        // grant to keep it first.
        match mode {
            PublicationAcl::Predefined(_) => owner.move_grant_first(&mut entries),
            PublicationAcl::InheritDefault | PublicationAcl::Preserve(_) => {
                owner.remove_implicit_grant(&mut entries);
            }
        }
    }
    let entries = serde_json::to_string(&entries).map_err(object_unavailable)?;
    sql::exec(
        connection,
        UPSERT_OBJECT_ENTRIES,
        params![bucket, record.name, record.generation, entries],
    )?;
    Ok(())
}

/// The effective entries plus the containing resource version used for ACL etags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AclState {
    pub entries: Vec<AclEntry>,
    pub metageneration: i64,
    /// Selected object name and generation for object ACLs.
    pub object: Option<(String, i64)>,
    /// The number of the owning project, as ACL entity names spell it.
    pub project_number: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AclChange {
    /// Insert, update and patch upsert one entity; `None` keeps an existing patch role.
    Upsert {
        entity: String,
        role: Option<AclRole>,
    },
    Delete {
        entity: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AclError {
    Bucket(CatalogError),
    Object(ObjectError),
    UniformAccess,
    /// The entity is not in the ACL. `project_number` is the owning project's, which decides
    /// whether a project-team entity names another project.
    EntityNotFound {
        entity: String,
        project_number: String,
    },
    RoleRequired,
    TooManyEntries,
    NotModified,
}

impl From<CatalogError> for AclError {
    fn from(error: CatalogError) -> Self {
        Self::Bucket(error)
    }
}

impl From<ObjectError> for AclError {
    fn from(error: ObjectError) -> Self {
        Self::Object(error)
    }
}

/// Stored ACLs are re-validated before they are written; a rejected entry list is invalid
/// metadata whose message names the entry, and the validation error has no cause to keep.
impl From<crate::acl::AclError> for CatalogError {
    fn from(error: crate::acl::AclError) -> Self {
        Self::InvalidMetadata(error.to_string())
    }
}

/// ACLs rendered by `projection=full`; `None` lists mean uniform bucket-level access hides them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AclProjection {
    pub uniform_access: bool,
    pub project: String,
    pub bucket: Vec<AclEntry>,
    pub default_object: Vec<AclEntry>,
    pub bucket_metageneration: i64,
    pub objects: Vec<Vec<AclEntry>>,
    pub owners: Vec<Option<crate::object::owner::ObjectOwner>>,
    pub visible: Vec<bool>,
}

#[derive(Clone)]
pub(crate) struct ObjectAclSnapshot {
    pub(super) bucket: String,
    pub(super) name: String,
    pub(super) generation: i64,
    pub entries: Vec<AclEntry>,
    pub owner: Option<crate::object::owner::ObjectOwner>,
    pub(super) incarnation: String,
}

impl ObjectAclSnapshot {
    pub(crate) fn matches_resource(
        &self,
        bucket: &str,
        name: Option<&str>,
        generation: Option<i64>,
    ) -> bool {
        self.bucket == bucket
            && name == Some(self.name.as_str())
            && generation == Some(self.generation)
    }

    pub(super) fn new(
        record: &crate::object::ObjectRecord,
        mut entries: Vec<AclEntry>,
        incarnation: &str,
    ) -> Self {
        if let Some(owner) = &record.owner {
            owner.remove_implicit_grant(&mut entries);
        }
        Self {
            bucket: record.bucket.clone(),
            name: record.name.clone(),
            generation: record.generation,
            entries,
            owner: record.owner.clone(),
            incarnation: incarnation.to_owned(),
        }
    }
}

struct BucketRow {
    project: String,
    metageneration: i64,
    uniform_access: bool,
    policy_bound: bool,
}

fn bucket_row(connection: &Connection, bucket: &str) -> Result<Option<BucketRow>, CatalogError> {
    const SELECT: &str = "SELECT project, metageneration, uniform_bucket_level_access_locked_time IS NOT NULL, acl_policy_bound FROM buckets WHERE name = ?1";
    sql::row(connection, SELECT, [bucket], |row| {
        Ok(BucketRow {
            project: row.get(0)?,
            metageneration: row.get(1)?,
            uniform_access: row.get(2)?,
            policy_bound: row.get(3)?,
        })
    })
    .optional()
    .map_err(unavailable)
}

pub(super) fn decode(entries: &str, kind: AclKind) -> Result<Vec<AclEntry>, CatalogError> {
    let invalid = |error| {
        CatalogError::Corrupt(crate::storage::failure::BackendCause::with_source(
            "invalid stored ACL",
            error,
        ))
    };
    let entries: Vec<AclEntry> = serde_json::from_str(entries).map_err(invalid)?;
    for entry in &entries {
        crate::acl::validate_role(kind, entry.role.as_str()).map_err(|error| {
            CatalogError::Corrupt(crate::storage::failure::BackendCause::with_source(
                "invalid stored ACL",
                error,
            ))
        })?;
    }
    Ok(entries)
}

fn stored_bucket_entries(
    connection: &Connection,
    bucket: &str,
    kind: AclKind,
) -> Result<Option<Vec<AclEntry>>, CatalogError> {
    sql::row(
        connection,
        "SELECT entries FROM acl_bucket_entries WHERE bucket = ?1 AND scope = ?2",
        params![bucket, scope(kind)],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|entries| decode(&entries, kind))
    .transpose()
}

fn bucket_projection(
    connection: &Connection,
    bucket: &str,
    row: &BucketRow,
) -> Result<AclProjection, CatalogError> {
    let default = || project_private(&row.project);
    Ok(AclProjection {
        uniform_access: row.uniform_access,
        bucket: stored_bucket_entries(connection, bucket, AclKind::Bucket)?.unwrap_or_else(default),
        default_object: stored_bucket_entries(connection, bucket, AclKind::DefaultObject)?
            .unwrap_or_else(default),
        bucket_metageneration: row.metageneration,
        objects: Vec::new(),
        owners: Vec::new(),
        visible: Vec::new(),
        project: row.project.clone(),
    })
}

pub(super) fn bucket_granted(
    connection: &Connection,
    bucket: &str,
    project: &str,
    identity: Option<&crate::auth::Identity>,
    permission: crate::auth::Permission,
) -> Result<bool, CatalogError> {
    let entries = stored_bucket_entries(connection, bucket, AclKind::Bucket)?
        .unwrap_or_else(|| project_private(project));
    // Bucket ACL roles are the same authority as legacy bucket IAM, including under UBLA.
    entries_grant(
        &entries,
        identity,
        |role| {
            crate::auth::roles::permissions(crate::acl::bucket_iam_role(role))
                .is_some_and(|permissions| permissions.binary_search(&permission.as_str()).is_ok())
        },
        || super::policies::policy_public_access(connection, bucket),
    )
}

fn entries_grant(
    entries: &[AclEntry],
    identity: Option<&crate::auth::Identity>,
    allows: impl Fn(AclRole) -> bool,
    mut public: impl FnMut() -> Result<crate::auth::PublicAccess, CatalogError>,
) -> Result<bool, CatalogError> {
    let mut cached = None;
    for entry in entries.iter().filter(|entry| allows(entry.role)) {
        let Ok(entity) = Entity::parse(&entry.entity) else {
            continue;
        };
        let access = if entity.is_public() {
            match cached {
                Some(access) => access,
                None => {
                    let access = public()?;
                    cached = Some(access);
                    access
                }
            }
        } else {
            crate::auth::PublicAccess::Allowed
        };
        if entity.matches(identity, access) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn object_entries_grant(
    owner: Option<&crate::object::owner::ObjectOwner>,
    entries: &[AclEntry],
    identity: Option<&crate::auth::Identity>,
    permission: crate::auth::Permission,
    public: impl FnMut() -> Result<crate::auth::PublicAccess, CatalogError>,
) -> Result<bool, CatalogError> {
    if owner.is_some_and(|owner| owner.is_principal(identity)) {
        return Ok(true);
    }
    entries_grant(
        entries,
        identity,
        |role| crate::acl::object_role_allows(role, permission),
        public,
    )
}

pub(super) fn object_granted(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
    access: &BucketAccess,
) -> Result<bool, CatalogError> {
    let BucketAccess::Policy {
        identity,
        permission,
        project,
    } = access
    else {
        return Ok(false);
    };
    if !crate::acl::object_permission(*permission) {
        return Ok(false);
    }
    let row = bucket_row(connection, bucket)?.ok_or(CatalogError::NotFound)?;
    if row.uniform_access
        || project
            .as_ref()
            .is_some_and(|project| project != &row.project)
    {
        return Ok(false);
    }
    let stored = sql::row(
        connection,
        "SELECT o.owner, e.entries FROM objects o LEFT JOIN acl_object_entries e ON e.bucket=o.bucket AND e.name=o.name AND e.generation=o.generation WHERE o.bucket=?1 AND o.name=?2 AND o.generation=?3",
        params![bucket, name, generation],
        |row| Ok((super::object_owner::read(row, 0)?, row.get::<_, Option<String>>(1)?)),
    ).optional()?;
    let Some((owner, stored)) = stored else {
        return Ok(false);
    };
    if owner
        .as_ref()
        .is_some_and(|owner| owner.is_principal(identity.as_ref()))
    {
        return Ok(true);
    }
    let entries = stored
        .map(|entries| decode(&entries, AclKind::Object))
        .transpose()?
        .unwrap_or_else(|| project_private(&row.project));
    object_entries_grant(
        owner.as_ref(),
        &entries,
        identity.as_ref(),
        *permission,
        || super::policies::policy_public_access(connection, bucket),
    )
}

fn stored_object_entries(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
) -> Result<Option<Vec<AclEntry>>, CatalogError> {
    const SELECT: &str = "SELECT entries FROM acl_object_entries WHERE bucket = ?1 AND name = ?2 AND generation = ?3";
    sql::optional_scalar::<String>(connection, SELECT, params![bucket, name, generation])?
        .map(|entries| decode(&entries, AclKind::Object))
        .transpose()
}

fn scope(kind: AclKind) -> &'static str {
    if kind == AclKind::Bucket {
        "bucket"
    } else {
        "defaultObject"
    }
}

/// The resource a metadata mutation replaces ACLs on. Unlike [`AclTarget`], an object target
/// carries the generation the mutation already selected, so no live-generation lookup remains.
#[derive(Clone, Copy, Debug)]
pub(super) enum ResolvedAclTarget<'a> {
    Bucket(&'a str),
    DefaultObject(&'a str),
    Object {
        bucket: &'a str,
        name: &'a str,
        generation: i64,
    },
}

impl<'a> ResolvedAclTarget<'a> {
    fn kind(self) -> AclKind {
        match self {
            Self::Bucket(_) => AclKind::Bucket,
            Self::DefaultObject(_) => AclKind::DefaultObject,
            Self::Object { .. } => AclKind::Object,
        }
    }

    fn bucket(self) -> &'a str {
        match self {
            Self::Bucket(bucket) | Self::DefaultObject(bucket) | Self::Object { bucket, .. } => {
                bucket
            }
        }
    }
}

fn target_access(
    connection: &Connection,
    target: ResolvedAclTarget<'_>,
    project: &str,
    access: &BucketAccess,
) -> Result<(), CatalogError> {
    match target {
        ResolvedAclTarget::Object {
            bucket,
            name,
            generation,
        } => super::objects::authorize_generation(connection, bucket, name, generation, access)
            .map_err(|error| match error {
                ObjectError::Denied(denied) => CatalogError::Denied(denied),
                ObjectError::Forbidden => CatalogError::Forbidden,
                ObjectError::BucketNotFound => CatalogError::NotFound,
                error => unavailable(error),
            }),
        other => check_access(connection, other.bucket(), project, access).map(|_| ()),
    }
}

/// Replace entries inside the metadata mutation's transaction, without a second revision bump.
pub(super) fn replace_resource_acl(
    connection: &Connection,
    target: ResolvedAclTarget<'_>,
    entries: &[AclEntry],
    access: &BucketAccess,
    principals: Option<&super::principals::Profile>,
) -> Result<(), CatalogError> {
    let bucket = target.bucket();
    let row = bucket_row(connection, bucket)?.ok_or(CatalogError::NotFound)?;
    if let BucketAccess::Policy {
        identity, project, ..
    } = access
    {
        let mut policy_access = BucketAccess::Policy {
            identity: identity.clone(),
            project: project.clone(),
            permission: if target.kind() == AclKind::Object {
                crate::auth::Permission::ObjectsSetIam
            } else {
                crate::auth::Permission::BucketsSetIam
            },
        };
        match target_access(connection, target, &row.project, &policy_access) {
            Ok(_) => {}
            // A caller who may only read the ACL can still resend it unchanged; any change
            // stays refused for the permission that writing it needs.
            Err(CatalogError::Denied(denied)) => {
                if let BucketAccess::Policy { permission, .. } = &mut policy_access {
                    *permission = if target.kind() == AclKind::Object {
                        crate::auth::Permission::ObjectsGetIam
                    } else {
                        crate::auth::Permission::BucketsGetIam
                    };
                }
                match target_access(connection, target, &row.project, &policy_access) {
                    Err(CatalogError::Denied(_)) => return Err(CatalogError::Denied(denied)),
                    other => other?,
                }
                // Preserve full-resource echoes only for callers already allowed to read the ACL.
                if !row.uniform_access {
                    let mut current = match target {
                        ResolvedAclTarget::Object {
                            name, generation, ..
                        } => stored_object_entries(connection, bucket, name, generation)?,
                        _ => stored_bucket_entries(connection, bucket, target.kind())?,
                    }
                    .unwrap_or_else(|| project_private(&row.project));
                    if let ResolvedAclTarget::Object {
                        name, generation, ..
                    } = target
                        && let Some(owner) =
                            super::object_owner::selected(connection, bucket, name, generation)?
                    {
                        owner.preserve_grant(&mut current);
                    }
                    let mut requested = entries.to_vec();
                    current.sort_by(|a, b| a.entity.cmp(&b.entity));
                    requested.sort_by(|a, b| a.entity.cmp(&b.entity));
                    if current == requested {
                        return Ok(());
                    }
                }
                return Err(CatalogError::Denied(denied));
            }
            Err(error) => return Err(error),
        }
    }
    if row.uniform_access {
        return Err(CatalogError::InvalidMetadata(
            "Cannot use legacy ACLs while uniform bucket-level access is enabled; omit acl/defaultObjectAcl or disable uniform access".into(),
        ));
    }
    let mut entries = entries.to_vec();
    let owner = match target {
        ResolvedAclTarget::Object {
            name, generation, ..
        } => super::object_owner::selected(connection, bucket, name, generation)?,
        _ => None,
    };
    if let Some(owner) = &owner {
        owner.preserve_grant(&mut entries);
    }
    let entries = entries.as_slice();
    validate_effective_entries(target.kind(), entries, owner.as_ref())?;
    if entries
        .iter()
        .any(|entry| Entity::parse(&entry.entity).is_ok_and(Entity::is_public))
        && public_access_prevention(connection, bucket)?
    {
        return Err(CatalogError::PublicAccessPrevention);
    }
    if target.kind() == AclKind::Bucket {
        bucket_policy::replace_policy(connection, bucket, entries, principals)?;
    }
    let mut stored = entries.to_vec();
    if let Some(owner) = &owner {
        owner.remove_implicit_grant(&mut stored);
    }
    let entries = serde_json::to_string(&stored).map_err(unavailable)?;
    match target {
        ResolvedAclTarget::Object {
            name, generation, ..
        } => {
            sql::exec(
                connection,
                UPSERT_OBJECT_ENTRIES,
                params![bucket, name, generation, entries],
            )?;
        }
        _ => {
            sql::exec(
                connection,
                UPSERT_BUCKET_ENTRIES,
                params![bucket, scope(target.kind()), entries],
            )?;
        }
    }
    Ok(())
}

/// Authorize, reject uniform access and select the target inside the caller's transaction.
fn load(
    connection: &Connection,
    target: &AclTarget,
    access: &BucketAccess,
) -> Result<(AclState, Option<crate::object::ObjectRecord>), AclError> {
    let bucket = target.bucket();
    let row = bucket_row(connection, bucket)?.ok_or(CatalogError::NotFound)?;
    Ok(match target {
        AclTarget::Bucket(_) | AclTarget::DefaultObject(_) => {
            check_access(connection, bucket, &row.project, access)?;
            if row.uniform_access {
                return Err(AclError::UniformAccess);
            }
            let entries = stored_bucket_entries(connection, bucket, target.kind())?
                .unwrap_or_else(|| project_private(&row.project));
            let state = AclState {
                entries,
                metageneration: row.metageneration,
                object: None,
                project_number: crate::acl::project_number(&row.project),
            };
            (state, None)
        }
        AclTarget::Object {
            name, generation, ..
        } => {
            let super::objects::StoredObject { record, .. } = selected(
                connection,
                bucket,
                name,
                *generation,
                ObjectConditions::default(),
                super::objects::RequestKind::Read,
                access,
            )?;
            if row.uniform_access {
                return Err(AclError::UniformAccess);
            }
            let mut entries = stored_object_entries(connection, bucket, name, record.generation)?
                .unwrap_or_else(|| project_private(&row.project));
            if let Some(owner) = &record.owner {
                owner.preserve_grant(&mut entries);
            }
            let state = AclState {
                entries,
                metageneration: record.metageneration,
                object: Some((record.name.clone(), record.generation)),
                project_number: crate::acl::project_number(&row.project),
            };
            (state, Some(record))
        }
    })
}

fn encode_changed_entries(
    state: &mut AclState,
    owner: Option<&crate::object::owner::ObjectOwner>,
    changed: &mut Option<AclEntry>,
    kind: AclKind,
) -> Result<String, CatalogError> {
    if let Some(owner) = owner {
        owner.preserve_grant(&mut state.entries);
        if let Some(entry) = changed {
            *entry = state
                .entries
                .iter()
                .find(|current| current.entity == entry.entity)
                .cloned()
                .ok_or_else(|| unavailable("changed ACL entry is absent"))?;
        }
    }
    validate_effective_entries(kind, &state.entries, owner)?;
    // Persist the owner position so later inserts follow every existing grant.
    serde_json::to_string(&state.entries).map_err(unavailable)
}

impl Catalog {
    pub(crate) async fn object_acl_snapshot(
        &self,
        mut snapshot: ObjectAclSnapshot,
        access: &BucketAccess,
    ) -> Result<Option<ObjectAclSnapshot>, CatalogError> {
        let access = access.clone();
        let principals = self.0.principals.get().cloned();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            let Some(row) = bucket_row(&transaction, &snapshot.bucket)? else {
                return Ok(None);
            };
            if row.uniform_access {
                return Ok(None);
            }
            if let BucketAccess::Policy {
                permission,
                project,
                ..
            } = &access
                && (*permission != crate::auth::Permission::ObjectsGetIam
                    || project
                        .as_ref()
                        .is_some_and(|project| project != &row.project))
            {
                return Ok(None);
            }
            let incarnation: String = bucket_incarnation(&transaction, &snapshot.bucket)?;
            if snapshot.incarnation != incarnation {
                return Ok(None);
            }
            let target = ResolvedAclTarget::Object {
                bucket: &snapshot.bucket,
                name: &snapshot.name,
                generation: snapshot.generation,
            };
            match target_access(&transaction, target, &row.project, &access) {
                Ok(()) => {}
                Err(error) if error.is_refusal() => {
                    let identity = match &access {
                        BucketAccess::Policy { identity, .. } => identity.as_ref(),
                        _ => None,
                    };
                    if !snapshot
                        .owner
                        .as_ref()
                        .is_some_and(|owner| owner.is_principal(identity))
                    {
                        return Ok(None);
                    }
                }
                Err(error) => return Err(error),
            }
            if let Some(owner) = &mut snapshot.owner {
                super::object_owner::canonicalize(owner, principals.as_ref());
                owner.preserve_grant(&mut snapshot.entries);
            }
            Ok(Some(snapshot))
        })
        .await
    }

    /// Read effective entries, checking conditions against the target's metageneration.
    /// Bucket and default object ACL targets use the bucket's metageneration; object ACL
    /// targets use the selected object's metageneration, so pass default conditions for them.
    /// JSON defines bucket metageneration conditions only for default object ACL lists.
    pub(crate) async fn read_acl(
        &self,
        target: AclTarget,
        conditions: BucketConditions,
        access: &BucketAccess,
    ) -> Result<AclState, AclError> {
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            let (state, _) = load(&transaction, &target, &access)?;
            if conditions
                .if_metageneration_match
                .is_some_and(|value| value != state.metageneration)
            {
                return Err(CatalogError::PreconditionFailed.into());
            }
            if conditions.if_metageneration_not_match == Some(state.metageneration) {
                return Err(AclError::NotModified);
            }
            Ok(state)
        })
        .await
    }

    /// Apply one entry change and advance the containing resource metageneration atomically.
    pub(crate) async fn change_acl(
        &self,
        target: AclTarget,
        change: AclChange,
        access: &BucketAccess,
    ) -> Result<(AclState, Option<AclEntry>), AclError> {
        let access = access.clone();
        let clock = self.0.clock.clone();
        let principals = self.0.principals.get().cloned();
        self.run(move |connection| {
            let transaction = sql::immediate(connection).map_err(unavailable)?;
            let (mut state, record) = load(&transaction, &target, &access)?;
            let bucket = target.bucket();
            let mut changed = match change {
                AclChange::Upsert { entity, role } => {
                    // Public access prevention blocks new public grants, as it does for IAM.
                    if Entity::parse(&entity).is_ok_and(Entity::is_public)
                        && public_access_prevention(&transaction, bucket)?
                    {
                        return Err(CatalogError::PublicAccessPrevention.into());
                    }
                    let existing = state.entries.iter_mut().find(|entry| entry.entity == entity);
                    let role = match (role, existing.as_deref()) {
                        (Some(role), _) => role,
                        (None, Some(entry)) => entry.role,
                        (None, None) => return Err(AclError::RoleRequired),
                    };
                    let entry = AclEntry { entity, role };
                    match existing {
                        Some(existing) => *existing = entry.clone(),
                        None => {
                            if state.entries.len() >= MAX_ENTRIES {
                                return Err(AclError::TooManyEntries);
                            }
                            state.entries.push(entry.clone());
                        }
                    }
                    Some(entry)
                }
                AclChange::Delete { entity } => {
                    let index = state
                        .entries
                        .iter()
                        .position(|entry| entry.entity == entity)
                        .ok_or_else(|| AclError::EntityNotFound {
                            entity: entity.clone(),
                            project_number: state.project_number.clone(),
                        })?;
                    state.entries.remove(index);
                    None
                }
            };
            let entries = encode_changed_entries(&mut state, record.as_ref().and_then(|record| record.owner.as_ref()), &mut changed, target.kind())?;
            let micros = unix_micros(clock.now())?;
            state.metageneration = state
                .metageneration
                .checked_add(1)
                .ok_or_else(|| unavailable("metageneration exhausted"))?;
            match record {
                None => {
                    if target.kind() == AclKind::Bucket {
                        bucket_policy::replace_policy(&transaction, bucket, &state.entries, principals.as_ref())?;
                    }
                    sql::exec(&transaction, UPSERT_BUCKET_ENTRIES, params![bucket, scope(target.kind()), entries])
                        .map_err(unavailable)?;
                    sql::exec(&transaction,
                            "UPDATE buckets SET metageneration = ?2, updated = ?3, updated_nanos = 0 WHERE name = ?1", params![bucket, state.metageneration, micros])
                        .map_err(unavailable)?;
                    change_journal::append(
                        &transaction,
                        change_journal::origin(&access),
                        change_journal::ChangeKind::BucketUpdate,
                        bucket,
                        None,
                    )?;
                }
                Some(mut record) => {
                    sql::exec(&transaction, UPSERT_OBJECT_ENTRIES, params![bucket, record.name, record.generation, entries])
                        .map_err(unavailable)?;
                    sql::exec(&transaction,
                            "UPDATE objects SET metageneration = ?4, updated = ?5 WHERE bucket = ?1 AND name = ?2 AND generation = ?3", params![bucket, record.name, record.generation, state.metageneration, micros])
                        .map_err(unavailable)?;
                    record.metageneration = state.metageneration;
                    record.updated = super::system_time_micros(micros).map_err(unavailable)?;
                    change_journal::object(
                        &transaction,
                        change_journal::origin(&access),
                        change_journal::ChangeKind::Metadata,
                        &record,
                    )?;
                }
            }
            transaction.commit().map_err(unavailable)?;
            Ok((state, changed))
        })
        .await
    }

    /// Effective ACLs for rendering; `None` when the bucket no longer exists.
    pub(crate) async fn acl_projection(
        &self,
        bucket: &str,
        objects: Vec<(String, i64)>,
    ) -> Result<Option<AclProjection>, CatalogError> {
        self.projection(bucket, objects, None).await
    }

    /// One snapshot keeps each listed bucket's ACL projection and permission decision together.
    pub(crate) async fn bucket_acl_projections(
        &self,
        buckets: Vec<String>,
        access: &BucketAccess,
    ) -> Result<Vec<Option<AclProjection>>, CatalogError> {
        if matches!(access, BucketAccess::Policy { permission, .. } if *permission != crate::auth::Permission::BucketsGetIam)
        {
            return Err(CatalogError::Forbidden);
        }
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            buckets
                .iter()
                .map(|bucket| {
                    let Some(row) = bucket_row(&transaction, bucket)? else {
                        return Ok(None);
                    };
                    match check_access(&transaction, bucket, &row.project, &access) {
                        Ok(_) => bucket_projection(&transaction, bucket, &row).map(Some),
                        Err(error) if error.is_refusal() => Ok(None),
                        Err(error) => Err(error),
                    }
                })
                .collect()
        })
        .await
    }

    pub(crate) async fn object_acl_projection(
        &self,
        bucket: &str,
        objects: Vec<(String, i64)>,
        access: &BucketAccess,
    ) -> Result<Option<AclProjection>, CatalogError> {
        if matches!(access, BucketAccess::Policy { permission, .. } if *permission != crate::auth::Permission::ObjectsGetIam)
        {
            return Err(CatalogError::Forbidden);
        }
        self.projection(bucket, objects, Some(access.clone())).await
    }

    async fn projection(
        &self,
        bucket: &str,
        objects: Vec<(String, i64)>,
        access: Option<BucketAccess>,
    ) -> Result<Option<AclProjection>, CatalogError> {
        let bucket = bucket.to_owned();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            let Some(row) = bucket_row(&transaction, &bucket)? else {
                return Ok(None);
            };
            let default = || project_private(&row.project);
            let object_access = access.as_ref().map(|access| super::objects::object_acl_authority(&transaction, &bucket, access))
                .transpose().map_err(unavailable)?.flatten().map(|check| check.access);
            let page = serde_json::to_string(&objects).map_err(unavailable)?;
            let records = transaction.prepare_cached(
                "SELECT o.generation IS NOT NULL, o.owner, e.entries FROM json_each(?2) j LEFT JOIN objects o ON o.bucket=?1 AND o.name=json_extract(j.value,'$[0]') AND o.generation=json_extract(j.value,'$[1]') LEFT JOIN acl_object_entries e ON e.bucket=o.bucket AND e.name=o.name AND e.generation=o.generation ORDER BY j.key"
            )?.query_map(params![bucket, page], |row| Ok((row.get::<_, bool>(0)?, super::object_owner::read(row, 1)?, row.get::<_, Option<String>>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut entries = Vec::with_capacity(objects.len());
            let mut owners = Vec::with_capacity(objects.len());
            let mut visible = Vec::with_capacity(objects.len());
            let mut cached_public = None;
            for (exists, owner, stored) in records {
                let mut object_entries = stored.map(|entries| decode(&entries, AclKind::Object)).transpose()?.unwrap_or_else(default);
                let allowed = exists && match object_access {
                    Some(BucketAccess::Policy { identity, permission, project }) => {
                        !row.uniform_access && project.as_ref().is_none_or(|project| project == &row.project)
                            && object_entries_grant(owner.as_ref(), &object_entries, identity.as_ref(), *permission, || {
                                if let Some(public) = cached_public { return Ok(public); }
                                let public = super::policies::policy_public_access(&transaction, &bucket)?;
                                cached_public = Some(public);
                                Ok(public)
                            })?
                    }
                    Some(_) => false,
                    None => true,
                };
                visible.push(allowed);
                if !allowed {
                    entries.push(Vec::new());
                    owners.push(None);
                    continue;
                }
                if let Some(owner) = &owner {
                    owner.preserve_grant(&mut object_entries);
                }
                entries.push(object_entries);
                owners.push(owner);
            }
            let mut projection = bucket_projection(&transaction, &bucket, &row)?;
            projection.objects = entries;
            projection.owners = owners;
            projection.visible = visible;
            Ok(Some(projection))
        })
        .await
    }
}
