use super::*;

use super::assemble::{Listeners, Tls, api_base};
use crate::url::validate_dns_name;

#[derive(Clone, Debug)]
pub(crate) struct ConfigDraft {
    pub(crate) log_level: LogLevel,
    pub(crate) listen: SocketAddr,
    pub(crate) listen_http: Option<SocketAddr>,
    pub(crate) grpc_listen: Option<SocketAddr>,
    /// How `grpc_listen` secures its connections; validation requires `grpc_listen` with it.
    pub(crate) grpc_scheme: GrpcScheme,
    /// A plain HTTP listener for the application's own servers on a private network, where a
    /// request without credentials acts as `trusted_principal`. Strict mode only.
    pub(crate) trusted_listen: Option<SocketAddr>,
    pub(crate) unix_socket: Option<PathBuf>,
    pub(crate) scheme: Scheme,
    pub(crate) tls_cert: Option<PathBuf>,
    pub(crate) tls_key: Option<PathBuf>,
    pub(crate) tls_handshake_timeout: TimeLimit,
    pub(crate) data_dir: PathBuf,
    pub(crate) backend: Backend,
    pub(crate) memory_max_bytes: u64,
    pub(crate) memory_catalog_max_bytes: u64,
    pub(crate) seed_dir: Option<PathBuf>,
    /// Fallback seed root used only when it exists and is disjoint from the data directory.
    pub(crate) default_seed_dir: Option<PathBuf>,
    pub(crate) seed_project: String,
    pub(crate) seed_content_type: crate::mime::SeedContentType,
    /// Files and directories a seed tree may hold; `None` stops at none. A seed is one atomic
    /// publication, so its memory grows with the tree until it commits.
    pub(crate) max_seed_entries: Option<NonZeroUsize>,
    /// Directory levels a seed tree may nest below a bucket; `None` leaves that to the object
    /// name's 1024 bytes.
    pub(crate) max_seed_depth: Option<NonZeroUsize>,
    /// How long the passes that replay `seed_dir` while the server runs wait between each other;
    /// `None` replays the tree only at startup.
    pub(crate) seed_watch_interval: Option<std::time::Duration>,
    /// Buckets one initialization file or one seed may declare; other buckets of the server do
    /// not count, and `None` stops at none. Either is one atomic publication, so its memory grows
    /// with the batch.
    pub(crate) max_buckets_per_batch: Option<NonZeroUsize>,
    pub(crate) default_location: String,
    /// Soft delete retention given to a new bucket that names no policy; `None` gives it none.
    pub(crate) default_soft_delete_retention_seconds: Option<NonZeroU64>,
    pub(crate) lifecycle_execution: LifecycleExecution,
    /// The bucket initialization document, from `buckets_file` or `buckets`.
    pub(crate) buckets: Option<DocumentSource>,
    /// What the buckets document does to existing buckets; it governs both document settings.
    pub(crate) buckets_mode: BucketsMode,
    pub(crate) api_base_url: Option<String>,
    pub(crate) public_base_url: Option<PublicOrigin>,
    /// Local management routes are disabled unless the operator explicitly enables them.
    pub(crate) admin_enabled: bool,
    pub(crate) admin_principal: Option<String>,
    pub(crate) admin_allowed_hosts: Vec<String>,
    /// The registered identity that requests without credentials on `trusted_listen` act as.
    pub(crate) trusted_principal: Option<String>,
    /// Dotted DNS names the trusted listener also accepts as Host, beyond IP literals,
    /// `localhost` names and single-label names.
    pub(crate) trusted_allowed_hosts: Vec<String>,
    /// Email recorded as the creator (owner and ACL OWNER entry) of objects written without
    /// authentication; `None` disables it, as the setting's empty value asks. Never an
    /// authorization input.
    pub(crate) local_creator_email: Option<String>,
    pub(crate) runtime_endpoints: RuntimeEndpoints,
    pub(crate) pubsub_emulator_host: Option<String>,
    pub(crate) event_pubsub_project_id: String,
    pub(crate) event_pubsub_topic: String,
    pub(crate) event_bucket: String,
    pub(crate) event_object_prefix: String,
    pub(crate) event_list: String,
    pub(crate) xml_virtual_host_domains: VirtualHostDomains,
    pub(crate) max_connections: usize,
    /// Streams each gRPC connection may run at once. Each can buffer a request message of up to
    /// 4 MiB and its encoded response, so lowering this limits a connection's memory.
    pub(crate) max_grpc_streams: NonZeroU32,
    pub(crate) max_object_bytes: u64,
    /// Active resumable uploads a server keeps; `None` leaves them unlimited, since expiry
    /// reclaims the disk they use.
    pub(crate) max_resumable_sessions: Option<NonZeroU32>,
    /// Unfinished rewrites a server keeps; `None` leaves them unlimited.
    pub(crate) max_rewrite_sessions: Option<NonZeroU32>,
    pub(crate) worker_threads: usize,
    pub(crate) max_blocking_threads: usize,
    // Cloud Storage documents no seconds for these and asks for "reasonably long" upload
    // timeouts, so the defaults are local choices that a slow client should not reach.
    /// Waiting for a request head: on a new connection, and once a next request has started.
    pub(crate) header_read_timeout: TimeLimit,
    /// Silence while a request body on any route or a response body is in flight.
    pub(crate) body_inactivity_timeout: TimeLimit,
    /// A handler's time before its response starts, apart from waits for request body bytes.
    pub(crate) request_timeout: TimeLimit,
    pub(crate) keep_alive_timeout: TimeLimit,
    pub(crate) object_request_timeout: TimeLimit,
    pub(crate) catalog_wait_timeout: TimeLimit,
    pub(crate) shutdown_timeout: TimeLimit,
    pub(crate) auth_mode: AuthMode,
    pub(crate) compat_profile: CompatProfile,
    /// The explicit `cors_fallback` setting; [`Config::cors_fallback`] applies the
    /// authentication mode's default.
    pub(crate) cors_fallback: Option<CorsFallback>,
    /// The explicit `signed_host_port` setting, whether V4 verification also accepts a canonical
    /// host without the request Host's port; loading applies the default, lenient wherever the
    /// built-in test signer is enabled.
    pub(crate) signed_host_port: Option<SignedHostPort>,
    /// The identity registry, from `identity_file` or `identities`; loaded once at startup.
    pub(crate) identities: Option<DocumentSource>,
    /// The explicit `test_signer` setting; validation selects the profile's default with
    /// [`TestSignerChoice::select`](crate::auth::TestSignerChoice::select), which loading the
    /// identity document settles in strict mode.
    pub(crate) test_signer: Option<TestSigner>,
    /// The explicit `signed_urls` setting; [`Config::signed_urls`] applies the
    /// profile's default.
    pub(crate) signed_urls: Option<SignedUrls>,
}

impl Default for ConfigDraft {
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

impl ConfigDraft {
    pub(super) fn resolved_listen_http(&self) -> Option<SocketAddr> {
        (self.scheme == Scheme::Both).then(|| {
            self.listen_http
                .unwrap_or_else(|| SocketAddr::new(self.listen.ip(), 8000))
        })
    }

    pub(super) fn resolved_signed_urls(&self) -> SignedUrls {
        SignedUrls::resolve(self.signed_urls, self.auth_mode)
    }

    pub(super) fn resolved_cors_fallback(&self) -> CorsFallbackChoice {
        CorsFallbackChoice::select(self.cors_fallback, self.auth_mode)
    }

    pub(super) fn grpc_security_scheme(&self) -> Option<GrpcScheme> {
        self.grpc_listen.map(|_| self.grpc_scheme)
    }

    pub(super) fn from_inputs(mut inputs: Inputs) -> ConfigurationReport {
        let invalid_file_fields = read_selected_config(&mut inputs);
        inputs.resolve_server_flags();
        let settings = collect_settings(&mut inputs);
        let mut report = Validation {
            draft: Self::default(),
            diagnostics: inputs.diagnostics,
            invalid: invalid_file_fields
                .into_iter()
                .filter(|key| !settings.contains_key(key))
                .collect(),
            auth_profile: None,
        };
        report.apply_settings(&settings);
        report.draft.runtime_endpoints =
            RuntimeEndpoints::new(report.draft.xml_virtual_host_domains.clone());
        report.check_document_sources(&settings);
        report.check_buckets_mode(&settings);
        report.check_profile_settings(&settings);
        report.check_backend(&settings);
        let tls = report.check_tls(&settings);
        let listeners = report.check_listeners(&settings);
        report.check_grpc_settings(&settings);
        report.check_public_base_url(&settings);
        report.check_api_base_url(&settings);
        report.check_auth(&settings);
        report.check_trusted_listener(&settings);
        let notifications = report.check_notifications(&settings);
        report.explain_empty_environment_values(&settings);
        ConfigurationReport {
            draft: report.draft,
            diagnostics: report.diagnostics,
            invalid: report.invalid,
            auth_profile: report.auth_profile,
            settings,
            tls,
            listeners,
            notifications,
        }
    }
}

/// Effective settings, after config file, environment and CLI precedence.
pub(super) type Settings = BTreeMap<Setting, Input>;

/// Reads the config file selected by `--config` or `GCS_MIMIC_CONFIG` into `inputs` and
/// returns the settings whose file values had an unsupported JSON type. A member that names no
/// setting is still diagnosed, but no check asks whether it is valid, so it is not returned.
fn read_selected_config(inputs: &mut Inputs) -> BTreeSet<Setting> {
    let mut invalid_file_fields = BTreeSet::new();
    let selector = inputs
        .entries
        .iter()
        .filter(|i| {
            i.name == "config"
                && i.kind == InputKind::Native
                && i.source != DiagnosticSource::Config
        })
        .max_by_key(|i| i32::from(i.source == DiagnosticSource::Cli))
        .cloned();
    let Some(selector) = selector.filter(|input| input.invalid.is_none()) else {
        return invalid_file_fields;
    };
    match read_config(&selector.value) {
        Ok(values) => {
            for (key, value) in values {
                let setting = key.parse::<Setting>().ok();
                let text = member_text(setting, &value).or_else(|| {
                    rejection::WithdrawnSetting::is_withdrawn(&key).then(|| value.get().to_owned())
                });
                // A member of an unsupported type still counts against the input limits.
                let Some(text) = text else {
                    if !inputs.admit(DiagnosticSource::Config, &key) {
                        break;
                    }
                    invalid_file_fields.extend(setting);
                    inputs.diagnostics.errors.push(Diagnostic::new(
                        DiagnosticSource::Config,
                        key,
                        member_fix(setting),
                    ));
                    continue;
                };
                if !inputs.push(Input {
                    source: DiagnosticSource::Config,
                    key: key.clone(),
                    name: key,
                    value: text,
                    kind: InputKind::Native,
                    invalid: None,
                }) {
                    break;
                }
            }
        }
        Err(error) => inputs.diagnostics.errors.push(Diagnostic::new(
            selector.source,
            selector.key,
            error.fix(if selector.source == DiagnosticSource::Environment {
                "GCS_MIMIC_CONFIG=config.json"
            } else {
                "--config PATH"
            }),
        )),
    }
    invalid_file_fields
}

/// Merges native inputs and translated aliases by precedence (config file, then environment,
/// then CLI) and diagnoses duplicate CLI options and settings that are unknown or not accepted
/// from their source.
fn collect_settings(inputs: &mut Inputs) -> Settings {
    let mut settings = BTreeMap::new();
    let mut seen_cli = BTreeSet::new();
    for source in [
        DiagnosticSource::Config,
        DiagnosticSource::Environment,
        DiagnosticSource::Cli,
    ] {
        for input in inputs.entries.iter().filter(|i| {
            i.source == source
                && !i.shadowed(&inputs.entries)
                && match i.kind {
                    InputKind::Native | InputKind::Translated => true,
                    InputKind::NativeFlag(_) | InputKind::Alias(_) | InputKind::Refused(_) => false,
                }
        }) {
            if source == DiagnosticSource::Cli
                && input.kind == InputKind::Native
                && !seen_cli.insert(&input.name)
            {
                inputs.diagnostics.errors.push(Diagnostic::new(
                    source,
                    &input.key,
                    "Duplicate option; supply each --name VALUE option exactly once.",
                ));
            }
            match input.setting() {
                Ok(setting) => {
                    settings.insert(setting, input.clone());
                }
                Err(rejection) => inputs.diagnostics.errors.push(Diagnostic::new(
                    source,
                    &input.key,
                    rejection.to_string(),
                )),
            }
        }
    }
    settings
}

/// Parses one setting into `config`. A rejected value may leave `config` partly updated, so the
/// caller applies it to a candidate copy and keeps the copy only on success.
/// `value` is borrowed: an inline document may be 1 GiB, and only the setting that stores it
/// needs its own copy.
fn apply_setting(
    config: &mut ConfigDraft,
    setting: Setting,
    value: &str,
) -> Result<(), SettingFix> {
    let key = setting.as_str();
    match setting {
        // A configuration file selects where the others come from; it never holds a value.
        Setting::Config => return Err(unknown_setting()),
        // Listeners and transport.
        Setting::LogLevel => config.log_level = value.parse()?,
        Setting::Scheme => config.scheme = value.parse()?,
        Setting::Listen => config.listen = socket_address(value, LISTEN_FIX)?,
        Setting::ListenHttp => config.listen_http = Some(socket_address(value, LISTEN_HTTP_FIX)?),
        Setting::GrpcListen => config.grpc_listen = Some(socket_address(value, GRPC_LISTEN_FIX)?),
        Setting::GrpcScheme => config.grpc_scheme = value.parse()?,
        Setting::TrustedListen => {
            config.trusted_listen = Some(socket_address(value, TRUSTED_LISTEN_FIX)?);
        }
        Setting::UnixSocket => config.unix_socket = Some(PathBuf::from(non_empty(key, value)?)),
        Setting::TlsCert => config.tls_cert = Some(PathBuf::from(non_empty(key, value)?)),
        Setting::TlsKey => config.tls_key = Some(PathBuf::from(non_empty(key, value)?)),
        Setting::TlsHandshakeTimeoutMs => {
            config.tls_handshake_timeout = setting.time_limit(value)?
        }
        // Endpoints.
        Setting::ApiBaseUrl => config.api_base_url = Some(api_base_url(value)?),
        Setting::PublicBaseUrl => config.public_base_url = Some(public_base_url(value)?),
        Setting::XmlVirtualHostDomains => {
            config.xml_virtual_host_domains = virtual_host_domains(value)?;
        }
        // Storage and its directories.
        Setting::Backend => config.backend = value.parse()?,
        Setting::MemoryMaxBytes => config.memory_max_bytes = setting.integer(value)?,
        Setting::MemoryCatalogMaxBytes => {
            config.memory_catalog_max_bytes = setting.integer(value)?
        }
        Setting::DataDir => config.data_dir = PathBuf::from(non_empty(key, value)?),
        Setting::SeedDir => config.seed_dir = Some(PathBuf::from(non_empty(key, value)?)),
        Setting::DefaultSeedDir => {
            config.default_seed_dir = Some(PathBuf::from(non_empty(key, value)?));
        }
        Setting::SeedProject => config.seed_project = seed_project(value)?,
        Setting::SeedContentType => config.seed_content_type = value.parse()?,
        Setting::DefaultLocation => config.default_location = default_location(value)?,
        Setting::DefaultSoftDeleteRetentionSeconds => {
            config.default_soft_delete_retention_seconds = soft_delete_retention(value)?;
        }
        Setting::LifecycleExecution => config.lifecycle_execution = value.parse()?,
        Setting::BucketsMode => config.buckets_mode = value.parse()?,
        Setting::BucketsFile => DocumentKind::Buckets.apply(config, Form::File, value)?,
        Setting::Buckets => DocumentKind::Buckets.apply(config, Form::Inline, value)?,
        Setting::IdentityFile => DocumentKind::Identities.apply(config, Form::File, value)?,
        Setting::Identities => DocumentKind::Identities.apply(config, Form::Inline, value)?,
        // Access.
        Setting::AdminEnabled => {
            config.admin_enabled = value
                .parse()
                .map_err(|_| "admin_enabled must be true or false")?;
        }
        Setting::AdminPrincipal => config.admin_principal = Some(principal(setting, value)?),
        Setting::AdminAllowedHosts => config.admin_allowed_hosts = allowed_hosts(setting, value)?,
        Setting::TrustedPrincipal => config.trusted_principal = Some(principal(setting, value)?),
        Setting::TrustedAllowedHosts => {
            config.trusted_allowed_hosts = allowed_hosts(setting, value)?;
        }
        Setting::LocalCreatorEmail => config.local_creator_email = local_creator_email(value)?,
        Setting::AuthMode => config.auth_mode = value.parse()?,
        Setting::CompatProfile => config.compat_profile = value.parse()?,
        Setting::CorsFallback => config.cors_fallback = Some(value.parse()?),
        Setting::SignedHostPort => config.signed_host_port = Some(value.parse()?),
        Setting::TestSigner => config.test_signer = Some(value.parse()?),
        Setting::SignedUrls => config.signed_urls = Some(value.parse()?),
        // Notifications.
        Setting::PubsubEmulatorHost => {
            config.pubsub_emulator_host = notifications::emulator_host(value)?;
        }
        Setting::EventPubsubProjectId => value.clone_into(&mut config.event_pubsub_project_id),
        Setting::EventPubsubTopic => value.clone_into(&mut config.event_pubsub_topic),
        Setting::EventBucket => value.clone_into(&mut config.event_bucket),
        Setting::EventObjectPrefix => value.clone_into(&mut config.event_object_prefix),
        Setting::EventList => value.clone_into(&mut config.event_list),
        // Limits; each checks the setting's bounds before it converts the value.
        Setting::MaxConnections => config.max_connections = count(setting, value)?,
        Setting::MaxGrpcStreams => config.max_grpc_streams = grpc_streams(setting, value)?,
        Setting::MaxObjectBytes => config.max_object_bytes = setting.integer(value)?,
        Setting::MaxResumableSessions => {
            config.max_resumable_sessions = session_limit(setting, value)?;
        }
        Setting::MaxRewriteSessions => config.max_rewrite_sessions = session_limit(setting, value)?,
        Setting::MaxSeedEntries => config.max_seed_entries = tree_limit(setting, value)?,
        Setting::MaxSeedDepth => config.max_seed_depth = tree_limit(setting, value)?,
        Setting::SeedWatchIntervalMs => {
            config.seed_watch_interval = NonZeroU64::new(setting.integer(value)?)
                .map(|millis| std::time::Duration::from_millis(millis.get()));
        }
        Setting::MaxBucketsPerBatch => config.max_buckets_per_batch = tree_limit(setting, value)?,
        Setting::WorkerThreads => config.worker_threads = count(setting, value)?,
        Setting::MaxBlockingThreads => config.max_blocking_threads = count(setting, value)?,
        // Timeouts.
        Setting::HeaderReadTimeoutMs => config.header_read_timeout = setting.time_limit(value)?,
        Setting::BodyInactivityTimeoutMs => {
            config.body_inactivity_timeout = setting.time_limit(value)?;
        }
        Setting::RequestTimeoutMs => config.request_timeout = setting.time_limit(value)?,
        Setting::KeepAliveTimeoutMs => config.keep_alive_timeout = setting.time_limit(value)?,
        Setting::ObjectRequestTimeoutMs => {
            config.object_request_timeout = setting.time_limit(value)?;
        }
        Setting::CatalogWaitTimeoutMs => config.catalog_wait_timeout = setting.time_limit(value)?,
        Setting::ShutdownTimeoutMs => config.shutdown_timeout = setting.time_limit(value)?,
    }
    Ok(())
}

pub(super) fn unknown_setting() -> SettingFix {
    setting::UNKNOWN_SETTING.into()
}

const LISTEN_FIX: &str = "listen must be an IP:port socket address";
const LISTEN_HTTP_FIX: &str =
    "listen_http must be an IP socket address, for example 127.0.0.1:8000";
const GRPC_LISTEN_FIX: &str =
    "grpc_listen must be an IP socket address; use 127.0.0.1:4444 for a separate gRPC port";
const TRUSTED_LISTEN_FIX: &str =
    "trusted_listen must be an IP socket address, for example 0.0.0.0:4444 inside a container";

fn socket_address(value: &str, fix: &'static str) -> Result<SocketAddr, SettingFix> {
    value.parse().map_err(|_| fix.into())
}

fn api_base_url(value: &str) -> Result<String, SettingFix> {
    crate::url::normalize_base_url(value).map_err(|error| format!("api_base_url: {error}").into())
}

fn public_base_url(value: &str) -> Result<PublicOrigin, SettingFix> {
    PublicOrigin::parse(value).map_err(|error| format!("public_base_url: {error}").into())
}

/// The comma-separated base domains; unlike `public_base_url`, the setting may not repeat one.
fn virtual_host_domains(value: &str) -> Result<VirtualHostDomains, SettingFix> {
    let mut domains = VirtualHostDomains::default();
    if value.is_empty() {
        return Ok(domains);
    }
    for domain in value.split(',') {
        let domain = crate::net::normalized_host(domain.trim(), crate::net::TrailingDot::Preserve);
        validate_dns_name(&domain).map_err(|error| error.to_string())?;
        match domains.insert(domain) {
            Ok(Inserted::Added) => {}
            Ok(Inserted::AlreadyListed) => {
                return Err(
                    "duplicate xml_virtual_host_domains entry; remove repeated DNS names".into(),
                );
            }
            Err(DomainLimit) => {
                return Err("xml_virtual_host_domains must contain at most 32 DNS names".into());
            }
        }
    }
    Ok(domains)
}

fn seed_project(value: &str) -> Result<String, SettingFix> {
    crate::auth::validate_project_id(value)
        .map_err(|_| format!("seed_project: {}", crate::auth::InvalidProjectId::MESSAGE))?;
    Ok(value.to_owned())
}

/// An empty value selects Cloud Storage's default location.
fn default_location(value: &str) -> Result<String, SettingFix> {
    let value = if value.is_empty() { "US" } else { value };
    Ok(value
        .parse::<Location>()
        .map_err(
            |_| "default_location must be a supported bucket location; use US or ASIA-NORTHEAST1",
        )?
        .as_str()
        .to_owned())
}

fn soft_delete_retention(value: &str) -> Result<Option<NonZeroU64>, SettingFix> {
    Ok(value
        .parse::<i64>()
        .ok()
        .and_then(|seconds| crate::bucket::SoftDeleteRetention::try_from(seconds).ok())
        .ok_or("default_soft_delete_retention_seconds must be 0 (no soft delete) or 1..=7776000 seconds (90 days); use 604800 for the Cloud Storage default of seven days")?
        .enabled())
}

/// `admin_principal` and `trusted_principal` name identities, so both follow identity id rules.
fn principal(setting: Setting, value: &str) -> Result<String, SettingFix> {
    if !crate::auth::valid_principal_id(value) {
        return Err(format!(
            "{} must be 1..=254 bytes of printable ASCII without whitespace, like identity ids",
            setting.as_str()
        )
        .into());
    }
    Ok(value.to_owned())
}

/// An empty value disables recording a creator.
fn local_creator_email(value: &str) -> Result<Option<String>, SettingFix> {
    if value.is_empty() {
        return Ok(None);
    }
    crate::auth::validate_local_creator_email(value).map_err(|reason| {
        format!(
            "local_creator_email {reason}; use an address such as \
             local-user@gcs-mimic.invalid or an empty value to disable it"
        )
    })?;
    Ok(Some(value.to_owned()))
}

/// The DNS names an allow-list `setting` names, normalized as request hosts compare.
fn allowed_hosts(setting: Setting, value: &str) -> Result<Vec<String>, SettingFix> {
    let name = setting.as_str();
    let mut hosts = Vec::new();
    if value.is_empty() {
        return Ok(hosts);
    }
    for host in value.split(',') {
        let host = crate::net::normalized_host(host.trim(), crate::net::TrailingDot::Remove);
        validate_dns_name(&host).map_err(|_| {
            format!("{name} requires comma-separated DNS names without ports or schemes")
        })?;
        if hosts.len() == 32 {
            return Err(format!("{name} supports at most 32 names").into());
        }
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    Ok(hosts)
}

fn count(setting: Setting, value: &str) -> Result<usize, SettingFix> {
    let value = setting.integer(value)?;
    usize::try_from(value)
        .map_err(|_| format!("{} exceeds this platform's count limit", setting.as_str()).into())
}

/// The setting's bounds keep this in range; the conversion records that in the type.
fn grpc_streams(setting: Setting, value: &str) -> Result<NonZeroU32, SettingFix> {
    u32::try_from(setting.integer(value)?)
        .ok()
        .and_then(NonZeroU32::new)
        .ok_or_else(unknown_setting)
}

/// The one place 0 means unlimited: past this point the absence of a limit is `None`.
fn session_limit(setting: Setting, value: &str) -> Result<Option<NonZeroU32>, SettingFix> {
    let value = setting.integer(value)?;
    u32::try_from(value)
        .map(NonZeroU32::new)
        .map_err(|_| format!("{} must fit in 32 bits", setting.as_str()).into())
}

/// 0 means unlimited here too.
fn tree_limit(setting: Setting, value: &str) -> Result<Option<NonZeroUsize>, SettingFix> {
    count(setting, value).map(NonZeroUsize::new)
}

fn non_empty<'a>(key: &str, value: &'a str) -> Result<&'a str, SettingFix> {
    if value.is_empty() {
        return Err(format!("{key} must not be empty").into());
    }
    Ok(value)
}

pub(crate) struct ConfigurationReport {
    pub(super) draft: ConfigDraft,
    pub(crate) diagnostics: Diagnostics,
    pub(super) invalid: BTreeSet<Setting>,
    /// Set by validation only when the authentication settings are valid and agree.
    pub(super) auth_profile: Option<AuthProfile>,
    /// The inputs validation read, so that a diagnostic after loading can still name the one
    /// that caused it.
    pub(super) settings: Settings,
    pub(super) tls: Option<Tls>,
    pub(super) listeners: Listeners,
    pub(super) notifications: crate::notification::Runtime,
}

impl ConfigurationReport {
    pub(crate) fn into_diagnostics(self) -> Diagnostics {
        self.diagnostics
    }

    pub(crate) fn record_error(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.errors.push(diagnostic);
    }

    pub(crate) fn valid(&self, settings: &[Setting]) -> bool {
        settings_valid(&self.invalid, settings)
    }

    #[cfg(test)]
    pub(crate) fn draft(&self) -> &ConfigDraft {
        &self.draft
    }

    pub(crate) fn log_level(&self) -> LogLevel {
        self.draft.log_level
    }

    pub(crate) fn seed_dir(&self) -> Option<&Path> {
        self.draft.seed_dir.as_deref()
    }

    pub(crate) fn local_origin(&self) -> String {
        super::assemble::local_origin(
            self.draft.listen,
            self.draft.resolved_listen_http(),
            self.draft.scheme,
        )
    }

    pub(crate) fn resolve_default_seed(&mut self) {
        if self.default_seed_settings_valid() {
            self.diagnostics
                .warnings
                .extend(crate::config_input::resolve_default_seed(&mut self.draft));
        }
    }

    /// Whether the settings that select the TLS identity are valid, so that loading it can fail
    /// only on the files themselves.
    pub(crate) fn tls_settings_valid(&self) -> bool {
        self.valid(&[
            Setting::Scheme,
            Setting::GrpcListen,
            Setting::GrpcScheme,
            Setting::TlsCert,
            Setting::TlsKey,
        ])
    }

    /// Whether the directories that adopting `default_seed_dir` compares are valid.
    pub(crate) fn default_seed_settings_valid(&self) -> bool {
        self.valid(&[Setting::DataDir, Setting::SeedDir, Setting::DefaultSeedDir])
    }

    pub(crate) fn load_tls(&mut self) -> crate::tls::ServerSecurity {
        if !self.tls_settings_valid() {
            return crate::tls::ServerSecurity::plain();
        }
        match crate::tls::ServerSecurity::load_settings(
            self.draft.scheme,
            self.draft.unix_socket.as_deref(),
            self.draft.grpc_security_scheme(),
            self.tls.as_ref(),
        ) {
            Ok(security) => security,
            Err(error) => {
                self.diagnostics.errors.push(error.diagnostic());
                crate::tls::ServerSecurity::plain()
            }
        }
    }
}

pub(super) struct Validation {
    pub(super) draft: ConfigDraft,
    pub(super) diagnostics: Diagnostics,
    pub(super) invalid: BTreeSet<Setting>,
    pub(super) auth_profile: Option<AuthProfile>,
}

fn settings_valid(invalid: &BTreeSet<Setting>, settings: &[Setting]) -> bool {
    settings.iter().all(|setting| !invalid.contains(setting))
}

impl Validation {
    pub(super) fn valid(&self, settings: &[Setting]) -> bool {
        settings_valid(&self.invalid, settings)
    }

    pub(super) fn any_invalid(&self, settings: &[Setting]) -> bool {
        !self.valid(settings)
    }

    fn apply_settings(&mut self, settings: &Settings) {
        for (&setting, input) in settings {
            if let Some(invalid) = input.invalid {
                self.invalid.insert(setting);
                if invalid == InputError::Encoding {
                    self.diagnostics.errors.push(Diagnostic::new(
                        input.source,
                        &input.key,
                        "Setting values must be valid UTF-8; correct the environment value.",
                    ));
                }
                continue;
            }
            if setting == Setting::Config {
                continue;
            }
            let mut candidate = self.draft.clone();
            match apply_setting(&mut candidate, setting, &input.value) {
                Ok(()) => self.draft = candidate,
                Err(SettingFix(fix)) => {
                    self.invalid.insert(setting);
                    self.diagnostics
                        .errors
                        .push(Diagnostic::new(input.source, &input.key, fix));
                }
            }
        }
    }

    /// An empty value stays an error so that a missing substitution cannot quietly select a
    /// default such as the permissive auth mode. Compose's `${VAR:-}` looks like leaving a setting
    /// out but passes an empty string, so the refusal names the form that does leave it out.
    fn explain_empty_environment_values(&mut self, settings: &Settings) {
        let empty_variables: BTreeSet<&str> = settings
            .values()
            .filter(|input| {
                input.source == DiagnosticSource::Environment
                    && input.value.is_empty()
                    && input.invalid.is_none()
            })
            .map(|input| input.key.as_str())
            .collect();
        for error in &mut self.diagnostics.errors {
            if error.source == DiagnosticSource::Environment
                && empty_variables.contains(error.key.as_str())
            {
                error.fix = format!(
                    "{}. An empty value does not mean unset: remove {key}, or in Compose write \
                     `{key}:` with no value to pass it only when it is set.",
                    error.fix.trim_end_matches('.'),
                    key = error.key
                );
            }
        }
    }

    /// The mode only changes what a buckets document does, so naming it without one is a mistake.
    fn check_buckets_mode(&mut self, settings: &Settings) {
        if self.draft.buckets.is_none()
            && self.valid(&[Setting::BucketsFile, Setting::Buckets])
            && let Some(input) = settings.get(&Setting::BucketsMode)
            && self.invalid.insert(Setting::BucketsMode)
        {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "This setting requires buckets_file or buckets; supply --buckets-file PATH or the document itself with --buckets JSON, or remove this setting.",
            ));
        }
    }

    /// Settings that the compatibility profile or the authentication mode leaves without effect.
    fn check_profile_settings(&mut self, settings: &Settings) {
        let config = &self.draft;
        if config.compat_profile == CompatProfile::FakeGcsServer
            && let Some(input) = settings.get(&Setting::DefaultLocation)
            && !self.invalid.contains(&Setting::DefaultLocation)
        {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "default_location requires compat_profile=native; use --compat-profile native --default-location EU or remove the default_location setting.",
            ));
            self.invalid.insert(Setting::DefaultLocation);
        }
        if config.compat_profile == CompatProfile::FakeGcsServer
            && config.local_creator_email.is_some()
            && let Some(input) = settings.get(&Setting::LocalCreatorEmail)
            && !self.invalid.contains(&Setting::LocalCreatorEmail)
        {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "local_creator_email requires compat_profile=native; the fake-gcs-server profile never records a creator. Use --compat-profile native or remove the setting.",
            ));
            self.invalid.insert(Setting::LocalCreatorEmail);
        }
        // Strict mode records the registered caller, so a creator address would be silently unused.
        // A permissive identity document that only verifies signatures leaves the creator in use.
        if (config.auth_mode == AuthMode::Strict
            || (config.identities.is_some() && config.resolved_signed_urls() == SignedUrls::Ignore))
            && config.local_creator_email.is_some()
            && let Some(input) = settings.get(&Setting::LocalCreatorEmail)
            && !self.invalid.contains(&Setting::LocalCreatorEmail)
        {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "local_creator_email applies only to auth_mode=permissive; strict mode records the registered caller as owner. Remove the setting or use permissive mode.",
            ));
            self.invalid.insert(Setting::LocalCreatorEmail);
        }
        // Zero is what the fake-gcs-server profile does anyway, so only a positive request conflicts.
        if config.compat_profile == CompatProfile::FakeGcsServer
            && config.default_soft_delete_retention_seconds.is_some()
            && let Some(input) = settings.get(&Setting::DefaultSoftDeleteRetentionSeconds)
            && !self
                .invalid
                .contains(&Setting::DefaultSoftDeleteRetentionSeconds)
        {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "default_soft_delete_retention_seconds requires compat_profile=native; the fake-gcs-server profile never applies a default soft delete policy. Use --compat-profile native or remove the setting.",
            ));
            self.invalid
                .insert(Setting::DefaultSoftDeleteRetentionSeconds);
        }
    }

    fn check_tls(&mut self, settings: &Settings) -> Option<Tls> {
        let config = &self.draft;
        // Without a listener that serves TLS every TLS setting would be silently ignored. The
        // gRPC listener uses the same identity, so it alone gives them an effect.
        // A Unix socket, which is never inspected, excludes every TCP listener, so no scheme or
        // gRPC listener can give the settings a use beside it.
        let plain_only = (!crate::tls::serves_tls(
            config.scheme, config.unix_socket.as_deref(), config.grpc_security_scheme(),
        )).then_some(
            if config.unix_socket.is_some() {
                "A Unix socket serves plain HTTP only; remove TLS settings or listen on TCP."
            } else {
                "TLS settings require scheme=auto, https or both, or grpc_listen with grpc_scheme=auto or tls; remove TLS settings for HTTP."
            },
        );
        if let Some(fix) = plain_only {
            if self.any_invalid(&[Setting::Scheme, Setting::GrpcListen, Setting::GrpcScheme]) {
                return None;
            }
            for setting in [
                Setting::TlsCert,
                Setting::TlsKey,
                Setting::TlsHandshakeTimeoutMs,
            ] {
                if let Some(input) = settings.get(&setting)
                    && !self.invalid.contains(&setting)
                {
                    self.diagnostics
                        .errors
                        .push(Diagnostic::new(input.source, &input.key, fix));
                    self.invalid.insert(setting);
                }
            }
            return None;
        }
        let present = match Tls::select(config) {
            Ok(tls) => return Some(tls),
            Err(setting) => setting,
        };
        if !self.any_invalid(&[Setting::Scheme, Setting::TlsCert, Setting::TlsKey]) {
            if let Some(input) = settings.get(&present) {
                self.diagnostics.errors.push(Diagnostic::new(
                    input.source,
                    &input.key,
                    "tls_cert and tls_key must be supplied together; provide both PEM paths.",
                ));
            }
            self.invalid.insert(present);
        }
        None
    }

    /// Without a gRPC listener these settings would silently do nothing, and a typo in the
    /// listener's setting would hide behind them.
    fn check_grpc_settings(&mut self, settings: &Settings) {
        if settings.contains_key(&Setting::GrpcListen) {
            return;
        }
        for setting in [Setting::MaxGrpcStreams, Setting::GrpcScheme] {
            if let Some(input) = settings.get(&setting)
                && self.invalid.insert(setting)
            {
                self.diagnostics.errors.push(Diagnostic::new(
                    input.source,
                    &input.key,
                    format!(
                        "{} requires grpc_listen; remove it or set --grpc-listen, for example 127.0.0.1:4444.",
                        setting.as_str()
                    ),
                ));
            }
        }
    }

    fn check_listeners(&mut self, settings: &Settings) -> Listeners {
        let listeners = Listeners::from(&self.draft);
        if self.draft.unix_socket.is_some() {
            let fix = if !cfg!(unix) {
                Some("unix_socket requires a Unix platform; no TCP fallback is used.")
            } else if matches!(self.draft.scheme, Scheme::Https | Scheme::Both) {
                Some(
                    "unix_socket serves plain HTTP and requires scheme=auto or http; advertised HTTP(S) origins remain independent.",
                )
            } else if [
                Setting::Listen,
                Setting::ListenHttp,
                Setting::GrpcListen,
                Setting::TrustedListen,
            ]
            .iter()
            .any(|setting| settings.contains_key(setting))
            {
                Some(
                    "unix_socket cannot be combined with explicit listen, listen_http, grpc_listen or trusted_listen settings; remove the TCP settings.",
                )
            } else {
                None
            };
            if let Some(fix) = fix
                && let Some(input) = settings.get(&Setting::UnixSocket)
            {
                self.invalid.insert(Setting::UnixSocket);
                self.diagnostics
                    .errors
                    .push(Diagnostic::new(input.source, &input.key, fix));
            }
            return listeners;
        }
        if self.any_invalid(&[Setting::Scheme, Setting::Listen, Setting::ListenHttp]) {
            return listeners;
        }
        let config = &self.draft;
        let fix = if config.scheme != Scheme::Both && config.listen_http.is_some() {
            Some("listen_http requires scheme=both; remove listen_http or use --scheme both.")
        } else if config.listen.port() != 0 && config.resolved_listen_http() == Some(config.listen)
        {
            Some(
                "listen and listen_http must use different nonzero endpoints; use --listen-http 127.0.0.1:8001 or port 0.",
            )
        } else {
            None
        };
        if let Some(fix) = fix {
            let setting = if settings.contains_key(&Setting::ListenHttp) {
                Setting::ListenHttp
            } else {
                Setting::Listen
            };
            if let Some(input) = settings.get(&setting) {
                self.diagnostics
                    .errors
                    .push(Diagnostic::new(input.source, &input.key, fix));
            }
            self.invalid.insert(Setting::ListenHttp);
        }
        listeners
    }

    fn check_public_base_url(&mut self, settings: &Settings) {
        let config = &mut self.draft;
        if !self.invalid.contains(&Setting::PublicBaseUrl)
            && let Some(input) = settings.get(&Setting::PublicBaseUrl)
            && let Some(origin) = config.public_base_url.as_ref()
        {
            if let Some(domain) = origin.dns_name() {
                // The origin's domain may repeat one the list already names.
                if let Err(DomainLimit) = config.xml_virtual_host_domains.insert(domain.to_owned())
                {
                    self.diagnostics.errors.push(Diagnostic::new(input.source, &input.key,
                        "public_base_url and xml_virtual_host_domains must contain at most 32 distinct DNS names; remove a domain or reuse one already configured."));
                    self.invalid.insert(Setting::PublicBaseUrl);
                }
            } else {
                self.diagnostics.warnings.push(Diagnostic::new(input.source, &input.key,
                    "public_base_url is an IP address: virtual-host routing is disabled; use path-style URLs or a DNS origin such as http://storage.example.invalid."));
            }
        }
    }

    fn check_api_base_url(&mut self, settings: &Settings) {
        let config = &self.draft;
        if !self.any_invalid(&[
            Setting::ApiBaseUrl,
            Setting::XmlVirtualHostDomains,
            Setting::Listen,
        ]) && api_base(config.api_base_url.as_deref(), config.scheme, config.listen)
            .parse::<hyper::Uri>()
            .ok()
            .and_then(|api| api.authority().cloned())
            .is_some_and(|a| {
                crate::xml::bucket_for_authority(a.as_str(), &config.xml_virtual_host_domains)
                    .is_some()
            })
        {
            let input = settings
                .get(&Setting::ApiBaseUrl)
                .or_else(|| settings.get(&Setting::XmlVirtualHostDomains))
                .or_else(|| settings.get(&Setting::PublicBaseUrl));
            if let Some(input) = input {
                self.diagnostics.errors.push(Diagnostic::new(input.source, &input.key, "api_base_url host selects an XML virtual bucket; use a host outside xml_virtual_host_domains or configure that host itself as a base domain."));
            }
        }
    }

    fn check_auth(&mut self, settings: &Settings) {
        self.check_admin_auth(settings);
        let config = &self.draft;
        if let Some(input) = settings.get(&Setting::AdminPrincipal)
            && !self.invalid.contains(&Setting::AdminPrincipal)
        {
            let fix = if config.auth_mode != AuthMode::Strict {
                Some("admin_principal requires auth_mode=strict; remove it or select strict mode.")
            } else if !config.admin_enabled {
                Some(
                    "admin_principal requires admin_enabled=true; enable administration or remove the principal setting.",
                )
            } else {
                None
            };
            if let Some(fix) = fix {
                self.diagnostics
                    .errors
                    .push(Diagnostic::new(input.source, &input.key, fix));
                self.invalid.insert(Setting::AdminPrincipal);
            }
        }
        self.check_profile_pairing(settings);
    }

    /// The trusted listener only changes who a request without credentials acts as, which
    /// matters only where strict mode judges anonymous requests. Without `trusted_principal` it
    /// acts as the built-in application identity, but a principal without the listener would
    /// claim an effect it does not have.
    fn check_trusted_listener(&mut self, settings: &Settings) {
        self.check_trusted_allowed_hosts(settings);
        let listen = Setting::TrustedListen;
        let principal = Setting::TrustedPrincipal;
        let named = (
            settings.contains_key(&listen),
            settings.contains_key(&principal),
        );
        let collision = self.trusted_listen_collision();
        let rejected = match (self.draft.auth_mode, named, collision) {
            (_, (false, false), _) => return,
            (AuthMode::Permissive, _, _) if self.valid(&[Setting::AuthMode]) => [listen, principal]
                .map(|setting| {
                    (setting, format!("{} requires auth_mode=strict; in permissive mode every request is already allowed, so a trusted listener changes nothing. Remove it or select strict mode.", setting.as_str()))
                })
                .to_vec(),
            (AuthMode::Strict, (false, true), _) => vec![(principal, "trusted_principal requires trusted_listen; set --trusted-listen IP:PORT for the application's servers or remove the setting.".to_owned())],
            (AuthMode::Strict, (true, _), Some((setting, example))) => vec![(listen, format!("trusted_listen must differ from {}, which binds the same address; choose a port that no other listener uses, for example {example}.", setting.as_str()))],
            // An invalid auth_mode was already reported, and a valid strict listener stays.
            (AuthMode::Permissive | AuthMode::Strict, _, _) => return,
        };
        for (setting, fix) in rejected {
            if let Some(input) = settings.get(&setting)
                && self.invalid.insert(setting)
            {
                self.diagnostics
                    .errors
                    .push(Diagnostic::new(input.source, &input.key, fix));
            }
        }
    }

    /// The list only widens the Host names the trusted listener accepts, so without that
    /// listener it would claim an effect it does not have.
    fn check_trusted_allowed_hosts(&mut self, settings: &Settings) {
        if !settings.contains_key(&Setting::TrustedListen)
            && let Some(input) = settings.get(&Setting::TrustedAllowedHosts)
            && self.invalid.insert(Setting::TrustedAllowedHosts)
        {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "trusted_allowed_hosts requires trusted_listen; set --trusted-listen IP:PORT or remove the setting.",
            ));
        }
    }

    /// The listener setting whose address `trusted_listen` repeats, with an address on the same
    /// IP whose port no listener uses. Port 0 picks a free port for each listener, so only a
    /// fixed port can collide. The example starts above 4443 and 4444, the main and trusted ports
    /// that the recipes and the README already use, the second also for gRPC.
    fn trusted_listen_collision(&self) -> Option<(Setting, SocketAddr)> {
        let config = &self.draft;
        if config.unix_socket.is_some()
            || !self.valid(&[
                Setting::TrustedListen,
                Setting::Listen,
                Setting::ListenHttp,
                Setting::GrpcListen,
                Setting::Scheme,
            ])
        {
            return None;
        }
        let address = config
            .trusted_listen
            .filter(|address| address.port() != 0)?;
        let others = [
            (Setting::Listen, Some(config.listen)),
            (Setting::ListenHttp, config.resolved_listen_http()),
            (Setting::GrpcListen, config.grpc_listen),
        ];
        let (setting, _) = others.iter().find(|(_, other)| *other == Some(address))?;
        let taken = |port: u16| {
            others
                .iter()
                .any(|(_, other)| other.is_some_and(|other| other.port() == port))
        };
        let port = (4445..=u16::MAX).find(|port| !taken(*port)).unwrap_or(4445);
        Some((*setting, SocketAddr::new(address.ip(), port)))
    }

    fn check_admin_auth(&mut self, settings: &Settings) {
        if self.draft.auth_mode == AuthMode::Strict
            && self.draft.admin_enabled
            && self.draft.admin_principal.is_none()
            && let Some(input) = settings.get(&Setting::AdminEnabled)
        {
            self.diagnostics.errors.push(Diagnostic::new(input.source, &input.key,
                "admin_enabled in strict mode requires admin_principal; set --admin-principal to the registered instance administrator"));
        }
    }
}

#[cfg(test)]
impl ConfigDraft {
    pub(super) fn load(
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<Self, Diagnostics> {
        let report = Config::preflight(args, env);
        if report.diagnostics.is_err() {
            return Err(report.diagnostics);
        }
        Ok(report.draft)
    }
}
