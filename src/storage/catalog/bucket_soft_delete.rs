//! Soft-deleted bucket versions.
//!
//! Deleting a bucket with a soft delete policy copies its row, IAM bindings and ACL entries
//! into `soft_deleted_buckets` in the deletion transaction. Its retained soft-deleted objects
//! stay in `objects`, re-keyed to an internal bucket key that no bucket name can take, so a
//! new live bucket with the same name never sees them and blob reference checks still find
//! them. Restore reverses both moves in one transaction. Only catalog rows change: no blob is
//! copied, linked or renamed.

use super::{
    Catalog, UnixMicros, acl::reconcile_bucket_policy, bucket_columns, bucket_metadata_charge,
    change_journal, policies::next_revision, principals::Profile, read_bucket, select_bucket,
    system_time_micros, unavailable, unix_micros,
};
use crate::bucket::{BucketAccess, BucketRecord, CatalogError, SoftDeletedBucket};
use crate::storage::catalog::sql;
use rusqlite::{Connection, OptionalExtension, Row, params};
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

/// Buckets get their own nanosecond-scale generation allocator (Cloud Storage returns 19-digit
/// bucket generations), separate from the microsecond-scale object allocator. Existing
/// generations are multiplied by 1000, which keeps their order and uniqueness; the retained
/// objects of a soft-deleted bucket are keyed by `name#generation`, so appending the three
/// digits to that decimal key moves them in step. Foreign keys to the old object keys are
/// deferred because parents and children change in one transaction. A generation backfilled
/// from a creation time beyond year 2262 would overflow a signed 64-bit nanosecond value, so
/// it saturates at the largest representable multiple of 1000. Saturation is not injective: if
/// two buckets (or two generations of one name) both saturate to the same value, the primary
/// key of `soft_deleted_buckets` (or the `objects` key rewrite) is violated, the transaction
/// fails and the catalog cannot be opened until those rows are removed or edited by hand. That
/// needs backfilled creation times past 2262, which no supported clock produces.
pub(super) const GENERATION_SCHEMA: &str = "PRAGMA defer_foreign_keys = ON;
UPDATE acl_object_entries SET bucket = (
        SELECT name || '#' || (min(generation, 9223372036854775) * 1000)
        FROM soft_deleted_buckets WHERE name || '#' || generation = acl_object_entries.bucket)
    WHERE bucket IN (SELECT name || '#' || generation FROM soft_deleted_buckets);
UPDATE objects SET bucket = (
        SELECT name || '#' || (min(generation, 9223372036854775) * 1000)
        FROM soft_deleted_buckets WHERE name || '#' || generation = objects.bucket)
    WHERE bucket IN (SELECT name || '#' || generation FROM soft_deleted_buckets);
UPDATE buckets SET generation = min(generation, 9223372036854775) * 1000;
UPDATE soft_deleted_buckets SET generation = min(generation, 9223372036854775) * 1000;
CREATE TABLE bucket_generation (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    last_generation INTEGER NOT NULL CHECK(last_generation >= 0)
) WITHOUT ROWID;
INSERT INTO bucket_generation SELECT 1, max(
    coalesce((SELECT max(generation) FROM buckets), 0),
    coalesce((SELECT max(generation) FROM soft_deleted_buckets), 0));
PRAGMA user_version = 46;";

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
    connection.pragma_update(None, "defer_foreign_keys", true)?;
    for query in [
        "UPDATE acl_object_entries SET bucket = ?2 WHERE bucket = ?1",
        "UPDATE objects SET bucket = ?2 WHERE bucket = ?1",
    ] {
        sql::exec(connection, query, params![from, to])?;
    }
    Ok(())
}

/// Called inside the bucket deletion transaction after its permission, condition and
/// emptiness checks. Without a positive soft delete policy the deletion stays permanent.
pub(super) fn retire(connection: &Connection, name: &str, now: i64) -> Result<(), CatalogError> {
    let (retention, generation): (i64, i64) = sql::row(
        connection,
        "SELECT COALESCE(json_extract(soft_delete_policy, '$.retention_duration_seconds'), 0), generation FROM buckets WHERE name = ?1",
        [name],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if retention <= 0 {
        return Ok(());
    }
    let latest_object: Option<i64> = sql::scalar(
        connection,
        "SELECT max(hard_delete_time) FROM objects WHERE bucket = ?1 AND soft_delete_time IS NOT NULL",
        [name],
    )?;
    let hard_delete = retention
        .checked_mul(1_000_000)
        .and_then(|retention| now.checked_add(retention))
        .ok_or_else(|| unavailable("soft delete time overflow"))?
        .max(latest_object.unwrap_or(i64::MIN));
    sql::exec(connection, RETIRE_BUCKET, params![name, now, hard_delete])?;
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
                .is_some_and(|identity| identity.allows(*permission, project))
            {
                Ok(())
            } else {
                Err(CatalogError::Denied(crate::auth::Denied::new(
                    identity.as_ref(),
                    *permission,
                    crate::auth::denied::Resource::project(Some(project)),
                )))
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
    principals: Option<&Profile>,
) -> Result<BucketRecord, CatalogError> {
    let project: String = sql::optional_scalar(connection,
            "SELECT project FROM soft_deleted_buckets WHERE name = ?1 AND generation = ?2 AND hard_delete_time > ?3", params![name, generation, now])?
        .ok_or(CatalogError::NotFound)?;
    authorize(connection, access, &project)?;
    if select_bucket(connection, name)?.is_some() {
        return Err(CatalogError::RestoreConflict);
    }
    // The restored policy is a new IAM revision so stale policy etags cannot overwrite it.
    let revision = next_revision(connection)?;
    sql::exec(
        connection,
        "INSERT INTO bucket_policies SELECT name, bindings, ?3 FROM soft_deleted_buckets WHERE name = ?1 AND generation = ?2",
        params![name, generation, revision],
    )?;
    for query in [
        RESTORE_BUCKET,
        "INSERT INTO acl_bucket_entries (bucket, scope, entries) SELECT ?1, json_extract(value, '$.scope'), json_extract(value, '$.entries') FROM soft_deleted_buckets, json_each(acl) WHERE name = ?1 AND generation = ?2",
    ] {
        sql::exec(connection, query, params![name, generation])?;
    }
    // Sessions bound to the deleted incarnation must not resume against the restored bucket.
    sql::exec(
        connection,
        "INSERT INTO bucket_incarnations VALUES (?1, lower(hex(randomblob(16))))",
        [name],
    )?;
    // Startup reconciliation visits only live buckets, so the saved ACL still names the
    // entities of the principals trusted when the version was deleted. Re-derive it from
    // the restored policy under the current principals before the bucket is visible.
    if let Some(principals) = principals {
        reconcile_bucket_policy(connection, name, principals, now)?;
    }
    rekey_objects(connection, &retained_key(name, generation), name)?;
    sql::exec(
        connection,
        "DELETE FROM soft_deleted_buckets WHERE name = ?1 AND generation = ?2",
        params![name, generation],
    )?;
    change_journal::append(
        connection,
        change_journal::origin(access),
        change_journal::ChangeKind::BucketCreate,
        name,
        None,
    )?;
    let record = select_bucket(connection, name)?.ok_or(CatalogError::NotFound)?;
    if !record.lifecycle.is_empty() {
        super::lifecycle::rules_written();
    }
    Ok(record)
}

/// Permanently removes up to `PURGE_BATCH` expired versions with their retained objects and
/// returns how many it removed. Blob files are reclaimed through the deletion journal.
fn purge_batch(connection: &mut Connection, now: UnixMicros) -> Result<i64, CatalogError> {
    let transaction = sql::immediate(connection)?;
    let expired = transaction
        .prepare_cached(
            "SELECT name, generation FROM soft_deleted_buckets WHERE hard_delete_time <= ?1 ORDER BY hard_delete_time LIMIT ?2",
        )?
        .query_map(params![now, PURGE_BATCH], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (name, generation) in &expired {
        let key = retained_key(name, *generation);
        for query in [
            "INSERT OR IGNORE INTO pending_blob_deletions SELECT blob_id FROM objects WHERE bucket = ?1",
            "DELETE FROM objects WHERE bucket = ?1",
        ] {
            sql::exec(&transaction, query, [&key])?;
        }
        sql::exec(
            &transaction,
            "DELETE FROM soft_deleted_buckets WHERE name = ?1 AND generation = ?2",
            params![name, generation],
        )?;
    }
    transaction.commit()?;
    i64::try_from(expired.len()).map_err(unavailable)
}

fn earliest_hard_delete(connection: &Connection) -> Result<Option<UnixMicros>, CatalogError> {
    sql::scalar(
        connection,
        "SELECT min(hard_delete_time) FROM soft_deleted_buckets",
        [],
    )
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
            let transaction = connection.transaction()?;
            let now = unix_micros(clock.now())?;
            let version = sql::row(
                &transaction,
                SELECT_VERSION,
                params![name, generation, now],
                read_version,
            )
            .optional()?
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
            let mut statement = connection.prepare_cached(LIST_VERSIONS)?;
            let mut rows = statement.query(params![
                project,
                now,
                prefix,
                after_name,
                after_generation,
                limit + 1
            ])?;
            let mut items = Vec::new();
            let mut remaining = super::BUCKET_LIST_METADATA_BUDGET;
            let mut has_more = false;
            while let Some(row) = rows.next()? {
                let charge = bucket_metadata_charge(row)?;
                if items.len() == limit || (!items.is_empty() && charge > remaining) {
                    has_more = !items.is_empty();
                    break;
                }
                items.push(read_version(row)?);
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
        let principals = self.0.principals.get().cloned();
        self.run(move |connection| {
            let transaction = sql::immediate(connection)?;
            let now = unix_micros(clock.now())?;
            let record = restore_in_transaction(
                &transaction,
                &name,
                generation,
                now,
                &access,
                principals.as_ref(),
            )?;
            transaction.commit()?;
            Ok(record)
        })
        .await
    }

    /// Removes expired versions and returns the next hard delete time, for the maintenance
    /// scheduler's deadline. Reads already hide expired versions, so this only frees space; the
    /// deletion journal sweep that follows it removes their blobs, however many a version held.
    pub(super) async fn maintain_soft_deleted_buckets(
        &self,
        now: UnixMicros,
    ) -> Result<Option<SystemTime>, CatalogError> {
        self.run::<_, CatalogError>(move |connection| {
            // Every maintenance pass reaches this sweep, including passes that requests
            // wake. Taking the write lock only when a version is due keeps a pass with
            // nothing to purge from waiting on another writer and holding the connection.
            let mut next = earliest_hard_delete(connection)?;
            while next.is_some_and(|time| time <= now) {
                let removed = purge_batch(connection, now)?;
                next = earliest_hard_delete(connection)?;
                if removed == 0 {
                    break;
                }
            }
            Ok(next.map(UnixMicros::system_time).transpose()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests;
