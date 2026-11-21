//! Durable write-back intent and protected recovery metadata; no adapter SQL or cleanup.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::super::{
    admin::{AdminError, BucketName, Generation, ObjectName},
    change_journal::JournalFence,
    objects, unavailable,
};
use super::{
    CatalogId, OpaqueId, Ordinal, PageLimit, SyncBinding, SyncFingerprint, SyncPath, SyncUpdate,
    durable::{self, Durable, NotDurable, SyncedDirectories},
    root::{Baseline, RootError, RootId, RootLease},
};
use crate::storage::catalog::Incarnation;
use crate::storage::catalog::sql;
use crate::storage::failure::{Link, TypedSource};
use crate::{
    bucket::CatalogError,
    fsutil::{self, NodeId},
    object::ObjectError,
    sync::{
        mapping::{self, PathMapping},
        naming::{Observation, ProbeError, RootNaming},
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{fs::File, io, sync::Arc};

mod body;
mod conflict_transfer;
mod live_export;
mod reason;
mod recovery;
mod target;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use live_export::faults::retrack_restored;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use live_export::tests::{
    armed_observation, observation as begin_observation, pending as live_pending,
};
pub(crate) use live_export::{InstallOutcome, LiveExport, LiveSource};
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use live_export::{
    inject_failure as inject_live_export_failure,
    inject_failure_after as inject_live_export_failure_after,
    inject_io_failure as inject_live_export_io_failure, inject_stale_fence,
};
pub(crate) use reason::{RaceReason, StaleReason};
use target::{TargetState, observe_target};

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
    /// The conflict store failed while write-back depended on it.
    Conflict(super::conflicts::Error),
    /// Installation failed after the target was displaced. Recovery owns the displaced bytes,
    /// so callers must not retry this automatically.
    Displaced(Box<Error>),
    Capacity,
    RecoveryRequired,
    Protected,
    WorkspaceChanged,
    /// The working tree changed from what the step read moments before.
    Raced(RaceReason),
    /// The catalog changed from the operation's plan.
    Obsolete(StaleReason),
    /// A state that write-back keeps broke: a bug. The step ends as a race does, as it did when
    /// the invariant was reported as a failed precondition.
    Invariant(&'static str),
}
impl Error {
    /// Whether the step met the tree or the catalog other than it read or planned them, by its
    /// own checks or by a catalog call that answered `Precondition`. Every such step ends the
    /// same way: its operation is cancelled or planned again. A tree changed under an
    /// installation attempt ([`Self::Raced`]), such as the directory at the target replaced, the
    /// path to the target no longer resolving, or an alias in the place of an entry, is a user's
    /// concurrent change, which a new export plans around, not a failure of the root: pausing the
    /// whole root over it would leave every other path unsynchronized until an operator rescans.
    /// An entry that kept failing to open ([`RaceReason::Unsettled`]) is no such change: the
    /// step fails, and its caller tries it again, as after any failure that passes by itself.
    fn changed(&self) -> bool {
        match self {
            Self::Admin(AdminError::Precondition)
            | Self::Raced(
                RaceReason::ParentChanged
                | RaceReason::Respelled
                | RaceReason::TargetChanged
                | RaceReason::DirectoryChanged,
            )
            | Self::Obsolete(_)
            | Self::Invariant(_) => true,
            Self::Admin(_)
            | Self::Root(_)
            | Self::Conflict(_)
            | Self::Displaced(_)
            | Self::Capacity
            | Self::RecoveryRequired
            | Self::Protected
            | Self::WorkspaceChanged
            | Self::Raced(RaceReason::Unsettled) => false,
        }
    }

    /// Whether the tree at the target is not the one the step read, or kept failing to open:
    /// either way the target holds nothing the step can still reach.
    fn unreachable(&self) -> bool {
        self.changed() || matches!(self, Self::Raced(RaceReason::Unsettled))
    }

    /// Whether the step's operation is moot: [`Self::changed`], or the catalog no longer holds
    /// what it names.
    fn superseded(&self) -> bool {
        self.changed() || matches!(self, Self::Admin(AdminError::NotFound))
    }

    /// Logs a broken invariant, which its caller then handles as a change of what the step read:
    /// the operation is cancelled or planned again, and this record is all that is left of the
    /// break. Any other error leaves no record here.
    pub(crate) fn record_invariant(&self) {
        if let Self::Invariant(broken) = self {
            record_invariant(broken);
        }
    }
}

/// Logs the invariant `broken`, which its caller handles as a concurrent change.
pub(crate) fn record_invariant(broken: &str) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Error,
        "sync_writeback_invariant",
        serde_json::json!({"reason":broken}),
    );
}
impl From<super::conflicts::Error> for Error {
    fn from(error: super::conflicts::Error) -> Self {
        Self::Conflict(error)
    }
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
        Self::Admin(e.into())
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("write-back: ")?;
        match self {
            Self::Admin(_) => f.write_str("admin operation failed"),
            Self::Root(_) => f.write_str("root validation failed"),
            Self::Conflict(_) => f.write_str("conflict store failed"),
            Self::Displaced(_) => f.write_str("installation failed after displacing the target"),
            Self::Capacity => f.write_str("Capacity"),
            Self::RecoveryRequired => f.write_str("RecoveryRequired"),
            Self::Protected => f.write_str("Protected"),
            Self::WorkspaceChanged => f.write_str("WorkspaceChanged"),
            Self::Raced(reason) => write!(f, "{reason}"),
            Self::Obsolete(reason) => write!(f, "{reason}"),
            Self::Invariant(invariant) => write!(f, "broken invariant: {invariant}"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.typed_source().map(Link::as_error)
    }
}
impl TypedSource for Error {
    fn typed_source(&self) -> Option<Link<'_>> {
        match self {
            Self::Admin(error) => Some(Link::Admin(error)),
            Self::Root(error) => Some(Link::Root(error)),
            Self::Conflict(error) => Some(Link::Conflict(error)),
            Self::Displaced(error) => Some(Link::Writeback(error)),
            Self::Capacity
            | Self::RecoveryRequired
            | Self::Protected
            | Self::WorkspaceChanged
            | Self::Raced(_)
            | Self::Obsolete(_)
            | Self::Invariant(_) => None,
        }
    }
}
/// A parent chain that no longer resolves means the target changed under this operation.
impl From<crate::sync::tree::ParentError> for Error {
    fn from(error: crate::sync::tree::ParentError) -> Self {
        use crate::sync::tree::ParentError;
        match error {
            ParentError::Io(error) => error.into(),
            ParentError::Finding(_) | ParentError::Missing(_) => {
                Self::Raced(RaceReason::ParentChanged)
            }
            // A directory that kept failing to open is read again by the export the race leads
            // to.
            ParentError::Transient => Self::Raced(RaceReason::Unsettled),
        }
    }
}
/// The root's usage held less than an operation accounts for.
fn unaccounted() -> Error {
    Error::Invariant("the root's usage does not hold an operation's bytes")
}
/// The directories of a write-back path, which always lies in a bucket directory.
fn parent_path(path: &str) -> Result<&str, Error> {
    path.rsplit_once('/')
        .map(|(parents, _)| parents)
        .ok_or(Error::Invariant(
            "a write-back target lies outside a bucket directory",
        ))
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
impl Phase {
    /// The phase's spelling in findings.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Armed => "armed",
            Self::Protected => "protected",
            Self::Durable => "durable",
            // Completed operations are excluded from every listing that prints a phase.
            Self::Complete => "complete",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    catalog: CatalogId,
    root: RootId,
    id: OpaqueId,
    revision: i64,
}
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub token: Token,
    pub ordinal: Ordinal,
    pub owner: String,
    pub bucket: BucketName,
    pub incarnation: Incarnation,
    pub name: ObjectName,
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
/// Why write-back stopped: every state but `Ready`, each of which an operator must repair, so
/// that what holds synchronization back cannot name a ready write-back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Halt {
    RecoveryRequired,
    ProtectedBody,
    ConflictCapacity,
    IoFailure,
}
impl Stop {
    /// Why write-back stopped, or nothing while it is ready.
    pub(crate) const fn halt(self) -> Option<Halt> {
        match self {
            Self::Ready => None,
            Self::RecoveryRequired => Some(Halt::RecoveryRequired),
            Self::ProtectedBody => Some(Halt::ProtectedBody),
            Self::ConflictCapacity => Some(Halt::ConflictCapacity),
            Self::IoFailure => Some(Halt::IoFailure),
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Status {
    pub reserved_bytes: u128,
    pub retained_bytes: u128,
    pub entries: u64,
    pub protected_entries: u64,
    limits: Limits,
    recovery: bool,
    io_failure: bool,
    conflicts: super::conflicts::Usage,
    captured_entries: u64,
    /// Whether an operation is left unfinished, which [`Self::stop`] does not say by itself
    /// until recovery is required.
    unfinished: bool,
}
impl Status {
    /// Whether write-back neither stops nor holds an operation that is left unfinished, so that
    /// a failed export left nothing for recovery to finish.
    pub(crate) fn idle(&self) -> bool {
        self.stop() == Stop::Ready && !self.unfinished
    }

    /// Why write-back is paused, derived from the counters it was read with.
    pub(crate) fn stop(&self) -> Stop {
        if self.io_failure {
            Stop::IoFailure
        } else if self.over_limit() {
            Stop::ConflictCapacity
        } else if self.protected_entries > 0 {
            Stop::ProtectedBody
        } else if self.recovery {
            Stop::RecoveryRequired
        } else {
            Stop::Ready
        }
    }
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
}

/// The `sync_writeback_roots` row before operation failures and conflict usage are added.
struct RootRow {
    limits: Limits,
    reserved_bytes: u128,
    retained_bytes: u128,
    entries: u64,
    protected_entries: u64,
    recovery: bool,
    io_failure: bool,
}

impl RootRow {
    const SELECT: &str = "SELECT max_bytes,max_entries,reserved,retained,entries,protected,recovery,io_failure FROM sync_writeback_roots WHERE root_id=?1";

    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            limits: Limits {
                bytes: row.get(0)?,
                entries: row.get(1)?,
            },
            reserved_bytes: u128::from_be_bytes(row.get(2)?),
            retained_bytes: u128::from_be_bytes(row.get(3)?),
            entries: row.get(4)?,
            protected_entries: row.get(5)?,
            recovery: row.get(6)?,
            io_failure: row.get(7)?,
        })
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
        let expected:Option<NodeId>=self.catalog().run_admin(move|c| {
            let _owner=owner;
            sql::row(c, "SELECT workspace_device,workspace_inode FROM sync_writeback_roots WHERE root_id=?1", [root],|r|Ok(NodeId::from_be_bytes(r.get(0)?,r.get(1)?))).optional().map_err(unavailable)
        }).await?;
        let directory = self
            .file_job(move |root| Ok(select_workspace(root, expected)))
            .await?;
        let directory = match directory {
            Ok(directory) => directory,
            Err(error @ Error::WorkspaceChanged) => {
                self.mark_workspace_changed().await?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let node = fsutil::stat_identity(&directory)?.node();
        let state = Writeback {
            lease: self.clone(),
            workspace: Arc::new(directory),
        };
        state.db(move |connection| {
            let (device, inode) = node.to_be_bytes();
            connection.prepare_cached("INSERT INTO sync_writeback_roots(root_id,max_bytes,max_entries,reserved,retained,entries,protected,recovery,io_failure,workspace_device,workspace_inode,sequence,epoch) VALUES(?1,?2,?3,?4,?4,0,0,0,0,?5,?6,NULL,NULL) ON CONFLICT(root_id) DO UPDATE SET max_bytes=excluded.max_bytes,max_entries=excluded.max_entries WHERE workspace_device=excluded.workspace_device AND workspace_inode=excluded.workspace_inode")?
                .execute(params![root,limits.bytes,limits.entries,[0u8;16],device,inode])?;
            let current=connection.prepare_cached("SELECT workspace_device,workspace_inode FROM sync_writeback_roots WHERE root_id=?1")?.query_row([root],|r|Ok(NodeId::from_be_bytes(r.get(0)?,r.get(1)?)))?;
            if current!=node {return Err(Error::WorkspaceChanged);}
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
    let id: OpaqueId = row.get(0)?;
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
        generation: row.get(7)?,
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
    let row = connection
        .prepare_cached(RootRow::SELECT)?
        .query_row([root], RootRow::read)?;
    let (unfinished, failed_operation): (bool, bool) = connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_writeback_ops WHERE root_id=?1 AND phase!=4),EXISTS(SELECT 1 FROM sync_writeback_ops WHERE root_id=?1 AND phase!=4 AND io_failure!=0)")?.query_row([root],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let conflicts = super::conflicts::usage(connection, root)?;
    let captured_entries = super::conflicts::captured_entries(connection, root)?;
    if captured_entries > row.entries {
        return Err(Error::Obsolete(StaleReason::Usage));
    }
    Ok(Status {
        reserved_bytes: row.reserved_bytes,
        retained_bytes: row.retained_bytes,
        entries: row.entries,
        protected_entries: row.protected_entries,
        limits: row.limits,
        recovery: row.recovery,
        io_failure: row.io_failure || failed_operation,
        conflicts,
        captured_entries,
        unfinished,
    })
}
pub(super) fn observation(
    connection: &Connection,
    root: RootId,
) -> Result<super::observation::WritebackSummary, Error> {
    let value = status(connection, root)?;
    Ok(super::observation::WritebackSummary {
        state: value.stop(),
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
    let valid: bool = connection.prepare_cached("SELECT enabled AND epoch=?1 AND ?2 BETWEEN floor AND sequence FROM change_journal_state WHERE singleton=1")?.query_row(params![fence.revision,i64::try_from(fence.sequence.get()).map_err(|_|Error::Invariant("a journal sequence exceeds the catalog's integers"))?], |r|r.get(0))?;
    if !valid {
        return Err(Error::Obsolete(StaleReason::JournalFence));
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
    let current: Option<(String,Incarnation)> = connection.prepare_cached("SELECT b.project,i.incarnation FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?1")?.query_row([item.bucket.as_str()], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if current.as_ref() != Some(&(item.owner.clone(), item.incarnation)) {
        return Err(Error::Obsolete(StaleReason::Bucket));
    }
    match item.kind {
        Kind::Publish => {
            let record = objects::lookup(connection, item.bucket.as_str(), item.name.as_str())?
                .ok_or(Error::Obsolete(StaleReason::Object))?
                .record;
            if record.has_hold() && protection == OriginalProtection::Enforce {
                return Err(AdminError::Hold.into());
            }
            if record.generation != item.generation.get()
                || record.size != item.size
                || record.md5 != item.md5
                || record.crc32c != item.crc32c
            {
                return Err(Error::Obsolete(StaleReason::Object));
            }
        }
        Kind::Remove => {
            if Baseline::read(connection, item.token.root)? != Baseline::Complete {
                return Err(Error::Obsolete(StaleReason::Baseline));
            }
            if objects::lookup(connection, item.bucket.as_str(), item.name.as_str())?.is_some() {
                return Err(Error::Obsolete(StaleReason::Object));
            }
            let held: bool = connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM objects WHERE bucket=?1 AND name=?2 AND generation=?3 AND (event_based_hold=1 OR temporary_hold=1))")?.query_row(params![item.bucket,item.name,item.generation],|r|r.get(0))?;
            if held {
                return Err(AdminError::Hold.into());
            }
            let valid: bool = connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?3 AND generation=?4 AND size=?5 AND md5=?6 AND crc32c=?7)")?.query_row(params![item.bucket,item.name,item.incarnation,item.generation,item.size,item.md5,item.crc32c],|r|r.get(0))?;
            if !valid {
                return Err(Error::Obsolete(StaleReason::Tracked));
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

fn parent(
    root: &File,
    naming: RootNaming,
    lookup: &fsutil::Lookup,
    path: &str,
    sync: ParentSync,
) -> Result<Option<File>, Error> {
    let parents = parent_path(path)?;
    use crate::sync::tree::{ParentError, ParentOpen};
    let directory = match crate::sync::tree::parent(
        root,
        naming,
        lookup,
        std::path::Path::new(parents),
        ParentOpen::Existing,
    ) {
        Ok(directory) => directory,
        Err(ParentError::Missing(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if sync == ParentSync::Sync {
        directory.sync_all()?;
    }
    Ok(Some(directory))
}
fn entry(workspace: &File, lookup: &fsutil::Lookup, id: OpaqueId) -> Result<Option<File>, Error> {
    present(lookup.regular_unconfirmed(workspace, id.to_hex().as_ref()))
}
/// Like [`entry`], but open for writing, so that a retried copy can empty and refill the private
/// copy a failed attempt left. A private copy that another name also links is `Protected`:
/// emptying it would empty that name too.
fn refillable_entry(workspace: &File, id: OpaqueId) -> Result<Option<File>, Error> {
    let name = id.to_hex();
    let file = match fsutil::open_regular_writable_at(workspace, name.as_ref()) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        // A macOS clone keeps its source's mode, so a copy cloned from a read-only blob, such as
        // one that publication cloned from a read-only seed or ingested file, cannot be opened
        // for writing. Only that copy is removed for a new clone, which keeps the mode the
        // installed file would have had. Linux creates every private copy itself with mode 0o600
        // and fills it through the descriptor, so it never removes one here.
        #[cfg(target_os = "macos")]
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            fsutil::unlink_file_at(workspace, name.as_ref())?;
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    if !fsutil::sole_regular_link(&file)? {
        return Err(Error::Protected);
    }
    Ok(Some(file))
}
/// A workspace entry the lookup proved missing is `None`, and any other failure to open it is an
/// error. One that kept failing to open while its name still held an entry is a race the
/// operation is read again after: taken for none, a body an exchange displaced into the
/// workspace would stay there with nothing tracking it.
fn present(opened: io::Result<fsutil::Opened<File>>) -> Result<Option<File>, Error> {
    match opened? {
        fsutil::Opened::Entry(file) => Ok(Some(file)),
        fsutil::Opened::Missing(_) => Ok(None),
        fsutil::Opened::Transient => Err(Error::Raced(RaceReason::Unsettled)),
    }
}
fn verify_workspace(root: &File, workspace: &File) -> Result<(), Error> {
    let selected = workspace_directory(root, fsutil::DirectoryOpen::Existing)?;
    let selected = fsutil::stat_identity(&selected)?;
    let owned = fsutil::stat_identity(workspace)?;
    if selected.node() != owned.node() || owned.device != fsutil::stat_identity(root)?.device {
        return Err(Error::WorkspaceChanged);
    }
    Ok(())
}

/// Opens the workspace and checks it is the one this root recorded; the first use creates it.
fn select_workspace(root: &File, recorded: Option<NodeId>) -> Result<File, Error> {
    let directory = workspace_directory(
        root,
        if recorded.is_none() {
            fsutil::DirectoryOpen::CreateIfMissing
        } else {
            fsutil::DirectoryOpen::Existing
        },
    )?;
    let identity = fsutil::stat_identity(&directory)?;
    if identity.device != fsutil::stat_identity(root)?.device
        || recorded.is_some_and(|node| node != identity.node())
    {
        return Err(Error::WorkspaceChanged);
    }
    if recorded.is_none() {
        directory.sync_all()?;
        root.sync_all()?;
    }
    Ok(directory)
}

/// Runs `job` between two workspace checks; a failed check replaces the job's own result.
fn verified<T>(
    root: &File,
    workspace: &File,
    job: impl FnOnce(&File, &File) -> Result<T, Error>,
) -> Result<T, Error> {
    verify_workspace(root, workspace)?;
    let result = job(root, workspace);
    verify_workspace(root, workspace)?;
    result
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
    naming: RootNaming,
    lookup: &fsutil::Lookup,
    item: &Pending,
    observed_ns: i128,
) -> Result<Option<SyncFingerprint>, Error> {
    if entry(workspace, lookup, item.token.id)?.is_some() {
        return Err(Error::Protected);
    }
    let file = observe_target(root, naming, lookup, &item.path)?.into_file()?;
    let changed = Error::Raced(RaceReason::TargetChanged);
    let result = match (item.kind, file) {
        (Kind::Remove, None) => None,
        (Kind::Publish, Some(file)) => {
            let body::Match::Same(before) = body::same_content(&file, item.into())? else {
                return Err(changed);
            };
            if before.device != fsutil::stat_identity(root)?.device {
                return Err(changed);
            }
            file.sync_all()?;
            if before != fsutil::stat_identity(&file)? {
                return Err(changed);
            }
            let hashes = crate::hashing::Hashes {
                md5: item.md5,
                crc32c: item.crc32c,
            };
            Some(SyncFingerprint::observed(before, hashes, observed_ns))
        }
        (Kind::Remove, Some(_)) | (Kind::Publish, None) => return Err(changed),
    };
    parent(root, naming, lookup, &item.path, ParentSync::Sync)?;
    workspace.sync_all()?;
    root.sync_all()?;
    Ok(result)
}

/// Checks that the target still holds what `expected` recorded, and makes that durable before
/// the acknowledgement records it. A published file comes with the proof its tracking row needs:
/// it may be one the user wrote that the export only observed, whose entry and the new
/// directories above it nobody synced, so every directory up to the root is synced, not just
/// its parent.
fn verify_fingerprint(
    root: &File,
    workspace: &File,
    naming: RootNaming,
    lookup: &fsutil::Lookup,
    item: &Pending,
    expected: Option<&SyncFingerprint>,
) -> Result<Option<Durable>, Error> {
    if entry(workspace, lookup, item.token.id)?.is_some() {
        return Err(Error::Protected);
    }
    let durable = match (
        item.kind,
        expected,
        observe_target(root, naming, lookup, &item.path)?.into_file()?,
    ) {
        (Kind::Remove, None, None) => {
            parent(root, naming, lookup, &item.path, ParentSync::Sync)?;
            None
        }
        (Kind::Publish, Some(expected), Some(file)) => {
            let identity = fsutil::stat_identity(&file)?;
            if identity.device != fsutil::stat_identity(root)?.device
                || !expected.matches_identity(identity)
            {
                return Err(Error::Raced(RaceReason::TargetChanged));
            }
            let path = SyncPath::try_from(item.path.clone())?;
            match durable::make_durable(
                root,
                lookup,
                &path,
                identity,
                &mut SyncedDirectories::default(),
            ) {
                Ok(durable) => Some(durable),
                Err(NotDurable::Changed) => return Err(Error::Raced(RaceReason::TargetChanged)),
                Err(NotDurable::Io(error)) => return Err(error.into()),
            }
        }
        (Kind::Remove, None, Some(_)) | (Kind::Publish, Some(_), None) => {
            return Err(Error::Raced(RaceReason::TargetChanged));
        }
        (Kind::Remove, Some(_), _) | (Kind::Publish, None, _) => {
            return Err(Error::Invariant(
                "an operation's fingerprint does not match its kind",
            ));
        }
    };
    workspace.sync_all()?;
    root.sync_all()?;
    Ok(durable)
}

impl Writeback {
    pub(crate) fn with_root(&self, root: &RootLease) -> Result<Self, Error> {
        if self.identities() != (root.status().catalog_id, root.status().root_id) {
            return Err(Error::Obsolete(StaleReason::Root));
        }
        Ok(Self {
            workspace: self.workspace.clone(),
            lease: root.clone(),
        })
    }
    async fn revalidate(&self) -> Result<(), Error> {
        self.fs(|_, _| Ok(())).await
    }
    /// Probes the root's naming rules in a fixed subdirectory of this verified workspace. The
    /// outer error is the workspace check; the inner one is the probe's own.
    pub(crate) async fn probe_naming(&self) -> Result<Result<Observation, ProbeError>, Error> {
        self.fs(|root, workspace| Ok(crate::sync::naming::probe(root, workspace)))
            .await
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
            .file_job(move |root| Ok(verified(root, &workspace, job)))
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
            return Err(Error::Obsolete(StaleReason::Root));
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
    async fn page_inner(
        &self,
        after: Ordinal,
        limit: PageLimit<256>,
    ) -> Result<Vec<Pending>, Error> {
        self.revalidate().await?;
        let (catalog, root) = self.identities();
        self.db(move |c| {
            Ok(c.prepare_cached(SELECT_PENDING_PAGE)?
                .query_map(params![root, after, limit], |r| pending(r, catalog, root))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }
    /// Marks a failed workspace entry (bit 1) and requires recovery. Catalogs written by older
    /// releases may also carry bit 2, a failed target, which the masks in `inspect_inner` keep.
    async fn mark_entry_failure(&self, token: Token) -> Result<(), Error> {
        self.db(move |c| {
            let tx = c.transaction()?;
            checked(&tx, &token)?;
            tx.prepare_cached(
                "UPDATE sync_writeback_ops SET io_failure=io_failure | 1 WHERE id=?1",
            )?
            .execute([token.id])?;
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
            return Err(Error::Obsolete(StaleReason::Completed));
        }
        let id = token.id;
        let lookup = self.lease.private_lookup();
        let observed = self
            .fs(move |root, workspace| {
                let Some(file) = entry(workspace, &lookup, id)? else {
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
                self.mark_entry_failure(token.clone()).await?;
                return Err(e);
            }
        };
        let Some((size, failure)) = observed else {
            if item.phase == Phase::Protected {
                self.mark_entry_failure(token.clone()).await?;
                return Err(Error::Protected);
            }
            return self.db(move|c| {
                let tx=c.transaction()?;checked(&tx,&token)?;
                tx.prepare_cached("UPDATE sync_writeback_ops SET io_failure=io_failure & 2,revision=revision+1 WHERE id=?1 AND (io_failure & 1)!=0")?.execute([&token.id])?;
                tx.prepare_cached("UPDATE sync_writeback_roots SET io_failure=0 WHERE root_id=?1 AND io_failure=1")?.execute([token.root])?;
                let item=load(&tx,token.catalog,token.root,token.id)?;tx.commit()?;Ok(item)
            }).await;
        };
        let next=self.db(move|c| {let tx=c.transaction()?;let old=checked(&tx,&token)?;let mut usage=status(&tx,token.root)?;
            if let Some(previous)=old.retained_bytes {usage.retained_bytes=usage.retained_bytes.checked_sub(u128::from(previous)).ok_or_else(unaccounted)?;}
            else {usage.reserved_bytes=usage.reserved_bytes.checked_sub(u128::from(old.reserved_bytes)).ok_or_else(unaccounted)?;usage.protected_entries+=1;}
            usage.retained_bytes+=u128::from(size);usage.recovery=true;
            save_status(&tx,token.root,&usage)?;tx.prepare_cached("UPDATE sync_writeback_ops SET phase=2,revision=revision+1,retained=?2,io_failure=(io_failure & 2) | ?3 WHERE id=?1")?.execute(params![token.id,size.to_be_bytes(),i64::from(failure.is_some())])?;
            tx.prepare_cached("UPDATE sync_writeback_roots SET io_failure=0 WHERE root_id=?1 AND io_failure=1")?.execute([token.root])?;
            let value=load(&tx,token.catalog,token.root,token.id)?;tx.commit()?;Ok(value)
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
            tx.commit()?;
            Ok(usage)
        })
        .await
    }
}
fn load(c: &Connection, catalog: CatalogId, root: RootId, id: OpaqueId) -> Result<Pending, Error> {
    c.prepare_cached(SELECT_PENDING)?
        .query_row(params![root, id], |r| pending(r, catalog, root))
        .optional()?
        .ok_or(Error::Obsolete(StaleReason::Revision))
}
fn checked(c: &Connection, token: &Token) -> Result<Pending, Error> {
    let item = load(c, token.catalog, token.root, token.id)?;
    if item.token.revision != token.revision {
        return Err(Error::Obsolete(StaleReason::Revision));
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
