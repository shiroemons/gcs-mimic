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
use crate::{
    acl::{AclEntry, AclKind, AclTarget, Entity, MAX_ENTRIES, project_private},
    bucket::{BucketAccess, CatalogError},
    object::{ObjectConditions, ObjectError},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

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

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

/// An owner without a canonical alias still consumes one logical ACL grant.
fn validate_effective_entries(
    kind: AclKind,
    entries: &[AclEntry],
    owner: Option<&crate::object::owner::ObjectOwner>,
) -> Result<(), CatalogError> {
    crate::acl::validate_entries(kind, entries).map_err(CatalogError::from)?;
    if entries.len() >= MAX_ENTRIES && owner.is_some_and(|owner| owner.entity().is_none()) {
        return Err(CatalogError::from(crate::acl::AclError::TooManyEntries));
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) enum PublicationAcl<'a> {
    InheritDefault,
    Preserve(&'a [AclEntry]),
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
    let row = bucket_row(connection, bucket)
        .map_err(object_unavailable)?
        .ok_or(ObjectError::BucketNotFound)?;
    let mut entries = match mode {
        PublicationAcl::InheritDefault => {
            let stored = stored_bucket_entries(connection, bucket, "defaultObject")
                .map_err(object_unavailable)?;
            if stored.is_none() && record.owner.is_none() {
                return Ok(());
            }
            stored.unwrap_or_else(|| project_private(&row.project))
        }
        PublicationAcl::Preserve(entries) => entries.to_vec(),
    };
    if let Some(owner) = &record.owner {
        owner.preserve_grant(&mut entries);
    }
    validate_effective_entries(AclKind::Object, &entries, record.owner.as_ref()).map_err(
        |error| match error {
            CatalogError::InvalidMetadata(reason) => ObjectError::InvalidFixedMetadata(reason),
            error => object_unavailable(error),
        },
    )?;
    if let Some(owner) = &record.owner {
        owner.remove_implicit_grant(&mut entries);
    }
    let entries = serde_json::to_string(&entries).map_err(object_unavailable)?;
    connection
        .prepare_cached(UPSERT_OBJECT_ENTRIES)
        .and_then(|mut statement| {
            statement.execute(params![bucket, record.name, record.generation, entries])
        })
        .map_err(object_unavailable)?;
    Ok(())
}

/// The effective entries plus the containing resource version used for ACL etags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AclState {
    pub entries: Vec<AclEntry>,
    pub metageneration: i64,
    /// Selected object name and generation for object ACLs.
    pub object: Option<(String, i64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AclChange {
    /// Insert, update and patch upsert one entity; `None` keeps an existing patch role.
    Upsert {
        entity: String,
        role: Option<String>,
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
    EntityNotFound(String),
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
    connection
        .prepare_cached(
            "SELECT project, metageneration, uniform_bucket_level_access_locked_time IS NOT NULL, acl_policy_bound FROM buckets WHERE name = ?1",
        )
        .and_then(|mut statement| {
            statement.query_row([bucket], |row| {
                Ok(BucketRow {
                    project: row.get(0)?,
                    metageneration: row.get(1)?,
                    uniform_access: row.get(2)?,
                    policy_bound: row.get(3)?,
                })
            })
        })
        .optional()
        .map_err(unavailable)
}

fn decode(entries: &str) -> Result<Vec<AclEntry>, CatalogError> {
    serde_json::from_str(entries).map_err(|_| CatalogError::Corrupt("invalid stored ACL".into()))
}

fn stored_bucket_entries(
    connection: &Connection,
    bucket: &str,
    scope: &str,
) -> Result<Option<Vec<AclEntry>>, CatalogError> {
    connection
        .prepare_cached("SELECT entries FROM acl_bucket_entries WHERE bucket = ?1 AND scope = ?2")
        .and_then(|mut statement| {
            statement.query_row(params![bucket, scope], |row| row.get::<_, String>(0))
        })
        .optional()
        .map_err(unavailable)?
        .map(|entries| decode(&entries))
        .transpose()
}

pub(super) fn bucket_granted(
    connection: &Connection,
    bucket: &str,
    project: &str,
    identity: Option<&crate::auth::Identity>,
    permission: &str,
) -> Result<bool, CatalogError> {
    let entries = stored_bucket_entries(connection, bucket, "bucket")?
        .unwrap_or_else(|| project_private(project));
    let public = super::policies::policy_public_access(connection, bucket)?;
    // Bucket ACL roles are the same authority as legacy bucket IAM, including under UBLA.
    Ok(entries.iter().any(|entry| {
        crate::acl::bucket_iam_role(&entry.role)
            .and_then(crate::auth::roles::permissions)
            .is_some_and(|permissions| permissions.binary_search(&permission).is_ok())
            && Entity::parse(&entry.entity).is_ok_and(|entity| entity.matches(identity, public))
    }))
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
    let exists: bool = connection
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket=?1 AND name=?2 AND generation=?3)",
        )
        .and_then(|mut statement| {
            statement.query_row(params![bucket, name, generation], |row| row.get(0))
        })
        .map_err(unavailable)?;
    if !exists {
        return Ok(false);
    }
    let owner = super::object_owner::selected(connection, bucket, name, generation)?;
    if owner
        .as_ref()
        .is_some_and(|owner| owner.is_principal(identity.as_ref()))
    {
        return Ok(true);
    }
    let entries = stored_object_entries(connection, bucket, name, generation)?
        .unwrap_or_else(|| project_private(&row.project));
    let public = super::policies::policy_public_access(connection, bucket)?;
    Ok(entries.iter().any(|entry| {
        crate::acl::object_role_allows(&entry.role, *permission)
            && Entity::parse(&entry.entity)
                .is_ok_and(|entity| entity.matches(identity.as_ref(), public))
    }))
}

fn stored_object_entries(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
) -> Result<Option<Vec<AclEntry>>, CatalogError> {
    connection
        .prepare_cached(
            "SELECT entries FROM acl_object_entries WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
        )
        .and_then(|mut statement| {
            statement.query_row(params![bucket, name, generation], |row| {
                row.get::<_, String>(0)
            })
        })
        .optional()
        .map_err(unavailable)?
        .map(|entries| decode(&entries))
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
            Err(CatalogError::Forbidden) => {
                if let BucketAccess::Policy { permission, .. } = &mut policy_access {
                    *permission = if target.kind() == AclKind::Object {
                        crate::auth::Permission::ObjectsGetIam
                    } else {
                        crate::auth::Permission::BucketsGetIam
                    };
                }
                target_access(connection, target, &row.project, &policy_access)?;
                // Preserve full-resource echoes only for callers already allowed to read the ACL.
                if !row.uniform_access {
                    let mut current = match target {
                        ResolvedAclTarget::Object {
                            name, generation, ..
                        } => stored_object_entries(connection, bucket, name, generation)?,
                        _ => stored_bucket_entries(connection, bucket, scope(target.kind()))?,
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
                return Err(CatalogError::Forbidden);
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
            connection
                .prepare_cached(UPSERT_OBJECT_ENTRIES)
                .and_then(|mut statement| {
                    statement.execute(params![bucket, name, generation, entries])
                })
                .map_err(unavailable)?;
        }
        _ => {
            connection
                .prepare_cached(UPSERT_BUCKET_ENTRIES)
                .and_then(|mut statement| {
                    statement.execute(params![bucket, scope(target.kind()), entries])
                })
                .map_err(unavailable)?;
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
            let entries = stored_bucket_entries(connection, bucket, scope(target.kind()))?
                .unwrap_or_else(|| project_private(&row.project));
            let state = AclState {
                entries,
                metageneration: row.metageneration,
                object: None,
            };
            (state, None)
        }
        AclTarget::Object {
            name, generation, ..
        } => {
            let (record, _) = selected(
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
    let mut stored = state.entries.clone();
    if let Some(owner) = owner {
        owner.remove_implicit_grant(&mut stored);
    }
    serde_json::to_string(&stored).map_err(unavailable)
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
            let transaction = connection.transaction().map_err(unavailable)?;
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
            let incarnation: String = transaction
                .prepare_cached("SELECT incarnation FROM bucket_incarnations WHERE bucket=?1")
                .and_then(|mut statement| statement.query_row([&snapshot.bucket], |row| row.get(0)))
                .map_err(unavailable)?;
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
                Err(CatalogError::Forbidden) => {
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

    /// Read effective entries; bucket metageneration conditions apply to default object ACL lists.
    pub(crate) async fn read_acl(
        &self,
        target: AclTarget,
        if_metageneration_match: Option<i64>,
        if_metageneration_not_match: Option<i64>,
        access: &BucketAccess,
    ) -> Result<AclState, AclError> {
        let access = access.clone();
        self.run(move |connection| {
            Ok((|| {
                let transaction = connection.transaction().map_err(unavailable)?;
                let (state, _) = load(&transaction, &target, &access)?;
                if if_metageneration_match.is_some_and(|value| value != state.metageneration) {
                    return Err(CatalogError::PreconditionFailed.into());
                }
                if if_metageneration_not_match == Some(state.metageneration) {
                    return Err(AclError::NotModified);
                }
                Ok(state)
            })())
        })
        .await?
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
            Ok((|| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(unavailable)?;
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
                        let position = state.entries.iter().position(|entry| entry.entity == entity);
                        let role = match (role, position) {
                            (Some(role), _) => role,
                            (None, Some(index)) => state.entries[index].role.clone(),
                            (None, None) => return Err(AclError::RoleRequired),
                        };
                        let entry = AclEntry { entity, role };
                        match position {
                            Some(index) => state.entries[index] = entry.clone(),
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
                            .ok_or(AclError::EntityNotFound(entity))?;
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
                        transaction
                            .prepare_cached(UPSERT_BUCKET_ENTRIES)
                            .and_then(|mut statement| {
                                statement.execute(params![bucket, scope(target.kind()), entries])
                            })
                            .map_err(unavailable)?;
                        transaction
                            .prepare_cached(
                                "UPDATE buckets SET metageneration = ?2, updated = ?3, updated_nanos = 0 WHERE name = ?1",
                            )
                            .and_then(|mut statement| {
                                statement.execute(params![bucket, state.metageneration, micros])
                            })
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
                        transaction
                            .prepare_cached(UPSERT_OBJECT_ENTRIES)
                            .and_then(|mut statement| {
                                statement.execute(params![bucket, record.name, record.generation, entries])
                            })
                            .map_err(unavailable)?;
                        transaction
                            .prepare_cached(
                                "UPDATE objects SET metageneration = ?4, updated = ?5 WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
                            )
                            .and_then(|mut statement| {
                                statement.execute(params![bucket, record.name, record.generation, state.metageneration, micros])
                            })
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
            })())
        })
        .await?
    }

    /// Effective ACLs for rendering; `None` when the bucket no longer exists.
    pub(crate) async fn acl_projection(
        &self,
        bucket: &str,
        objects: Vec<(String, i64)>,
    ) -> Result<Option<AclProjection>, CatalogError> {
        self.projection(bucket, objects, None).await
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
            let transaction = connection.transaction().map_err(unavailable)?;
            let Some(row) = bucket_row(&transaction, &bucket)? else {
                return Ok(None);
            };
            let default = || project_private(&row.project);
            let mut entries = Vec::with_capacity(objects.len());
            let mut owners = Vec::with_capacity(objects.len());
            let mut visible = Vec::with_capacity(objects.len());
            for (name, generation) in &objects {
                let allowed = match &access {
                    Some(access) => match super::objects::authorize_generation(
                        &transaction,
                        &bucket,
                        name,
                        *generation,
                        access,
                    ) {
                        Ok(()) => true,
                        Err(ObjectError::Forbidden) => false,
                        Err(error) => return Err(unavailable(error)),
                    },
                    None => true,
                };
                visible.push(allowed);
                if !allowed {
                    entries.push(Vec::new());
                    owners.push(None);
                    continue;
                }
                let owner =
                    super::object_owner::selected(&transaction, &bucket, name, *generation)?;
                let mut object_entries =
                    stored_object_entries(&transaction, &bucket, name, *generation)?
                        .unwrap_or_else(default);
                if let Some(owner) = &owner {
                    owner.preserve_grant(&mut object_entries);
                }
                entries.push(object_entries);
                owners.push(owner);
            }
            Ok(Some(AclProjection {
                uniform_access: row.uniform_access,
                bucket: stored_bucket_entries(&transaction, &bucket, "bucket")?
                    .unwrap_or_else(default),
                default_object: stored_bucket_entries(&transaction, &bucket, "defaultObject")?
                    .unwrap_or_else(default),
                bucket_metageneration: row.metageneration,
                objects: entries,
                owners,
                visible,
                project: row.project,
            }))
        })
        .await
    }
}
