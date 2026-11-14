use gcs_mimic::{
    auth::Auth,
    bucket::CatalogError,
    bucket_init::InitOutcome,
    clock::{Clock, SystemClock},
    config::Config,
    diagnostics::{DiagnosticSource, Diagnostics, combine_shutdown_results as merge_close_result},
    seed::{SeedError, SeedOutcome},
    server::{self, State},
    storage::{Catalog, DataDirState},
    time_limit::TimeLimit,
};
use serde::Serialize;
use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
};

mod authentication;
pub(crate) use authentication::Failure as CatalogAuthenticationFailure;

pub(crate) enum PhaseResult<T> {
    Complete(T),
    Stopped,
}

// Only reviewed request waits may be dropped; their effects retain catalog-owned permits.
struct CancelSafe<F>(F);

async fn race<T>(
    work: CancelSafe<impl Future<Output = T>>,
    shutdown: Pin<&mut impl Future<Output = io::Result<()>>>,
) -> Result<PhaseResult<T>, Diagnostics> {
    tokio::select! {
        biased;
        signal = shutdown => shutdown_result(signal).map(|_| PhaseResult::Stopped),
        result = work.0 => Ok(PhaseResult::Complete(result)),
    }
}

#[derive(Serialize)]
struct ReadyListener {
    listen: SocketAddr,
    scheme: &'static str,
}

#[derive(Serialize)]
struct ReadyDataDirectory {
    data_dir: String,
    data_dir_state: DataDirState,
}

#[derive(Serialize)]
pub(crate) struct ReadyEvent {
    event: &'static str,
    executable: &'static str,
    version: &'static str,
    healthcheck_path: &'static str,
    admin_enabled: bool,
    lifecycle_execution: &'static str,
    message: String,
    listen: Option<SocketAddr>,
    listeners: Vec<ReadyListener>,
    /// Reported apart from `listeners`, whose entries all judge requests without credentials as
    /// anonymous.
    #[serde(skip_serializing_if = "Option::is_none")]
    trusted_listen: Option<SocketAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trusted_principal: Option<String>,
    transport: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    unix_socket: Option<String>,
    #[serde(flatten)]
    data_directory: Option<ReadyDataDirectory>,
    max_connections: usize,
    max_object_bytes: u64,
    backend: &'static str,
    memory_max_bytes: u64,
    memory_catalog_max_bytes: u64,
    worker_threads: usize,
    max_blocking_threads: usize,
    header_read_timeout_ms: TimeLimit,
    body_inactivity_timeout_ms: TimeLimit,
    request_timeout_ms: TimeLimit,
    keep_alive_timeout_ms: TimeLimit,
    object_request_timeout_ms: TimeLimit,
    catalog_wait_timeout_ms: TimeLimit,
    shutdown_timeout_ms: TimeLimit,
    auth_mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    identities: Option<usize>,
    test_signer: &'static str,
    signed_urls: &'static str,
    seed: SeedOutcome,
    buckets: InitOutcome,
    scheme: &'static str,
    tls_handshake_timeout_ms: TimeLimit,
    public_base_url: Option<String>,
    public_virtual_host_enabled: bool,
    cors_fallback: &'static str,
    signed_host_port: &'static str,
    /// Whose signatures `signed_host_port=lenient` covers: `all`, or only `test-signer` by
    /// default, where a registered key still names the port. Absent while it is exact.
    #[serde(skip_serializing_if = "Option::is_none")]
    signed_host_port_signers: Option<&'static str>,
    api_base_url: String,
}

impl ReadyEvent {
    pub(crate) fn new(
        config: &Config,
        auth: &Auth,
        listen: SocketAddr,
        listen_http: Option<SocketAddr>,
        seed: SeedOutcome,
        buckets: InitOutcome,
        api_base_url: String,
    ) -> Self {
        let mut listeners = vec![ReadyListener {
            listen,
            scheme: config.scheme.primary(),
        }];
        if let Some(listen) = listen_http {
            listeners.push(ReadyListener {
                listen,
                scheme: "http",
            });
        }
        Self {
            event: "ready",
            executable: "gcs-mimic",
            version: gcs_mimic::VERSION,
            healthcheck_path: Config::HEALTHCHECK_PATH,
            admin_enabled: config.admin_enabled,
            lifecycle_execution: config.effective_lifecycle_execution().as_str(),
            message: format!("server started at {api_base_url}"),
            listen: Some(listen),
            listeners,
            trusted_listen: None,
            trusted_principal: None,
            transport: "tcp",
            unix_socket: None,
            data_directory: None,
            max_connections: config.max_connections,
            max_object_bytes: config.max_object_bytes,
            backend: config.backend.as_str(),
            memory_max_bytes: config.memory_max_bytes,
            memory_catalog_max_bytes: config.memory_catalog_max_bytes,
            worker_threads: config.worker_threads,
            max_blocking_threads: config.max_blocking_threads,
            header_read_timeout_ms: config.header_read_timeout,
            body_inactivity_timeout_ms: config.body_inactivity_timeout,
            request_timeout_ms: config.request_timeout,
            keep_alive_timeout_ms: config.keep_alive_timeout,
            object_request_timeout_ms: config.object_request_timeout,
            catalog_wait_timeout_ms: config.catalog_wait_timeout,
            shutdown_timeout_ms: config.shutdown_timeout,
            auth_mode: auth.mode().as_str(),
            identities: auth.identity_count(),
            test_signer: auth.test_signer().as_str(),
            signed_urls: auth.signed_urls().as_str(),
            seed,
            buckets,
            scheme: config.scheme.as_str(),
            tls_handshake_timeout_ms: config.tls_handshake_timeout,
            public_base_url: config
                .public_base_url
                .as_ref()
                .map(|origin| origin.as_str().to_owned()),
            public_virtual_host_enabled: config.public_virtual_host_enabled(),
            cors_fallback: config.effective_cors_fallback().fallback().as_str(),
            signed_host_port: auth.host_port_policy().setting().as_str(),
            signed_host_port_signers: auth.host_port_policy().lenient_signers(),
            api_base_url,
        }
    }
}

/// The sockets a server accepts on, all bound before readiness.
struct BoundListeners {
    primary: PrimaryListener,
    http: Option<tokio::net::TcpListener>,
    trusted: Option<tokio::net::TcpListener>,
    grpc: Option<tokio::net::TcpListener>,
}

struct BoundStartup {
    listeners: BoundListeners,
    seed: SeedOutcome,
    buckets: InitOutcome,
    sync_root: Option<gcs_mimic::storage::sync_state::root::RootLease>,
}

enum PrimaryListener {
    Tcp(tokio::net::TcpListener),
    #[cfg(unix)]
    Unix(server::unix::Listener),
}

impl PrimaryListener {
    fn local_addr(&self) -> io::Result<Option<SocketAddr>> {
        match self {
            Self::Tcp(listener) => listener.local_addr().map(Some),
            #[cfg(unix)]
            Self::Unix(_) => Ok(None),
        }
    }
}

async fn bind_primary(config: &Config) -> Result<PrimaryListener, Diagnostics> {
    if let Some(_path) = &config.unix_socket {
        if matches!(
            config.scheme,
            gcs_mimic::config::Scheme::Https | gcs_mimic::config::Scheme::Both
        ) || config.listen_http.is_some()
            || config.grpc_listen.is_some()
            || config.trusted_listen.is_some()
        {
            return Err(Diagnostics::error(
                DiagnosticSource::Listener,
                "unix_socket",
                "Unix transport serves plain HTTP; it requires scheme=auto or http and no TCP HTTP/gRPC listener settings.",
            ));
        }
        #[cfg(unix)]
        return server::unix::Listener::bind(_path).await.map(PrimaryListener::Unix).map_err(|error| {
            Diagnostics::error(DiagnosticSource::Listener, "unix_socket", format!(
                "Cannot bind Unix socket ({:?}); use a new short pathname in an existing private 0700 directory owned by the server user; never reuse an existing entry.", error.kind()))
        });
        #[cfg(not(unix))]
        return Err(Diagnostics::error(
            DiagnosticSource::Listener,
            "unix_socket",
            "Unix sockets are unsupported on this platform; no TCP fallback is used.",
        ));
    }
    bind_configured_listener(config.listen, "listen")
        .await
        .map(PrimaryListener::Tcp)
}

async fn open_catalog(
    config: &Config,
    clock: Arc<dyn Clock>,
    shutdown: std::pin::Pin<&mut impl std::future::Future<Output = io::Result<()>>>,
) -> Result<PhaseResult<Catalog>, Diagnostics> {
    let opening = async {
        let admin = if config.admin_enabled {
            gcs_mimic::storage::admin::LocalAdmin::Enabled
        } else {
            gcs_mimic::storage::admin::LocalAdmin::Disabled
        };
        let opened = match config.backend {
            gcs_mimic::config::Backend::Disk => {
                Catalog::open_for_server(&config.data_dir, clock, admin).await
            }
            gcs_mimic::config::Backend::Memory => {
                Catalog::open_memory_for_server(
                    config.memory_max_bytes,
                    config.memory_catalog_max_bytes,
                    clock,
                    admin,
                )
                .await
            }
        };
        // Applied before seeding, bucket files and listeners create any bucket. The reference
        // emulator profile never gets a default policy.
        if let Ok(catalog) = &opened {
            if let Some((path, DataDirState::Created)) = catalog.data_directory() {
                gcs_mimic::diagnostics::emit(
                    gcs_mimic::diagnostics::LogLevel::Info,
                    "data_dir_created",
                    serde_json::json!({"data_dir":path.to_string_lossy(),"message":format!("created a new empty catalog at {}", path.display())}),
                );
            }
            catalog.set_default_soft_delete_retention(match config.compat_profile {
                gcs_mimic::config::CompatProfile::FakeGcsServer => None,
                gcs_mimic::config::CompatProfile::Native => {
                    config.default_soft_delete_retention_seconds
                }
            });
            catalog.apply_session_limits(config);
            catalog.configure_lifecycle(config);
        }
        opened.map_err(|error| match error {
            CatalogError::NewerSchema { version, supported } => Diagnostics::error(DiagnosticSource::Catalog, "data_dir", format!("catalog schema version {version} was written by a newer gcs-mimic release; this release reads schema {supported} and older. Start a release that supports schema {version}, restore a backup taken before the upgrade, or choose a different data directory; migrated catalogs cannot be downgraded.")),
            _ => Diagnostics::error(DiagnosticSource::Catalog, "data_dir", "catalog unavailable or already in use; use a writable compatible native data directory owned by only one server and inspect its recovery state."),
        })
    };
    tokio::pin!(opening);
    // Dropping this borrowed wait keeps the pinned owner alive for the required recovery drain.
    match race(CancelSafe(opening.as_mut()), shutdown).await {
        Ok(PhaseResult::Complete(result)) => result.map(PhaseResult::Complete),
        interrupted => {
            let catalog = opening.await?;
            finish_startup(&catalog, interrupted.map(|_| ())).await?;
            Ok(PhaseResult::Stopped)
        }
    }
}

async fn configure_catalog_policy(
    catalog: &Catalog,
    projects: &[String],
) -> Result<(), Diagnostics> {
    catalog.configure_public_access_prevention(projects).await.map_err(|_| Diagnostics::error(DiagnosticSource::Catalog, "data_dir", "Cannot configure catalog policy; inspect the native catalog and identity policy before restarting."))
}

async fn prepare_startup(
    catalog: &Catalog,
    config: &Config,
    projects: &[String],
    clock: Arc<dyn Clock>,
    mut shutdown: Pin<&mut impl Future<Output = io::Result<()>>>,
) -> Result<PhaseResult<BoundStartup>, Diagnostics> {
    macro_rules! complete {
        ($work:expr) => {
            match race(CancelSafe($work), shutdown.as_mut()).await? {
                PhaseResult::Complete(result) => result,
                PhaseResult::Stopped => return Ok(PhaseResult::Stopped),
            }
        };
    }
    match complete!(catalog.import_journal().in_progress()) {
        Ok(false) => {}
        Ok(true) => {
            return Err(Diagnostics::error(
                DiagnosticSource::Catalog,
                "data_dir",
                "Import is incomplete; resume the same unchanged source export with the import command before serving this destination. Do not remove the import marker.",
            ));
        }
        Err(_) => {
            return Err(Diagnostics::error(
                DiagnosticSource::Catalog,
                "data_dir",
                "Cannot inspect import state; inspect the native catalog before restarting.",
            ));
        }
    }
    // PAP's submitted SQLite job owns its execution permit until completion.
    complete!(configure_catalog_policy(catalog, projects))?;
    complete!(catalog.start_notifications(config))
        // The generic diagnostic names the error kind; a catalog error has none of its own.
        .map_err(|error| Diagnostics::startup_failure(&io::Error::other(error)))?;
    // Loading has no effects; submitted administrative jobs retain their permits through close.
    let buckets = complete!(gcs_mimic::bucket_init::apply(
        catalog,
        config,
        clock.clone()
    ))?;
    // Seed drops only source reads/request waits; publication and cleanup own upload permits.
    let seed = complete!(async {
        gcs_mimic::seed::import(catalog, config, clock.as_ref())
            .await
            .map_err(|error| {
                Diagnostics::error(DiagnosticSource::Seed, "seed_dir", seed_failure(&error))
            })
    })?;
    // Activation jobs own their catalog/root leases; close drains effects after cancellation.
    let sync_root = complete!(gcs_mimic::sync::preflight(catalog, config)).map_err(|error| {
        Diagnostics::error(DiagnosticSource::Sync, "sync_dir", error.to_string())
    })?;
    // Bind/TIME_WAIT has no catalog effects and owns its socket locally.
    let listeners = complete!(async {
        let primary = bind_primary(config).await?;
        let http = match config.effective_listen_http() {
            Some(address) => Some(bind_configured_listener(address, "listen_http").await?),
            None => None,
        };
        let trusted = match config.trusted_listen {
            Some(address) => Some(bind_configured_listener(address, "trusted_listen").await?),
            None => None,
        };
        let grpc = match config.grpc_listen {
            Some(address) => Some(bind_configured_listener(address, "grpc_listen").await?),
            None => None,
        };
        Ok::<_, Diagnostics>(BoundListeners {
            primary,
            http,
            trusted,
            grpc,
        })
    })?;
    Ok(PhaseResult::Complete(BoundStartup {
        listeners,
        seed,
        buckets,
        sync_root,
    }))
}

async fn close_startup(
    catalog: &Catalog,
    result: Result<(), Diagnostics>,
) -> Result<(), Diagnostics> {
    merge_close_result(result, catalog.close().await.map_err(io::Error::other))
}

async fn finish_startup(
    catalog: &Catalog,
    result: Result<(), Diagnostics>,
) -> Result<(), Diagnostics> {
    close_startup(catalog, result).await?;
    report_stopped();
    Ok(())
}

fn report_stopped() {
    println!("{{\"event\":\"stopped\"}}");
}

fn shutdown_result(signal: io::Result<()>) -> Result<(), Diagnostics> {
    signal.map_err(|_| {
        Diagnostics::error(
            DiagnosticSource::Runtime,
            "signal",
            "Cannot receive shutdown signal; check the runtime environment.",
        )
    })?;
    gcs_mimic::diagnostics::emit(
        gcs_mimic::diagnostics::LogLevel::Info,
        "stopping",
        serde_json::json!({}),
    );
    Ok(())
}

pub(crate) async fn serve(
    config: Config,
    auth: Auth,
    security: gcs_mimic::tls::ListenerSecurity,
    shutdown: impl std::future::Future<Output = io::Result<()>>,
) -> Result<(), crate::CliError> {
    tokio::pin!(shutdown);
    let clock = Arc::new(SystemClock);
    let catalog = match open_catalog(&config, clock.clone(), shutdown.as_mut()).await? {
        PhaseResult::Complete(catalog) => catalog,
        PhaseResult::Stopped => return Ok(()),
    };
    if let Err(cause) = catalog.configure_authentication(&auth).await {
        return Err(authentication::close(&catalog, cause).await.into());
    }
    let BoundStartup {
        listeners,
        seed,
        buckets,
        sync_root,
    } = match prepare_startup(
        &catalog,
        &config,
        auth.public_access_prevention_projects(),
        clock.clone(),
        shutdown.as_mut(),
    )
    .await
    {
        Ok(PhaseResult::Complete(bound)) => bound,
        result => return Ok(finish_startup(&catalog, result.map(|_| ())).await?),
    };
    let ready = ready_event(&config, &auth, &catalog, &listeners, seed, buckets);
    let (ready, ready_api_base_url) = match ready {
        Ok(ready) => ready,
        Err(error) => {
            drop(sync_root);
            return Ok(close_startup(&catalog, Err(error)).await?);
        }
    };
    let sync_task = match sync_root
        .map(|root| gcs_mimic::sync::start(catalog.clone(), root, &config))
        .transpose()
    {
        Ok(task) => task,
        Err(error) => {
            return Ok(close_startup(&catalog, Err(Diagnostics::startup_failure(&error))).await?);
        }
    };
    catalog.start_resumable_maintenance();
    let state = Arc::new(State {
        pools: server::WorkPools::new(&config),
        xml_cors_warned_buckets: Default::default(),
        grpc_read_handles: Default::default(),
        ready: AtomicBool::new(true),
        clock,
        catalog,
        api_base_url: ready_api_base_url,
        default_location: config.default_location.clone(),
        auth,
    });
    // Readiness must follow every fallible startup step, including sync worker creation.
    println!("{ready}");
    let background_drain = async move {
        if let Some(task) = sync_task {
            task.finish().await?;
        }
        Ok(())
    };
    let BoundListeners {
        primary,
        http,
        trusted,
        grpc,
    } = listeners;
    match primary {
        PrimaryListener::Tcp(listener) => {
            let mut public = vec![(listener, security)];
            public.extend(http.map(|listener| (listener, gcs_mimic::tls::ListenerSecurity::Plain)));
            let listeners = server::TcpListeners {
                public,
                trusted,
                grpc,
            };
            server::serve_with_grpc_and_drain(listeners, config, state, shutdown, background_drain)
                .await?;
        }
        #[cfg(unix)]
        PrimaryListener::Unix(listener) => {
            server::serve_with_unix_and_drain(listener, config, state, shutdown, background_drain)
                .await?;
        }
    }
    report_stopped();
    Ok(())
}

fn ready_event(
    config: &Config,
    auth: &Auth,
    catalog: &Catalog,
    listeners: &BoundListeners,
    seed: SeedOutcome,
    buckets: InitOutcome,
) -> Result<(String, String), Diagnostics> {
    listeners
        .primary
        .local_addr()
        .map_err(|_| {
            Diagnostics::error(
                DiagnosticSource::Listener,
                "listen",
                "Cannot inspect listener address; check the local network environment.",
            )
        })
        .and_then(|address| {
            let address_http = listeners.http.as_ref().map(|listener| listener.local_addr()).transpose()
                .map_err(|_| Diagnostics::error(DiagnosticSource::Listener, "listen_http", "Cannot inspect HTTP listener address; check the local network environment."))?;
            let api_base_url = address.map_or_else(|| config.api_base_url.clone().unwrap_or_else(|| "http://localhost".into()),
                |address| config.effective_api_base_url(address));
            let mut event = ReadyEvent::new(
                config,
                auth,
                address.unwrap_or(config.listen),
                address_http,
                seed,
                buckets,
                api_base_url.clone(),
            );
            event.data_directory = catalog.data_directory().map(|(path, data_dir_state)| ReadyDataDirectory {
                data_dir: path.to_string_lossy().into_owned(),
                data_dir_state,
            });
            if let Some(path) = &config.unix_socket {
                event.listen = None;
                event.listeners.clear();
                event.transport = "unix";
                event.unix_socket = Some(path.to_string_lossy().into_owned());
                event.message = "server started on a private Unix socket".into();
            }
            if let Some(listener) = &listeners.grpc {
                event.listeners.push(ReadyListener { listen: listener.local_addr().map_err(|error| Diagnostics::startup_failure(&error))?, scheme: "h2c" });
            }
            if let Some(listener) = &listeners.trusted {
                event.trusted_listen = Some(listener.local_addr().map_err(|_| Diagnostics::error(DiagnosticSource::Listener, "trusted_listen", "Cannot inspect trusted listener address; check the local network environment."))?);
                event.trusted_principal = auth.trusted_principal().map(str::to_owned);
            }
            serde_json::to_string(&event)
            .map(|ready| (ready, api_base_url))
            .map_err(|_| {
                Diagnostics::error(
                    DiagnosticSource::Runtime,
                    "ready",
                    "Cannot encode ready event; inspect startup configuration.",
                )
            })
        })
}

fn seed_failure(error: &SeedError) -> String {
    use gcs_mimic::storage::admin::AdminError;
    let reason = match error {
        SeedError::Import(source) | SeedError::Entry { source, .. } => return seed_failure(source),
        SeedError::Admin(AdminError::Hold) => {
            return "seed import: a changed fixture is protected by a hold or retention; the entire seed was left unapplied. Restart without seed_dir to release the hold through the API, or wait for retention to expire, then retry.".into();
        }
        // These name the configured limit and the setting that changes it.
        SeedError::DepthLimit { .. }
        | SeedError::EntryLimit { .. }
        | SeedError::BucketLimit { .. } => {
            return format!("seed import: {error}.");
        }
        // No entry limit stops a seed before this. Only the storage that ran out has a remedy:
        // a disk backend has no RAM budget to raise, and a memory backend no data directory.
        SeedError::Admin(AdminError::DiskFull(cause)) => {
            use gcs_mimic::storage::MemoryBudget;
            return match cause.memory_budget() {
                None => {
                    "seed import: storage capacity exhausted; free space in the data directory before restarting."
                }
                Some(MemoryBudget::Blobs) => {
                    "seed import: storage capacity exhausted; restart with a larger memory_max_bytes or a smaller seed tree."
                }
                Some(MemoryBudget::Catalog) => {
                    "seed import: storage capacity exhausted; restart with a larger memory_catalog_max_bytes or a smaller seed tree."
                }
            }
            .into();
        }
        // The walk opens absolute paths; without a depth limit nothing else stops at this length.
        SeedError::Io(error) if error.kind() == io::ErrorKind::InvalidFilename => {
            "a fixture path exceeds this platform's path length limit"
        }
        SeedError::BucketName(_) => "Invalid bucket name",
        SeedError::ObjectName(_) => "Invalid object name",
        SeedError::FileLimit => "max_object_bytes",
        SeedError::Overlap => "must not overlap",
        SeedError::UnsupportedFile => "only regular files",
        SeedError::NotDirectory => "not a symlink",
        SeedError::NonUtf8Name => "must be UTF-8",
        SeedError::Admin(AdminError::SeedProjectMismatch) => "belongs to another project",
        _ => "unreadable or invalid fixture input",
    };
    format!(
        "seed import: {reason}; correct seed_dir fixtures and their permissions, names, project, overlap and configured limits before restarting."
    )
}

async fn bind_configured_listener(
    address: SocketAddr,
    key: &str,
) -> Result<tokio::net::TcpListener, Diagnostics> {
    bind_listener(address).await.map_err(|error| {
        let flag = key.replace('_', "-");
        let fix = if key == "grpc_listen" {
            format!("Cannot bind grpc_listen ({:?}); stop the process using that port or choose a free separate address, for example --grpc-listen 127.0.0.1:4444.", error.kind())
        } else if error.kind() == io::ErrorKind::AddrInUse {
            format!("Address already in use; choose a free --{flag} IP:PORT.")
        } else {
            format!("Cannot bind listener; check --{flag} IP:PORT and local network permissions.")
        };
        Diagnostics::error(DiagnosticSource::Listener, key, fix)
    })
}

/// Matches the default `max_connections`, so a burst of new clients at a full or starting server
/// waits instead of being refused. The kernel can still lower it to its own limit.
const LISTEN_BACKLOG: u32 = 1024;

async fn bind_listener(address: SocketAddr) -> io::Result<tokio::net::TcpListener> {
    let socket = if address.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    // Linux socket(7) excludes live listeners; BSD address reuse can steal wildcard traffic.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    socket.set_reuseaddr(true)?;
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    if let Err(error) = socket.bind(address) {
        bind_after_time_wait(&socket, address, error).await?;
    }
    #[cfg(not(all(unix, not(any(target_os = "linux", target_os = "android")))))]
    socket.bind(address)?;
    socket.listen(LISTEN_BACKLOG)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
async fn bind_after_time_wait(
    socket: &tokio::net::TcpSocket,
    address: SocketAddr,
    original: io::Error,
) -> io::Result<()> {
    use std::time::Duration;

    if original.kind() != io::ErrorKind::AddrInUse {
        return Err(original);
    }
    // Wait out TIME_WAIT without weakening exclusive binding or waiting indefinitely.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(65);
    let probe = gcs_mimic::config::loopback_if_unspecified(address);
    let mut logged = false;
    loop {
        let connected = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(250)),
            tokio::net::TcpStream::connect(probe),
        )
        .await;
        if matches!(connected, Ok(Ok(_))) || tokio::time::Instant::now() >= deadline {
            return Err(original);
        }
        if !logged {
            gcs_mimic::diagnostics::emit(
                gcs_mimic::diagnostics::LogLevel::Warn,
                "bind_wait",
                serde_json::json!({"listen":address.to_string(),"timeout_ms":65000,"reason":"address_in_use"}),
            );
            logged = true;
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(250)),
        )
        .await;
        if tokio::time::Instant::now() >= deadline {
            return Err(original);
        }
        match socket.bind(address) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use gcs_mimic::{
        clock::{Clock, SystemClock},
        config::DocumentSource,
        storage::Catalog,
    };
    use std::{
        fs::{self, File},
        sync::{Arc, Mutex, mpsc},
        task::Poll,
        time::{Duration, SystemTime},
    };

    #[test]
    fn seed_diagnostics_follow_typed_causes_through_context() {
        use gcs_mimic::storage::admin::AdminError;
        for (error, message) in [
            (
                SeedError::EntryLimit { limit: 10_000 },
                "seed import: fixture exceeds 10000 entries; raise max_seed_entries or reduce the tree.",
            ),
            (
                SeedError::DepthLimit { limit: 32 },
                "seed import: fixture exceeds 32 directory levels below a bucket; raise max_seed_depth or flatten the tree.",
            ),
            (
                SeedError::BucketLimit { limit: 1000 },
                "seed import: fixture exceeds 1000 buckets; raise max_buckets_per_batch or reduce the tree.",
            ),
        ] {
            let error = SeedError::Import(Box::new(SeedError::Entry {
                label: "fixture".into(),
                source: Box::new(error),
            }));
            assert_eq!(seed_failure(&error), message);
        }
        for (error, reason) in [
            (SeedError::NotDirectory, "not a symlink"),
            (SeedError::Overlap, "must not overlap"),
            (SeedError::NonUtf8Name, "must be UTF-8"),
            (SeedError::UnsupportedFile, "only regular files"),
            (SeedError::FileLimit, "max_object_bytes"),
            (
                SeedError::Admin(AdminError::SeedProjectMismatch),
                "belongs to another project",
            ),
            (
                SeedError::BucketName(AdminError::Invalid("Invalid bucket name".into())),
                "Invalid bucket name",
            ),
            (
                SeedError::WalkStopped,
                "unreadable or invalid fixture input",
            ),
            (
                SeedError::Io(io::ErrorKind::InvalidFilename.into()),
                "a fixture path exceeds this platform's path length limit",
            ),
        ] {
            let error = SeedError::Import(Box::new(SeedError::Entry {
                label: "fixture".into(),
                source: Box::new(error),
            }));
            assert_eq!(
                seed_failure(&error),
                format!(
                    "seed import: {reason}; correct seed_dir fixtures and their permissions, names, project, overlap and configured limits before restarting."
                )
            );
        }
        assert_eq!(
            seed_failure(&SeedError::Admin(AdminError::Hold)),
            "seed import: a changed fixture is protected by a hold or retention; the entire seed was left unapplied. Restart without seed_dir to release the hold through the API, or wait for retention to expire, then retry."
        );
        // A full disk names the data directory only; the memory budgets are covered end to end,
        // since only the library can mark which one ran out.
        let disk_full = seed_failure(&SeedError::Import(Box::new(SeedError::Entry {
            label: "fixture".into(),
            source: Box::new(SeedError::Admin(AdminError::DiskFull(
                io::ErrorKind::StorageFull.into(),
            ))),
        })));
        assert_eq!(
            disk_full,
            "seed import: storage capacity exhausted; free space in the data directory before restarting."
        );
        let error = SeedError::Io(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        let error = SeedError::Import(Box::new(error));
        use std::error::Error;
        assert!(
            matches!(error.source().unwrap().downcast_ref::<SeedError>(), Some(SeedError::Io(cause)) if cause.kind() == io::ErrorKind::PermissionDenied)
        );
        assert!(error.source().unwrap().source().is_none());
    }

    #[test]
    fn seed_io_diagnostics_ignore_words_in_fixture_paths() {
        let error = SeedError::Entry {
            label: "bucket/max_object_bytes".into(),
            source: Box::new(SeedError::Io(io::ErrorKind::PermissionDenied.into())),
        };
        assert_eq!(
            seed_failure(&error),
            "seed import: unreadable or invalid fixture input; correct seed_dir fixtures and their permissions, names, project, overlap and configured limits before restarting."
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn unix_startup_does_not_bind_the_configured_default_tcp_address() {
        use std::os::unix::fs::PermissionsExt;
        let directory = TestDir::new();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let catalog = Catalog::open(&directory.0).await.unwrap();
        let mut config = directory.config();
        config.listen = occupied.local_addr().unwrap();
        config.scheme = gcs_mimic::config::Scheme::Http;
        config.unix_socket = Some(directory.0.join("s"));
        let shutdown = std::future::pending();
        tokio::pin!(shutdown);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            prepare_startup(
                &catalog,
                &config,
                &[],
                Arc::new(SystemClock),
                shutdown.as_mut(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let PhaseResult::Complete(bound) = result else {
            panic!("unexpected shutdown")
        };
        let listeners = bound.listeners;
        assert!(listeners.primary.local_addr().unwrap().is_none());
        assert!(listeners.http.is_none() && listeners.grpc.is_none());
        assert!(listeners.trusted.is_none());
        let PrimaryListener::Unix(listener) = listeners.primary else {
            panic!("TCP fallback")
        };
        listener.close().await.unwrap();
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_listener_disabled_by_default_and_bound_after_initialization() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let mut config = dir.config();
        let shutdown = std::future::pending();
        tokio::pin!(shutdown);
        let PhaseResult::Complete(bound) = prepare_startup(
            &catalog,
            &config,
            &[],
            Arc::new(SystemClock),
            shutdown.as_mut(),
        )
        .await
        .unwrap() else {
            panic!("unexpected stop");
        };
        assert!(bound.listeners.grpc.is_none());
        assert!(bound.listeners.trusted.is_none());
        drop(bound);
        config.grpc_listen = Some("127.0.0.1:0".parse().unwrap());
        config.buckets = Some(DocumentSource::File(dir.0.join("missing-buckets.json")));
        let error = prepare_startup(
            &catalog,
            &config,
            &[],
            Arc::new(SystemClock),
            shutdown.as_mut(),
        )
        .await
        .err()
        .unwrap();
        assert!(error.to_string().contains("buckets_file"));
        config.buckets = None;
        let PhaseResult::Complete(bound) = prepare_startup(
            &catalog,
            &config,
            &[],
            Arc::new(SystemClock),
            shutdown.as_mut(),
        )
        .await
        .unwrap() else {
            panic!("unexpected stop");
        };
        let listener = bound.listeners.grpc.as_ref().unwrap();
        let address = listener.local_addr().unwrap();
        let failure = bind_configured_listener(address, "grpc_listen")
            .await
            .unwrap_err();
        assert!(
            failure.to_string().contains("grpc_listen")
                && failure.to_string().contains("127.0.0.1:4444")
        );
        drop(bound);
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn unfinished_import_blocks_startup_before_policy_seed_and_bind() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let binding = gcs_mimic::storage::import_journal::ImportBinding {
            source: [1; 32],
            fingerprint: [2; 32],
            expected_elements: 0,
        };
        catalog
            .import_journal()
            .begin(binding.clone())
            .await
            .unwrap();
        let shutdown = std::future::pending();
        tokio::pin!(shutdown);
        let mut config = dir.config();
        config.seed_dir = Some(dir.0.join("missing-seed"));
        let failure = prepare_startup(
            &catalog,
            &config,
            &vec!["project".into(); 65],
            Arc::new(SystemClock),
            shutdown.as_mut(),
        )
        .await
        .err()
        .unwrap();
        assert!(failure.to_string().contains("Import is incomplete"));
        finish_startup(&catalog, Err(failure)).await.unwrap_err();
        assert!(!catalog.is_healthy());
        let reopened = Catalog::open(&dir.0).await.unwrap();
        assert!(reopened.import_journal().in_progress().await.unwrap());
        reopened.import_journal().finish(binding).await.unwrap();
        // The missing root above proves the import gate runs before seeding; replay now reads
        // the seed root on every start, so the completed startup needs an existing one.
        let seed = TestDir::new();
        config.seed_dir = Some(seed.0.clone());
        let started = prepare_startup(
            &reopened,
            &config,
            &[],
            Arc::new(SystemClock),
            shutdown.as_mut(),
        )
        .await
        .unwrap();
        assert!(matches!(started, PhaseResult::Complete(_)));
        drop(started);
        reopened.close().await.unwrap();
    }

    impl TestDir {
        fn config(&self) -> Config {
            Config {
                data_dir: self.0.clone(),
                listen: "127.0.0.1:0".parse().unwrap(),
                ..Config::default()
            }
        }
    }

    #[tokio::test]
    async fn shutdown_wins_before_starting_the_next_phase() {
        let shutdown = async { Ok(()) };
        tokio::pin!(shutdown);
        assert!(matches!(
            race::<()>(
                CancelSafe(std::future::poll_fn(|_| panic!("phase must not start"))),
                shutdown.as_mut()
            )
            .await,
            Ok(PhaseResult::Stopped)
        ));
    }

    #[tokio::test]
    async fn authentication_setup_failure_keeps_catalog_cause_closes_and_selects_diagnostics() {
        use std::error::Error;

        let directory = TestDir::new();
        let catalog = Catalog::open_for_server(
            &directory.0,
            Arc::new(SystemClock),
            gcs_mimic::storage::admin::LocalAdmin::Disabled,
        )
        .await
        .unwrap();
        let cause = CatalogError::from(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected reconciliation read failure",
        ));
        let failure = authentication::close(&catalog, cause).await;
        let cause = failure
            .source()
            .unwrap()
            .downcast_ref::<CatalogError>()
            .unwrap();
        assert_eq!(
            cause
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(failure.cleanup_cause().is_none());
        assert!(!catalog.is_healthy());
        let cli = crate::CliError::from(failure);
        let diagnostics = cli.diagnostics().unwrap();
        assert_eq!(diagnostics.errors[0].source, DiagnosticSource::Catalog);
        assert_eq!(diagnostics.errors[0].key, "data_dir");
        // A failure without advice of its own falls back to the generic runtime diagnostic.
        assert!(
            crate::CliError::Io(io::Error::other("x"))
                .diagnostics()
                .is_none()
        );
        let reopened = Catalog::open(&directory.0).await.unwrap();
        reopened.close().await.unwrap();
    }

    #[test]
    fn authentication_setup_failure_keeps_primary_and_cleanup_causes_separate() {
        use std::error::Error;

        let failure = CatalogAuthenticationFailure::new(
            CatalogError::AuthenticationAlreadyConfigured,
            Some(CatalogError::from(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected close failure",
            ))),
        );
        assert!(matches!(
            failure.source().unwrap().downcast_ref::<CatalogError>(),
            Some(CatalogError::AuthenticationAlreadyConfigured)
        ));
        assert_eq!(failure.diagnostics().errors[0].key, "authentication");
        assert_eq!(
            failure.diagnostics().errors[0].source,
            DiagnosticSource::Runtime
        );
        assert_eq!(failure.diagnostics().errors[1].key, "data_dir");
        assert_eq!(
            failure
                .cleanup_cause()
                .unwrap()
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[tokio::test]
    async fn phase_completion_and_signal_errors_are_preserved() {
        let shutdown = std::future::pending();
        tokio::pin!(shutdown);
        assert!(matches!(
            race(CancelSafe(async { 42 }), shutdown.as_mut()).await,
            Ok(PhaseResult::Complete(42))
        ));
        assert!(matches!(
            race(
                CancelSafe(async { Err::<(), _>(io::Error::other("work failure")) }),
                shutdown.as_mut()
            )
            .await,
            Ok(PhaseResult::Complete(Err(_)))
        ));
        let shutdown = async { Err(io::Error::other("signal failure")) };
        tokio::pin!(shutdown);
        assert!(
            race(
                CancelSafe(std::future::pending::<io::Result<()>>()),
                shutdown.as_mut()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn catalog_open_failure_preserves_diagnostics_without_repolling() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let shutdown = std::future::pending();
        tokio::pin!(shutdown);
        let error = open_catalog(&dir.config(), Arc::new(SystemClock), shutdown.as_mut())
            .await
            .err()
            .expect("live owner must reject open");
        let diagnostics = &error;
        assert_eq!(diagnostics.errors[0].source, DiagnosticSource::Catalog);
        assert_eq!(diagnostics.errors[0].key, "data_dir");
        assert!(catalog.is_healthy());
        catalog.close().await.unwrap();
    }

    struct RecoveryClock {
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl Clock for RecoveryClock {
        fn now(&self) -> SystemTime {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
                let _ = self
                    .release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10));
            }
            SystemTime::now()
        }
    }

    #[tokio::test]
    async fn shutdown_during_recovery_drains_open_before_unlocking() {
        let dir = TestDir::new();
        let config = dir.config();
        let (entered, recovering) = tokio::sync::oneshot::channel();
        let (release, released) = mpsc::channel();
        let clock = Arc::new(RecoveryClock {
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(released),
        });
        let (signal, shutdown) = tokio::sync::oneshot::channel();
        let (observed, stopping) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let shutdown = async {
                shutdown.await.map_err(io::Error::other)?;
                observed.send(()).unwrap();
                Ok(())
            };
            tokio::pin!(shutdown);
            open_catalog(&config, clock, shutdown.as_mut()).await
        });
        // Hang guards only: opening the catalog on a loaded Windows runner disk
        // has exceeded five seconds before recovery began.
        let hang_guard = Duration::from_secs(30);
        tokio::time::timeout(hang_guard, recovering)
            .await
            .unwrap()
            .unwrap();
        signal.send(()).unwrap();
        stopping.await.unwrap();
        assert!(!task.is_finished());
        assert!(Catalog::open(&dir.0).await.is_err());
        release.send(()).unwrap();
        assert!(matches!(
            tokio::time::timeout(hang_guard, task)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            PhaseResult::Stopped
        ));
        let reopened = Catalog::open(&dir.0).await.unwrap();
        reopened.close().await.unwrap();
    }

    #[test]
    fn shutdown_during_policy_configuration_drains_owned_work() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let dir = TestDir::new();
            let mut config = dir.config();
            config.data_dir = dir.0.join("data");
            config.seed_dir = Some(dir.0.join("fixtures"));
            fs::create_dir_all(dir.0.join("fixtures/seed-bucket")).unwrap();
            let catalog = Catalog::open(&config.data_dir).await.unwrap();
            catalog
                .configure_public_access_prevention(&["old-project".into()])
                .await
                .unwrap();
            let (release, released) = mpsc::channel();
            let (entered, blocked) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(10)).unwrap();
            });
            blocked.await.unwrap();
            {
                let projects = ["new-project".into()];
                let (signal, shutdown) = tokio::sync::oneshot::channel();
                let shutdown = async { shutdown.await.map_err(io::Error::other) };
                tokio::pin!(shutdown);
                let work = prepare_startup(
                    &catalog,
                    &config,
                    &projects,
                    Arc::new(SystemClock),
                    shutdown.as_mut(),
                );
                tokio::pin!(work);
                std::future::poll_fn(|cx| {
                    assert!(work.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                })
                .await;
                signal.send(()).unwrap();
                assert!(matches!(
                    tokio::time::timeout(Duration::from_secs(2), work)
                        .await
                        .unwrap(),
                    Ok(PhaseResult::Stopped)
                ));
            }
            let close = close_startup(&catalog, Ok(()));
            tokio::pin!(close);
            std::future::poll_fn(|cx| {
                assert!(close.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            assert!(matches!(
                File::open(config.data_dir.join("catalog.lock"))
                    .unwrap()
                    .try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(5), close)
                .await
                .unwrap()
                .unwrap();
            blocker.await.unwrap();
            let reopened = Catalog::open(&config.data_dir).await.unwrap();
            assert!(
                reopened
                    .admin(gcs_mimic::storage::admin::Origin::Seed)
                    .get_bucket(&gcs_mimic::storage::admin::BucketName::new("seed-bucket").unwrap())
                    .await
                    .unwrap()
                    .is_none()
            );
            reopened.close().await.unwrap();
            let connection =
                rusqlite::Connection::open(config.data_dir.join("catalog.sqlite3")).unwrap();
            assert_eq!(
                connection
                    .query_row("SELECT complete FROM startup_seed", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0,
                "seed must not run after interrupted PAP"
            );
        });
    }

    #[tokio::test]
    async fn shutdown_during_initializer_validation_never_publishes_or_runs_seed() {
        let dir = TestDir::new();
        let mut config = dir.config();
        config.data_dir = dir.0.join("data");
        let buckets = dir.0.join("buckets.json");
        fs::write(
            &buckets,
            r#"{"version":1,"buckets":[{"name":"pending-bucket"}]}"#,
        )
        .unwrap();
        config.buckets = Some(DocumentSource::File(buckets));
        let catalog = Catalog::open(&config.data_dir).await.unwrap();
        let (entered, validating) = tokio::sync::oneshot::channel();
        let (release, released) = mpsc::channel();
        let clock = Arc::new(RecoveryClock {
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(released),
        });
        let (signal, shutdown) = tokio::sync::oneshot::channel();
        let shutdown = async { shutdown.await.map_err(io::Error::other) };
        tokio::pin!(shutdown);
        {
            let work = prepare_startup(&catalog, &config, &[], clock, shutdown.as_mut());
            tokio::pin!(work);
            tokio::select! {
                _ = validating => {},
                _ = &mut work => panic!("startup completed before validation was released"),
            }
            signal.send(()).unwrap();
            assert!(matches!(work.await, Ok(PhaseResult::Stopped)));
        }
        release.send(()).unwrap();
        close_startup(&catalog, Ok(())).await.unwrap();
        let reopened = Catalog::open(&config.data_dir).await.unwrap();
        assert!(
            reopened
                .admin(gcs_mimic::storage::admin::Origin::BucketInit)
                .get_bucket(&gcs_mimic::storage::admin::BucketName::new("pending-bucket").unwrap())
                .await
                .unwrap()
                .is_none()
        );
        reopened.close().await.unwrap();
    }

    #[tokio::test]
    async fn startup_failures_close_catalog_even_with_live_handles() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let retained = catalog.clone();
        let config = dir.config();
        let shutdown = std::future::pending();
        tokio::pin!(shutdown);
        let failure = prepare_startup(
            &catalog,
            &config,
            &["bad project".into()],
            Arc::new(SystemClock),
            shutdown.as_mut(),
        )
        .await
        .err()
        .expect("PAP failure");
        let diagnostics = &failure;
        assert_eq!(diagnostics.errors[0].source, DiagnosticSource::Catalog);
        assert_eq!(diagnostics.errors[0].key, "data_dir");
        let error = finish_startup(&catalog, Err(failure)).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Cannot configure catalog policy")
        );
        assert!(!retained.is_healthy());
        let reopened = Catalog::open(&dir.0).await.unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = dir.config();
        config.listen = listener.local_addr().unwrap();
        let failure = prepare_startup(
            &reopened,
            &config,
            &[],
            Arc::new(SystemClock),
            shutdown.as_mut(),
        )
        .await
        .err()
        .expect("bind failure");
        assert!(failure.to_string().contains("Address already in use"));
        finish_startup(&reopened, Err(failure)).await.unwrap_err();
        assert!(!reopened.is_healthy());
        let latest = Catalog::open(&dir.0).await.unwrap();
        latest.close().await.unwrap();
    }
}
