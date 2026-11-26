use gcs_mimic::{
    auth::Auth,
    bucket::CatalogError,
    bucket_init::InitOutcome,
    clock::{Clock, SystemClock},
    config::{Config, GrpcScheme},
    diagnostics::{DiagnosticSource, Diagnostics, combine_shutdown_results as merge_close_result},
    seed::{SeedError, SeedOutcome},
    server::{self, State},
    storage::{Catalog, DataDirState, MemoryBudget, admin::AdminError},
    time_limit::TimeLimit,
};
use serde::Serialize;
use std::{
    borrow::Cow,
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
    /// The configured `grpc_scheme`, present only with a gRPC listener.
    #[serde(skip_serializing_if = "Option::is_none")]
    grpc_scheme: Option<&'static str>,
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

/// Where a ready server accepts connections.
enum ReadyTransport {
    /// The primary listener first, then the `scheme=both` HTTP listener, then gRPC.
    Tcp {
        listen: SocketAddr,
        listeners: Vec<ReadyListener>,
    },
    /// The private Unix socket, the only listener.
    #[cfg(unix)]
    Unix { socket: String },
}

impl ReadyTransport {
    fn api_base_url(&self, config: &Config) -> String {
        match self {
            Self::Tcp { listen, .. } => config.effective_api_base_url(*listen),
            #[cfg(unix)]
            Self::Unix { .. } => config
                .api_base_url()
                .unwrap_or("http://localhost")
                .to_owned(),
        }
    }
}

/// What startup produced that readiness reports, beside the configuration and authentication.
struct Readiness {
    transport: ReadyTransport,
    trusted_listen: Option<SocketAddr>,
    data_directory: Option<ReadyDataDirectory>,
    seed: SeedOutcome,
    buckets: InitOutcome,
    api_base_url: String,
}

/// The ready event line and the API base URL it announces.
struct Ready {
    event: String,
    api_base_url: String,
}

impl ReadyEvent {
    fn new(config: &Config, auth: &Auth, readiness: Readiness) -> Self {
        let Readiness {
            transport,
            trusted_listen,
            data_directory,
            seed,
            buckets,
            api_base_url,
        } = readiness;
        let (message, listen, listeners, transport, unix_socket) = match transport {
            ReadyTransport::Tcp { listen, listeners } => (
                format!("server started at {api_base_url}"),
                Some(listen),
                listeners,
                "tcp",
                None,
            ),
            #[cfg(unix)]
            ReadyTransport::Unix { socket } => (
                "server started on a private Unix socket".to_owned(),
                None,
                Vec::new(),
                "unix",
                Some(socket),
            ),
        };
        Self {
            event: "ready",
            executable: "gcs-mimic",
            version: gcs_mimic::VERSION,
            healthcheck_path: Config::HEALTHCHECK_PATH,
            admin_enabled: config.admin_enabled(),
            lifecycle_execution: config.lifecycle_execution().as_str(),
            message,
            listen,
            listeners,
            trusted_listen,
            trusted_principal: trusted_listen
                .and_then(|_| auth.trusted_principal())
                .map(str::to_owned),
            transport,
            unix_socket,
            data_directory,
            max_connections: config.max_connections(),
            max_object_bytes: config.max_object_bytes(),
            backend: config.backend().as_str(),
            memory_max_bytes: config.memory_max_bytes(),
            memory_catalog_max_bytes: config.memory_catalog_max_bytes(),
            worker_threads: config.worker_threads(),
            max_blocking_threads: config.max_blocking_threads(),
            header_read_timeout_ms: config.header_read_timeout(),
            body_inactivity_timeout_ms: config.body_inactivity_timeout(),
            request_timeout_ms: config.request_timeout(),
            keep_alive_timeout_ms: config.keep_alive_timeout(),
            object_request_timeout_ms: config.object_request_timeout(),
            catalog_wait_timeout_ms: config.catalog_wait_timeout(),
            shutdown_timeout_ms: config.shutdown_timeout(),
            auth_mode: auth.mode().as_str(),
            identities: auth.identity_count(),
            test_signer: auth.test_signer().as_str(),
            signed_urls: auth.signed_urls().as_str(),
            seed,
            buckets,
            scheme: config.scheme().as_str(),
            grpc_scheme: config.grpc_scheme().map(GrpcScheme::as_str),
            tls_handshake_timeout_ms: config.tls_handshake_timeout(),
            public_base_url: config
                .public_base_url()
                .as_ref()
                .map(|origin| origin.as_str().to_owned()),
            public_virtual_host_enabled: config.public_virtual_host_enabled(),
            cors_fallback: config.cors_fallback().fallback().as_str(),
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
    /// The buckets document initialization read, which a reset to the startup state re-applies.
    buckets_document: Option<gcs_mimic::bucket_init::ParsedDocument>,
}

enum PrimaryListener {
    Tcp(tokio::net::TcpListener),
    #[cfg(unix)]
    Unix {
        listener: server::unix::Listener,
        /// The configured path, which readiness reports.
        path: std::path::PathBuf,
    },
}

/// A setting that binds a TCP listener. Its diagnostics name the setting and the flag that
/// changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenerSetting {
    Listen,
    ListenHttp,
    TrustedListen,
    GrpcListen,
}

impl ListenerSetting {
    const fn key(self) -> &'static str {
        match self {
            Self::Listen => "listen",
            Self::ListenHttp => "listen_http",
            Self::TrustedListen => "trusted_listen",
            Self::GrpcListen => "grpc_listen",
        }
    }

    const fn flag(self) -> &'static str {
        match self {
            Self::Listen => "--listen",
            Self::ListenHttp => "--listen-http",
            Self::TrustedListen => "--trusted-listen",
            Self::GrpcListen => "--grpc-listen",
        }
    }

    fn bind_failure(self, error: &io::Error) -> Diagnostics {
        let fix = match self {
            Self::GrpcListen => format!(
                "Cannot bind grpc_listen ({:?}); stop the process using that port or choose a free separate address, for example --grpc-listen 127.0.0.1:4444.",
                error.kind()
            ),
            Self::Listen | Self::ListenHttp | Self::TrustedListen
                if error.kind() == io::ErrorKind::AddrInUse =>
            {
                format!(
                    "Address already in use; choose a free {} IP:PORT.",
                    self.flag()
                )
            }
            Self::Listen | Self::ListenHttp | Self::TrustedListen => format!(
                "Cannot bind listener; check {} IP:PORT and local network permissions.",
                self.flag()
            ),
        };
        Diagnostics::error(DiagnosticSource::Listener, self.key(), fix)
    }

    /// The address a bound listener accepts on.
    fn local_addr(self, listener: &tokio::net::TcpListener) -> Result<SocketAddr, Diagnostics> {
        listener.local_addr().map_err(|_| self.address_unreadable())
    }

    fn address_unreadable(self) -> Diagnostics {
        let fix = match self {
            Self::Listen => "Cannot inspect listener address; check the local network environment.",
            Self::ListenHttp => {
                "Cannot inspect HTTP listener address; check the local network environment."
            }
            Self::TrustedListen => {
                "Cannot inspect trusted listener address; check the local network environment."
            }
            Self::GrpcListen => {
                "Cannot inspect gRPC listener address; check the local network environment."
            }
        };
        Diagnostics::error(DiagnosticSource::Listener, self.key(), fix)
    }
}

async fn bind_primary(config: &Config) -> Result<PrimaryListener, Diagnostics> {
    if let Some(_path) = config.unix_socket() {
        if matches!(
            config.scheme(),
            gcs_mimic::config::Scheme::Https | gcs_mimic::config::Scheme::Both
        ) {
            return Err(Diagnostics::error(
                DiagnosticSource::Listener,
                "unix_socket",
                "Unix transport serves plain HTTP; it requires scheme=auto or http and no TCP HTTP/gRPC listener settings.",
            ));
        }
        #[cfg(unix)]
        return server::unix::Listener::bind(_path).await.map(|listener| PrimaryListener::Unix { listener, path: _path.to_owned() }).map_err(|error| {
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
    bind_configured_listener(config.listen(), ListenerSetting::Listen)
        .await
        .map(PrimaryListener::Tcp)
}

async fn open_catalog(
    config: &Config,
    clock: Arc<dyn Clock>,
    shutdown: std::pin::Pin<&mut impl std::future::Future<Output = io::Result<()>>>,
) -> Result<PhaseResult<Catalog>, Diagnostics> {
    let opening = async {
        let admin = if config.admin_enabled() {
            gcs_mimic::storage::admin::LocalAdmin::Enabled
        } else {
            gcs_mimic::storage::admin::LocalAdmin::Disabled
        };
        let opened = match config.backend() {
            gcs_mimic::config::Backend::Disk => {
                Catalog::open_for_server(config.data_dir(), clock, admin).await
            }
            gcs_mimic::config::Backend::Memory => {
                Catalog::open_memory_for_server(
                    config.memory_max_bytes(),
                    config.memory_catalog_max_bytes(),
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
            catalog.set_default_soft_delete_retention(match config.compat_profile() {
                gcs_mimic::config::CompatProfile::FakeGcsServer => None,
                gcs_mimic::config::CompatProfile::Native => {
                    config.default_soft_delete_retention_seconds()
                }
            });
            catalog.apply_session_limits(config);
            catalog.configure_lifecycle(config);
        }
        opened.map_err(|error| catalog_open_failure(&error))
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

/// The diagnostic for a catalog that could not be opened. Only a newer schema has its own remedy.
fn catalog_open_failure(error: &CatalogError) -> Diagnostics {
    let fix: Cow<'static, str> = match error {
        CatalogError::NewerSchema { version, supported } => format!("catalog schema version {version} was written by a newer gcs-mimic release; this release reads schema {supported} and older. Start a release that supports schema {version}, restore a backup taken before the upgrade, or choose a different data directory; migrated catalogs cannot be downgraded.").into(),
        CatalogError::Io(_)
        | CatalogError::Corrupt(_)
        | CatalogError::Unavailable(_)
        // Request outcomes that opening never produces share the same advice.
        | CatalogError::AuthenticationAlreadyConfigured
        | CatalogError::AlreadyExists
        | CatalogError::NotFound
        | CatalogError::NotEmpty
        | CatalogError::RestoreConflict
        | CatalogError::PreconditionFailed
        | CatalogError::NotModified
        | CatalogError::PublicAccessPrevention
        | CatalogError::Denied(_)
        | CatalogError::Forbidden
        | CatalogError::RetentionPolicyLocked(_)
        | CatalogError::RetentionPolicyNotSet
        | CatalogError::InvalidMetadata(_)
        | CatalogError::Metadata(_)
        | CatalogError::UnsupportedMetadata(_) => Cow::Borrowed("catalog unavailable or already in use; use a writable compatible native data directory owned by only one server and inspect its recovery state."),
    };
    Diagnostics::error(DiagnosticSource::Catalog, "data_dir", fix)
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
        gcs_mimic::seed::import(catalog, config, &clock)
            .await
            .map_err(|error| {
                Diagnostics::error(DiagnosticSource::Seed, "seed_dir", seed_failure(&error))
            })
    })?;
    // Bind/TIME_WAIT has no catalog effects and owns its socket locally.
    let listeners = complete!(async {
        let primary = bind_primary(config).await?;
        let http = bind_optional(config.listen_http(), ListenerSetting::ListenHttp).await?;
        let trusted =
            bind_optional(config.trusted_listen(), ListenerSetting::TrustedListen).await?;
        let grpc = bind_optional(config.grpc_listen(), ListenerSetting::GrpcListen).await?;
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
        buckets: buckets.outcome,
        buckets_document: buckets.document,
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
    mut config: Config,
    auth: Auth,
    security: gcs_mimic::tls::ServerSecurity,
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
        buckets_document,
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
    // Only a reset to the startup state reads the document again, so a server without local
    // administration does not keep a document of up to 1 GiB for its lifetime.
    if config.admin_enabled() {
        config.set_startup_buckets(buckets_document.map(gcs_mimic::config::DocumentSource::Parsed));
    }
    let ready = match ready_event(&config, &auth, &catalog, &listeners, seed, buckets) {
        Ok(ready) => ready,
        Err(error) => return Ok(close_startup(&catalog, Err(error)).await?),
    };
    catalog.start_resumable_maintenance();
    // The passes start with the listeners: the startup replay has just read the tree.
    gcs_mimic::seed::watch(&catalog, &config, clock.clone());
    let state = Arc::new(State {
        pools: server::WorkPools::new(&config),
        xml_cors_warned_buckets: Default::default(),
        grpc_read_handles: Default::default(),
        ready: AtomicBool::new(true),
        clock,
        catalog,
        api_base_url: ready.api_base_url,
        default_location: config.default_location().to_owned(),
        auth,
    });
    // Readiness must follow every fallible startup step.
    println!("{}", ready.event);
    let BoundListeners {
        primary,
        http,
        trusted,
        grpc,
    } = listeners;
    let gcs_mimic::tls::ServerSecurity {
        primary: primary_security,
        grpc: grpc_security,
    } = security;
    let listeners = match primary {
        PrimaryListener::Tcp(listener) => {
            let mut public = vec![(listener, primary_security)];
            public.extend(http.map(|listener| (listener, gcs_mimic::tls::ListenerSecurity::Plain)));
            server::Listeners::from(server::TcpListeners {
                public,
                trusted,
                grpc: grpc.map(|listener| (listener, grpc_security)),
            })
        }
        #[cfg(unix)]
        PrimaryListener::Unix { listener, .. } => server::Listeners::from(listener),
    };
    server::serve(listeners, config, state, server::Stop::on(shutdown)).await?;
    report_stopped();
    Ok(())
}

/// The bound listeners in the order readiness reports them. A Unix socket is the only listener
/// its server binds, which `bind_primary` enforces.
fn ready_transport(
    config: &Config,
    listeners: &BoundListeners,
) -> Result<ReadyTransport, Diagnostics> {
    match &listeners.primary {
        #[cfg(unix)]
        PrimaryListener::Unix { path, .. } => Ok(ReadyTransport::Unix {
            socket: path.to_string_lossy().into_owned(),
        }),
        PrimaryListener::Tcp(primary) => {
            let listen = ListenerSetting::Listen.local_addr(primary)?;
            let mut entries = vec![ReadyListener {
                listen,
                scheme: config.scheme().primary(),
            }];
            if let Some(http) = &listeners.http {
                entries.push(ReadyListener {
                    listen: ListenerSetting::ListenHttp.local_addr(http)?,
                    scheme: "http",
                });
            }
            if let (Some(grpc), Some(scheme)) = (&listeners.grpc, config.grpc_scheme()) {
                entries.push(ReadyListener {
                    listen: ListenerSetting::GrpcListen.local_addr(grpc)?,
                    scheme: scheme.listener_scheme(),
                });
            }
            Ok(ReadyTransport::Tcp {
                listen,
                listeners: entries,
            })
        }
    }
}

fn ready_event(
    config: &Config,
    auth: &Auth,
    catalog: &Catalog,
    listeners: &BoundListeners,
    seed: SeedOutcome,
    buckets: InitOutcome,
) -> Result<Ready, Diagnostics> {
    let transport = ready_transport(config, listeners)?;
    let trusted_listen = listeners
        .trusted
        .as_ref()
        .map(|listener| ListenerSetting::TrustedListen.local_addr(listener))
        .transpose()?;
    let api_base_url = transport.api_base_url(config);
    let data_directory =
        catalog
            .data_directory()
            .map(|(path, data_dir_state)| ReadyDataDirectory {
                data_dir: path.to_string_lossy().into_owned(),
                data_dir_state,
            });
    let event = ReadyEvent::new(
        config,
        auth,
        Readiness {
            transport,
            trusted_listen,
            data_directory,
            seed,
            buckets,
            api_base_url: api_base_url.clone(),
        },
    );
    let event = serde_json::to_string(&event).map_err(|_| {
        Diagnostics::error(
            DiagnosticSource::Runtime,
            "ready",
            "Cannot encode ready event; inspect startup configuration.",
        )
    })?;
    Ok(Ready {
        event,
        api_base_url,
    })
}

/// The reason a seed failure has when nothing more specific explains it.
const UNREADABLE_FIXTURE: &str = "unreadable or invalid fixture input";

/// What the seed diagnostic says about a failure.
enum SeedAdvice {
    /// A complete message that names its own remedy.
    Complete(Cow<'static, str>),
    /// A short reason, followed by the shared advice to correct the fixtures.
    Reason(&'static str),
}

impl SeedAdvice {
    fn into_fix(self) -> String {
        match self {
            Self::Complete(message) => message.into_owned(),
            Self::Reason(reason) => format!(
                "seed import: {reason}; correct seed_dir fixtures and their permissions, names, project, overlap and configured limits before restarting."
            ),
        }
    }
}

fn seed_failure(error: &SeedError) -> String {
    seed_advice(error).into_fix()
}

fn seed_advice(error: &SeedError) -> SeedAdvice {
    match error {
        SeedError::Import(source) | SeedError::Entry { source, .. } => seed_advice(source),
        SeedError::Admin(error) => admin_seed_advice(error),
        // These name the configured limit and the setting that changes it.
        SeedError::DepthLimit { .. }
        | SeedError::EntryLimit { .. }
        | SeedError::BucketLimit { .. } => {
            SeedAdvice::Complete(format!("seed import: {error}.").into())
        }
        // The walk opens absolute paths; without a depth limit nothing else stops at this length.
        SeedError::Io(error) if error.kind() == io::ErrorKind::InvalidFilename => {
            SeedAdvice::Reason("a fixture path exceeds this platform's path length limit")
        }
        SeedError::BucketName(_) => SeedAdvice::Reason("Invalid bucket name"),
        SeedError::ObjectName(_) => SeedAdvice::Reason("Invalid object name"),
        SeedError::FileLimit => SeedAdvice::Reason("max_object_bytes"),
        SeedError::Overlap => SeedAdvice::Reason("must not overlap"),
        SeedError::UnsupportedFile => SeedAdvice::Reason("only regular files"),
        SeedError::NotDirectory => SeedAdvice::Reason("not a symlink"),
        SeedError::NonUtf8Name => SeedAdvice::Reason("must be UTF-8"),
        SeedError::Io(_)
        | SeedError::Unopenable
        | SeedError::RelativePath(_)
        | SeedError::WalkStopped
        | SeedError::TreeChanged
        | SeedError::MissingRoot => SeedAdvice::Reason(UNREADABLE_FIXTURE),
    }
}

fn admin_seed_advice(error: &AdminError) -> SeedAdvice {
    match error {
        AdminError::Hold => SeedAdvice::Complete(Cow::Borrowed(
            "seed import: a changed fixture is protected by a hold or retention; the entire seed was left unapplied. Restart without seed_dir to release the hold through the API, or wait for retention to expire, then retry.",
        )),
        // No entry limit stops a seed before this. Only the storage that ran out has a remedy:
        // a disk backend has no RAM budget to raise, and a memory backend no data directory.
        AdminError::DiskFull(cause) => {
            SeedAdvice::Complete(Cow::Borrowed(match cause.memory_budget() {
                None => {
                    "seed import: storage capacity exhausted; free space in the data directory before restarting."
                }
                Some(MemoryBudget::Blobs) => {
                    "seed import: storage capacity exhausted; restart with a larger memory_max_bytes or a smaller seed tree."
                }
                Some(MemoryBudget::Catalog) => {
                    "seed import: storage capacity exhausted; restart with a larger memory_catalog_max_bytes or a smaller seed tree."
                }
            }))
        }
        AdminError::SeedProjectMismatch => SeedAdvice::Reason("belongs to another project"),
        // The catalog enforces these limits while it publishes; each names the setting to raise.
        AdminError::SeedMetadataBudget { mib } => SeedAdvice::Complete(
            format!(
                "seed import: fixture metadata exceeds {mib} MiB; raise max_seed_entries or reduce the tree."
            )
            .into(),
        ),
        AdminError::TooLarge => SeedAdvice::Complete(Cow::Borrowed(
            "seed import: a fixture file exceeds max_object_bytes; raise max_object_bytes or use a smaller file.",
        )),
        AdminError::Busy { .. }
        | AdminError::Closed
        | AdminError::Precondition
        | AdminError::SourceChanged { .. }
        | AdminError::Corrupt(_)
        | AdminError::Io(_)
        | AdminError::NotFound
        | AdminError::Invalid(_)
        | AdminError::Checksum
        | AdminError::Forbidden
        | AdminError::BodyAborted(_)
        | AdminError::Backend(_) => SeedAdvice::Reason(UNREADABLE_FIXTURE),
    }
}

async fn bind_configured_listener(
    address: SocketAddr,
    setting: ListenerSetting,
) -> Result<tokio::net::TcpListener, Diagnostics> {
    bind_listener(address)
        .await
        .map_err(|error| setting.bind_failure(&error))
}

async fn bind_optional(
    address: Option<SocketAddr>,
    setting: ListenerSetting,
) -> Result<Option<tokio::net::TcpListener>, Diagnostics> {
    match address {
        Some(address) => bind_configured_listener(address, setting).await.map(Some),
        None => Ok(None),
    }
}

/// Matches the default `max_connections`, so a burst of new clients at a full or starting server
/// waits instead of being refused. The kernel can still lower it to its own limit.
const LISTEN_BACKLOG: u32 = 1024;

#[cfg_attr(
    not(all(unix, not(any(target_os = "linux", target_os = "android")))),
    expect(
        clippy::unused_async,
        reason = "only BSD-style Unix hosts await the TIME_WAIT retry after a failed bind"
    )
)]
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
                SeedError::MissingRoot,
                "unreadable or invalid fixture input",
            ),
            (
                SeedError::Admin(AdminError::Closed),
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
    fn seed_limits_the_catalog_enforces_name_the_setting_to_raise() {
        for (error, message) in [
            (
                AdminError::SeedMetadataBudget { mib: 4 },
                "seed import: fixture metadata exceeds 4 MiB; raise max_seed_entries or reduce the tree.",
            ),
            (
                AdminError::TooLarge,
                "seed import: a fixture file exceeds max_object_bytes; raise max_object_bytes or use a smaller file.",
            ),
        ] {
            let error = SeedError::Import(Box::new(SeedError::Entry {
                label: "fixture".into(),
                source: Box::new(SeedError::Admin(error)),
            }));
            assert_eq!(seed_failure(&error), message);
        }
    }

    #[test]
    fn catalog_open_failures_name_a_newer_schema_and_share_other_advice() {
        let diagnostics = catalog_open_failure(&CatalogError::NewerSchema {
            version: 9,
            supported: 8,
        });
        let error = only_error(&diagnostics);
        assert_eq!(error.source, DiagnosticSource::Catalog);
        assert_eq!(error.key, "data_dir");
        assert!(
            error.fix.starts_with(
                "catalog schema version 9 was written by a newer gcs-mimic release; this release reads schema 8 and older"
            ),
            "{}",
            error.fix
        );
        let diagnostics = catalog_open_failure(&CatalogError::from(io::Error::from(
            io::ErrorKind::PermissionDenied,
        )));
        let error = only_error(&diagnostics);
        assert_eq!(error.key, "data_dir");
        assert_eq!(
            error.fix,
            "catalog unavailable or already in use; use a writable compatible native data directory owned by only one server and inspect its recovery state."
        );
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
        // Unix settings reject an explicit TCP address. Occupy the implicit address to prove
        // that Unix startup succeeds even when the ordinary TCP bind would fail.
        let _occupied = match std::net::TcpListener::bind(Config::default().listen()) {
            Ok(listener) => Some(listener),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => None,
            Err(error) => panic!("{error}"),
        };
        let catalog = Catalog::open(&directory.0).await.unwrap();
        let socket = directory.0.join("s");
        let config = test_config(
            &directory.0,
            &["--scheme=http", "--unix-socket", socket.to_str().unwrap()],
        );
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
        assert!(listeners.http.is_none() && listeners.grpc.is_none());
        assert!(listeners.trusted.is_none());
        let PrimaryListener::Unix { listener, path } = listeners.primary else {
            panic!("TCP fallback")
        };
        assert_eq!(Some(path.as_path()), config.unix_socket());
        listener.close().await.unwrap();
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_listener_disabled_by_default_and_bound_after_initialization() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let config = dir.config();
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
        let missing = dir.0.join("missing-buckets.json");
        let config = test_config(
            &dir.0,
            &[
                "--listen=127.0.0.1:0",
                "--grpc-listen=127.0.0.1:0",
                "--buckets-file",
                missing.to_str().unwrap(),
            ],
        );
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
        let config = test_config(
            &dir.0,
            &["--listen=127.0.0.1:0", "--grpc-listen=127.0.0.1:0"],
        );
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
        let failure = bind_configured_listener(address, ListenerSetting::GrpcListen)
            .await
            .unwrap_err();
        assert!(
            failure.to_string().contains("grpc_listen")
                && failure.to_string().contains("127.0.0.1:4444")
        );
        drop(bound);
        catalog.close().await.unwrap();
    }

    /// The only error of a diagnostic that reports one failure.
    fn only_error(diagnostics: &Diagnostics) -> &gcs_mimic::diagnostics::Diagnostic {
        assert!(diagnostics.info.is_empty() && diagnostics.warnings.is_empty());
        let [error] = diagnostics.errors.as_slice() else {
            panic!("one error expected: {:?}", diagnostics.errors);
        };
        error
    }

    #[test]
    fn listener_bind_failures_name_the_setting_and_its_flag() {
        for (setting, key, in_use, denied) in [
            (
                ListenerSetting::Listen,
                "listen",
                "Address already in use; choose a free --listen IP:PORT.",
                "Cannot bind listener; check --listen IP:PORT and local network permissions.",
            ),
            (
                ListenerSetting::ListenHttp,
                "listen_http",
                "Address already in use; choose a free --listen-http IP:PORT.",
                "Cannot bind listener; check --listen-http IP:PORT and local network permissions.",
            ),
            (
                ListenerSetting::TrustedListen,
                "trusted_listen",
                "Address already in use; choose a free --trusted-listen IP:PORT.",
                "Cannot bind listener; check --trusted-listen IP:PORT and local network permissions.",
            ),
            (
                ListenerSetting::GrpcListen,
                "grpc_listen",
                "Cannot bind grpc_listen (AddrInUse); stop the process using that port or choose a free separate address, for example --grpc-listen 127.0.0.1:4444.",
                "Cannot bind grpc_listen (PermissionDenied); stop the process using that port or choose a free separate address, for example --grpc-listen 127.0.0.1:4444.",
            ),
        ] {
            for (kind, fix) in [
                (io::ErrorKind::AddrInUse, in_use),
                (io::ErrorKind::PermissionDenied, denied),
            ] {
                let diagnostics = setting.bind_failure(&kind.into());
                let error = only_error(&diagnostics);
                assert_eq!(error.source, DiagnosticSource::Listener, "{setting:?}");
                assert_eq!(error.key, key, "{setting:?}");
                assert_eq!(error.fix, fix, "{setting:?} {kind:?}");
            }
        }
    }

    #[test]
    fn unreadable_listener_addresses_name_the_listener() {
        for (setting, key, fix) in [
            (
                ListenerSetting::Listen,
                "listen",
                "Cannot inspect listener address; check the local network environment.",
            ),
            (
                ListenerSetting::ListenHttp,
                "listen_http",
                "Cannot inspect HTTP listener address; check the local network environment.",
            ),
            (
                ListenerSetting::TrustedListen,
                "trusted_listen",
                "Cannot inspect trusted listener address; check the local network environment.",
            ),
            (
                ListenerSetting::GrpcListen,
                "grpc_listen",
                "Cannot inspect gRPC listener address; check the local network environment.",
            ),
        ] {
            let diagnostics = setting.address_unreadable();
            let error = only_error(&diagnostics);
            assert_eq!(error.source, DiagnosticSource::Listener, "{setting:?}");
            assert_eq!(error.key, key, "{setting:?}");
            assert_eq!(error.fix, fix, "{setting:?}");
        }
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
        let missing_seed = dir.0.join("missing-seed");
        let config = test_config(
            &dir.0,
            &[
                "--listen=127.0.0.1:0",
                "--seed-dir",
                missing_seed.to_str().unwrap(),
            ],
        );
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
        let config = test_config(
            &dir.0,
            &[
                "--listen=127.0.0.1:0",
                "--seed-dir",
                seed.0.to_str().unwrap(),
            ],
        );
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
            test_config(&self.0, &["--listen=127.0.0.1:0"])
        }
    }

    fn test_config(data_dir: &std::path::Path, options: &[&str]) -> Config {
        let mut args = vec!["--data-dir".into(), data_dir.to_str().unwrap().to_owned()];
        args.extend(options.iter().map(|option| (*option).to_owned()));
        Config::load(&args, &Default::default()).unwrap()
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
            let seed = dir.0.join("fixtures");
            let config = test_config(
                &dir.0.join("data"),
                &["--listen=127.0.0.1:0", "--seed-dir", seed.to_str().unwrap()],
            );
            fs::create_dir_all(seed.join("seed-bucket")).unwrap();
            let catalog = Catalog::open(config.data_dir()).await.unwrap();
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
                File::open(config.data_dir().join("catalog.lock"))
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
            let reopened = Catalog::open(config.data_dir()).await.unwrap();
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
                rusqlite::Connection::open(config.data_dir().join("catalog.sqlite3")).unwrap();
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
        let buckets = dir.0.join("buckets.json");
        fs::write(
            &buckets,
            r#"{"version":1,"buckets":[{"name":"pending-bucket"}]}"#,
        )
        .unwrap();
        let config = test_config(
            &dir.0.join("data"),
            &[
                "--listen=127.0.0.1:0",
                "--buckets-file",
                buckets.to_str().unwrap(),
            ],
        );
        let catalog = Catalog::open(config.data_dir()).await.unwrap();
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
        let reopened = Catalog::open(config.data_dir()).await.unwrap();
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
        let listen = listener.local_addr().unwrap().to_string();
        let config = test_config(&dir.0, &["--listen", &listen]);
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
