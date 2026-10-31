//! Owned conflict copies and exact, durable operator resolution.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{
    SyncBinding, SyncFingerprint,
    authority::ControlAuthority,
    root::{RootError, RootLease},
};
use crate::{
    bucket::CatalogError,
    fsutil::{self, FileIdentity},
    hashing::{Hasher, Hashes},
    object::{ObjectError, ObjectRecord},
    storage::{
        admin::{AdminError, Generation, ObjectName, WAIT_TIMEOUT},
        catalog::{objects, unavailable},
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::{fs::File, io, path::PathBuf};

mod capture;
mod fault;
mod list;
mod recovery;
mod resolve;
#[cfg(all(test, unix))]
mod tests;
mod transfer;

pub(in crate::storage) const SCHEMA: &str = include_str!("conflicts/schema.sql");
pub(crate) const MAX_BYTES: u64 = 1 << 30;
pub(crate) const MAX_ENTRIES: u64 = 1000;
#[cfg(all(test, unix))]
use list::ConflictPage;
pub(in crate::storage) use list::usage;
pub(crate) use list::{Conflict, Usage};
pub(crate) use recovery::Recovery;
pub(crate) use resolve::{Action, Confirmation, ResolveOutcome, ResolveRequest, ResolveTarget};
pub(crate) use resolve::{ResolutionBinding, complete_restore, validate_restore};
pub(in crate::storage) use transfer::captured_entries;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Admin(AdminError),
    Root(RootError),
    Capacity,
    RevisionMismatch,
    TargetChanged,
    OperationIdReused,
    ConfirmationRequired,
    InvalidResolution,
}
impl From<AdminError> for Error {
    fn from(error: AdminError) -> Self {
        Self::Admin(error)
    }
}
impl From<RootError> for Error {
    fn from(error: RootError) -> Self {
        Self::Root(error)
    }
}
impl From<CatalogError> for Error {
    fn from(error: CatalogError) -> Self {
        Self::Admin(error.into())
    }
}
impl From<ObjectError> for Error {
    fn from(error: ObjectError) -> Self {
        Self::Admin(error.into())
    }
}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::from(unavailable(error))
    }
}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Admin(error.into())
    }
}
impl From<fsutil::FsError> for Error {
    fn from(error: fsutil::FsError) -> Self {
        match error {
            fsutil::FsError::Io(error) => Self::from(error),
            fsutil::FsError::Unsupported => Self::Admin(AdminError::Io(io::ErrorKind::Unsupported)),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sync conflict: {self:?}")
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Admin(error) => Some(error),
            Self::Root(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CaptureRequest {
    pub binding: SyncBinding,
    pub name: ObjectName,
    pub expected_generation: Option<Generation>,
    pub source_op: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CapturedConflict {
    root: [u8; 16],
    id: String,
    revision: String,
    binding: SyncBinding,
    name: ObjectName,
    expected_generation: Option<Generation>,
    fingerprint: SyncFingerprint,
    device: u64,
}

pub(crate) struct CaptureProof {
    pub fingerprint: SyncFingerprint,
    pub device: u64,
}

impl CapturedConflict {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    pub(in crate::storage) fn validate(
        &self,
        tx: &Transaction<'_>,
        root: [u8; 16],
        binding: &SyncBinding,
        name: &ObjectName,
        expected_generation: Option<Generation>,
    ) -> Result<CaptureProof, ObjectError> {
        if self.root != root
            || binding.catalog != self.binding.catalog
            || binding.bucket != self.binding.bucket
            || binding.incarnation != self.binding.incarnation
            || name != &self.name
            || expected_generation != self.expected_generation
        {
            return Err(ObjectError::PreconditionFailed);
        }
        super::matching(tx, binding, &binding.bucket)?;
        let valid: bool = tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_conflicts WHERE root_id=?1 AND id=?2 AND revision=?3 AND state=1)")
            .and_then(|mut statement| statement.query_row(params![self.root,self.id,self.revision], |row| row.get(0)))
            .map_err(objects::object_unavailable)?;
        if !valid {
            return Err(ObjectError::PreconditionFailed);
        }
        Ok(CaptureProof {
            fingerprint: self.fingerprint.clone(),
            device: self.device,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Observation {
    device: u64,
    inode: u64,
    size: u64,
    modified_ns: i128,
    changed_ns: i128,
    hashes: Option<StoredHashes>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StoredHashes {
    md5: [u8; 16],
    crc32c: u32,
}

impl Observation {
    fn new(identity: FileIdentity, hashes: Option<Hashes>) -> Self {
        Self {
            device: identity.device,
            inode: identity.inode,
            size: identity.size,
            modified_ns: identity.modified_ns,
            changed_ns: identity.changed_ns,
            hashes: hashes.map(|h| StoredHashes {
                md5: h.md5,
                crc32c: h.crc32c,
            }),
        }
    }
    fn matches(&self, identity: FileIdentity) -> bool {
        self.device == identity.device
            && self.inode == identity.inode
            && self.size == identity.size
            && self.modified_ns == identity.modified_ns
            && self.changed_ns == identity.changed_ns
    }
    fn encode(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(self).map_err(|e| AdminError::backend(e).into())
    }
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        serde_json::from_slice(bytes).map_err(|e| AdminError::backend(e).into())
    }
}

fn opaque_id() -> Result<String, Error> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(AdminError::backend)?;
    Ok(crate::hashing::encode_lower(&bytes))
}
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn changed(connection: &Connection, root: [u8; 16]) -> Result<(), Error> {
    connection.prepare_cached("INSERT INTO sync_conflict_roots(root_id,revision) VALUES(?1,?2) ON CONFLICT(root_id) DO UPDATE SET revision=excluded.revision")?.execute(params![root,opaque_id()?])?;
    Ok(())
}
fn data_path(lease: &RootLease) -> PathBuf {
    lease.catalog().0.data_dir.clone()
}

impl RootLease {
    #[cfg(test)]
    async fn conflict_boundary(&self, step: fault::Step) -> Result<(), Error> {
        let data = data_path(self);
        let tree = self.fault_tree();
        self.conflict_files(move |_| {
            fault::hit(&tree, step)?;
            fault::hit(&data, step)
        })
        .await
    }
    async fn conflict_db<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Connection) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let lease = self.clone();
        self.catalog()
            .run_admin(move |connection| {
                lease.control_scope().authorize(connection)?;
                let _lease = lease;
                Ok(job(connection))
            })
            .await?
    }
    async fn conflict_serial<T: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = Result<T, Error>> + Send + 'static,
    ) -> Result<T, Error> {
        let permit = self
            .catalog()
            .wait_admin(
                self.catalog().0.writeback_execution.clone(),
                tokio::time::Instant::now() + WAIT_TIMEOUT,
            )
            .await?;
        crate::task::spawn_supervised("sync-conflict", async move {
            let _permit = permit;
            future.await
        })
        .await
        .map_err(AdminError::backend)?
    }
    async fn conflict_files<T: Send + 'static>(
        &self,
        job: impl FnOnce(&File) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        self.file_job(move |root| Ok(job(root))).await?
    }
}

#[derive(Clone, Copy)]
enum HashMode {
    Metadata,
    Body,
}

fn observe(file: &File, hash: HashMode) -> Result<Observation, Error> {
    let before = fsutil::stat_identity(file)?;
    let hashes = if matches!(hash, HashMode::Body) {
        let mut hasher = Hasher::default();
        let mut offset = 0;
        let mut buffer = [0; 65_536];
        while offset < before.size {
            let count = fsutil::read_at(
                file,
                &mut buffer[..(before.size - offset).min(65_536) as usize],
                offset,
            )?;
            if count == 0 {
                return Err(Error::TargetChanged);
            }
            hasher.update(&buffer[..count]);
            offset += count as u64;
        }
        Some(hasher.finish())
    } else {
        None
    };
    if before != fsutil::stat_identity(file)? {
        return Err(Error::TargetChanged);
    }
    Ok(Observation::new(before, hashes))
}

fn check_original(
    connection: &Connection,
    binding: &SyncBinding,
    name: &ObjectName,
    expected: Option<Generation>,
) -> Result<String, Error> {
    super::matching(connection, binding, &binding.bucket)?;
    let current = objects::lookup(connection, &binding.bucket, name.as_str())?;
    if current.as_ref().map(|(record, _)| record.generation) != expected.map(Generation::get) {
        return Err(Error::TargetChanged);
    }
    connection
        .prepare_cached("SELECT project FROM buckets WHERE name=?1")?
        .query_row([&binding.bucket], |row| row.get(0))
        .map_err(Error::from)
}
