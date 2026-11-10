#![deny(clippy::unwrap_used, clippy::expect_used)]

use crate::config_input::Fix as SettingFix;
use crate::{
    auth::{AuthMode, SignedUrls, TestSigner, signature::SignedHostPort},
    bucket::bucket_location,
    config_input::{CliCommand, Input, InputError, InputKind, Inputs},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics, LogLevel},
    time_limit::TimeLimit,
    url::PublicOrigin,
};
use serde_json::value::RawValue;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    num::{NonZeroU32, NonZeroUsize},
    path::{Path, PathBuf},
};

mod auth;
mod document;
mod notifications;
mod runtime;
mod setting;
mod storage;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod unix_tests;
#[cfg(test)]
pub(crate) use auth::Verification;
pub(crate) use auth::{AuthProfile, TrustedCaller};
pub use document::DocumentSource;
pub(crate) use document::{DocumentKind, Form};
pub use runtime::RuntimeEndpoints;
pub(crate) use runtime::{
    ApiBaseSource, ConnectionScheme, EndpointError, EndpointSnapshot, RequestEndpoints,
};
pub(crate) use setting::{MAX_OBJECT_BYTES, MAX_TREE_DEPTH, Setting};
pub use storage::Backend;

/// Reserved-domain (RFC 2606) address, so it can never name a real Google account.
pub const DEFAULT_LOCAL_CREATOR_EMAIL: &str = "local-user@gcs-mimic.invalid";

/// RFC 9113 section 6.5.2 recommends advertising no fewer than 100 concurrent streams, "so as to
/// not unnecessarily limit parallelism". A client past the limit queues its calls on its side.
const DEFAULT_MAX_GRPC_STREAMS: NonZeroU32 = NonZeroU32::new(100).unwrap();

/// One worker per CPU this process may use, which on Linux honors a container's quota. The former
/// two workers served eight concurrent 10 MiB downloads at about a third of the rate eight workers
/// did. Past eight, workers would mostly compete with the application under test, which usually
/// runs on the same CPUs.
fn default_worker_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(2, usize::from)
        .clamp(2, 8)
}

const CONFIG_VALUE_FIX: &str =
    "Expected a string or unsigned integer; use the documented flat JSON setting type.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidChoice(pub(crate) &'static str);

impl std::fmt::Display for InvalidChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for InvalidChoice {}

macro_rules! string_enum {
    ($(#[$meta:meta])* $vis:vis enum $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $value:literal),+ $(,)?
    } $error:expr) => {
        $crate::config::string_enum! { @impl [Default] $(#[$meta])* $vis enum $name {
            $($(#[$variant_meta])* $variant => $value),+
        } $error }
    };
    (no_default $(#[$meta:meta])* $vis:vis enum $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $value:literal),+ $(,)?
    } $error:expr) => {
        $crate::config::string_enum! { @impl [] $(#[$meta])* $vis enum $name {
            $($(#[$variant_meta])* $variant => $value),+
        } $error }
    };
    (@impl [$($derive:ident),*] $(#[$meta:meta])* $vis:vis enum $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $value:literal),+ $(,)?
    } $error:expr) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, $($derive),*)]
        $vis enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            $vis fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::config::InvalidChoice;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err($crate::config::InvalidChoice($error)),
                }
            }
        }
    };
}
pub(crate) use string_enum;

string_enum! {
    pub enum CompatProfile {
        #[default]
        Native => "native",
        LegacyEmulator => "legacy-emulator",
    }
    "compat_profile must be native or legacy-emulator"
}

string_enum! {
    no_default
    /// How XML/public CORS treats a bucket without stored rules. Cloud Storage has no server-wide
    /// rules, so `AllowAll` exists only as a local convenience and fake-gcs-server migration aid;
    /// see [`CorsFallbackChoice::select`] for the default.
    pub enum CorsFallback {
        None => "none",
        AllowAll => "allow-all",
    }
    "cors_fallback must be none or allow-all"
}

/// The effective CORS fallback and what chose it, since only a default that allows every origin
/// is worth noting when it lets through a request that Cloud Storage would refuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorsFallbackChoice {
    /// The `cors_fallback` setting.
    Explicit(CorsFallback),
    /// The authentication mode's default.
    Default(CorsFallback),
}

impl CorsFallbackChoice {
    /// An explicit setting wins. Otherwise permissive mode allows every origin, as
    /// fake-gcs-server does: its users want browser access without configuration, and CORS never
    /// grants access. Strict mode follows Cloud Storage, which has no fallback, so a
    /// production-like setup still catches a bucket that lacks `cors` rules.
    pub fn select(configured: Option<CorsFallback>, mode: AuthMode) -> Self {
        match (configured, mode) {
            (Some(fallback), _) => Self::Explicit(fallback),
            (None, AuthMode::Permissive) => Self::Default(CorsFallback::AllowAll),
            (None, AuthMode::Strict) => Self::Default(CorsFallback::None),
        }
    }

    pub fn fallback(self) -> CorsFallback {
        match self {
            Self::Explicit(fallback) | Self::Default(fallback) => fallback,
        }
    }
}

string_enum! {
    no_default
    /// What a start does with an existing bucket that the buckets document names; see
    /// [`BucketsFileMode::resolve`] for the default.
    pub enum BucketsFileMode {
        /// Creates missing buckets only; existing ones keep every setting.
        Create => "create",
        /// Also applies the members each entry writes to its existing bucket.
        Reconcile => "reconcile",
    }
    "buckets_file_mode must be create or reconcile"
}

impl BucketsFileMode {
    /// An explicit setting wins. An inline document lives in the file that starts the server,
    /// such as compose.yaml, which is then the source of truth, so its edits apply; a separate
    /// buckets file only creates what is missing, as it always has.
    pub(crate) fn resolve(configured: Option<Self>, form: Form) -> Self {
        configured.unwrap_or(match form {
            Form::File => Self::Create,
            Form::Inline => Self::Reconcile,
        })
    }
}

string_enum! {
    pub enum Scheme {
        Http => "http",
        #[default]
        Https => "https",
        Both => "both",
    }
    "scheme must be http, https or both"
}

impl Scheme {
    pub fn primary(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https | Self::Both => "https",
        }
    }
}

pub fn loopback_if_unspecified(mut address: SocketAddr) -> SocketAddr {
    if address.ip().is_unspecified() {
        address.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    address
}

#[derive(Clone, Debug)]
pub struct Config {
    pub log_level: LogLevel,
    pub listen: SocketAddr,
    pub listen_http: Option<SocketAddr>,
    pub grpc_listen: Option<SocketAddr>,
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
    /// Fallback seed root used only when it exists and is disjoint from data/sync directories.
    pub default_seed_dir: Option<PathBuf>,
    pub sync_dir: Option<PathBuf>,
    pub sync_delete_guard: bool,
    pub sync_reinitialize: Option<crate::storage::sync_state::CatalogId>,
    pub seed_project: String,
    pub seed_content_type: crate::mime::SeedContentType,
    /// Files and directories a seed tree may hold; `None` stops at none. A seed is one atomic
    /// publication, so its memory grows with the tree until it commits.
    pub max_seed_entries: Option<NonZeroUsize>,
    /// Directory levels a seed tree may nest below a bucket; `None` leaves that to the object
    /// name's 1024 bytes.
    pub max_seed_depth: Option<NonZeroUsize>,
    /// Buckets one initialization file or one seed may declare; other buckets of the server do
    /// not count, and `None` stops at none. Either is one atomic publication, so its memory grows
    /// with the batch.
    pub max_buckets_per_batch: Option<NonZeroUsize>,
    /// Files and directories a synchronized tree may hold; `None` stops at none. A scan keeps one
    /// page per directory level, so a larger tree takes longer but holds no more memory.
    pub max_sync_entries: Option<NonZeroUsize>,
    /// Directory levels a synchronized tree may nest below `sync_dir`, counting the bucket;
    /// `None` leaves that to the object name's 1024 bytes.
    pub max_sync_depth: Option<NonZeroUsize>,
    pub default_location: String,
    /// Soft delete retention given to a new bucket that names no policy; `None` gives it none.
    pub default_soft_delete_retention_seconds: Option<std::num::NonZeroU64>,
    /// Preserve stored lifecycle rules without applying their Delete actions.
    pub disable_lifecycle: bool,
    /// The bucket initialization document, from `buckets_file` or `buckets`.
    pub buckets: Option<DocumentSource>,
    /// The explicit `buckets_file_mode`, which governs both document settings;
    /// [`BucketsFileMode::resolve`] applies the default for the document's form.
    pub buckets_file_mode: Option<BucketsFileMode>,
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
    /// authentication; empty disables it. Never an authorization input.
    pub local_creator_email: String,
    pub runtime_endpoints: RuntimeEndpoints,
    pub pubsub_emulator_host: Option<String>,
    pub event_pubsub_project_id: String,
    pub event_pubsub_topic: String,
    pub event_bucket: String,
    pub event_object_prefix: String,
    pub event_list: String,
    pub xml_virtual_host_domains: Vec<String>,
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
            trusted_listen: None,
            unix_socket: None,
            scheme: Scheme::Https,
            tls_cert: None,
            tls_key: None,
            tls_handshake_timeout: TimeLimit::from_millis(5_000),
            data_dir: PathBuf::from("data"),
            backend: Backend::Disk,
            memory_max_bytes: crate::memory_limit::default_budget(),
            memory_catalog_max_bytes: 16 * 1024 * 1024,
            seed_dir: None,
            default_seed_dir: None,
            sync_dir: None,
            sync_delete_guard: true,
            sync_reinitialize: None,
            seed_project: "local-project".into(),
            seed_content_type: crate::mime::SeedContentType::Extension,
            max_seed_entries: None,
            max_seed_depth: None,
            max_buckets_per_batch: None,
            max_sync_entries: None,
            max_sync_depth: None,
            default_location: "US".into(),
            // Cloud Storage defaults to seven days; the emulator keeps no deleted data unless asked,
            // so a local disk or RAM budget is not spent on bytes nobody will restore.
            default_soft_delete_retention_seconds: None,
            disable_lifecycle: false,
            buckets: None,
            buckets_file_mode: None,
            api_base_url: None,
            public_base_url: None,
            admin_enabled: false,
            admin_principal: None,
            admin_allowed_hosts: Vec::new(),
            trusted_principal: None,
            trusted_allowed_hosts: Vec::new(),
            local_creator_email: DEFAULT_LOCAL_CREATOR_EMAIL.into(),
            runtime_endpoints: RuntimeEndpoints::default(),
            pubsub_emulator_host: None,
            event_pubsub_project_id: String::new(),
            event_pubsub_topic: String::new(),
            event_bucket: String::new(),
            event_object_prefix: String::new(),
            event_list: "finalize".into(),
            xml_virtual_host_domains: Vec::new(),
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
        let listen = loopback_if_unspecified(self.effective_listen_http().unwrap_or(self.listen));
        let scheme = if self.scheme == Scheme::Both {
            Scheme::Http
        } else {
            self.scheme
        };
        format!("{}://{listen}{}", scheme.as_str(), Self::HEALTHCHECK_PATH)
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

    fn from_inputs(mut inputs: Inputs) -> ConfigurationReport {
        let invalid_file_fields = read_selected_config(&mut inputs);
        inputs.translate_aliases();
        let settings = collect_settings(&mut inputs);
        let mut report = ConfigurationReport {
            config: Self::default(),
            diagnostics: inputs.diagnostics,
            invalid: invalid_file_fields
                .into_iter()
                .filter(|key| !settings.contains_key(key))
                .collect(),
            auth_profile: None,
            settings: Settings::new(),
        };
        report.apply_settings(&settings);
        report.config.runtime_endpoints =
            RuntimeEndpoints::new(report.config.xml_virtual_host_domains.clone());
        report.check_document_sources(&settings);
        report.check_buckets_file(&settings);
        report.check_sync(&settings);
        report.check_backend(&settings);
        report.check_tls(&settings);
        report.check_listeners(&settings);
        report.check_grpc_streams(&settings);
        report.check_public_base_url(&settings);
        report.check_api_base_url(&settings);
        report.check_auth(&settings);
        report.check_trusted_listener(&settings);
        report.check_notifications(&settings);
        report.explain_empty_environment_values(&settings);
        report.settings = settings;
        report
    }
}

/// Effective settings by native name, after config file, environment and CLI precedence.
type Settings = BTreeMap<String, Input>;

/// Reads the config file selected by `--config` or `GCS_MIMIC_CONFIG` into `inputs` and
/// returns the file keys whose values had an unsupported JSON type.
fn read_selected_config(inputs: &mut Inputs) -> BTreeSet<String> {
    let mut invalid_file_fields = BTreeSet::new();
    let selector = inputs
        .entries
        .iter()
        .filter(|i| {
            i.name == "config"
                && i.kind == InputKind::Native
                && i.source != DiagnosticSource::Config
        })
        .max_by_key(|i| {
            if i.source == DiagnosticSource::Cli {
                1
            } else {
                0
            }
        })
        .cloned();
    let Some(selector) = selector.filter(|input| input.invalid.is_none()) else {
        return invalid_file_fields;
    };
    match read_config(&selector.value) {
        Ok(values) => {
            for (key, value) in values {
                let text = member_text(&key, &value);
                if !inputs.push(Input {
                    source: DiagnosticSource::Config,
                    key: key.clone(),
                    name: key.clone(),
                    value: text.clone().unwrap_or_default(),
                    kind: InputKind::Native,
                    invalid: None,
                }) {
                    break;
                }
                if text.is_none() {
                    invalid_file_fields.insert(key.clone());
                    let fix = member_fix(&key);
                    inputs.diagnostics.errors.push(Diagnostic::new(
                        DiagnosticSource::Config,
                        key,
                        fix,
                    ));
                    inputs.entries.pop();
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
                && !(i.key == "PUBSUB_EMULATOR_HOST"
                    && inputs.entries.iter().any(|other| {
                        other.source == source && other.key == "GCS_MIMIC_PUBSUB_EMULATOR_HOST"
                    }))
                && match i.kind {
                    InputKind::Native | InputKind::Translated => true,
                    InputKind::Alias(_) => false,
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
            if input.name.parse::<Setting>().is_err()
                || (input.name == "config" && source == DiagnosticSource::Config)
                || (input.name == "sync_reinitialize" && source != DiagnosticSource::Cli)
            {
                inputs.diagnostics.errors.push(Diagnostic::new(
                    source,
                    &input.key,
                    rejected_setting_fix(&input.name),
                ));
                continue;
            }
            settings.insert(input.name.clone(), input.clone());
        }
    }
    settings
}

fn rejected_setting_fix(name: &str) -> &'static str {
    match name {
        "sync_reinitialize" => {
            "sync_reinitialize is startup-only CLI input; remove it here and use --sync-dir DIRECTORY --sync-reinitialize CATALOG_ID once."
        }
        "port" => {
            "GCS_MIMIC_PORT is Compose-only host port publication; keep it in the Compose invocation, remove it from the server environment and use GCS_MIMIC_LISTEN=0.0.0.0:4443 for server binding."
        }
        "image" => {
            "GCS_MIMIC_IMAGE is Compose-only image selection; remove it from the server environment; there is no server equivalent."
        }
        "tls_dir" => {
            "GCS_MIMIC_TLS_DIR is Compose-only mount selection; remove it from the server environment and supply GCS_MIMIC_TLS_CERT=certs/server.pem and GCS_MIMIC_TLS_KEY=certs/server-key.pem."
        }
        _ => {
            "Unknown setting; remove it and use --help for supported native settings. Reserved future settings are unsupported."
        }
    }
}

/// Parses one setting into `config`. A rejected value may leave `config` partly updated, so the
/// caller applies it to a candidate copy and keeps the copy only on success.
/// `value` is borrowed: an inline document may be 1 GiB, and only the setting that stores it
/// needs its own copy.
fn apply_setting(config: &mut Config, key: &str, value: &str) -> Result<(), SettingFix> {
    let setting = key.parse::<Setting>().map_err(|_| unknown_setting())?;
    match setting {
        Setting::Backend | Setting::MemoryMaxBytes | Setting::MemoryCatalogMaxBytes => {
            storage::apply(config, setting, value)
        }
        Setting::PubsubEmulatorHost
        | Setting::EventPubsubProjectId
        | Setting::EventPubsubTopic
        | Setting::EventBucket
        | Setting::EventObjectPrefix
        | Setting::EventList => notifications::apply(config, setting, value),
        Setting::LogLevel
        | Setting::Scheme
        | Setting::Listen
        | Setting::ListenHttp
        | Setting::GrpcListen
        | Setting::TrustedListen
        | Setting::UnixSocket
        | Setting::TlsCert
        | Setting::TlsKey
        | Setting::TlsHandshakeTimeoutMs => apply_network_setting(config, setting, value),
        Setting::ApiBaseUrl | Setting::PublicBaseUrl | Setting::XmlVirtualHostDomains => {
            apply_url_setting(config, setting, value)
        }
        Setting::DataDir
        | Setting::SeedDir
        | Setting::DefaultSeedDir
        | Setting::SyncDir
        | Setting::SyncDeleteGuard
        | Setting::SyncReinitialize
        | Setting::SeedProject
        | Setting::SeedContentType
        | Setting::DefaultLocation
        | Setting::DefaultSoftDeleteRetentionSeconds
        | Setting::DisableLifecycle
        | Setting::BucketsFileMode => apply_storage_setting(config, setting, value),
        Setting::BucketsFile => DocumentKind::Buckets.apply(config, Form::File, value),
        Setting::Buckets => DocumentKind::Buckets.apply(config, Form::Inline, value),
        Setting::IdentityFile => DocumentKind::Identities.apply(config, Form::File, value),
        Setting::Identities => DocumentKind::Identities.apply(config, Form::Inline, value),
        Setting::AdminPrincipal
        | Setting::AdminAllowedHosts
        | Setting::TrustedPrincipal
        | Setting::TrustedAllowedHosts
        | Setting::LocalCreatorEmail
        | Setting::AdminEnabled
        | Setting::AuthMode
        | Setting::CompatProfile
        | Setting::CorsFallback
        | Setting::SignedHostPort
        | Setting::TestSigner
        | Setting::SignedUrls => apply_access_setting(config, setting, value),
        Setting::MaxConnections
        | Setting::MaxGrpcStreams
        | Setting::MaxObjectBytes
        | Setting::MaxResumableSessions
        | Setting::MaxRewriteSessions
        | Setting::MaxSeedEntries
        | Setting::MaxSeedDepth
        | Setting::MaxBucketsPerBatch
        | Setting::MaxSyncEntries
        | Setting::MaxSyncDepth
        | Setting::WorkerThreads
        | Setting::MaxBlockingThreads => apply_limit_setting(config, setting, value),
        Setting::ConnectionTimeoutMs
        | Setting::HeaderReadTimeoutMs
        | Setting::BodyInactivityTimeoutMs
        | Setting::RequestTimeoutMs
        | Setting::KeepAliveTimeoutMs
        | Setting::ObjectRequestTimeoutMs
        | Setting::CatalogWaitTimeoutMs
        | Setting::ShutdownTimeoutMs => apply_timeout_setting(config, setting, value),
        Setting::Config => Err(unknown_setting()),
    }
}

fn unknown_setting() -> SettingFix {
    setting::UNKNOWN_SETTING.into()
}

fn apply_network_setting(
    config: &mut Config,
    setting: Setting,
    value: &str,
) -> Result<(), SettingFix> {
    let key = setting.as_str();
    match setting {
        Setting::LogLevel => config.log_level = value.parse()?,
        Setting::Scheme => config.scheme = value.parse()?,
        Setting::Listen => {
            config.listen = value
                .parse()
                .map_err(|_| "listen must be an IP:port socket address")?
        }
        Setting::ListenHttp => {
            config.listen_http = Some(value.parse().map_err(
                |_| "listen_http must be an IP socket address, for example 127.0.0.1:8000",
            )?);
        }
        Setting::GrpcListen => {
            config.grpc_listen = Some(value.parse().map_err(|_| {
                "grpc_listen must be an IP socket address; use 127.0.0.1:4444 for a separate plaintext gRPC port"
            })?);
        }
        Setting::TrustedListen => {
            config.trusted_listen = Some(value.parse().map_err(|_| {
                "trusted_listen must be an IP socket address, for example 0.0.0.0:4444 inside a container"
            })?);
        }
        Setting::UnixSocket => config.unix_socket = Some(PathBuf::from(non_empty(key, value)?)),
        Setting::TlsCert | Setting::TlsKey => {
            if value.is_empty() {
                return Err(format!("{key} must not be empty").into());
            }
            if setting == Setting::TlsCert {
                config.tls_cert = Some(PathBuf::from(value));
            } else {
                config.tls_key = Some(PathBuf::from(value));
            }
        }
        Setting::TlsHandshakeTimeoutMs => {
            config.tls_handshake_timeout = setting.time_limit(value)?
        }
        _ => return Err(unknown_setting()),
    }
    Ok(())
}

fn apply_url_setting(config: &mut Config, setting: Setting, value: &str) -> Result<(), SettingFix> {
    match setting {
        Setting::ApiBaseUrl => {
            config.api_base_url = Some(
                crate::url::normalize_base_url(value)
                    .map_err(|error| format!("api_base_url: {error}"))?,
            );
        }
        Setting::PublicBaseUrl => {
            config.public_base_url = Some(
                PublicOrigin::parse(value).map_err(|error| format!("public_base_url: {error}"))?,
            );
        }
        Setting::XmlVirtualHostDomains => {
            if !value.is_empty() {
                for domain in value.split(',') {
                    let domain = domain.trim().to_ascii_lowercase();
                    validate_dns_name(&domain).map_err(|error| error.to_string())?;
                    if config.xml_virtual_host_domains.contains(&domain) {
                        return Err(
                            "duplicate xml_virtual_host_domains entry; remove repeated DNS names"
                                .into(),
                        );
                    }
                    if config.xml_virtual_host_domains.len() == 32 {
                        return Err(
                            "xml_virtual_host_domains must contain at most 32 DNS names".into()
                        );
                    }
                    config.xml_virtual_host_domains.push(domain);
                }
                // The most specific configured suffix owns overlapping domains.
                config
                    .xml_virtual_host_domains
                    .sort_by_key(|d| std::cmp::Reverse(d.len()));
            }
        }
        _ => return Err(unknown_setting()),
    }
    Ok(())
}

fn apply_storage_setting(
    config: &mut Config,
    setting: Setting,
    value: &str,
) -> Result<(), SettingFix> {
    let key = setting.as_str();
    match setting {
        Setting::DataDir => config.data_dir = PathBuf::from(non_empty(key, value)?),
        Setting::SeedDir => config.seed_dir = Some(PathBuf::from(non_empty(key, value)?)),
        Setting::DefaultSeedDir => {
            config.default_seed_dir = Some(PathBuf::from(non_empty(key, value)?));
        }
        Setting::SyncDir => {
            if value.is_empty() {
                return Err(
                    "sync_dir must not be empty; supply an existing writable directory".into(),
                );
            }
            config.sync_dir = Some(PathBuf::from(value));
        }
        Setting::SyncDeleteGuard => {
            config.sync_delete_guard = value
                .parse::<bool>()
                .map_err(|_| "sync_delete_guard must be true or false")?
        }
        Setting::SyncReinitialize => {
            config.sync_reinitialize =
                Some(crate::sync::parse_catalog_id(value).map_err(|error| error.to_string())?)
        }
        Setting::SeedProject => {
            crate::json::validate_project(value)
                .map_err(|error| format!("seed_project: {}", error.message))?;
            config.seed_project = value.to_owned();
        }
        Setting::SeedContentType => config.seed_content_type = value.parse()?,
        Setting::DefaultLocation => {
            let value = if value.is_empty() { "US" } else { value };
            config.default_location = bucket_location(value)
                .map_err(|_| "default_location must be a supported bucket location; use US or ASIA-NORTHEAST1")?
                .0
                .to_owned();
        }
        Setting::DefaultSoftDeleteRetentionSeconds => {
            config.default_soft_delete_retention_seconds = value
                .parse::<i64>()
                .ok()
                .and_then(|seconds| crate::bucket::SoftDeleteRetention::try_from(seconds).ok())
                .ok_or("default_soft_delete_retention_seconds must be 0 (no soft delete) or 1..=3155760000 seconds; use 604800 for the Cloud Storage default of seven days")?
                .enabled();
        }
        Setting::DisableLifecycle => {
            config.disable_lifecycle = value
                .parse::<bool>()
                .map_err(|_| "disable_lifecycle must be true or false")?
        }
        Setting::BucketsFileMode => config.buckets_file_mode = Some(value.parse()?),
        _ => return Err(unknown_setting()),
    }
    Ok(())
}

fn apply_access_setting(
    config: &mut Config,
    setting: Setting,
    value: &str,
) -> Result<(), SettingFix> {
    match setting {
        Setting::AdminAllowedHosts => config.admin_allowed_hosts = allowed_hosts(setting, value)?,
        Setting::TrustedAllowedHosts => {
            config.trusted_allowed_hosts = allowed_hosts(setting, value)?;
        }
        Setting::AdminPrincipal => {
            if !crate::auth::valid_principal_id(value) {
                return Err(
                    "admin_principal must be 1..=254 bytes of printable ASCII without whitespace, like identity ids"
                        .into(),
                );
            }
            config.admin_principal = Some(value.to_owned());
        }
        Setting::TrustedPrincipal => {
            if !crate::auth::valid_principal_id(value) {
                return Err(
                    "trusted_principal must be 1..=254 bytes of printable ASCII without whitespace, like identity ids"
                        .into(),
                );
            }
            config.trusted_principal = Some(value.to_owned());
        }
        Setting::LocalCreatorEmail => {
            if !value.is_empty() {
                crate::auth::validate_local_creator_email(value).map_err(|reason| {
                    format!(
                        "local_creator_email {reason}; use an address such as \
                         local-user@gcs-mimic.invalid or an empty value to disable it"
                    )
                })?;
            }
            config.local_creator_email = value.to_owned();
        }
        Setting::AdminEnabled => {
            config.admin_enabled = value
                .parse::<bool>()
                .map_err(|_| "admin_enabled must be true or false")?;
        }
        Setting::AuthMode => config.auth_mode = value.parse()?,
        Setting::CompatProfile => config.compat_profile = value.parse()?,
        Setting::CorsFallback => config.cors_fallback = Some(value.parse()?),
        Setting::SignedHostPort => config.signed_host_port = Some(value.parse()?),
        Setting::TestSigner => config.test_signer = Some(value.parse()?),
        Setting::SignedUrls => config.signed_urls = Some(value.parse()?),
        _ => return Err(unknown_setting()),
    }
    Ok(())
}

/// The DNS names an allow-list `setting` names, normalized as request hosts compare.
fn allowed_hosts(setting: Setting, value: &str) -> Result<Vec<String>, SettingFix> {
    let name = setting.as_str();
    let mut hosts = Vec::new();
    if value.is_empty() {
        return Ok(hosts);
    }
    for host in value.split(',') {
        let host = host.trim().to_ascii_lowercase();
        let host = host.strip_suffix('.').unwrap_or(&host).to_owned();
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

fn apply_limit_setting(
    config: &mut Config,
    setting: Setting,
    value: &str,
) -> Result<(), SettingFix> {
    let value = setting.integer(value)?;
    let count = || {
        usize::try_from(value).map_err(|_| {
            SettingFix::from(format!(
                "{} exceeds this platform's count limit",
                setting.as_str()
            ))
        })
    };
    // The one place 0 means unlimited: past this point the absence of a limit is `None`.
    let session_limit = || {
        u32::try_from(value)
            .map(NonZeroU32::new)
            .map_err(|_| SettingFix::from(format!("{} must fit in 32 bits", setting.as_str())))
    };
    let tree_limit = || count().map(NonZeroUsize::new);
    match setting {
        Setting::MaxConnections => config.max_connections = count()?,
        // The setting's bounds keep this in range; the conversion records that in the type.
        Setting::MaxGrpcStreams => {
            config.max_grpc_streams = u32::try_from(value)
                .ok()
                .and_then(NonZeroU32::new)
                .ok_or_else(unknown_setting)?;
        }
        Setting::MaxObjectBytes => config.max_object_bytes = value,
        Setting::MaxResumableSessions => config.max_resumable_sessions = session_limit()?,
        Setting::MaxRewriteSessions => config.max_rewrite_sessions = session_limit()?,
        Setting::MaxSeedEntries => config.max_seed_entries = tree_limit()?,
        Setting::MaxSeedDepth => config.max_seed_depth = tree_limit()?,
        Setting::MaxBucketsPerBatch => config.max_buckets_per_batch = tree_limit()?,
        Setting::MaxSyncEntries => config.max_sync_entries = tree_limit()?,
        Setting::MaxSyncDepth => config.max_sync_depth = tree_limit()?,
        Setting::WorkerThreads => config.worker_threads = count()?,
        Setting::MaxBlockingThreads => config.max_blocking_threads = count()?,
        _ => return Err(unknown_setting()),
    }
    Ok(())
}

fn apply_timeout_setting(
    config: &mut Config,
    setting: Setting,
    value: &str,
) -> Result<(), SettingFix> {
    let limit = setting.time_limit(value)?;
    match setting {
        Setting::ConnectionTimeoutMs => {
            config.header_read_timeout = limit;
            config.body_inactivity_timeout = limit;
            config.request_timeout = limit;
        }
        Setting::HeaderReadTimeoutMs => config.header_read_timeout = limit,
        Setting::BodyInactivityTimeoutMs => config.body_inactivity_timeout = limit,
        Setting::RequestTimeoutMs => config.request_timeout = limit,
        Setting::KeepAliveTimeoutMs => config.keep_alive_timeout = limit,
        Setting::ObjectRequestTimeoutMs => config.object_request_timeout = limit,
        Setting::CatalogWaitTimeoutMs => config.catalog_wait_timeout = limit,
        Setting::ShutdownTimeoutMs => config.shutdown_timeout = limit,
        _ => return Err(unknown_setting()),
    }
    Ok(())
}

fn non_empty<'a>(key: &str, value: &'a str) -> Result<&'a str, SettingFix> {
    if value.is_empty() {
        return Err(format!("{key} must not be empty").into());
    }
    Ok(value)
}

pub struct ConfigurationReport {
    pub config: Config,
    pub diagnostics: Diagnostics,
    invalid: BTreeSet<String>,
    /// Set by validation only when the authentication settings are valid and agree.
    auth_profile: Option<AuthProfile>,
    /// The inputs validation read, so that a diagnostic after loading can still name the one
    /// that caused it.
    settings: Settings,
}

impl ConfigurationReport {
    pub fn valid(&self, keys: &[&str]) -> bool {
        !keys.iter().any(|key| self.invalid.contains(*key))
    }

    fn any_invalid(&self, keys: &[&str]) -> bool {
        !self.valid(keys)
    }

    fn apply_settings(&mut self, settings: &Settings) {
        // `connection_timeout_ms` is a coarse alias for three finer settings, so it goes first:
        // any of those named explicitly overrides it, whichever source named either one.
        let alias_key = Setting::ConnectionTimeoutMs.as_str();
        let alias = settings.get_key_value(alias_key);
        let rest = settings.iter().filter(|(key, _)| key.as_str() != alias_key);
        for (key, input) in alias.into_iter().chain(rest) {
            if let Some(invalid) = input.invalid {
                self.invalid.insert(key.clone());
                if invalid == InputError::Encoding {
                    self.diagnostics.errors.push(Diagnostic::new(
                        input.source,
                        &input.key,
                        "Setting values must be valid UTF-8; correct the environment value.",
                    ));
                }
                continue;
            }
            if key == "config" {
                continue;
            }
            let mut candidate = self.config.clone();
            match apply_setting(&mut candidate, key, &input.value) {
                Ok(()) => self.config = candidate,
                Err(SettingFix(fix)) => {
                    self.invalid.insert(key.clone());
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
    fn check_buckets_file(&mut self, settings: &Settings) {
        let mode = Setting::BucketsFileMode.as_str();
        if self.config.buckets.is_none()
            && self.valid(&[Setting::BucketsFile.as_str(), Setting::Buckets.as_str()])
            && let Some(input) = settings.get(mode)
            && self.invalid.insert(mode.to_owned())
        {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "This setting requires buckets_file or buckets; supply --buckets-file PATH or the document itself with --buckets JSON, or remove this setting.",
            ));
        }
    }

    fn check_sync(&mut self, settings: &Settings) {
        let config = &self.config;
        if config.sync_dir.is_none() && !self.invalid.contains("sync_dir") {
            for input in settings
                .values()
                .filter(|input| setting::requires_sync_dir(&input.name))
            {
                if self.invalid.insert(input.name.clone()) {
                    self.diagnostics.errors.push(Diagnostic::new(
                        input.source,
                        &input.key,
                        "This setting requires sync_dir; supply --sync-dir DIRECTORY or remove this setting.",
                    ));
                }
            }
        }
        if config.backend == Backend::Disk
            && !self.any_invalid(&["sync_dir", "data_dir", "seed_dir"])
            && let Some(root) = &config.sync_dir
            && let Err(error) =
                crate::sync::validate_directory(root, &config.data_dir, config.seed_dir.as_deref())
        {
            let (source, key) = settings
                .get("sync_dir")
                .map_or((DiagnosticSource::Config, "sync_dir"), |input| {
                    (input.source, input.key.as_str())
                });
            self.invalid.insert("sync_dir".into());
            self.diagnostics
                .errors
                .push(Diagnostic::new(source, key, error.to_string()));
        }
        if config.compat_profile == CompatProfile::LegacyEmulator
            && let Some(input) = settings.get("default_location")
            && !self.invalid.contains("default_location")
        {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "default_location requires compat_profile=native; use --compat-profile native --default-location EU or remove the default_location setting.",
            ));
            self.invalid.insert("default_location".into());
        }
        if config.compat_profile == CompatProfile::LegacyEmulator
            && !config.local_creator_email.is_empty()
            && let Some(input) = settings.get("local_creator_email")
            && !self.invalid.contains("local_creator_email")
        {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "local_creator_email requires compat_profile=native; the legacy emulator profile never records a creator. Use --compat-profile native or remove the setting.",
            ));
            self.invalid.insert("local_creator_email".into());
        }
        // Strict mode records the registered caller, so a creator address would be silently unused.
        // A permissive identity document that only verifies signatures leaves the creator in use.
        if (config.auth_mode == AuthMode::Strict
            || (config.identities.is_some()
                && config.effective_signed_urls() == SignedUrls::Ignore))
            && !config.local_creator_email.is_empty()
            && let Some(input) = settings.get("local_creator_email")
            && !self.invalid.contains("local_creator_email")
        {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "local_creator_email applies only to auth_mode=permissive; strict mode records the registered caller as owner. Remove the setting or use permissive mode.",
            ));
            self.invalid.insert("local_creator_email".into());
        }
        // Zero is what the legacy profile does anyway, so only a positive request conflicts.
        if config.compat_profile == CompatProfile::LegacyEmulator
            && config.default_soft_delete_retention_seconds.is_some()
            && let Some(input) = settings.get("default_soft_delete_retention_seconds")
            && !self
                .invalid
                .contains("default_soft_delete_retention_seconds")
        {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "default_soft_delete_retention_seconds requires compat_profile=native; the legacy emulator profile never applies a default soft delete policy. Use --compat-profile native or remove the setting.",
            ));
            self.invalid
                .insert("default_soft_delete_retention_seconds".into());
        }
    }

    fn check_tls(&mut self, settings: &Settings) {
        let tls_options_supplied = settings.keys().any(|key| {
            matches!(
                key.as_str(),
                "tls_cert" | "tls_key" | "tls_handshake_timeout_ms"
            )
        });
        let config = &self.config;
        match config.scheme {
            Scheme::Http if tls_options_supplied && !self.invalid.contains("scheme") => {
                for name in ["tls_cert", "tls_key", "tls_handshake_timeout_ms"] {
                    if let Some(input) = settings.get(name)
                        && !self.invalid.contains(name)
                    {
                        self.diagnostics.errors.push(Diagnostic::new(
                            input.source,
                            &input.key,
                            "TLS settings require scheme=https or both; remove TLS settings for HTTP.",
                        ));
                        self.invalid.insert(name.into());
                    }
                }
            }
            Scheme::Https | Scheme::Both
                if config.tls_cert.is_some() != config.tls_key.is_some()
                    && !self.any_invalid(&["scheme", "tls_cert", "tls_key"]) =>
            {
                let name = if config.tls_cert.is_some() {
                    "tls_cert"
                } else {
                    "tls_key"
                };
                if let Some(input) = settings.get(name) {
                    self.diagnostics.errors.push(Diagnostic::new(
                        input.source,
                        &input.key,
                        "tls_cert and tls_key must be supplied together; provide both PEM paths.",
                    ));
                }
                self.invalid.insert(name.into());
            }
            _ => {}
        }
    }

    /// Without a gRPC listener the limit would silently do nothing, and a typo in the listener's
    /// setting would hide behind it.
    fn check_grpc_streams(&mut self, settings: &Settings) {
        if let Some(input) = settings.get("max_grpc_streams")
            && !self.invalid.contains("max_grpc_streams")
            && !settings.contains_key("grpc_listen")
        {
            self.invalid.insert("max_grpc_streams".into());
            self.diagnostics.errors.push(Diagnostic::new(input.source, &input.key,
                "max_grpc_streams requires grpc_listen; remove it or set --grpc-listen, for example 127.0.0.1:4444."));
        }
    }

    fn check_listeners(&mut self, settings: &Settings) {
        if self.config.unix_socket.is_some() {
            let fix = if !cfg!(unix) {
                Some("unix_socket requires a Unix platform; no TCP fallback is used.")
            } else if self.config.scheme != Scheme::Http {
                Some(
                    "unix_socket requires scheme=http; advertised HTTP(S) origins remain independent.",
                )
            } else if ["listen", "listen_http", "grpc_listen", "trusted_listen"]
                .iter()
                .any(|key| settings.contains_key(*key))
            {
                Some(
                    "unix_socket cannot be combined with explicit listen, listen_http, grpc_listen or trusted_listen settings; remove the TCP settings.",
                )
            } else {
                None
            };
            if let Some(fix) = fix
                && let Some(input) = settings.get("unix_socket")
            {
                self.invalid.insert("unix_socket".into());
                self.diagnostics
                    .errors
                    .push(Diagnostic::new(input.source, &input.key, fix));
            }
            return;
        }
        if self.any_invalid(&["scheme", "listen", "listen_http"]) {
            return;
        }
        // A port for the second listener means nothing without scheme=both, and a shared command
        // line may carry it for the other schemes. An explicit listen_http stays an error below.
        if self.config.scheme != Scheme::Both
            && let Some(input) = settings
                .get("listen_http")
                .filter(|input| input.kind == InputKind::Translated)
        {
            self.diagnostics.warnings.push(Diagnostic::new(
                input.source,
                &input.key,
                "This port applies only with scheme=both, so it is ignored; use --scheme both to serve HTTP on it.",
            ));
            self.config.listen_http = None;
            return;
        }
        let config = &self.config;
        let fix = if config.scheme != Scheme::Both && config.listen_http.is_some() {
            Some("listen_http requires scheme=both; remove listen_http or use --scheme both.")
        } else if config.listen.port() != 0 && config.effective_listen_http() == Some(config.listen)
        {
            Some(
                "listen and listen_http must use different nonzero endpoints; use --listen-http 127.0.0.1:8001 or port 0.",
            )
        } else {
            None
        };
        if let Some(fix) = fix {
            let name = if settings.contains_key("listen_http") {
                "listen_http"
            } else {
                "listen"
            };
            if let Some(input) = settings.get(name) {
                self.diagnostics
                    .errors
                    .push(Diagnostic::new(input.source, &input.key, fix));
            }
            self.invalid.insert("listen_http".into());
        }
    }

    fn check_public_base_url(&mut self, settings: &Settings) {
        let config = &mut self.config;
        if !self.invalid.contains("public_base_url")
            && let Some(input) = settings.get("public_base_url")
            && let Some(origin) = config.public_base_url.as_ref()
        {
            if let Some(domain) = origin.dns_name() {
                let domain = domain.to_owned();
                if !config.xml_virtual_host_domains.contains(&domain) {
                    if config.xml_virtual_host_domains.len() == 32 {
                        self.diagnostics.errors.push(Diagnostic::new(input.source, &input.key,
                            "public_base_url and xml_virtual_host_domains must contain at most 32 distinct DNS names; remove a domain or reuse one already configured."));
                        self.invalid.insert("public_base_url".into());
                    } else {
                        config.xml_virtual_host_domains.push(domain);
                        config
                            .xml_virtual_host_domains
                            .sort_by_key(|d| std::cmp::Reverse(d.len()));
                    }
                }
            } else {
                self.diagnostics.warnings.push(Diagnostic::new(input.source, &input.key,
                    "public_base_url is an IP address: virtual-host routing is disabled; use path-style URLs or a DNS origin such as http://storage.example.invalid."));
            }
        }
    }

    fn check_api_base_url(&mut self, settings: &Settings) {
        let config = &self.config;
        if !self.any_invalid(&["api_base_url", "xml_virtual_host_domains", "listen"])
            && config
                .effective_api_base_url(config.listen)
                .parse::<hyper::Uri>()
                .ok()
                .and_then(|api| api.authority().cloned())
                .is_some_and(|a| {
                    crate::xml::bucket_for_authority(a.as_str(), &config.xml_virtual_host_domains)
                        .is_some()
                })
        {
            let input = settings
                .get("api_base_url")
                .or_else(|| settings.get("xml_virtual_host_domains"))
                .or_else(|| settings.get("public_base_url"));
            if let Some(input) = input {
                self.diagnostics.errors.push(Diagnostic::new(input.source, &input.key, "api_base_url host selects an XML virtual bucket; use a host outside xml_virtual_host_domains or configure that host itself as a base domain."));
            }
        }
    }

    fn check_auth(&mut self, settings: &Settings) {
        self.check_admin_auth(settings);
        let config = &self.config;
        if let Some(input) = settings.get("admin_principal")
            && !self.invalid.contains("admin_principal")
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
                self.invalid.insert("admin_principal".into());
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
        let listen = Setting::TrustedListen.as_str();
        let principal = Setting::TrustedPrincipal.as_str();
        let named = (
            settings.contains_key(listen),
            settings.contains_key(principal),
        );
        let collision = self.trusted_listen_collision();
        let rejected = match (self.config.auth_mode, named, collision) {
            (_, (false, false), _) => return,
            (AuthMode::Permissive, _, _) if self.valid(&["auth_mode"]) => [listen, principal]
                .map(|name| {
                    (name, format!("{name} requires auth_mode=strict; in permissive mode every request is already allowed, so a trusted listener changes nothing. Remove it or select strict mode."))
                })
                .to_vec(),
            (AuthMode::Strict, (false, true), _) => vec![(principal, "trusted_principal requires trusted_listen; set --trusted-listen IP:PORT for the application's servers or remove the setting.".to_owned())],
            (AuthMode::Strict, (true, _), Some((setting, example))) => vec![(listen, format!("trusted_listen must differ from {}, which binds the same address; choose a port that no other listener uses, for example {example}.", setting.as_str()))],
            // An invalid auth_mode was already reported, and a valid strict listener stays.
            (AuthMode::Permissive | AuthMode::Strict, _, _) => return,
        };
        for (name, fix) in rejected {
            if let Some(input) = settings.get(name)
                && self.invalid.insert(name.to_owned())
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
        let name = Setting::TrustedAllowedHosts.as_str();
        if !settings.contains_key(Setting::TrustedListen.as_str())
            && let Some(input) = settings.get(name)
            && self.invalid.insert(name.to_owned())
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
        let config = &self.config;
        if config.unix_socket.is_some()
            || !self.valid(&[
                "trusted_listen",
                "listen",
                "listen_http",
                "grpc_listen",
                "scheme",
            ])
        {
            return None;
        }
        let address = config
            .trusted_listen
            .filter(|address| address.port() != 0)?;
        let others = [
            (Setting::Listen, Some(config.listen)),
            (Setting::ListenHttp, config.effective_listen_http()),
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
        if self.config.auth_mode == AuthMode::Strict
            && self.config.admin_enabled
            && self.config.admin_principal.is_none()
            && let Some(input) = settings.get("admin_enabled")
        {
            self.diagnostics.errors.push(Diagnostic::new(input.source, &input.key,
                "admin_enabled in strict mode requires admin_principal; set --admin-principal to the registered instance administrator"));
        }
    }
}

/// Read only an explicitly selected helper config, without server-side validation or resources.
pub fn read_public_base_url(path: &str) -> Result<Option<String>, Diagnostic> {
    let entries = read_config(path).map_err(|error| {
        Diagnostic::new(
            DiagnosticSource::Cli,
            "--config",
            error.fix("--config PATH"),
        )
    })?;
    let mut public_base = None;
    for (key, raw) in entries {
        let invalid = || Diagnostic::new(DiagnosticSource::Config, &key, member_fix(&key));
        // The server validates a document; this helper only checks that the member may hold one.
        if document::holds_document(&key) {
            document::member_text(&raw).ok_or_else(invalid)?;
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        if setting_text(&value).is_none() {
            return Err(invalid());
        }
        if key == "public_base_url" {
            public_base = Some(
                crate::url::normalize_public_base_url(
                    value
                        .as_str()
                        .ok_or_else(|| Diagnostic::new(DiagnosticSource::Config, &key,
                            "public_base_url must be a string HTTP(S) origin, for example http://localhost:4443."))?,
                )
                .map_err(|error| Diagnostic::new(DiagnosticSource::Config, key, format!("public_base_url: {error}")))?,
            );
        }
    }
    Ok(public_base)
}

fn setting_text(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|v| v.to_string()))
}

/// The setting value a config file member supplies, or `None` for a JSON type its setting does
/// not take.
fn member_text(key: &str, raw: &RawValue) -> Option<String> {
    if document::holds_document(key) {
        return document::member_text(raw);
    }
    let value: serde_json::Value = serde_json::from_str(raw.get()).ok()?;
    if key == "default_location" && value.is_null() {
        Some(String::new())
    } else if matches!(key, "sync_delete_guard" | "admin_enabled") && value.is_boolean() {
        Some(value.to_string())
    } else if key == "buckets_file" {
        value.as_str().map(str::to_owned)
    } else {
        setting_text(&value)
    }
}

fn member_fix(key: &str) -> &'static str {
    if document::holds_document(key) {
        document::MEMBER_FIX
    } else {
        CONFIG_VALUE_FIX
    }
}

enum ConfigReadError {
    Unreadable,
    TooLarge,
    InputLimit,
    Malformed,
}

impl ConfigReadError {
    fn fix(&self, selector: &str) -> String {
        match self {
            Self::Unreadable => format!("Cannot read config; select a readable JSON file with {selector}."),
            Self::TooLarge => "Config exceeds 64 KiB; reduce the file size.".into(),
            Self::InputLimit => "Configuration input limit exceeded; supply at most 1024 settings across inputs and keys at most 256 UTF-8 bytes.".into(),
            Self::Malformed => "Malformed config; provide a valid flat JSON object with string or unsigned integer values.".into(),
        }
    }
}

/// The members of a config file as written, so that a document member keeps its exact text.
fn read_config(path: &str) -> Result<Vec<(String, Box<RawValue>)>, ConfigReadError> {
    let bytes =
        crate::bounded_file::read(Path::new(path), 65_536).map_err(|error| match error {
            crate::bounded_file::ReadError::Limit => ConfigReadError::TooLarge,
            crate::bounded_file::ReadError::Io(_) => ConfigReadError::Unreadable,
        })?;
    let mut limited = false;
    let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
    crate::config_input::ordered_members(
        &mut deserializer,
        "a flat configuration object",
        |count, key| {
            limited = count > crate::config_input::MAX_SETTINGS
                || key.len() > crate::config_input::MAX_KEY_BYTES;
            !limited
        },
    )
    .and_then(|entries| deserializer.end().map(|()| entries))
    .map_err(|_| {
        if limited {
            ConfigReadError::InputLimit
        } else {
            ConfigReadError::Malformed
        }
    })
}

/// A host or virtual-host domain that is not a plain DNS name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidDnsName;

impl std::fmt::Display for InvalidDnsName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "virtual-host domain must be a DNS name without scheme, port, wildcard or trailing dot",
        )
    }
}

impl std::error::Error for InvalidDnsName {}

pub(crate) fn validate_dns_name(value: &str) -> Result<(), InvalidDnsName> {
    if value.is_empty()
        || value.len() > 253
        || value.parse::<std::net::IpAddr>().is_ok()
        || !value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err(InvalidDnsName);
    }
    Ok(())
}

fn bounded(key: &str, value: &str, min: u64, max: u64) -> Result<u64, SettingFix> {
    value
        .parse::<u64>()
        .ok()
        .filter(|n| (min..=max).contains(n))
        .ok_or_else(|| format!("{key} must be an integer in {min}..={max}").into())
}

#[cfg(test)]
mod tests {
    use super::test_support::{assert_precedence, assert_rejected};
    use super::*;
    use crate::{auth::TestSignerChoice, test_support::TestDir};

    #[test]
    fn strict_admin_requires_an_instance_principal_and_hosts_are_validated() {
        let args = [
            "--auth-mode",
            "strict",
            "--identity-file",
            "identities.json",
            "--admin-enabled",
            "true",
        ]
        .map(str::to_owned);
        let error = Config::load(&args, &BTreeMap::new()).unwrap_err();
        assert!(error.to_string().contains("admin_principal"));
        let mut configured = args.to_vec();
        configured.extend(["--admin-principal".into(), "admin".into()]);
        Config::load(&configured, &BTreeMap::new()).unwrap();
        let args = [
            "--admin-allowed-hosts",
            "Dev.Example.com.,other.example.com",
        ]
        .map(str::to_owned);
        assert_eq!(
            Config::load(&args, &BTreeMap::new())
                .unwrap()
                .admin_allowed_hosts,
            ["dev.example.com", "other.example.com"]
        );
        for value in [
            "http://example.com",
            "example.com:1234",
            "*.example.com",
            "example.com,",
        ] {
            let args = ["--admin-allowed-hosts", value].map(str::to_owned);
            assert!(Config::load(&args, &BTreeMap::new()).is_err(), "{value}");
        }
    }

    #[test]
    fn administrative_principals_require_a_strict_enabled_registry_profile() {
        for (args, message) in [
            (vec!["--admin-principal", "admin"], "auth_mode=strict"),
            (
                vec![
                    "--auth-mode",
                    "strict",
                    "--identity-file",
                    "identities.json",
                    "--admin-principal",
                    "admin",
                ],
                "admin_enabled=true",
            ),
            (
                vec![
                    "--auth-mode",
                    "strict",
                    "--identity-file",
                    "identities.json",
                    "--admin-enabled",
                    "true",
                    "--admin-principal",
                    "bad principal",
                ],
                "without whitespace",
            ),
        ] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let error = Config::load(&args, &BTreeMap::new()).unwrap_err();
            assert!(
                error
                    .errors
                    .iter()
                    .any(|error| error.key.contains("admin-principal")
                        && error.fix.contains(message)),
                "{error}"
            );
        }
    }

    #[test]
    fn a_trusted_listener_needs_strict_mode_and_its_own_address() {
        let strict = [
            "--auth-mode",
            "strict",
            "--identity-file",
            "identities.json",
        ];
        let load = |options: &[&str]| {
            let args: Vec<String> = options.iter().map(|option| (*option).to_owned()).collect();
            Config::load(&args, &BTreeMap::new())
        };
        let mut paired = strict.to_vec();
        paired.extend([
            "--trusted-listen",
            "0.0.0.0:4444",
            "--trusted-principal",
            "app@example.invalid",
        ]);
        let config = load(&paired).unwrap();
        assert_eq!(config.trusted_listen, Some("0.0.0.0:4444".parse().unwrap()));
        assert_eq!(
            config.trusted_principal.as_deref(),
            Some("app@example.invalid")
        );
        let defaults = Config::default();
        assert_eq!(
            (defaults.trusted_listen, defaults.trusted_principal),
            (None, None)
        );
        let with = |extra: &[&'static str]| {
            let mut options = strict.to_vec();
            options.extend_from_slice(extra);
            options
        };
        let app = ["--trusted-principal", "app@example.invalid"];
        for (options, key, message) in [
            (
                vec!["--trusted-listen", "0.0.0.0:4444"],
                "--trusted-listen",
                "requires auth_mode=strict",
            ),
            (
                vec!["--trusted-principal", "app@example.invalid"],
                "--trusted-principal",
                "requires auth_mode=strict",
            ),
            (
                vec![
                    "--signed-urls",
                    "verify",
                    "--trusted-listen",
                    "0.0.0.0:4444",
                    "--trusted-principal",
                    "app@example.invalid",
                ],
                "--trusted-principal",
                "requires auth_mode=strict",
            ),
            (with(&app), "--trusted-principal", "requires trusted_listen"),
            (
                with(&[
                    "--listen",
                    "0.0.0.0:4443",
                    "--trusted-listen",
                    "0.0.0.0:4443",
                ]),
                "--trusted-listen",
                "must differ from listen, which binds the same address; choose a port that no other listener uses, for example 0.0.0.0:4445.",
            ),
            (
                with(&["--trusted-listen", "gcs:4444", app[0], app[1]]),
                "--trusted-listen",
                "IP socket address",
            ),
            (
                with(&[
                    "--trusted-listen",
                    "0.0.0.0:4444",
                    "--trusted-principal",
                    "bad id",
                ]),
                "--trusted-principal",
                "without whitespace",
            ),
            (
                with(&[
                    "--listen",
                    "0.0.0.0:4443",
                    "--trusted-listen",
                    "0.0.0.0:4443",
                    app[0],
                    app[1],
                ]),
                "--trusted-listen",
                "must differ from listen, which binds",
            ),
            (
                with(&[
                    "--scheme",
                    "both",
                    "--trusted-listen",
                    "127.0.0.1:8000",
                    app[0],
                    app[1],
                ]),
                "--trusted-listen",
                "must differ from listen_http, which binds the same address; choose a port that no other listener uses, for example 127.0.0.1:4445.",
            ),
            // The example skips a port another listener already uses.
            (
                with(&[
                    "--grpc-listen",
                    "127.0.0.1:4445",
                    "--trusted-listen",
                    "127.0.0.1:4445",
                    app[0],
                    app[1],
                ]),
                "--trusted-listen",
                "must differ from grpc_listen, which binds the same address; choose a port that no other listener uses, for example 127.0.0.1:4446.",
            ),
        ] {
            let error = load(&options).unwrap_err();
            assert!(
                error
                    .errors
                    .iter()
                    .any(|error| error.key == key && error.fix.contains(message)),
                "{options:?}: {error}"
            );
        }
        // Port 0 picks a free port for each listener, so equal ephemeral addresses are distinct.
        load(&with(&[
            "--listen",
            "127.0.0.1:0",
            "--trusted-listen",
            "127.0.0.1:0",
            app[0],
            app[1],
        ]))
        .unwrap();
        // A principal the identity file lacks is found only when startup reads the file.
        load(&with(&[
            "--trusted-listen",
            "0.0.0.0:4444",
            "--trusted-principal",
            "unknown@example.invalid",
        ]))
        .unwrap();
        // Without a principal the listener acts as the built-in application identity.
        let alone = with(&["--trusted-listen", "0.0.0.0:4444"]);
        let args: Vec<String> = alone.iter().map(|option| (*option).to_owned()).collect();
        let report = Config::preflight(&args, &BTreeMap::new());
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        assert!(matches!(
            report.auth_profile(),
            Some(AuthProfile::Strict {
                trusted: TrustedCaller::BuiltIn,
                ..
            })
        ));
        let report = Config::preflight(
            &paired
                .iter()
                .map(|option| (*option).to_owned())
                .collect::<Vec<_>>(),
            &BTreeMap::new(),
        );
        assert!(matches!(
            report.auth_profile(),
            Some(AuthProfile::Strict {
                trusted: TrustedCaller::Named(principal),
                ..
            }) if principal == "app@example.invalid"
        ));
    }

    #[test]
    fn trusted_allowed_hosts_need_the_listener_and_name_dns_hosts() {
        let load = |extra: &[&str]| {
            let mut args = vec![
                "--auth-mode",
                "strict",
                "--identity-file",
                "identities.json",
            ];
            args.extend_from_slice(extra);
            let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
            Config::load(&args, &BTreeMap::new())
        };
        let pair = [
            "--trusted-listen",
            "0.0.0.0:4444",
            "--trusted-principal",
            "app@example.invalid",
        ];
        let with_hosts = |value: &'static str| {
            let mut args = pair.to_vec();
            args.extend(["--trusted-allowed-hosts", value]);
            load(&args)
        };
        assert_eq!(
            with_hosts("Storage.Compose.Example., other.example,storage.compose.example")
                .unwrap()
                .trusted_allowed_hosts,
            ["storage.compose.example", "other.example"]
        );
        assert!(load(&pair).unwrap().trusted_allowed_hosts.is_empty());
        assert!(Config::default().trusted_allowed_hosts.is_empty());
        let error = load(&["--trusted-allowed-hosts", "storage.compose.example"]).unwrap_err();
        assert!(
            error
                .errors
                .iter()
                .any(|error| error.key == "--trusted-allowed-hosts"
                    && error.fix.contains("requires trusted_listen")),
            "{error}"
        );
        for value in [
            "http://storage.example",
            "storage.example:4444",
            "*.example",
            "storage.example,",
            "127.0.0.1",
            "bad host",
        ] {
            let error = with_hosts(value).unwrap_err();
            assert!(
                error
                    .errors
                    .iter()
                    .any(|error| error.key == "--trusted-allowed-hosts"
                        && error
                            .fix
                            .contains("trusted_allowed_hosts requires comma-separated DNS names")),
                "{value}: {error}"
            );
        }
        let many = (0..33)
            .map(|index| format!("host{index}.example"))
            .collect::<Vec<_>>()
            .join(",");
        let mut args = pair.to_vec();
        args.extend(["--trusted-allowed-hosts", many.as_str()]);
        let error = load(&args).unwrap_err();
        assert!(
            error.errors.iter().any(|error| error
                .fix
                .contains("trusted_allowed_hosts supports at most 32 names")),
            "{error}"
        );
    }

    #[test]
    fn fake_gcs_server_names_in_a_config_file_are_unknown_settings() {
        let dir = crate::test_support::TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"FAKE_GCS_HOST":"0.0.0.0"}"#).unwrap();
        let args = ["--config".into(), file.to_str().unwrap().into()];
        let report = Config::preflight(&args, &BTreeMap::new());
        assert!(
            report
                .diagnostics
                .errors
                .iter()
                .any(|d| d.key == "FAKE_GCS_HOST"),
            "{}",
            report.diagnostics
        );
    }

    #[test]
    fn advertised_ipv6_origin_drops_listener_scope_and_flow_information() {
        let bound = SocketAddr::V6(std::net::SocketAddrV6::new(
            "fe80::1".parse().unwrap(),
            4443,
            7,
            3,
        ));
        assert_eq!(
            Config::default().effective_api_base_url(bound),
            "https://[fe80::1]:4443"
        );
        assert_eq!(
            Config::default().effective_api_base_url(SocketAddr::V6(std::net::SocketAddrV6::new(
                std::net::Ipv6Addr::UNSPECIFIED,
                4443,
                7,
                3
            ))),
            "https://[::1]:4443"
        );
    }

    #[test]
    fn grpc_listener_configuration_precedence_and_diagnostics() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"grpc_listen":"127.0.0.1:4444"}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_GRPC_LISTEN".into(), "127.0.0.1:5555".into())]);
        assert_precedence(
            &args,
            &env,
            &["--grpc-listen", "127.0.0.1:0"],
            [None, Some(4444), Some(5555), Some(0)],
            Config::load,
            |config| config.grpc_listen.map(|address| address.port()),
        );
        assert_rejected(
            "grpc_listen",
            &[
                "",
                "localhost:4444",
                "http://127.0.0.1:4444",
                "127.0.0.1:65536",
            ],
            "127.0.0.1:4444",
        );
    }

    #[test]
    fn grpc_stream_limit_follows_precedence_within_its_bounds() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(
            &file,
            r#"{"max_grpc_streams":10,"grpc_listen":"127.0.0.1:4444"}"#,
        )
        .unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_MAX_GRPC_STREAMS".into(), "20".into())]);
        assert_precedence(
            &args,
            &env,
            &["--max-grpc-streams", "30"],
            [100, 10, 20, 30],
            Config::load,
            |config| config.max_grpc_streams.get(),
        );
        for value in [1, 65_536] {
            let args = [
                "--grpc-listen".to_owned(),
                "127.0.0.1:0".to_owned(),
                "--max-grpc-streams".to_owned(),
                value.to_string(),
            ];
            assert_eq!(
                Config::load(&args, &BTreeMap::new())
                    .unwrap()
                    .max_grpc_streams
                    .get(),
                value
            );
        }
        assert_rejected(
            "max_grpc_streams",
            &["0", "65537", "-1", "many"],
            "max_grpc_streams must be an integer in 1..=65536",
        );
    }

    #[test]
    fn grpc_stream_limit_without_a_grpc_listener_is_an_error() {
        assert_rejected("max_grpc_streams", &["200"], "requires grpc_listen");
        // A broken listener setting is the one fault to report, not a missing listener.
        let args = [
            "--grpc-listen",
            "localhost:4444",
            "--max-grpc-streams",
            "200",
        ]
        .map(str::to_owned);
        let errors = Config::load(&args, &BTreeMap::new()).unwrap_err().errors;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].key, "--grpc-listen");
    }

    /// Only an explicit value is configuration; loading applies the default that the settled
    /// test signer selects, which config::auth::tests covers.
    #[test]
    fn signed_host_port_follows_precedence_in_both_auth_modes() {
        assert_eq!(Config::default().signed_host_port, None);
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        // Lenient needs signatures to verify, which permissive mode does only when asked.
        std::fs::write(
            &file,
            r#"{"signed_host_port":"lenient","signed_urls":"verify"}"#,
        )
        .unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_SIGNED_HOST_PORT".into(), "exact".into())]);
        assert_precedence(
            &args,
            &env,
            &["--signed-host-port", "lenient"],
            [
                None,
                Some(SignedHostPort::Lenient),
                Some(SignedHostPort::Exact),
                Some(SignedHostPort::Lenient),
            ],
            Config::load,
            |config| config.signed_host_port,
        );
        let identities = dir.0.join("identities.json");
        let lenient = |mode: &[&str]| {
            let mut args = mode.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
            args.extend(["--signed-host-port".into(), "lenient".into()]);
            Config::load(&args, &BTreeMap::new())
        };
        for mode in [
            &["--auth-mode", "permissive", "--signed-urls", "verify"][..],
            &[
                "--auth-mode",
                "strict",
                "--identity-file",
                identities.to_str().unwrap(),
            ],
        ] {
            assert_eq!(
                lenient(mode).unwrap().signed_host_port,
                Some(SignedHostPort::Lenient)
            );
        }
        // Permissive mode ignores signatures by default, so lenient would change nothing there.
        let error = lenient(&["--auth-mode", "permissive"])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(
                "--signed-host-port: signed_host_port=lenient has no effect while signatures are ignored"
            ),
            "{error}"
        );
        // An explicit exact claims nothing, so it is accepted whatever is verified.
        let exact = ["--signed-host-port", "exact"].map(str::to_owned);
        assert_eq!(
            Config::load(&exact, &BTreeMap::new())
                .unwrap()
                .signed_host_port,
            Some(SignedHostPort::Exact)
        );
        assert_rejected(
            "signed_host_port",
            &["", "true", "Lenient", "EXACT", "ignore-port"],
            "signed_host_port must be exact or lenient",
        );
        std::fs::write(&file, r#"{"signed_host_port":true}"#).unwrap();
        assert!(Config::load(&args, &BTreeMap::new()).is_err());
    }

    #[test]
    fn cors_fallback_defaults_by_auth_mode_with_precedence_in_both_profiles() {
        let empty = BTreeMap::new();
        let load =
            |args: &[String], env| Config::load(args, env).unwrap().effective_cors_fallback();
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"cors_fallback":"allow-all"}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_CORS_FALLBACK".into(), "none".into())]);
        let args = assert_precedence(
            &args,
            &env,
            &["--cors-fallback", "allow-all"],
            [
                None,
                Some(CorsFallback::AllowAll),
                Some(CorsFallback::None),
                Some(CorsFallback::AllowAll),
            ],
            Config::load,
            |config| config.cors_fallback,
        );
        // Permissive mode allows every origin by default, as fake-gcs-server does; strict mode
        // follows Cloud Storage. An explicit value wins in both.
        let strict = ["--auth-mode", "strict", "--identity-file", "ids.json"].map(str::to_owned);
        for (mode, default) in [
            (&[][..], CorsFallback::AllowAll),
            (&strict[..], CorsFallback::None),
        ] {
            assert_eq!(
                load(mode, &empty),
                CorsFallbackChoice::Default(default),
                "{mode:?}"
            );
            for explicit in [CorsFallback::None, CorsFallback::AllowAll] {
                let mut args = mode.to_vec();
                args.extend(["--cors-fallback".into(), explicit.as_str().into()]);
                assert_eq!(
                    load(&args, &empty),
                    CorsFallbackChoice::Explicit(explicit),
                    "{args:?}"
                );
            }
        }
        for profile in ["native", "legacy-emulator"] {
            let args =
                ["--cors-fallback", "allow-all", "--compat-profile", profile].map(str::to_owned);
            assert_eq!(
                load(&args, &empty),
                CorsFallbackChoice::Explicit(CorsFallback::AllowAll)
            );
        }
        assert_rejected(
            "cors_fallback",
            &["", "true", "allow_all", "ALLOW-ALL", "*"],
            "none or allow-all",
        );
        std::fs::write(&file, r#"{"cors_fallback":true}"#).unwrap();
        assert!(Config::load(&args[..2], &empty).is_err());
        // A bare flag names the accepted value instead of an unrelated example.
        let errors = Config::load(&["--cors-fallback".into()], &empty).unwrap_err();
        assert!(errors.to_string().contains("--cors-fallback allow-all"));
    }

    #[test]
    fn test_signer_defaults_are_selected_by_profile() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        // An explicit enabled needs signatures to verify, which permissive mode does only when asked.
        std::fs::write(
            &file,
            r#"{"test_signer":"disabled","signed_urls":"verify"}"#,
        )
        .unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_TEST_SIGNER".into(), "enabled".into())]);
        assert_precedence(
            &args,
            &env,
            &["--test-signer", "disabled"],
            [
                None,
                Some(TestSigner::Disabled),
                Some(TestSigner::Enabled),
                Some(TestSigner::Disabled),
            ],
            Config::load,
            |config| config.test_signer,
        );
        let empty = BTreeMap::new();
        let strict = ["--auth-mode", "strict", "--identity-file", "ids.json"].map(str::to_owned);
        let verify = ["--signed-urls", "verify"].map(str::to_owned);
        let fixed = TestSignerChoice::Fixed;
        // Validation selects the rule; loading settles strict mode's default against the
        // identity document and warns there, which config::auth::tests covers.
        for (args, explicit, choice) in [
            (&[][..], None, None),
            (&[][..], Some("disabled"), None),
            (&verify[..], None, Some(fixed(TestSigner::Enabled))),
            (
                &verify[..],
                Some("enabled"),
                Some(fixed(TestSigner::Enabled)),
            ),
            (
                &verify[..],
                Some("disabled"),
                Some(fixed(TestSigner::Disabled)),
            ),
            (&strict[..], None, Some(TestSignerChoice::WhenDeclared)),
            (
                &strict[..],
                Some("disabled"),
                Some(fixed(TestSigner::Disabled)),
            ),
            (
                &strict[..],
                Some("enabled"),
                Some(fixed(TestSigner::Enabled)),
            ),
        ] {
            let mut args = args.to_vec();
            args.extend(
                explicit
                    .map(|value| ["--test-signer".into(), value.into()])
                    .into_iter()
                    .flatten(),
            );
            let report = Config::preflight(&args, &empty);
            assert!(!report.diagnostics.is_err(), "{args:?}");
            let selected = match report.auth_profile() {
                Some(AuthProfile::Permissive) => None,
                Some(
                    AuthProfile::VerifyingPermissive { verification, .. }
                    | AuthProfile::Strict { verification, .. },
                ) => Some(verification.test_signer),
                None => panic!("{args:?}: no profile"),
            };
            assert_eq!(selected, choice, "{args:?}");
            assert!(report.diagnostics.warnings.is_empty(), "{args:?}");
        }
        // Where signatures are ignored an enabled signer would promise what never happens.
        let ignored = ["--test-signer", "enabled"].map(str::to_owned);
        let error = Config::load(&ignored, &empty).unwrap_err().to_string();
        assert!(
            error.contains(
                "--test-signer: test_signer=enabled has no effect while signatures are ignored"
            ),
            "{error}"
        );
        assert_rejected(
            "test_signer",
            &["", "true", "Disabled", "off"],
            "enabled or disabled",
        );
    }

    /// Validation alone decides the profile, so loading never re-checks the pairing.
    #[test]
    fn validation_selects_the_profile_only_for_agreeing_authentication_settings() {
        let empty = BTreeMap::new();
        let profile = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            Config::preflight(&args, &empty).auth_profile().cloned()
        };
        let verification = |test_signer, signed_host_port| Verification {
            test_signer: TestSignerChoice::Fixed(test_signer),
            signed_host_port,
        };
        assert_eq!(profile(&[]), Some(AuthProfile::Permissive));
        assert_eq!(
            profile(&["--signed-urls", "verify", "--signed-host-port", "lenient"]),
            Some(AuthProfile::VerifyingPermissive {
                identities: None,
                verification: verification(TestSigner::Enabled, Some(SignedHostPort::Lenient)),
            })
        );
        assert_eq!(
            profile(&[
                "--auth-mode",
                "strict",
                "--identity-file",
                "ids.json",
                "--test-signer",
                "enabled"
            ]),
            Some(AuthProfile::Strict {
                identities: Some(DocumentSource::File("ids.json".into())),
                verification: verification(TestSigner::Enabled, None),
                trusted: TrustedCaller::Anonymous,
            })
        );
        for args in [
            &["--auth-mode", "strict"][..],
            &["--identity-file", "ids.json"],
            &["--signed-host-port", "lenient"],
            &["--test-signer", "enabled"],
            &[
                "--auth-mode",
                "strict",
                "--identity-file",
                "ids.json",
                "--signed-urls",
                "ignore",
            ],
            &["--signed-urls", "verify", "--test-signer", "sometimes"],
        ] {
            assert_eq!(profile(args), None, "{args:?}");
        }
    }

    #[test]
    fn signed_urls_default_by_profile_and_pair_identity_files_with_verification() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"signed_urls":"verify"}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_SIGNED_URLS".into(), "ignore".into())]);
        assert_precedence(
            &args,
            &env,
            &["--signed-urls", "verify"],
            [
                None,
                Some(SignedUrls::Verify),
                Some(SignedUrls::Ignore),
                Some(SignedUrls::Verify),
            ],
            Config::load,
            |config| config.signed_urls,
        );
        let empty = BTreeMap::new();
        let load = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            Config::load(&args, &empty)
        };
        let effective = |args: &[&str]| load(args).unwrap().effective_signed_urls();
        let strict = ["--auth-mode", "strict", "--identity-file", "ids.json"];
        assert_eq!(effective(&[]), SignedUrls::Ignore);
        assert_eq!(effective(&strict), SignedUrls::Verify);
        assert_eq!(
            effective(&[&strict[..], &["--signed-urls", "verify"]].concat()),
            SignedUrls::Verify
        );
        let error = load(&[&strict[..], &["--signed-urls", "ignore"]].concat())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("--signed-urls: signed_urls=ignore requires auth_mode=permissive"),
            "{error}"
        );
        let keys = ["--identity-file", "ids.json", "--signed-urls", "verify"];
        let config = load(&keys).unwrap();
        assert_eq!(config.auth_mode, AuthMode::Permissive);
        assert_eq!(config.effective_signed_urls(), SignedUrls::Verify);
        // The creator still labels permissive writes when the file only verifies signatures.
        let creator = ["--local-creator-email", "writer@example.invalid"];
        assert!(load(&[&keys[..], &creator].concat()).is_ok());
        for args in [
            &["--identity-file", "ids.json"][..],
            &["--identity-file", "ids.json", "--signed-urls", "ignore"],
        ] {
            let error = load(args).unwrap_err().to_string();
            assert!(
                error.contains("identity_file requires auth_mode=strict, or signed_urls=verify"),
                "{error}"
            );
        }
        assert_rejected(
            "signed_urls",
            &["", "true", "Verify", "strict"],
            "ignore or verify",
        );
    }

    #[test]
    fn local_creator_email_defaults_validates_and_requires_the_native_profile() {
        let empty = BTreeMap::new();
        assert_eq!(
            Config::load(&[], &empty).unwrap().local_creator_email,
            DEFAULT_LOCAL_CREATOR_EMAIL
        );
        let env = BTreeMap::from([(
            "GCS_MIMIC_LOCAL_CREATOR_EMAIL".into(),
            "env@example.test".into(),
        )]);
        assert_eq!(
            Config::load(&[], &env).unwrap().local_creator_email,
            "env@example.test"
        );
        let cli = ["--local-creator-email", ""].map(str::to_owned);
        assert_eq!(Config::load(&cli, &env).unwrap().local_creator_email, "");
        for bad in ["plain", "a b@example.test", "@example.test", "user-x"] {
            let args = ["--local-creator-email", bad].map(str::to_owned);
            assert!(
                Config::load(&args, &empty)
                    .unwrap_err()
                    .to_string()
                    .contains("local_creator_email"),
                "{bad}"
            );
        }
        // Strict mode records the registered caller, so an explicit creator is rejected rather
        // than silently ignored; the same holds through the environment.
        let strict = [
            "--auth-mode",
            "strict",
            "--identity-file",
            "identities.json",
            "--local-creator-email",
            "a@example.test",
        ]
        .map(str::to_owned);
        assert!(
            Config::load(&strict, &empty)
                .unwrap_err()
                .to_string()
                .contains("applies only to auth_mode=permissive")
        );
        let strict_env = BTreeMap::from([(
            "GCS_MIMIC_LOCAL_CREATOR_EMAIL".into(),
            "a@example.test".into(),
        )]);
        let strict_args = [
            "--auth-mode",
            "strict",
            "--identity-file",
            "identities.json",
        ]
        .map(str::to_owned);
        assert!(
            Config::load(&strict_args, &strict_env)
                .unwrap_err()
                .to_string()
                .contains("applies only to auth_mode=permissive")
        );
        let legacy = ["--compat-profile", "legacy-emulator"].map(str::to_owned);
        assert!(Config::load(&legacy, &empty).is_ok());
        let mut conflict = legacy.to_vec();
        conflict.extend(["--local-creator-email".into(), "a@example.test".into()]);
        assert!(
            Config::load(&conflict, &empty)
                .unwrap_err()
                .to_string()
                .contains("requires compat_profile=native")
        );
    }

    #[test]
    fn default_location_validates_precedence_and_native_profile() {
        let empty = BTreeMap::new();
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"default_location":"EU"}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_DEFAULT_LOCATION".into(), "ASIA".into())]);
        assert_precedence(
            &args,
            &env,
            &["--default-location", "US"],
            ["US".to_owned(), "EU".into(), "ASIA".into(), "US".into()],
            Config::load,
            |config| config.default_location,
        );
        std::fs::write(&file, r#"{"default_location":null}"#).unwrap();
        assert_eq!(Config::load(&args, &empty).unwrap().default_location, "US");
        std::fs::write(&file, r#"{"default_location":"EU"}"#).unwrap();
        assert_rejected(
            "default_location",
            &[" EU", "EU ", "unknown", "MARS-CENTRAL1"],
            "US or ASIA-NORTHEAST1",
        );
        for value in ["US", "EU", "ASIA", "ASIA-NORTHEAST1", "EUR4", "NAM4"] {
            let args = ["--default-location", value].map(str::to_owned);
            assert_eq!(Config::load(&args, &empty).unwrap().default_location, value);
            let mut args = args.to_vec();
            args.extend(["--compat-profile".into(), "legacy-emulator".into()]);
            assert!(
                Config::load(&args, &empty)
                    .unwrap_err()
                    .to_string()
                    .contains("requires compat_profile=native")
            );
        }
        for (value, expected) in [
            ("", "US"),
            ("us", "US"),
            ("asia-northeast1", "ASIA-NORTHEAST1"),
        ] {
            let args = ["--default-location", value].map(str::to_owned);
            assert_eq!(
                Config::load(&args, &empty).unwrap().default_location,
                expected
            );
        }
        let args = [
            "--config".into(),
            file.to_str().unwrap().into(),
            "--compat-profile".into(),
            "legacy-emulator".into(),
        ];
        assert!(Config::load(&args, &empty).is_err());
        assert!(
            Config::load(&["--compat-profile".into(), "legacy-emulator".into()], &env).is_err()
        );
    }

    #[test]
    fn log_level_defaults_precedence_alias_and_invalid_values() {
        let directory = TestDir::new();
        let file = directory.0.join("settings.json");
        std::fs::write(&file, r#"{"log_level":"debug"}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_LOG_LEVEL".into(), "error".into())]);
        let args = assert_precedence(
            &args,
            &env,
            &["--log-level", "warning"],
            [
                LogLevel::Info,
                LogLevel::Debug,
                LogLevel::Error,
                LogLevel::Warn,
            ],
            Config::load,
            |config| config.log_level,
        );
        assert_rejected(
            "log_level",
            &["", "trace", "off", "fatal", "INFO", "secret\n"],
            "log_level must",
        );
        for invalid in ["", "trace", "off", "fatal", "INFO", "secret\n"] {
            std::fs::write(&file, serde_json::json!({"log_level":invalid}).to_string()).unwrap();
            let env = BTreeMap::from([("GCS_MIMIC_LOG_LEVEL".into(), invalid.into())]);
            let report = Config::preflight(&[], &env);
            assert_eq!(report.diagnostics.errors[0].key, "GCS_MIMIC_LOG_LEVEL");
            assert!(!report.diagnostics.to_string().contains("secret"));
            assert_eq!(Config::load(&args, &env).unwrap().log_level, LogLevel::Warn);
        }
    }

    #[cfg(unix)]
    #[test]
    fn bounded_config_reads_preserve_fifo_inputs() {
        use std::io::Write;
        let directory = crate::test_support::TestDir::new();
        let file = directory.0.join("config-input");
        crate::test_support::fifo(&file);
        let writer_file = file.clone();
        let writer = std::thread::spawn(move || {
            std::fs::File::create(writer_file)
                .unwrap()
                .write_all(br#"{"worker_threads":3}"#)
                .unwrap()
        });
        let result = Config::load(
            &["--config".into(), file.to_str().unwrap().into()],
            &BTreeMap::new(),
        );
        writer.join().unwrap();
        assert_eq!(result.unwrap().worker_threads, 3);
    }

    #[test]
    fn bucket_initializer_selectors_follow_precedence_and_reject_empty_paths() {
        let directory = crate::test_support::TestDir::new();
        let file = directory.0.join("config.json");
        std::fs::write(&file, r#"{"buckets_file":"file-buckets.json"}"#).unwrap();
        for (source, key, args, env) in [
            (
                DiagnosticSource::Cli,
                "--buckets-file",
                vec!["--buckets-file".into(), "buckets.json".into()],
                BTreeMap::new(),
            ),
            (
                DiagnosticSource::Environment,
                "GCS_MIMIC_BUCKETS_FILE",
                vec![],
                BTreeMap::from([("GCS_MIMIC_BUCKETS_FILE".into(), "buckets.json".into())]),
            ),
            (
                DiagnosticSource::Config,
                "buckets_file",
                vec!["--config".into(), file.to_str().unwrap().into()],
                BTreeMap::new(),
            ),
        ] {
            let loaded = Config::load(&args, &env).unwrap();
            assert!(matches!(loaded.buckets, Some(DocumentSource::File(_))));
            let args = if source == DiagnosticSource::Cli {
                vec!["--buckets-file".into(), "".into()]
            } else {
                args
            };
            let env = if source == DiagnosticSource::Environment {
                BTreeMap::from([("GCS_MIMIC_BUCKETS_FILE".into(), "".into())])
            } else {
                env
            };
            if source == DiagnosticSource::Config {
                std::fs::write(&file, r#"{"buckets_file":""}"#).unwrap();
            }
            let errors = Config::load(&args, &env).unwrap_err();
            assert_eq!(errors.errors.len(), 1);
            assert_eq!(errors.errors[0].source, source);
            assert_eq!(errors.errors[0].key, key);
            assert!(errors.errors[0].fix.contains("must not be empty"));
        }
        std::fs::write(&file, r#"{"buckets_file":"file.json"}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_BUCKETS_FILE".into(), "env.json".into())]);
        assert_precedence(
            &args,
            &env,
            &["--buckets-file", "cli.json"],
            [
                None,
                Some(DocumentSource::File("file.json".into())),
                Some(DocumentSource::File("env.json".into())),
                Some(DocumentSource::File("cli.json".into())),
            ],
            Config::load,
            |config| config.buckets,
        );
        assert_eq!(Config::default().buckets, None);
    }

    #[test]
    fn buckets_file_mode_defaults_to_create_and_requires_a_buckets_file() {
        let directory = crate::test_support::TestDir::new();
        let file = directory.0.join("config.json");
        std::fs::write(
            &file,
            r#"{"buckets_file":"buckets.json","buckets_file_mode":"reconcile"}"#,
        )
        .unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_BUCKETS_FILE_MODE".into(), "create".into())]);
        assert_precedence(
            &args,
            &env,
            &["--buckets-file-mode", "reconcile"],
            [
                None,
                Some(BucketsFileMode::Reconcile),
                Some(BucketsFileMode::Create),
                Some(BucketsFileMode::Reconcile),
            ],
            Config::load,
            |config| config.buckets_file_mode,
        );
        // A file creates what is missing unless asked; config::document::tests covers inline.
        assert_eq!(
            BucketsFileMode::resolve(None, Form::File),
            BucketsFileMode::Create
        );
        assert_rejected(
            "buckets_file_mode",
            &["", "Reconcile", "merge", "true"],
            "create or reconcile",
        );
        // Each source of the mode is refused alone, and the fix names the missing setting.
        std::fs::write(&file, r#"{"buckets_file_mode":"create"}"#).unwrap();
        for (key, args, env) in [
            (
                "--buckets-file-mode",
                vec!["--buckets-file-mode".to_owned(), "reconcile".into()],
                BTreeMap::new(),
            ),
            (
                "GCS_MIMIC_BUCKETS_FILE_MODE",
                vec![],
                BTreeMap::from([("GCS_MIMIC_BUCKETS_FILE_MODE".into(), "reconcile".into())]),
            ),
            (
                "buckets_file_mode",
                vec!["--config".into(), file.to_str().unwrap().into()],
                BTreeMap::new(),
            ),
        ] {
            let errors = Config::load(&args, &env).unwrap_err().errors;
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert_eq!(errors[0].key, key);
            assert!(
                errors[0].fix.contains("requires buckets_file"),
                "{errors:?}"
            );
        }
        // An invalid selector is reported once, without a second error about the mode.
        let errors = Config::load(
            &["--buckets-file", "", "--buckets-file-mode", "reconcile"].map(str::to_owned),
            &BTreeMap::new(),
        )
        .unwrap_err()
        .errors;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].key, "--buckets-file");
    }

    #[test]
    fn effective_validation_aggregates_without_cascades_and_keeps_native_precedence() {
        let env = BTreeMap::from([
            (
                "GCS_MIMIC_WORKER_THREADS".into(),
                "bad-lower-priority".into(),
            ),
            ("GCS_MIMIC_MAX_CONNECTIONS".into(), "0".into()),
            ("GCS_MIMIC_AUTH_MODE".into(), "bad-secret-mode".into()),
            ("GCS_MIMIC_IDENTITY_FILE".into(), "ids.json".into()),
        ]);
        let args = [
            "--worker-threads",
            "3",
            "--listen",
            "invalid-secret-listen",
            "--tls-cert",
            "",
            "--tls-key",
            "key.pem",
        ]
        .map(String::from);
        let report = Config::preflight(&args, &env);
        assert_eq!(report.config.worker_threads, 3);
        assert_eq!(report.diagnostics.errors.len(), 4);
        assert!(report.valid(&["worker_threads"]));
        assert!(!report.valid(&["auth_mode", "tls_cert"]));
        assert!(!report.diagnostics.to_string().contains("secret"));
        assert!(!report.diagnostics.to_string().contains("supplied together"));
        assert!(
            !report
                .diagnostics
                .to_string()
                .contains("requires auth_mode")
        );
    }

    #[test]
    fn config_occurrence_limits_count_duplicate_json_keys_and_all_input_sources() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        let args = ["--config".into(), file.to_str().unwrap().into()];
        for (count, accepted) in [(1023, true), (1024, false), (1025, false), (1026, false)] {
            let fields = vec!["\"max_connections\":32"; count].join(",");
            std::fs::write(&file, format!("{{{fields}}}")).unwrap();
            let report = Config::preflight(&args, &BTreeMap::new());
            assert_eq!(!report.diagnostics.is_err(), accepted, "{count}");
            if !accepted {
                assert!(
                    report
                        .diagnostics
                        .to_string()
                        .to_lowercase()
                        .contains("limit")
                );
            }
        }
        for bytes in [256, 257] {
            std::fs::write(&file, format!("{{\"{}\":1}}", "x".repeat(bytes))).unwrap();
            let report = Config::preflight(&args, &BTreeMap::new());
            assert_eq!(
                report
                    .diagnostics
                    .to_string()
                    .to_lowercase()
                    .contains("limit"),
                bytes > 256
            );
        }
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn native_syntax_errors_preserve_invalid_prerequisites_without_cascades() {
        for args in [
            vec!["--identity-file", "ids.json", "---auth-mode=strict"],
            vec!["--identity-file", "ids.json", "--auth-mode"],
            vec!["--tls-key", "key.pem", "---tls-cert=cert.pem"],
            vec!["--tls-key", "key.pem", "--tls-cert"],
            vec!["--tls-cert", "cert.pem", "--tls-key", "key.pem", "--scheme"],
        ] {
            let report = Config::preflight(
                &args.into_iter().map(String::from).collect::<Vec<_>>(),
                &BTreeMap::new(),
            );
            assert_eq!(report.diagnostics.errors.len(), 1, "{}", report.diagnostics);
            assert!(
                report.diagnostics.errors[0]
                    .fix
                    .contains("--name VALUE pairs")
            );
            assert!(!report.valid(&["auth_mode", "tls_cert", "scheme"]));
        }
        let env = BTreeMap::from([
            ("GCS_MIMIC_AUTH_MODE".into(), "strict".into()),
            ("GCS_MIMIC_IDENTITY_FILE".into(), "ids.json".into()),
        ]);
        let report = Config::preflight(&["---auth-mode=strict".into()], &env);
        assert_eq!(report.diagnostics.errors.len(), 1);
        assert!(!report.valid(&["auth_mode"]));
    }

    #[test]
    fn config_file_diagnostics_keep_the_selected_source_and_field() {
        let dir = TestDir::new();
        let missing = dir.0.join("missing.json");
        let report = Config::preflight(
            &[],
            &BTreeMap::from([("GCS_MIMIC_CONFIG".into(), missing.to_str().unwrap().into())]),
        );
        assert_eq!(report.diagnostics.errors.len(), 1);
        let diagnostic = &report.diagnostics.errors[0];
        assert_eq!(diagnostic.key, "GCS_MIMIC_CONFIG");
        assert!(diagnostic.fix.contains("GCS_MIMIC_CONFIG=config.json"));
        assert!(!diagnostic.fix.contains("--config"));
        let diagnostic = read_public_base_url(missing.to_str().unwrap()).unwrap_err();
        assert_eq!(diagnostic.key, "--config");
        assert!(diagnostic.fix.contains("--config PATH"));
        assert!(!diagnostic.fix.contains("GCS_MIMIC_CONFIG"));
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"named_bad_field":true}"#).unwrap();
        let diagnostic = read_public_base_url(file.to_str().unwrap()).unwrap_err();
        assert_eq!(diagnostic.source, DiagnosticSource::Config);
        assert_eq!(diagnostic.key, "named_bad_field");
        assert_eq!(diagnostic.fix, CONFIG_VALUE_FIX);
    }

    #[test]
    fn public_origin_merges_bounded_domains_and_preserves_input_precedence() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"public_base_url":"http://file.invalid"}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([(
            "GCS_MIMIC_PUBLIC_BASE_URL".into(),
            "https://Env.Invalid:443/".into(),
        )]);
        let mut args = assert_precedence(
            &args,
            &env,
            &[
                "--public-base-url",
                "http://Storage.Invalid:80/",
                "--xml-virtual-host-domains",
                "invalid,storage.invalid",
            ],
            [
                None,
                Some("http://file.invalid".into()),
                Some("https://env.invalid".into()),
                Some("http://storage.invalid".into()),
            ],
            Config::load,
            |config| {
                config
                    .public_base_url
                    .map(|origin| origin.as_str().to_owned())
            },
        );
        let config = Config::load(&args, &env).unwrap();
        assert_eq!(
            config.public_base_url.as_ref().map(PublicOrigin::as_str),
            Some("http://storage.invalid")
        );
        assert!(config.public_virtual_host_enabled());
        assert_eq!(
            config.xml_virtual_host_domains,
            ["storage.invalid", "invalid"]
        );
        args[5] = "storage.invalid,STORAGE.invalid".into();
        assert!(
            Config::load(&args, &env)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        args[5] = (0..32)
            .map(|i| format!("host{i}.invalid"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            Config::load(&args, &env)
                .unwrap_err()
                .to_string()
                .contains("at most 32")
        );
        args[3] = "http://host0.invalid".into();
        assert_eq!(
            Config::load(&args, &env)
                .unwrap()
                .xml_virtual_host_domains
                .len(),
            32
        );
        args[3] = "http://storage.invalid".into();
        args[5] = "invalid".into();
        args.extend(["--api-base-url".into(), "http://api.storage.invalid".into()]);
        assert!(
            Config::load(&args, &env)
                .unwrap_err()
                .to_string()
                .contains("api_base_url host selects")
        );
    }

    #[test]
    fn public_ip_origins_warn_without_disabling_explicit_domains_or_path_urls() {
        for base in ["http://127.0.0.1:80", "https://[::1]:443"] {
            let report = Config::preflight(
                &[
                    "--public-base-url".into(),
                    base.into(),
                    "--xml-virtual-host-domains".into(),
                    "storage.invalid".into(),
                ],
                &BTreeMap::new(),
            );
            assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
            assert_eq!(report.diagnostics.warnings.len(), 1);
            assert!(
                report.diagnostics.warnings[0]
                    .fix
                    .contains("virtual-host routing is disabled")
            );
            assert!(!report.config.public_virtual_host_enabled());
            assert_eq!(report.config.xml_virtual_host_domains, ["storage.invalid"]);
        }
        for base in [
            "http://storage.invalid/path",
            "http://storage.invalid?secret",
            "",
        ] {
            let report =
                Config::preflight(&["--public-base-url".into(), base.into()], &BTreeMap::new());
            assert_eq!(report.diagnostics.errors.len(), 1);
            assert!(report.diagnostics.warnings.is_empty());
        }
        let report = Config::preflight(
            &[
                "--public-base-url".into(),
                "http://127.0.0.1".into(),
                "--xml-virtual-host-domains".into(),
                "bad_domain".into(),
            ],
            &BTreeMap::new(),
        );
        assert_eq!(report.diagnostics.errors.len(), 1);
        assert_eq!(report.diagnostics.warnings.len(), 1);
        let report = Config::preflight(
            &[
                "--public-base-url".into(),
                "invalid".into(),
                "--xml-virtual-host-domains".into(),
                "storage.invalid".into(),
                "--api-base-url".into(),
                "http://api.storage.invalid".into(),
            ],
            &BTreeMap::new(),
        );
        assert_eq!(report.diagnostics.errors.len(), 2);
    }

    #[test]
    fn virtual_host_domains_are_opt_in_bounded_and_follow_precedence() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"xml_virtual_host_domains":"file.invalid"}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([(
            "GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS".into(),
            "env.invalid".into(),
        )]);
        let mut args = assert_precedence(
            &args,
            &env,
            &[
                "--xml-virtual-host-domains",
                "EXAMPLE.invalid, storage.example.invalid",
            ],
            [
                vec![],
                vec!["file.invalid".into()],
                vec!["env.invalid".into()],
                vec!["storage.example.invalid".into(), "example.invalid".into()],
            ],
            Config::load,
            |config| config.xml_virtual_host_domains,
        );
        *args.last_mut().unwrap() = String::new();
        assert!(
            Config::load(&args, &env)
                .unwrap()
                .xml_virtual_host_domains
                .is_empty()
        );
        for invalid in [
            "https://example.invalid",
            "example.invalid:4443",
            "*.example.invalid",
            "example.invalid.",
            "127.0.0.1",
            "::1",
            "bad_label.invalid",
            "-bad.invalid",
            "bad-.invalid",
            "a,,b",
            "a,A",
            "a/b",
        ] {
            *args.last_mut().unwrap() = invalid.into();
            assert!(Config::load(&args, &env).is_err(), "{invalid}");
        }
        *args.last_mut().unwrap() = (0..33)
            .map(|i| format!("host{i}.invalid"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(Config::load(&args, &env).is_err());
        args[3] = "example.invalid".into();
        args.extend([
            "--api-base-url".into(),
            "http://api.example.invalid:4443".into(),
        ]);
        assert!(
            Config::load(&args, &env)
                .unwrap_err()
                .to_string()
                .contains("api_base_url host selects an XML virtual bucket")
        );
        *args.last_mut().unwrap() = "http://example.invalid:4443".into();
        assert!(Config::load(&args, &env).is_ok());
        args[3] = "example.invalid,api.example.invalid".into();
        *args.last_mut().unwrap() = "http://api.example.invalid:4443".into();
        assert!(Config::load(&args, &env).is_ok());
        std::fs::remove_file(file).unwrap();
    }

    fn load_http(args: &[String], env: &BTreeMap<String, String>) -> Result<Config, Diagnostics> {
        let mut args = args.to_vec();
        args.extend(["--scheme".into(), "http".into()]);
        Config::load(&args, env)
    }

    #[test]
    fn tls_configuration_precedence_and_validation() {
        let defaults = Config::default();
        assert_eq!(defaults.scheme, Scheme::Https);
        assert_eq!(defaults.api_base_url, None);
        let loaded = Config::load(&[], &BTreeMap::new()).unwrap();
        assert_eq!(loaded.scheme, Scheme::Https);
        assert_eq!(loaded.tls_cert, None);
        assert_eq!(loaded.tls_key, None);
        assert_eq!(
            Config::load(
                &[
                    "--tls-cert".into(),
                    "cert.pem".into(),
                    "--tls-key".into(),
                    "key.pem".into()
                ],
                &BTreeMap::new()
            )
            .unwrap()
            .scheme,
            Scheme::Https
        );
        assert_eq!(load_http(&[], &BTreeMap::new()).unwrap().api_base_url, None);
        assert_eq!(defaults.tls_handshake_timeout.as_millis(), 5_000);
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"scheme":"https","tls_cert":"file-cert.pem","tls_key":"file-key.pem","tls_handshake_timeout_ms":100}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let loaded = Config::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(loaded.scheme, Scheme::Https);
        assert_eq!(loaded.api_base_url, None);
        let env = BTreeMap::from([
            ("GCS_MIMIC_TLS_CERT".into(), "env-cert.pem".into()),
            ("GCS_MIMIC_TLS_KEY".into(), "env-key.pem".into()),
            ("GCS_MIMIC_TLS_HANDSHAKE_TIMEOUT_MS".into(), "200".into()),
            (
                "GCS_MIMIC_API_BASE_URL".into(),
                "http://proxy.invalid/base".into(),
            ),
        ]);
        let loaded = Config::load(&args, &env).unwrap();
        assert_eq!(
            loaded.api_base_url.as_deref(),
            Some("http://proxy.invalid/base")
        );
        let cli = assert_precedence(
            &args,
            &env,
            &[
                "--tls-cert",
                "cli-cert.pem",
                "--tls-key",
                "cli-key.pem",
                "--tls-handshake-timeout-ms",
                "300",
            ],
            [
                (None, None, 5_000),
                (
                    Some("file-cert.pem".into()),
                    Some("file-key.pem".into()),
                    100,
                ),
                (Some("env-cert.pem".into()), Some("env-key.pem".into()), 200),
                (Some("cli-cert.pem".into()), Some("cli-key.pem".into()), 300),
            ],
            Config::load,
            |config| {
                (
                    config.tls_cert,
                    config.tls_key,
                    config.tls_handshake_timeout.as_millis(),
                )
            },
        );
        for args in [
            vec!["--scheme", "HTTPS"],
            vec!["--scheme", "https", "--tls-cert", "cert.pem"],
            vec!["--scheme", "https", "--tls-key", "key.pem"],
            vec!["--tls-cert", "cert.pem"],
            vec!["--tls-key", "key.pem"],
            vec![
                "--scheme",
                "http",
                "--tls-cert",
                "cert.pem",
                "--tls-key",
                "key.pem",
            ],
            vec!["--scheme", "http", "--tls-handshake-timeout-ms", "5000"],
            vec![
                "--scheme",
                "https",
                "--tls-cert",
                "",
                "--tls-key",
                "key.pem",
            ],
        ] {
            assert!(
                Config::load(
                    &args.into_iter().map(String::from).collect::<Vec<_>>(),
                    &BTreeMap::new()
                )
                .is_err()
            );
        }
        for invalid in ["18446744073709551616", "-1", "unlimited"] {
            let mut args = cli.clone();
            *args.last_mut().unwrap() = invalid.into();
            assert!(Config::load(&args, &env).is_err());
        }
        for bound in [0, 1, u64::MAX] {
            let mut args = cli.clone();
            *args.last_mut().unwrap() = bound.to_string();
            assert_eq!(
                Config::load(&args, &env)
                    .unwrap()
                    .tls_handshake_timeout
                    .as_millis(),
                bound
            );
        }
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn dual_listener_defaults_precedence_and_validation() {
        let directory = crate::test_support::TestDir::new();
        let file = directory.0.join("config.json");
        std::fs::write(
            &file,
            r#"{"scheme":"both","listen":"[::1]:4444","listen_http":"[::1]:8001"}"#,
        )
        .unwrap();
        let mut args = vec!["--config".into(), file.to_str().unwrap().into()];
        let mut env = BTreeMap::new();
        assert_eq!(
            Config::load(&args, &env).unwrap().effective_listen_http(),
            Some("[::1]:8001".parse().unwrap())
        );
        env.insert("GCS_MIMIC_LISTEN_HTTP".into(), "[::1]:8002".into());
        assert_eq!(
            Config::load(&args, &env).unwrap().effective_listen_http(),
            Some("[::1]:8002".parse().unwrap())
        );
        args.extend(["--listen-http".into(), "[::1]:0".into()]);
        assert_eq!(
            Config::load(&args, &env).unwrap().effective_listen_http(),
            Some("[::1]:0".parse().unwrap())
        );
        for listen in ["127.0.0.2:4443", "[::]:0"] {
            let config = Config::load(
                &[
                    "--scheme".into(),
                    "both".into(),
                    "--listen".into(),
                    listen.into(),
                ],
                &BTreeMap::new(),
            )
            .unwrap();
            assert_eq!(
                config.effective_listen_http(),
                Some(SocketAddr::new(config.listen.ip(), 8000))
            );
            assert!(
                config
                    .effective_api_base_url(config.listen)
                    .starts_with("https://")
            );
        }
        for arguments in [
            vec!["--scheme", "http", "--listen-http", "127.0.0.1:0"],
            vec!["--listen-http", "127.0.0.1:0"],
            vec!["--scheme", "both", "--listen", "127.0.0.1:8000"],
            vec![
                "--scheme",
                "both",
                "--listen",
                "127.0.0.1:4443",
                "--listen-http",
                "127.0.0.1:4443",
            ],
            vec!["--scheme", "both", "--listen-http", "localhost:8000"],
            vec!["--scheme", "both", "--listen-http", ""],
            vec!["--scheme", "both", "--tls-cert", "cert.pem"],
        ] {
            let arguments: Vec<_> = arguments.into_iter().map(str::to_owned).collect();
            assert!(
                Config::load(&arguments, &BTreeMap::new()).is_err(),
                "{arguments:?}"
            );
        }
        let config = Config::load(
            &[
                "--scheme".into(),
                "both".into(),
                "--listen".into(),
                "127.0.0.1:0".into(),
                "--listen-http".into(),
                "127.0.0.1:0".into(),
            ],
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(config.listen, config.effective_listen_http().unwrap());
        let config = Config::load(
            &[],
            &BTreeMap::from([("GCS_MIMIC_SCHEME".into(), "both".into())]),
        )
        .unwrap();
        assert_eq!(config.scheme, Scheme::Both);
    }

    #[test]
    fn compat_profile_precedence_and_validation() {
        assert_eq!(Config::default().compat_profile, CompatProfile::Native);
        let dir = TestDir::new();
        let path = dir.0.join("config.json");
        std::fs::write(&path, r#"{"compat_profile":"legacy-emulator"}"#).unwrap();
        let args = vec!["--config".into(), path.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_COMPAT_PROFILE".into(), "native".into())]);
        let args = assert_precedence(
            &args,
            &env,
            &["--compat-profile", "legacy-emulator"],
            [
                CompatProfile::Native,
                CompatProfile::LegacyEmulator,
                CompatProfile::Native,
                CompatProfile::LegacyEmulator,
            ],
            load_http,
            |config| config.compat_profile,
        );
        assert_rejected(
            "compat_profile",
            &["", "NATIVE", "legacy_emulator", "permissive", "unknown"],
            "compat_profile",
        );
        for body in [
            r#"{"compat_profile":false}"#,
            r#"{"compat_profile":null}"#,
            r#"{"compat_profile":1}"#,
        ] {
            std::fs::write(&path, body).unwrap();
            assert!(load_http(&args[..2], &BTreeMap::new()).is_err());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn seed_limits_default_to_unlimited_and_reject_values_out_of_range() {
        let limit = NonZeroUsize::new;
        let default = Config::default();
        assert_eq!(
            (default.max_seed_entries, default.max_seed_depth),
            (None, None)
        );
        let args = ["--max-seed-entries", "20000", "--max-seed-depth", "40"].map(str::to_owned);
        let config = Config::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(
            (config.max_seed_entries, config.max_seed_depth),
            (limit(20_000), limit(40))
        );
        let env = BTreeMap::from([("GCS_MIMIC_MAX_SEED_DEPTH".to_owned(), "50".to_owned())]);
        assert_eq!(Config::load(&[], &env).unwrap().max_seed_depth, limit(50));
        let args = ["--max-seed-entries", "0", "--max-seed-depth", "0"].map(str::to_owned);
        let config = Config::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(
            (config.max_seed_entries, config.max_seed_depth),
            (None, None)
        );
        assert_rejected(
            "max_seed_entries",
            &["-1", "18446744073709551616"],
            "max_seed_entries must be an integer in 0..=18446744073709551615",
        );
        assert_rejected(
            "max_seed_depth",
            &["-1", "1025"],
            "max_seed_depth must be an integer in 0..=1024",
        );
    }

    #[test]
    fn the_bucket_batch_limit_defaults_to_unlimited_and_rejects_values_out_of_range() {
        let limit = NonZeroUsize::new;
        assert_eq!(Config::default().max_buckets_per_batch, None);
        let args = ["--max-buckets-per-batch", "1000000"].map(str::to_owned);
        let config = Config::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(config.max_buckets_per_batch, limit(1_000_000));
        let env = BTreeMap::from([("GCS_MIMIC_MAX_BUCKETS_PER_BATCH".to_owned(), "1".to_owned())]);
        assert_eq!(
            Config::load(&[], &env).unwrap().max_buckets_per_batch,
            limit(1)
        );
        let args = ["--max-buckets-per-batch", "0"].map(str::to_owned);
        let config = Config::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(config.max_buckets_per_batch, None);
        let directory = TestDir::new();
        let file = directory.0.join("config.json");
        std::fs::write(&file, r#"{"max_buckets_per_batch":2000}"#).unwrap();
        let args = ["--config".to_owned(), file.to_str().unwrap().to_owned()];
        assert_eq!(
            Config::load(&args, &BTreeMap::new())
                .unwrap()
                .max_buckets_per_batch,
            limit(2000)
        );
        assert_rejected(
            "max_buckets_per_batch",
            &["-1", "18446744073709551616"],
            "max_buckets_per_batch must be an integer in 0..=18446744073709551615",
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn sync_tree_limits_default_to_unlimited_and_mean_nothing_without_a_sync_dir() {
        let default = Config::default();
        assert_eq!(
            (default.max_sync_entries, default.max_sync_depth),
            (None, None)
        );
        let tree = TestDir::new();
        let data = TestDir::new();
        let args = [
            "--sync-dir",
            tree.0.to_str().unwrap(),
            "--data-dir",
            data.0.to_str().unwrap(),
            "--max-sync-entries",
            "100000000",
            "--max-sync-depth",
            "1024",
        ]
        .map(str::to_owned);
        let config = Config::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(
            (config.max_sync_entries, config.max_sync_depth),
            (NonZeroUsize::new(100_000_000), NonZeroUsize::new(1024))
        );
        let args = [
            "--sync-dir",
            tree.0.to_str().unwrap(),
            "--data-dir",
            data.0.to_str().unwrap(),
            "--max-sync-entries",
            "0",
            "--max-sync-depth",
            "0",
        ]
        .map(str::to_owned);
        let config = Config::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(
            (config.max_sync_entries, config.max_sync_depth),
            (None, None)
        );
        for (flag, env) in [
            ("--max-sync-entries", "GCS_MIMIC_MAX_SYNC_ENTRIES"),
            ("--max-sync-depth", "GCS_MIMIC_MAX_SYNC_DEPTH"),
        ] {
            for (key, args, env) in [
                (flag, vec![flag.to_owned(), "5".to_owned()], BTreeMap::new()),
                (
                    env,
                    Vec::new(),
                    BTreeMap::from([(env.to_owned(), "5".to_owned())]),
                ),
            ] {
                let errors = Config::load(&args, &env).unwrap_err();
                assert!(
                    errors
                        .errors
                        .iter()
                        .any(|error| error.key == key && error.fix.contains("requires sync_dir")),
                    "{errors}"
                );
            }
        }
        for (setting, values, fix) in [
            (
                "--max-sync-entries",
                ["-1", "18446744073709551616"],
                "max_sync_entries must be an integer in 0..=18446744073709551615",
            ),
            (
                "--max-sync-depth",
                ["-1", "1025"],
                "max_sync_depth must be an integer in 0..=1024",
            ),
        ] {
            for value in values {
                let args = [
                    "--sync-dir",
                    tree.0.to_str().unwrap(),
                    "--data-dir",
                    data.0.to_str().unwrap(),
                    setting,
                    value,
                ]
                .map(str::to_owned);
                let errors = Config::load(&args, &BTreeMap::new()).unwrap_err();
                assert!(
                    errors.errors.iter().any(|error| error.fix.contains(fix)),
                    "{errors}"
                );
            }
        }
    }

    #[test]
    fn session_limits_default_to_unlimited_and_zero_stays_outside_the_config() {
        let default = Config::default();
        assert_eq!(default.max_resumable_sessions, None);
        assert_eq!(default.max_rewrite_sessions, None);
        type Read = fn(&Config) -> Option<NonZeroU32>;
        let settings: [(&str, Read); 2] = [
            ("max_resumable_sessions", |config| {
                config.max_resumable_sessions
            }),
            ("max_rewrite_sessions", |config| config.max_rewrite_sessions),
        ];
        for (setting, read) in settings {
            let flag = format!("--{}", setting.replace('_', "-"));
            let load = |value: &str| {
                Config::load(&[flag.clone(), value.to_owned()], &BTreeMap::new()).unwrap()
            };
            assert_eq!(read(&load("0")), None);
            assert_eq!(read(&load("1")), NonZeroU32::new(1));
            assert_eq!(read(&load("4294967295")), NonZeroU32::new(u32::MAX));
            assert_rejected(
                setting,
                &["4294967296", "-1", "many"],
                &format!("{setting} must be an integer in 0..=4294967295"),
            );
        }
        let env = BTreeMap::from([(
            "GCS_MIMIC_MAX_RESUMABLE_SESSIONS".to_owned(),
            "9".to_owned(),
        )]);
        assert_eq!(
            Config::load(&[], &env).unwrap().max_resumable_sessions,
            NonZeroU32::new(9)
        );
    }

    #[test]
    fn transfer_timeout_precedence_and_bounds() {
        type Field = fn(Config) -> TimeLimit;
        let settings: [(&str, u64, Field); 7] = [
            ("header_read_timeout_ms", 30_000, |config| {
                config.header_read_timeout
            }),
            ("body_inactivity_timeout_ms", 0, |config| {
                config.body_inactivity_timeout
            }),
            ("request_timeout_ms", 180_000, |config| {
                config.request_timeout
            }),
            ("object_request_timeout_ms", 0, |config| {
                config.object_request_timeout
            }),
            ("catalog_wait_timeout_ms", 120_000, |config| {
                config.catalog_wait_timeout
            }),
            ("keep_alive_timeout_ms", 120_000, |config| {
                config.keep_alive_timeout
            }),
            ("shutdown_timeout_ms", 5_000, |config| {
                config.shutdown_timeout
            }),
        ];
        for (setting, default, limit) in settings {
            let dir = TestDir::new();
            let file = dir.0.join("config.json");
            let value = |config: Config| limit(config).as_millis();
            let flag = format!("--{}", setting.replace('_', "-"));
            let key = format!("GCS_MIMIC_{}", setting.to_ascii_uppercase());
            let args = vec!["--config".into(), file.to_str().unwrap().into()];
            std::fs::write(&file, format!(r#"{{"{setting}":100}}"#)).unwrap();
            let env = BTreeMap::from([(key.clone(), "200".into())]);
            assert_precedence(
                &args,
                &env,
                &[&flag, "300"],
                [default, 100, 200, 300],
                load_http,
                value,
            );
            assert_rejected(
                setting,
                &["-1", "18446744073709551616", "unlimited"],
                "must be an integer",
            );
            for bound in [0, 1, u64::MAX] {
                assert_eq!(
                    value(load_http(&[flag.clone(), bound.to_string()], &BTreeMap::new()).unwrap()),
                    bound
                );
            }
            std::fs::remove_file(file).unwrap();
        }
    }

    /// The three settings `connection_timeout_ms` stands for, in that order.
    fn split_timeouts(config: &Config) -> [u64; 3] {
        [
            config.header_read_timeout,
            config.body_inactivity_timeout,
            config.request_timeout,
        ]
        .map(TimeLimit::as_millis)
    }

    #[test]
    fn connection_timeout_alias_fills_every_setting_it_replaces() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"connection_timeout_ms":100}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_CONNECTION_TIMEOUT_MS".into(), "200".into())]);
        assert_precedence(
            &args,
            &env,
            &["--connection-timeout-ms", "300"],
            [[30_000, 0, 180_000], [100; 3], [200; 3], [300; 3]],
            load_http,
            |config| split_timeouts(&config),
        );
        assert_rejected(
            "connection_timeout_ms",
            &["-1", "18446744073709551616", "unlimited"],
            "must be an integer",
        );
        for bound in [0, u64::MAX] {
            let flag = ["--connection-timeout-ms".to_owned(), bound.to_string()];
            let config = load_http(&flag, &BTreeMap::new()).unwrap();
            assert_eq!(split_timeouts(&config), [bound; 3]);
        }
    }

    /// `body_inactivity_timeout_ms` sorts before the alias and the others after it, so each one
    /// is checked: no name order may let the alias overwrite an explicit value.
    #[test]
    fn an_explicit_timeout_beats_the_alias_from_any_source() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        for (index, setting) in [
            "body_inactivity_timeout_ms",
            "header_read_timeout_ms",
            "request_timeout_ms",
        ]
        .into_iter()
        .enumerate()
        {
            let mut expected = [7; 3];
            let order = [1, 0, 2][index];
            expected[order] = 9;
            let flag = format!("--{}", setting.replace('_', "-"));
            let env_key = format!("GCS_MIMIC_{}", setting.to_ascii_uppercase());
            let alias_cli = ["--connection-timeout-ms".to_owned(), "7".to_owned()];
            let alias_env =
                BTreeMap::from([("GCS_MIMIC_CONNECTION_TIMEOUT_MS".into(), "7".to_owned())]);

            // Same source.
            let args = [alias_cli[0].clone(), "7".into(), flag.clone(), "9".into()];
            let loaded = load_http(&args, &BTreeMap::new()).unwrap();
            assert_eq!(split_timeouts(&loaded), expected, "{setting} on the CLI");
            std::fs::write(
                &file,
                format!(r#"{{"connection_timeout_ms":7,"{setting}":9}}"#),
            )
            .unwrap();
            let config_args = ["--config".to_owned(), file.to_str().unwrap().into()];
            let loaded = load_http(&config_args, &BTreeMap::new()).unwrap();
            assert_eq!(split_timeouts(&loaded), expected, "{setting} in the file");

            // The alias from a source that otherwise takes precedence.
            let env = BTreeMap::from([(env_key, "9".to_owned())]);
            let loaded = load_http(&alias_cli, &env).unwrap();
            assert_eq!(split_timeouts(&loaded), expected, "{setting} over the CLI");
            std::fs::write(&file, format!(r#"{{"{setting}":9}}"#)).unwrap();
            let loaded = load_http(&config_args, &alias_env).unwrap();
            assert_eq!(split_timeouts(&loaded), expected, "{setting} over the env");
        }
    }

    #[test]
    fn capacity_settings_accept_their_raised_bounds_only() {
        for (setting, maximum, value) in [
            (
                "max_connections",
                1_048_576,
                (|config| config.max_connections as u64) as fn(Config) -> u64,
            ),
            ("worker_threads", 1024, |config| {
                config.worker_threads as u64
            }),
            ("max_blocking_threads", 1024, |config| {
                config.max_blocking_threads as u64
            }),
            ("memory_max_bytes", 1024_u64.pow(4), |config| {
                config.memory_max_bytes
            }),
            ("memory_catalog_max_bytes", 1024_u64.pow(4), |config| {
                config.memory_catalog_max_bytes
            }),
        ] {
            let flag = format!("--{}", setting.replace('_', "-"));
            let mut args = vec![flag, maximum.to_string()];
            if setting.starts_with("memory") {
                args.extend(["--backend".into(), "memory".into()]);
            }
            assert_eq!(
                value(load_http(&args, &BTreeMap::new()).unwrap()),
                maximum,
                "{setting}"
            );
            assert_rejected(setting, &[(maximum + 1).to_string()], "must be an integer");
        }
    }

    #[test]
    fn object_limit_precedence_and_bounds() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"max_object_bytes":64}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_MAX_OBJECT_BYTES".into(), "128".into())]);
        assert_precedence(
            &args,
            &env,
            &["--max-object-bytes", "256"],
            [5 * 1024_u64.pow(4), 64, 128, 256],
            load_http,
            |config| config.max_object_bytes,
        );
        assert_rejected(
            "max_object_bytes",
            &["0", "-1", "5497558138881", "unlimited"],
            "must be an integer",
        );
        assert_eq!(
            load_http(
                &["--max-object-bytes".into(), "5497558138880".into()],
                &BTreeMap::new()
            )
            .unwrap()
            .max_object_bytes,
            5 * 1024_u64.pow(4)
        );
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn api_origin_defaults_use_bound_addresses_and_preflight_hosts() {
        for (bound, expected) in [
            ("0.0.0.0:54321", "https://127.0.0.1:54321"),
            ("[::]:54321", "https://[::1]:54321"),
            ("127.0.0.2:54321", "https://127.0.0.2:54321"),
            ("[::1]:54321", "https://[::1]:54321"),
        ] {
            assert_eq!(
                Config::default().effective_api_base_url(bound.parse().unwrap()),
                expected
            );
        }
        for listen in ["127.0.0.1:0", "0.0.0.0:0"] {
            let args = [
                "--listen".into(),
                listen.into(),
                "--xml-virtual-host-domains".into(),
                "0.1".into(),
            ];
            assert!(
                load_http(&args, &BTreeMap::new())
                    .unwrap_err()
                    .to_string()
                    .contains("api_base_url host selects")
            );
            let explicit = [
                args.to_vec(),
                vec![
                    "--api-base-url".into(),
                    "https://proxy.invalid/prefix/".into(),
                ],
            ]
            .concat();
            let config = load_http(&explicit, &BTreeMap::new()).unwrap();
            assert_eq!(
                config.effective_api_base_url("127.0.0.1:12345".parse().unwrap()),
                "https://proxy.invalid/prefix"
            );
        }
    }

    #[test]
    fn advertised_endpoint_precedence_and_validation() {
        let env = BTreeMap::from([(
            "GCS_MIMIC_API_BASE_URL".into(),
            "https://example.invalid/base/".into(),
        )]);
        assert_eq!(
            load_http(&[], &env).unwrap().api_base_url.as_deref(),
            Some("https://example.invalid/base")
        );
        assert_eq!(
            load_http(
                &["--api-base-url".into(), "http://127.0.0.1:1234".into()],
                &env
            )
            .unwrap()
            .api_base_url
            .as_deref(),
            Some("http://127.0.0.1:1234")
        );
        for value in [
            "relative",
            "ftp://example.invalid",
            "http://a@b",
            "http://a?q=x",
            "http://a/#fragment",
            "http://",
            "http://a\r\n",
        ] {
            assert!(
                load_http(&["--api-base-url".into(), value.into()], &BTreeMap::new()).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn authentication_profile_precedence_and_validation() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let defaults = load_http(&[], &BTreeMap::new()).unwrap();
        assert_eq!(defaults.auth_mode, AuthMode::Permissive);
        assert_eq!(defaults.identities, None);
        std::fs::write(
            &file,
            r#"{"auth_mode":"strict","identity_file":"file-ids.json"}"#,
        )
        .unwrap();
        let loaded = load_http(&args, &BTreeMap::new()).unwrap();
        assert_eq!(loaded.auth_mode, AuthMode::Strict);
        let env = BTreeMap::from([("GCS_MIMIC_IDENTITY_FILE".into(), "env-ids.json".into())]);
        assert_precedence(
            &args,
            &env,
            &["--identity-file", "cli-ids.json"],
            [
                None,
                Some(DocumentSource::File("file-ids.json".into())),
                Some(DocumentSource::File("env-ids.json".into())),
                Some(DocumentSource::File("cli-ids.json".into())),
            ],
            load_http,
            |config| config.identities,
        );
        // A higher-precedence permissive mode with a lower-precedence registry is a conflict.
        let permissive_env = BTreeMap::from([("GCS_MIMIC_AUTH_MODE".into(), "permissive".into())]);
        assert!(
            load_http(&args, &permissive_env)
                .unwrap_err()
                .to_string()
                .contains("identity_file requires auth_mode=strict")
        );
        let mut strict_cli = vec!["--auth-mode".into(), "strict".into()];
        assert!(
            load_http(&strict_cli, &permissive_env)
                .unwrap_err()
                .to_string()
                .contains("auth_mode=strict requires identity_file")
        );
        strict_cli.extend(["--identity-file".into(), "ids.json".into()]);
        assert_eq!(
            load_http(&strict_cli, &permissive_env).unwrap().auth_mode,
            AuthMode::Strict
        );
        assert!(
            load_http(
                &["--identity-file".into(), "ids.json".into()],
                &BTreeMap::new()
            )
            .unwrap_err()
            .to_string()
            .contains("identity_file requires auth_mode=strict")
        );
        assert_rejected("auth_mode", &["", "Strict", "none", "STRICT"], "auth_mode");
        assert!(
            load_http(
                &[
                    "--auth-mode".into(),
                    "strict".into(),
                    "--identity-file".into(),
                    "".into()
                ],
                &BTreeMap::new()
            )
            .is_err()
        );
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn data_directory_precedence_and_validation() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"data_dir":"file-data"}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_DATA_DIR".into(), "env-data".into())]);
        assert_precedence(
            &args,
            &env,
            &["--data-dir", "cli-data"],
            [
                "data".into(),
                "file-data".into(),
                "env-data".into(),
                "cli-data".into(),
            ],
            load_http,
            |config| config.data_dir,
        );
        assert_rejected("data_dir", &[""], "must not be empty");
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn an_empty_environment_value_is_refused_and_the_refusal_names_how_to_leave_it_out() {
        for key in [
            "GCS_MIMIC_AUTH_MODE",
            "GCS_MIMIC_BUCKETS",
            "GCS_MIMIC_CONFIG",
            "GCS_MIMIC_CORS_FALLBACK",
            "GCS_MIMIC_LOG_LEVEL",
            "GCS_MIMIC_MAX_CONNECTIONS",
            "GCS_MIMIC_SIGNED_HOST_PORT",
            "GCS_MIMIC_SIGNED_URLS",
            "GCS_MIMIC_TEST_SIGNER",
            "GCS_MIMIC_TRUSTED_LISTEN",
        ] {
            let env = BTreeMap::from([(key.to_owned(), String::new())]);
            let errors = Config::load(&[], &env).unwrap_err();
            assert!(
                errors.errors.iter().any(|error| {
                    error.source == DiagnosticSource::Environment
                        && error.key == key
                        && error.fix.contains(&format!("`{key}:` with no value"))
                }),
                "{key}: {errors}"
            );
        }
    }

    #[test]
    fn only_an_empty_environment_value_gets_the_advice_about_leaving_it_out() {
        let advice = "does not mean unset";
        let invalid = BTreeMap::from([("GCS_MIMIC_SIGNED_URLS".to_owned(), "maybe".to_owned())]);
        let unknown = BTreeMap::from([("GCS_MIMIC_NO_SUCH_SETTING".to_owned(), String::new())]);
        let compose_only = BTreeMap::from([("GCS_MIMIC_PORT".to_owned(), String::new())]);
        let flag = ["--signed-urls", ""].map(str::to_owned);
        let none = BTreeMap::new();
        for (args, env) in [
            (&[][..], &invalid),
            (&[][..], &unknown),
            (&[][..], &compose_only),
            (&flag[..], &none),
        ] {
            let errors = Config::load(args, env).unwrap_err().to_string();
            assert!(!errors.contains(advice), "{args:?} {env:?}: {errors}");
        }
    }

    #[test]
    fn settings_that_define_an_empty_environment_value_start_with_it() {
        for key in [
            "GCS_MIMIC_ADMIN_ALLOWED_HOSTS",
            "GCS_MIMIC_DEFAULT_LOCATION",
            "GCS_MIMIC_EVENT_BUCKET",
            "GCS_MIMIC_EVENT_OBJECT_PREFIX",
            "GCS_MIMIC_EVENT_PUBSUB_PROJECT_ID",
            "GCS_MIMIC_EVENT_PUBSUB_TOPIC",
            "GCS_MIMIC_LOCAL_CREATOR_EMAIL",
            "GCS_MIMIC_PUBSUB_EMULATOR_HOST",
            "GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS",
        ] {
            let env = BTreeMap::from([(key.to_owned(), String::new())]);
            let report = Config::preflight(&[], &env);
            assert!(
                !report.diagnostics.is_err(),
                "{key}: {}",
                report.diagnostics
            );
        }
        let env = BTreeMap::from([("GCS_MIMIC_LOCAL_CREATOR_EMAIL".to_owned(), String::new())]);
        assert_eq!(Config::load(&[], &env).unwrap().local_creator_email, "");
    }
}
