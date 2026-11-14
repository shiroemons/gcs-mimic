//! Typed synchronization baselines; adapters never write catalog SQL.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{
    Catalog,
    admin::{AdminError, BucketName, Generation, ObjectName},
    unavailable,
};
use crate::storage::catalog::{Incarnation, bucket_incarnation, sql};
use crate::{
    bucket::CatalogError,
    object::{ObjectError, ObjectRecord},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(crate) const MAX_PAGE: usize = 256;
pub(super) const SCHEMA: &str = "CREATE TABLE catalog_identity (
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

#[cfg(test)]
mod tests;

pub mod root;

pub(crate) mod writeback;

pub(crate) mod findings;

pub(crate) mod buckets;

pub(crate) mod deletions;

pub(crate) mod authority;
pub(crate) mod conflicts;
pub(crate) mod observation;

crate::storage::opaque_id::opaque_id! {
    /// The identity of one data directory's catalog, stored as 16 raw bytes.
    pub struct CatalogId([u8; 16]);
    invalid = "invalid catalog ID";
    column = blob;
}
impl CatalogId {
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
}

crate::storage::opaque_id::opaque_id! {
    /// A synchronization operation, conflict, recovery workspace or revision identifier:
    /// 16 random bytes the admin API shows and accepts as 32 lowercase hexadecimal digits.
    pub(crate) struct OpaqueId([u8; 16]);
    invalid = "invalid synchronization identifier";
    column = text;
}
impl OpaqueId {
    pub(crate) fn generate() -> Result<Self, getrandom::Error> {
        crate::storage::opaque_id::random().map(Self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SyncFingerprint {
    pub size: u64,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
    pub inode: u64,
    pub md5: [u8; 16],
    pub crc32c: u32,
    pub racy: bool,
}
impl SyncFingerprint {
    pub(crate) fn matches_identity(&self, identity: crate::fsutil::FileIdentity) -> bool {
        self.size == identity.size
            && self.inode == identity.inode
            && self.mtime_ns == identity.modified_ns
            && self.ctime_ns == identity.changed_ns
    }
    pub(crate) fn observed(
        identity: crate::fsutil::FileIdentity,
        hashes: crate::hashing::Hashes,
        observed_ns: i128,
    ) -> Self {
        // Two seconds covers the coarsest supported HFS+/FAT timestamp granularity.
        let racy = observed_ns.saturating_sub(identity.modified_ns.max(identity.changed_ns))
            < 2_000_000_000;
        Self {
            size: identity.size,
            mtime_ns: identity.modified_ns,
            ctime_ns: identity.changed_ns,
            inode: identity.inode,
            md5: hashes.md5,
            crc32c: hashes.crc32c,
            racy,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncBinding {
    catalog: CatalogId,
    bucket: BucketName,
    incarnation: Incarnation,
    deletion: Option<deletions::Permit>,
    control: Option<authority::ControlScope>,
}
impl SyncBinding {
    /// A binding to one bucket incarnation without a deletion permit or control scope.
    pub(in crate::storage::catalog::sync_state) fn new(
        catalog: CatalogId,
        bucket: BucketName,
        incarnation: Incarnation,
    ) -> Self {
        Self {
            catalog,
            bucket,
            incarnation,
            deletion: None,
            control: None,
        }
    }

    pub(crate) fn bucket(&self) -> &BucketName {
        &self.bucket
    }

    pub(crate) fn with_control(mut self, scope: authority::ControlScope) -> Self {
        self.control = Some(scope);
        self
    }

    fn with_deletion(mut self, permit: deletions::Permit) -> Self {
        self.deletion = Some(permit);
        self
    }
}

/// A tree-relative path the catalog may track: at most 4096 bytes, no empty, `.` or `..`
/// component, and no `\`, NUL or `:`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SyncPath(String);

impl TryFrom<String> for SyncPath {
    type Error = AdminError;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        if valid_path(&path) {
            Ok(Self(path))
        } else {
            Err(AdminError::Invalid("invalid synchronization path".into()))
        }
    }
}

impl rusqlite::ToSql for SyncPath {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyncUpdate {
    binding: SyncBinding,
    path: SyncPath,
    fingerprint: SyncFingerprint,
}
impl SyncUpdate {
    pub(crate) fn new(
        binding: SyncBinding,
        path: SyncPath,
        fingerprint: SyncFingerprint,
    ) -> Result<Self, AdminError> {
        if i64::try_from(fingerprint.size).is_err() {
            return Err(AdminError::Invalid(
                "invalid synchronization path or size".into(),
            ));
        }
        Ok(Self {
            binding,
            path,
            fingerprint,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncRecord {
    pub name: String,
    pub path: String,
    pub generation: Generation,
    pub(crate) fingerprint: SyncFingerprint,
}

#[derive(Clone)]
pub struct SyncState(Catalog);

/// The tracked paths of `bucket`, from `bucket/` up to `bucket0`: '0' sorts right after '/'.
fn bucket_paths(bucket: &str) -> (String, String) {
    (format!("{bucket}/"), format!("{bucket}0"))
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains(['\\', '\0', ':'])
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration step"
)]
pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SCHEMA)?;
    let mut id = [0; 16];
    getrandom::fill(&mut id).map_err(unavailable)?;
    transaction.execute("INSERT INTO catalog_identity VALUES (1, ?1)", [id])?;
    transaction.commit().map_err(unavailable)
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    sql::validate(
        connection,
        "SELECT count(*) = 1 AND min(singleton) = 1 FROM catalog_identity",
        "invalid catalog identity",
    )?;
    catalog_id(connection)?;
    Ok(())
}

fn catalog_id(connection: &Connection) -> Result<CatalogId, CatalogError> {
    sql::scalar(
        connection,
        "SELECT id FROM catalog_identity WHERE singleton = 1",
        [],
    )
    .map_err(unavailable)
}

/// The change-journal fence while the journal is enabled; a disabled journal has no row.
fn enabled_fence(
    connection: &Connection,
) -> rusqlite::Result<crate::storage::change_journal::JournalFence> {
    sql::row(
        connection,
        "SELECT sequence,epoch FROM change_journal_state WHERE singleton=1 AND enabled=1",
        [],
        |row| {
            Ok(crate::storage::change_journal::JournalFence {
                sequence: row.get(0)?,
                revision: row.get(1)?,
            })
        },
    )
}

pub(super) fn matching(
    connection: &Connection,
    binding: &SyncBinding,
    bucket: &str,
) -> Result<(), ObjectError> {
    if let Some(scope) = &binding.control {
        scope.authorize(connection).map_err(ObjectError::from)?;
    }
    let incarnation = bucket_incarnation(connection, bucket).optional()?;
    if binding.catalog != catalog_id(connection)?
        || binding.bucket.as_str() != bucket
        || incarnation.as_ref() != Some(&binding.incarnation)
    {
        return Err(ObjectError::PreconditionFailed);
    }
    Ok(())
}

pub(super) fn record_in_transaction(
    transaction: &Transaction<'_>,
    update: &SyncUpdate,
    record: &ObjectRecord,
) -> Result<(), ObjectError> {
    matching(transaction, &update.binding, &record.bucket)?;
    let fp = &update.fingerprint;
    if fp.size != record.size || fp.md5 != record.md5 || fp.crc32c != record.crc32c {
        return Err(ObjectError::ChecksumMismatch);
    }
    let occupied: bool = sql::scalar(
        transaction,
        "SELECT EXISTS(SELECT 1 FROM sync_objects WHERE path = ?1 AND (bucket != ?2 OR name != ?3))",
        params![update.path, record.bucket, record.name],
    )?;
    if occupied {
        return Err(ObjectError::PreconditionFailed);
    }
    sql::exec(transaction,
        "INSERT INTO sync_objects (bucket,name,incarnation,path,generation,size,mtime_ns,ctime_ns,inode,md5,crc32c,racy)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(bucket,name) DO UPDATE SET incarnation=excluded.incarnation,path=excluded.path,
         generation=excluded.generation,size=excluded.size,mtime_ns=excluded.mtime_ns,ctime_ns=excluded.ctime_ns,
         inode=excluded.inode,md5=excluded.md5,crc32c=excluded.crc32c,racy=excluded.racy",
        params![record.bucket,record.name,update.binding.incarnation,update.path,record.generation,fp.size,
            fp.mtime_ns.to_be_bytes(),fp.ctime_ns.to_be_bytes(),fp.inode.to_be_bytes(),fp.md5,fp.crc32c,fp.racy],
    )?;
    if let Some(scope) = &update.binding.control {
        sql::exec(
            transaction,
            "DELETE FROM sync_findings WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4",
            params![
                scope.root_id(),
                record.bucket,
                record.name,
                update.binding.incarnation
            ],
        )?;
    }
    Ok(())
}

pub(super) fn delete_in_transaction(
    transaction: &Transaction<'_>,
    binding: &SyncBinding,
    record: &ObjectRecord,
) -> Result<(), ObjectError> {
    matching(transaction, binding, &record.bucket)?;
    if let Some(permit) = &binding.deletion {
        deletions::consume(transaction, permit, record)?;
    }
    let count = sql::exec(
        transaction,
        "DELETE FROM sync_objects WHERE bucket = ?1 AND name = ?2 AND incarnation = ?3 AND generation = ?4",
        params![
            record.bucket,
            record.name,
            binding.incarnation,
            record.generation
        ],
    )?;
    if count != 1 {
        return Err(ObjectError::PreconditionFailed);
    }
    Ok(())
}

// A literal macro keeps the statements below static for the prepared statement cache.
macro_rules! sync_columns {
    () => {
        "name,path,generation,size,mtime_ns,ctime_ns,inode,md5,crc32c,racy"
    };
}
const SELECT_BY_PATH: &str = concat!(
    "SELECT ",
    sync_columns!(),
    " FROM sync_objects WHERE path = ?1 AND bucket = ?2 AND incarnation = ?3"
);
const SELECT_PAGE: &str = concat!(
    "SELECT ",
    sync_columns!(),
    " FROM sync_objects INDEXED BY sync_objects_path WHERE path > ?1 AND bucket = ?2 AND incarnation = ?3 AND path < ?5 ORDER BY path LIMIT ?4"
);
const SELECT_BY_NAME: &str = concat!(
    "SELECT ",
    sync_columns!(),
    " FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?3"
);
fn read_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncRecord> {
    Ok(SyncRecord {
        name: row.get(0)?,
        path: row.get(1)?,
        generation: row.get(2)?,
        fingerprint: SyncFingerprint {
            size: row.get(3)?,
            mtime_ns: i128::from_be_bytes(row.get(4)?),
            ctime_ns: i128::from_be_bytes(row.get(5)?),
            inode: u64::from_be_bytes(row.get(6)?),
            md5: row.get(7)?,
            crc32c: row.get(8)?,
            racy: row.get(9)?,
        },
    })
}

impl Catalog {
    pub fn sync_state(&self) -> SyncState {
        SyncState(self.clone())
    }
}
impl SyncState {
    pub(crate) async fn catalog_id(&self) -> Result<CatalogId, AdminError> {
        self.0.run_admin(|connection| catalog_id(connection)).await
    }

    pub async fn binding(&self, bucket: &BucketName) -> Result<SyncBinding, AdminError> {
        let bucket = bucket.clone();
        self.0
            .run_admin(move |connection| {
                let incarnation = bucket_incarnation(connection, bucket.as_str())
                    .optional()?
                    .ok_or(CatalogError::NotFound)?;
                Ok(SyncBinding::new(
                    catalog_id(connection)?,
                    bucket,
                    incarnation,
                ))
            })
            .await
    }

    /// Call only after durable file/parent writes; generation and hashes are rechecked atomically.
    pub(crate) async fn acknowledge(
        &self,
        name: &ObjectName,
        generation: Generation,
        update: SyncUpdate,
    ) -> Result<(), AdminError> {
        let name = name.as_str().to_owned();
        self.0
            .run_admin_with_error(move |connection| {
                let transaction = connection
                    .transaction()
                    .map_err(super::objects::object_unavailable)?;
                matching(
                    &transaction,
                    &update.binding,
                    update.binding.bucket.as_str(),
                )?;
                let record =
                    super::objects::lookup(&transaction, update.binding.bucket.as_str(), &name)?
                        .ok_or(ObjectError::ObjectNotFound)?
                        .record;
                if record.generation != generation.get() {
                    return Err(ObjectError::PreconditionFailed.into());
                }
                record_in_transaction(&transaction, &update, &record)?;
                transaction
                    .commit()
                    .map_err(super::objects::object_unavailable)
                    .map_err(AdminError::from)
            })
            .await
    }

    pub async fn get_path(
        &self,
        binding: SyncBinding,
        path: String,
    ) -> Result<Option<SyncRecord>, AdminError> {
        let path = SyncPath::try_from(path)?;
        self.0
            .run_admin_with_error(move |connection| {
                matching(connection, &binding, binding.bucket.as_str())?;
                sql::row(
                    connection,
                    SELECT_BY_PATH,
                    params![path, binding.bucket, binding.incarnation],
                    read_record,
                )
                .optional()
                .map_err(super::objects::object_unavailable)
                .map_err(AdminError::from)
            })
            .await
    }

    /// Continue after the last returned path; an empty page terminates traversal.
    pub(crate) async fn page(
        &self,
        binding: SyncBinding,
        after: Option<SyncPath>,
        limit: usize,
    ) -> Result<Vec<SyncRecord>, AdminError> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(AdminError::Invalid("invalid synchronization page".into()));
        }
        let (start, end) = bucket_paths(binding.bucket.as_str());
        let after = match after {
            Some(after) => after.0.max(start),
            None => start,
        };
        self.0
            .run_admin_with_error(move |connection| {
                matching(connection, &binding, binding.bucket.as_str())?;
                connection
                    .prepare_cached(SELECT_PAGE)
                    .map_err(super::objects::object_unavailable)?
                    .query_map(
                        params![after, binding.bucket, binding.incarnation, limit, end],
                        read_record,
                    )
                    .map_err(super::objects::object_unavailable)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(super::objects::object_unavailable)
                    .map_err(AdminError::from)
            })
            .await
    }
}
