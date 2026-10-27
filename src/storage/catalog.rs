use super::lock;
use crate::auth::{Identity, OBJECTS_DELETE, Policy};
use crate::bucket::{
    BucketAccess, BucketMetadataUpdate, BucketRecord, CatalogError, UNIFORM_ACCESS_LOCK_PERIOD,
    cors_from_value, cors_value, valid_label, validate_cors, validate_storage_class,
};
use crate::clock::{Clock, SystemClock};
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
#[path = "timestamp_tests.rs"]
mod timestamp_tests;

#[cfg(test)]
#[path = "test_support.rs"]
pub(super) mod test_support;

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

#[path = "resumable.rs"]
pub(crate) mod resumable;
#[path = "resumable_transfer.rs"]
mod resumable_transfer;
pub use resumable::ResumableError;
#[cfg(test)]
pub(crate) use resumable::ResumableSession;
pub(crate) use resumable::{ResumableOptions, SessionState};
pub(crate) use resumable_transfer::ResumableChunk;

#[path = "resumable_maintenance.rs"]
mod resumable_maintenance;

#[path = "resumable_recovery.rs"]
mod resumable_recovery;

#[path = "compose.rs"]
mod compose;
#[path = "compose_crc32c.rs"]
mod compose_crc32c;
#[path = "rewrite.rs"]
pub(crate) mod rewrite;
#[path = "rewrite_token.rs"]
mod rewrite_token;

#[path = "object_move.rs"]
mod object_move;
#[path = "objects.rs"]
mod objects;
pub use objects::{ObjectListCursor, ObjectListPage, ObjectListQuery, ObjectReader};

#[path = "derived.rs"]
mod derived;
#[cfg(test)]
pub(crate) use derived::DerivedTransfer;
pub(crate) use derived::{
    DerivedDestination, DerivedRequestGuard, DerivedSource, DerivedSourceSpec,
};

#[path = "admin.rs"]
pub mod admin;
#[path = "policies.rs"]
mod policies;
use policies::{check_access, insert_policy};

#[path = "change_journal.rs"]
pub mod change_journal;

#[path = "import_journal.rs"]
pub mod import_journal;

#[path = "sync_state.rs"]
pub mod sync_state;

const MAX_IN_FLIGHT: usize = 32;
const MAX_LIST_LIMIT: usize = 1001;
const BUCKET_LIST_METADATA_BUDGET: usize = 256 * 1024;
const CATALOG_SCHEMA_VERSION: i64 = 25;
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
const COLUMNS: &str = "name, project, location, location_type, storage_class, labels,
                      metageneration, time_created, updated, versioning_enabled,
                      default_event_based_hold, cors, public_access_prevention_enforced,
                      uniform_bucket_level_access_locked_time, lifecycle, retention_policy";
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
pub struct BucketListPage {
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
    for (table, columns) in [
        ("buckets", &["time_created", "updated"][..]),
        ("objects", &["time_created", "updated", "time_deleted"][..]),
    ] {
        let condition = columns.iter().map(|column| format!("({column} IS NOT NULL AND (typeof({column}) != 'integer' OR {column} NOT BETWEEN -9223372036854775 AND 9223372036854775))")).collect::<Vec<_>>().join(" OR ");
        let invalid: bool = transaction
            .query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {condition})"),
                [],
                |row| row.get(0),
            )
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
    if !(0..=supported).contains(&version) {
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

fn validate_schema(connection: &Connection, version: i64) -> Result<(), CatalogError> {
    let expected = Connection::open_in_memory().map_err(unavailable)?;
    expected.execute_batch(BUCKET_SCHEMA).map_err(unavailable)?;
    if version >= 2 {
        expected
            .execute_batch(objects::SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 3 {
        expected
            .execute_batch(objects::GENERATION_SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 4 {
        expected
            .execute_batch(objects::METADATA_SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 5 {
        expected.execute_batch(SEED_SCHEMA).map_err(unavailable)?;
        let valid: bool = connection
            .query_row(
                "SELECT count(*) = 1 AND min(singleton) = 1 FROM startup_seed",
                [],
                |r| r.get(0),
            )
            .map_err(unavailable)?;
        if !valid {
            return Err(unavailable("invalid startup seed state"));
        }
    }
    if version >= 6 {
        expected
            .execute_batch(policies::SCHEMA)
            .map_err(unavailable)?;
        policies::validate(connection)?;
    }
    if version >= 7 {
        expected
            .execute_batch(resumable::SCHEMA)
            .map_err(unavailable)?;
        resumable::validate(connection)?;
    }
    if version >= 8 {
        expected
            .execute_batch(BUCKET_SETTINGS_SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 9 {
        expected
            .execute_batch(objects::VERSIONING_SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 10 {
        expected
            .execute_batch(derived::SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 11 {
        expected
            .execute_batch(rewrite::SCHEMA)
            .map_err(unavailable)?;
        rewrite::validate(connection)?;
    }
    if version >= 12 {
        expected
            .execute_batch(BUCKET_CORS_SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 13 {
        expected
            .execute_batch(BUCKET_PAP_SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 15 {
        expected
            .execute_batch(change_journal::SCHEMA)
            .map_err(unavailable)?;
        change_journal::validate(connection)?;
    }
    if version >= 16 {
        expected
            .execute_batch(import_journal::SCHEMA)
            .map_err(unavailable)?;
        if version < 18 {
            import_journal::validate(connection)?;
        }
    }
    if version >= 17 {
        expected
            .execute_batch(sync_state::SCHEMA)
            .map_err(unavailable)?;
        sync_state::validate(connection)?;
    }
    if version >= 18 {
        expected
            .execute_batch(import_journal::FAILURE_SCHEMA)
            .map_err(unavailable)?;
        import_journal::validate_failure(connection)?;
    }
    if version >= 19 {
        expected
            .execute_batch(sync_state::root::SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 20 {
        expected
            .execute_batch(sync_state::writeback::SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 21 {
        expected
            .execute_batch(sync_state::findings::SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 22 {
        expected
            .execute_batch(OBJECT_HOLDS_CUSTOM_TIME_SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 23 {
        expected
            .execute_batch(BUCKET_UNIFORM_ACCESS_SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 24 {
        expected
            .execute_batch(BUCKET_LIFECYCLE_SCHEMA)
            .map_err(unavailable)?;
    }
    if version >= 25 {
        expected
            .execute_batch(BUCKET_RETENTION_SCHEMA)
            .map_err(unavailable)?;
    }
    let inventory =
        |db: &Connection| -> Result<Vec<(String, String, String, String)>, CatalogError> {
            db.prepare("SELECT type, name, tbl_name, substr(sql, 1, 16385) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name LIMIT 257")
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
}

struct Resources {
    // Drop the connection before releasing directory ownership, including on cancelled calls.
    connection: Connection,
    lock: lock::DirectoryLock,
}

#[derive(Default)]
struct UploadState {
    active: usize,
    closed: bool,
}

pub(crate) struct UploadGuard {
    catalog: Option<Catalog>,
    _admin: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl From<tokio::sync::OwnedSemaphorePermit> for UploadGuard {
    fn from(permit: tokio::sync::OwnedSemaphorePermit) -> Self {
        Self {
            catalog: None,
            _admin: Some(permit),
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
        let catalog = tokio::task::spawn_blocking(move || {
            let lock = lock::acquire(&data_dir)?;
            let path = data_dir.join("catalog.sqlite3");
            // A read-write connection can checkpoint a rejected catalog's hot WAL on close.
            let connection = if path.try_exists().map_err(unavailable)? {
                if path.with_extension("sqlite3-wal").try_exists().map_err(unavailable)? {
                    Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
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
            let mut version: i64 = connection
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .map_err(unavailable)?;
            validate_schema_version(version, CATALOG_SCHEMA_VERSION)?;
            if version == 2 {
                return Err(unavailable("unsupported unreleased catalog schema version 2; deleted generation history cannot be recovered"));
            }
            let check: String = connection
                .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
                .map_err(unavailable)?;
            if check != "ok" {
                return Err(CatalogError::Unavailable(format!(
                    "catalog quick_check failed: {check}"
                )));
            }
            if version == 0 {
                let tables: i64 = connection
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
            drop(connection);
            let mut connection = Connection::open(path).map_err(unavailable)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(unavailable)?;
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
            if version == 0 {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction
                    .execute_batch(BUCKET_SCHEMA)
                    .map_err(unavailable)?;
                // Freshness and the retryable seed state must survive the same commit.
                for schema in [objects::SCHEMA, objects::GENERATION_SCHEMA, objects::METADATA_SCHEMA, SEED_SCHEMA] {
                    transaction.execute_batch(schema).map_err(unavailable)?;
                }
                transaction.execute("INSERT INTO object_generation VALUES (1, 0)", []).map_err(unavailable)?;
                transaction.execute("INSERT INTO startup_seed VALUES (1, 0)", []).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
                version = 5;
            }
            // A version marker alone must not make a missing or incompatible schema ready.
            connection
                .prepare("SELECT name, project, location, location_type, storage_class, labels, metageneration, time_created, updated FROM buckets LIMIT 0")
                .map_err(unavailable)?;
            if version < 2 {
                objects::migrate(&mut connection)?;
            }
            if version < 4 {
                objects::migrate_metadata(&mut connection)?;
            }
            if version < 5 {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction.execute_batch(SEED_SCHEMA).map_err(unavailable)?;
                // Older catalogs have already served user mutations, even if now empty.
                transaction.execute("INSERT INTO startup_seed VALUES (1, 1)", []).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
            }
            if version < 6 {
                policies::migrate(&mut connection)?;
            }
            if version < 7 {
                resumable::migrate(&mut connection)?;
            }
            if version < 8 {
                migrate_bucket_settings(&mut connection)?;
            }
            if version < 9 {
                objects::migrate_versioning(&mut connection)?;
            }
            if version < 10 {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction.execute_batch(derived::SCHEMA).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
            }
            if version < 11 {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction.execute_batch(rewrite::SCHEMA).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
            }
            if version < 12 {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction.execute_batch(BUCKET_CORS_SCHEMA).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
            }
            if version < 13 {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction.execute_batch(BUCKET_PAP_SCHEMA).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
            }
            if version < TIMESTAMP_MICROS_SCHEMA_VERSION {
                migrate_timestamp_precision(&mut connection)?;
            }
            if version < 15 {
                change_journal::migrate(&mut connection)?;
            }
            if version < 16 {
                import_journal::migrate(&mut connection)?;
            }
            if version < 17 {
                sync_state::migrate(&mut connection)?;
            }
            if version < 18 {
                import_journal::migrate_failure(&mut connection)?;
            }
            if version < 19 {
                sync_state::root::migrate(&mut connection)?;
            }
            if version < 20 {
                sync_state::writeback::migrate(&mut connection)?;
            }
            if version < 21 {
                sync_state::findings::migrate(&mut connection)?;
            }
            if version < 22 {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction.execute_batch(OBJECT_HOLDS_CUSTOM_TIME_SCHEMA).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
            }
            if version < 23 {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction.execute_batch(BUCKET_UNIFORM_ACCESS_SCHEMA).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
            }
            if version < 24 {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction.execute_batch(BUCKET_LIFECYCLE_SCHEMA).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
            }
            if version < 25 {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction.execute_batch(BUCKET_RETENTION_SCHEMA).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
            }
            let pending: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sync_writeback_ops WHERE phase!=4)", [], |row| row.get(0)).map_err(unavailable)?;
            if pending {
                connection.execute("UPDATE sync_writeback_roots SET recovery=1 WHERE EXISTS(SELECT 1 FROM sync_writeback_ops p WHERE p.root_id=sync_writeback_roots.root_id AND p.phase!=4)",[]).map_err(unavailable)?;
            }
            let journal_enabled: bool = connection.query_row("SELECT enabled FROM change_journal_state WHERE singleton = 1", [], |row| row.get(0)).map_err(unavailable)?;
            if journal_enabled {
                // shortcut: restart requires a full rescan, add durable consumer checkpoints when restart continuity is needed.
                connection.execute("UPDATE change_journal_state SET enabled = 0, epoch = epoch + 1 WHERE singleton = 1", []).map_err(unavailable)?;
            }
            connection.execute_batch("CREATE TEMP TABLE parent_public_access_prevention (project TEXT PRIMARY KEY) WITHOUT ROWID;").map_err(unavailable)?;
            let recovery_now = unix_millis(clock.now())?;
            rewrite::recover_due(&mut connection, recovery_now)?;
            resumable_maintenance::recover_due(&connection, &data_dir, recovery_now)?;
            objects::recover(&connection, &data_dir, recovery_now)?;
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
            })))
        })
        .await
        .map_err(unavailable)??;
        Ok(catalog)
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
            let result = match inner.resources.lock() {
                Ok(mut resources) => match resources.as_mut() {
                    Some(resources) => change_journal::notify_on_write(
                        &mut resources.connection,
                        &inner.change_wake,
                        operation,
                    ),
                    None => Err(CatalogError::Unavailable("catalog is closed".into())),
                },
                Err(error) => Err(unavailable(error.to_string())),
            };
            // Release ownership before Tokio publishes completion to an immediate reopen.
            drop(inner);
            result
        })
        .await
        .map_err(unavailable)?
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

    pub async fn patch_bucket(
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
            false,
        )
        .await
    }

    pub async fn replace_bucket(
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
            true,
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
        replace: bool,
    ) -> Result<BucketRecord, CatalogError> {
        let name = name.to_owned();
        let access = access.clone();
        let clock = self.0.clock.clone();
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            let mut record = transaction
                .query_row(
                    &format!("SELECT {COLUMNS} FROM buckets WHERE name = ?1"),
                    [&name],
                    read_bucket,
                )
                .optional()
                .map_err(unavailable)?
                .ok_or(CatalogError::NotFound)?;
            check_access(&transaction, &name, &record.project, &access)?;
            let pap = update.public_access_prevention_enforced.unwrap_or(!replace && record.public_access_prevention_enforced);
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
            if let Some(enabled) = update.uniform_bucket_level_access_enabled {
                match (enabled, record.uniform_bucket_level_access_locked_time) {
                    (true, None) => {
                        record.uniform_bucket_level_access_locked_time = Some(clock.now()
                            .checked_add(UNIFORM_ACCESS_LOCK_PERIOD)
                            .ok_or_else(|| CatalogError::InvalidMetadata("Uniform access deadline is outside the supported time range".into()))?);
                    }
                    (false, Some(locked_time)) => {
                        if clock.now() >= locked_time {
                            return Err(CatalogError::InvalidMetadata("Uniform bucket-level access has been enabled for 90 consecutive days and cannot be disabled; keep uniformBucketLevelAccess.enabled=true".into()));
                        }
                        record.uniform_bucket_level_access_locked_time = None;
                    }
                    _ => {}
                }
            }
            if update.lock_retention_policy {
                if if_metageneration_match.is_none() {
                    return Err(CatalogError::InvalidMetadata("Locking requires ifMetagenerationMatch; read the bucket metageneration first".into()));
                }
                let policy = record.retention_policy.as_mut().ok_or(CatalogError::RetentionPolicyNotSet)?;
                if policy.is_locked {
                    return Err(CatalogError::InvalidMetadata("Retention policy is already locked".into()));
                }
                policy.is_locked = true;
            } else if let Some(period) = update.retention_period.or(if replace { Some(None) } else { None }) {
                if let Some(policy) = record.retention_policy.as_ref().filter(|p| p.is_locked && period.is_none_or(|period| period < p.retention_period)) {
                    return Err(CatalogError::RetentionPolicyLocked(policy.retention_period));
                }
                if let Some(period) = period {
                    if !(1..3_155_760_000).contains(&period) {
                        return Err(CatalogError::InvalidMetadata("retentionPeriod must be greater than 0 and less than 3155760000; use 3600".into()));
                    }
                    let locked = record.retention_policy.as_ref().is_some_and(|p| p.is_locked);
                    if record.retention_policy.as_ref().is_none_or(|p| p.retention_period != period) {
                        record.retention_policy = Some(crate::bucket::RetentionPolicy { retention_period: period, effective_time: clock.now(), is_locked: locked });
                    }
                } else {
                    record.retention_policy = None;
                }
            }
            if replace {
                record.cors.clear();
                record.lifecycle.clear();
                record.labels.clear();
                record.storage_class = "STANDARD".into();
                record.versioning_enabled = false;
                record.default_event_based_hold = false;
                record.public_access_prevention_enforced = false;
            }
            if let Some(labels) = update.labels {
                if let Some(labels) = labels {
                    for (key, value) in labels {
                        // Library callers can bypass adapter validation.
                        if (replace && value.is_none())
                            || !valid_label(&key, true)
                            || value
                                .as_deref()
                                .is_some_and(|value| !valid_label(value, false))
                        {
                            return Err(CatalogError::InvalidMetadata(
                                "Invalid bucket label".into(),
                            ));
                        }
                        if let Some(value) = value {
                            record.labels.insert(key, value);
                        } else {
                            record.labels.remove(&key);
                        }
                    }
                } else {
                    record.labels.clear();
                }
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
                .execute(
                    "UPDATE buckets SET labels = ?2, storage_class = ?3,
                     versioning_enabled = ?4, default_event_based_hold = ?5,
                     metageneration = ?6, updated = ?7, cors = ?8, public_access_prevention_enforced = ?9,
                     uniform_bucket_level_access_locked_time = ?10, lifecycle = ?11, retention_policy = ?12 WHERE name = ?1",
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
                    ],
                )
                .map_err(unavailable)?;
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
    ) -> Result<(bool, bool), CatalogError> {
        transaction
            .query_row(
                "SELECT versioning_enabled, default_event_based_hold FROM buckets WHERE name = ?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
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
            let record = transaction
                .query_row(
                    &format!("SELECT {COLUMNS} FROM buckets WHERE name = ?1"),
                    [&name],
                    read_bucket,
                )
                .optional()
                .map_err(unavailable)?;
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
        if limit > MAX_LIST_LIMIT {
            return Err(CatalogError::Unavailable(format!(
                "list limit exceeds {MAX_LIST_LIMIT}"
            )));
        }
        let project = project.to_owned();
        let prefix = prefix.unwrap_or("").to_owned();
        let start_after = start_after.map(str::to_owned);
        self.run(move |connection| {
            bucket_list_page(connection, project, prefix, start_after, limit)
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
                .query_row(
                    "SELECT metageneration, project FROM buckets WHERE name = ?1",
                    [&name],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
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
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1)",
                    [&name],
                    |r| r.get(0),
                )
                .map_err(unavailable)?;
            if nonempty {
                return Err(CatalogError::NotEmpty);
            }
            change_journal::append(
                &transaction,
                change_journal::origin(&access),
                change_journal::ChangeKind::BucketDelete,
                &name,
                None,
            )?;
            transaction
                .execute("DELETE FROM bucket_policies WHERE bucket = ?1", [&name])
                .map_err(unavailable)?;
            transaction
                .execute("DELETE FROM bucket_incarnations WHERE bucket = ?1", [&name])
                .map_err(unavailable)?;
            transaction
                .execute("DELETE FROM sync_objects WHERE bucket = ?1", [&name])
                .map_err(unavailable)?;
            transaction
                .execute("DELETE FROM buckets WHERE name = ?1", [name])
                .map_err(unavailable)?;
            transaction.commit().map_err(unavailable)
        })
        .await
    }
}

fn bucket_list_page(
    connection: &Connection,
    project: String,
    prefix: String,
    start_after: Option<String>,
    limit: usize,
) -> Result<BucketListPage, CatalogError> {
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
    let upper_clause = if upper.is_some() {
        "AND name < CAST(?4 AS TEXT)"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {COLUMNS} FROM buckets
        WHERE project = ?1 AND name {comparison} ?2 {upper_clause} ORDER BY name LIMIT ?3"
    );
    let mut statement = connection.prepare(&sql).map_err(unavailable)?;
    let mut rows = match upper {
        Some(upper) => statement.query(params![project, lower, limit as i64 + 1, upper]),
        None => statement.query(params![project, lower, limit as i64 + 1]),
    }
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
        .execute(
            &format!("INSERT INTO buckets ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(name) DO NOTHING"),
            params![
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
                record.uniform_bucket_level_access_locked_time.map(unix_micros).transpose()?,
                serde_json::to_string(&record.lifecycle).map_err(unavailable)?,
                record.retention_policy.as_ref().map(serde_json::to_string).transpose().map_err(unavailable)?,
            ],
        )
        .map_err(unavailable)?;
    if changed == 0 {
        return Err(CatalogError::AlreadyExists);
    }
    insert_policy(
        transaction,
        &record.name,
        &Policy::default_for(&record.project),
    )?;
    transaction
        .execute(
            "INSERT INTO bucket_incarnations VALUES (?1, lower(hex(randomblob(16))))",
            [&record.name],
        )
        .map_err(unavailable)?;
    Ok(())
}

fn unavailable(error: impl std::fmt::Display + std::any::Any) -> CatalogError {
    if let Some(reason) = admin::corrupt_reason(&error) {
        return CatalogError::Corrupt(reason);
    }
    match admin::io_kind(&error) {
        Some(kind) => CatalogError::Io(kind),
        None => CatalogError::Unavailable(error.to_string()),
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
    let duration = Duration::from_millis(millis.unsigned_abs());
    signed_system_time(duration, millis < 0)
}

fn system_time_micros(micros: i64) -> rusqlite::Result<SystemTime> {
    signed_system_time(Duration::from_micros(micros.unsigned_abs()), micros < 0)
}

fn signed_system_time(duration: Duration, negative: bool) -> rusqlite::Result<SystemTime> {
    let time = if negative {
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

fn read_bucket(row: &Row<'_>) -> rusqlite::Result<BucketRecord> {
    let labels: String = row.get(5)?;
    Ok(BucketRecord {
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
        labels: serde_json::from_str(&labels)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(5, Type::Text, Box::new(e)))?,
        lifecycle: serde_json::from_str(&row.get::<_, String>(14)?)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(14, Type::Text, Box::new(e)))?,
        metageneration: row.get(6)?,
        time_created: system_time_micros(row.get(7)?)?,
        updated: system_time_micros(row.get(8)?)?,
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
        bucket_metadata_rechecks_revocation(false).await;
    }

    #[tokio::test]
    async fn bucket_put_rechecks_revocation_and_bucket_recreation() {
        bucket_metadata_rechecks_revocation(true).await;
    }

    async fn bucket_metadata_rechecks_revocation(replace: bool) {
        use crate::auth::{BUCKETS_UPDATE, Binding};
        let directory =
            std::env::temp_dir().join(format!("gcs-mimic-policy-{replace}-{}", std::process::id()));
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
                    replace
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
                    replace
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
                .update_bucket("patch-policy", update, None, None, &access, replace)
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog.get_bucket("patch-policy").await.unwrap(),
            Some(replacement.clone())
        );
        assert_eq!(
            catalog
                .update_bucket("patch-policy", invalid, None, None, &access, replace)
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

    #[tokio::test]
    async fn poisoned_catalog_permanently_revokes_readiness() {
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
        assert!(
            catalog
                .run::<()>(|_| panic!("injected catalog panic"))
                .await
                .is_err()
        );
        assert!(!state.is_ready());
        assert!(!state.ready.load(Ordering::Acquire));
        assert!(matches!(
            catalog.get_bucket("abc").await,
            Err(CatalogError::Unavailable(_))
        ));
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
