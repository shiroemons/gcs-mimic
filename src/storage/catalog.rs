use super::lock;
use crate::auth::{Identity, OBJECTS_DELETE, Policy};
use crate::bucket::{
    BucketAccess, BucketConditions, BucketMetadataUpdate, BucketRecord, CatalogError, LabelPart,
    UNIFORM_ACCESS_LOCK_PERIOD, cors_from_value, cors_value, valid_label, validate_cors,
};
use crate::clock::{Clock, SystemClock};
use crate::storage::failure::{BackendCause, BackendFailure, unavailable};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params, types::Type};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Notify, Semaphore};

mod activity;
use activity::{Activity, ActivityGate, ExclusiveGuard, Work};

#[cfg(test)]
mod memory_tests;
#[cfg(test)]
mod timestamp_tests;

// Keep crate-local fixtures separate from schema helpers included by integration tests.
#[cfg(test)]
pub(crate) mod test_fixtures;
#[cfg(test)]
pub(super) mod test_support;

#[cfg(test)]
pub(crate) mod fault;

pub(crate) mod resumable;
mod resumable_transfer;
mod upload_replay;
pub(crate) use resumable::ResumableError;
#[cfg(test)]
pub(crate) use resumable::ResumableSession;
pub(crate) use resumable::{ResumableOptions, SessionState};
pub(crate) use resumable_transfer::ResumableChunk;
pub(crate) use upload_replay::UploadAclSnapshot;
pub(crate) use upload_replay::{CopyReplay, CopyReplaySource, ReplayInvocation};

pub(crate) mod lifecycle;
mod resumable_maintenance;
mod session_limits;
pub(crate) mod xml_multipart;

mod executor;
mod principals;
mod read_cache;
mod read_pool;
mod sql;

mod resumable_recovery;

mod compose;
pub(crate) use compose::SourceDisposal;
mod compose_crc32c;
pub(crate) mod rewrite;
mod rewrite_token;

mod customer_keys;
mod md5_availability;
mod object_hold;

mod appendable;
pub(crate) use appendable::{AppendError, AppendWriter, Finality};

mod object_move;
pub(crate) use object_move::{MoveAccess, MoveConditions, MoveNames};
mod bucket_soft_delete;
mod object_owner;
mod objects;
mod soft_delete;
pub(crate) use objects::{
    FoldersAsPrefixes, ListQueryError, ObjectListCursor, ObjectListPage, ObjectListQuery,
    ObjectReader, ObjectUpload,
};
pub(crate) use soft_delete::RestoreAccess;

mod blob_backend;
mod derived;
use blob_backend::BlobBackend;
mod transfer_io;
#[cfg(test)]
pub(crate) use derived::DerivedTransfer;
pub(crate) use derived::{
    DerivedDestination, DerivedRequestGuard, DerivedSource, DerivedSourceSpec,
};

pub mod admin;
pub(crate) mod bucket_autoclass;
mod bucket_timestamps;
mod notification_outbox;
mod notifications;
mod policies;
mod sync_control;
use policies::{check_access, insert_policy};

pub(crate) mod change_journal;

pub mod import_journal;

pub mod sync_state;

pub(crate) mod hmac_keys;

pub(crate) mod acl;

mod bucket_list;
mod bucket_update;
mod buckets;
mod close;
mod connection;
mod open;
mod schema;
#[cfg(test)]
pub(crate) use bucket_list::BucketListPage;
#[cfg(test)]
use bucket_list::bucket_list_page;
use bucket_list::bucket_metadata_charge;
#[cfg(test)]
use bucket_update::BucketWrite;
pub(super) use buckets::select_bucket;
use buckets::{
    BucketCreation, create_bucket_in_transaction, insert_bucket_in_transaction, read_bucket,
};
use connection::ConnectionUnavailable;
#[cfg(test)]
use open::immutable_catalog_uri;
#[cfg(test)]
use schema::{
    BUCKET_LIFECYCLE_SCHEMA, BUCKET_RETENTION_SCHEMA, BUCKET_SCHEMA, BUCKET_SETTINGS_SCHEMA,
    BUCKET_UNIFORM_ACCESS_SCHEMA, Migration, OBJECT_HOLDS_CUSTOM_TIME_SCHEMA, SOFT_DELETE_SCHEMA,
    migrate_timestamp_precision,
};
use schema::{MIGRATIONS, initialize_catalog, validate_schema, validate_schema_version};

/// Calls beyond this many wait for admission rather than for the execution permit. Execution
/// finishes what it has queued even while closing, so this bounds the jobs left to run at
/// shutdown; `close` fails the admission waiters at once.
const MAX_IN_FLIGHT: usize = 32;
const MAX_LIST_LIMIT: usize = 1001;
const BUCKET_LIST_METADATA_BUDGET: usize = 256 * 1024;
pub(super) const CATALOG_SCHEMA_VERSION: i64 = 53;
// A literal macro lets per-request queries build static SQL with concat!, which
// the connection's prepared statement cache reuses without formatting.
macro_rules! bucket_columns {
    () => {
        "name, project, location, location_type, storage_class, labels,
                      metageneration, time_created, updated, versioning_enabled,
                      default_event_based_hold, cors, public_access_prevention_enforced,
                      uniform_bucket_level_access_locked_time, lifecycle, retention_policy, soft_delete_policy,
                      object_retention_enabled, autoclass, time_created_nanos, updated_nanos, generation, versioning_configured"
    };
}
use bucket_columns;

fn bucket_incarnation(connection: &Connection, bucket: &str) -> rusqlite::Result<String> {
    sql::scalar(
        connection,
        "SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1",
        [bucket],
    )
}

struct Inner {
    principals: OnceLock<principals::Profile>,
    activity: ActivityGate,
    resources: Mutex<Option<Resources>>,
    admission: Arc<Semaphore>,
    execution: Arc<Semaphore>,
    executor: executor::Executor,
    /// Upload commits waiting for the writer, which its next holder publishes together.
    upload_commits: objects::CommitQueue,
    /// Absent for a RAM catalog, whose database exists only inside the writer's connection.
    read_pool: Option<read_pool::ReadPool>,
    parent_policy: Arc<read_pool::ParentPolicy>,
    read_cache: read_cache::ReadCache,
    reclamation: Arc<Semaphore>,
    uploads: Mutex<UploadState>,
    uploads_idle: Notify,
    admin_uploads: Arc<Semaphore>,
    admin_reads: Arc<Semaphore>,
    staged_handles: Arc<Semaphore>,
    publication_batch: Arc<Semaphore>,
    /// Held by a seed batch from before it lets go of its lease until its commit ends, while
    /// nothing else keeps a reset from clearing the bodies it already put in the RAM store.
    seed_commit: Arc<Semaphore>,
    /// Held by a publication request until its batch and every staged cleanup it left are done,
    /// which is what `close` waits for. Callers past the permits wait for one like any other slot.
    publication_requests: Arc<Semaphore>,
    writeback_execution: Arc<Semaphore>,
    file_workers: admin::FileWorkers,
    admin_shutdown: Notify,
    sync_control: Mutex<Option<crate::sync::control::Registration>>,
    local_admin: Option<admin::AdminGrant>,
    change_wake: Notify,
    notification_wake: Arc<Notify>,
    notification_started: AtomicBool,
    /// The local Pub/Sub emulator, set once when the publisher starts; also answers topic lookups.
    pubsub_endpoint: OnceLock<crate::notification::Endpoint>,
    /// Retention a new bucket without an explicit softDeletePolicy receives, an
    /// `Option<NonZeroU64>` in atomic form: read it through `default_soft_delete_retention()`.
    default_soft_delete_retention: AtomicU64,
    notification_attempts: Arc<Semaphore>,
    data_dir: PathBuf,
    data_dir_state: DataDirState,
    backend: BlobBackend,
    clock: Arc<dyn Clock>,
    session_writers: Mutex<resumable::writers::SessionWriters>,
    /// Wakes requests waiting for a writer. One waker serves every session: a release with no
    /// waiter costs an atomic check, and waiters on a busy session are few and short-lived.
    writer_released: Notify,
    session_limits: OnceLock<session_limits::SessionLimits>,
    derived_wake: Notify,
    maintenance_wake: Arc<Notify>,
    lifecycle: lifecycle::LifecycleSchedule,
    /// Whether a bucket delete may have left XML multipart uploads behind.
    multipart_orphan_scan: AtomicBool,
    maintenance_error: Mutex<Option<BackendCause>>,
    maintenance_started: AtomicBool,
    maintenance_health: crate::task::TaskHealth,
    /// Workers started beside the catalog whose health readiness also reports. Each worker
    /// registers once and is never removed: a worker that stops for good must keep readiness
    /// off for the catalog's lifetime, and the server starts each such worker once per catalog,
    /// so the list stays as short as the set of workers.
    background_health: Mutex<Vec<crate::task::TaskHealth>>,
    #[cfg(test)]
    fail_rollback: AtomicBool,
    #[cfg(test)]
    maintenance_panics: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    admission_waiters: std::sync::atomic::AtomicUsize,
}

struct Resources {
    // Drop the connection before releasing directory ownership, including on cancelled calls.
    connection: Connection,
    lock: Option<lock::DirectoryLock>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Weak background loops need a stored wake permit even when the last owner disappears between polls.
        self.notification_wake.notify_one();
        self.maintenance_wake.notify_one();
    }
}

#[derive(Default)]
struct UploadState {
    active: usize,
    closed: bool,
}

/// The admission held through publication and cleanup.
enum UploadSlot {
    Counted(Catalog),
    Admin {
        _permit: tokio::sync::OwnedSemaphorePermit,
    },
}

pub(crate) struct UploadGuard {
    slot: UploadSlot,
    replay: Option<upload_replay::UploadReplay>,
    _activity: Activity,
}

impl UploadGuard {
    fn admin(permit: tokio::sync::OwnedSemaphorePermit, activity: Activity) -> Self {
        Self {
            slot: UploadSlot::Admin { _permit: permit },
            replay: None,
            _activity: activity,
        }
    }
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        if let UploadSlot::Counted(catalog) = &self.slot {
            let mut uploads = catalog
                .0
                .uploads
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            uploads.active -= 1;
            if uploads.active == 0 {
                catalog.0.uploads_idle.notify_waiters();
            }
        }
    }
}

/// A single SQLite connection. Metadata jobs execute serially, and callers wait for their turn.
#[derive(Clone)]
pub struct Catalog(Arc<Inner>);

/// Whether this open initialized the catalog schema under directory ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DataDirState {
    Created,
    Opened,
}

impl Catalog {
    /// Returns startup facts without resolving symlinks or inspecting the filesystem again.
    pub fn data_directory(&self) -> Option<(&Path, DataDirState)> {
        match self.0.backend {
            BlobBackend::Disk(_) => Some((&self.0.data_dir, self.0.data_dir_state)),
            BlobBackend::Memory(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn active_uploads(&self) -> usize {
        self.0.uploads.lock().unwrap().active
    }

    #[cfg(test)]
    pub(crate) fn session_writers_idle(&self) -> bool {
        self.0.session_writers.lock().unwrap().is_empty()
    }

    /// Arms a one-shot fault for the `skip + 1`-th time a write path reaches `step`.
    #[cfg(test)]
    pub(crate) fn inject(&self, step: fault::Step, skip: usize, fault: fault::Fault) {
        fault::arm(&self.0.data_dir, step, skip, fault);
    }

    /// The number of injected faults no write path has reached yet.
    #[cfg(test)]
    pub(crate) fn pending_faults(&self) -> usize {
        fault::pending(&self.0.data_dir)
    }

    pub fn is_healthy(&self) -> bool {
        !self.0.resources.is_poisoned()
            && self.0.backend.is_healthy()
            && !self.0.session_writers.is_poisoned()
            && !self.0.admission.is_closed()
            && !self.0.admin_uploads.is_closed()
            && !self.0.admin_reads.is_closed()
    }
}

/// A hard delete time, or a clock reading compared with one: whole microseconds since the Unix
/// epoch. A maintenance pass also handles session deadlines in milliseconds as plain `i64`; this
/// type keeps a reading in one unit from reaching a sweep that expects the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct UnixMicros(i64);

impl UnixMicros {
    fn system_time(self) -> rusqlite::Result<SystemTime> {
        system_time_micros(self.0)
    }
}

impl TryFrom<SystemTime> for UnixMicros {
    type Error = CatalogError;

    fn try_from(time: SystemTime) -> Result<Self, CatalogError> {
        unix_micros(time).map(Self)
    }
}

impl rusqlite::ToSql for UnixMicros {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

impl rusqlite::types::FromSql for UnixMicros {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        i64::column_result(value).map(Self)
    }
}

fn unix_millis(time: SystemTime) -> Result<i64, CatalogError> {
    unix_timestamp(time, 1_000_000)
}

fn unix_micros(time: SystemTime) -> Result<i64, CatalogError> {
    unix_timestamp(time, 1000)
}

fn unix_timestamp(time: SystemTime, nanos_per_unit: i128) -> Result<i64, CatalogError> {
    let nanos = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as i128,
        Err(error) => -(error.duration().as_nanos() as i128),
    };
    i64::try_from(nanos.div_euclid(nanos_per_unit)).map_err(unavailable)
}

fn system_time_millis(millis: i64) -> rusqlite::Result<SystemTime> {
    signed_system_time(millis, Duration::from_millis)
}

fn system_time_micros(micros: i64) -> rusqlite::Result<SystemTime> {
    signed_system_time(micros, Duration::from_micros)
}

/// Converts a signed count of `unit`s since the Unix epoch.
fn signed_system_time(value: i64, unit: fn(u64) -> Duration) -> rusqlite::Result<SystemTime> {
    let duration = unit(value.unsigned_abs());
    let time = if value < 0 {
        UNIX_EPOCH.checked_sub(duration)
    } else {
        UNIX_EPOCH.checked_add(duration)
    };
    time.ok_or_else(|| {
        sql::conversion_error(0, Type::Integer, "timestamp outside SystemTime range")
    })
}

#[cfg(test)]
mod tests;
