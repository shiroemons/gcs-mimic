use super::*;

#[derive(Clone, Debug)]
pub struct Config {
    pub log_level: LogLevel,
    pub listen: SocketAddr,
    pub listen_http: Option<SocketAddr>,
    pub grpc_listen: Option<SocketAddr>,
    /// How `grpc_listen` secures its connections; validation requires `grpc_listen` with it.
    pub grpc_scheme: GrpcScheme,
    /// A plain HTTP listener for the application's own servers on a private network, where a
    /// request without credentials acts as `trusted_principal`. Strict mode only.
    pub trusted_listen: Option<SocketAddr>,
    pub unix_socket: Option<PathBuf>,
    pub scheme: Scheme,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub tls_handshake_timeout: TimeLimit,
    pub data_dir: PathBuf,
    pub backend: Backend,
    pub memory_max_bytes: u64,
    pub memory_catalog_max_bytes: u64,
    pub seed_dir: Option<PathBuf>,
    /// Fallback seed root used only when it exists and is disjoint from the data directory.
    pub default_seed_dir: Option<PathBuf>,
    pub seed_project: String,
    pub seed_content_type: crate::mime::SeedContentType,
    /// Files and directories a seed tree may hold; `None` stops at none. A seed is one atomic
    /// publication, so its memory grows with the tree until it commits.
    pub max_seed_entries: Option<NonZeroUsize>,
    /// Directory levels a seed tree may nest below a bucket; `None` leaves that to the object
    /// name's 1024 bytes.
    pub max_seed_depth: Option<NonZeroUsize>,
    /// How long the passes that replay `seed_dir` while the server runs wait between each other;
    /// `None` replays the tree only at startup.
    pub seed_watch_interval: Option<std::time::Duration>,
    /// Buckets one initialization file or one seed may declare; other buckets of the server do
    /// not count, and `None` stops at none. Either is one atomic publication, so its memory grows
    /// with the batch.
    pub max_buckets_per_batch: Option<NonZeroUsize>,
    pub default_location: String,
    /// Soft delete retention given to a new bucket that names no policy; `None` gives it none.
    pub default_soft_delete_retention_seconds: Option<NonZeroU64>,
    pub lifecycle_execution: LifecycleExecution,
    /// The bucket initialization document, from `buckets_file` or `buckets`.
    pub buckets: Option<DocumentSource>,
    /// What the buckets document does to existing buckets; it governs both document settings.
    pub buckets_mode: BucketsMode,
    pub api_base_url: Option<String>,
    pub public_base_url: Option<PublicOrigin>,
    /// Local management routes are disabled unless the operator explicitly enables them.
    pub admin_enabled: bool,
    pub admin_principal: Option<String>,
    pub admin_allowed_hosts: Vec<String>,
    /// The registered identity that requests without credentials on `trusted_listen` act as.
    pub trusted_principal: Option<String>,
    /// Dotted DNS names the trusted listener also accepts as Host, beyond IP literals,
    /// `localhost` names and single-label names.
    pub trusted_allowed_hosts: Vec<String>,
    /// Email recorded as the creator (owner and ACL OWNER entry) of objects written without
    /// authentication; `None` disables it, as the setting's empty value asks. Never an
    /// authorization input.
    pub local_creator_email: Option<String>,
    pub runtime_endpoints: RuntimeEndpoints,
    pub pubsub_emulator_host: Option<String>,
    pub event_pubsub_project_id: String,
    pub event_pubsub_topic: String,
    pub event_bucket: String,
    pub event_object_prefix: String,
    pub event_list: String,
    pub xml_virtual_host_domains: VirtualHostDomains,
    pub max_connections: usize,
    /// Streams each gRPC connection may run at once. Each can buffer a request message of up to
    /// 4 MiB and its encoded response, so lowering this limits a connection's memory.
    pub max_grpc_streams: NonZeroU32,
    pub max_object_bytes: u64,
    /// Active resumable uploads a server keeps; `None` leaves them unlimited, since expiry
    /// reclaims the disk they use.
    pub max_resumable_sessions: Option<NonZeroU32>,
    /// Unfinished rewrites a server keeps; `None` leaves them unlimited.
    pub max_rewrite_sessions: Option<NonZeroU32>,
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
    // Cloud Storage documents no seconds for these and asks for "reasonably long" upload
    // timeouts, so the defaults are local choices that a slow client should not reach.
    /// Waiting for a request head: on a new connection, and once a next request has started.
    pub header_read_timeout: TimeLimit,
    /// Silence while a request body on any route or a response body is in flight.
    pub body_inactivity_timeout: TimeLimit,
    /// A handler's time before its response starts, apart from waits for request body bytes.
    pub request_timeout: TimeLimit,
    pub keep_alive_timeout: TimeLimit,
    pub object_request_timeout: TimeLimit,
    pub catalog_wait_timeout: TimeLimit,
    pub shutdown_timeout: TimeLimit,
    pub auth_mode: AuthMode,
    pub compat_profile: CompatProfile,
    /// The explicit `cors_fallback` setting; [`Config::effective_cors_fallback`] applies the
    /// authentication mode's default.
    pub cors_fallback: Option<CorsFallback>,
    /// The explicit `signed_host_port` setting, whether V4 verification also accepts a canonical
    /// host without the request Host's port; loading applies the default, lenient wherever the
    /// built-in test signer is enabled.
    pub signed_host_port: Option<SignedHostPort>,
    /// The identity registry, from `identity_file` or `identities`; loaded once at startup.
    pub identities: Option<DocumentSource>,
    /// The explicit `test_signer` setting; validation selects the profile's default with
    /// [`TestSignerChoice::select`](crate::auth::TestSignerChoice::select), which loading the
    /// identity document settles in strict mode.
    pub test_signer: Option<TestSigner>,
    /// The explicit `signed_urls` setting; [`Config::effective_signed_urls`] applies the
    /// profile's default.
    pub signed_urls: Option<SignedUrls>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Info,
            listen: SocketAddr::from(([127, 0, 0, 1], 4443)),
            listen_http: None,
            grpc_listen: None,
            grpc_scheme: GrpcScheme::Auto,
            trusted_listen: None,
            unix_socket: None,
            scheme: Scheme::Auto,
            tls_cert: None,
            tls_key: None,
            tls_handshake_timeout: TimeLimit::from_millis(5_000),
            data_dir: PathBuf::from("data"),
            backend: Backend::Disk,
            memory_max_bytes: crate::memory_limit::default_budget(),
            memory_catalog_max_bytes: 16 * 1024 * 1024,
            seed_dir: None,
            default_seed_dir: None,
            seed_project: "local-project".into(),
            seed_content_type: crate::mime::SeedContentType::Extension,
            max_seed_entries: None,
            max_seed_depth: None,
            seed_watch_interval: Some(std::time::Duration::from_secs(1)),
            max_buckets_per_batch: None,
            default_location: "US".into(),
            // Cloud Storage defaults to seven days; the emulator keeps no deleted data unless asked,
            // so a local disk or RAM budget is not spent on bytes nobody will restore.
            default_soft_delete_retention_seconds: None,
            lifecycle_execution: LifecycleExecution::default(),
            buckets: None,
            buckets_mode: BucketsMode::default(),
            api_base_url: None,
            public_base_url: None,
            admin_enabled: false,
            admin_principal: None,
            admin_allowed_hosts: Vec::new(),
            trusted_principal: None,
            trusted_allowed_hosts: Vec::new(),
            local_creator_email: Some(DEFAULT_LOCAL_CREATOR_EMAIL.into()),
            runtime_endpoints: RuntimeEndpoints::default(),
            pubsub_emulator_host: None,
            event_pubsub_project_id: String::new(),
            event_pubsub_topic: String::new(),
            event_bucket: String::new(),
            event_object_prefix: String::new(),
            event_list: notifications::DEFAULT_EVENT_LIST.into(),
            xml_virtual_host_domains: VirtualHostDomains::default(),
            max_connections: 1024,
            max_grpc_streams: DEFAULT_MAX_GRPC_STREAMS,
            max_object_bytes: setting::MAX_OBJECT_BYTES,
            max_resumable_sessions: None,
            max_rewrite_sessions: None,
            worker_threads: default_worker_threads(),
            // The catalog keeps one blocking thread busy (including its commit fsync);
            // the rest serve tokio::fs reads, writes and syncs for every connection, so
            // one slow fsync must not stall all other transfers. Still bounded.
            max_blocking_threads: 8,
            header_read_timeout: TimeLimit::from_millis(30_000),
            body_inactivity_timeout: TimeLimit::DISABLED,
            // Longer than the catalog wait, so a waiting request reports that wait's own failure.
            request_timeout: TimeLimit::from_millis(180_000),
            keep_alive_timeout: TimeLimit::from_millis(120_000),
            // Accepting 5 TiB objects rules out a fixed bound on a whole transfer.
            object_request_timeout: TimeLimit::DISABLED,
            catalog_wait_timeout: TimeLimit::from_millis(120_000),
            shutdown_timeout: TimeLimit::from_millis(5_000),
            auth_mode: AuthMode::Permissive,
            compat_profile: CompatProfile::Native,
            cors_fallback: None,
            signed_host_port: None,
            identities: None,
            test_signer: None,
            signed_urls: None,
        }
    }
}

impl Config {
    pub const HEALTHCHECK_PATH: &str = "/_internal/healthcheck";

    /// Whether signed XML requests verify, after the profile's default.
    pub fn effective_signed_urls(&self) -> SignedUrls {
        SignedUrls::resolve(self.signed_urls, self.auth_mode)
    }

    /// XML/public CORS for buckets without stored rules, after the authentication mode's
    /// default. Derived from the final mode, so a configuration built in code agrees with one
    /// that validation produced.
    pub fn effective_cors_fallback(&self) -> CorsFallbackChoice {
        CorsFallbackChoice::select(self.cors_fallback, self.auth_mode)
    }

    pub fn healthcheck_url(&self) -> String {
        format!("{}{}", self.local_origin(), Self::HEALTHCHECK_PATH)
    }

    /// The origin through which a client on this host reaches the server, which the healthcheck
    /// probes.
    pub fn local_origin(&self) -> String {
        let listen = loopback_if_unspecified(self.effective_listen_http().unwrap_or(self.listen));
        // Plain HTTP wherever a listener accepts it, so the client needs no CA.
        let scheme = match self.scheme {
            Scheme::Auto | Scheme::Http | Scheme::Both => "http",
            Scheme::Https => "https",
        };
        format!("{scheme}://{listen}")
    }

    pub fn effective_listen_http(&self) -> Option<SocketAddr> {
        (self.scheme == Scheme::Both).then(|| {
            self.listen_http
                .unwrap_or_else(|| SocketAddr::new(self.listen.ip(), 8000))
        })
    }

    pub fn effective_api_base_url(&self, bound: SocketAddr) -> String {
        self.api_base_url.clone().unwrap_or_else(|| {
            format!(
                "{}://{}",
                self.scheme.primary(),
                loopback_if_unspecified(SocketAddr::new(bound.ip(), bound.port()))
            )
        })
    }

    pub fn public_virtual_host_enabled(&self) -> bool {
        self.public_base_url
            .as_ref()
            .is_some_and(|origin| origin.dns_name().is_some())
    }

    /// `tls_cert` and `tls_key` name one identity, so one without the other is an error rather
    /// than a fallback to the development identity.
    pub(crate) fn tls_files(&self) -> Result<TlsFiles<'_>, UnpairedTlsFile> {
        match (&self.tls_cert, &self.tls_key) {
            (None, None) => Ok(TlsFiles::Development),
            (Some(certificate), Some(key)) => Ok(TlsFiles::Provided { certificate, key }),
            (Some(_), None) => Err(UnpairedTlsFile(Setting::TlsCert)),
            (None, Some(_)) => Err(UnpairedTlsFile(Setting::TlsKey)),
        }
    }

    pub fn load(args: &[String], env: &BTreeMap<String, String>) -> Result<Self, Diagnostics> {
        let report = Self::preflight(args, env);
        if report.diagnostics.is_err() {
            Err(report.diagnostics)
        } else {
            Ok(report.config)
        }
    }

    pub(crate) fn preflight(
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> ConfigurationReport {
        let mut inputs = Inputs::default();
        inputs.cli(args, CliCommand::Serve);
        inputs.environment(env);
        Self::from_inputs(inputs)
    }

    pub fn preflight_os(
        args: &[String],
        env: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    ) -> ConfigurationReport {
        let mut inputs = Inputs::default();
        inputs.cli(args, CliCommand::Serve);
        inputs.environment_os(env);
        Self::from_inputs(inputs)
    }
}
