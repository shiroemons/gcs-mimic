//! Stored bucket, default object and object ACLs.
//!
//! A missing row means the resource still has the `projectPrivate` canned ACL of its owning
//! project, so buckets and objects created by any publication path need no ACL write. Once a
//! bucket's default object ACL is stored, publication copies it to each new generation in the
//! publishing transaction; later default changes therefore never rewrite existing objects.
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

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

/// Copy a stored default object ACL to a generation inserted in the same transaction.
pub(super) fn inherit_default_object_acl(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
) -> Result<(), ObjectError> {
    connection
        .execute(
            "INSERT INTO acl_object_entries (bucket, name, generation, entries)
             SELECT bucket, ?2, ?3, entries FROM acl_bucket_entries WHERE bucket = ?1 AND scope = 'defaultObject'",
            params![bucket, name, generation],
        )
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

/// ACLs rendered by `projection=full`; `None` lists mean uniform bucket-level access hides them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AclProjection {
    pub uniform_access: bool,
    pub project: String,
    pub bucket: Vec<AclEntry>,
    pub default_object: Vec<AclEntry>,
    pub bucket_metageneration: i64,
    pub objects: Vec<Vec<AclEntry>>,
}

struct BucketRow {
    project: String,
    metageneration: i64,
    uniform_access: bool,
}

fn bucket_row(connection: &Connection, bucket: &str) -> Result<Option<BucketRow>, CatalogError> {
    connection
        .query_row(
            "SELECT project, metageneration, uniform_bucket_level_access_locked_time IS NOT NULL FROM buckets WHERE name = ?1",
            [bucket],
            |row| {
                Ok(BucketRow {
                    project: row.get(0)?,
                    metageneration: row.get(1)?,
                    uniform_access: row.get(2)?,
                })
            },
        )
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
        .query_row(
            "SELECT entries FROM acl_bucket_entries WHERE bucket = ?1 AND scope = ?2",
            params![bucket, scope],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(unavailable)?
        .map(|entries| decode(&entries))
        .transpose()
}

fn stored_object_entries(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
) -> Result<Option<Vec<AclEntry>>, CatalogError> {
    connection
        .query_row(
            "SELECT entries FROM acl_object_entries WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
            params![bucket, name, generation],
            |row| row.get::<_, String>(0),
        )
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

/// Replace entries inside the metadata mutation's transaction, without a second revision bump.
pub(super) fn replace_resource_acl(
    connection: &Connection,
    target: &AclTarget,
    entries: &[AclEntry],
    access: &BucketAccess,
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
        match check_access(connection, bucket, &row.project, &policy_access) {
            Ok(_) => {}
            Err(CatalogError::Forbidden) => {
                if let BucketAccess::Policy { permission, .. } = &mut policy_access {
                    *permission = if target.kind() == AclKind::Object {
                        crate::auth::Permission::ObjectsGetIam
                    } else {
                        crate::auth::Permission::BucketsGetIam
                    };
                }
                check_access(connection, bucket, &row.project, &policy_access)?;
                // Preserve full-resource echoes only for callers already allowed to read the ACL.
                if !row.uniform_access {
                    let mut current = match target {
                        AclTarget::Object {
                            name, generation, ..
                        } => stored_object_entries(
                            connection,
                            bucket,
                            name,
                            generation.expect("metadata mutation selects a generation"),
                        )?,
                        _ => stored_bucket_entries(connection, bucket, scope(target.kind()))?,
                    }
                    .unwrap_or_else(|| project_private(&row.project));
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
    crate::acl::validate_entries(target.kind(), entries).map_err(CatalogError::InvalidMetadata)?;
    if entries
        .iter()
        .any(|entry| Entity::parse(&entry.entity).is_ok_and(Entity::is_public))
        && public_access_prevention(connection, bucket)?
    {
        return Err(CatalogError::PublicAccessPrevention);
    }
    let entries = serde_json::to_string(entries).map_err(unavailable)?;
    match target {
        AclTarget::Object {
            name, generation, ..
        } => {
            connection.execute(
                "INSERT INTO acl_object_entries (bucket, name, generation, entries) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(bucket, name, generation) DO UPDATE SET entries = excluded.entries",
                params![bucket, name, generation.expect("metadata mutation selects a generation"), entries],
            ).map_err(unavailable)?;
        }
        _ => {
            connection
                .execute(
                    "INSERT INTO acl_bucket_entries (bucket, scope, entries) VALUES (?1, ?2, ?3)
                 ON CONFLICT(bucket, scope) DO UPDATE SET entries = excluded.entries",
                    params![bucket, scope(target.kind()), entries],
                )
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
    check_access(connection, bucket, &row.project, access)?;
    if row.uniform_access {
        return Err(AclError::UniformAccess);
    }
    Ok(match target {
        AclTarget::Bucket(_) | AclTarget::DefaultObject(_) => {
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
                true,
                access,
            )?;
            let entries = stored_object_entries(connection, bucket, name, record.generation)?
                .unwrap_or_else(|| project_private(&row.project));
            let state = AclState {
                entries,
                metageneration: record.metageneration,
                object: Some((record.name.clone(), record.generation)),
            };
            (state, Some(record))
        }
    })
}

impl Catalog {
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
        self.run(move |connection| {
            Ok((|| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(unavailable)?;
                let (mut state, record) = load(&transaction, &target, &access)?;
                let bucket = target.bucket();
                let changed = match change {
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
                let entries = serde_json::to_string(&state.entries).map_err(unavailable)?;
                let micros = unix_micros(clock.now())?;
                state.metageneration = state
                    .metageneration
                    .checked_add(1)
                    .ok_or_else(|| unavailable("metageneration exhausted"))?;
                match record {
                    None => {
                        transaction
                            .execute(
                                "INSERT INTO acl_bucket_entries (bucket, scope, entries) VALUES (?1, ?2, ?3)
                                 ON CONFLICT(bucket, scope) DO UPDATE SET entries = excluded.entries",
                                params![bucket, scope(target.kind()), entries],
                            )
                            .map_err(unavailable)?;
                        transaction
                            .execute(
                                "UPDATE buckets SET metageneration = ?2, updated = ?3 WHERE name = ?1",
                                params![bucket, state.metageneration, micros],
                            )
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
                            .execute(
                                "INSERT INTO acl_object_entries (bucket, name, generation, entries) VALUES (?1, ?2, ?3, ?4)
                                 ON CONFLICT(bucket, name, generation) DO UPDATE SET entries = excluded.entries",
                                params![bucket, record.name, record.generation, entries],
                            )
                            .map_err(unavailable)?;
                        transaction
                            .execute(
                                "UPDATE objects SET metageneration = ?4, updated = ?5 WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
                                params![bucket, record.name, record.generation, state.metageneration, micros],
                            )
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
        let bucket = bucket.to_owned();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            let Some(row) = bucket_row(&transaction, &bucket)? else {
                return Ok(None);
            };
            let default = || project_private(&row.project);
            let mut entries = Vec::with_capacity(objects.len());
            for (name, generation) in &objects {
                entries.push(
                    stored_object_entries(&transaction, &bucket, name, *generation)?
                        .unwrap_or_else(default),
                );
            }
            Ok(Some(AclProjection {
                uniform_access: row.uniform_access,
                bucket: stored_bucket_entries(&transaction, &bucket, "bucket")?
                    .unwrap_or_else(default),
                default_object: stored_bucket_entries(&transaction, &bucket, "defaultObject")?
                    .unwrap_or_else(default),
                bucket_metageneration: row.metageneration,
                objects: entries,
                project: row.project,
            }))
        })
        .await
    }
}
