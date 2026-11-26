use super::*;
use crate::{mime::SeedContentType, notification};
use std::time::Duration;

/// Settings that have passed configuration validation.
#[derive(Clone, Debug)]
pub struct Config {
    listeners: Listeners,
    tls: Option<Tls>,
    storage: Storage,
    seed: Seed,
    endpoints: Endpoints,
    admin: Admin,
    notifications: notification::Runtime,
    limits: Limits,
    timeouts: Timeouts,
    auth: AuthSettings,
    log_level: LogLevel,
    compat_profile: CompatProfile,
    cors_fallback: CorsFallbackChoice,
}

#[derive(Clone, Debug)]
pub(super) struct Listeners {
    listen: SocketAddr,
    scheme: Scheme,
    transport: Bindings,
}

#[derive(Clone, Debug)]
enum Bindings {
    Tcp {
        http: Option<SocketAddr>,
        grpc: Option<GrpcListener>,
        trusted: Option<TrustedListener>,
    },
    Unix(PathBuf),
}

#[derive(Clone, Debug)]
struct GrpcListener {
    address: SocketAddr,
    scheme: GrpcScheme,
}

#[derive(Clone, Debug)]
struct TrustedListener {
    address: SocketAddr,
    allowed_hosts: Vec<String>,
}

impl From<&ConfigDraft> for Listeners {
    fn from(draft: &ConfigDraft) -> Self {
        let transport = match &draft.unix_socket {
            Some(path) => Bindings::Unix(path.clone()),
            None => Bindings::Tcp {
                http: draft.resolved_listen_http(),
                grpc: draft.grpc_listen.map(|address| GrpcListener {
                    address,
                    scheme: draft.grpc_scheme,
                }),
                trusted: draft.trusted_listen.map(|address| TrustedListener {
                    address,
                    allowed_hosts: draft.trusted_allowed_hosts.clone(),
                }),
            },
        };
        Self {
            listen: draft.listen,
            scheme: draft.scheme,
            transport,
        }
    }
}

impl Listeners {
    fn http(&self) -> Option<SocketAddr> {
        match &self.transport {
            Bindings::Tcp { http, .. } => *http,
            Bindings::Unix(_) => None,
        }
    }

    fn grpc(&self) -> Option<&GrpcListener> {
        match &self.transport {
            Bindings::Tcp { grpc, .. } => grpc.as_ref(),
            Bindings::Unix(_) => None,
        }
    }

    fn trusted(&self) -> Option<&TrustedListener> {
        match &self.transport {
            Bindings::Tcp { trusted, .. } => trusted.as_ref(),
            Bindings::Unix(_) => None,
        }
    }

    fn unix_socket(&self) -> Option<&Path> {
        match &self.transport {
            Bindings::Tcp { .. } => None,
            Bindings::Unix(path) => Some(path),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Tls {
    identity: TlsIdentity,
}

#[derive(Clone, Debug)]
pub(crate) enum TlsIdentity {
    Development,
    Provided { certificate: PathBuf, key: PathBuf },
}

impl Tls {
    pub(super) fn select(draft: &ConfigDraft) -> Result<Self, Setting> {
        let identity = match (&draft.tls_cert, &draft.tls_key) {
            (None, None) => TlsIdentity::Development,
            (Some(certificate), Some(key)) => TlsIdentity::Provided {
                certificate: certificate.clone(),
                key: key.clone(),
            },
            (Some(_), None) => return Err(Setting::TlsCert),
            (None, Some(_)) => return Err(Setting::TlsKey),
        };
        Ok(Self { identity })
    }

    pub(crate) fn identity(&self) -> &TlsIdentity {
        &self.identity
    }
}

#[derive(Clone, Debug)]
struct Storage {
    data_dir: PathBuf,
    backend: Backend,
    memory_max_bytes: u64,
    memory_catalog_max_bytes: u64,
    default_location: String,
    default_soft_delete_retention_seconds: Option<NonZeroU64>,
    lifecycle_execution: LifecycleExecution,
    buckets: Option<DocumentSource>,
    buckets_mode: BucketsMode,
}

#[derive(Clone, Debug)]
struct Seed {
    dir: Option<PathBuf>,
    default_dir: Option<PathBuf>,
    project: String,
    content_type: SeedContentType,
    watch_interval: Option<Duration>,
}

#[derive(Clone, Debug)]
struct Endpoints {
    api_base_url: Option<String>,
    public_base_url: Option<PublicOrigin>,
    xml_virtual_host_domains: VirtualHostDomains,
    runtime: RuntimeEndpoints,
}

#[derive(Clone, Debug)]
struct Admin {
    enabled: bool,
    principal: Option<String>,
    allowed_hosts: Vec<String>,
}

#[derive(Clone, Debug)]
struct Limits {
    max_connections: usize,
    max_grpc_streams: NonZeroU32,
    max_object_bytes: u64,
    max_resumable_sessions: Option<NonZeroU32>,
    max_rewrite_sessions: Option<NonZeroU32>,
    max_seed_entries: Option<NonZeroUsize>,
    max_seed_depth: Option<NonZeroUsize>,
    max_buckets_per_batch: Option<NonZeroUsize>,
    worker_threads: usize,
    max_blocking_threads: usize,
}

#[derive(Clone, Debug)]
struct Timeouts {
    tls_handshake: TimeLimit,
    header_read: TimeLimit,
    body_inactivity: TimeLimit,
    request: TimeLimit,
    keep_alive: TimeLimit,
    object_request: TimeLimit,
    catalog_wait: TimeLimit,
    shutdown: TimeLimit,
}

#[derive(Clone, Debug)]
struct AuthSettings {
    mode: AuthMode,
    local_creator_email: Option<String>,
    signed_urls: SignedUrls,
}

impl TryFrom<ConfigurationReport> for Config {
    type Error = Diagnostics;

    fn try_from(report: ConfigurationReport) -> Result<Self, Self::Error> {
        report.into_config().map(|(config, _)| config)
    }
}

impl ConfigurationReport {
    /// Assembles the configuration and returns the startup notices to report before serving.
    pub fn into_config(self) -> Result<(Config, Diagnostics), Diagnostics> {
        if self.diagnostics.is_err() {
            return Err(self.diagnostics);
        }
        let config = Config::assemble(self.draft, self.listeners, self.tls, self.notifications);
        Ok((config, self.diagnostics))
    }
}

impl Default for Config {
    fn default() -> Self {
        let draft = ConfigDraft::default();
        let listeners = Listeners::from(&draft);
        let tls = Tls {
            identity: TlsIdentity::Development,
        };
        Self::assemble(
            draft,
            listeners,
            Some(tls),
            notification::Runtime::default(),
        )
    }
}

#[cfg(test)]
impl ConfigDraft {
    pub(crate) fn build(self) -> Config {
        let listeners = Listeners::from(&self);
        let tls = crate::tls::serves_tls(
            self.scheme,
            self.unix_socket.as_deref(),
            self.grpc_security_scheme(),
        )
        .then(|| Tls::select(&self).unwrap());
        let notifications = self.notification_runtime().unwrap();
        Config::assemble(self, listeners, tls, notifications)
    }
}

macro_rules! getters {
    ($($name:ident: $ty:ty = $($field:ident).+;)*) => {
        $(pub fn $name(&self) -> $ty { self.$($field).+ })*
    };
}

impl Config {
    pub const HEALTHCHECK_PATH: &str = "/_internal/healthcheck";

    fn assemble(
        draft: ConfigDraft,
        listeners: Listeners,
        tls: Option<Tls>,
        notifications: notification::Runtime,
    ) -> Self {
        let cors_fallback = draft.resolved_cors_fallback();
        let signed_urls = draft.resolved_signed_urls();
        Self {
            listeners,
            tls,
            storage: Storage {
                data_dir: draft.data_dir,
                backend: draft.backend,
                memory_max_bytes: draft.memory_max_bytes,
                memory_catalog_max_bytes: draft.memory_catalog_max_bytes,
                default_location: draft.default_location,
                default_soft_delete_retention_seconds: draft.default_soft_delete_retention_seconds,
                lifecycle_execution: draft.lifecycle_execution,
                buckets: draft.buckets,
                buckets_mode: draft.buckets_mode,
            },
            seed: Seed {
                dir: draft.seed_dir,
                default_dir: draft.default_seed_dir,
                project: draft.seed_project,
                content_type: draft.seed_content_type,
                watch_interval: draft.seed_watch_interval,
            },
            endpoints: Endpoints {
                api_base_url: draft.api_base_url,
                public_base_url: draft.public_base_url,
                xml_virtual_host_domains: draft.xml_virtual_host_domains,
                runtime: draft.runtime_endpoints,
            },
            admin: Admin {
                enabled: draft.admin_enabled,
                principal: draft.admin_principal,
                allowed_hosts: draft.admin_allowed_hosts,
            },
            notifications,
            limits: Limits {
                max_connections: draft.max_connections,
                max_grpc_streams: draft.max_grpc_streams,
                max_object_bytes: draft.max_object_bytes,
                max_resumable_sessions: draft.max_resumable_sessions,
                max_rewrite_sessions: draft.max_rewrite_sessions,
                max_seed_entries: draft.max_seed_entries,
                max_seed_depth: draft.max_seed_depth,
                max_buckets_per_batch: draft.max_buckets_per_batch,
                worker_threads: draft.worker_threads,
                max_blocking_threads: draft.max_blocking_threads,
            },
            timeouts: Timeouts {
                tls_handshake: draft.tls_handshake_timeout,
                header_read: draft.header_read_timeout,
                body_inactivity: draft.body_inactivity_timeout,
                request: draft.request_timeout,
                keep_alive: draft.keep_alive_timeout,
                object_request: draft.object_request_timeout,
                catalog_wait: draft.catalog_wait_timeout,
                shutdown: draft.shutdown_timeout,
            },
            auth: AuthSettings {
                mode: draft.auth_mode,
                local_creator_email: draft.local_creator_email,
                signed_urls,
            },
            log_level: draft.log_level,
            compat_profile: draft.compat_profile,
            cors_fallback,
        }
    }

    getters! {
        log_level: LogLevel = log_level;
        listen: SocketAddr = listeners.listen;
        scheme: Scheme = listeners.scheme;
        backend: Backend = storage.backend;
        memory_max_bytes: u64 = storage.memory_max_bytes;
        memory_catalog_max_bytes: u64 = storage.memory_catalog_max_bytes;
        seed_content_type: SeedContentType = seed.content_type;
        seed_watch_interval: Option<Duration> = seed.watch_interval;
        default_soft_delete_retention_seconds: Option<NonZeroU64> = storage.default_soft_delete_retention_seconds;
        lifecycle_execution: LifecycleExecution = storage.lifecycle_execution;
        buckets_mode: BucketsMode = storage.buckets_mode;
        admin_enabled: bool = admin.enabled;
        max_connections: usize = limits.max_connections;
        max_grpc_streams: NonZeroU32 = limits.max_grpc_streams;
        max_object_bytes: u64 = limits.max_object_bytes;
        max_resumable_sessions: Option<NonZeroU32> = limits.max_resumable_sessions;
        max_rewrite_sessions: Option<NonZeroU32> = limits.max_rewrite_sessions;
        max_seed_entries: Option<NonZeroUsize> = limits.max_seed_entries;
        max_seed_depth: Option<NonZeroUsize> = limits.max_seed_depth;
        max_buckets_per_batch: Option<NonZeroUsize> = limits.max_buckets_per_batch;
        worker_threads: usize = limits.worker_threads;
        max_blocking_threads: usize = limits.max_blocking_threads;
        tls_handshake_timeout: TimeLimit = timeouts.tls_handshake;
        header_read_timeout: TimeLimit = timeouts.header_read;
        body_inactivity_timeout: TimeLimit = timeouts.body_inactivity;
        request_timeout: TimeLimit = timeouts.request;
        keep_alive_timeout: TimeLimit = timeouts.keep_alive;
        object_request_timeout: TimeLimit = timeouts.object_request;
        catalog_wait_timeout: TimeLimit = timeouts.catalog_wait;
        shutdown_timeout: TimeLimit = timeouts.shutdown;
        auth_mode: AuthMode = auth.mode;
        signed_urls: SignedUrls = auth.signed_urls;
        compat_profile: CompatProfile = compat_profile;
        cors_fallback: CorsFallbackChoice = cors_fallback;
    }

    pub fn listen_http(&self) -> Option<SocketAddr> {
        self.listeners.http()
    }

    pub fn grpc_listen(&self) -> Option<SocketAddr> {
        self.grpc_listener().map(|(address, _)| address)
    }

    pub fn grpc_scheme(&self) -> Option<GrpcScheme> {
        self.grpc_listener().map(|(_, scheme)| scheme)
    }

    /// The address and scheme of the dedicated gRPC listener, when configured.
    pub fn grpc_listener(&self) -> Option<(SocketAddr, GrpcScheme)> {
        self.listeners
            .grpc()
            .map(|listener| (listener.address, listener.scheme))
    }

    pub fn trusted_listen(&self) -> Option<SocketAddr> {
        self.listeners.trusted().map(|listener| listener.address)
    }

    pub fn unix_socket(&self) -> Option<&Path> {
        self.listeners.unix_socket()
    }

    pub(crate) fn tls(&self) -> Option<&Tls> {
        self.tls.as_ref()
    }

    pub fn data_dir(&self) -> &Path {
        &self.storage.data_dir
    }

    pub fn seed_dir(&self) -> Option<&Path> {
        self.seed.dir.as_deref()
    }

    pub fn default_seed_dir(&self) -> Option<&Path> {
        self.seed.default_dir.as_deref()
    }

    pub fn seed_project(&self) -> &str {
        &self.seed.project
    }

    pub fn default_location(&self) -> &str {
        &self.storage.default_location
    }

    pub fn buckets(&self) -> Option<&DocumentSource> {
        self.storage.buckets.as_ref()
    }

    /// Retains the buckets document that startup actually applied, for future resets.
    pub fn set_startup_buckets(&mut self, document: Option<DocumentSource>) {
        self.storage.buckets = document;
    }

    pub fn api_base_url(&self) -> Option<&str> {
        self.endpoints.api_base_url.as_deref()
    }

    pub fn public_base_url(&self) -> Option<&PublicOrigin> {
        self.endpoints.public_base_url.as_ref()
    }

    pub fn xml_virtual_host_domains(&self) -> &VirtualHostDomains {
        &self.endpoints.xml_virtual_host_domains
    }

    pub fn runtime_endpoints(&self) -> &RuntimeEndpoints {
        &self.endpoints.runtime
    }

    pub fn admin_principal(&self) -> Option<&str> {
        self.admin.principal.as_deref()
    }

    pub fn admin_allowed_hosts(&self) -> &[String] {
        &self.admin.allowed_hosts
    }

    pub fn trusted_allowed_hosts(&self) -> &[String] {
        self.listeners
            .trusted()
            .map_or(&[], |listener| listener.allowed_hosts.as_slice())
    }

    pub fn local_creator_email(&self) -> Option<&str> {
        self.auth.local_creator_email.as_deref()
    }

    pub(crate) fn notification_runtime(&self) -> &notification::Runtime {
        &self.notifications
    }

    pub fn healthcheck_url(&self) -> String {
        format!("{}{}", self.local_origin(), Self::HEALTHCHECK_PATH)
    }

    pub fn local_origin(&self) -> String {
        local_origin(self.listen(), self.listen_http(), self.scheme())
    }

    pub fn effective_api_base_url(&self, bound: SocketAddr) -> String {
        api_base(self.api_base_url(), self.scheme(), bound)
    }

    pub fn public_virtual_host_enabled(&self) -> bool {
        self.public_base_url()
            .is_some_and(|origin| origin.dns_name().is_some())
    }

    pub fn load(args: &[String], env: &BTreeMap<String, String>) -> Result<Self, Diagnostics> {
        Self::preflight(args, env).try_into()
    }

    pub(crate) fn preflight(
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> ConfigurationReport {
        let mut inputs = Inputs::default();
        inputs.cli(args, CliCommand::Serve);
        inputs.environment(env);
        ConfigDraft::from_inputs(inputs)
    }

    pub fn preflight_os(
        args: &[String],
        env: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    ) -> ConfigurationReport {
        let mut inputs = Inputs::default();
        inputs.cli(args, CliCommand::Serve);
        inputs.environment_os(env);
        ConfigDraft::from_inputs(inputs)
    }
}

pub(super) fn local_origin(listen: SocketAddr, http: Option<SocketAddr>, scheme: Scheme) -> String {
    let listen = loopback_if_unspecified(http.unwrap_or(listen));
    let scheme = match scheme {
        Scheme::Auto | Scheme::Http | Scheme::Both => "http",
        Scheme::Https => "https",
    };
    format!("{scheme}://{listen}")
}

pub(super) fn api_base(explicit: Option<&str>, scheme: Scheme, bound: SocketAddr) -> String {
    explicit.map_or_else(
        || {
            format!(
                "{}://{}",
                scheme.primary(),
                loopback_if_unspecified(SocketAddr::new(bound.ip(), bound.port()))
            )
        },
        str::to_owned,
    )
}
