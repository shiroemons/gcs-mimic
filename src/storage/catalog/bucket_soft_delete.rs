//! Soft-deleted bucket versions.
//!
//! Deleting a bucket with a soft delete policy copies its row, IAM bindings and ACL entries
//! into `soft_deleted_buckets` in the deletion transaction. Its retained soft-deleted objects
//! stay in `objects`, re-keyed to an internal bucket key that no bucket name can take, so a
//! new live bucket with the same name never sees them and blob reference checks still find
//! them. Restore reverses both moves in one transaction. Only catalog rows change: no blob is
//! copied, linked or renamed.

use super::{
    Catalog, bucket_columns, bucket_metadata_charge, change_journal, policies::next_revision,
    read_bucket, select_bucket, system_time_micros, unavailable, unix_micros,
};
use crate::bucket::{BucketAccess, BucketRecord, CatalogError, SoftDeletedBucket};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use std::time::SystemTime;

pub(super) const SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;
UPDATE buckets SET generation = max(time_created, 1);
UPDATE object_generation SET last_generation = max(last_generation, (SELECT coalesce(max(generation), 0) FROM buckets));
CREATE TABLE soft_deleted_buckets (
    name TEXT NOT NULL COLLATE BINARY,
    generation INTEGER NOT NULL CHECK(generation > 0),
    project TEXT NOT NULL,
    location TEXT NOT NULL,
    location_type TEXT NOT NULL,
    storage_class TEXT NOT NULL,
    labels TEXT NOT NULL,
    metageneration INTEGER NOT NULL,
    time_created INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    versioning_enabled INTEGER NOT NULL,
    default_event_based_hold INTEGER NOT NULL,
    cors TEXT NOT NULL,
    public_access_prevention_enforced INTEGER NOT NULL,
    uniform_bucket_level_access_locked_time INTEGER,
    lifecycle TEXT NOT NULL,
    retention_policy TEXT,
    soft_delete_policy TEXT,
    object_retention_enabled INTEGER NOT NULL,
    autoclass TEXT,
    time_created_nanos INTEGER NOT NULL,
    updated_nanos INTEGER NOT NULL,
    acl_policy_bound INTEGER NOT NULL,
    bindings TEXT NOT NULL,
    acl TEXT NOT NULL CHECK(json_valid(acl) AND json_type(acl) = 'array'),
    soft_delete_time INTEGER NOT NULL,
    hard_delete_time INTEGER NOT NULL,
    PRIMARY KEY(name, generation)
) WITHOUT ROWID;
CREATE INDEX soft_deleted_buckets_project ON soft_deleted_buckets(project, name, generation);
CREATE INDEX soft_deleted_buckets_expiry ON soft_deleted_buckets(hard_delete_time);
PRAGMA user_version = 45;";

/// Expired versions removed per maintenance transaction; each also removes its objects.
const PURGE_BATCH: i64 = 16;

const RETIRE_BUCKET: &str = concat!(
    "INSERT INTO soft_deleted_buckets (",
    bucket_columns!(),
    ", acl_policy_bound, bindings, acl, soft_delete_time, hard_delete_time) SELECT ",
    bucket_columns!(),
    ", acl_policy_bound,
        (SELECT bindings FROM bucket_policies WHERE bucket = ?1),
        (SELECT json_group_array(json_object('scope', scope, 'entries', json(entries)))
            FROM acl_bucket_entries WHERE bucket = ?1),
        ?2, ?3 FROM buckets WHERE name = ?1"
);
const RESTORE_BUCKET: &str = concat!(
    "INSERT INTO buckets (",
    bucket_columns!(),
    ", acl_policy_bound) SELECT ",
    bucket_columns!(),
    ", acl_policy_bound FROM soft_deleted_buckets WHERE name = ?1 AND generation = ?2"
);
const SELECT_VERSION: &str = concat!(
    "SELECT ",
    bucket_columns!(),
    ", soft_delete_time, hard_delete_time FROM soft_deleted_buckets
        WHERE name = ?1 AND generation = ?2 AND hard_delete_time > ?3"
);
// Keyset order is (name, generation); the prefix bound keeps the name range indexable.
const LIST_VERSIONS: &str = concat!(
    "SELECT ",
    bucket_columns!(),
    ", soft_delete_time, hard_delete_time FROM soft_deleted_buckets
        WHERE project = ?1 AND hard_delete_time > ?2 AND name >= ?3
            AND substr(name, 1, length(?3)) = ?3
            AND (?4 IS NULL OR name > ?4 OR (name = ?4 AND generation > ?5))
        ORDER BY name, generation LIMIT ?6"
);

/// A bounded page of soft-deleted versions in (name, generation) order.
pub(crate) struct SoftDeletedBucketPage {
    pub items: Vec<SoftDeletedBucket>,
    pub has_more: bool,
}

/// Retained objects of a soft-deleted version live under this key. Bucket names never
/// contain `#` in any profile, so no request can address it.
fn retained_key(name: &str, generation: i64) -> String {
    format!("{name}#{generation}")
}

/// Moves objects (and, through deferred foreign keys, their ACL rows) between bucket keys.
fn rekey_objects(connection: &Connection, from: &str, to: &str) -> Result<(), CatalogError> {
    // Parent and child keys change in one transaction; defer the check to its commit.
    connection
        .pragma_update(None, "defer_foreign_keys", true)
        .map_err(unavailable)?;
    for sql in [
        "UPDATE acl_object_entries SET bucket = ?2 WHERE bucket = ?1",
        "UPDATE objects SET bucket = ?2 WHERE bucket = ?1",
    ] {
        connection
            .prepare_cached(sql)
            .and_then(|mut statement| statement.execute(params![from, to]))
            .map_err(unavailable)?;
    }
    Ok(())
}

/// Called inside the bucket deletion transaction after its permission, condition and
/// emptiness checks. Without a positive soft delete policy the deletion stays permanent.
pub(super) fn retire(connection: &Connection, name: &str, now: i64) -> Result<(), CatalogError> {
    let (retention, generation): (i64, i64) = connection
        .prepare_cached(
            "SELECT COALESCE(json_extract(soft_delete_policy, '$.retention_duration_seconds'), 0), generation FROM buckets WHERE name = ?1",
        )
        .and_then(|mut statement| statement.query_row([name], |row| Ok((row.get(0)?, row.get(1)?))))
        .map_err(unavailable)?;
    if retention <= 0 {
        return Ok(());
    }
    let latest_object: Option<i64> = connection
        .prepare_cached(
            "SELECT max(hard_delete_time) FROM objects WHERE bucket = ?1 AND soft_delete_time IS NOT NULL",
        )
        .and_then(|mut statement| statement.query_row([name], |row| row.get(0)))
        .map_err(unavailable)?;
    let hard_delete = retention
        .checked_mul(1_000_000)
        .and_then(|retention| now.checked_add(retention))
        .ok_or_else(|| unavailable("soft delete time overflow"))?
        .max(latest_object.unwrap_or(i64::MIN));
    connection
        .prepare_cached(RETIRE_BUCKET)
        .and_then(|mut statement| statement.execute(params![name, now, hard_delete]))
        .map_err(unavailable)?;
    rekey_objects(connection, name, &retained_key(name, generation))
}

fn read_version(row: &Row<'_>) -> rusqlite::Result<SoftDeletedBucket> {
    Ok(SoftDeletedBucket {
        record: read_bucket(row)?,
        soft_delete_time: system_time_micros(row.get("soft_delete_time")?)?,
        hard_delete_time: system_time_micros(row.get("hard_delete_time")?)?,
    })
}

/// Soft-deleted versions are authorized against their project only: the deleted bucket's own
/// IAM policy no longer applies, and the documented permissions are project-level.
fn authorize(
    connection: &Connection,
    access: &BucketAccess,
    project: &str,
) -> Result<(), CatalogError> {
    match access {
        BucketAccess::Permissive => Ok(()),
        BucketAccess::Admin(grant) => grant.authorize(connection),
        BucketAccess::Policy {
            identity,
            permission,
            ..
        } => {
            if identity
                .as_ref()
                .is_some_and(|identity| identity.allows(permission.as_str(), project))
            {
                Ok(())
            } else {
                Err(CatalogError::Forbidden)
            }
        }
    }
}

fn restore_in_transaction(
    connection: &Connection,
    name: &str,
    generation: i64,
    now: i64,
    access: &BucketAccess,
) -> Result<BucketRecord, CatalogError> {
    let project: String = connection
        .prepare_cached(
            "SELECT project FROM soft_deleted_buckets WHERE name = ?1 AND generation = ?2 AND hard_delete_time > ?3",
        )
        .and_then(|mut statement| {
            statement.query_row(params![name, generation, now], |row| row.get(0))
        })
        .optional()
        .map_err(unavailable)?
        .ok_or(CatalogError::NotFound)?;
    authorize(connection, access, &project)?;
    if select_bucket(connection, name)
        .map_err(unavailable)?
        .is_some()
    {
        return Err(CatalogError::AlreadyExists);
    }
    // The restored policy is a new IAM revision so stale policy etags cannot overwrite it.
    let revision = next_revision(connection)?;
    connection
        .prepare_cached(
            "INSERT INTO bucket_policies SELECT name, bindings, ?3 FROM soft_deleted_buckets WHERE name = ?1 AND generation = ?2",
        )
        .and_then(|mut statement| statement.execute(params![name, generation, revision]))
        .map_err(unavailable)?;
    for sql in [
        RESTORE_BUCKET,
        "INSERT INTO acl_bucket_entries (bucket, scope, entries) SELECT ?1, json_extract(value, '$.scope'), json_extract(value, '$.entries') FROM soft_deleted_buckets, json_each(acl) WHERE name = ?1 AND generation = ?2",
    ] {
        connection
            .prepare_cached(sql)
            .and_then(|mut statement| statement.execute(params![name, generation]))
            .map_err(unavailable)?;
    }
    // Sessions bound to the deleted incarnation must not resume against the restored bucket.
    connection
        .prepare_cached("INSERT INTO bucket_incarnations VALUES (?1, lower(hex(randomblob(16))))")
        .and_then(|mut statement| statement.execute([name]))
        .map_err(unavailable)?;
    rekey_objects(connection, &retained_key(name, generation), name)?;
    connection
        .prepare_cached("DELETE FROM soft_deleted_buckets WHERE name = ?1 AND generation = ?2")
        .and_then(|mut statement| statement.execute(params![name, generation]))
        .map_err(unavailable)?;
    change_journal::append(
        connection,
        change_journal::origin(access),
        change_journal::ChangeKind::BucketCreate,
        name,
        None,
    )?;
    select_bucket(connection, name)
        .map_err(unavailable)?
        .ok_or(CatalogError::NotFound)
}

/// Permanently removes up to `PURGE_BATCH` expired versions with their retained objects and
/// returns how many it removed. Blob files are reclaimed through the deletion journal.
fn purge_batch(connection: &mut Connection, now: i64) -> Result<i64, CatalogError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let expired = transaction
        .prepare_cached(
            "SELECT name, generation FROM soft_deleted_buckets WHERE hard_delete_time <= ?1 ORDER BY hard_delete_time LIMIT ?2",
        )
        .map_err(unavailable)?
        .query_map(params![now, PURGE_BATCH], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(unavailable)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(unavailable)?;
    for (name, generation) in &expired {
        let key = retained_key(name, *generation);
        for sql in [
            "INSERT OR IGNORE INTO pending_blob_deletions SELECT blob_id FROM objects WHERE bucket = ?1",
            "DELETE FROM objects WHERE bucket = ?1",
        ] {
            transaction
                .prepare_cached(sql)
                .and_then(|mut statement| statement.execute([&key]))
                .map_err(unavailable)?;
        }
        transaction
            .prepare_cached("DELETE FROM soft_deleted_buckets WHERE name = ?1 AND generation = ?2")
            .and_then(|mut statement| statement.execute(params![name, generation]))
            .map_err(unavailable)?;
    }
    transaction.commit().map_err(unavailable)?;
    Ok(expired.len() as i64)
}

fn earliest_hard_delete(connection: &Connection) -> Result<Option<i64>, CatalogError> {
    connection
        .prepare_cached("SELECT min(hard_delete_time) FROM soft_deleted_buckets")
        .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
        .map_err(unavailable)
}

impl Catalog {
    /// Reads one unexpired soft-deleted version.
    pub(crate) async fn get_soft_deleted_bucket(
        &self,
        name: &str,
        generation: i64,
        access: &BucketAccess,
    ) -> Result<SoftDeletedBucket, CatalogError> {
        let (name, access, clock) = (name.to_owned(), access.clone(), self.0.clock.clone());
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            let now = unix_micros(clock.now())?;
            let version = transaction
                .prepare_cached(SELECT_VERSION)
                .and_then(|mut statement| {
                    statement
                        .query_row(params![name, generation, now], read_version)
                        .optional()
                })
                .map_err(unavailable)?
                .ok_or(CatalogError::NotFound)?;
            authorize(&transaction, &access, &version.record.project)?;
            Ok(version)
        })
        .await
    }

    /// Lists unexpired soft-deleted versions of `project` after the `(name, generation)` cursor.
    pub(crate) async fn list_soft_deleted_buckets(
        &self,
        project: &str,
        prefix: &str,
        start_after: Option<(String, i64)>,
        limit: usize,
    ) -> Result<SoftDeletedBucketPage, CatalogError> {
        let (project, prefix, clock) =
            (project.to_owned(), prefix.to_owned(), self.0.clock.clone());
        self.run(move |connection| {
            let now = unix_micros(clock.now())?;
            let (after_name, after_generation) = start_after.unzip();
            let mut statement = connection
                .prepare_cached(LIST_VERSIONS)
                .map_err(unavailable)?;
            let mut rows = statement
                .query(params![
                    project,
                    now,
                    prefix,
                    after_name,
                    after_generation,
                    limit as i64 + 1
                ])
                .map_err(unavailable)?;
            let mut items = Vec::new();
            let mut remaining = super::BUCKET_LIST_METADATA_BUDGET;
            let mut has_more = false;
            while let Some(row) = rows.next().map_err(unavailable)? {
                let charge = bucket_metadata_charge(row)?;
                if items.len() == limit || (!items.is_empty() && charge > remaining) {
                    has_more = !items.is_empty();
                    break;
                }
                items.push(read_version(row).map_err(unavailable)?);
                remaining = remaining.saturating_sub(charge);
            }
            Ok(SoftDeletedBucketPage { items, has_more })
        })
        .await
    }

    /// Restores a soft-deleted version as the live bucket; its objects stay soft-deleted.
    pub(crate) async fn restore_bucket(
        &self,
        name: &str,
        generation: i64,
        access: &BucketAccess,
    ) -> Result<BucketRecord, CatalogError> {
        let (name, access, clock) = (name.to_owned(), access.clone(), self.0.clock.clone());
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            let now = unix_micros(clock.now())?;
            let record = restore_in_transaction(&transaction, &name, generation, now, &access)?;
            transaction.commit().map_err(unavailable)?;
            Ok(record)
        })
        .await
    }

    /// Removes expired versions and returns the next hard delete time, for the maintenance
    /// scheduler's deadline. Reads already hide expired versions, so this only frees space.
    /// `now_millis` is the pass's single clock reading, shared with the other sweeps.
    pub(super) async fn maintain_soft_deleted_buckets(
        &self,
        now_millis: i64,
    ) -> Result<Option<SystemTime>, CatalogError> {
        let now = now_millis
            .checked_mul(1000)
            .ok_or_else(|| unavailable("maintenance time overflow"))?;
        let (purged, next) = self
            .run(move |connection| {
                // Every maintenance pass reaches this sweep, including passes that requests
                // wake. Taking the write lock only when a version is due keeps a pass with
                // nothing to purge from waiting on another writer and holding the connection.
                let mut next = earliest_hard_delete(connection)?;
                let mut purged = 0;
                while next.is_some_and(|time| time <= now) {
                    let removed = purge_batch(connection, now)?;
                    purged += removed;
                    next = earliest_hard_delete(connection)?;
                    if removed == 0 {
                        break;
                    }
                }
                Ok((
                    purged,
                    next.map(system_time_micros)
                        .transpose()
                        .map_err(unavailable)?,
                ))
            })
            .await?;
        if purged > 0 {
            self.reclaim_after_commit().await;
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests;
