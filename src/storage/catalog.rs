use super::lock;
use crate::auth::{Identity, OBJECTS_DELETE, Policy};
use crate::bucket::{
    BucketAccess, BucketMetadataUpdate, BucketRecord, CatalogError, LabelPart,
    UNIFORM_ACCESS_LOCK_PERIOD, cors_from_value, cors_value, valid_label, validate_cors,
    validate_storage_class,
};
use crate::clock::{Clock, SystemClock};
use crate::storage::failure::unavailable;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params, types::Type,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Notify, Semaphore};

#[cfg(test)]
mod timestamp_tests;

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
            Ok(())
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
pub(crate) use upload_replay::{CopyReplay, CopyReplaySource, ReplayInvocation};

mod resumable_maintenance;

mod resumable_recovery;

mod compose;
pub(crate) use compose::SourceDisposal;
mod compose_crc32c;
pub(crate) mod rewrite;
mod rewrite_token;

mod customer_keys;

mod appendable;
pub(crate) use appendable::{AppendError, AppendWriter};

mod object_move;
pub(crate) use object_move::MoveAccess;
mod objects;
#[cfg(test)]
pub(crate) use objects::Versioning;
mod soft_delete;
pub(crate) use objects::{
    ObjectListCursor, ObjectListPage, ObjectListQuery, ObjectReader, ObjectSelector, ObjectUpload,
};

mod derived;
#[cfg(test)]
pub(crate) use derived::DerivedTransfer;
pub(crate) use derived::{
    DerivedDestination, DerivedRequestGuard, DerivedSource, DerivedSourceSpec,
};

pub mod admin;
pub(crate) mod bucket_autoclass;
mod notifications;
mod policies;
use policies::{check_access, insert_policy};

pub(crate) mod change_journal;

pub mod import_journal;

pub mod sync_state;

pub(crate) mod hmac_keys;

pub(crate) mod acl;

const MAX_IN_FLIGHT: usize = 32;
const MAX_LIST_LIMIT: usize = 1001;
const BUCKET_LIST_METADATA_BUDGET: usize = 256 * 1024;
const CATALOG_SCHEMA_VERSION: i64 = 35;
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
                      object_retention_enabled, autoclass"
    };
}
use bucket_columns;
const INSERT_BUCKET: &str = concat!(
    "INSERT INTO buckets (",
    bucket_columns!(),
    ") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, NULL)
         ON CONFLICT(name) DO NOTHING"
);
const SELECT_BUCKET: &str = concat!(
    "SELECT ",
    bucket_columns!(),
    " FROM buckets WHERE name = ?1"
);
const UPDATE_BUCKET: &str = "UPDATE buckets SET labels = ?2, storage_class = ?3,
                     versioning_enabled = ?4, default_event_based_hold = ?5,
                     metageneration = ?6, updated = ?7, cors = ?8, public_access_prevention_enforced = ?9,
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

/// A bounded keyset page. Continue after the last returned name when `has_more` is true.
pub(crate) struct BucketListPage {
    pub items: Vec<BucketRecord>,
    pub has_more: bool,
}

fn migrate_bucket_settings(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction
        .execute_batch(BUCKET_SETTINGS_SCHEMA)
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

fn migrate_timestamp_precision(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
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
        let invalid: bool = transaction
            .query_row(check, [], |row| row.get(0))
            .map_err(unavailable)?;
        if invalid {
            return Err(unavailable(format!(
                "{table} timestamps cannot be converted to integer Unix microseconds"
            )));
        }
    }
    transaction.execute_batch(
        "UPDATE buckets SET time_created = time_created * 1000, updated = updated * 1000;
         UPDATE objects SET time_created = time_created * 1000, updated = updated * 1000, time_deleted = time_deleted * 1000;",
    ).map_err(unavailable)?;
    transaction
        .pragma_update(None, "user_version", TIMESTAMP_MICROS_SCHEMA_VERSION)
        .map_err(unavailable)?;
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
];

fn validate_seed(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection
        // xtask-lint: allow(text::uncached_statement) -- schema validation runs once at open
        .query_row(
            "SELECT count(*) = 1 AND min(singleton) = 1 FROM startup_seed",
            [],
            |r| r.get(0),
        )
        .map_err(unavailable)?;
    if valid {
        Ok(())
    } else {
        Err(unavailable("invalid startup seed state"))
    }
}

fn validate_schema(connection: &Connection, version: i64) -> Result<(), CatalogError> {
    let expected = Connection::open_in_memory().map_err(unavailable)?;
    expected.execute_batch(BUCKET_SCHEMA).map_err(unavailable)?;
    for &(introduced, schema, check) in SCHEMA_STEPS {
        if version < introduced {
            break;
        }
        expected.execute_batch(schema).map_err(unavailable)?;
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
            db.prepare_cached("SELECT type, name, tbl_name, substr(sql, 1, 16385) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name LIMIT 257")
            .map_err(unavailable)?.query_map([], |row| {
                let sql: String = row.get(3)?;
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, sql.split_whitespace().collect::<Vec<_>>().join(" ")))
            }).map_err(unavailable)?.collect::<rusqlite::Result<_>>().map_err(unavailable)
        };
    // Versioned catalogs must retain our constraints, collations, indexes and table inventory.
    let actual = inventory(connection)?;
    if actual.len() > 256 || actual != inventory(&expected)? {
        return Err(unavailable("catalog schema does not match its version"));
    }
    if version >= 3 {
        // xtask-lint: allow(text::uncached_statement) -- schema validation runs once at open
        let valid: bool = connection.query_row("SELECT count(*) = 1 AND min(singleton) = 1 AND min(last_generation) >= (SELECT coalesce(max(generation), 0) FROM objects) FROM object_generation", [], |row| row.get(0)).map_err(unavailable)?;
        if !valid {
            return Err(unavailable("invalid generation allocator state"));
        }
    }
    Ok(())
}

struct Inner {
    resources: Mutex<Option<Resources>>,
    admission: Arc<Semaphore>,
    execution: Arc<Semaphore>,
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
    change_wake: Notify,
    data_dir: PathBuf,
    clock: Arc<dyn Clock>,
    session_writers: Mutex<std::collections::HashSet<String>>,
    derived_wake: Notify,
    maintenance_wake: Arc<Notify>,
    maintenance_error: Mutex<Option<String>>,
    maintenance_started: AtomicBool,
    #[cfg(test)]
    fail_rollback: AtomicBool,
}

struct Resources {
    // Drop the connection before releasing directory ownership, including on cancelled calls.
    connection: Connection,
    lock: lock::DirectoryLock,
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
            Self::Closed => f.write_str("catalog is closed"),
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
            Self::Closed | Self::Panicked => None,
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
            change_journal::notify_on_write(connection, &self.change_wake, operation)
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

/// A blocking catalog task fails as a whole only by panicking or when the
/// runtime shuts down; either way the request gets a retryable error.
fn join_unavailable(error: &tokio::task::JoinError) -> CatalogError {
    if error.is_panic() {
        CatalogError::Unavailable("catalog operation panicked".into())
    } else {
        CatalogError::Unavailable("catalog operation was cancelled".into())
    }
}

#[derive(Default)]
struct UploadState {
    active: usize,
    closed: bool,
}

pub(crate) struct UploadGuard {
    catalog: Option<Catalog>,
    _admin: Option<tokio::sync::OwnedSemaphorePermit>,
    replay: Option<upload_replay::UploadReplay>,
}

impl From<tokio::sync::OwnedSemaphorePermit> for UploadGuard {
    fn from(permit: tokio::sync::OwnedSemaphorePermit) -> Self {
        Self {
            catalog: None,
            _admin: Some(permit),
            replay: None,
        }
    }
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        if let Some(catalog) = &self.catalog {
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
fn inspect_catalog(path: &Path) -> Result<i64, CatalogError> {
    // A read-write connection can checkpoint a rejected catalog's hot WAL on close.
    let connection = if path.try_exists().map_err(unavailable)? {
        if path
            .with_extension("sqlite3-wal")
            .try_exists()
            .map_err(unavailable)?
        {
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        } else {
            // Directory ownership makes a WAL-free main file safe for immutable reads.
            let absolute = path.canonicalize().map_err(unavailable)?;
            Connection::open_with_flags(
                immutable_catalog_uri(&absolute)?,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
        }
    } else {
        Connection::open_in_memory()
    }
    .map_err(unavailable)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(unavailable)?;
    connection
        .execute_batch(
            "PRAGMA cache_size = -2048;
                 PRAGMA mmap_size = 0;
                 PRAGMA temp_store = FILE;",
        )
        .map_err(unavailable)?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(unavailable)?;
    validate_schema_version(version, CATALOG_SCHEMA_VERSION)?;
    if version == 2 {
        return Err(unavailable(
            "unsupported unreleased catalog schema version 2; deleted generation history cannot be recovered",
        ));
    }
    let check: String = connection
        // xtask-lint: allow(text::uncached_statement) -- an integrity check that runs once at open
        .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
        .map_err(unavailable)?;
    if check != "ok" {
        return Err(CatalogError::Unavailable(
            format!("catalog quick_check failed: {check}").into(),
        ));
    }
    if version == 0 {
        let tables: i64 = connection
            // xtask-lint: allow(text::uncached_statement) -- schema inspection runs once at open
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
                [],
                |r| r.get(0),
            )
            .map_err(unavailable)?;
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
fn open_writable(path: &Path) -> Result<Connection, CatalogError> {
    let connection = Connection::open(path).map_err(unavailable)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(unavailable)?;
    connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
                 PRAGMA cache_size = -2048;
                 PRAGMA mmap_size = 0;
                 PRAGMA temp_store = FILE;",
        )
        .map_err(unavailable)?;
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .map_err(unavailable)?;
    if mode != "wal" {
        let mode: String = connection
            // xtask-lint: allow(text::uncached_statement) -- switches the journal mode once at open
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .map_err(unavailable)?;
        if mode != "wal" {
            return Err(CatalogError::Unavailable(
                "SQLite WAL mode is required".into(),
            ));
        }
    }
    connection
        .execute_batch("PRAGMA synchronous = FULL; PRAGMA wal_autocheckpoint = 256;")
        .map_err(unavailable)?;
    Ok(connection)
}

/// Creates a new catalog at schema version 5, the base the migrations continue from.
fn initialize_catalog(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction
        .execute_batch(BUCKET_SCHEMA)
        .map_err(unavailable)?;
    // Freshness and the retryable seed state must survive the same commit.
    for schema in [
        objects::SCHEMA,
        objects::GENERATION_SCHEMA,
        objects::METADATA_SCHEMA,
        SEED_SCHEMA,
    ] {
        transaction.execute_batch(schema).map_err(unavailable)?;
    }
    transaction
        // xtask-lint: allow(text::uncached_statement) -- creates a new catalog once at open
        .execute("INSERT INTO object_generation VALUES (1, 0)", [])
        .map_err(unavailable)?;
    transaction
        // xtask-lint: allow(text::uncached_statement) -- creates a new catalog once at open
        .execute("INSERT INTO startup_seed VALUES (1, 0)", [])
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

fn migrate_seed(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction
        .execute_batch(SEED_SCHEMA)
        .map_err(unavailable)?;
    // Older catalogs have already served user mutations, even if now empty.
    transaction
        // xtask-lint: allow(text::uncached_statement) -- a one-time schema migration at open
        .execute("INSERT INTO startup_seed VALUES (1, 1)", [])
        .map_err(unavailable)?;
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
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction.execute_batch(schema).map_err(unavailable)?;
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
    (8, Migration::Function(migrate_bucket_settings)),
    (9, Migration::Function(objects::migrate_versioning)),
    (10, Migration::Statements(derived::SCHEMA)),
    (11, Migration::Statements(rewrite::SCHEMA)),
    (12, Migration::Statements(BUCKET_CORS_SCHEMA)),
    (13, Migration::Statements(BUCKET_PAP_SCHEMA)),
    (
        TIMESTAMP_MICROS_SCHEMA_VERSION,
        Migration::Function(migrate_timestamp_precision),
    ),
    (15, Migration::Function(change_journal::migrate)),
    (16, Migration::Function(import_journal::migrate)),
    (17, Migration::Function(sync_state::migrate)),
    (18, Migration::Function(import_journal::migrate_failure)),
    (19, Migration::Function(sync_state::root::migrate)),
    (20, Migration::Function(sync_state::writeback::migrate)),
    (21, Migration::Function(sync_state::findings::migrate)),
    (22, Migration::Statements(OBJECT_HOLDS_CUSTOM_TIME_SCHEMA)),
    (23, Migration::Statements(BUCKET_UNIFORM_ACCESS_SCHEMA)),
    (24, Migration::Statements(BUCKET_LIFECYCLE_SCHEMA)),
    (25, Migration::Statements(BUCKET_RETENTION_SCHEMA)),
    (26, Migration::Statements(SOFT_DELETE_SCHEMA)),
    (27, Migration::Function(sync_state::buckets::migrate)),
    (28, Migration::Function(notifications::migrate)),
    (29, Migration::Function(objects::retention::migrate)),
    (30, Migration::Function(customer_keys::migrate)),
    (31, Migration::Function(bucket_autoclass::migrate)),
    (32, Migration::Function(hmac_keys::migrate)),
    (33, Migration::Function(acl::migrate)),
    (34, Migration::Function(appendable::migrate)),
    (35, Migration::Function(upload_replay::migrate)),
];

/// Resets volatile state and completes crash recovery before the catalog serves requests.
fn recover_on_open(
    connection: &mut Connection,
    data_dir: &Path,
    clock: &dyn Clock,
) -> Result<(), CatalogError> {
    let pending: bool = connection
        // xtask-lint: allow(text::uncached_statement) -- crash recovery runs once at open
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_writeback_ops WHERE phase!=4)",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if pending {
        // xtask-lint: allow(text::uncached_statement) -- crash recovery runs once at open
        connection.execute("UPDATE sync_writeback_roots SET recovery=1 WHERE EXISTS(SELECT 1 FROM sync_writeback_ops p WHERE p.root_id=sync_writeback_roots.root_id AND p.phase!=4)",[]).map_err(unavailable)?;
    }
    let journal_enabled: bool = connection
        // xtask-lint: allow(text::uncached_statement) -- crash recovery runs once at open
        .query_row(
            "SELECT enabled FROM change_journal_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if journal_enabled {
        // shortcut: restart requires a full rescan, add durable consumer checkpoints when restart continuity is needed.
        connection
            // xtask-lint: allow(text::uncached_statement) -- crash recovery runs once at open
            .execute(
                "UPDATE change_journal_state SET enabled = 0, epoch = epoch + 1 WHERE singleton = 1",
                [],
            )
            .map_err(unavailable)?;
    }
    connection.execute_batch("CREATE TEMP TABLE parent_public_access_prevention (project TEXT PRIMARY KEY) WITHOUT ROWID;").map_err(unavailable)?;
    let recovery_now = unix_millis(clock.now())?;
    rewrite::recover_due(connection, recovery_now)?;
    resumable_maintenance::recover_due(connection, data_dir, recovery_now)?;
    objects::recover(connection, data_dir, recovery_now)?;
    appendable::recover(connection, data_dir)
}

impl Catalog {
    /// Acquires directory ownership and completes SQLite recovery before returning.
    pub async fn open(data_dir: impl AsRef<Path>) -> Result<Self, CatalogError> {
        Self::open_with_clock(data_dir, Arc::new(SystemClock)).await
    }

    pub async fn open_with_clock(
        data_dir: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, CatalogError> {
        let data_dir = data_dir.as_ref().to_owned();
        let catalog = tokio::task::spawn_blocking(move || -> Result<Self, CatalogError> {
            let lock = lock::acquire(&data_dir)?;
            let path = data_dir.join("catalog.sqlite3");
            let stored_version = inspect_catalog(&path)?;
            let mut connection = open_writable(&path)?;
            let mut version = stored_version;
            if version == 0 {
                initialize_catalog(&mut connection)?;
                version = 5;
            }
            // A version marker alone must not make a missing or incompatible schema ready.
            connection
                .prepare_cached("SELECT name, project, location, location_type, storage_class, labels, metageneration, time_created, updated FROM buckets LIMIT 0")
                .map_err(unavailable)?;
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
            recover_on_open(&mut connection, &data_dir, &*clock)?;
            Ok(Self(Arc::new(Inner {
                resources: Mutex::new(Some(Resources { connection, lock })),
                admission: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
                execution: Arc::new(Semaphore::new(1)),
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
                file_workers: admin::FileWorkers::new().map_err(unavailable)?,
                admin_shutdown: Notify::new(),
                change_wake: Notify::new(),
                data_dir,
                clock,
                session_writers: Mutex::new(Default::default()),
                derived_wake: Notify::new(),
                maintenance_wake: Arc::new(Notify::new()),
                maintenance_error: Mutex::new(None),
                maintenance_started: AtomicBool::new(false),
                #[cfg(test)]
                fail_rollback: AtomicBool::new(false),
            })))
        })
        .await
        .map_err(unavailable)??;
        Ok(catalog)
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
        self.0.maintenance_wake.notify_one();
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
        tokio::task::spawn_blocking(move || {
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
                lock.release().map_err(unavailable)?;
            }
            Ok(())
        })
        .await
        .map_err(unavailable)?
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, CatalogError> + Send + 'static,
    ) -> Result<T, CatalogError> {
        let admission = self
            .0
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| CatalogError::Unavailable("catalog operation limit reached".into()))?;
        self.run_admitted(admission, operation).await
    }

    async fn run_waiting<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, CatalogError> + Send + 'static,
    ) -> Result<T, CatalogError> {
        let admission = self
            .0
            .admission
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| unavailable("catalog is closed"))?;
        self.run_admitted(admission, operation).await
    }

    async fn run_admitted<T: Send + 'static>(
        &self,
        admission: tokio::sync::OwnedSemaphorePermit,
        operation: impl FnOnce(&mut Connection) -> Result<T, CatalogError> + Send + 'static,
    ) -> Result<T, CatalogError> {
        let execution = self
            .0
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let (_admission, _execution) = (admission, execution);
            let result = match inner.with_connection(operation) {
                Ok(result) => result,
                Err(ConnectionUnavailable::Closed) => {
                    Err(CatalogError::Unavailable("catalog is closed".into()))
                }
                Err(error) => Err(unavailable(error)),
            };
            // Release ownership before Tokio publishes completion to an immediate reopen.
            drop(inner);
            result
        })
        .await
        .map_err(|error| join_unavailable(&error))?
    }

    pub(crate) async fn insert_bucket(
        &self,
        mut record: BucketRecord,
    ) -> Result<BucketRecord, CatalogError> {
        self.run(move |connection| {
            validate_cors(&record.cors)?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            insert_bucket_in_transaction(&transaction, &mut record)?;
            change_journal::append(
                &transaction,
                admin::Origin::Api,
                change_journal::ChangeKind::BucketCreate,
                &record.name,
                None,
            )?;
            transaction.commit().map_err(unavailable)?;
            Ok(record)
        })
        .await
    }

    pub(crate) async fn get_bucket(
        &self,
        name: &str,
    ) -> Result<Option<BucketRecord>, CatalogError> {
        self.get_bucket_authorized(name, &BucketAccess::Permissive)
            .await
    }

    pub(crate) async fn patch_bucket(
        &self,
        name: &str,
        update: BucketMetadataUpdate,
        if_metageneration_match: Option<i64>,
        if_metageneration_not_match: Option<i64>,
        access: &BucketAccess,
    ) -> Result<BucketRecord, CatalogError> {
        self.update_bucket(
            name,
            update,
            if_metageneration_match,
            if_metageneration_not_match,
            access,
            BucketWrite::Patch,
        )
        .await
    }

    pub(crate) async fn replace_bucket(
        &self,
        name: &str,
        update: BucketMetadataUpdate,
        if_metageneration_match: Option<i64>,
        if_metageneration_not_match: Option<i64>,
        access: &BucketAccess,
    ) -> Result<BucketRecord, CatalogError> {
        self.update_bucket(
            name,
            update,
            if_metageneration_match,
            if_metageneration_not_match,
            access,
            BucketWrite::Replace,
        )
        .await
    }

    async fn update_bucket(
        &self,
        name: &str,
        update: BucketMetadataUpdate,
        if_metageneration_match: Option<i64>,
        if_metageneration_not_match: Option<i64>,
        access: &BucketAccess,
        mode: BucketWrite,
    ) -> Result<BucketRecord, CatalogError> {
        let name = name.to_owned();
        let access = access.clone();
        let clock = self.0.clock.clone();
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            let mut record = select_bucket(&transaction, &name)
                .map_err(unavailable)?
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
            if if_metageneration_match.is_some_and(|value| value != record.metageneration)
                || if_metageneration_not_match == Some(record.metageneration)
            {
                return Err(CatalogError::PreconditionFailed);
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
            apply_retention_policy(&mut record, &update, if_metageneration_match, clock.now())?;
            bucket_autoclass::update(&transaction, &mut record, &update, clock.now())?;
            if let Some(labels) = update.labels {
                apply_labels(&mut record, labels, mode)?;
            }
            if record.labels.len() > 64 {
                return Err(CatalogError::InvalidMetadata(
                    "A bucket can have at most 64 labels".into(),
                ));
            }
            if let Some(class) = update.storage_class {
                validate_storage_class(&class)?;
                record.storage_class = class;
            }
            if let Some(enabled) = update.versioning_enabled {
                record.versioning_enabled = enabled;
            }
            if let Some(hold) = update.default_event_based_hold {
                record.default_event_based_hold = hold;
            }
            if let Some(enforced) = update.public_access_prevention_enforced {
                record.public_access_prevention_enforced = enforced;
            }
            if let Some(cors) = update.cors {
                validate_cors(&cors)?;
                record.cors = cors;
            }
            if let Some(lifecycle) = update.lifecycle {
                crate::bucket::lifecycle::validate(&lifecycle)?;
                crate::bucket::lifecycle::validate_location(&lifecycle, &record.location_type)?;
                record.lifecycle = lifecycle;
            }
            record.metageneration = record
                .metageneration
                .checked_add(1)
                .ok_or_else(|| unavailable("bucket metageneration exhausted"))?;
            let updated = unix_micros(clock.now())?;
            record.updated = system_time_micros(updated).map_err(unavailable)?;
            record.uniform_bucket_level_access_locked_time = record.uniform_bucket_level_access_locked_time
                .map(|time| unix_micros(time).and_then(|time| system_time_micros(time).map_err(unavailable))).transpose()?;
            transaction
                .prepare_cached(UPDATE_BUCKET)
                .map_err(unavailable)?
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
                    ],
                )
                .map_err(unavailable)?;
            for (target, entries) in [
                (acl::ResolvedAclTarget::Bucket(&name), update.acl),
                (acl::ResolvedAclTarget::DefaultObject(&name), update.default_object_acl),
            ] {
                if let Some(entries) = entries {
                    acl::replace_resource_acl(&transaction, target, &entries, &access)?;
                }
            }
            change_journal::append(&transaction, change_journal::origin(&access), change_journal::ChangeKind::BucketUpdate, &name, None)?;
            transaction.commit().map_err(unavailable)?;
            Ok(record)
        })
        .await
    }

    /// Reads (versioning, default event-based hold) in the caller's mutation transaction.
    pub(crate) fn bucket_settings_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        name: &str,
    ) -> Result<(objects::Versioning, bool), CatalogError> {
        transaction
            .prepare_cached(
                "SELECT versioning_enabled, default_event_based_hold FROM buckets WHERE name = ?1",
            )
            .and_then(|mut statement| {
                statement.query_row([name], |row| {
                    let versioning = if row.get(0)? {
                        objects::Versioning::Enabled
                    } else {
                        objects::Versioning::Disabled
                    };
                    Ok((versioning, row.get(1)?))
                })
            })
            .optional()
            .map_err(unavailable)?
            .ok_or(CatalogError::NotFound)
    }

    /// Reads the bucket and checks `access` against the owner observed by the same read.
    pub(crate) async fn get_bucket_authorized(
        &self,
        name: &str,
        access: &BucketAccess,
    ) -> Result<Option<BucketRecord>, CatalogError> {
        let name = name.to_owned();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            let record = select_bucket(&transaction, &name).map_err(unavailable)?;
            if let Some(record) = &record {
                check_access(&transaction, &name, &record.project, &access)?;
            }
            Ok(record)
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
            bucket_list_page(connection, projects, prefix, start_after, limit)
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn delete_bucket(
        &self,
        name: &str,
        if_metageneration_match: Option<i64>,
        if_metageneration_not_match: Option<i64>,
    ) -> Result<(), CatalogError> {
        self.delete_bucket_authorized(
            name,
            if_metageneration_match,
            if_metageneration_not_match,
            &BucketAccess::Permissive,
        )
        .await
    }

    /// Deletes the bucket only if `access` holds for the owner observed under the write lock.
    pub(crate) async fn delete_bucket_authorized(
        &self,
        name: &str,
        if_metageneration_match: Option<i64>,
        if_metageneration_not_match: Option<i64>,
        access: &BucketAccess,
    ) -> Result<(), CatalogError> {
        let name = name.to_owned();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            let (metageneration, project): (i64, String) = transaction
                .prepare_cached("SELECT metageneration, project FROM buckets WHERE name = ?1")
                .and_then(|mut statement| {
                    statement.query_row([&name], |r| Ok((r.get(0)?, r.get(1)?)))
                })
                .optional()
                .map_err(unavailable)?
                .ok_or(CatalogError::NotFound)?;
            check_access(&transaction, &name, &project, &access)?;
            if if_metageneration_match.is_some_and(|value| value != metageneration)
                || if_metageneration_not_match == Some(metageneration)
            {
                return Err(CatalogError::PreconditionFailed);
            }
            let nonempty: bool = transaction
                .prepare_cached(
                    "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND soft_delete_time IS NULL)",
                )
                .and_then(|mut statement| statement.query_row([&name], |r| r.get(0)))
                .map_err(unavailable)?;
            if nonempty {
                return Err(CatalogError::NotEmpty);
            }
            let execute = |sql: &'static str| {
                transaction
                    .prepare_cached(sql)
                    .and_then(|mut statement| statement.execute([&name]))
                    .map_err(unavailable)
            };
            execute(
                "INSERT OR IGNORE INTO pending_blob_deletions SELECT blob_id FROM objects WHERE bucket = ?1",
            )?;
            execute("DELETE FROM objects WHERE bucket = ?1")?;
            change_journal::append(
                &transaction,
                change_journal::origin(&access),
                change_journal::ChangeKind::BucketDelete,
                &name,
                None,
            )?;
            execute("DELETE FROM bucket_policies WHERE bucket = ?1")?;
            execute("DELETE FROM bucket_incarnations WHERE bucket = ?1")?;
            execute("DELETE FROM sync_objects WHERE bucket = ?1")?;
            execute("DELETE FROM buckets WHERE name = ?1")?;
            transaction.commit().map_err(unavailable)
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
                return Err(CatalogError::InvalidMetadata("Uniform bucket-level access has been enabled for 90 consecutive days and cannot be disabled; keep uniformBucketLevelAccess.enabled=true".into()));
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
        return Err(CatalogError::InvalidMetadata("softDeletePolicy.retentionDurationSeconds must be 0 or 604800 through 7776000; use 604800".into()));
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
            "retentionPeriod must be greater than 0 and less than 3155760000; use 3600".into(),
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

/// Applies a label update; `None` clears every label and a replace starts from none.
fn apply_labels(
    record: &mut BucketRecord,
    labels: Option<std::collections::BTreeMap<String, Option<String>>>,
    mode: BucketWrite,
) -> Result<(), CatalogError> {
    if mode == BucketWrite::Replace {
        record.labels.clear();
    }
    let Some(labels) = labels else {
        record.labels.clear();
        return Ok(());
    };
    for (key, value) in labels {
        // Library callers can bypass adapter validation.
        if (mode == BucketWrite::Replace && value.is_none())
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
    projects: Option<Vec<String>>,
    prefix: String,
    start_after: Option<String>,
    limit: usize,
) -> Result<BucketListPage, CatalogError> {
    use rusqlite::types::Value;
    let mut upper = prefix.as_bytes().to_vec();
    let upper = upper
        .iter()
        .rposition(|byte| *byte != u8::MAX)
        .map(|index| {
            upper[index] += 1;
            upper.truncate(index + 1);
            upper
        });
    // Both bounds remain indexable, including when a page starts beyond the prefix.
    let (lower, comparison) = match start_after {
        Some(start) if start >= prefix => (start, ">"),
        _ => (prefix, ">="),
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
        (false, ">", false) => page!("", ">", ""),
        (false, ">", true) => page!("", ">", "AND name < CAST(? AS TEXT)"),
        (false, _, false) => page!("", ">=", ""),
        (false, _, true) => page!("", ">=", "AND name < CAST(? AS TEXT)"),
        (true, ">", false) => page!(projects!(), ">", ""),
        (true, ">", true) => page!(projects!(), ">", "AND name < CAST(? AS TEXT)"),
        (true, _, false) => page!(projects!(), ">=", ""),
        (true, _, true) => page!(projects!(), ">=", "AND name < CAST(? AS TEXT)"),
    };
    let mut values = Vec::new();
    if let Some(projects) = projects {
        values.push(Value::Text(
            serde_json::to_string(&projects).map_err(unavailable)?,
        ));
    }
    values.push(Value::Text(lower));
    if let Some(upper) = upper {
        values.push(Value::Blob(upper));
    }
    values.push(Value::Integer(limit as i64 + 1));
    let mut statement = connection.prepare_cached(sql).map_err(unavailable)?;
    let mut rows = statement
        .query(rusqlite::params_from_iter(values))
        .map_err(unavailable)?;
    let mut records = Vec::new();
    let mut remaining = BUCKET_LIST_METADATA_BUDGET;
    let mut has_more = false;
    while let Some(row) = rows.next().map_err(unavailable)? {
        // Charge typed rules, JSON values and serialization before allocating any of them.
        let charge = bucket_metadata_charge(row)?;
        if records.len() == limit || (!records.is_empty() && charge > remaining) {
            has_more = !records.is_empty();
            break;
        }
        records.push(read_bucket(row).map_err(unavailable)?);
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
        let bytes = row
            .get_ref(column)
            .map_err(unavailable)?
            .as_str()
            .map_err(unavailable)?
            .len();
        charge = charge.saturating_add(bytes.saturating_mul(multiplier));
    }
    Ok(charge)
}

fn insert_bucket_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    record: &mut BucketRecord,
) -> Result<(), CatalogError> {
    crate::bucket::lifecycle::validate(&record.lifecycle)?;
    crate::bucket::lifecycle::validate_location(&record.lifecycle, &record.location_type)?;
    let created = unix_micros(record.time_created)?;
    let updated = unix_micros(record.updated)?;
    record.time_created = system_time_micros(created).map_err(unavailable)?;
    record.updated = system_time_micros(updated).map_err(unavailable)?;
    record.uniform_bucket_level_access_locked_time = record
        .uniform_bucket_level_access_locked_time
        .map(|time| {
            unix_micros(time).and_then(|time| system_time_micros(time).map_err(unavailable))
        })
        .transpose()?;
    let labels = serde_json::to_string(&record.labels).map_err(unavailable)?;
    let changed = transaction
        .prepare_cached(INSERT_BUCKET)
        .map_err(unavailable)?
        .execute(params![
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
        ])
        .map_err(unavailable)?;
    if changed == 0 {
        return Err(CatalogError::AlreadyExists);
    }
    bucket_autoclass::insert(transaction, record)?;
    insert_policy(
        transaction,
        &record.name,
        &Policy::default_for(&record.project),
    )?;
    transaction
        .prepare_cached("INSERT INTO bucket_incarnations VALUES (?1, lower(hex(randomblob(16))))")
        .and_then(|mut statement| statement.execute([&record.name]))
        .map_err(unavailable)?;
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
        rusqlite::Error::FromSqlConversionFailure(
            0,
            Type::Integer,
            "timestamp outside SystemTime range".into(),
        )
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
            .map(|v| {
                serde_json::from_str(&v).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(16, Type::Text, Box::new(e))
                })
            })
            .transpose()?,
        retention_policy: row
            .get::<_, Option<String>>(15)?
            .map(|v| {
                serde_json::from_str(&v).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(15, Type::Text, Box::new(e))
                })
            })
            .transpose()?,
        name: row.get(0)?,
        project: row.get(1)?,
        location: row.get(2)?,
        location_type: row.get(3)?,
        storage_class: row.get(4)?,
        autoclass: bucket_autoclass::read(row)?,
        labels: serde_json::from_str(&labels)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(5, Type::Text, Box::new(e)))?,
        lifecycle: serde_json::from_str(&row.get::<_, String>(14)?)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(14, Type::Text, Box::new(e)))?,
        metageneration: row.get(6)?,
        time_created: system_time_micros(row.get(7)?)?,
        updated: system_time_micros(row.get(8)?)?,
        object_retention_enabled: row.get(17)?,
        versioning_enabled: row.get(9)?,
        default_event_based_hold: row.get(10)?,
        public_access_prevention_enforced: row.get(12)?,
        uniform_bucket_level_access_locked_time: row
            .get::<_, Option<i64>>(13)?
            .map(system_time_micros)
            .transpose()?,
        cors: cors_from_value(
            &serde_json::from_str(&row.get::<_, String>(11)?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(11, Type::Text, Box::new(e))
            })?,
        )
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(11, Type::Text, Box::new(e)))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let directory =
            std::env::temp_dir().join(format!("gcs-mimic-policy-{mode:?}-{}", std::process::id()));
        let catalog = Catalog::open(&directory).await.unwrap();
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
                &BucketAccess::Permissive,
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
                &BucketAccess::Permissive,
            )
            .await
            .unwrap();
        let update = BucketMetadataUpdate {
            labels: Some(None),
            ..Default::default()
        };
        assert_eq!(
            catalog
                .update_bucket(
                    "patch-policy",
                    update.clone(),
                    Some(99),
                    None,
                    &access,
                    mode
                )
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog.get_bucket("patch-policy").await.unwrap(),
            Some(record.clone())
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
                .update_bucket(
                    "patch-policy",
                    invalid.clone(),
                    Some(99),
                    None,
                    &access,
                    mode
                )
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog.get_bucket("patch-policy").await.unwrap(),
            Some(record.clone())
        );
        catalog
            .delete_bucket("patch-policy", None, None)
            .await
            .unwrap();
        let mut replacement = record;
        replacement.project = "other".into();
        catalog.insert_bucket(replacement.clone()).await.unwrap();
        assert_eq!(
            catalog
                .update_bucket("patch-policy", update, None, None, &access, mode)
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog.get_bucket("patch-policy").await.unwrap(),
            Some(replacement.clone())
        );
        assert_eq!(
            catalog
                .update_bucket("patch-policy", invalid, None, None, &access, mode)
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog.get_bucket("patch-policy").await.unwrap(),
            Some(replacement)
        );
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bucket_settings_migration_rolls_back_columns_and_version_after_late_failure() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(BUCKET_SCHEMA).unwrap();
        connection.pragma_update(None, "user_version", 7).unwrap();
        // The first added column fits; the second exceeds this connection-local limit.
        // SAFETY: The connection owns the SQLite handle throughout this synchronous call.
        #[allow(unsafe_code)]
        unsafe {
            rusqlite::ffi::sqlite3_limit(
                connection.handle(),
                rusqlite::ffi::SQLITE_LIMIT_COLUMN,
                10,
            );
        }
        let error = migrate_bucket_settings(&mut connection).unwrap_err();
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
        // SAFETY: Restore the limit on the same live connection before retrying the migration.
        #[allow(unsafe_code)]
        unsafe {
            rusqlite::ffi::sqlite3_limit(
                connection.handle(),
                rusqlite::ffi::SQLITE_LIMIT_COLUMN,
                2000,
            );
        }
        migrate_bucket_settings(&mut connection).unwrap();
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
        let dir = std::env::temp_dir().join(format!(
            "gcs-mimic-immutable-{}-日本語 # percent%",
            std::process::id()
        ));
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
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn close_waits_for_cancelled_blocking_work_and_invalidates_clones() {
        use std::{future::Future, task::Poll};

        let dir =
            std::env::temp_dir().join(format!("gcs-mimic-catalog-close-{}", std::process::id()));
        let catalog = Catalog::open(&dir).await.unwrap();
        let worker = catalog.clone();
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, finish) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            worker
                .run(move |_| {
                    started.send(()).unwrap();
                    finish.recv().unwrap();
                    Ok(())
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
                catalog.get_bucket("missing").await,
                Err(CatalogError::Unavailable(_))
            ));
            assert!(Catalog::open(&dir).await.is_err());
            release.send(()).unwrap();
            close.await.unwrap();
        }
        let reopened = Catalog::open(&dir).await.unwrap();
        catalog.close().await.unwrap();
        drop(catalog);
        assert!(Catalog::open(&dir).await.is_err());
        reopened.close().await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Opens a raw transaction that rusqlite will not roll back on unwind, then panics.
    async fn panic_inside_transaction(catalog: &Catalog) -> CatalogError {
        catalog
            .run::<()>(|connection| {
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
        use crate::{clock::SystemClock, server::State};
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir =
            std::env::temp_dir().join(format!("gcs-mimic-catalog-poison-{}", std::process::id()));
        let catalog = Catalog::open(&dir).await.unwrap();
        let state = State {
            ready: AtomicBool::new(true),
            clock: Arc::new(SystemClock),
            catalog: catalog.clone(),
            api_base_url: "http://127.0.0.1:4443".into(),
            default_location: "US".into(),
            auth: Default::default(),
        };
        assert!(state.is_ready());
        assert!(matches!(
            panic_inside_transaction(&catalog).await,
            CatalogError::Unavailable(_)
        ));
        assert!(state.is_ready());
        assert!(!probe_exists(&catalog).await.unwrap());
        assert!(catalog.get_bucket("abc").await.unwrap().is_none());

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
            catalog.get_bucket("abc").await,
            Err(CatalogError::Unavailable(_))
        ));
        catalog.0.fail_rollback.store(false, Ordering::Release);
        assert!(!probe_exists(&catalog).await.unwrap());
        assert!(catalog.is_healthy());
        assert!(!state.is_ready());
        catalog.close().await.unwrap();
        Catalog::open(&dir).await.unwrap().close().await.unwrap();
        drop(state);
        drop(catalog);
        std::fs::remove_dir_all(dir).unwrap();
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
        let dir =
            std::env::temp_dir().join(format!("gcs-mimic-catalog-settings-{}", std::process::id()));
        let catalog = Catalog::open(&dir).await.unwrap();
        let permits = catalog
            .0
            .admission
            .clone()
            .try_acquire_many_owned(MAX_IN_FLIGHT as u32)
            .unwrap();
        assert!(matches!(
            catalog.get_bucket("bucket").await,
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
                        connection
                            .pragma_query_value(None, pragma, |row| row.get::<_, i64>(0))
                            .map_err(unavailable)?,
                        expected,
                        "{pragma}"
                    );
                }
                assert_eq!(
                    connection
                        .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
                        .map_err(unavailable)?,
                    "wal"
                );
                Ok(())
            })
            .await
            .unwrap();
        drop(catalog);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
