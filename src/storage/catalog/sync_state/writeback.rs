//! Durable write-back intent and protected recovery metadata; no adapter SQL or cleanup.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::super::{
    admin::{AdminError, BucketName, Generation, ObjectName},
    change_journal::JournalFence,
    objects, unavailable,
};
use super::{
    CatalogId, SyncBinding, SyncFingerprint, SyncUpdate,
    root::{RootError, RootId, RootLease},
};
use crate::storage::blob_id::BlobId;
use crate::storage::catalog::Incarnation;
use crate::storage::catalog::sql;
use crate::{
    bucket::CatalogError,
    fsutil,
    object::ObjectError,
    sync::mapping::{self, PathMapping},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{fs::File, io, sync::Arc};

mod conflict_transfer;
mod live_export;
mod recovery;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use live_export::inject_failure as inject_live_export_failure;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use live_export::tests::{
    armed_observation, observation as begin_observation, pending as live_pending,
};
pub(crate) use live_export::{LiveExport, LiveResult, LiveSource};

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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Error {
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
            fsutil::FsError::Unsupported => {
                Self::Admin(AdminError::Io(io::ErrorKind::Unsupported.into()))
            }
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("write-back: ")?;
        match self {
            Self::Admin(_) => f.write_str("admin operation failed"),
            Self::Root(_) => f.write_str("root validation failed"),
            Self::Capacity => f.write_str("Capacity"),
            Self::RecoveryRequired => f.write_str("RecoveryRequired"),
            Self::Protected => f.write_str("Protected"),
            Self::WorkspaceChanged => f.write_str("WorkspaceChanged"),
        }
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
fn stale() -> Error {
    Error::Admin(AdminError::Precondition)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    bytes: u64,
    entries: u64,
}

impl Limits {
    pub(crate) fn new(bytes: u64, entries: u64) -> Result<Self, Error> {
        use super::conflicts::{MAX_BYTES, MAX_ENTRIES};
        if !(1..=MAX_BYTES).contains(&bytes) || !(1..=MAX_ENTRIES).contains(&entries) {
            return Err(AdminError::Invalid("invalid write-back limits".into()).into());
        }
        Ok(Self { bytes, entries })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Publish,
    Remove,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Prepared,
    Armed,
    Protected,
    Durable,
    Complete,
}
sql::integer_enum!(Kind, { Publish => 0, Remove => 1 });
sql::integer_enum!(Phase, {
    Prepared => 0, Armed => 1, Protected => 2, Durable => 3, Complete => 4,
});
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    catalog: CatalogId,
    root: RootId,
    id: String,
    revision: i64,
}
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub token: Token,
    pub ordinal: i64,
    pub owner: String,
    pub bucket: String,
    pub incarnation: Incarnation,
    pub name: String,
    pub path: String,
    pub generation: Generation,
    pub kind: Kind,
    pub phase: Phase,
    pub reserved_bytes: u64,
    pub retained_bytes: Option<u64>,
    size: u64,
    md5: [u8; 16],
    crc32c: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Ready,
    RecoveryRequired,
    ProtectedBody,
    ConflictCapacity,
    IoFailure,
}
impl Stop {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::RecoveryRequired => "RecoveryRequired",
            Self::ProtectedBody => "ProtectedBody",
            Self::ConflictCapacity => "ConflictCapacity",
            Self::IoFailure => "IoFailure",
        }
    }
}
crate::serde_util::serialize_as_str!(Stop);
impl std::fmt::Display for Stop {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "write-back stopped: {}", self.as_str())
    }
}
impl std::error::Error for Stop {}
#[derive(Clone, Debug)]
pub(crate) struct Status {
    pub reserved_bytes: u128,
    pub retained_bytes: u128,
    pub entries: u64,
    pub protected_entries: u64,
    pub stop: Stop,
    limits: Limits,
    recovery: bool,
    io_failure: bool,
    conflicts: super::conflicts::Usage,
    captured_entries: u64,
}
impl Status {
    fn effective_entries(&self) -> u64 {
        self.entries + self.conflicts.entries - self.captured_entries
    }
    fn over_limit(&self) -> bool {
        self.reserved_bytes
            + self.retained_bytes
            + self.conflicts.reserved_bytes
            + self.conflicts.retained_bytes
            > u128::from(self.limits.bytes)
            || self.effective_entries() > self.limits.entries
    }
    fn would_exceed(&self, reservation: u64) -> bool {
        self.effective_entries() >= self.limits.entries
            || self.reserved_bytes
                + self.retained_bytes
                + self.conflicts.reserved_bytes
                + self.conflicts.retained_bytes
                + u128::from(reservation)
                > u128::from(self.limits.bytes)
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

#[derive(Clone)]
pub(crate) struct Writeback {
    workspace: Arc<File>,
    lease: RootLease,
}

impl RootLease {
    pub(crate) async fn default_writeback(&self) -> Result<Writeback, Error> {
        self.writeback(Limits::new(
            crate::sync::CONFLICT_MAX_BYTES,
            crate::sync::CONFLICT_MAX_ENTRIES,
        )?)
        .await
    }
    pub(crate) async fn writeback(&self, limits: Limits) -> Result<Writeback, Error> {
        let lease = self.clone();
        serialized(self, async move { lease.writeback_inner(limits).await }).await
    }
    async fn writeback_inner(&self, limits: Limits) -> Result<Writeback, Error> {
        let root = self.status().root_id;
        let owner = self.clone();
        let expected:Option<([u8;8],[u8;8])>=self.catalog().run_admin(move|c| {
            let _owner=owner;
            sql::row(c, "SELECT workspace_device,workspace_inode FROM sync_writeback_roots WHERE root_id=?1", [root],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(unavailable)
        }).await?;
        let directory = self
            .file_job(move |root| {
                let selected = (|| -> Result<File, Error> {
                    let directory = workspace_directory(
                        root,
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
                    if expected.is_none() {
                        directory.sync_all()?;
                        root.sync_all()?;
                    }
                    Ok(directory)
                })();
                Ok(selected)
            })
            .await?;
        let directory = match directory {
            Ok(directory) => directory,
            Err(error @ Error::WorkspaceChanged) => {
                self.mark_workspace_changed().await?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let identity = fsutil::stat_identity(&directory)?;
        let state = Writeback {
            lease: self.clone(),
            workspace: Arc::new(directory),
        };
        state.db(move |connection| {
            connection.prepare_cached("INSERT INTO sync_writeback_roots(root_id,max_bytes,max_entries,reserved,retained,entries,protected,recovery,io_failure,workspace_device,workspace_inode,sequence,epoch) VALUES(?1,?2,?3,?4,?4,0,0,0,0,?5,?6,NULL,NULL) ON CONFLICT(root_id) DO UPDATE SET max_bytes=excluded.max_bytes,max_entries=excluded.max_entries WHERE workspace_device=excluded.workspace_device AND workspace_inode=excluded.workspace_inode")?
                .execute(params![root,limits.bytes,limits.entries,[0u8;16],identity.device.to_be_bytes(),identity.inode.to_be_bytes()])?;
            let current:([u8;8],[u8;8])=connection.prepare_cached("SELECT workspace_device,workspace_inode FROM sync_writeback_roots WHERE root_id=?1")?.query_row([root],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if current!=(identity.device.to_be_bytes(),identity.inode.to_be_bytes()){return Err(Error::WorkspaceChanged);}
            Ok(())
        }).await?;
        Ok(state)
    }

    async fn mark_workspace_changed(&self) -> Result<(), Error> {
        let root = self.status().root_id;
        let scope = self.control_scope();
        self.catalog()
            .run_admin(move |connection| {
                scope.authorize(connection)?;
                sql::exec(
                    connection,
                    "UPDATE sync_writeback_roots SET recovery=1,io_failure=1 WHERE root_id=?1",
                    [root],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
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
    if id.parse::<BlobId>().is_err() {
        return Err(sql::conversion_error(
            0,
            rusqlite::types::Type::Text,
            "invalid write-back operation ID",
        ));
    }
    let generation = Generation::new(row.get(7)?)
        .map_err(|error| sql::conversion_error(7, rusqlite::types::Type::Integer, error))?;
    Ok(Pending {
        token: Token {
            catalog,
            root,
            id,
            revision: row.get(10)?,
        },
        ordinal: row.get(1)?,
        owner: row.get(2)?,
        bucket: row.get(3)?,
        incarnation: row.get(4)?,
        name: row.get(5)?,
        path: row.get(6)?,
        generation,
        kind: row.get(8)?,
        phase: row.get(9)?,
        reserved_bytes: u64::from_be_bytes(row.get(13)?),
        retained_bytes: row.get::<_, Option<[u8; 8]>>(14)?.map(u64::from_be_bytes),
        size: row.get(15)?,
        md5: row.get(16)?,
        crc32c: row.get(17)?,
    })
}
fn status(connection: &Connection, root: RootId) -> Result<Status, Error> {
    let mut value = connection.prepare_cached("SELECT max_bytes,max_entries,reserved,retained,entries,protected,recovery,io_failure FROM sync_writeback_roots WHERE root_id=?1")?.query_row([root], |r| {
        Ok(Status { limits:Limits {bytes:r.get(0)?,entries:r.get(1)?}, reserved_bytes:u128::from_be_bytes(r.get(2)?),retained_bytes:u128::from_be_bytes(r.get(3)?),entries:r.get(4)?,protected_entries:r.get(5)?,recovery:r.get(6)?,io_failure:r.get(7)?,stop:Stop::Ready,conflicts:Default::default(),captured_entries:0 })
    })?;
    value.io_failure |= connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_writeback_ops WHERE root_id=?1 AND phase!=4 AND io_failure!=0)")?.query_row([root],|r|r.get::<_,bool>(0))?;
    value.conflicts = super::conflicts::usage(connection, root).map_err(AdminError::backend)?;
    value.captured_entries =
        super::conflicts::captured_entries(connection, root).map_err(AdminError::backend)?;
    if value.captured_entries > value.entries {
        return Err(stale());
    }
    value.refresh();
    Ok(value)
}
pub(super) fn observation(
    connection: &Connection,
    root: RootId,
) -> Result<super::observation::WritebackSummary, Error> {
    let value = status(connection, root)?;
    Ok(super::observation::WritebackSummary {
        state: value.stop,
        reserved_bytes: value.reserved_bytes + value.conflicts.reserved_bytes,
        retained_bytes: value.retained_bytes + value.conflicts.retained_bytes,
        entries: value.effective_entries(),
        protected_entries: value.protected_entries,
        max_bytes: value.limits.bytes,
        max_entries: value.limits.entries,
        over_limit: value.over_limit(),
    })
}
fn save_status(connection: &Connection, root: RootId, value: &Status) -> Result<(), Error> {
    // Aggregated operation failures must never be copied into the workspace-only root flag.
    connection.prepare_cached("UPDATE sync_writeback_roots SET reserved=?2,retained=?3,entries=?4,protected=?5,recovery=?6,sequence=NULL,epoch=NULL WHERE root_id=?1")?
        .execute(params![root,value.reserved_bytes.to_be_bytes(),value.retained_bytes.to_be_bytes(),value.entries,value.protected_entries,value.recovery])?;
    Ok(())
}
fn check_fence(connection: &Connection, fence: JournalFence) -> Result<(), Error> {
    let valid: bool = connection.prepare_cached("SELECT enabled AND epoch=?1 AND ?2 BETWEEN floor AND sequence FROM change_journal_state WHERE singleton=1")?.query_row(params![fence.revision,i64::try_from(fence.sequence.get()).map_err(|_|stale())?], |r|r.get(0))?;
    if !valid {
        return Err(stale());
    }
    Ok(())
}
fn original(connection: &Connection, item: &Pending) -> Result<(), Error> {
    original_with_protection(connection, item, OriginalProtection::Enforce)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OriginalProtection {
    Enforce,
    ReadOnlyCopy,
}

fn original_with_protection(
    connection: &Connection,
    item: &Pending,
    protection: OriginalProtection,
) -> Result<(), Error> {
    let current: Option<(String,Incarnation)> = connection.prepare_cached("SELECT b.project,i.incarnation FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?1")?.query_row([&item.bucket], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if current.as_ref() != Some(&(item.owner.clone(), item.incarnation)) {
        return Err(stale());
    }
    match item.kind {
        Kind::Publish => {
            let record = objects::lookup(connection, &item.bucket, &item.name)?
                .ok_or_else(stale)?
                .record;
            if record.has_hold() && protection == OriginalProtection::Enforce {
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
            let complete: bool = connection
                .prepare_cached("SELECT complete FROM sync_roots WHERE root_id=?1")?
                .query_row([item.token.root], |r| r.get(0))?;
            if !complete || objects::lookup(connection, &item.bucket, &item.name)?.is_some() {
                return Err(stale());
            }
            let held: bool = connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM objects WHERE bucket=?1 AND name=?2 AND generation=?3 AND (event_based_hold=1 OR temporary_hold=1))")?.query_row(params![item.bucket,item.name,item.generation.get()],|r|r.get(0))?;
            if held {
                return Err(AdminError::Hold.into());
            }
            let valid: bool = connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?3 AND generation=?4 AND size=?5 AND md5=?6 AND crc32c=?7)")?.query_row(params![item.bucket,item.name,item.incarnation,item.generation.get(),item.size,item.md5,item.crc32c],|r|r.get(0))?;
            if !valid {
                return Err(stale());
            }
        }
    }
    Ok(())
}

/// Whether opening a parent chain also fsyncs each directory on the way.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ParentSync {
    Sync,
    Skip,
}

fn parent(root: &File, path: &str, sync: ParentSync) -> Result<Option<File>, Error> {
    let (parents, _) = path.rsplit_once('/').ok_or_else(stale)?;
    use crate::sync::tree::{ParentError, ParentOpen};
    let directory = match crate::sync::tree::parent(
        root,
        std::path::Path::new(parents),
        ParentOpen::Existing,
    ) {
        Ok(directory) => directory,
        Err(ParentError::Missing) => return Ok(None),
        Err(ParentError::Finding(_)) => return Err(stale()),
        Err(ParentError::Io(error)) => return Err(error.into()),
    };
    if sync == ParentSync::Sync {
        directory.sync_all()?;
    }
    Ok(Some(directory))
}
fn target(root: &File, path: &str) -> Result<Option<File>, Error> {
    let Some(directory) = parent(root, path, ParentSync::Skip)? else {
        return Ok(None);
    };
    let name = path.rsplit('/').next().ok_or_else(stale)?;
    match fsutil::child_regular(&directory, name.as_ref()) {
        Ok(fsutil::RegularChild::File(file)) => Ok(Some(file)),
        Ok(fsutil::RegularChild::Missing) => Ok(None),
        Ok(fsutil::RegularChild::Alias) => Err(stale()),
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
    let selected = workspace_directory(root, fsutil::DirectoryOpen::Existing)?;
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

fn workspace_open_error(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::NotFound || crate::sync::tree::structural_entry_error(&error)
    {
        Error::WorkspaceChanged
    } else {
        error.into()
    }
}

fn workspace_directory(root: &File, open: fsutil::DirectoryOpen) -> Result<File, Error> {
    #[cfg(test)]
    workspace_failure(root)?;
    fsutil::directory_at(root, mapping::WORKSPACE.as_ref(), open).map_err(workspace_open_error)
}
#[cfg(test)]
static WORKSPACE_FAILURES: std::sync::Mutex<Vec<(u64, u64, i32)>> =
    std::sync::Mutex::new(Vec::new());
#[cfg(test)]
fn workspace_failure(root: &File) -> Result<(), Error> {
    let identity = fsutil::stat_identity(root)?;
    let mut failures = WORKSPACE_FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(index) = failures
        .iter()
        .position(|(device, inode, _)| *device == identity.device && *inode == identity.inode)
    {
        let (_, _, errno) = failures.remove(index);
        return Err(io::Error::from_raw_os_error(errno).into());
    }
    Ok(())
}
fn fingerprint(
    root: &File,
    workspace: &File,
    item: &Pending,
    observed_ns: i128,
) -> Result<Option<SyncFingerprint>, Error> {
    if entry(workspace, &item.token.id)?.is_some() {
        return Err(Error::Protected);
    }
    let file = target(root, &item.path)?;
    let result = match (item.kind, file) {
        (Kind::Remove, None) => None,
        (Kind::Publish, Some(file)) => {
            let before = fsutil::stat_identity(&file)?;
            if before.device != fsutil::stat_identity(root)?.device || before.size != item.size {
                return Err(stale());
            }
            let fsutil::HashOutcome::Complete(hashes) =
                fsutil::hash_exact(&file, item.size, |_| Ok::<_, io::Error>(()))?
            else {
                return Err(stale());
            };
            file.sync_all()?;
            if before != fsutil::stat_identity(&file)?
                || hashes.md5 != item.md5
                || hashes.crc32c != item.crc32c
            {
                return Err(stale());
            }
            Some(SyncFingerprint::observed(before, hashes, observed_ns))
        }
        _ => return Err(stale()),
    };
    parent(root, &item.path, ParentSync::Sync)?;
    workspace.sync_all()?;
    root.sync_all()?;
    Ok(result)
}

fn verify_fingerprint(
    root: &File,
    workspace: &File,
    item: &Pending,
    expected: Option<&SyncFingerprint>,
) -> Result<(), Error> {
    if entry(workspace, &item.token.id)?.is_some() {
        return Err(Error::Protected);
    }
    match (item.kind, expected, target(root, &item.path)?) {
        (Kind::Remove, None, None) => {}
        (Kind::Publish, Some(expected), Some(file)) => {
            let identity = fsutil::stat_identity(&file)?;
            if identity.device != fsutil::stat_identity(root)?.device
                || !expected.matches_identity(identity)
            {
                return Err(stale());
            }
        }
        _ => return Err(stale()),
    }
    parent(root, &item.path, ParentSync::Sync)?;
    workspace.sync_all()?;
    root.sync_all()?;
    Ok(())
}

impl Writeback {
    pub(crate) fn with_root(&self, root: &RootLease) -> Result<Self, Error> {
        if self.identities() != (root.status().catalog_id, root.status().root_id) {
            return Err(stale());
        }
        Ok(Self {
            workspace: self.workspace.clone(),
            lease: root.clone(),
        })
    }
    async fn revalidate(&self) -> Result<(), Error> {
        self.fs(|_, _| Ok(())).await
    }
    pub(crate) fn root_lease(&self) -> &RootLease {
        &self.lease
    }
    async fn serial_with<T: Send + 'static, F>(
        &self,
        operation: impl FnOnce(Self) -> F + Send + 'static,
    ) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send + 'static,
    {
        let this = self.clone();
        serialized(&self.lease, async move { operation(this).await }).await
    }
    async fn db<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Connection) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let lease = self.lease.clone();
        self.lease
            .catalog()
            .run_admin_with_error(move |connection| {
                lease.control_scope().authorize(connection)?;
                let _lease = lease;
                job(connection)
            })
            .await
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
            self.lease.mark_workspace_changed().await?;
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
    pub(crate) async fn status(&self) -> Result<Status, Error> {
        self.serial_with(|this| async move { this.status_inner().await })
            .await
    }
    async fn status_inner(&self) -> Result<Status, Error> {
        self.revalidate().await?;
        let root = self.identities().1;
        self.db(move |c| status(c, root)).await
    }
    async fn page_inner(&self, after: i64, limit: usize) -> Result<Vec<Pending>, Error> {
        if after < 0 || !(1..=256).contains(&limit) {
            return Err(AdminError::Invalid("invalid pending page".into()).into());
        }
        self.revalidate().await?;
        let (catalog, root) = self.identities();
        self.db(move |c| {
            Ok(c.prepare_cached(SELECT_PENDING_PAGE)?
                .query_map(params![root, after, limit], |r| pending(r, catalog, root))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }
    async fn io_failure(&self, token: Token, stage: i64) -> Result<(), Error> {
        // Entry and target failures coexist until their own revision is revalidated.
        self.db(move |c| {
            let tx = c.transaction()?;
            checked(&tx, &token)?;
            tx.prepare_cached(
                "UPDATE sync_writeback_ops SET io_failure=io_failure | ?2 WHERE id=?1",
            )?
            .execute(params![token.id, stage])?;
            tx.prepare_cached("UPDATE sync_writeback_roots SET recovery=1 WHERE root_id=?1")?
                .execute([token.root])?;
            tx.commit()?;
            Ok(())
        })
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
                tx.prepare_cached("UPDATE sync_writeback_ops SET io_failure=io_failure & 2,revision=revision+1 WHERE id=?1 AND (io_failure & 1)!=0")?.execute([&token.id])?;
                tx.prepare_cached("UPDATE sync_writeback_roots SET io_failure=0 WHERE root_id=?1 AND io_failure=1")?.execute([token.root])?;
                let item=load(&tx,token.catalog,token.root,&token.id)?;tx.commit()?;Ok(item)
            }).await;
        };
        let next=self.db(move|c| {let tx=c.transaction()?;let old=checked(&tx,&token)?;let mut usage=status(&tx,token.root)?;
            if let Some(previous)=old.retained_bytes {usage.retained_bytes=usage.retained_bytes.checked_sub(u128::from(previous)).ok_or_else(stale)?;}
            else {usage.reserved_bytes=usage.reserved_bytes.checked_sub(u128::from(old.reserved_bytes)).ok_or_else(stale)?;usage.protected_entries+=1;}
            usage.retained_bytes+=u128::from(size);usage.recovery=true;
            save_status(&tx,token.root,&usage)?;tx.prepare_cached("UPDATE sync_writeback_ops SET phase=2,revision=revision+1,retained=?2,io_failure=(io_failure & 2) | ?3 WHERE id=?1")?.execute(params![token.id,size.to_be_bytes(),i64::from(failure.is_some())])?;
            tx.prepare_cached("UPDATE sync_writeback_roots SET io_failure=0 WHERE root_id=?1 AND io_failure=1")?.execute([token.root])?;
            let value=load(&tx,token.catalog,token.root,&token.id)?;tx.commit()?;Ok(value)
        }).await?;
        if let Some(kind) = failure {
            return Err(Error::Admin(io::Error::from(kind).into()));
        }
        Ok(next)
    }
    async fn resume_inner(&self) -> Result<Status, Error> {
        self.revalidate().await?;
        let root = self.identities().1;
        self.db(move |c| {
            let tx = c.transaction()?;
            let mut usage = status(&tx, root)?;
            if usage.entries > 0 || usage.over_limit() || usage.protected_entries > 0 {
                return Err(Error::RecoveryRequired);
            }
            usage.io_failure = false;
            usage.recovery = false;
            tx.prepare_cached("UPDATE sync_writeback_roots SET io_failure=0 WHERE root_id=?1")?
                .execute([root])?;
            save_status(&tx, root, &usage)?;
            usage.refresh();
            tx.commit()?;
            Ok(usage)
        })
        .await
    }
}
fn load(c: &Connection, catalog: CatalogId, root: RootId, id: &str) -> Result<Pending, Error> {
    c.prepare_cached(SELECT_PENDING)?
        .query_row(params![root, id], |r| pending(r, catalog, root))
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
async fn serialized<T: Send + 'static>(
    lease: &RootLease,
    future: impl std::future::Future<Output = Result<T, Error>> + Send + 'static,
) -> Result<T, Error> {
    let permit = lease
        .catalog()
        .wait_admin(
            lease.catalog().0.writeback_execution.clone(),
            lease.catalog().admin_deadline(),
        )
        .await?;
    // Cancellation can release a waiting request, but never an executing file/SQL transition.
    owned(async move {
        let _permit = permit;
        future.await
    })
    .await
}

async fn owned<T: Send + 'static>(
    future: impl std::future::Future<Output = Result<T, Error>> + Send + 'static,
) -> Result<T, Error> {
    crate::task::spawn_supervised("sync-writeback", future)
        .await
        .map_err(|e| Error::Admin(AdminError::backend(e)))?
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
