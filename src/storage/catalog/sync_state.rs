//! Typed synchronization baselines; adapters never write catalog SQL.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{
    Catalog,
    admin::{AdminError, BucketName, Generation, ObjectName},
    unavailable,
};
use crate::{
    bucket::CatalogError,
    object::{ObjectError, ObjectRecord},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub const MAX_PAGE: usize = 256;
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

pub mod writeback;

pub mod findings;

pub(crate) mod buckets;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogId([u8; 16]);
impl CatalogId {
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}
impl std::fmt::Display for CatalogId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::hashing::encode_lower(&self.0))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncFingerprint {
    pub size: u64,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
    pub inode: u64,
    pub md5: [u8; 16],
    pub crc32c: u32,
    pub racy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncBinding {
    catalog: CatalogId,
    bucket: String,
    incarnation: String,
}
impl SyncBinding {
    pub fn catalog_id(&self) -> CatalogId {
        self.catalog
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncUpdate {
    binding: SyncBinding,
    path: String,
    fingerprint: SyncFingerprint,
}
impl SyncUpdate {
    pub fn new(
        binding: SyncBinding,
        path: String,
        fingerprint: SyncFingerprint,
    ) -> Result<Self, AdminError> {
        if !valid_path(&path) || fingerprint.size > i64::MAX as u64 {
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
    pub fingerprint: SyncFingerprint,
}

#[derive(Clone)]
pub struct SyncState(Catalog);

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains(['\\', '\0', ':'])
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    let mut id = [0; 16];
    getrandom::fill(&mut id).map_err(unavailable)?;
    transaction
        .execute("INSERT INTO catalog_identity VALUES (1, ?1)", [id])
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection
        .query_row(
            "SELECT count(*) = 1 AND min(singleton) = 1 FROM catalog_identity",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !valid {
        return Err(unavailable("invalid catalog identity"));
    }
    catalog_id(connection)?;
    Ok(())
}

fn catalog_id(connection: &Connection) -> Result<CatalogId, CatalogError> {
    connection
        .query_row(
            "SELECT id FROM catalog_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map(CatalogId)
        .map_err(unavailable)
}

pub(super) fn matching(
    connection: &Connection,
    binding: &SyncBinding,
    bucket: &str,
) -> Result<(), ObjectError> {
    let incarnation: Option<String> = connection
        .query_row(
            "SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1",
            [bucket],
            |row| row.get(0),
        )
        .optional()
        .map_err(super::objects::object_unavailable)?;
    if binding.catalog != catalog_id(connection).map_err(super::objects::object_unavailable)?
        || binding.bucket != bucket
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
    let occupied: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_objects WHERE path = ?1 AND (bucket != ?2 OR name != ?3))",
        params![update.path,record.bucket,record.name], |row| row.get(0),
    ).map_err(super::objects::object_unavailable)?;
    if occupied {
        return Err(ObjectError::PreconditionFailed);
    }
    transaction.execute(
        "INSERT INTO sync_objects (bucket,name,incarnation,path,generation,size,mtime_ns,ctime_ns,inode,md5,crc32c,racy)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(bucket,name) DO UPDATE SET incarnation=excluded.incarnation,path=excluded.path,
         generation=excluded.generation,size=excluded.size,mtime_ns=excluded.mtime_ns,ctime_ns=excluded.ctime_ns,
         inode=excluded.inode,md5=excluded.md5,crc32c=excluded.crc32c,racy=excluded.racy",
        params![record.bucket,record.name,update.binding.incarnation,update.path,record.generation,fp.size as i64,
            fp.mtime_ns.to_be_bytes(),fp.ctime_ns.to_be_bytes(),fp.inode.to_be_bytes(),fp.md5,fp.crc32c,fp.racy],
    ).map_err(super::objects::object_unavailable)?;
    Ok(())
}

pub(super) fn delete_in_transaction(
    transaction: &Transaction<'_>,
    binding: &SyncBinding,
    record: &ObjectRecord,
) -> Result<(), ObjectError> {
    matching(transaction, binding, &record.bucket)?;
    let count = transaction.execute(
        "DELETE FROM sync_objects WHERE bucket = ?1 AND name = ?2 AND incarnation = ?3 AND generation = ?4",
        params![record.bucket,record.name,binding.incarnation,record.generation],
    ).map_err(super::objects::object_unavailable)?;
    if count != 1 {
        return Err(ObjectError::PreconditionFailed);
    }
    Ok(())
}

const COLUMNS: &str = "name,path,generation,size,mtime_ns,ctime_ns,inode,md5,crc32c,racy";
fn read_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncRecord> {
    let generation: i64 = row.get(2)?;
    Ok(SyncRecord {
        name: row.get(0)?,
        path: row.get(1)?,
        generation: Generation::new(generation)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, generation))?,
        fingerprint: SyncFingerprint {
            size: row.get::<_, i64>(3)? as u64,
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
    pub async fn catalog_id(&self) -> Result<CatalogId, AdminError> {
        self.0.run_admin(|connection| catalog_id(connection)).await
    }

    pub async fn binding(&self, bucket: &BucketName) -> Result<SyncBinding, AdminError> {
        let bucket = bucket.as_str().to_owned();
        self.0
            .run_admin(move |connection| {
                let incarnation = connection
                    .query_row(
                        "SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1",
                        [&bucket],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(unavailable)?
                    .ok_or(CatalogError::NotFound)?;
                Ok(SyncBinding {
                    catalog: catalog_id(connection)?,
                    bucket,
                    incarnation,
                })
            })
            .await
    }

    /// Call only after durable file/parent writes; generation and hashes are rechecked atomically.
    pub async fn acknowledge(
        &self,
        name: &ObjectName,
        generation: Generation,
        update: SyncUpdate,
    ) -> Result<(), AdminError> {
        let name = name.as_str().to_owned();
        self.0
            .run_admin(move |connection| {
                Ok((|| {
                    let transaction = connection
                        .transaction()
                        .map_err(super::objects::object_unavailable)?;
                    matching(&transaction, &update.binding, &update.binding.bucket)?;
                    let record =
                        super::objects::lookup(&transaction, &update.binding.bucket, &name)?
                            .ok_or(ObjectError::ObjectNotFound)?
                            .0;
                    if record.generation != generation.get() {
                        return Err(ObjectError::PreconditionFailed);
                    }
                    record_in_transaction(&transaction, &update, &record)?;
                    transaction
                        .commit()
                        .map_err(super::objects::object_unavailable)
                })())
            })
            .await?
            .map_err(AdminError::from)
    }

    pub async fn get_path(
        &self,
        binding: SyncBinding,
        path: String,
    ) -> Result<Option<SyncRecord>, AdminError> {
        if !valid_path(&path) {
            return Err(AdminError::Invalid("invalid synchronization path".into()));
        }
        self.0.run_admin(move |connection| {
            Ok((|| {
                matching(connection, &binding, &binding.bucket)?;
                connection.query_row(&format!("SELECT {COLUMNS} FROM sync_objects WHERE path = ?1 AND bucket = ?2 AND incarnation = ?3"),
                    params![path,binding.bucket,binding.incarnation], read_record).optional().map_err(super::objects::object_unavailable)
            })())
        }).await?.map_err(AdminError::from)
    }

    /// Continue after the last returned path; an empty page terminates traversal.
    pub async fn page(
        &self,
        binding: SyncBinding,
        after: String,
        limit: usize,
    ) -> Result<Vec<SyncRecord>, AdminError> {
        if !(1..=MAX_PAGE).contains(&limit) || (!after.is_empty() && !valid_path(&after)) {
            return Err(AdminError::Invalid("invalid synchronization page".into()));
        }
        self.0.run_admin(move |connection| {
            Ok((|| {
                matching(connection, &binding, &binding.bucket)?;
                connection.prepare(&format!("SELECT {COLUMNS} FROM sync_objects INDEXED BY sync_objects_path WHERE path > ?1 AND bucket = ?2 AND incarnation = ?3 ORDER BY path LIMIT ?4"))
                    .map_err(super::objects::object_unavailable)?
                    .query_map(params![after,binding.bucket,binding.incarnation,limit as i64], read_record)
                    .map_err(super::objects::object_unavailable)?.collect::<rusqlite::Result<Vec<_>>>().map_err(super::objects::object_unavailable)
            })())
        }).await?.map_err(AdminError::from)
    }
}
