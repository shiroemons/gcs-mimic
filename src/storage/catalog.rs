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
use tokio::sync::{Notify, OwnedRwLockReadGuard, RwLock, Semaphore};

type Activity = OwnedRwLockReadGuard<()>;

fn admission_error(error: &tokio::sync::TryAcquireError) -> CatalogError {
    unavailable(match error {
        tokio::sync::TryAcquireError::Closed => BackendFailure::CatalogClosed,
        tokio::sync::TryAcquireError::NoPermits => BackendFailure::OperationLimit,
    })
}

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

#[cfg(test)]
pub(super) async fn replace_resumable_cleanup_obstacle(catalog: &Catalog, stage: PathBuf) {
    catalog
        .run(move |_| {
            // Maintenance must not clear the deletion journal while this path is temporarily absent.
            std::fs::remove_dir(&stage).unwrap();
            std::fs::write(stage, b"uncommitted tail").unwrap();
            Ok::<_, CatalogError>(())
        })
        .await
        .unwrap();
}

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

mod resumable_maintenance;
pub(crate) mod xml_multipart;

mod executor;
mod principals;
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

const MAX_IN_FLIGHT: usize = 32;
const MAX_LIST_LIMIT: usize = 1001;
const BUCKET_LIST_METADATA_BUDGET: usize = 256 * 1024;
pub(super) const CATALOG_SCHEMA_VERSION: i64 = 50;
const BUCKET_VERSIONING_PRESENCE_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN versioning_configured INTEGER NOT NULL DEFAULT 0 CHECK(versioning_configured IN (0, 1));
ALTER TABLE soft_deleted_buckets ADD COLUMN versioning_configured INTEGER NOT NULL DEFAULT 0 CHECK(versioning_configured IN (0, 1));
PRAGMA user_version = 49;";
const SOFT_DELETE_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN soft_delete_policy TEXT;
ALTER TABLE objects ADD COLUMN soft_delete_time INTEGER;
ALTER TABLE objects ADD COLUMN hard_delete_time INTEGER;
CREATE INDEX objects_soft_deleted ON objects(bucket, name, generation) WHERE soft_delete_time IS NOT NULL;
CREATE INDEX objects_soft_expiry ON objects(bucket, hard_delete_time) WHERE soft_delete_time IS NOT NULL;
PRAGMA user_version = 26;";
const BUCKET_RETENTION_SCHEMA: &str =
    "ALTER TABLE buckets ADD COLUMN retention_policy TEXT; PRAGMA user_version = 25;";
const BUCKET_LIFECYCLE_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN lifecycle TEXT NOT NULL DEFAULT '[]'; PRAGMA user_version = 24;";
const BUCKET_UNIFORM_ACCESS_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN uniform_bucket_level_access_locked_time INTEGER; PRAGMA user_version = 23;";
const OBJECT_HOLDS_CUSTOM_TIME_SCHEMA: &str = "ALTER TABLE objects ADD COLUMN temporary_hold INTEGER NOT NULL DEFAULT 0 CHECK(temporary_hold IN (0, 1)); ALTER TABLE objects ADD COLUMN custom_time TEXT; PRAGMA user_version = 22;";
const TIMESTAMP_MICROS_SCHEMA_VERSION: i64 = 14;
const SEED_SCHEMA: &str = "CREATE TABLE startup_seed (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    complete INTEGER NOT NULL CHECK(complete IN (0, 1))
) WITHOUT ROWID;
PRAGMA user_version = 5;";
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
const INSERT_BUCKET: &str = concat!(
    "INSERT INTO buckets (",
    bucket_columns!(),
    ", acl_policy_bound) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, NULL, 0, 0, ?19, ?20, 1)
         ON CONFLICT(name) DO NOTHING"
);
const SELECT_BUCKET: &str = concat!(
    "SELECT ",
    bucket_columns!(),
    " FROM buckets WHERE name = ?1"
);
const UPDATE_BUCKET: &str = "UPDATE buckets SET labels = ?2, storage_class = ?3,
                     versioning_enabled = ?4, versioning_configured = ?14, default_event_based_hold = ?5,
                     metageneration = ?6, updated = ?7, updated_nanos = 0, cors = ?8, public_access_prevention_enforced = ?9,
                     uniform_bucket_level_access_locked_time = ?10, lifecycle = ?11, retention_policy = ?12, soft_delete_policy = ?13 WHERE name = ?1";
/// Bounds the parsed statements the catalog's single connection keeps. Request paths prepare
/// about 150 distinct static SQL texts (some list and derived shapes are chosen at run time),
/// so 256 keeps the whole hot set cached with room for new statements, while open-time and
/// maintenance statements that pass through the LRU cannot evict it. A few hundred prepared
/// statements cost on the order of a megabyte, bounded by this one connection.
const STATEMENT_CACHE_CAPACITY: usize = 256;
const BUCKET_PAP_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN public_access_prevention_enforced INTEGER NOT NULL DEFAULT 0 CHECK(public_access_prevention_enforced IN (0, 1));
PRAGMA user_version = 13;";
const BUCKET_CORS_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN cors TEXT NOT NULL DEFAULT '[]';
PRAGMA user_version = 12;";
const BUCKET_SETTINGS_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN versioning_enabled INTEGER NOT NULL DEFAULT 0 CHECK(versioning_enabled IN (0, 1));
ALTER TABLE buckets ADD COLUMN default_event_based_hold INTEGER NOT NULL DEFAULT 0 CHECK(default_event_based_hold IN (0, 1));
PRAGMA user_version = 8;";
const BUCKET_SCHEMA: &str = "CREATE TABLE buckets (
                        name TEXT PRIMARY KEY COLLATE BINARY,
                        project TEXT NOT NULL,
                        location TEXT NOT NULL,
                        location_type TEXT NOT NULL,
                        storage_class TEXT NOT NULL,
                        labels TEXT NOT NULL,
                        metageneration INTEGER NOT NULL,
                        time_created INTEGER NOT NULL,
                        updated INTEGER NOT NULL
                     ) WITHOUT ROWID;
                     CREATE INDEX buckets_project_name ON buckets(project, name);
                     PRAGMA user_version = 1;";

fn bucket_incarnation(connection: &Connection, bucket: &str) -> rusqlite::Result<String> {
    sql::scalar(
        connection,
        "SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1",
        [bucket],
    )
}

/// A bounded keyset page. Continue after the last returned name when `has_more` is true.
pub(crate) struct BucketListPage {
    pub items: Vec<BucketRecord>,
    pub has_more: bool,
}

#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration at open"
)]
fn migrate_timestamp_precision(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    // SQLite promotes overflowing integer multiplication to REAL, losing precision.
    macro_rules! unconvertible {
        ($column:literal) => {
            concat!(
                "(",
                $column,
                " IS NOT NULL AND (typeof(",
                $column,
                ") != 'integer' OR ",
                $column,
                " NOT BETWEEN -9223372036854775 AND 9223372036854775))"
            )
        };
    }
    for (table, check) in [
        (
            "buckets",
            concat!(
                "SELECT EXISTS(SELECT 1 FROM buckets WHERE ",
                unconvertible!("time_created"),
                " OR ",
                unconvertible!("updated"),
                ")"
            ),
        ),
        (
            "objects",
            concat!(
                "SELECT EXISTS(SELECT 1 FROM objects WHERE ",
                unconvertible!("time_created"),
                " OR ",
                unconvertible!("updated"),
                " OR ",
                unconvertible!("time_deleted"),
                ")"
            ),
        ),
    ] {
        let invalid: bool = transaction.query_row(check, [], |row| row.get(0))?;
        if invalid {
            return Err(unavailable(format!(
                "{table} timestamps cannot be converted to integer Unix microseconds"
            )));
        }
    }
    transaction.execute_batch(
        "UPDATE buckets SET time_created = time_created * 1000, updated = updated * 1000;
         UPDATE objects SET time_created = time_created * 1000, updated = updated * 1000, time_deleted = time_deleted * 1000;",
    )?;
    transaction.pragma_update(None, "user_version", TIMESTAMP_MICROS_SCHEMA_VERSION)?;
    transaction.commit().map_err(unavailable)
}

fn validate_schema_version(version: i64, supported: i64) -> Result<(), CatalogError> {
    if version > supported {
        return Err(CatalogError::NewerSchema { version, supported });
    }
    if version < 0 {
        return Err(unavailable(format!(
            "unsupported catalog schema version {version}; supported version is {supported}"
        )));
    }
    Ok(())
}

fn immutable_catalog_uri(path: &Path) -> Result<String, CatalogError> {
    #[cfg(unix)]
    let bytes = path.as_os_str().as_encoded_bytes();
    #[cfg(windows)]
    let bytes = path
        .to_str()
        .ok_or_else(|| unavailable("catalog path cannot be represented as UTF-8"))?
        .as_bytes();
    // SQLite decodes the path before passing it to its native VFS. Encoding
    // separators prevents UNC paths from being interpreted as URI authorities
    // and preserves Windows extended prefixes instead of stripping long-path support.
    let encoded: String = bytes
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~:".contains(byte) {
                char::from(*byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    Ok(format!("file:{encoded}?immutable=1"))
}

type StateCheck = fn(&Connection) -> Result<(), CatalogError>;

/// Each schema version's statements, applied in order to build the expected catalog, and the
/// stored-state check that version introduced.
const SCHEMA_STEPS: &[(i64, &str, Option<StateCheck>)] = &[
    (2, objects::SCHEMA, None),
    (3, objects::GENERATION_SCHEMA, None),
    (4, objects::METADATA_SCHEMA, None),
    (5, SEED_SCHEMA, Some(validate_seed)),
    (6, policies::SCHEMA, Some(policies::validate)),
    (7, resumable::SCHEMA, Some(resumable::validate)),
    (8, BUCKET_SETTINGS_SCHEMA, None),
    (9, objects::VERSIONING_SCHEMA, None),
    (10, derived::SCHEMA, None),
    (11, rewrite::SCHEMA, Some(rewrite::validate)),
    (12, BUCKET_CORS_SCHEMA, None),
    (13, BUCKET_PAP_SCHEMA, None),
    (15, change_journal::SCHEMA, Some(change_journal::validate)),
    (16, import_journal::SCHEMA, None),
    (17, sync_state::SCHEMA, Some(sync_state::validate)),
    (
        18,
        import_journal::FAILURE_SCHEMA,
        Some(import_journal::validate_failure),
    ),
    (19, sync_state::root::SCHEMA, None),
    (20, sync_state::writeback::SCHEMA, None),
    (21, sync_state::findings::SCHEMA, None),
    (22, OBJECT_HOLDS_CUSTOM_TIME_SCHEMA, None),
    (23, BUCKET_UNIFORM_ACCESS_SCHEMA, None),
    (24, BUCKET_LIFECYCLE_SCHEMA, None),
    (25, BUCKET_RETENTION_SCHEMA, None),
    (26, SOFT_DELETE_SCHEMA, None),
    (27, sync_state::buckets::SCHEMA, None),
    (28, notifications::SCHEMA, Some(notifications::validate)),
    (29, objects::retention::SCHEMA, None),
    (30, customer_keys::SCHEMA, None),
    (31, bucket_autoclass::SCHEMA, None),
    (32, hmac_keys::SCHEMA, None),
    (33, acl::SCHEMA, None),
    (34, appendable::SCHEMA, None),
    (35, upload_replay::SCHEMA, Some(upload_replay::validate)),
    (36, md5_availability::SCHEMA, None),
    (37, xml_multipart::SCHEMA, Some(xml_multipart::validate)),
    (
        38,
        notification_outbox::SCHEMA,
        Some(notification_outbox::validate),
    ),
    (39, bucket_timestamps::SCHEMA, None),
    (40, sync_state::deletions::SCHEMA, None),
    (41, sync_state::observation::SCHEMA, None),
    (42, sync_state::conflicts::SCHEMA, None),
    (43, acl::POLICY_BINDING_SCHEMA, None),
    (44, object_owner::SCHEMA, None),
    (45, bucket_soft_delete::SCHEMA, None),
    (46, bucket_soft_delete::GENERATION_SCHEMA, None),
    (47, objects::retention::SUB_MICRO_SCHEMA, None),
    (48, object_hold::SCHEMA, None),
    (49, BUCKET_VERSIONING_PRESENCE_SCHEMA, None),
    (50, sync_state::deletions::history::SCHEMA, None),
];

#[expect(
    clippy::disallowed_methods,
    reason = "schema validation runs once at open"
)]
fn validate_seed(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection.query_row(
        "SELECT count(*) = 1 AND min(singleton) = 1 FROM startup_seed",
        [],
        |r| r.get(0),
    )?;
    if valid {
        Ok(())
    } else {
        Err(unavailable("invalid startup seed state"))
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "schema validation runs once at open"
)]
fn validate_schema(connection: &Connection, version: i64) -> Result<(), CatalogError> {
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(BUCKET_SCHEMA)?;
    for &(introduced, schema, check) in SCHEMA_STEPS {
        if version < introduced {
            break;
        }
        expected.execute_batch(schema)?;
        if let Some(check) = check {
            check(connection)?;
        }
        // Version 18 replaced the import journal's validation with the failure journal's.
        if introduced == 16 && version < 18 {
            import_journal::validate(connection)?;
        }
    }
    let inventory =
        |db: &Connection| -> Result<Vec<(String, String, String, String)>, CatalogError> {
            db.prepare_cached("SELECT type, name, tbl_name, substr(sql, 1, 16385) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name LIMIT 257")?.query_map([], |row| {
                let sql: String = row.get(3)?;
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, sql.split_whitespace().collect::<Vec<_>>().join(" ")))
            })?.collect::<rusqlite::Result<_>>().map_err(unavailable)
        };
    // Versioned catalogs must retain our constraints, collations, indexes and table inventory.
    let actual = inventory(connection)?;
    if actual.len() > 256 || actual != inventory(&expected)? {
        return Err(unavailable("catalog schema does not match its version"));
    }
    if version >= 3 {
        let valid: bool = connection.query_row("SELECT count(*) = 1 AND min(singleton) = 1 AND min(last_generation) >= (SELECT coalesce(max(generation), 0) FROM objects) FROM object_generation", [], |row| row.get(0))?;
        if !valid {
            return Err(unavailable("invalid generation allocator state"));
        }
    }
    Ok(())
}

struct Inner {
    principals: OnceLock<principals::Profile>,
    activity: Arc<RwLock<()>>,
    resources: Mutex<Option<Resources>>,
    admission: Arc<Semaphore>,
    execution: Arc<Semaphore>,
    executor: executor::Executor,
    reclamation: Arc<Semaphore>,
    uploads: Mutex<UploadState>,
    uploads_idle: Notify,
    admin_uploads: Arc<Semaphore>,
    admin_reads: Arc<Semaphore>,
    admin_waiters: Arc<Semaphore>,
    staged_handles: Arc<Semaphore>,
    publication_batch: Arc<Semaphore>,
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
    /// Retention a new bucket without an explicit softDeletePolicy receives; 0 gives none.
    default_soft_delete_retention: AtomicU64,
    notification_attempts: Arc<Semaphore>,
    data_dir: PathBuf,
    backend: BlobBackend,
    clock: Arc<dyn Clock>,
    session_writers: Mutex<resumable::writers::SessionWriters>,
    derived_wake: Notify,
    maintenance_wake: Arc<Notify>,
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

/// Why an operation could not run on the shared catalog connection.
#[derive(Debug)]
enum ConnectionUnavailable {
    Closed,
    /// The operation panicked; its changes were rolled back.
    Panicked,
    /// An earlier panic left a transaction that still cannot be rolled back.
    Unrecovered(RollbackError),
}

impl std::fmt::Display for ConnectionUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str(BackendFailure::CatalogClosed.message()),
            Self::Panicked => {
                f.write_str("catalog operation panicked; its changes were rolled back")
            }
            Self::Unrecovered(error) => write!(f, "catalog connection is unusable: {error}"),
        }
    }
}

impl std::error::Error for ConnectionUnavailable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unrecovered(error) => Some(error),
            Self::Closed => Some(&BackendFailure::CatalogClosed),
            Self::Panicked => None,
        }
    }
}

impl crate::storage::failure::StorageCause for ConnectionUnavailable {
    fn into_cause(self) -> crate::storage::failure::BackendCause {
        crate::storage::failure::BackendCause::from_error(self)
    }
}

/// Why a transaction abandoned by a panic could not be confirmed as rolled back.
#[derive(Debug)]
enum RollbackError {
    Rollback(rusqlite::Error),
    StillOpen,
    #[cfg(test)]
    Injected,
}

impl std::fmt::Display for RollbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rollback(error) => write!(f, "transaction could not be rolled back: {error}"),
            Self::StillOpen => f.write_str("connection is still inside a transaction"),
            #[cfg(test)]
            Self::Injected => f.write_str("injected rollback failure"),
        }
    }
}

impl std::error::Error for RollbackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rollback(error) => Some(error),
            _ => None,
        }
    }
}

impl Inner {
    /// Runs one operation on the shared connection. A panicking operation fails
    /// only its own request: its transaction is rolled back and the connection
    /// stays usable. Only when the rollback cannot be confirmed does the mutex
    /// stay poisoned, so later calls fail closed and `is_healthy` turns false.
    fn with_connection<T, E>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, E>,
    ) -> Result<Result<T, E>, ConnectionUnavailable> {
        let mut resources = match self.resources.lock() {
            Ok(resources) => resources,
            Err(poisoned) => {
                let resources = poisoned.into_inner();
                if let Some(resources) = resources.as_ref() {
                    self.end_abandoned_transaction(&resources.connection)
                        .map_err(ConnectionUnavailable::Unrecovered)?;
                }
                self.resources.clear_poison();
                resources
            }
        };
        let Some(resources) = resources.as_mut() else {
            return Err(ConnectionUnavailable::Closed);
        };
        let connection = &mut resources.connection;
        // The connection is checked after an unwind before anyone reuses it, so
        // observing it mid-operation is harmless.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            change_journal::notify_on_write(connection, &self.change_wake, |connection| {
                change_journal::notify_on_write(connection, &self.notification_wake, operation)
            })
        }));
        match outcome {
            Ok(result) => Ok(result),
            Err(payload) => {
                if self.end_abandoned_transaction(connection).is_err() {
                    // Unwinding with the guard held poisons the mutex on purpose.
                    std::panic::resume_unwind(payload);
                }
                Err(ConnectionUnavailable::Panicked)
            }
        }
    }

    /// rusqlite rolls a `Transaction` back when it drops, including during a
    /// panic, but a raw `BEGIN` or a failed drop-time rollback can leave one open.
    fn end_abandoned_transaction(&self, connection: &Connection) -> Result<(), RollbackError> {
        #[cfg(test)]
        if self
            .fail_rollback
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RollbackError::Injected);
        }
        if !connection.is_autocommit() {
            connection
                .execute_batch("ROLLBACK")
                .map_err(RollbackError::Rollback)?;
        }
        if connection.is_autocommit() {
            Ok(())
        } else {
            Err(RollbackError::StillOpen)
        }
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

/// A single SQLite connection with at most 32 catalog calls.
/// Metadata jobs execute serially; upload prechecks wait for admission, other excess calls fail.
#[derive(Clone)]
pub struct Catalog(Arc<Inner>);

/// Reads and validates an existing catalog without writing to it; returns its stored schema
/// version, or 0 when the catalog is new.
#[expect(
    clippy::disallowed_methods,
    reason = "integrity and schema inspection run once at open"
)]
fn inspect_catalog(path: &Path) -> Result<i64, CatalogError> {
    // A read-write connection can checkpoint a rejected catalog's hot WAL on close.
    let connection = if path.try_exists()? {
        if path.with_extension("sqlite3-wal").try_exists()? {
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        } else {
            // Directory ownership makes a WAL-free main file safe for immutable reads.
            let absolute = path.canonicalize()?;
            Connection::open_with_flags(
                immutable_catalog_uri(&absolute)?,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
        }
    } else {
        Connection::open_in_memory()
    }?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA cache_size = -2048;
                 PRAGMA mmap_size = 0;
                 PRAGMA temp_store = FILE;",
    )?;
    let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    validate_schema_version(version, CATALOG_SCHEMA_VERSION)?;
    if version == 2 {
        return Err(unavailable(
            "unsupported unreleased catalog schema version 2; deleted generation history cannot be recovered",
        ));
    }
    let check: String = connection.query_row("PRAGMA quick_check(1)", [], |r| r.get(0))?;
    if check != "ok" {
        return Err(CatalogError::Unavailable(
            format!("catalog quick_check failed: {check}").into(),
        ));
    }
    if version == 0 {
        let tables: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
            [],
            |r| r.get(0),
        )?;
        if tables != 0 {
            return Err(CatalogError::Unavailable(
                "unversioned catalog is not empty".into(),
            ));
        }
    } else {
        // Reject foreign schemas before any persistent journal-mode change.
        validate_schema(&connection, version)?;
    }
    Ok(version)
}

/// Opens the catalog for writing in WAL mode with full synchronization.
#[expect(
    clippy::disallowed_methods,
    reason = "switches the journal mode once at open"
)]
fn open_writable(path: &Path) -> Result<Connection, CatalogError> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
                 PRAGMA cache_size = -2048;
                 PRAGMA mmap_size = 0;
                 PRAGMA temp_store = FILE;",
    )?;
    let mode: String = connection.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    if mode != "wal" {
        let mode: String = connection.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if mode != "wal" {
            return Err(CatalogError::Unavailable(
                "SQLite WAL mode is required".into(),
            ));
        }
    }
    connection.execute_batch("PRAGMA synchronous = FULL; PRAGMA wal_autocheckpoint = 256;")?;
    Ok(connection)
}

fn open_memory(max_bytes: u64) -> Result<Connection, CatalogError> {
    let connection = Connection::open_in_memory()?;
    connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    connection.execute_batch("PRAGMA page_size = 4096; PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY; PRAGMA cache_size = -2048;")?;
    let pages = i64::try_from(max_bytes / 4096).map_err(unavailable)?;
    connection.pragma_update(None, "max_page_count", pages)?;
    connection.pragma_update(Some("temp"), "max_page_count", pages)?;
    Ok(connection)
}

/// Creates a new catalog at schema version 5, the base the migrations continue from.
#[expect(
    clippy::disallowed_methods,
    reason = "creates a new catalog once at open"
)]
fn initialize_catalog(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(BUCKET_SCHEMA)?;
    // Freshness and the retryable seed state must survive the same commit.
    for schema in [
        objects::SCHEMA,
        objects::GENERATION_SCHEMA,
        objects::METADATA_SCHEMA,
        SEED_SCHEMA,
    ] {
        transaction.execute_batch(schema)?;
    }
    transaction.execute("INSERT INTO object_generation VALUES (1, 0)", [])?;
    transaction.execute("INSERT INTO startup_seed VALUES (1, 0)", [])?;
    transaction.commit().map_err(unavailable)
}

#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration at open"
)]
fn migrate_seed(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SEED_SCHEMA)?;
    // Older catalogs have already served user mutations, even if now empty.
    transaction.execute("INSERT INTO startup_seed VALUES (1, 1)", [])?;
    transaction.commit().map_err(unavailable)
}

/// One schema upgrade: statements committed in one transaction, or a migration function.
enum Migration {
    Statements(&'static str),
    Function(fn(&mut Connection) -> Result<(), CatalogError>),
}

impl Migration {
    fn apply(&self, connection: &mut Connection) -> Result<(), CatalogError> {
        match self {
            Self::Statements(schema) => {
                let transaction = connection.transaction()?;
                transaction.execute_batch(schema)?;
                transaction.commit().map_err(unavailable)
            }
            Self::Function(migrate) => migrate(connection),
        }
    }
}

/// Upgrades in order; each applies when the catalog's version is below the paired version.
const MIGRATIONS: &[(i64, Migration)] = &[
    (2, Migration::Function(objects::migrate)),
    (4, Migration::Function(objects::migrate_metadata)),
    (5, Migration::Function(migrate_seed)),
    (6, Migration::Function(policies::migrate)),
    (7, Migration::Function(resumable::migrate)),
    (8, Migration::Statements(BUCKET_SETTINGS_SCHEMA)),
    (9, Migration::Function(objects::migrate_versioning)),
    (10, Migration::Statements(derived::SCHEMA)),
    (11, Migration::Statements(rewrite::SCHEMA)),
    (12, Migration::Statements(BUCKET_CORS_SCHEMA)),
    (13, Migration::Statements(BUCKET_PAP_SCHEMA)),
    (
        TIMESTAMP_MICROS_SCHEMA_VERSION,
        Migration::Function(migrate_timestamp_precision),
    ),
    (15, Migration::Statements(change_journal::SCHEMA)),
    (16, Migration::Statements(import_journal::SCHEMA)),
    (17, Migration::Function(sync_state::migrate)),
    (18, Migration::Statements(import_journal::FAILURE_SCHEMA)),
    (19, Migration::Statements(sync_state::root::SCHEMA)),
    (20, Migration::Statements(sync_state::writeback::SCHEMA)),
    (21, Migration::Statements(sync_state::findings::SCHEMA)),
    (22, Migration::Statements(OBJECT_HOLDS_CUSTOM_TIME_SCHEMA)),
    (23, Migration::Statements(BUCKET_UNIFORM_ACCESS_SCHEMA)),
    (24, Migration::Statements(BUCKET_LIFECYCLE_SCHEMA)),
    (25, Migration::Statements(BUCKET_RETENTION_SCHEMA)),
    (26, Migration::Statements(SOFT_DELETE_SCHEMA)),
    (27, Migration::Statements(sync_state::buckets::SCHEMA)),
    (28, Migration::Statements(notifications::SCHEMA)),
    (29, Migration::Function(objects::retention::migrate)),
    (30, Migration::Function(customer_keys::migrate)),
    (31, Migration::Statements(bucket_autoclass::SCHEMA)),
    (32, Migration::Statements(hmac_keys::SCHEMA)),
    (33, Migration::Statements(acl::SCHEMA)),
    (34, Migration::Statements(appendable::SCHEMA)),
    (35, Migration::Statements(upload_replay::SCHEMA)),
    (36, Migration::Statements(md5_availability::SCHEMA)),
    (37, Migration::Statements(xml_multipart::SCHEMA)),
    (38, Migration::Statements(notification_outbox::SCHEMA)),
    (39, Migration::Statements(bucket_timestamps::SCHEMA)),
    (40, Migration::Statements(sync_state::deletions::SCHEMA)),
    (41, Migration::Statements(sync_state::observation::SCHEMA)),
    (42, Migration::Statements(sync_state::conflicts::SCHEMA)),
    (43, Migration::Statements(acl::POLICY_BINDING_SCHEMA)),
    (44, Migration::Statements(object_owner::SCHEMA)),
    (45, Migration::Statements(bucket_soft_delete::SCHEMA)),
    (
        46,
        Migration::Statements(bucket_soft_delete::GENERATION_SCHEMA),
    ),
    (
        47,
        Migration::Statements(objects::retention::SUB_MICRO_SCHEMA),
    ),
    (48, Migration::Statements(object_hold::SCHEMA)),
    (49, Migration::Statements(BUCKET_VERSIONING_PRESENCE_SCHEMA)),
    (
        50,
        Migration::Statements(sync_state::deletions::history::SCHEMA),
    ),
];

/// Resets volatile state and completes crash recovery before the catalog serves requests.
#[expect(
    clippy::disallowed_methods,
    reason = "crash recovery runs once at open"
)]
fn recover_on_open(
    connection: &mut Connection,
    backend: &BlobBackend,
    clock: &dyn Clock,
) -> Result<(), CatalogError> {
    let BlobBackend::Disk(data_dir) = backend else {
        return Err(unavailable("disk recovery requires a disk backend"));
    };
    let pending: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_writeback_ops WHERE phase!=4)",
        [],
        |row| row.get(0),
    )?;
    if pending {
        connection.execute("UPDATE sync_writeback_roots SET recovery=1 WHERE EXISTS(SELECT 1 FROM sync_writeback_ops p WHERE p.root_id=sync_writeback_roots.root_id AND p.phase!=4)",[])?;
    }
    let journal_enabled: bool = connection.query_row(
        "SELECT enabled FROM change_journal_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    if journal_enabled {
        // shortcut: restart requires a full rescan, add durable consumer checkpoints when restart continuity is needed.
        connection.execute(
            "UPDATE change_journal_state SET enabled = 0, epoch = epoch + 1 WHERE singleton = 1",
            [],
        )?;
    }
    connection.execute_batch("CREATE TEMP TABLE parent_public_access_prevention (project TEXT PRIMARY KEY) WITHOUT ROWID;")?;
    notification_outbox::open(connection)?;
    let has_deletion_revision: bool = sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM sync_deletion_sets WHERE valid=1 OR authorization!=0)",
        [],
    )?;
    if has_deletion_revision {
        connection.execute_batch("UPDATE sync_deletion_sets SET valid=0,authorization=0 WHERE valid=1 OR authorization!=0;")?;
    }
    let has_conflict_revision: bool = sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM sync_conflict_roots UNION ALL SELECT 1 FROM sync_recovery_workspaces)",
        [],
    )?;
    if has_conflict_revision {
        connection.execute_batch("UPDATE sync_conflict_roots SET revision=lower(hex(randomblob(16))); UPDATE sync_recovery_workspaces SET revision=lower(hex(randomblob(16)));")?;
    }
    let recovery_now = unix_millis(clock.now())?;
    rewrite::recover_due(connection, recovery_now)?;
    resumable_maintenance::recover_due(connection, backend, recovery_now)?;
    xml_multipart::recover(connection, data_dir, recovery_now)?;
    objects::recover(connection, data_dir, recovery_now)?;
    appendable::recover(connection, data_dir)
}

impl Catalog {
    fn activity(&self) -> Result<Activity, CatalogError> {
        self.0
            .activity
            .clone()
            .try_read_owned()
            .map_err(unavailable)
    }

    /// Acquires directory ownership and completes SQLite recovery before returning.
    pub async fn open(data_dir: impl AsRef<Path>) -> Result<Self, CatalogError> {
        Self::open_with_clock(data_dir, Arc::new(SystemClock)).await
    }

    pub async fn open_with_clock(
        data_dir: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, CatalogError> {
        let catalog = Self::open_for_server(data_dir, clock, admin::LocalAdmin::Disabled).await?;
        catalog.set_principal_profile(principals::Profile::TrustedLocal)?;
        Ok(catalog)
    }

    /// Issues local management authority only while opening the operator-owned catalog.
    pub async fn open_for_server(
        data_dir: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
        local_admin: admin::LocalAdmin,
    ) -> Result<Self, CatalogError> {
        let data_dir = data_dir.as_ref().to_owned();
        Self::open_backend(data_dir, clock, local_admin, None).await
    }

    /// Opens an isolated volatile catalog without reading or creating a data directory.
    pub async fn open_memory_for_server(
        max_bytes: u64,
        catalog_max_bytes: u64,
        clock: Arc<dyn Clock>,
        local_admin: admin::LocalAdmin,
    ) -> Result<Self, CatalogError> {
        if max_bytes == 0 || catalog_max_bytes < 1024 * 1024 {
            return Err(unavailable(
                "RAM requires a positive blob budget and at least 1 MiB of catalog pages",
            ));
        }
        Self::open_backend(
            PathBuf::new(),
            clock,
            local_admin,
            Some((max_bytes, catalog_max_bytes)),
        )
        .await
    }

    async fn open_backend(
        data_dir: PathBuf,
        clock: Arc<dyn Clock>,
        local_admin: admin::LocalAdmin,
        memory_limits: Option<(u64, u64)>,
    ) -> Result<Self, CatalogError> {
        crate::task::run_blocking("catalog-open", move || -> Result<Self, CatalogError> {
            let backend = match memory_limits {
                Some((bytes, _)) => BlobBackend::Memory(super::memory::Store::new(bytes)),
                None => BlobBackend::Disk(data_dir.clone()),
            };
            let (lock, stored_version, mut connection) = if let Some((_, bytes)) = memory_limits {
                (None, 0, open_memory(bytes)?)
            } else {
                let lock = lock::acquire(&data_dir)?;
                let path = data_dir.join("catalog.sqlite3");
                (Some(lock), inspect_catalog(&path)?, open_writable(&path)?)
            };
            let mut version = stored_version;
            if version == 0 {
                initialize_catalog(&mut connection)?;
                version = 5;
            }
            // A version marker alone must not make a missing or incompatible schema ready.
            connection
                .prepare_cached("SELECT name, project, location, location_type, storage_class, labels, metageneration, time_created, updated FROM buckets LIMIT 0")?;
            for (before, migration) in MIGRATIONS {
                if version < *before {
                    migration.apply(&mut connection)?;
                }
            }
            // Migrations are one-way, so operators need to see which upgrade ended older-release compatibility.
            if (1..CATALOG_SCHEMA_VERSION).contains(&stored_version) {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Info,
                    "catalog_schema_migrated",
                    serde_json::json!({"message":format!("migrated catalog schema from version {stored_version} to {CATALOG_SCHEMA_VERSION}; releases that support only schema {stored_version} can no longer open this data directory"),"from":stored_version,"to":CATALOG_SCHEMA_VERSION}),
                );
            }
            if memory_limits.is_some() {
                connection.execute_batch("CREATE TEMP TABLE parent_public_access_prevention (project TEXT PRIMARY KEY) WITHOUT ROWID;")?;
                notification_outbox::open(&connection)?;
            } else {
                recover_on_open(&mut connection, &backend, &*clock)?;
            }
            Ok(Self(Arc::new(Inner {
                principals: OnceLock::new(),
                activity: Arc::new(RwLock::new(())),
                resources: Mutex::new(Some(Resources { connection, lock })),
                admission: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
                execution: Arc::new(Semaphore::new(1)),
                executor: executor::Executor::start()?,
                reclamation: Arc::new(Semaphore::new(1)),
                uploads: Mutex::new(UploadState::default()),
                uploads_idle: Notify::new(),
                admin_uploads: Arc::new(Semaphore::new(2)),
                admin_reads: Arc::new(Semaphore::new(32)),
                admin_waiters: Arc::new(Semaphore::new(32)),
                staged_handles: Arc::new(Semaphore::new(256)),
                publication_batch: Arc::new(Semaphore::new(1)),
                publication_requests: Arc::new(Semaphore::new(2)),
                writeback_execution: Arc::new(Semaphore::new(1)),
                file_workers: admin::FileWorkers::new()?,
                admin_shutdown: Notify::new(),
                sync_control: Mutex::new(None),
                local_admin: admin::startup_grant(local_admin),
                change_wake: Notify::new(),
                notification_wake: Arc::new(Notify::new()),
                notification_started: AtomicBool::new(false),
                pubsub_endpoint: OnceLock::new(),
                default_soft_delete_retention: AtomicU64::new(0),
                notification_attempts: Arc::new(Semaphore::new(1)),
                backend,
                data_dir,
                clock,
                session_writers: Mutex::new(Default::default()),
                derived_wake: Notify::new(),
                maintenance_wake: Arc::new(Notify::new()),
                maintenance_error: Mutex::new(None),
                maintenance_started: AtomicBool::new(false),
                maintenance_health: crate::task::TaskHealth::default(),
                background_health: Mutex::new(Vec::new()),
                #[cfg(test)]
                fail_rollback: AtomicBool::new(false),
                #[cfg(test)]
                maintenance_panics: std::sync::atomic::AtomicUsize::new(0),
            })))
        })
        .await?
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
            && !self.0.admin_waiters.is_closed()
    }

    /// Stops admission on all handles and waits for blocking work before releasing ownership.
    /// Remaining handles become unavailable; repeated closes succeed without touching a new owner.
    pub async fn close(&self) -> Result<(), CatalogError> {
        self.0
            .uploads
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closed = true;
        self.0.admission.close();
        self.0.admin_shutdown.notify_waiters();
        self.0.notification_wake.notify_one();
        self.0.maintenance_wake.notify_one();
        let _notification_attempt = self
            .0
            .notification_attempts
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        let _publication_requests = self
            .0
            .publication_requests
            .clone()
            .acquire_many_owned(2)
            .await
            .map_err(unavailable)?;
        let _staged_handles = self
            .0
            .staged_handles
            .clone()
            .acquire_many_owned(256)
            .await
            .map_err(unavailable)?;
        let _admin_uploads = self
            .0
            .admin_uploads
            .clone()
            .acquire_many_owned(2)
            .await
            .map_err(unavailable)?;
        let _admin_reads = self
            .0
            .admin_reads
            .clone()
            .acquire_many_owned(32)
            .await
            .map_err(unavailable)?;
        loop {
            let idle = self.0.uploads_idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if self
                .0
                .uploads
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .active
                == 0
            {
                break;
            }
            idle.await;
        }
        let _reclamation = self
            .0
            .reclamation
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        let execution = self
            .0
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        let inner = self.0.clone();
        let closed = self
            .0
            .executor
            .run_last(move || {
                let _execution = execution;
                // Poison must not prevent shutdown from closing the connection and its lock.
                let mut resources = inner
                    .resources
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if let Some(Resources { connection, lock }) = resources.take() {
                    connection
                        .close()
                        .map_err(|(_, error)| unavailable(error))?;
                    if let Some(lock) = lock {
                        lock.release()?;
                    }
                    inner.backend.clear_memory()?;
                }
                Ok(())
            })
            .await;
        match closed {
            Ok(result) => result,
            // An earlier close already released the connection and stopped the thread.
            Err(executor::ExecutorError::Closed) => Ok(()),
            Err(error) => Err(unavailable(error)),
        }
    }

    async fn run<T: Send + 'static, E: From<CatalogError> + Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        let admission = self
            .0
            .admission
            .clone()
            .try_acquire_owned()
            // A closed catalog is not full: reporting a limit would invite retries at shutdown.
            .map_err(|error| admission_error(&error))?;
        self.run_admitted(admission, operation).await
    }

    async fn run_waiting<T: Send + 'static, E: From<CatalogError> + Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        let admission = self
            .0
            .admission
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| unavailable(BackendFailure::CatalogClosed))?;
        self.run_admitted(admission, operation).await
    }

    async fn run_admitted<T: Send + 'static, E: From<CatalogError> + Send + 'static>(
        &self,
        admission: tokio::sync::OwnedSemaphorePermit,
        operation: impl FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        let activity = self.activity()?;
        let execution = self
            .0
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        let inner = self.0.clone();
        self.0
            .executor
            .run(move || {
                let (_admission, _execution, _activity) = (admission, execution, activity);
                let result = match inner.with_connection(operation) {
                    Ok(result) => result,
                    Err(ConnectionUnavailable::Closed) => {
                        Err(unavailable(BackendFailure::CatalogClosed).into())
                    }
                    Err(error) => Err(unavailable(error).into()),
                };
                // Release ownership before the reply publishes completion to an immediate reopen.
                drop(inner);
                result
            })
            .await
            .map_err(unavailable)?
    }

    /// Sets the soft delete retention that a bucket created without a policy receives. The
    /// server applies its configuration before any bucket is created; an embedded catalog keeps
    /// 0, which leaves such buckets without a policy.
    pub fn set_default_soft_delete_retention(&self, seconds: u64) {
        self.0
            .default_soft_delete_retention
            .store(seconds, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn default_soft_delete_retention(&self) -> u64 {
        self.0
            .default_soft_delete_retention
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) async fn insert_bucket(
        &self,
        mut record: BucketRecord,
    ) -> Result<BucketRecord, CatalogError> {
        let default_retention = self.default_soft_delete_retention();
        self.run(move |connection| {
            validate_cors(&record.cors)?;
            let transaction = sql::immediate(connection)?;
            insert_bucket_in_transaction(&transaction, &mut record, default_retention)?;
            change_journal::append(
                &transaction,
                admin::Origin::Api,
                change_journal::ChangeKind::BucketCreate,
                &record.name,
                None,
            )?;
            transaction.commit()?;
            Ok(record)
        })
        .await
    }

    pub(crate) async fn patch_bucket(
        &self,
        name: &str,
        update: BucketMetadataUpdate,
        conditions: BucketConditions,
        access: &BucketAccess,
    ) -> Result<BucketRecord, CatalogError> {
        self.update_bucket(name, update, conditions, access, BucketWrite::Patch)
            .await
    }

    pub(crate) async fn replace_bucket(
        &self,
        name: &str,
        update: BucketMetadataUpdate,
        conditions: BucketConditions,
        access: &BucketAccess,
    ) -> Result<BucketRecord, CatalogError> {
        self.update_bucket(name, update, conditions, access, BucketWrite::Replace)
            .await
    }

    async fn update_bucket(
        &self,
        name: &str,
        mut update: BucketMetadataUpdate,
        conditions: BucketConditions,
        access: &BucketAccess,
        mode: BucketWrite,
    ) -> Result<BucketRecord, CatalogError> {
        let name = name.to_owned();
        let access = access.clone();
        let clock = self.0.clock.clone();
        let principals = self.0.principals.get().cloned();
        self.run(move |connection| {
            let transaction = sql::immediate(connection)?;
            let mut record = select_bucket(&transaction, &name)?
                .ok_or(CatalogError::NotFound)?;
            check_access(&transaction, &name, &record.project, &access)?;
            let pap = update.public_access_prevention_enforced.unwrap_or(record.public_access_prevention_enforced);
            if pap != record.public_access_prevention_enforced
                && let BucketAccess::Policy { identity, project, .. } = &access
            {
                check_access(&transaction, &name, &record.project, &BucketAccess::Policy {
                    identity: identity.clone(), project: project.clone(), permission: crate::auth::Permission::BucketsSetIam,
                })?;
            }
            if !conditions.hold(record.metageneration) {
                return Err(CatalogError::PreconditionFailed);
            }
            // Cloud Storage answered a PATCH carrying only ignored members with the unchanged
            // resource: no metageneration advance and no new `updated` time (observed 2026-10-31).
            if matches!(mode, BucketWrite::Patch) && update.is_empty() {
                return Ok(record);
            }
            if update
                .location
                .as_ref()
                .is_some_and(|location| !location.eq_ignore_ascii_case(&record.location))
            {
                return Err(CatalogError::UnsupportedMetadata("location".into()));
            }
            apply_uniform_access(
                &mut record,
                update.uniform_bucket_level_access_enabled,
                clock.now(),
            )?;
            if update.object_retention_enabled.is_some_and(|enabled| enabled != record.object_retention_enabled) {
                return Err(CatalogError::InvalidMetadata("objectRetention can only be enabled when the bucket is created (enableObjectRetention=true) and can never be disabled; omit objectRetention".into()));
            }
            if let Some(duration) = update.soft_delete_retention {
                apply_soft_delete_retention(&mut record, duration, clock.now())?;
            }
            apply_retention_policy(&mut record, &update, conditions.if_metageneration_match, clock.now())?;
            validate_retention_expiration(&transaction, &record)?;
            bucket_autoclass::update(&transaction, &mut record, &update, clock.now())?;
            if let Some(labels) = update.labels.take() {
                apply_labels(&mut record, labels, mode, update.labels_mode)?;
            }
            if record.labels.len() > 64 {
                return Err(CatalogError::InvalidMetadata(
                    "A bucket can have at most 64 labels".into(),
                ));
            }
            apply_plain_settings(&mut record, &mut update)?;
            record.metageneration = record
                .metageneration
                .checked_add(1)
                .ok_or_else(|| unavailable("bucket metageneration exhausted"))?;
            let updated = unix_micros(clock.now())?;
            record.updated = system_time_micros(updated)?;
            record.uniform_bucket_level_access_locked_time = record.uniform_bucket_level_access_locked_time
                .map(|time| unix_micros(time).and_then(|time| system_time_micros(time).map_err(unavailable))).transpose()?;
            transaction
                .prepare_cached(UPDATE_BUCKET)?
                .execute(
                    params![
                        name,
                        serde_json::to_string(&record.labels).map_err(unavailable)?,
                        record.storage_class,
                        record.versioning_enabled,
                        record.default_event_based_hold,
                        record.metageneration,
                        updated,
                        cors_value(&record.cors).to_string(),
                        record.public_access_prevention_enforced,
                        record.uniform_bucket_level_access_locked_time.map(unix_micros).transpose()?,
                        serde_json::to_string(&record.lifecycle).map_err(unavailable)?,
                        record.retention_policy.as_ref().map(serde_json::to_string).transpose().map_err(unavailable)?,
                        record.soft_delete_policy.as_ref().map(serde_json::to_string).transpose().map_err(unavailable)?,
                        record.versioning_configured,
                    ],
                )?;
            for (target, entries) in [
                (acl::ResolvedAclTarget::Bucket(&name), update.acl),
                (acl::ResolvedAclTarget::DefaultObject(&name), update.default_object_acl),
            ] {
                if let Some(entries) = entries {
                    acl::replace_resource_acl(&transaction, target, &entries, &access, principals.as_ref())?;
                }
            }
            change_journal::append(&transaction, change_journal::origin(&access), change_journal::ChangeKind::BucketUpdate, &name, None)?;
            transaction.commit()?;
            Ok(record)
        })
        .await
    }

    /// Reads the bucket and checks `access` against the owner observed by the same read.
    pub(crate) async fn get_bucket(
        &self,
        name: &str,
        access: &BucketAccess,
    ) -> Result<Option<BucketRecord>, CatalogError> {
        let name = name.to_owned();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            let record = select_bucket(&transaction, &name)?;
            if let Some(record) = &record {
                check_access(&transaction, &name, &record.project, &access)?;
            }
            Ok(record)
        })
        .await
    }

    /// Whether the bucket stores any CORS rule, without decoding the bucket row. A missing bucket
    /// has none. Every XML object request asks this to decide `Vary`, so it must stay cheap.
    pub(crate) async fn bucket_has_cors(&self, name: &str) -> Result<bool, CatalogError> {
        let name = name.to_owned();
        self.run(move |connection| {
            let stored = sql::row(
                connection,
                "SELECT cors FROM buckets WHERE name = ?1",
                [name],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
            Ok(stored.is_some_and(|cors| cors != "[]"))
        })
        .await
    }

    /// Returns at most `limit` rows within the expanded metadata budget (or one large row).
    /// Use `list_buckets_page` when a continuation indicator is needed.
    #[cfg(test)]
    pub(crate) async fn list_buckets(
        &self,
        project: &str,
        prefix: Option<&str>,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<BucketRecord>, CatalogError> {
        Ok(self
            .list_buckets_page(project, prefix, start_after, limit)
            .await?
            .items)
    }

    /// Bounds expansion before decoding, including lookahead; a single large row still advances.
    pub(crate) async fn list_buckets_page(
        &self,
        project: &str,
        prefix: Option<&str>,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<BucketListPage, CatalogError> {
        self.list_buckets_page_among(Some(vec![project.to_owned()]), prefix, start_after, limit)
            .await
    }

    /// Lists buckets owned by any of `projects`, or by every project when `projects` is `None`.
    pub(crate) async fn list_buckets_page_among(
        &self,
        projects: Option<Vec<String>>,
        prefix: Option<&str>,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<BucketListPage, CatalogError> {
        if limit > MAX_LIST_LIMIT {
            return Err(CatalogError::Unavailable(
                format!("list limit exceeds {MAX_LIST_LIMIT}").into(),
            ));
        }
        if projects.as_ref().is_some_and(Vec::is_empty) {
            return Ok(BucketListPage {
                items: Vec::new(),
                has_more: false,
            });
        }
        let prefix = prefix.unwrap_or("").to_owned();
        let start_after = start_after.map(str::to_owned);
        self.run(move |connection| {
            bucket_list_page(
                connection,
                projects.as_deref(),
                &prefix,
                start_after.as_deref(),
                limit,
            )
        })
        .await
    }

    /// Deletes the bucket only if `access` holds for the owner observed under the write lock.
    pub(crate) async fn delete_bucket(
        &self,
        name: &str,
        conditions: BucketConditions,
        access: &BucketAccess,
    ) -> Result<(), CatalogError> {
        let name = name.to_owned();
        let access = access.clone();
        let maintenance_wake = self.0.maintenance_wake.clone();
        let clock = self.0.clock.clone();
        self.run(move |connection| {
            let transaction = sql::immediate(connection)?;
            let (metageneration, project): (i64, String) = sql::row(
                &transaction,
                "SELECT metageneration, project FROM buckets WHERE name = ?1",
                [&name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(CatalogError::NotFound)?;
            check_access(&transaction, &name, &project, &access)?;
            if !conditions.hold(metageneration) {
                return Err(CatalogError::PreconditionFailed);
            }
            const NONEMPTY: &str = "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND soft_delete_time IS NULL)";
            let nonempty: bool = sql::scalar(&transaction, NONEMPTY, [&name])?;
            if nonempty {
                return Err(CatalogError::NotEmpty);
            }
            // With a soft delete policy the version and its retained objects move aside first,
            // so the permanent deletions below no longer find them.
            bucket_soft_delete::retire(&transaction, &name, unix_micros(clock.now())?)?;
            sql::exec(&transaction,
                "INSERT OR IGNORE INTO pending_blob_deletions SELECT blob_id FROM objects WHERE bucket = ?1",
                [&name],
            )?;
            sql::exec(&transaction, "DELETE FROM objects WHERE bucket = ?1", [&name])?;
            change_journal::append(
                &transaction,
                change_journal::origin(&access),
                change_journal::ChangeKind::BucketDelete,
                &name,
                None,
            )?;
            sql::exec(&transaction, "DELETE FROM bucket_policies WHERE bucket = ?1", [&name])?;
            sql::exec(&transaction, "DELETE FROM bucket_incarnations WHERE bucket = ?1", [&name])?;
            sql::exec(&transaction, "DELETE FROM sync_objects WHERE bucket = ?1", [&name])?;
            sql::exec(&transaction, "DELETE FROM buckets WHERE name = ?1", [&name])?;
            let result = transaction.commit().map_err(unavailable);
            // Commit failures may be ambiguous; cleanup rechecks the current bucket incarnation.
            maintenance_wake.notify_one();
            result
        })
        .await?;
        self.reclaim_after_commit().await;
        Ok(())
    }
}

/// Whether a bucket metadata write merges into (PATCH) or replaces (PUT) stored labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BucketWrite {
    Patch,
    Replace,
}

fn apply_uniform_access(
    record: &mut BucketRecord,
    requested: Option<bool>,
    now: SystemTime,
) -> Result<(), CatalogError> {
    let Some(enabled) = requested else {
        return Ok(());
    };
    match (enabled, record.uniform_bucket_level_access_locked_time) {
        (true, None) => {
            record.uniform_bucket_level_access_locked_time =
                Some(now.checked_add(UNIFORM_ACCESS_LOCK_PERIOD).ok_or_else(|| {
                    CatalogError::InvalidMetadata(
                        "Uniform access deadline is outside the supported time range".into(),
                    )
                })?);
        }
        (false, Some(locked_time)) => {
            if now >= locked_time {
                return Err(CatalogError::Metadata(
                    crate::metadata_error::MetadataError::UniformAccessLocked,
                ));
            }
            record.uniform_bucket_level_access_locked_time = None;
        }
        _ => {}
    }
    Ok(())
}

fn apply_soft_delete_retention(
    record: &mut BucketRecord,
    duration: u64,
    now: SystemTime,
) -> Result<(), CatalogError> {
    if duration != 0 && !(604800..=7776000).contains(&duration) {
        return Err(CatalogError::InvalidMetadata(
            "Soft delete policy must have a retention duration between 3 days and 90 days.".into(),
        ));
    }
    let effective_time = record
        .soft_delete_policy
        .as_ref()
        .filter(|policy| duration <= policy.retention_duration_seconds)
        .map_or(now, |policy| policy.effective_time);
    record.soft_delete_policy = Some(crate::bucket::SoftDeletePolicy {
        retention_duration_seconds: duration,
        effective_time,
    });
    Ok(())
}

/// Locks, sets or clears the bucket retention policy; a locked policy never shrinks.
fn validate_retention_expiration(
    connection: &Connection,
    record: &BucketRecord,
) -> Result<(), CatalogError> {
    let Some(policy) = &record.retention_policy else {
        return Ok(());
    };
    let latest_creation = i64::try_from(
        253_402_300_799_999_999_i128 - i128::from(policy.retention_period) * 1_000_000,
    )
    .map_err(unavailable)?;
    let invalid: bool = sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND time_created > ?2)",
        rusqlite::params![record.name, latest_creation],
    )?;
    if invalid {
        return Err(CatalogError::InvalidMetadata("Bucket retention would place an existing object's expiration outside the supported RFC3339 UTC range".into()));
    }
    Ok(())
}

fn apply_retention_policy(
    record: &mut BucketRecord,
    update: &BucketMetadataUpdate,
    if_metageneration_match: Option<i64>,
    now: SystemTime,
) -> Result<(), CatalogError> {
    if update.lock_retention_policy {
        if if_metageneration_match.is_none() {
            return Err(CatalogError::InvalidMetadata(
                "Locking requires ifMetagenerationMatch; read the bucket metageneration first"
                    .into(),
            ));
        }
        let policy = record
            .retention_policy
            .as_mut()
            .ok_or(CatalogError::RetentionPolicyNotSet)?;
        if policy.is_locked {
            return Err(CatalogError::InvalidMetadata(
                "Retention policy is already locked".into(),
            ));
        }
        policy.is_locked = true;
        return Ok(());
    }
    let Some(period) = update.retention_period else {
        return Ok(());
    };
    if update.omitted_retention
        && record
            .retention_policy
            .as_ref()
            .is_some_and(|p| p.is_locked)
    {
        return Ok(());
    }
    if let Some(policy) = record
        .retention_policy
        .as_ref()
        .filter(|p| p.is_locked && period.is_none_or(|period| period < p.retention_period))
    {
        return Err(CatalogError::RetentionPolicyLocked(policy.retention_period));
    }
    let Some(period) = period else {
        record.retention_policy = None;
        return Ok(());
    };
    if !(1..3_155_760_000).contains(&period) {
        return Err(CatalogError::InvalidMetadata(
            "Retention policy must have a retention period greater than 0 and less than 100 years."
                .into(),
        ));
    }
    let locked = record
        .retention_policy
        .as_ref()
        .is_some_and(|p| p.is_locked);
    if record
        .retention_policy
        .as_ref()
        .is_none_or(|p| p.retention_period != period)
    {
        record.retention_policy = Some(crate::bucket::RetentionPolicy {
            retention_period: period,
            effective_time: now,
            is_locked: locked,
        });
    }
    Ok(())
}

/// Applies the settings that only validate and replace a stored value.
fn apply_plain_settings(
    record: &mut BucketRecord,
    update: &mut BucketMetadataUpdate,
) -> Result<(), CatalogError> {
    if let Some(class) = update.storage_class.take() {
        record.storage_class = class;
    }
    if let Some(enabled) = update.versioning_enabled {
        record.versioning_enabled = enabled;
    }
    if let Some(configured) = update.versioning_configured {
        record.versioning_configured = configured;
    }
    if let Some(hold) = update.default_event_based_hold {
        record.default_event_based_hold = hold;
    }
    if let Some(enforced) = update.public_access_prevention_enforced {
        record.public_access_prevention_enforced = enforced;
    }
    if let Some(cors) = update.cors.take() {
        validate_cors(&cors)?;
        record.cors = cors;
    }
    if let Some(lifecycle) = update.lifecycle.take() {
        crate::bucket::lifecycle::validate(&lifecycle)?;
        crate::bucket::lifecycle::validate_location(&lifecycle, &record.location_type)?;
        record.lifecycle = lifecycle;
    }
    Ok(())
}

/// Applies a label update; `None` clears every label and a replace starts from none.
fn apply_labels(
    record: &mut BucketRecord,
    labels: Option<std::collections::BTreeMap<String, Option<String>>>,
    mode: BucketWrite,
    map_mode: crate::object::MapMode,
) -> Result<(), CatalogError> {
    if mode == BucketWrite::Replace || map_mode == crate::object::MapMode::Replace {
        record.labels.clear();
    }
    let Some(labels) = labels else {
        record.labels.clear();
        return Ok(());
    };
    for (key, value) in labels {
        // Library callers can bypass adapter validation.
        if ((mode == BucketWrite::Replace || map_mode == crate::object::MapMode::Replace)
            && value.is_none())
            || !valid_label(&key, LabelPart::Key)
            || value
                .as_deref()
                .is_some_and(|value| !valid_label(value, LabelPart::Value))
        {
            return Err(CatalogError::InvalidMetadata("Invalid bucket label".into()));
        }
        if let Some(value) = value {
            record.labels.insert(key, value);
        } else {
            record.labels.remove(&key);
        }
    }
    Ok(())
}

fn bucket_list_page(
    connection: &Connection,
    projects: Option<&[String]>,
    prefix: &str,
    start_after: Option<&str>,
    limit: usize,
) -> Result<BucketListPage, CatalogError> {
    use objects::SeekStart;
    use rusqlite::types::Value;
    let upper = objects::prefix_end(prefix);
    // Both bounds remain indexable, including when a page starts beyond the prefix.
    let (lower, comparison) = match start_after {
        Some(start) if start >= prefix => (start, SeekStart::Exclusive),
        _ => (prefix, SeekStart::Inclusive),
    };
    // One static text per bound shape keeps both name bounds indexable; the project list is
    // bound as one JSON array so its length does not change the statement text.
    macro_rules! projects {
        () => {
            "project IN (SELECT value FROM json_each(?)) AND "
        };
    }
    macro_rules! page {
        ($projects:expr, $comparison:literal, $upper:literal) => {
            concat!(
                "SELECT ",
                bucket_columns!(),
                " FROM buckets WHERE ",
                $projects,
                "name ",
                $comparison,
                " ? ",
                $upper,
                " ORDER BY name LIMIT ?"
            )
        };
    }
    let sql = match (projects.is_some(), comparison, upper.is_some()) {
        (false, SeekStart::Exclusive, false) => page!("", ">", ""),
        (false, SeekStart::Exclusive, true) => page!("", ">", "AND name < CAST(? AS TEXT)"),
        (false, SeekStart::Inclusive, false) => page!("", ">=", ""),
        (false, SeekStart::Inclusive, true) => page!("", ">=", "AND name < CAST(? AS TEXT)"),
        (true, SeekStart::Exclusive, false) => page!(projects!(), ">", ""),
        (true, SeekStart::Exclusive, true) => page!(projects!(), ">", "AND name < CAST(? AS TEXT)"),
        (true, SeekStart::Inclusive, false) => page!(projects!(), ">=", ""),
        (true, SeekStart::Inclusive, true) => {
            page!(projects!(), ">=", "AND name < CAST(? AS TEXT)")
        }
    };
    let mut values = Vec::new();
    if let Some(projects) = projects {
        values.push(Value::Text(
            serde_json::to_string(projects).map_err(unavailable)?,
        ));
    }
    values.push(Value::Text(lower.to_owned()));
    if let Some(upper) = upper {
        values.push(Value::Blob(upper));
    }
    values.push(Value::Integer(
        i64::try_from(limit).map_err(unavailable)? + 1,
    ));
    let mut statement = connection.prepare_cached(sql)?;
    let mut rows = statement.query(rusqlite::params_from_iter(values))?;
    let mut records = Vec::new();
    let mut remaining = BUCKET_LIST_METADATA_BUDGET;
    let mut has_more = false;
    while let Some(row) = rows.next()? {
        // Charge typed rules, JSON values and serialization before allocating any of them.
        let charge = bucket_metadata_charge(row)?;
        if records.len() == limit || (!records.is_empty() && charge > remaining) {
            has_more = !records.is_empty();
            break;
        }
        records.push(read_bucket(row)?);
        remaining = remaining.saturating_sub(charge);
    }
    Ok(BucketListPage {
        items: records,
        has_more,
    })
}

fn bucket_metadata_charge(row: &Row<'_>) -> Result<usize, CatalogError> {
    let mut charge = 4096usize;
    for (column, multiplier) in [
        (0, 8),
        (1, 8),
        (2, 8),
        (3, 8),
        (4, 8),
        (5, 16),
        (11, 64),
        (14, 64),
    ] {
        let bytes = row.get_ref(column)?.as_str().map_err(unavailable)?.len();
        charge = charge.saturating_add(bytes.saturating_mul(multiplier));
    }
    Ok(charge)
}

/// Largest creation-time seed for a bucket generation (2255-03-14T16:00:00Z, in nanoseconds). A
/// time past 2262 would saturate at `i64::MAX`, leaving no room for `last + 1` and failing every later
/// bucket with "generation exhausted"; the cap keeps about 2e17 allocations of headroom. The
/// price is that a bucket created after the cap gets `previous + 1` instead of a value tracking
/// its creation time (documented in docs/en/slices/bucket-soft-delete-restore.md).
const MAX_GENERATION_SEED: i64 = 9_000_000_000_000_000_000;

fn insert_bucket_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    record: &mut BucketRecord,
    default_soft_delete_retention: u64,
) -> Result<(), CatalogError> {
    // Cloud Storage gives a new bucket that names no policy a seven-day one whose
    // effectiveTime is the creation time. An explicit policy, zero included, is kept as
    // sent. A zero default leaves the bucket without a policy, as before the default existed.
    if record.soft_delete_policy.is_none() && default_soft_delete_retention > 0 {
        record.soft_delete_policy = Some(crate::bucket::SoftDeletePolicy {
            retention_duration_seconds: default_soft_delete_retention,
            effective_time: record.time_created,
        });
    }
    crate::bucket::lifecycle::validate(&record.lifecycle)?;
    crate::bucket::lifecycle::validate_location(&record.lifecycle, &record.location_type)?;
    let created = unix_micros(record.time_created)?;
    // Times outside the nanosecond i64 range (before 1677) fall back to the microsecond value.
    let created_nanos = unix_timestamp(record.time_created, 1)
        .unwrap_or_else(|_| created.saturating_mul(1000))
        .min(MAX_GENERATION_SEED);
    let updated = unix_micros(record.updated)?;
    record.time_created = system_time_micros(created)?;
    record.updated = system_time_micros(updated)?;
    record.uniform_bucket_level_access_locked_time = record
        .uniform_bucket_level_access_locked_time
        .map(|time| {
            unix_micros(time).and_then(|time| system_time_micros(time).map_err(unavailable))
        })
        .transpose()?;
    let labels = serde_json::to_string(&record.labels).map_err(unavailable)?;
    // Cloud Storage bucket generations are 19-digit nanosecond values. A dedicated allocator
    // keeps them unique and increasing even under a frozen injected clock, and across a
    // recreated name's soft-deleted versions, without disturbing object generations.
    let last: i64 = sql::scalar(
        transaction,
        "SELECT last_generation FROM bucket_generation WHERE singleton = 1",
        [],
    )?;
    record.generation = created_nanos.max(
        last.checked_add(1)
            .ok_or_else(|| unavailable("generation exhausted"))?,
    );
    let changed = transaction.prepare_cached(INSERT_BUCKET)?.execute(params![
        record.name,
        record.project,
        record.location,
        record.location_type,
        record.storage_class,
        labels,
        record.metageneration,
        created,
        updated,
        record.versioning_enabled,
        record.default_event_based_hold,
        cors_value(&record.cors).to_string(),
        record.public_access_prevention_enforced,
        record
            .uniform_bucket_level_access_locked_time
            .map(unix_micros)
            .transpose()?,
        serde_json::to_string(&record.lifecycle).map_err(unavailable)?,
        record
            .retention_policy
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(unavailable)?,
        record
            .soft_delete_policy
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(unavailable)?,
        record.object_retention_enabled,
        record.generation,
        record.versioning_configured,
    ])?;
    if changed == 0 {
        return Err(CatalogError::AlreadyExists);
    }
    sql::exec(
        transaction,
        "UPDATE bucket_generation SET last_generation = ?1 WHERE singleton = 1",
        [record.generation],
    )?;
    bucket_autoclass::insert(transaction, record)?;
    insert_policy(
        transaction,
        &record.name,
        &Policy::default_for(&record.project),
    )?;
    sql::exec(
        transaction,
        "INSERT INTO bucket_incarnations VALUES (?1, lower(hex(randomblob(16))))",
        [&record.name],
    )?;
    Ok(())
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

/// Reads one bucket through the cached statement that request paths share.
pub(super) fn select_bucket(
    connection: &Connection,
    name: &str,
) -> rusqlite::Result<Option<BucketRecord>> {
    connection
        .prepare_cached(SELECT_BUCKET)?
        .query_row([name], read_bucket)
        .optional()
}

fn read_bucket(row: &Row<'_>) -> rusqlite::Result<BucketRecord> {
    let labels: String = row.get(5)?;
    Ok(BucketRecord {
        soft_delete_policy: row
            .get::<_, Option<String>>(16)?
            .map(|v| serde_json::from_str(&v).map_err(|e| sql::conversion_error(16, Type::Text, e)))
            .transpose()?,
        retention_policy: row
            .get::<_, Option<String>>(15)?
            .map(|v| serde_json::from_str(&v).map_err(|e| sql::conversion_error(15, Type::Text, e)))
            .transpose()?,
        name: row.get(0)?,
        project: row.get(1)?,
        location: row.get(2)?,
        location_type: row.get(3)?,
        storage_class: row.get(4)?,
        autoclass: bucket_autoclass::read(row)?,
        labels: serde_json::from_str(&labels)
            .map_err(|e| sql::conversion_error(5, Type::Text, e))?,
        lifecycle: serde_json::from_str(&row.get::<_, String>(14)?)
            .map_err(|e| sql::conversion_error(14, Type::Text, e))?,
        metageneration: row.get(6)?,
        generation: row.get("generation")?,
        time_created: bucket_timestamps::read(row, row.get(7)?, "time_created_nanos")?,
        updated: bucket_timestamps::read(row, row.get(8)?, "updated_nanos")?,
        object_retention_enabled: row.get(17)?,
        versioning_enabled: row.get(9)?,
        versioning_configured: row.get("versioning_configured")?,
        default_event_based_hold: row.get(10)?,
        public_access_prevention_enforced: row.get(12)?,
        uniform_bucket_level_access_locked_time: row
            .get::<_, Option<i64>>(13)?
            .map(system_time_micros)
            .transpose()?,
        cors: cors_from_value(
            &serde_json::from_str(&row.get::<_, String>(11)?)
                .map_err(|e| sql::conversion_error(11, Type::Text, e))?,
        )
        .map_err(|e| sql::conversion_error(11, Type::Text, e))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_fixtures::PERMISSIVE;

    #[tokio::test]
    async fn unknown_catalog_classes_fail_bucket_and_object_read_paths() {
        use crate::object::{ObjectError, UploadChunk};
        use crate::storage::test_fixtures::{bucket, live, permissive_upload};
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        let (sender, body) = tokio::sync::mpsc::channel(1);
        sender.try_send(Ok(UploadChunk::End)).unwrap();
        catalog
            .put_object_prechecked(
                permissive_upload("bucket", "object", 1),
                body,
                catalog.admit_object_upload().unwrap(),
            )
            .await
            .unwrap();
        catalog
            .run(|connection| {
                sql::exec(
                    connection,
                    "UPDATE buckets SET storage_class='bogus' WHERE name='bucket'",
                    [],
                )?;
                Ok::<_, CatalogError>(())
            })
            .await
            .unwrap();
        let error = catalog.get_bucket("bucket", PERMISSIVE).await.unwrap_err();
        assert!(matches!(error, CatalogError::Corrupt(_)));
        assert_eq!(
            crate::json::JsonError::from(error).message,
            "Bucket catalog is unavailable"
        );
        let error = catalog
            .run(|connection| objects::authorized_bucket(connection, "bucket", PERMISSIVE))
            .await
            .unwrap_err();
        assert!(matches!(error, ObjectError::Corrupt(_)));
        catalog
            .run(|connection| {
                sql::exec(
                    connection,
                    "UPDATE buckets SET storage_class='STANDARD' WHERE name='bucket'",
                    [],
                )?;
                sql::exec(
                    connection,
                    "UPDATE objects SET storage_class='bogus' WHERE bucket='bucket'",
                    [],
                )?;
                Ok::<_, CatalogError>(())
            })
            .await
            .unwrap();
        let error = catalog
            .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap_err();
        assert!(matches!(error, ObjectError::Corrupt(_)));
        assert_eq!(
            crate::json::JsonError::from(error).message,
            "Object storage is unavailable: stored data is corrupt; restore a valid data directory"
        );
        let error = catalog
            .open_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .err()
            .unwrap();
        assert!(matches!(error, ObjectError::Corrupt(_)));
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn publication_keeps_unset_holds_and_default_object_acl_json() {
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog
            .insert_bucket(crate::storage::test_fixtures::bucket("bucket", "project"))
            .await
            .unwrap();
        let entries = vec![crate::acl::AclEntry {
            entity: "allUsers".into(),
            role: crate::acl::AclRole::Reader,
        }];
        catalog
            .patch_bucket(
                "bucket",
                BucketMetadataUpdate {
                    default_object_acl: Some(entries),
                    ..Default::default()
                },
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        let (sender, body) = tokio::sync::mpsc::channel(1);
        sender
            .try_send(Ok(crate::object::UploadChunk::End))
            .unwrap();
        let record = catalog
            .put_object_prechecked(
                crate::storage::test_fixtures::permissive_upload("bucket", "object", 1),
                body,
                catalog.admit_object_upload().unwrap(),
            )
            .await
            .unwrap();
        let resource = crate::json::object_resource(
            &record,
            "http://localhost",
            crate::json::Projection::NoAcl,
        )
        .unwrap();
        assert!(resource.get("eventBasedHold").is_none());
        assert!(resource.get("temporaryHold").is_none());
        let stored = catalog.run(|connection| {
            let default: String = sql::scalar(connection, "SELECT entries FROM acl_bucket_entries WHERE bucket='bucket' AND scope='defaultObject'", [])?;
            let object: String = sql::scalar(connection, "SELECT entries FROM acl_object_entries WHERE bucket='bucket' AND name='object'", [])?;
            Ok::<_, CatalogError>((default, object))
        }).await.unwrap();
        assert_eq!(stored.0, r#"[{"entity":"allUsers","role":"READER"}]"#);
        assert_eq!(
            stored.1,
            r#"[{"entity":"project-owners-0","role":"OWNER"},{"entity":"allUsers","role":"READER"}]"#
        );
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn acl_and_storage_class_keep_catalog_text() {
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        for (index, class) in [
            "STANDARD",
            "NEARLINE",
            "COLDLINE",
            "ARCHIVE",
            "MULTI_REGIONAL",
            "DURABLE_REDUCED_AVAILABILITY",
        ]
        .into_iter()
        .enumerate()
        {
            let name = format!("bucket-{index}");
            let mut record = crate::storage::test_fixtures::bucket(&name, "project");
            record.storage_class = class.parse().unwrap();
            catalog.insert_bucket(record).await.unwrap();
            for role in ["READER", "WRITER", "OWNER"] {
                catalog
                    .change_acl(
                        crate::acl::AclTarget::Bucket(name.clone()),
                        acl::AclChange::Upsert {
                            entity: "allUsers".into(),
                            role: Some(role.parse().unwrap()),
                        },
                        PERMISSIVE,
                    )
                    .await
                    .unwrap();
                let selected = name.clone();
                let stored = catalog
                    .run(move |connection| {
                        let class: String = sql::scalar(
                            connection,
                            "SELECT storage_class FROM buckets WHERE name=?1",
                            [&selected],
                        )?;
                        let entries: String = sql::scalar(
                            connection,
                            "SELECT entries FROM acl_bucket_entries WHERE bucket=?1 AND scope='bucket'",
                            [&selected],
                        )?;
                        Ok::<_, CatalogError>((class, entries))
                    })
                    .await
                    .unwrap();
                assert_eq!(stored.0, class, "{class} {role}");
                let entries: serde_json::Value = serde_json::from_str(&stored.1).unwrap();
                assert_eq!(
                    entries
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|entry| entry["entity"] == "allUsers")
                        .unwrap(),
                    &serde_json::json!({"entity":"allUsers","role":role}),
                    "{class} {role}"
                );
            }
        }
        catalog.close().await.unwrap();
    }

    /// Shutdown must not look like overload: only a full catalog asks the client to retry.
    #[tokio::test]
    async fn a_closed_catalog_reports_closed_and_a_full_one_reports_its_limit() {
        use crate::object::ObjectError;

        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(
            catalog
                .run::<(), ObjectError>(|_| Err(ObjectError::ObjectNotFound))
                .await
                .unwrap_err(),
            ObjectError::ObjectNotFound
        );
        let record =
            crate::json::parse_insert(br#"{"name":"admission"}"#, "project", UNIX_EPOCH).unwrap();
        let held = catalog
            .0
            .admission
            .clone()
            .try_acquire_many_owned(u32::try_from(MAX_IN_FLIGHT).unwrap())
            .unwrap();
        let full = catalog.insert_bucket(record.clone()).await.unwrap_err();
        assert!(
            matches!(&full, CatalogError::Unavailable(cause) if *cause == "catalog operation limit reached"),
            "{full}"
        );
        drop(held);
        catalog.close().await.unwrap();
        let closed = catalog.insert_bucket(record).await.unwrap_err();
        assert!(
            matches!(&closed, CatalogError::Unavailable(cause) if *cause == "catalog is closed"),
            "{closed}"
        );
        let closed_object = catalog
            .run::<(), ObjectError>(|_| Ok(()))
            .await
            .unwrap_err();
        assert_eq!(
            closed_object,
            ObjectError::from(CatalogError::Unavailable("catalog is closed".into()))
        );
    }

    #[tokio::test]
    async fn bucket_patch_rechecks_revocation_and_bucket_recreation() {
        bucket_metadata_rechecks_revocation(BucketWrite::Patch).await;
    }

    #[tokio::test]
    async fn bucket_put_rechecks_revocation_and_bucket_recreation() {
        bucket_metadata_rechecks_revocation(BucketWrite::Replace).await;
    }

    async fn bucket_metadata_rechecks_revocation(mode: BucketWrite) {
        use crate::auth::{BUCKETS_UPDATE, Binding};
        let directory = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&directory.0).await.unwrap();
        let record =
            crate::json::parse_insert(br#"{"name":"patch-policy"}"#, "project", UNIX_EPOCH)
                .unwrap();
        catalog.insert_bucket(record.clone()).await.unwrap();
        catalog
            .set_policy(
                "patch-policy",
                Policy {
                    bindings: vec![Binding {
                        role: "roles/storage.admin".into(),
                        members: vec!["allUsers".into()],
                    }],
                    etag: None,
                },
                PERMISSIVE,
            )
            .await
            .unwrap();
        let access = catalog
            .bucket_access("patch-policy", None, BUCKETS_UPDATE)
            .await
            .unwrap();
        catalog
            .set_policy(
                "patch-policy",
                Policy {
                    bindings: vec![],
                    etag: None,
                },
                PERMISSIVE,
            )
            .await
            .unwrap();
        let update = BucketMetadataUpdate {
            labels: Some(None),
            ..Default::default()
        };
        let before = catalog
            .get_bucket("patch-policy", PERMISSIVE)
            .await
            .unwrap();
        let conditions = BucketConditions {
            if_metageneration_match: Some(99),
            ..Default::default()
        };
        assert_eq!(
            catalog
                .update_bucket("patch-policy", update.clone(), conditions, &access, mode)
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog
                .get_bucket("patch-policy", PERMISSIVE)
                .await
                .unwrap(),
            before
        );
        let invalid = BucketMetadataUpdate {
            labels: Some(Some(std::collections::BTreeMap::from([(
                "Bad".into(),
                Some("value".into()),
            )]))),
            ..Default::default()
        };
        assert_eq!(
            catalog
                .update_bucket("patch-policy", invalid.clone(), conditions, &access, mode)
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog
                .get_bucket("patch-policy", PERMISSIVE)
                .await
                .unwrap(),
            before
        );
        catalog
            .delete_bucket("patch-policy", Default::default(), PERMISSIVE)
            .await
            .unwrap();
        let mut replacement = record;
        replacement.project = "other".into();
        let replacement = catalog.insert_bucket(replacement).await.unwrap();
        assert_eq!(
            catalog
                .update_bucket("patch-policy", update, Default::default(), &access, mode)
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog
                .get_bucket("patch-policy", PERMISSIVE)
                .await
                .unwrap(),
            Some(replacement.clone())
        );
        assert_eq!(
            catalog
                .update_bucket("patch-policy", invalid, Default::default(), &access, mode)
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog
                .get_bucket("patch-policy", PERMISSIVE)
                .await
                .unwrap(),
            Some(replacement)
        );
        catalog.close().await.unwrap();
    }

    #[test]
    fn bucket_settings_migration_rolls_back_columns_and_version_after_late_failure() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(BUCKET_SCHEMA).unwrap();
        connection.pragma_update(None, "user_version", 7).unwrap();
        // The first added column fits; the second exceeds this connection-local limit.
        #[expect(
            unsafe_code,
            reason = "Test-only SQLite limits avoid enabling the limits feature in the production dependency."
        )]
        // SAFETY: The connection owns the SQLite handle throughout this synchronous call.
        unsafe {
            rusqlite::ffi::sqlite3_limit(
                connection.handle(),
                rusqlite::ffi::SQLITE_LIMIT_COLUMN,
                10,
            );
        }
        let error = Migration::Statements(BUCKET_SETTINGS_SCHEMA)
            .apply(&mut connection)
            .unwrap_err();
        assert!(error.to_string().contains("too many columns"), "{error}");
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            7
        );
        let columns: Vec<String> = connection
            .prepare("PRAGMA table_info(buckets)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            !columns
                .iter()
                .any(|name| name == "versioning_enabled" || name == "default_event_based_hold")
        );
        #[expect(
            unsafe_code,
            reason = "Restore the test-only SQLite limit without adding production dependency features."
        )]
        // SAFETY: Restore the limit on the same live connection before retrying the migration.
        unsafe {
            rusqlite::ffi::sqlite3_limit(
                connection.handle(),
                rusqlite::ffi::SQLITE_LIMIT_COLUMN,
                2000,
            );
        }
        Migration::Statements(BUCKET_SETTINGS_SCHEMA)
            .apply(&mut connection)
            .unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            8
        );
    }

    #[test]
    fn immutable_uri_preserves_native_windows_paths() {
        for (path, encoded) in [
            (r"C:\catalog.sqlite3", "C:%5Ccatalog.sqlite3"),
            (
                r"\\server\share\catalog.sqlite3",
                "%5C%5Cserver%5Cshare%5Ccatalog.sqlite3",
            ),
            (
                r"\\?\C:\catalog.sqlite3",
                "%5C%5C%3F%5CC:%5Ccatalog.sqlite3",
            ),
            (
                r"\\?\UNC\server\share\catalog.sqlite3",
                "%5C%5C%3F%5CUNC%5Cserver%5Cshare%5Ccatalog.sqlite3",
            ),
        ] {
            assert_eq!(
                immutable_catalog_uri(Path::new(path)).unwrap(),
                format!("file:{encoded}?immutable=1")
            );
        }
    }

    #[test]
    fn immutable_uri_reads_exact_unicode_and_reserved_filename() {
        let parent = crate::test_support::TestDir::new();
        let dir = parent.0.join("日本語 # percent%");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("catalog.sqlite3");
        let writable = Connection::open(&path).unwrap();
        writable
            .execute_batch("CREATE TABLE sentinel(value); INSERT INTO sentinel VALUES (42);")
            .unwrap();
        writable.close().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let readonly = Connection::open_with_flags(
            immutable_catalog_uri(&path.canonicalize().unwrap()).unwrap(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .unwrap();
        assert_eq!(
            readonly
                .query_row("SELECT value FROM sentinel", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            42
        );
        assert!(
            readonly
                .execute("INSERT INTO sentinel VALUES (43)", [])
                .is_err()
        );
        readonly.close().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn close_waits_for_cancelled_blocking_work_and_invalidates_clones() {
        use std::{future::Future, task::Poll};

        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let worker = catalog.clone();
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, finish) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            worker
                .run(move |_| {
                    started.send(()).unwrap();
                    finish.recv().unwrap();
                    Ok::<_, CatalogError>(())
                })
                .await
        });
        running.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        {
            let mut close = std::pin::pin!(catalog.close());
            assert!(
                std::future::poll_fn(|cx| Poll::Ready(close.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            assert!(!catalog.is_healthy());
            assert!(matches!(
                catalog.get_bucket("missing", PERMISSIVE).await,
                Err(CatalogError::Unavailable(_))
            ));
            assert!(Catalog::open(&dir.0).await.is_err());
            release.send(()).unwrap();
            close.await.unwrap();
        }
        let reopened = Catalog::open(&dir.0).await.unwrap();
        catalog.close().await.unwrap();
        drop(catalog);
        assert!(Catalog::open(&dir.0).await.is_err());
        reopened.close().await.unwrap();
    }

    #[test]
    fn catalog_operations_do_not_wait_for_a_saturated_blocking_pool() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let dir = crate::test_support::TestDir::new();
            let catalog = Catalog::open(&dir.0).await.unwrap();
            let (release, held) = std::sync::mpsc::channel::<()>();
            let (entered, blocked) = tokio::sync::oneshot::channel();
            // File I/O and gzip work share this pool; here it holds the only thread.
            let blocker = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                held.recv()
            });
            blocked.await.unwrap();
            let lookup = tokio::time::timeout(
                Duration::from_secs(5),
                catalog.get_bucket("missing", PERMISSIVE),
            )
            .await;
            release.send(()).unwrap();
            blocker.await.unwrap().unwrap();
            assert!(lookup.unwrap().unwrap().is_none());
            catalog.close().await.unwrap();
        });
    }

    /// Opens a raw transaction that rusqlite will not roll back on unwind, then panics.
    async fn panic_inside_transaction(catalog: &Catalog) -> CatalogError {
        catalog
            .run::<(), CatalogError>(|connection| {
                connection
                    .execute_batch("BEGIN IMMEDIATE; CREATE TABLE panic_probe (x INTEGER);")
                    .unwrap();
                panic!("injected catalog panic")
            })
            .await
            .unwrap_err()
    }

    async fn probe_exists(catalog: &Catalog) -> Result<bool, CatalogError> {
        catalog
            .run(|connection| {
                connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'panic_probe')",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(unavailable)
            })
            .await
    }

    #[tokio::test]
    async fn panicked_operation_rolls_back_and_keeps_the_catalog_ready() {
        use crate::server::State;
        use std::sync::atomic::Ordering;

        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let state = State {
            api_base_url: "http://127.0.0.1:4443".into(),
            ..State::for_test(catalog.clone(), Default::default())
        };
        assert!(state.is_ready());
        assert!(matches!(
            panic_inside_transaction(&catalog).await,
            CatalogError::Unavailable(_)
        ));
        assert!(state.is_ready());
        assert!(!probe_exists(&catalog).await.unwrap());
        assert!(
            catalog
                .get_bucket("abc", PERMISSIVE)
                .await
                .unwrap()
                .is_none()
        );

        // When the rollback cannot be confirmed the catalog fails closed and
        // readiness is revoked for good, even after a later call recovers.
        catalog.0.fail_rollback.store(true, Ordering::Release);
        assert!(matches!(
            panic_inside_transaction(&catalog).await,
            CatalogError::Unavailable(_)
        ));
        assert!(!state.is_ready());
        assert!(!state.ready.load(Ordering::Acquire));
        assert!(matches!(
            catalog.get_bucket("abc", PERMISSIVE).await,
            Err(CatalogError::Unavailable(_))
        ));
        catalog.0.fail_rollback.store(false, Ordering::Release);
        assert!(!probe_exists(&catalog).await.unwrap());
        assert!(catalog.is_healthy());
        assert!(!state.is_ready());
        catalog.close().await.unwrap();
        Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
        drop(state);
        drop(catalog);
    }

    #[test]
    fn timestamps_use_integer_unix_milliseconds() {
        assert_eq!(
            unix_millis(UNIX_EPOCH + Duration::from_micros(1999)).unwrap(),
            1
        );
        assert_eq!(
            unix_millis(UNIX_EPOCH - Duration::from_micros(1)).unwrap(),
            -1
        );
        for millis in [-1001, -1, 0, 1, 1001] {
            assert_eq!(
                unix_millis(system_time_millis(millis).unwrap()).unwrap(),
                millis
            );
        }
    }

    #[tokio::test]
    async fn admission_is_bounded_and_sqlite_settings_are_explicit() {
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let permits = catalog
            .0
            .admission
            .clone()
            .try_acquire_many_owned(MAX_IN_FLIGHT as u32)
            .unwrap();
        assert!(matches!(
            catalog.get_bucket("bucket", PERMISSIVE).await,
            Err(CatalogError::Unavailable(_))
        ));
        drop(permits);
        catalog
            .run(|connection| {
                for (pragma, expected) in [
                    ("synchronous", 2),
                    ("foreign_keys", 1),
                    ("cache_size", -2048),
                    ("mmap_size", 0),
                    ("busy_timeout", 5000),
                    ("user_version", CATALOG_SCHEMA_VERSION),
                ] {
                    assert_eq!(
                        connection.pragma_query_value(None, pragma, |row| row.get::<_, i64>(0))?,
                        expected,
                        "{pragma}"
                    );
                }
                assert_eq!(
                    connection
                        .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?,
                    "wal"
                );
                Ok::<_, CatalogError>(())
            })
            .await
            .unwrap();
        catalog.close().await.unwrap();
    }
}
