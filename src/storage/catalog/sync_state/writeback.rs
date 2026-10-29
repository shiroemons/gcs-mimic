//! Durable write-back intent and protected recovery metadata; no adapter SQL or cleanup.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::super::{
    admin::{AdminError, BucketName, Generation, ObjectName, WAIT_TIMEOUT},
    change_journal::{JournalFence, JournalSeq},
    objects, unavailable,
};
use super::{
    CatalogId, SyncBinding, SyncFingerprint, SyncUpdate,
    root::{RootError, RootId, RootLease},
};
use crate::storage::blob_id::BlobId;
use crate::{
    bucket::CatalogError,
    fsutil,
    hashing::Hasher,
    object::ObjectError,
    sync::mapping::{self, PathMapping},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{fs::File, io, sync::Arc};

mod live_export;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use live_export::inject_failure as inject_live_export_failure;
pub use live_export::{LiveExport, LiveResult};

pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE sync_writeback_roots (
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

pub(in crate::storage) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Admin(AdminError),
    Root(RootError),
    Capacity,
    RecoveryRequired,
    Protected,
    WorkspaceChanged,
}
impl From<AdminError> for Error {
    fn from(e: AdminError) -> Self {
        Self::Admin(e)
    }
}
impl From<CatalogError> for Error {
    fn from(e: CatalogError) -> Self {
        Self::Admin(e.into())
    }
}
impl From<ObjectError> for Error {
    fn from(e: ObjectError) -> Self {
        Self::Admin(e.into())
    }
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::from(unavailable(e))
    }
}
impl From<RootError> for Error {
    fn from(e: RootError) -> Self {
        Self::Root(e)
    }
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Admin(e.into())
    }
}
impl From<fsutil::FsError> for Error {
    fn from(e: fsutil::FsError) -> Self {
        match e {
            fsutil::FsError::Io(e) => Self::from(e),
            fsutil::FsError::Unsupported => Self::Admin(AdminError::Io(io::ErrorKind::Unsupported)),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "write-back: {self:?}")
    }
}
impl std::error::Error for Error {}
fn stale() -> Error {
    Error::Admin(AdminError::Precondition)
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    bytes: u64,
    entries: u64,
}
impl Limits {
    pub fn new(bytes: u64, entries: u64) -> Result<Self, Error> {
        if !(1..=1_099_511_627_776).contains(&bytes) || !(1..=1_000_000).contains(&entries) {
            return Err(AdminError::Invalid("invalid write-back limits".into()).into());
        }
        Ok(Self { bytes, entries })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Publish,
    Remove,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Prepared,
    Armed,
    Protected,
    Durable,
    Complete,
}
impl Phase {
    fn read(value: i64) -> rusqlite::Result<Self> {
        match value {
            0 => Ok(Self::Prepared),
            1 => Ok(Self::Armed),
            2 => Ok(Self::Protected),
            3 => Ok(Self::Durable),
            4 => Ok(Self::Complete),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    catalog: CatalogId,
    root: RootId,
    id: String,
    revision: i64,
}
impl Token {
    pub fn id(&self) -> &str {
        &self.id
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceEntry(String);
impl WorkspaceEntry {
    pub fn name(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug)]
pub struct Pending {
    pub token: Token,
    pub ordinal: i64,
    pub owner: String,
    pub bucket: String,
    pub incarnation: String,
    pub name: String,
    pub path: String,
    pub generation: Generation,
    pub kind: Kind,
    pub phase: Phase,
    pub fence: JournalFence,
    pub workspace_entry: WorkspaceEntry,
    pub reserved_bytes: u64,
    pub retained_bytes: Option<u64>,
    size: u64,
    md5: [u8; 16],
    crc32c: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Ready,
    RecoveryRequired,
    ProtectedBody,
    ConflictCapacity,
    IoFailure,
}
#[derive(Clone, Debug)]
pub struct Status {
    pub reserved_bytes: u128,
    pub retained_bytes: u128,
    pub entries: u64,
    pub protected_entries: u64,
    pub progress: Option<JournalFence>,
    pub stop: Stop,
    limits: Limits,
    recovery: bool,
    io_failure: bool,
}
impl Status {
    fn over_limit(&self) -> bool {
        self.reserved_bytes.saturating_add(self.retained_bytes) > u128::from(self.limits.bytes)
            || self.entries > self.limits.entries
    }
    fn refresh(&mut self) {
        self.stop = if self.io_failure {
            Stop::IoFailure
        } else if self.over_limit() {
            Stop::ConflictCapacity
        } else if self.protected_entries > 0 {
            Stop::ProtectedBody
        } else if self.recovery {
            Stop::RecoveryRequired
        } else {
            Stop::Ready
        };
    }
}

/// Created only after storage verifies file and parent durability on its owned worker.
pub struct DurableReceipt {
    token: Token,
    fingerprint: Option<SyncFingerprint>,
    lease: RootLease,
}

#[derive(Clone)]
pub struct Writeback {
    lease: RootLease,
    workspace: Arc<File>,
}

mod unpublished;
pub use unpublished::Unpublished;
impl RootLease {
    pub async fn writeback(&self, limits: Limits) -> Result<Writeback, Error> {
        let permit = self
            .catalog()
            .wait_admin(
                self.catalog().0.writeback_execution.clone(),
                tokio::time::Instant::now() + WAIT_TIMEOUT,
            )
            .await?;
        let lease = self.clone();
        owned(async move {
            let _permit = permit;
            lease.writeback_inner(limits).await
        })
        .await
    }
    async fn writeback_inner(&self, limits: Limits) -> Result<Writeback, Error> {
        let root = self.status().root_id.as_bytes();
        let owner = self.clone();
        let expected:Option<([u8;8],[u8;8])>=self.catalog().run_admin(move|c| {
            let _owner=owner;
            c.query_row("SELECT workspace_device,workspace_inode FROM sync_writeback_roots WHERE root_id=?1",[root],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(unavailable)
        }).await?;
        let directory = self
            .file_job(move |root| {
                let selected = (|| -> Result<File, Error> {
                    let directory = fsutil::directory_at(
                        root,
                        mapping::WORKSPACE.as_ref(),
                        if expected.is_none() {
                            fsutil::DirectoryOpen::CreateIfMissing
                        } else {
                            fsutil::DirectoryOpen::Existing
                        },
                    )?;
                    let identity = fsutil::stat_identity(&directory)?;
                    if identity.device != fsutil::stat_identity(root)?.device
                        || expected.is_some_and(|(device, inode)| {
                            device != identity.device.to_be_bytes()
                                || inode != identity.inode.to_be_bytes()
                        })
                    {
                        return Err(Error::WorkspaceChanged);
                    }
                    directory.sync_all()?;
                    root.sync_all()?;
                    Ok(directory)
                })();
                Ok(selected)
            })
            .await?;
        let directory = match directory {
            Ok(directory) => directory,
            Err(error) => {
                let owner = self.clone();
                self.catalog().run_admin(move|c| {let _owner=owner;c.execute("UPDATE sync_writeback_roots SET recovery=1,io_failure=1 WHERE root_id=?1",[root]).map_err(unavailable)?;Ok(())}).await?;
                return Err(error);
            }
        };
        let identity = fsutil::stat_identity(&directory)?;
        let state = Writeback {
            lease: self.clone(),
            workspace: Arc::new(directory),
        };
        state.db(move |connection| {
            connection.execute("INSERT INTO sync_writeback_roots(root_id,max_bytes,max_entries,reserved,retained,entries,protected,recovery,io_failure,workspace_device,workspace_inode,sequence,epoch) VALUES(?1,?2,?3,?4,?4,0,0,0,0,?5,?6,NULL,NULL) ON CONFLICT(root_id) DO UPDATE SET max_bytes=excluded.max_bytes,max_entries=excluded.max_entries WHERE workspace_device=excluded.workspace_device AND workspace_inode=excluded.workspace_inode",
                params![root,limits.bytes as i64,limits.entries as i64,[0u8;16],identity.device.to_be_bytes(),identity.inode.to_be_bytes()])?;
            let current:([u8;8],[u8;8])=connection.query_row("SELECT workspace_device,workspace_inode FROM sync_writeback_roots WHERE root_id=?1",[root],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if current!=(identity.device.to_be_bytes(),identity.inode.to_be_bytes()){return Err(Error::WorkspaceChanged);}
            Ok(())
        }).await?;
        Ok(state)
    }
}

// A literal macro keeps the statements below static for the prepared statement cache.
macro_rules! pending_columns {
    () => {
        "id,ordinal,owner,bucket,incarnation,name,path,generation,kind,phase,revision,fence_sequence,fence_epoch,reservation,retained,size,md5,crc32c"
    };
}
const SELECT_PENDING_PAGE: &str = concat!(
    "SELECT ",
    pending_columns!(),
    " FROM sync_writeback_ops WHERE root_id=?1 AND phase!=4 AND ordinal>?2 ORDER BY ordinal LIMIT ?3"
);
const SELECT_PENDING: &str = concat!(
    "SELECT ",
    pending_columns!(),
    " FROM sync_writeback_ops WHERE root_id=?1 AND id=?2"
);
fn pending(row: &rusqlite::Row<'_>, catalog: CatalogId, root: RootId) -> rusqlite::Result<Pending> {
    let id: String = row.get(0)?;
    if BlobId::parse(&id).is_none() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let generation = Generation::new(row.get(7)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(Pending {
        token: Token {
            catalog,
            root,
            id: id.clone(),
            revision: row.get(10)?,
        },
        ordinal: row.get(1)?,
        owner: row.get(2)?,
        bucket: row.get(3)?,
        incarnation: row.get(4)?,
        name: row.get(5)?,
        path: row.get(6)?,
        generation,
        kind: match row.get::<_, i64>(8)? {
            0 => Kind::Publish,
            1 => Kind::Remove,
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
        phase: Phase::read(row.get(9)?)?,
        fence: JournalFence {
            sequence: JournalSeq(row.get::<_, i64>(11)? as u64),
            revision: row.get(12)?,
        },
        workspace_entry: WorkspaceEntry(id),
        reserved_bytes: u64::from_be_bytes(row.get(13)?),
        retained_bytes: row.get::<_, Option<[u8; 8]>>(14)?.map(u64::from_be_bytes),
        size: row.get::<_, i64>(15)? as u64,
        md5: row.get(16)?,
        crc32c: row.get(17)?,
    })
}
fn status(connection: &Connection, root: RootId) -> Result<Status, Error> {
    let mut value = connection.query_row("SELECT max_bytes,max_entries,reserved,retained,entries,protected,recovery,io_failure,sequence,epoch FROM sync_writeback_roots WHERE root_id=?1", [root.as_bytes()], |r| {
        let sequence: Option<i64> = r.get(8)?;
        let epoch: Option<i64> = r.get(9)?;
        let progress = match (sequence,epoch) {
            (None,None)=>None,(Some(s),Some(e))=>Some(JournalFence {sequence:JournalSeq(s as u64),revision:e}),
            _=>return Err(rusqlite::Error::InvalidQuery),
        };
        Ok(Status { limits:Limits {bytes:r.get::<_,i64>(0)? as u64,entries:r.get::<_,i64>(1)? as u64}, reserved_bytes:u128::from_be_bytes(r.get(2)?),retained_bytes:u128::from_be_bytes(r.get(3)?),entries:r.get::<_,i64>(4)? as u64,protected_entries:r.get::<_,i64>(5)? as u64,recovery:r.get(6)?,io_failure:r.get(7)?,progress,stop:Stop::Ready })
    })?;
    value.io_failure |= connection.query_row("SELECT EXISTS(SELECT 1 FROM sync_writeback_ops WHERE root_id=?1 AND phase!=4 AND io_failure!=0)",[root.as_bytes()],|r|r.get::<_,bool>(0))?;
    value.refresh();
    Ok(value)
}
fn save_status(connection: &Connection, root: RootId, value: &Status) -> Result<(), Error> {
    // Aggregated operation failures must never be copied into the workspace-only root flag.
    connection.execute("UPDATE sync_writeback_roots SET reserved=?2,retained=?3,entries=?4,protected=?5,recovery=?6,sequence=?7,epoch=?8 WHERE root_id=?1",
        params![root.as_bytes(),value.reserved_bytes.to_be_bytes(),value.retained_bytes.to_be_bytes(),value.entries as i64,value.protected_entries as i64,value.recovery,value.progress.map(|p|p.sequence.0 as i64),value.progress.map(|p|p.revision)])?;
    Ok(())
}
fn check_fence(connection: &Connection, fence: JournalFence) -> Result<(), Error> {
    let valid: bool = connection.query_row("SELECT enabled AND epoch=?1 AND ?2 BETWEEN floor AND sequence FROM change_journal_state WHERE singleton=1", params![fence.revision,i64::try_from(fence.sequence.0).map_err(|_|stale())?], |r|r.get(0))?;
    if !valid {
        return Err(stale());
    }
    Ok(())
}
fn original(connection: &Connection, item: &Pending) -> Result<(), Error> {
    let current: Option<(String,String)> = connection.query_row("SELECT b.project,i.incarnation FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?1", [&item.bucket], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if current.as_ref() != Some(&(item.owner.clone(), item.incarnation.clone())) {
        return Err(stale());
    }
    match item.kind {
        Kind::Publish => {
            let record = objects::lookup(connection, &item.bucket, &item.name)?
                .ok_or_else(stale)?
                .0;
            if record.has_hold() {
                return Err(AdminError::Hold.into());
            }
            if record.generation != item.generation.get()
                || record.size != item.size
                || record.md5 != item.md5
                || record.crc32c != item.crc32c
            {
                return Err(stale());
            }
        }
        Kind::Remove => {
            let complete: bool = connection.query_row(
                "SELECT complete FROM sync_roots WHERE root_id=?1",
                [item.token.root.as_bytes()],
                |r| r.get(0),
            )?;
            if !complete || objects::lookup(connection, &item.bucket, &item.name)?.is_some() {
                return Err(stale());
            }
            let held: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM objects WHERE bucket=?1 AND name=?2 AND generation=?3 AND (event_based_hold=1 OR temporary_hold=1))",params![item.bucket,item.name,item.generation.get()],|r|r.get(0))?;
            if held {
                return Err(AdminError::Hold.into());
            }
            let valid: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?3 AND generation=?4 AND size=?5 AND md5=?6 AND crc32c=?7)",params![item.bucket,item.name,item.incarnation,item.generation.get(),item.size as i64,item.md5,item.crc32c],|r|r.get(0))?;
            if !valid {
                return Err(stale());
            }
        }
    }
    Ok(())
}

fn parent(root: &File, path: &str, sync: bool) -> Result<Option<File>, Error> {
    let (parents, _) = path.rsplit_once('/').ok_or_else(stale)?;
    let mut directory = root.try_clone()?;
    if sync {
        directory.sync_all()?;
    }
    for part in parents.split('/') {
        directory = match fsutil::directory_at(
            &directory,
            part.as_ref(),
            fsutil::DirectoryOpen::Existing,
        ) {
            Ok(directory) => directory,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if sync {
            directory.sync_all()?;
        }
    }
    Ok(Some(directory))
}
fn target(root: &File, path: &str) -> Result<Option<File>, Error> {
    let Some(directory) = parent(root, path, false)? else {
        return Ok(None);
    };
    let name = path.rsplit('/').next().ok_or_else(stale)?;
    match fsutil::open_regular_at(&directory, name.as_ref()) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn entry(workspace: &File, id: &str) -> Result<Option<File>, Error> {
    match fsutil::open_regular_at(workspace, id.as_ref()) {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn verify_workspace(root: &File, workspace: &File) -> Result<(), Error> {
    let selected = fsutil::directory_at(
        root,
        mapping::WORKSPACE.as_ref(),
        fsutil::DirectoryOpen::Existing,
    )
    .map_err(|_| Error::WorkspaceChanged)?;
    let selected = fsutil::stat_identity(&selected)?;
    let owned = fsutil::stat_identity(workspace)?;
    if selected.device != owned.device
        || selected.inode != owned.inode
        || owned.device != fsutil::stat_identity(root)?.device
    {
        return Err(Error::WorkspaceChanged);
    }
    Ok(())
}
fn fingerprint(
    root: &File,
    workspace: &File,
    item: &Pending,
) -> Result<Option<SyncFingerprint>, Error> {
    let file = target(root, &item.path)?;
    let result = match (item.kind, file) {
        (Kind::Remove, None) => None,
        (Kind::Publish, Some(file)) => {
            let before = fsutil::stat_identity(&file)?;
            if before.device != fsutil::stat_identity(root)?.device || before.size != item.size {
                return Err(stale());
            }
            let mut hasher = Hasher::default();
            let mut bytes = [0u8; 65_536];
            let mut offset = 0;
            loop {
                let count = fsutil::read_at(&file, &mut bytes, offset)?;
                if count == 0 {
                    break;
                }
                hasher.update(&bytes[..count]);
                offset += count as u64;
                if offset > item.size {
                    return Err(stale());
                }
            }
            let hashes = hasher.finish();
            file.sync_all()?;
            if before != fsutil::stat_identity(&file)?
                || offset != item.size
                || hashes.md5 != item.md5
                || hashes.crc32c != item.crc32c
            {
                return Err(stale());
            }
            Some(SyncFingerprint {
                size: before.size,
                mtime_ns: before.modified_ns,
                ctime_ns: before.changed_ns,
                inode: before.inode,
                md5: hashes.md5,
                crc32c: hashes.crc32c,
                racy: true,
            })
        }
        _ => return Err(stale()),
    };
    parent(root, &item.path, true)?;
    workspace.sync_all()?;
    root.sync_all()?;
    Ok(result)
}

impl Writeback {
    async fn serial<T: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = Result<T, Error>> + Send + 'static,
    ) -> Result<T, Error> {
        let permit = self
            .lease
            .catalog()
            .wait_admin(
                self.lease.catalog().0.writeback_execution.clone(),
                tokio::time::Instant::now() + WAIT_TIMEOUT,
            )
            .await?;
        // Cancellation can release a waiting request, but never an executing file/SQL transition.
        owned(async move {
            let _permit = permit;
            future.await
        })
        .await
    }
    /// Keeps the selected workspace descriptor and root/catalog leases through worker drain.
    /// The closure must finish its work before returning; directory descriptors must not escape it.
    pub async fn file_job<T: Send + 'static>(
        &self,
        token: Token,
        job: impl FnOnce(&File, &File) -> Result<T, AdminError> + Send + 'static,
    ) -> Result<T, Error> {
        self.token(&token)?;
        let this = self.clone();
        self.serial(async move {
            let selected = token.clone();
            this.db(move |c| {
                let item = checked(c, &selected)?;
                let usage = status(c, selected.root)?;
                if item.phase != Phase::Armed
                    || usage.io_failure
                    || usage.protected_entries > 0
                    || usage.over_limit()
                {
                    return Err(Error::RecoveryRequired);
                }
                original(c, &item)
            })
            .await?;
            let result = this
                .fs(move |root, workspace| job(root, workspace).map_err(Error::from))
                .await;
            if matches!(
                result,
                Err(Error::Admin(AdminError::DiskFull | AdminError::Io(_)))
            ) {
                this.io_failure(token, 1).await?;
            }
            result
        })
        .await
    }
    async fn db<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Connection) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let lease = self.lease.clone();
        self.lease
            .catalog()
            .run_admin(move |connection| {
                let _lease = lease;
                Ok(job(connection))
            })
            .await?
    }
    async fn fs<T: Send + 'static>(
        &self,
        job: impl FnOnce(&File, &File) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let workspace = self.workspace.clone();
        let result = self
            .lease
            .file_job(move |root| {
                Ok((|| {
                    verify_workspace(root, &workspace)?;
                    let result = job(root, &workspace);
                    verify_workspace(root, &workspace)?;
                    result
                })())
            })
            .await?;
        if matches!(result, Err(Error::WorkspaceChanged)) {
            let root = self.identities().1;
            self.db(move |c| {
                c.execute(
                    "UPDATE sync_writeback_roots SET recovery=1,io_failure=1 WHERE root_id=?1",
                    [root.as_bytes()],
                )?;
                Ok(())
            })
            .await?;
        }
        result
    }
    fn identities(&self) -> (CatalogId, RootId) {
        let s = self.lease.status();
        (s.catalog_id, s.root_id)
    }
    fn token(&self, token: &Token) -> Result<(), Error> {
        let (c, r) = self.identities();
        if token.catalog != c || token.root != r {
            return Err(stale());
        }
        Ok(())
    }
    pub async fn status(&self) -> Result<Status, Error> {
        let this = self.clone();
        self.serial(async move { this.status_inner().await }).await
    }
    async fn status_inner(&self) -> Result<Status, Error> {
        self.fs(|_, _| Ok(())).await?;
        let root = self.identities().1;
        self.db(move |c| status(c, root)).await
    }
    pub async fn page(&self, after: i64, limit: usize) -> Result<Vec<Pending>, Error> {
        let this = self.clone();
        self.serial(async move { this.page_inner(after, limit).await })
            .await
    }
    async fn page_inner(&self, after: i64, limit: usize) -> Result<Vec<Pending>, Error> {
        if after < 0 || !(1..=256).contains(&limit) {
            return Err(AdminError::Invalid("invalid pending page".into()).into());
        }
        self.fs(|_, _| Ok(())).await?;
        let (catalog, root) = self.identities();
        self.db(move |c| {
            Ok(c.prepare_cached(SELECT_PENDING_PAGE)?
                .query_map(params![root.as_bytes(), after, limit as i64], |r| {
                    pending(r, catalog, root)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }
    pub async fn get(&self, id: &str) -> Result<Pending, Error> {
        if BlobId::parse(id).is_none() {
            return Err(stale());
        }
        let (this, id) = (self.clone(), id.to_owned());
        self.serial(async move { this.get_inner(&id).await }).await
    }
    async fn get_inner(&self, id: &str) -> Result<Pending, Error> {
        self.fs(|_, _| Ok(())).await?;
        if BlobId::parse(id).is_none() {
            return Err(stale());
        }
        let (catalog, root) = self.identities();
        let id = id.to_owned();
        self.db(move |c| load(c, catalog, root, &id)).await
    }
    pub async fn prepare(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        generation: Generation,
        kind: Kind,
        fence: JournalFence,
    ) -> Result<Pending, Error> {
        let (this, bucket, name) = (self.clone(), bucket.clone(), name.clone());
        self.serial(async move {
            this.prepare_inner(&bucket, &name, generation, kind, fence)
                .await
        })
        .await
    }
    async fn prepare_inner(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        generation: Generation,
        kind: Kind,
        fence: JournalFence,
    ) -> Result<Pending, Error> {
        let path = match mapping::name_to_path(bucket, name) {
            PathMapping::File(p) => p.to_str().ok_or_else(stale)?.to_owned(),
            _ => return Err(stale()),
        };
        if !super::valid_path(&path) {
            return Err(stale());
        }
        let selected_path = path.clone();
        let reservation = self
            .fs(move |root, _| {
                Ok(target(root, &selected_path)?
                    .map(|f| fsutil::stat_identity(&f))
                    .transpose()?
                    .map_or(0, |f| f.size))
            })
            .await?;
        let (catalog, root) = self.identities();
        let bucket = bucket.as_str().to_owned();
        let name = name.as_str().to_owned();
        self.db(move |c| {
            let tx=c.transaction()?;check_fence(&tx,fence)?;let mut usage=status(&tx,root)?;
            if usage.stop!=Stop::Ready {return Err(Error::RecoveryRequired);}
            if usage.entries>=usage.limits.entries || usage.reserved_bytes+usage.retained_bytes+u128::from(reservation)>u128::from(usage.limits.bytes) {
                usage.recovery=true;save_status(&tx,root,&usage)?;tx.commit()?;return Err(Error::Capacity);
            }
            let (owner,incarnation):(String,String)=tx.query_row("SELECT b.project,i.incarnation FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?1",[&bucket],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let (size,md5,crc32c)=match kind {
                Kind::Publish=>{let value=objects::lookup(&tx,&bucket,&name)?.ok_or_else(stale)?.0;(value.size,value.md5,value.crc32c)},
                Kind::Remove=>tx.query_row("SELECT size,md5,crc32c FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?3 AND generation=?4",params![bucket,name,incarnation,generation.get()],|r|Ok((r.get::<_,i64>(0)? as u64,r.get(1)?,r.get(2)?)))?,
            };
            let id=BlobId::generate().map_err(|e|Error::Admin(AdminError::Backend(e.to_string())))?.to_hex();
            tx.execute("INSERT INTO sync_writeback_ops(id,root_id,owner,bucket,incarnation,name,path,generation,kind,phase,revision,fence_sequence,fence_epoch,reservation,size,md5,crc32c) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,1,?10,?11,?12,?13,?14,?15)",
                params![id,root.as_bytes(),owner,bucket,incarnation,name,path,generation.get(),kind as i64,fence.sequence.0 as i64,fence.revision,reservation.to_be_bytes(),size as i64,md5,crc32c])?;
            let item=load(&tx,catalog,root,&id)?;original(&tx,&item)?;
            usage.entries+=1;usage.reserved_bytes+=u128::from(reservation);save_status(&tx,root,&usage)?;tx.commit()?;Ok(item)
        }).await
    }
    pub async fn arm(&self, token: Token) -> Result<Pending, Error> {
        self.token(&token)?;
        let this = self.clone();
        self.serial(async move { this.arm_inner(token).await })
            .await
    }
    async fn arm_inner(&self, token: Token) -> Result<Pending, Error> {
        self.token(&token)?;
        self.fs(|_, _| Ok(())).await?;
        self.db(move |c| {let tx=c.transaction()?;let item=checked(&tx,&token)?;if item.phase!=Phase::Prepared {return Err(stale());}
            let usage=status(&tx,token.root)?;if usage.over_limit() || usage.io_failure || usage.protected_entries>0 {return Err(Error::RecoveryRequired);}
            let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_writeback_ops WHERE root_id=?1 AND phase IN (1,2,3))",[token.root.as_bytes()],|r|r.get(0))?;
            if active {return Err(Error::RecoveryRequired);}original(&tx,&item)?;
            tx.execute("UPDATE sync_writeback_ops SET phase=1,revision=revision+1 WHERE id=?1",[&token.id])?;
            tx.execute("UPDATE sync_writeback_roots SET recovery=1 WHERE root_id=?1",[token.root.as_bytes()])?;
            let item=load(&tx,token.catalog,token.root,&token.id)?;tx.commit()?;Ok(item)
        }).await
    }
    async fn io_failure(&self, token: Token, stage: i64) -> Result<(), Error> {
        // Entry and target failures coexist until their own revision is revalidated.
        self.db(move |c| {
            let tx = c.transaction()?;
            checked(&tx, &token)?;
            tx.execute(
                "UPDATE sync_writeback_ops SET io_failure=io_failure | ?2 WHERE id=?1",
                params![token.id, stage],
            )?;
            tx.execute(
                "UPDATE sync_writeback_roots SET recovery=1 WHERE root_id=?1",
                [token.root.as_bytes()],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn inspect(&self, token: Token) -> Result<Pending, Error> {
        self.token(&token)?;
        let this = self.clone();
        self.serial(async move { this.inspect_inner(token).await })
            .await
    }
    async fn inspect_inner(&self, token: Token) -> Result<Pending, Error> {
        let selected = token.clone();
        let item = self.db(move |c| checked(c, &selected)).await?;
        if item.phase == Phase::Complete {
            return Err(stale());
        }
        let id = token.id.clone();
        let observed = self
            .fs(move |root, workspace| {
                let Some(file) = entry(workspace, &id)? else {
                    workspace.sync_all()?;
                    root.sync_all()?;
                    return Ok(None);
                };
                let identity = fsutil::stat_identity(&file)?;
                let durability = file
                    .sync_all()
                    .and_then(|_| workspace.sync_all())
                    .and_then(|_| root.sync_all());
                Ok(Some((identity.size, durability.err().map(|e| e.kind()))))
            })
            .await;
        let observed = match observed {
            Ok(v) => v,
            Err(e) => {
                self.io_failure(token.clone(), 1).await?;
                return Err(e);
            }
        };
        let Some((size, failure)) = observed else {
            if item.phase == Phase::Protected {
                self.io_failure(token.clone(), 1).await?;
                return Err(Error::Protected);
            }
            return self.db(move|c| {
                let tx=c.transaction()?;checked(&tx,&token)?;
                tx.execute("UPDATE sync_writeback_ops SET io_failure=io_failure & 2,revision=revision+1 WHERE id=?1 AND (io_failure & 1)!=0",[&token.id])?;
                tx.execute("UPDATE sync_writeback_roots SET io_failure=0 WHERE root_id=?1 AND io_failure=1",[token.root.as_bytes()])?;
                let item=load(&tx,token.catalog,token.root,&token.id)?;tx.commit()?;Ok(item)
            }).await;
        };
        let next=self.db(move|c| {let tx=c.transaction()?;let old=checked(&tx,&token)?;let mut usage=status(&tx,token.root)?;
            if let Some(previous)=old.retained_bytes {usage.retained_bytes=usage.retained_bytes.checked_sub(u128::from(previous)).ok_or_else(stale)?;}
            else {usage.reserved_bytes=usage.reserved_bytes.checked_sub(u128::from(old.reserved_bytes)).ok_or_else(stale)?;usage.protected_entries+=1;}
            usage.retained_bytes+=u128::from(size);usage.recovery=true;
            save_status(&tx,token.root,&usage)?;tx.execute("UPDATE sync_writeback_ops SET phase=2,revision=revision+1,retained=?2,io_failure=(io_failure & 2) | ?3 WHERE id=?1",params![token.id,size.to_be_bytes(),i64::from(failure.is_some())])?;
            tx.execute("UPDATE sync_writeback_roots SET io_failure=0 WHERE root_id=?1 AND io_failure=1",[token.root.as_bytes()])?;
            let value=load(&tx,token.catalog,token.root,&token.id)?;tx.commit()?;Ok(value)
        }).await?;
        if let Some(kind) = failure {
            return Err(Error::Admin(io::Error::from(kind).into()));
        }
        Ok(next)
    }
    pub async fn durable(&self, token: Token) -> Result<DurableReceipt, Error> {
        self.token(&token)?;
        let this = self.clone();
        self.serial(async move { this.durable_inner(token).await })
            .await
    }
    async fn durable_inner(&self, token: Token) -> Result<DurableReceipt, Error> {
        let item = self.inspect_inner(token).await?;
        if item.phase == Phase::Protected {
            return Err(Error::Protected);
        }
        if !matches!(item.phase, Phase::Armed | Phase::Durable) {
            return Err(stale());
        }
        let selected = item.clone();
        let observed = self
            .fs(move |root, workspace| fingerprint(root, workspace, &selected))
            .await;
        let fp = match observed {
            Ok(fp) => fp,
            Err(e) => {
                self.io_failure(item.token.clone(), 2).await?;
                return Err(e);
            }
        };
        let token = item.token;
        let snapshot = fp
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|e| Error::Admin(AdminError::Backend(e.to_string())))?;
        let next=self.db(move|c|{let tx=c.transaction()?;let item=checked(&tx,&token)?;original(&tx,&item)?;
            let usage=status(&tx,token.root)?;
            if usage.protected_entries>0||usage.over_limit(){return Err(Error::RecoveryRequired);}
            tx.execute("UPDATE sync_writeback_ops SET phase=3,revision=revision+1,snapshot=?2,io_failure=0 WHERE id=?1",params![token.id,snapshot])?;
            let item=load(&tx,token.catalog,token.root,&token.id)?;tx.commit()?;Ok(item.token)
        }).await?;
        Ok(DurableReceipt {
            token: next,
            fingerprint: fp,
            lease: self.lease.clone(),
        })
    }
    pub async fn acknowledge(
        &self,
        receipt: DurableReceipt,
        progress: Option<JournalFence>,
    ) -> Result<Pending, Error> {
        self.token(&receipt.token)?;
        let this = self.clone();
        self.serial(async move { this.ack_inner(receipt, progress).await })
            .await
    }
    async fn ack_inner(
        &self,
        receipt: DurableReceipt,
        progress: Option<JournalFence>,
    ) -> Result<Pending, Error> {
        let _lease = receipt.lease;
        let token = receipt.token;
        let item = self.inspect_inner(token.clone()).await?;
        if item.phase != Phase::Durable {
            return Err(stale());
        }
        let record = item.clone();
        let expected = receipt.fingerprint.clone();
        self.fs(move |root, workspace| {
            if entry(workspace, &record.token.id)?.is_some() {
                return Err(Error::Protected);
            }
            let found = target(root, &record.path)?;
            match (expected, found) {
                (None, None) if record.kind == Kind::Remove => Ok(()),
                (Some(fp), Some(file)) => {
                    let now = fsutil::stat_identity(&file)?;
                    if now.size != fp.size
                        || now.inode != fp.inode
                        || now.modified_ns != fp.mtime_ns
                        || now.changed_ns != fp.ctime_ns
                    {
                        Err(stale())
                    } else {
                        Ok(())
                    }
                }
                _ => Err(stale()),
            }
        })
        .await?;
        self.db(move|c|{let tx=c.transaction()?;let item=checked(&tx,&token)?;if item.phase!=Phase::Durable||item.retained_bytes.is_some(){return Err(Error::Protected);}original(&tx,&item)?;
            let snapshot:Option<Vec<u8>>=tx.query_row("SELECT snapshot FROM sync_writeback_ops WHERE id=?1",[&token.id],|r|r.get(0))?;
            let stored=snapshot.as_deref().map(serde_json::from_slice::<SyncFingerprint>).transpose().map_err(|_|stale())?;
            if stored!=receipt.fingerprint{return Err(stale());}
            let mut usage=status(&tx,token.root)?;if usage.io_failure||usage.protected_entries>0||usage.over_limit(){return Err(Error::RecoveryRequired);}
            if let Some(fence)=progress {
                if fence.revision!=item.fence.revision||fence.sequence<item.fence.sequence{return Err(stale());}
                advance(&tx,token.root,&token.id,&mut usage,fence)?;
            }
            match receipt.fingerprint {
                Some(fp)=>{let binding=SyncBinding {catalog:token.catalog,bucket:item.bucket.clone(),incarnation:item.incarnation.clone()};let update=SyncUpdate::new(binding,item.path.clone(),fp)?;
                    let record=objects::lookup(&tx,&item.bucket,&item.name)?.ok_or_else(stale)?.0;super::record_in_transaction(&tx,&update,&record)?;},
                None=>{let count=tx.execute("DELETE FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?3 AND generation=?4",params![item.bucket,item.name,item.incarnation,item.generation.get()])?;if count!=1{return Err(stale());}},
            }
            usage.entries=usage.entries.checked_sub(1).ok_or_else(stale)?;usage.reserved_bytes=usage.reserved_bytes.checked_sub(u128::from(item.reserved_bytes)).ok_or_else(stale)?;
            save_status(&tx,token.root,&usage)?;tx.execute("UPDATE sync_writeback_ops SET phase=4,revision=revision+1 WHERE id=?1",[&token.id])?;
            let complete=load(&tx,token.catalog,token.root,&token.id)?;
            tx.execute("DELETE FROM sync_writeback_ops WHERE phase=4 AND ordinal <= (SELECT coalesce(max(ordinal),0)-65536 FROM sync_writeback_ops) AND ordinal IN (SELECT ordinal FROM sync_writeback_ops WHERE phase=4 ORDER BY ordinal LIMIT 256)",[])?;
            tx.commit()?;Ok(complete)
        }).await
    }
    pub async fn resume(&self) -> Result<Status, Error> {
        let this = self.clone();
        self.serial(async move { this.resume_inner().await }).await
    }
    async fn resume_inner(&self) -> Result<Status, Error> {
        self.fs(|_, _| Ok(())).await?;
        let root = self.identities().1;
        self.db(move |c| {
            let tx = c.transaction()?;
            let mut usage = status(&tx, root)?;
            if usage.entries > 0 || usage.over_limit() || usage.protected_entries > 0 {
                return Err(Error::RecoveryRequired);
            }
            usage.io_failure = false;
            usage.recovery = false;
            tx.execute(
                "UPDATE sync_writeback_roots SET io_failure=0 WHERE root_id=?1",
                [root.as_bytes()],
            )?;
            save_status(&tx, root, &usage)?;
            usage.refresh();
            tx.commit()?;
            Ok(usage)
        })
        .await
    }
    pub async fn checkpoint(&self, fence: JournalFence) -> Result<(), Error> {
        let this = self.clone();
        self.serial(async move { this.checkpoint_inner(fence).await })
            .await
    }
    async fn checkpoint_inner(&self, fence: JournalFence) -> Result<(), Error> {
        self.fs(|_, _| Ok(())).await?;
        let root = self.identities().1;
        self.db(move |c| {
            let tx = c.transaction()?;
            let mut usage = status(&tx, root)?;
            if usage.stop != Stop::Ready || usage.entries != 0 {
                return Err(Error::RecoveryRequired);
            }
            let complete: bool = tx.query_row(
                "SELECT complete FROM sync_roots WHERE root_id=?1",
                [root.as_bytes()],
                |r| r.get(0),
            )?;
            if !complete {
                return Err(stale());
            }
            advance(&tx, root, "", &mut usage, fence)?;
            save_status(&tx, root, &usage)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
}
fn load(c: &Connection, catalog: CatalogId, root: RootId, id: &str) -> Result<Pending, Error> {
    c.prepare_cached(SELECT_PENDING)?
        .query_row(params![root.as_bytes(), id], |r| pending(r, catalog, root))
        .optional()?
        .ok_or_else(stale)
}
fn checked(c: &Connection, token: &Token) -> Result<Pending, Error> {
    let item = load(c, token.catalog, token.root, &token.id)?;
    if item.token.revision != token.revision {
        return Err(stale());
    }
    Ok(item)
}
fn advance(
    tx: &Transaction<'_>,
    root: RootId,
    except: &str,
    usage: &mut Status,
    fence: JournalFence,
) -> Result<(), Error> {
    check_fence(tx, fence)?;
    if usage
        .progress
        .is_some_and(|p| p.revision == fence.revision && p.sequence > fence.sequence)
    {
        return Err(stale());
    }
    let blocked: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_writeback_ops WHERE root_id=?1 AND phase!=4 AND id!=?2)",
        params![root.as_bytes(), except],
        |r| r.get(0),
    )?;
    if blocked {
        return Err(stale());
    }
    usage.progress = Some(fence);
    Ok(())
}
async fn owned<T: Send + 'static>(
    future: impl std::future::Future<Output = Result<T, Error>> + Send + 'static,
) -> Result<T, Error> {
    tokio::spawn(future)
        .await
        .map_err(|e| Error::Admin(AdminError::Backend(e.to_string())))?
}

#[cfg(all(test, unix))]
mod tests;
