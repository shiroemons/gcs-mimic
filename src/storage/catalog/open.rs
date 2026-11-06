use super::*;

/// Bounds the parsed statements the catalog's single connection keeps. Request paths prepare
/// about 150 distinct static SQL texts (some list and derived shapes are chosen at run time),
/// so 256 keeps the whole hot set cached with room for new statements, while open-time and
/// maintenance statements that pass through the LRU cannot evict it. A few hundred prepared
/// statements cost on the order of a megabyte, bounded by this one connection.
const STATEMENT_CACHE_CAPACITY: usize = 256;

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
    Ok((version, connection))
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
    resumable::open(connection)?;
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
        Self::open_backend(data_dir, clock, local_admin, None, None).await
    }

    /// Checks the unfinished import binding under the directory lock before migration/recovery.
    pub(crate) async fn resume_import(
        data_dir: &Path,
        source: [u8; 32],
    ) -> Result<Self, CatalogError> {
        let catalog = Self::open_backend(
            data_dir.to_owned(),
            Arc::new(SystemClock),
            admin::LocalAdmin::Disabled,
            None,
            Some(source),
        )
        .await?;
        catalog.set_principal_profile(principals::Profile::TrustedLocal)?;
        Ok(catalog)
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
            None,
        )
        .await
    }

    pub(super) async fn open_backend(
        data_dir: PathBuf,
        clock: Arc<dyn Clock>,
        local_admin: admin::LocalAdmin,
        memory_limits: Option<(u64, u64)>,
        resume_source: Option<[u8; 32]>,
    ) -> Result<Self, CatalogError> {
        crate::task::run_blocking("catalog-open", move || -> Result<Self, CatalogError> {
            let backend = match memory_limits {
                Some((bytes, _)) => BlobBackend::Memory(super::super::memory::Store::new(bytes)),
                None => BlobBackend::Disk(data_dir.clone()),
            };
            let (lock, stored_version, mut connection) = if let Some((_, bytes)) = memory_limits {
                (None, 0, open_memory(bytes)?)
            } else {
                let path = data_dir.join("catalog.sqlite3");
                let (lock, (version, readonly)) = if let Some(source) = resume_source {
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
                (Some(lock), version, open_writable(&path)?)
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
                resumable::open(&connection)?;
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
                writer_released: Notify::new(),
                session_limits: OnceLock::new(),
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
                #[cfg(test)]
                admission_waiters: std::sync::atomic::AtomicUsize::new(0),
            })))
        })
        .await?
    }
}
