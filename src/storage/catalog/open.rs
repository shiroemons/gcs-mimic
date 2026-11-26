use super::*;

/// Bounds the parsed statements the catalog's single connection keeps. Request paths prepare
/// about 150 distinct static SQL texts (some list and derived shapes are chosen at run time),
/// so 256 keeps the whole hot set cached with room for new statements, while open-time and
/// maintenance statements that pass through the LRU cannot evict it. A few hundred prepared
/// statements cost on the order of a megabyte, bounded by this one connection.
const STATEMENT_CACHE_CAPACITY: usize = 256;

// Capacities of the admission semaphores that `close` drains: it waits for every permit, so the
// two sides must agree on the count.
/// Concurrent emulator-only admin uploads.
pub(super) const ADMIN_UPLOADS: u32 = 2;
/// Concurrent emulator-only admin reads.
pub(super) const ADMIN_READS: u32 = 32;
/// Staged object handles an admin publication may hold open at once.
pub(super) const STAGED_HANDLES: u32 = 256;
/// Concurrent admin publication requests.
pub(super) const PUBLICATION_REQUESTS: u32 = 2;

/// Where a catalog keeps its data, decided once at open.
pub(super) enum Backing {
    /// A data directory. `resume` is an unfinished import's source digest, verified under the
    /// directory lock before migration and recovery.
    Disk {
        data_dir: PathBuf,
        resume: Option<[u8; 32]>,
    },
    /// A volatile catalog with budgets for blob bytes and catalog database pages. `data_dir` is
    /// only the path the catalog reports: a server passes an empty one, and tests a directory
    /// that keeps their fault injection apart.
    Memory {
        limits: MemoryLimits,
        data_dir: PathBuf,
    },
}

#[derive(Clone, Copy)]
pub(super) struct MemoryLimits {
    pub(super) blob_bytes: u64,
    pub(super) catalog_bytes: u64,
}

#[cfg(test)]
impl Backing {
    /// A small memory catalog whose fault injection is keyed by `data_dir`.
    pub(super) fn test_memory(data_dir: PathBuf) -> Self {
        Self::Memory {
            limits: MemoryLimits {
                blob_bytes: 8192,
                catalog_bytes: 1024 * 1024,
            },
            data_dir,
        }
    }

    pub(super) fn test_disk(data_dir: PathBuf) -> Self {
        Self::Disk {
            data_dir,
            resume: None,
        }
    }
}

/// Opened storage, before the schema is brought up to date.
struct OpenedStorage {
    backend: BlobBackend,
    /// Held for a data directory for as long as the catalog is open.
    lock: Option<lock::DirectoryLock>,
    stored_version: i64,
    connection: Connection,
    data_dir: PathBuf,
}

/// Opens the blob store and the catalog database: for a data directory, under its lock and
/// after inspecting the stored catalog read-only.
fn open_storage(backing: Backing) -> Result<OpenedStorage, CatalogError> {
    match backing {
        Backing::Memory { limits, data_dir } => {
            let backend = BlobBackend::Memory(super::super::memory::Store::new(limits.blob_bytes));
            Ok(OpenedStorage {
                backend,
                lock: None,
                stored_version: 0,
                connection: open_memory(limits.catalog_bytes)?,
                data_dir,
            })
        }
        Backing::Disk { data_dir, resume } => {
            let data_dir = std::path::absolute(data_dir)?;
            let backend = BlobBackend::Disk(data_dir.clone());
            let path = data_dir.join("catalog.sqlite3");
            let (lock, (version, readonly)) = if let Some(source) = resume {
                lock::acquire_import(&data_dir, || {
                    let (version, readonly) = inspect_catalog(&path)?;
                    import_journal::verify_resume(&readonly, version, source)?;
                    Ok((version, readonly))
                })?
            } else {
                let lock = lock::acquire(&data_dir)?;
                (lock, inspect_catalog(&path)?)
            };
            drop(readonly);
            Ok(OpenedStorage {
                backend,
                lock: Some(lock),
                stored_version: version,
                connection: open_writable(&path)?,
                data_dir,
            })
        }
    }
}

#[cfg_attr(
    unix,
    expect(
        clippy::unnecessary_wraps,
        reason = "only a Windows path can fail to encode, and both targets share one signature"
    )
)]
pub(super) fn immutable_catalog_uri(path: &Path) -> Result<String, CatalogError> {
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

/// Reads and validates without writing; retains the connection for pre-migration checks.
#[expect(
    clippy::disallowed_methods,
    reason = "integrity and schema inspection run once at open"
)]
fn inspect_catalog(path: &Path) -> Result<(i64, Connection), CatalogError> {
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
    // Temporary storage stays in memory for the reason `open_writable` gives.
    connection.execute_batch(
        "PRAGMA cache_size = -2048;
                 PRAGMA mmap_size = 0;
                 PRAGMA temp_store = MEMORY;",
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
    Ok((version, connection))
}

/// Opens the catalog for writing in WAL mode with full synchronization.
///
/// Temporary storage stays in memory, as on the readers. Otherwise SQLite moves a savepoint or
/// statement journal past 64 KiB, and a sort or transient index past the page cache, to a file in
/// the first writable directory among `SQLITE_TMPDIR`, `TMPDIR`, `/var/tmp`, `/usr/tmp`, `/tmp`
/// and the working directory. A read-only root offers none of them, and the failure surfaced as
/// `disk I/O error` on uploads whose savepoint journal had grown. In memory, a request or
/// maintenance pass journals and sorts only what its own rows and `LIMIT` batches touch. A
/// statement that rewrites every object of one bucket (a bucket soft delete or restore, enabling
/// Autoclass) journals pages in proportion to that bucket, as do schema migrations and reset.
#[expect(
    clippy::disallowed_methods,
    reason = "switches the journal mode once at open"
)]
fn open_writable(path: &Path) -> Result<Connection, CatalogError> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    // The readers and the RAM catalog's writer already run every catalog statement this way.
    sql::keep_plans_across_bindings(&connection)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
                 PRAGMA cache_size = -2048;
                 PRAGMA mmap_size = 0;
                 PRAGMA temp_store = MEMORY;",
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
    // The commit that takes the WAL past the auto-checkpoint threshold runs a passive checkpoint
    // on the writer before it returns, so one request pays for copying those pages into the
    // database and syncing it. 256 pages, 1 MiB at the default page size, keeps that pause and the
    // WAL file, which SQLite reuses rather than truncates, at a quarter of SQLite's default
    // 1000 pages, at the cost of checkpointing four times as often. A long read on the pool can
    // still hold the WAL past it, as it would at any threshold.
    connection.execute_batch("PRAGMA synchronous = FULL; PRAGMA wal_autocheckpoint = 256;")?;
    Ok(connection)
}

fn open_memory(max_bytes: u64) -> Result<Connection, CatalogError> {
    let connection = Connection::open_in_memory()?;
    connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    sql::keep_plans_across_bindings(&connection)?;
    connection.execute_batch("PRAGMA page_size = 4096; PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY; PRAGMA cache_size = -2048;")?;
    let pages = i64::try_from(max_bytes / 4096).map_err(unavailable)?;
    connection.pragma_update(None, "max_page_count", pages)?;
    connection.pragma_update(Some("temp"), "max_page_count", pages)?;
    Ok(connection)
}

/// Brings the catalog at `stored_version` to the current schema, creating it when there is none,
/// and tells the operator what an upgrade changed or left behind.
fn upgrade_catalog(
    connection: &mut Connection,
    stored_version: i64,
    backend: &BlobBackend,
) -> Result<(), CatalogError> {
    let mut version = stored_version;
    if version == 0 {
        initialize_catalog(connection)?;
        version = 5;
    }
    // A version marker alone must not make a missing or incompatible schema ready.
    connection
        .prepare_cached("SELECT name, project, location, location_type, storage_class, labels, metageneration, time_created, updated FROM buckets LIMIT 0")?;
    let disk_dir = match backend {
        BlobBackend::Disk(root) => Some(root.as_path()),
        BlobBackend::Memory(_) => None,
    };
    upgrade(connection, version, disk_dir)?;
    // Migrations are one-way, so operators need to see which upgrade ended older-release compatibility.
    if (1..CATALOG_SCHEMA_VERSION).contains(&stored_version) {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Info,
            crate::diagnostics::Event::CatalogSchemaMigrated,
            serde_json::json!({"message":format!("migrated catalog schema from version {stored_version} to {CATALOG_SCHEMA_VERSION}; releases that support only schema {stored_version} can no longer open this data directory"),"from":stored_version,"to":CATALOG_SCHEMA_VERSION}),
        );
    }
    if let Some(data_dir) = disk_dir {
        retired_sync::warn_retired_files(data_dir);
    }
    Ok(())
}

/// Resets volatile state and completes crash recovery before the catalog serves requests.
fn recover_on_open(
    connection: &mut Connection,
    backend: &BlobBackend,
    clock: &dyn Clock,
) -> Result<(), CatalogError> {
    let BlobBackend::Disk(data_dir) = backend else {
        return Err(unavailable("disk recovery requires a disk backend"));
    };
    connection.execute_batch("CREATE TEMP TABLE parent_public_access_prevention (project TEXT PRIMARY KEY) WITHOUT ROWID;")?;
    notification_outbox::open(connection)?;
    resumable::open(connection)?;
    let recovery_now = UnixMillis::try_from(clock.now())?;
    rewrite::recover_due(connection, recovery_now)?;
    resumable_maintenance::recover_due(connection, backend, recovery_now)?;
    xml_multipart::recover(connection, data_dir, recovery_now)?;
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
        let options = CatalogOptions {
            principals: Some(principals::Profile::TrustedLocal),
            ..CatalogOptions::embedded(clock, admin::LocalAdmin::Disabled)
        };
        Self::open_with_options(CatalogBacking::Disk(data_dir.as_ref()), options)
            .await
            .map_err(CatalogOpenError::into_cause)
    }

    /// Issues local management authority only while opening the operator-owned catalog.
    pub async fn open_for_server(
        data_dir: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
        local_admin: admin::LocalAdmin,
    ) -> Result<Self, CatalogError> {
        let backing = Backing::Disk {
            data_dir: data_dir.as_ref().to_owned(),
            resume: None,
        };
        Self::open_backend(backing, clock, local_admin).await
    }

    /// Checks the unfinished import binding under the directory lock before migration/recovery.
    pub(crate) async fn resume_import(
        data_dir: &Path,
        source: [u8; 32],
    ) -> Result<Self, CatalogError> {
        Self::open_options(
            Backing::Disk {
                data_dir: data_dir.to_owned(),
                resume: Some(source),
            },
            CatalogOptions {
                principals: Some(principals::Profile::TrustedLocal),
                ..CatalogOptions::embedded(Arc::new(SystemClock), admin::LocalAdmin::Disabled)
            },
        )
        .await
        .map_err(CatalogOpenError::into_cause)
    }

    /// Opens an isolated volatile catalog without reading or creating a data directory.
    pub async fn open_memory_for_server(
        max_bytes: u64,
        catalog_max_bytes: u64,
        clock: Arc<dyn Clock>,
        local_admin: admin::LocalAdmin,
    ) -> Result<Self, CatalogError> {
        Self::open_with_options(
            CatalogBacking::Memory {
                blob_bytes: max_bytes,
                catalog_bytes: catalog_max_bytes,
            },
            CatalogOptions::embedded(clock, local_admin),
        )
        .await
        .map_err(CatalogOpenError::into_cause)
    }

    pub async fn open_with_options(
        backing: CatalogBacking<'_>,
        options: CatalogOptions,
    ) -> Result<Self, CatalogOpenError> {
        let backing = match backing {
            CatalogBacking::Disk(data_dir) => Backing::Disk {
                data_dir: data_dir.to_owned(),
                resume: None,
            },
            CatalogBacking::Memory {
                blob_bytes,
                catalog_bytes,
            } => {
                if blob_bytes == 0 || catalog_bytes < 1024 * 1024 {
                    return Err(CatalogOpenError::Storage(unavailable(
                        "RAM requires a positive blob budget and at least 1 MiB of catalog pages",
                    )));
                }
                Backing::Memory {
                    limits: MemoryLimits {
                        blob_bytes,
                        catalog_bytes,
                    },
                    data_dir: PathBuf::new(),
                }
            }
        };
        Self::open_options(backing, options).await
    }

    pub(super) async fn open_backend(
        backing: Backing,
        clock: Arc<dyn Clock>,
        local_admin: admin::LocalAdmin,
    ) -> Result<Self, CatalogError> {
        Self::open_options(backing, CatalogOptions::embedded(clock, local_admin))
            .await
            .map_err(CatalogOpenError::into_cause)
    }

    async fn open_options(
        backing: Backing,
        options: CatalogOptions,
    ) -> Result<Self, CatalogOpenError> {
        let CatalogOptions {
            clock,
            local_admin,
            principals,
        } = options;
        crate::task::run_blocking("catalog-open", move || -> Result<Self, CatalogOpenError> {
            let OpenedStorage {
                backend,
                lock,
                stored_version,
                mut connection,
                data_dir,
            } = open_storage(backing)?;
            // A version-zero database has no catalog yet, including an interrupted first open.
            let data_dir_state = if stored_version == 0 {
                DataDirState::Created
            } else {
                DataDirState::Opened
            };
            upgrade_catalog(&mut connection, stored_version, &backend)?;
            match backend.kind() {
                crate::config::Backend::Memory => {
                    connection.execute_batch("CREATE TEMP TABLE parent_public_access_prevention (project TEXT PRIMARY KEY) WITHOUT ROWID;").map_err(CatalogError::from)?;
                    notification_outbox::open(&connection)?;
                    resumable::open(&connection)?;
                }
                crate::config::Backend::Disk => recover_on_open(&mut connection, &backend, &*clock)?,
            }
            if let Some(profile @ (principals::Profile::Strict(_) | principals::Profile::Permissive { .. })) = &principals {
                profile.reconcile(&mut connection, &*clock).map_err(CatalogOpenError::Authentication)?;
            }
            let parent_policy = Arc::<read_pool::ParentPolicy>::default();
            let lifecycle = lifecycle::LifecycleSchedule::open(&mut connection)?;
            // An in-memory database exists only inside the writer's connection.
            let read_pool = match backend.kind() {
                crate::config::Backend::Memory => None,
                crate::config::Backend::Disk => Some(read_pool::ReadPool::new(
                    data_dir.join("catalog.sqlite3"),
                    parent_policy.clone(),
                )),
            };
            Ok(Self(Arc::new(Inner {
                policy: state::Policy {
                    principals,
                    clock,
                    session_limits: OnceLock::new(),
                    default_soft_delete_retention: AtomicU64::new(0),
                },
                storage: state::Storage {
                    backend,
                    data_dir,
                    data_dir_state,
                    reclamation: Arc::new(Semaphore::new(1)),
                },
                writer: state::Writer {
                    resources: Mutex::new(Some(Resources { connection, lock })),
                    execution: Arc::new(Semaphore::new(1)),
                    executor: executor::Executor::start("gcs-mimic-catalog").map_err(CatalogError::from)?,
                    upload_commits: objects::CommitQueue::default(),
                    #[cfg(test)]
                    fail_rollback: AtomicBool::new(false),
                },
                reads: state::Reads {
                    pool: read_pool,
                    parent_policy,
                    cache: read_cache::ReadCache::new(crate::fd_limit::cacheable_files()),
                    runtime: tokio::runtime::Handle::current(),
                },
                admission: state::Admission {
                    slots: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
                    activity: ActivityGate::default(),
                    uploads: Mutex::new(UploadState::default()),
                    uploads_idle: Notify::new(),
                    #[cfg(test)]
                    waiters: std::sync::atomic::AtomicUsize::new(0),
                },
                admin: state::AdminState {
                    uploads: Arc::new(Semaphore::new(ADMIN_UPLOADS as usize)),
                    reads: Arc::new(Semaphore::new(ADMIN_READS as usize)),
                    staged_handles: Arc::new(Semaphore::new(STAGED_HANDLES as usize)),
                    publication_batch: Arc::new(Semaphore::new(1)),
                    seed_commit: Arc::new(Semaphore::new(1)),
                    publication_requests: Arc::new(Semaphore::new(PUBLICATION_REQUESTS as usize)),
                    file_workers: admin::FileWorkers::new().map_err(CatalogError::from)?,
                    grant: admin::startup_grant(local_admin),
                    seed_watch: Arc::default(),
                    #[cfg(test)]
                    cleanup_tasks: Default::default(),
                },
                writers: state::WriterRegistry {
                    sessions: Mutex::new(Default::default()),
                    released: Notify::new(),
                },
                maintenance: state::Maintenance {
                    wake: Arc::new(Notify::new()),
                    derived_wake: Notify::new(),
                    lifecycle,
                    multipart_orphan_scan: AtomicBool::new(false),
                    error: Mutex::new(None),
                    started: AtomicBool::new(false),
                    health: crate::task::TaskHealth::default(),
                    #[cfg(test)]
                    panics: std::sync::atomic::AtomicUsize::new(0),
                },
                notifications: state::Notifications {
                    wake: Arc::new(Notify::new()),
                    started: AtomicBool::new(false),
                    endpoint: OnceLock::new(),
                    attempts: Arc::new(Semaphore::new(1)),
                },
                shutdown: state::Shutdown {
                    admin_wake: Notify::new(),
                    background_health: Mutex::new(Vec::new()),
                },
            })))
        })
        .await.map_err(CatalogError::from)?
    }
}
