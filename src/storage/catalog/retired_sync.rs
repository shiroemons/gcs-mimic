//! The schema steps that released directory synchronization added to the catalog. Synchronization
//! was removed, so nothing reads or writes its tables any more; the steps stay so that a data
//! directory from any release up to 0.27.0 still upgrades and validates, and its tables stay as
//! that release left them until a later schema retires them. Released schema text is frozen
//! history: a change here breaks the validation of catalogs that ran it.
//!
//! The triggers these steps create fire only on the synchronization tables themselves, so API
//! writes to buckets and objects never touch them.

use super::{CatalogError, sql, unavailable};
use rusqlite::Connection;

#[cfg(test)]
mod tests;

/// Schema 17: the catalog's identity and the files synchronization tracked.
pub(super) const IDENTITY_SCHEMA: &str = "CREATE TABLE catalog_identity (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    id BLOB NOT NULL CHECK(typeof(id) = 'blob' AND length(id) = 16)
) WITHOUT ROWID;
CREATE TABLE sync_objects (
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL,
    path TEXT NOT NULL COLLATE BINARY CHECK(length(path) BETWEEN 1 AND 4096),
    generation INTEGER NOT NULL CHECK(typeof(generation) = 'integer' AND generation > 0),
    size INTEGER NOT NULL CHECK(typeof(size) = 'integer' AND size >= 0),
    mtime_ns BLOB NOT NULL CHECK(typeof(mtime_ns) = 'blob' AND length(mtime_ns) = 16),
    ctime_ns BLOB NOT NULL CHECK(typeof(ctime_ns) = 'blob' AND length(ctime_ns) = 16),
    inode BLOB NOT NULL CHECK(typeof(inode) = 'blob' AND length(inode) = 8),
    md5 BLOB NOT NULL CHECK(typeof(md5) = 'blob' AND length(md5) = 16),
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    racy INTEGER NOT NULL CHECK(racy IN (0, 1)),
    PRIMARY KEY(bucket, name)
) WITHOUT ROWID;
CREATE UNIQUE INDEX sync_objects_path ON sync_objects(path);
PRAGMA user_version = 17;";

/// Schema 19: the synchronized directory roots.
pub(super) const ROOT_SCHEMA: &str = "CREATE TABLE sync_roots (
    root_id BLOB PRIMARY KEY CHECK(typeof(root_id) = 'blob' AND length(root_id) = 16),
    location BLOB NOT NULL UNIQUE CHECK(typeof(location) = 'blob'),
    device BLOB NOT NULL CHECK(typeof(device) = 'blob' AND length(device) = 8),
    inode BLOB NOT NULL CHECK(typeof(inode) = 'blob' AND length(inode) = 8),
    complete INTEGER NOT NULL CHECK(complete IN (0, 1)),
    UNIQUE(device, inode)
) WITHOUT ROWID;
PRAGMA user_version = 19;";

/// Schema 20: the write-back of objects into a root.
pub(super) const WRITEBACK_SCHEMA: &str = "CREATE TABLE sync_writeback_roots (
    root_id BLOB PRIMARY KEY REFERENCES sync_roots(root_id),
    max_bytes INTEGER NOT NULL CHECK(max_bytes BETWEEN 1 AND 1099511627776),
    max_entries INTEGER NOT NULL CHECK(max_entries BETWEEN 1 AND 1000000),
    reserved BLOB NOT NULL CHECK(typeof(reserved) = 'blob' AND length(reserved) = 16),
    retained BLOB NOT NULL CHECK(typeof(retained) = 'blob' AND length(retained) = 16),
    entries INTEGER NOT NULL CHECK(entries BETWEEN 0 AND 1000000),
    protected INTEGER NOT NULL CHECK(protected BETWEEN 0 AND entries),
    recovery INTEGER NOT NULL CHECK(recovery IN (0,1)),
    io_failure INTEGER NOT NULL CHECK(io_failure IN (0,1)),
    workspace_device BLOB NOT NULL CHECK(typeof(workspace_device) = 'blob' AND length(workspace_device) = 8),
    workspace_inode BLOB NOT NULL CHECK(typeof(workspace_inode) = 'blob' AND length(workspace_inode) = 8),
    sequence INTEGER CHECK(sequence >= 0),
    epoch INTEGER CHECK(epoch >= 0),
    CHECK((sequence IS NULL) = (epoch IS NULL))
) WITHOUT ROWID;
CREATE TABLE sync_writeback_ops (
    ordinal INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE CHECK(length(id) = 32 AND id NOT GLOB '*[^0-9a-f]*'),
    root_id BLOB NOT NULL REFERENCES sync_writeback_roots(root_id),
    owner TEXT NOT NULL CHECK(length(owner) BETWEEN 1 AND 256),
    bucket TEXT NOT NULL CHECK(length(bucket) BETWEEN 1 AND 222),
    incarnation TEXT NOT NULL CHECK(length(incarnation) = 32),
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 1024),
    path TEXT NOT NULL CHECK(length(path) BETWEEN 1 AND 4096),
    generation INTEGER NOT NULL CHECK(generation > 0),
    kind INTEGER NOT NULL CHECK(kind IN (0,1)),
    phase INTEGER NOT NULL CHECK(phase BETWEEN 0 AND 4),
    revision INTEGER NOT NULL CHECK(revision > 0),
    io_failure INTEGER NOT NULL DEFAULT 0 CHECK(io_failure BETWEEN 0 AND 3),
    fence_sequence INTEGER NOT NULL CHECK(fence_sequence >= 0),
    fence_epoch INTEGER NOT NULL CHECK(fence_epoch >= 0),
    reservation BLOB NOT NULL CHECK(typeof(reservation) = 'blob' AND length(reservation) = 8),
    retained BLOB CHECK(retained IS NULL OR (typeof(retained) = 'blob' AND length(retained) = 8)),
    size INTEGER NOT NULL CHECK(size >= 0),
    md5 BLOB NOT NULL CHECK(typeof(md5) = 'blob' AND length(md5) = 16),
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    snapshot BLOB CHECK(snapshot IS NULL OR length(snapshot) <= 4096)
);
CREATE UNIQUE INDEX sync_writeback_target ON sync_writeback_ops(root_id,path) WHERE phase != 4;
CREATE INDEX sync_writeback_page ON sync_writeback_ops(root_id,ordinal) WHERE phase != 4;
CREATE INDEX sync_writeback_phase ON sync_writeback_ops(root_id,phase);
PRAGMA user_version = 20;";

/// Schema 21: the objects a root could not hold.
pub(super) const FINDINGS_SCHEMA: &str = "CREATE TABLE sync_findings (
    ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
    root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    reason TEXT NOT NULL CHECK(reason IN ('invalid-name','host-restriction','reserved','ignored','collision','unsafe-entry')),
    UNIQUE(root_id,bucket,name)
);
CREATE INDEX sync_findings_page ON sync_findings(root_id,ordinal);
PRAGMA user_version = 21;";

/// Schema 27: the bucket incarnations a root mapped.
pub(super) const BUCKETS_SCHEMA: &str = "CREATE TABLE sync_buckets (
    root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
    bucket TEXT NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL CHECK(length(incarnation) = 32),
    PRIMARY KEY(root_id,bucket)
) WITHOUT ROWID;
PRAGMA user_version = 27;";

/// Schema 40: the deletion sets waiting for a decision, and the owner of each finding, which the
/// step derives from the bucket incarnation the finding names.
pub(super) const DELETIONS_SCHEMA: &str = "CREATE TABLE sync_deletion_sets (
 root_id BLOB PRIMARY KEY REFERENCES sync_roots(root_id),
 revision TEXT NOT NULL UNIQUE CHECK(length(revision)=32),
 scan_revision TEXT NOT NULL CHECK(length(scan_revision)=32),
 sequence INTEGER NOT NULL CHECK(sequence>=0),
 epoch INTEGER NOT NULL CHECK(epoch>=0),
 execution_sequence INTEGER NOT NULL CHECK(execution_sequence>=sequence),
 baseline_count INTEGER NOT NULL CHECK(baseline_count>=0),
 candidate_count INTEGER NOT NULL CHECK(candidate_count BETWEEN 0 AND baseline_count),
 guard_enabled INTEGER NOT NULL CHECK(guard_enabled IN (0,1)),
 guarded INTEGER NOT NULL CHECK(guarded IN (0,1)),
 complete INTEGER NOT NULL CHECK(complete IN (0,1)),
 valid INTEGER NOT NULL CHECK(valid IN (0,1)),
 authorization INTEGER NOT NULL CHECK(authorization IN (0,1,2))
) WITHOUT ROWID;
CREATE TABLE sync_deletion_members (
 root_id BLOB NOT NULL REFERENCES sync_deletion_sets(root_id) ON DELETE CASCADE,
 ordinal INTEGER NOT NULL CHECK(ordinal>0),
 bucket TEXT NOT NULL COLLATE BINARY,
 name TEXT NOT NULL COLLATE BINARY,
 owner_project TEXT NOT NULL,
 incarnation TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation>0),
 path TEXT NOT NULL COLLATE BINARY,
 done INTEGER NOT NULL CHECK(done IN (0,1)),
 PRIMARY KEY(root_id,ordinal),UNIQUE(root_id,bucket,name)
) WITHOUT ROWID;
ALTER TABLE sync_findings ADD COLUMN owner_project TEXT CHECK(owner_project IS NULL OR length(owner_project) BETWEEN 1 AND 256);
UPDATE sync_findings SET owner_project=(SELECT b.project FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=sync_findings.bucket AND i.incarnation=sync_findings.incarnation);
CREATE TABLE sync_finding_owners (
 ordinal INTEGER NOT NULL REFERENCES sync_findings(ordinal) ON DELETE CASCADE,
 incarnation TEXT NOT NULL CHECK(length(incarnation)=32),
 owner_project TEXT CHECK(owner_project IS NULL OR length(owner_project) BETWEEN 1 AND 256),
 PRIMARY KEY(ordinal,incarnation)
) WITHOUT ROWID;
INSERT INTO sync_finding_owners SELECT ordinal,incarnation,owner_project FROM sync_findings;
PRAGMA user_version=40;";

/// Schema 41: what the synchronization status showed, with one observation row per root.
pub(super) const OBSERVATION_SCHEMA: &str = include_str!("retired_sync/observation.sql");

/// Schema 42: the user edits a write-back kept aside and the recovery workspaces.
pub(super) const CONFLICTS_SCHEMA: &str = include_str!("retired_sync/conflicts.sql");

/// Schema 50: how many deletions a root carried out without a decision, with one row per root.
pub(super) const DELETION_HISTORY_SCHEMA: &str = "CREATE TABLE sync_deletion_history (
 root_id BLOB PRIMARY KEY REFERENCES sync_roots(root_id) ON DELETE CASCADE,
 automatic_count INTEGER NOT NULL DEFAULT 0 CHECK(typeof(automatic_count)='integer' AND automatic_count>=0)
) WITHOUT ROWID;
INSERT INTO sync_deletion_history(root_id) SELECT root_id FROM sync_roots;
CREATE TRIGGER sync_deletion_history_root_insert AFTER INSERT ON sync_roots BEGIN
 INSERT INTO sync_deletion_history(root_id) VALUES(NEW.root_id);
END;
CREATE TRIGGER sync_deletion_history_baseline AFTER UPDATE OF complete ON sync_roots
WHEN OLD.complete=0 AND NEW.complete=1 BEGIN
 UPDATE sync_deletion_history SET automatic_count=0 WHERE root_id=NEW.root_id;
END;
PRAGMA user_version=50;";

/// The tables the steps above create, which a released catalog may still hold rows in. Reset,
/// purge and bucket deletion leave them as they are.
#[cfg(test)]
pub(super) const TABLES: &[&str] = &[
    "sync_buckets",
    "sync_conflict_operations",
    "sync_conflict_roots",
    "sync_conflicts",
    "sync_deletion_history",
    "sync_deletion_members",
    "sync_deletion_sets",
    "sync_finding_owners",
    "sync_findings",
    "sync_objects",
    "sync_observations",
    "sync_path_stops",
    "sync_recovery_workspaces",
    "sync_roots",
    "sync_writeback_ops",
    "sync_writeback_roots",
];

/// Upgrades to schema 17, giving the catalog a random identity in the same transaction.
#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration step"
)]
pub(super) fn migrate_identity(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(IDENTITY_SCHEMA)?;
    let mut id = [0; 16];
    getrandom::fill(&mut id).map_err(unavailable)?;
    transaction.execute("INSERT INTO catalog_identity VALUES (1, ?1)", [id])?;
    transaction.commit().map_err(unavailable)
}

#[cfg(test)]
impl super::Catalog {
    /// The identity schema 17 gave the catalog, which reset, reseed and reopening keep.
    pub(crate) async fn identity(&self) -> Result<[u8; 16], super::admin::AdminError> {
        self.run_admin(|connection| {
            sql::scalar(
                connection,
                "SELECT id FROM catalog_identity WHERE singleton = 1",
                [],
            )
            .map_err(unavailable)
        })
        .await
    }
}

/// Checks that a catalog from schema 17 on holds exactly one 16-byte identity.
pub(super) fn validate_identity(connection: &Connection) -> Result<(), CatalogError> {
    sql::validate(
        connection,
        "SELECT count(*) = 1 AND min(singleton) = 1 FROM catalog_identity",
        "invalid catalog identity",
    )?;
    sql::scalar::<[u8; 16]>(
        connection,
        "SELECT id FROM catalog_identity WHERE singleton = 1",
        [],
    )
    .map_err(unavailable)?;
    Ok(())
}
