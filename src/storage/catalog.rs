use super::lock;
use crate::auth::{Identity, OBJECTS_DELETE, Policy};
use crate::bucket::{
    BucketAccess, BucketConditions, BucketMetadataUpdate, BucketRecord, CatalogError, LabelPart,
    UNIFORM_ACCESS_LOCK_PERIOD, cors_from_value, cors_value, valid_label, validate_cors,
};
use crate::cause::{BackendCause, BackendFailure};
use crate::clock::{Clock, SystemClock, signed_nanos};
use crate::storage::failure::unavailable;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, types::Type};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
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

pub(crate) mod fault;

pub(crate) mod resumable;
mod resumable_transfer;
mod upload_replay;
pub(crate) use resumable::ResumableError;
#[cfg(test)]
pub(crate) use resumable::ResumableSession;
pub(crate) use resumable::{
    Overrun, Progress, ResumableOptions, SessionId, SessionLease, SessionState, StoredCompletion,
};
pub(crate) use resumable_transfer::{ChunkEnd, ChunkWait, ResumableChunk, Written};
pub(crate) use upload_replay::UploadAclSnapshot;
pub(crate) use upload_replay::{CopyReplay, CopyReplaySource, ReplayInvocation, UploadCheck};

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
pub(crate) use appendable::{AppendError, AppendWriter, Finality, ReadableMetadata};

mod object_move;
pub(crate) use object_move::{MoveAccess, MoveConditions, MoveNames};
mod bucket_soft_delete;
mod object_owner;
mod objects;
mod soft_delete;
pub(crate) use objects::{
    FoldersAsPrefixes, FrameLimit, ListQueryError, ObjectFrames, ObjectListCursor, ObjectListPage,
    ObjectListQuery, ObjectReader, ObjectUpload,
};
pub(crate) use soft_delete::RestoreAccess;

mod blob_backend;
mod derived;
use blob_backend::BlobBackend;
mod transfer_io;
pub(crate) use derived::{
    DerivedDestination, DerivedRequestGuard, DerivedSource, DerivedSourceSpec, DerivedTransfer,
};

pub mod admin;
pub(crate) mod bucket_autoclass;
mod bucket_timestamps;
mod notification_outbox;
mod notifications;
mod policies;
mod retired_sync;
use policies::{check_access, insert_policy};
pub(crate) use retired_sync::left_in_folder as retired_sync_left_in_folder;

pub(crate) mod change_journal;

pub mod import_journal;

pub(crate) mod hmac_keys;

pub(crate) mod acl;

mod bucket_list;
mod bucket_update;
mod buckets;
mod close;
mod connection;
mod keyset;
mod known_projects;
mod open;
mod options;
mod state;
pub use open::RecoveredCatalog;
pub use options::{CatalogBacking, CatalogOpenError, CatalogOpenFailure, CatalogOptions};
mod schema;
#[cfg(test)]
use bucket_list::bucket_list_page;
use bucket_list::bucket_metadata_charge;
pub(super) use buckets::select_bucket;
use buckets::{
    BucketCreation, Cors, create_bucket_in_transaction, insert_bucket_in_transaction, read_bucket,
    remove_live_bucket, remove_objects,
};
pub(in crate::storage) use connection::ConnectionUnavailable;
#[cfg(test)]
pub(in crate::storage) use connection::RollbackError;
#[cfg(test)]
pub(in crate::storage) use executor::ExecutorError;
use keyset::After;
pub(crate) use keyset::{Page, PageLimit};
#[cfg(test)]
use open::immutable_catalog_uri;
#[cfg(test)]
use schema::STEPS;
#[cfg(test)]
use schema::{
    BUCKET_LIFECYCLE_SCHEMA, BUCKET_RETENTION_SCHEMA, BUCKET_SCHEMA, BUCKET_SETTINGS_SCHEMA,
    BUCKET_UNIFORM_ACCESS_SCHEMA, Migration, OBJECT_HOLDS_CUSTOM_TIME_SCHEMA, SOFT_DELETE_SCHEMA,
    migrate_timestamp_precision,
};
use schema::{initialize_catalog, upgrade, validate_schema, validate_schema_version};

/// Calls beyond this many wait for admission rather than for the execution permit. Execution
/// finishes what it has queued even while closing, so this bounds the jobs left to run at
/// shutdown; `close` fails the admission waiters at once.
const MAX_IN_FLIGHT: usize = 32;
const MAX_LIST_LIMIT: usize = 1001;
const BUCKET_LIST_METADATA_BUDGET: usize = 256 * 1024;
pub(super) const CATALOG_SCHEMA_VERSION: i64 = admin::seed_records::SCHEMA_VERSION;
/// Matches `length(CAST(.. AS BLOB)) <= 65536` on the session, rewrite, notification, appendable,
/// upload replay and multipart documents (schema versions 7, 11, 28, 34, 35 and 37). Those
/// schema texts are frozen history: raising this needs a migration such as
/// `policies::RAISED_BINDINGS_SCHEMA`.
const MAX_STORED_DOCUMENT_BYTES: usize = 65_536;
/// Matches `CHECK(deadline = created + 604800000)` on resumable sessions and multipart uploads
/// (schema versions 7 and 37), whose deadlines are seven days after creation.
const SESSION_LIFETIME_MILLIS: i64 = 604_800_000;
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
// The named parameter of each column in `bucket_columns!`, in the same order, so a write binds
// columns by name; a test keeps the two lists aligned.
macro_rules! bucket_parameters {
    () => {
        ":name, :project, :location, :location_type, :storage_class, :labels,
                      :metageneration, :time_created, :updated, :versioning_enabled,
                      :default_event_based_hold, :cors, :public_access_prevention_enforced,
                      :uniform_bucket_level_access_locked_time, :lifecycle, :retention_policy, :soft_delete_policy,
                      :object_retention_enabled, :autoclass, :time_created_nanos, :updated_nanos, :generation, :versioning_configured"
    };
}
use bucket_parameters;

crate::storage::opaque_id::opaque_id! {
    /// Which life of a bucket name a row belongs to. SQLite draws a new value
    /// (`lower(hex(randomblob(16)))`) each time a bucket with that name is created, so state
    /// recorded for a deleted bucket never applies to its successor. Rust only compares and stores it.
    pub(crate) struct Incarnation([u8; 16]);
    invalid = "invalid bucket incarnation";
    column = text;
}

/// The incarnation of the bucket named `bucket`; `QueryReturnedNoRows` when no such bucket exists.
fn bucket_incarnation(connection: &Connection, bucket: &str) -> rusqlite::Result<Incarnation> {
    sql::scalar(
        connection,
        "SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1",
        [bucket],
    )
}

struct Inner {
    policy: state::Policy,
    storage: state::Storage,
    writer: state::Writer,
    reads: state::Reads,
    admission: state::Admission,
    admin: state::AdminState,
    writers: state::WriterRegistry,
    maintenance: state::Maintenance,
    notifications: state::Notifications,
    shutdown: state::Shutdown,
}

struct Resources {
    // Drop the connection before releasing directory ownership, including on cancelled calls.
    connection: Connection,
    lock: Option<lock::DirectoryLock>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown.token.cancel();
    }
}

impl Inner {
    fn is_closing(&self) -> bool {
        self.shutdown.token.is_cancelled()
    }
}

#[derive(Default)]
struct UploadState {
    active: usize,
}

/// The admission held through publication and cleanup.
enum UploadSlot {
    Counted {
        catalog: Catalog,
        _task: Arc<crate::task::TaskToken>,
    },
    Admin {
        _permit: admin::AdminPermit,
    },
}

#[must_use = "dropping the guard releases upload admission"]
pub(crate) struct UploadGuard {
    slot: UploadSlot,
    replay: Option<upload_replay::UploadReplay>,
    _activity: Activity,
}

impl UploadGuard {
    fn admin(permit: admin::AdminPermit, activity: Activity) -> Self {
        Self {
            slot: UploadSlot::Admin { _permit: permit },
            replay: None,
            _activity: activity,
        }
    }
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        if let UploadSlot::Counted { catalog, .. } = &self.slot {
            let mut uploads = crate::sync::lock(&catalog.0.admission.uploads);
            uploads.active -= 1;
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
        match self.0.storage.backend {
            BlobBackend::Disk(_) => Some((&self.0.storage.data_dir, self.0.storage.data_dir_state)),
            BlobBackend::Memory(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn active_uploads(&self) -> usize {
        self.0.admission.uploads.lock().unwrap().active
    }

    #[cfg(test)]
    pub(crate) fn session_writers_idle(&self) -> bool {
        self.0.writers.sessions.lock().unwrap().is_empty()
    }

    /// Arms a one-shot fault for the `skip + 1`-th time the catalog reaches `step`.
    #[cfg(test)]
    pub(crate) fn inject(&self, step: fault::Step, skip: usize, fault: fault::Fault) {
        fault::arm(&self.0.storage.data_dir, step, skip, fault);
    }

    /// The number of injected faults the catalog has yet to reach.
    #[cfg(test)]
    pub(crate) fn pending_faults(&self) -> usize {
        fault::pending(&self.0.storage.data_dir)
    }

    /// Whether shutdown started closing the catalog. An unhealthy catalog also refuses work, so
    /// only this tells a stopping server from a failing one.
    pub(crate) fn is_closing(&self) -> bool {
        self.0.is_closing()
    }

    fn shutdown_interrupted(&self, error: &impl crate::cause::TypedSource) -> bool {
        self.is_closing()
            && error.typed_source().is_some_and(|source| {
                crate::cause::chain(source).find_map(crate::cause::Link::failure)
                    == Some(BackendFailure::CatalogClosed)
            })
    }

    pub fn is_healthy(&self) -> bool {
        !self.0.writer.resources.is_poisoned()
            && self.0.storage.backend.is_healthy()
            && !self.0.writers.sessions.is_poisoned()
            && !self.is_closing()
            && !self.0.admin.uploads.is_closed()
            && !self.0.admin.reads.is_closed()
    }
}

/// A hard delete time, or a clock reading compared with one: whole microseconds since the Unix
/// epoch. A maintenance pass also handles session deadlines in milliseconds as [`UnixMillis`];
/// the two types keep a reading in one unit from reaching a sweep that expects the other.
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

/// A resumable, multipart, rewrite, derived-transfer or HMAC key time, or a clock reading compared
/// with one: whole milliseconds since the Unix epoch, the unit those tables have always stored.
/// Multipart upload and part times keep the microseconds past it in a column of their own
/// (`xml_multipart::timestamps`). Hard delete times use [`UnixMicros`]; neither converts into
/// the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct UnixMillis(i64);

impl UnixMillis {
    fn system_time(self) -> rusqlite::Result<SystemTime> {
        signed_system_time(self.0, Duration::from_millis)
    }

    /// `None` past the `i64` range; callers report their own overflow.
    fn checked_add_millis(self, millis: i64) -> Option<Self> {
        self.0.checked_add(millis).map(Self)
    }

    fn saturating_add_millis(self, millis: i64) -> Self {
        Self(self.0.saturating_add(millis))
    }
}

impl TryFrom<SystemTime> for UnixMillis {
    type Error = CatalogError;

    fn try_from(time: SystemTime) -> Result<Self, CatalogError> {
        unix_timestamp(time, 1_000_000).map(Self)
    }
}

impl rusqlite::ToSql for UnixMillis {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

impl rusqlite::types::FromSql for UnixMillis {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        i64::column_result(value).map(Self)
    }
}

fn unix_micros(time: SystemTime) -> Result<i64, CatalogError> {
    unix_timestamp(time, 1000)
}

fn unix_timestamp(time: SystemTime, nanos_per_unit: i128) -> Result<i64, CatalogError> {
    i64::try_from(signed_nanos(time).div_euclid(nanos_per_unit)).map_err(unavailable)
}

fn system_time_micros(micros: i64) -> rusqlite::Result<SystemTime> {
    signed_system_time(micros, Duration::from_micros)
}

/// An optional whole-microsecond timestamp column.
fn micros_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<SystemTime>> {
    row.get::<_, Option<UnixMicros>>(index)?
        .map(UnixMicros::system_time)
        .transpose()
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
