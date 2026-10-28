#![deny(clippy::unwrap_used, clippy::expect_used)]

use crate::{
    auth::AuthMode,
    bucket::bucket_location,
    config_input::{Input, InputError, Inputs, NATIVE_KEYS},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics, LogLevel},
    url::PublicOrigin,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::{Path, PathBuf},
};

const CONFIG_VALUE_FIX: &str =
    "Expected a string or unsigned integer; use the documented flat JSON setting type.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompatProfile {
    #[default]
    Native,
    LegacyEmulator,
}

impl std::str::FromStr for CompatProfile {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "native" => Ok(Self::Native),
            "legacy-emulator" => Ok(Self::LegacyEmulator),
            _ => Err("compat_profile must be native or legacy-emulator".into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scheme {
    Http,
    #[default]
    Https,
    Both,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Both => "both",
        }
    }
}

impl std::str::FromStr for Scheme {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            "both" => Ok(Self::Both),
            _ => Err("scheme must be http, https or both".into()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub log_level: LogLevel,
    pub listen: SocketAddr,
    pub listen_http: Option<SocketAddr>,
    pub grpc_listen: Option<SocketAddr>,
    pub scheme: Scheme,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub tls_handshake_timeout_ms: u64,
    pub data_dir: PathBuf,
    pub seed_dir: Option<PathBuf>,
    pub sync_dir: Option<PathBuf>,
    pub sync_delete_guard: bool,
    pub sync_reinitialize: Option<crate::storage::sync_state::CatalogId>,
    pub seed_project: String,
    pub seed_content_type: crate::mime::SeedContentType,
    pub default_location: String,
    pub buckets_file: Option<PathBuf>,
    pub api_base_url: Option<String>,
    pub public_base_url: Option<PublicOrigin>,
    pub xml_virtual_host_domains: Vec<String>,
    pub max_connections: usize,
    pub max_object_bytes: u64,
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
    pub connection_timeout_ms: u64,
    pub keep_alive_timeout_ms: u64,
    pub object_request_timeout_ms: u64,
    pub catalog_wait_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
    pub auth_mode: AuthMode,
    pub compat_profile: CompatProfile,
    /// Strict-mode identity registry; loaded once at startup, never in permissive mode.
    pub identity_file: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Info,
            listen: SocketAddr::from(([127, 0, 0, 1], 4443)),
            listen_http: None,
            grpc_listen: None,
            scheme: Scheme::Https,
            tls_cert: None,
            tls_key: None,
            tls_handshake_timeout_ms: 5_000,
            data_dir: PathBuf::from("data"),
            seed_dir: None,
            sync_dir: None,
            sync_delete_guard: true,
            sync_reinitialize: None,
            seed_project: "local-project".into(),
            seed_content_type: crate::mime::SeedContentType::OctetStream,
            default_location: "US".into(),
            buckets_file: None,
            api_base_url: None,
            public_base_url: None,
            xml_virtual_host_domains: Vec::new(),
            max_connections: 32,
            max_object_bytes: 1024 * 1024 * 1024,
            worker_threads: 2,
            max_blocking_threads: 2,
            connection_timeout_ms: 5_000,
            keep_alive_timeout_ms: 120_000,
            object_request_timeout_ms: 900_000,
            catalog_wait_timeout_ms: 30_000,
            shutdown_timeout_ms: 5_000,
            auth_mode: AuthMode::Permissive,
            compat_profile: CompatProfile::Native,
            identity_file: None,
        }
    }
}

impl Config {
    pub fn healthcheck_url(&self) -> String {
        let mut listen = self.effective_listen_http().unwrap_or(self.listen);
        if listen.ip().is_unspecified() {
            listen.set_ip(if listen.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        let scheme = if self.scheme == Scheme::Https {
            "https"
        } else {
            "http"
        };
        format!("{scheme}://{listen}/_internal/ready")
    }

    pub fn effective_listen_http(&self) -> Option<SocketAddr> {
        (self.scheme == Scheme::Both).then(|| {
            self.listen_http
                .unwrap_or_else(|| SocketAddr::new(self.listen.ip(), 8000))
        })
    }

    pub fn effective_api_base_url(&self, bound: SocketAddr) -> String {
        self.api_base_url.clone().unwrap_or_else(|| {
            let ip = match bound.ip() {
                std::net::IpAddr::V4(ip) if ip.is_unspecified() => {
                    std::net::Ipv4Addr::LOCALHOST.into()
                }
                std::net::IpAddr::V6(ip) if ip.is_unspecified() => {
                    std::net::Ipv6Addr::LOCALHOST.into()
                }
                ip => ip,
            };
            format!(
                "{}://{}",
                if self.scheme == Scheme::Http {
                    "http"
                } else {
                    "https"
                },
                SocketAddr::new(ip, bound.port())
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

    pub fn preflight(args: &[String], env: &BTreeMap<String, String>) -> ConfigurationReport {
        let mut inputs = Inputs::default();
        inputs.cli(args, true);
        inputs.environment(env);
        Self::from_inputs(inputs)
    }

    pub fn preflight_os(
        args: &[String],
        env: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    ) -> ConfigurationReport {
        let mut inputs = Inputs::default();
        inputs.cli(args, true);
        inputs.environment_os(env);
        Self::from_inputs(inputs)
    }

    fn from_inputs(mut inputs: Inputs) -> ConfigurationReport {
        inputs.diagnose_legacy();
        let mut invalid_file_fields = BTreeSet::new();
        let selector = inputs
            .entries
            .iter()
            .filter(|i| {
                i.name == "config"
                    && !i.legacy
                    && i.source != DiagnosticSource::Config
                    && !i.key.starts_with("FAKE_GCS_")
            })
            .max_by_key(|i| {
                if i.source == DiagnosticSource::Cli {
                    1
                } else {
                    0
                }
            })
            .cloned();
        if let Some(selector) = selector.filter(|input| input.invalid.is_none()) {
            match read_config(&selector.value) {
                Ok(values) => {
                    for (key, value) in values {
                        let text = if key == "default_location" && value.is_null() {
                            Some(String::new())
                        } else if key == "sync_delete_guard" && value.is_boolean() {
                            Some(value.to_string())
                        } else if key == "buckets_file" {
                            value.as_str().map(str::to_owned)
                        } else {
                            setting_text(&value)
                        };
                        if !inputs.push(Input {
                            source: DiagnosticSource::Config,
                            key: key.clone(),
                            name: key.clone(),
                            value: text.clone().unwrap_or_default(),
                            legacy: false,
                            invalid: None,
                        }) {
                            break;
                        }
                        if text.is_none() {
                            invalid_file_fields.insert(key.clone());
                            inputs.diagnostics.errors.push(Diagnostic::new(
                                DiagnosticSource::Config,
                                key,
                                CONFIG_VALUE_FIX,
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
        }
        let mut settings = BTreeMap::new();
        let mut seen_cli = BTreeSet::new();
        for source in [
            DiagnosticSource::Config,
            DiagnosticSource::Environment,
            DiagnosticSource::Cli,
        ] {
            for input in inputs
                .entries
                .iter()
                .filter(|i| i.source == source && !i.legacy && !i.key.starts_with("FAKE_GCS_"))
            {
                if source == DiagnosticSource::Cli && !seen_cli.insert(&input.name) {
                    inputs.diagnostics.errors.push(Diagnostic::new(
                        source,
                        &input.key,
                        "Duplicate option; supply each --name VALUE option exactly once.",
                    ));
                }
                if !NATIVE_KEYS.contains(&input.name.as_str())
                    || (input.name == "config" && source == DiagnosticSource::Config)
                    || (input.name == "sync_reinitialize" && source != DiagnosticSource::Cli)
                {
                    let fix = match input.name.as_str() {
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
                    };
                    inputs
                        .diagnostics
                        .errors
                        .push(Diagnostic::new(source, &input.key, fix));
                    continue;
                }
                settings.insert(input.name.clone(), input.clone());
            }
        }
        let mut report = ConfigurationReport {
            config: Self::default(),
            diagnostics: inputs.diagnostics,
            invalid: invalid_file_fields
                .into_iter()
                .filter(|key| !settings.contains_key(key))
                .collect(),
        };
        let tls_options_supplied = settings.keys().any(|key| {
            matches!(
                key.as_str(),
                "tls_cert" | "tls_key" | "tls_handshake_timeout_ms"
            )
        });
        for (key, input) in &settings {
            if let Some(invalid) = input.invalid {
                report.invalid.insert(key.clone());
                if invalid == InputError::Encoding {
                    report.diagnostics.errors.push(Diagnostic::new(
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
            let mut candidate = report.config.clone();
            let config = &mut candidate;
            let value = input.value.clone();
            let result: Result<(), String> = (|| {
                match key.as_str() {
                    "log_level" => config.log_level = value.parse()?,
                    "scheme" => config.scheme = value.parse()?,
                    "listen_http" => {
                        config.listen_http = Some(value.parse().map_err(|_| {
                            "listen_http must be an IP socket address, for example 127.0.0.1:8000".to_owned()
                        })?);
                    }
                    "grpc_listen" => {
                        config.grpc_listen = Some(value.parse().map_err(|_| {
                            "grpc_listen must be an IP socket address; use 127.0.0.1:4444 for a separate plaintext gRPC port".to_owned()
                        })?);
                    }
                    "tls_cert" | "tls_key" => {
                        if value.is_empty() {
                            return Err(format!("{key} must not be empty"));
                        }
                        if key == "tls_cert" {
                            config.tls_cert = Some(PathBuf::from(value));
                        } else {
                            config.tls_key = Some(PathBuf::from(value));
                        }
                    }
                    "tls_handshake_timeout_ms" => {
                        config.tls_handshake_timeout_ms = bounded(key, &value, 1, 60_000)?
                    }
                    "api_base_url" => {
                        config.api_base_url = Some(
                            crate::url::normalize_base_url(&value)
                                .map_err(|error| format!("api_base_url: {error}"))?,
                        );
                    }
                    "public_base_url" => {
                        config.public_base_url = Some(
                            PublicOrigin::parse(&value)
                                .map_err(|error| format!("public_base_url: {error}"))?,
                        );
                    }
                    "xml_virtual_host_domains" => {
                        if !value.is_empty() {
                            for domain in value.split(',') {
                                let domain = domain.trim().to_ascii_lowercase();
                                validate_dns_name(&domain)?;
                                if config.xml_virtual_host_domains.contains(&domain) {
                                    return Err("duplicate xml_virtual_host_domains entry; remove repeated DNS names".into());
                                }
                                if config.xml_virtual_host_domains.len() == 32 {
                                    return Err(
                                    "xml_virtual_host_domains must contain at most 32 DNS names"
                                        .into(),
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
                    "data_dir" => {
                        if value.is_empty() {
                            return Err("data_dir must not be empty".into());
                        }
                        config.data_dir = PathBuf::from(value);
                    }
                    "seed_dir" => {
                        if value.is_empty() {
                            return Err("seed_dir must not be empty".into());
                        }
                        config.seed_dir = Some(PathBuf::from(value));
                    }
                    "sync_dir" => {
                        if value.is_empty() {
                            return Err(
                                "sync_dir must not be empty; supply an existing writable directory"
                                    .into(),
                            );
                        }
                        config.sync_dir = Some(PathBuf::from(value));
                    }
                    "sync_delete_guard" => {
                        config.sync_delete_guard = value
                            .parse::<bool>()
                            .map_err(|_| "sync_delete_guard must be true or false")?
                    }
                    "sync_reinitialize" => {
                        config.sync_reinitialize = Some(crate::sync::parse_catalog_id(&value)?)
                    }
                    "seed_project" => {
                        crate::json::validate_project(&value)
                            .map_err(|error| format!("seed_project: {}", error.message))?;
                        config.seed_project = value;
                    }
                    "seed_content_type" => config.seed_content_type = value.parse()?,
                    "default_location" => {
                        let value = if value.is_empty() { "US" } else { &value };
                        config.default_location = bucket_location(value)
                            .map_err(|_| "default_location must be a supported bucket location; use US or ASIA-NORTHEAST1")?
                            .0.to_owned();
                    }
                    "buckets_file" => {
                        if value.is_empty() {
                            return Err("buckets_file must not be empty; use buckets.json or omit the setting".into());
                        }
                        config.buckets_file = Some(PathBuf::from(value));
                    }
                    "listen" => {
                        config.listen = value
                            .parse()
                            .map_err(|_| "listen must be an IP:port socket address")?
                    }
                    "max_connections" => {
                        config.max_connections = bounded(key, &value, 1, 1024)? as usize
                    }
                    "max_object_bytes" => {
                        config.max_object_bytes = bounded(key, &value, 1, 5 * 1024_u64.pow(4))?
                    }
                    "worker_threads" => {
                        config.worker_threads = bounded(key, &value, 1, 32)? as usize
                    }
                    "max_blocking_threads" => {
                        config.max_blocking_threads = bounded(key, &value, 1, 32)? as usize
                    }
                    "connection_timeout_ms" => {
                        config.connection_timeout_ms = bounded(key, &value, 1, 60_000)?
                    }
                    "keep_alive_timeout_ms" => {
                        config.keep_alive_timeout_ms = bounded(key, &value, 1, 600_000)?
                    }
                    "object_request_timeout_ms" => {
                        config.object_request_timeout_ms = bounded(key, &value, 1, 86_400_000)?
                    }
                    "catalog_wait_timeout_ms" => {
                        config.catalog_wait_timeout_ms = bounded(key, &value, 1, 60_000)?
                    }
                    "shutdown_timeout_ms" => {
                        config.shutdown_timeout_ms = bounded(key, &value, 1, 60_000)?
                    }
                    "auth_mode" => config.auth_mode = value.parse()?,
                    "compat_profile" => config.compat_profile = value.parse()?,
                    "identity_file" => {
                        if value.is_empty() {
                            return Err("identity_file must not be empty".into());
                        }
                        config.identity_file = Some(PathBuf::from(value));
                    }
                    _ => return Err("Unknown setting; remove it and use --help.".into()),
                }
                Ok(())
            })();
            match result {
                Ok(()) => report.config = candidate,
                Err(fix) => {
                    report.invalid.insert(key.clone());
                    report
                        .diagnostics
                        .errors
                        .push(Diagnostic::new(input.source, &input.key, fix));
                }
            }
        }
        let config = &mut report.config;
        if config.sync_dir.is_none() && !report.invalid.contains("sync_dir") {
            for input in settings
                .values()
                .filter(|input| input.name.starts_with("sync_"))
            {
                if report.invalid.insert(input.name.clone()) {
                    report.diagnostics.errors.push(Diagnostic::new(input.source, &input.key,
                        "This setting requires sync_dir; supply --sync-dir DIRECTORY or remove the guard/reinitialization setting."));
                }
            }
        }
        if !["sync_dir", "data_dir", "seed_dir"]
            .iter()
            .any(|key| report.invalid.contains(*key))
            && let Some(root) = &config.sync_dir
            && let Err(error) =
                crate::sync::validate_directory(root, &config.data_dir, config.seed_dir.as_deref())
        {
            let input = &settings["sync_dir"];
            report.invalid.insert("sync_dir".into());
            report.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                error.to_string(),
            ));
        }
        if config.compat_profile == CompatProfile::LegacyEmulator
            && let Some(input) = settings.get("default_location")
            && !report.invalid.contains("default_location")
        {
            report.diagnostics.errors.push(Diagnostic::new(
                input.source,
                &input.key,
                "default_location requires compat_profile=native; use --compat-profile native --default-location EU or remove the default_location setting.",
            ));
            report.invalid.insert("default_location".into());
        }
        match config.scheme {
            Scheme::Http if tls_options_supplied && !report.invalid.contains("scheme") => {
                for name in ["tls_cert", "tls_key", "tls_handshake_timeout_ms"] {
                    if let Some(input) = settings.get(name)
                        && !report.invalid.contains(name)
                    {
                        report.diagnostics.errors.push(Diagnostic::new(
                            input.source,
                            &input.key,
                            "TLS settings require scheme=https or both; remove TLS settings for HTTP.",
                        ));
                        report.invalid.insert(name.into());
                    }
                }
            }
            Scheme::Https | Scheme::Both
                if config.tls_cert.is_some() != config.tls_key.is_some()
                    && !["scheme", "tls_cert", "tls_key"]
                        .iter()
                        .any(|k| report.invalid.contains(*k)) =>
            {
                let name = if config.tls_cert.is_some() {
                    "tls_cert"
                } else {
                    "tls_key"
                };
                if let Some(input) = settings.get(name) {
                    report.diagnostics.errors.push(Diagnostic::new(
                        input.source,
                        &input.key,
                        "tls_cert and tls_key must be supplied together; provide both PEM paths.",
                    ));
                }
                report.invalid.insert(name.into());
            }
            _ => {}
        }
        if !["scheme", "listen", "listen_http"]
            .iter()
            .any(|key| report.invalid.contains(*key))
        {
            let fix = if config.scheme != Scheme::Both && config.listen_http.is_some() {
                Some("listen_http requires scheme=both; remove listen_http or use --scheme both.")
            } else if config.listen.port() != 0
                && config.effective_listen_http() == Some(config.listen)
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
                    report
                        .diagnostics
                        .errors
                        .push(Diagnostic::new(input.source, &input.key, fix));
                }
                report.invalid.insert("listen_http".into());
            }
        }
        if !report.invalid.contains("public_base_url")
            && let Some(input) = settings.get("public_base_url")
            && let Some(origin) = config.public_base_url.as_ref()
        {
            if let Some(domain) = origin.dns_name() {
                let domain = domain.to_owned();
                if !config.xml_virtual_host_domains.contains(&domain) {
                    if config.xml_virtual_host_domains.len() == 32 {
                        report.diagnostics.errors.push(Diagnostic::new(input.source, &input.key,
                            "public_base_url and xml_virtual_host_domains must contain at most 32 distinct DNS names; remove a domain or reuse one already configured."));
                        report.invalid.insert("public_base_url".into());
                    } else {
                        config.xml_virtual_host_domains.push(domain);
                        config
                            .xml_virtual_host_domains
                            .sort_by_key(|d| std::cmp::Reverse(d.len()));
                    }
                }
            } else {
                report.diagnostics.warnings.push(Diagnostic::new(input.source, &input.key,
                    "public_base_url is an IP address: virtual-host routing is disabled; use path-style URLs or a DNS origin such as http://storage.example.invalid."));
            }
        }
        if !["api_base_url", "xml_virtual_host_domains", "listen"]
            .iter()
            .any(|k| report.invalid.contains(*k))
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
                report.diagnostics.errors.push(Diagnostic::new(input.source, &input.key, "api_base_url host selects an XML virtual bucket; use a host outside xml_virtual_host_domains or configure that host itself as a base domain."));
            }
        }
        // Reject mismatched profiles so a registry is never silently ignored or missing.
        if config.auth_mode == AuthMode::Strict
            && config.sync_dir.is_some()
            && let Some(input) = settings.get("sync_dir")
        {
            report.diagnostics.errors.push(Diagnostic::new(input.source, &input.key,
                "strict authentication for sync management is not supported yet; use --auth-mode permissive and remove identity_file (for example, auth_mode=permissive), or remove sync_dir/GCS_MIMIC_SYNC_DIR/--sync-dir."));
        }
        match (config.auth_mode, &config.identity_file) {
            (AuthMode::Strict, None) if !report.invalid.contains("identity_file") => {
                if let Some(input) = settings.get("auth_mode") {
                    report.diagnostics.errors.push(Diagnostic::new(input.source, &input.key, "auth_mode=strict requires identity_file; provide --identity-file identities.json."));
                }
                report.invalid.insert("identity_file".into());
            }
            (AuthMode::Permissive, Some(_)) if !report.invalid.contains("auth_mode") => {
                if let Some(input) = settings.get("identity_file") {
                    report.diagnostics.errors.push(Diagnostic::new(input.source, &input.key, "identity_file requires auth_mode=strict; select strict mode or remove the registry setting."));
                }
                report.invalid.insert("identity_file".into());
            }
            _ => {}
        }
        report
    }
}

pub struct ConfigurationReport {
    pub config: Config,
    pub diagnostics: Diagnostics,
    invalid: BTreeSet<String>,
}

impl ConfigurationReport {
    pub fn valid(&self, keys: &[&str]) -> bool {
        !keys.iter().any(|key| self.invalid.contains(*key))
    }
}

struct ConfigEntries(Vec<(String, serde_json::Value)>);

impl<'de> serde::Deserialize<'de> for ConfigEntries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> serde::de::Visitor<'de> for Entries {
            type Value = ConfigEntries;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a flat configuration object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if entries.len() > crate::config_input::MAX_SETTINGS
                        || key.len() > crate::config_input::MAX_KEY_BYTES
                    {
                        return Err(serde::de::Error::custom(
                            "configuration input limit exceeded",
                        ));
                    }
                    entries.push((key, map.next_value()?));
                }
                Ok(ConfigEntries(entries))
            }
        }
        deserializer.deserialize_map(Entries)
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
    for (key, value) in entries {
        if setting_text(&value).is_none() {
            return Err(Diagnostic::new(
                DiagnosticSource::Config,
                key,
                CONFIG_VALUE_FIX,
            ));
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

fn read_config(path: &str) -> Result<Vec<(String, serde_json::Value)>, ConfigReadError> {
    let bytes =
        crate::bounded_file::read(Path::new(path), 65_536).map_err(|error| match error {
            crate::bounded_file::ReadError::Limit => ConfigReadError::TooLarge,
            crate::bounded_file::ReadError::Io(_) => ConfigReadError::Unreadable,
        })?;
    serde_json::from_slice::<ConfigEntries>(&bytes)
        .map(|entries| entries.0)
        .map_err(|error| {
            if error
                .to_string()
                .starts_with("configuration input limit exceeded")
            {
                ConfigReadError::InputLimit
            } else {
                ConfigReadError::Malformed
            }
        })
}

pub(crate) fn validate_dns_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 253
        || value.parse::<std::net::IpAddr>().is_ok()
        || !value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err(
            "virtual-host domain must be a DNS name without scheme, port, wildcard or trailing dot"
                .into(),
        );
    }
    Ok(())
}

fn bounded(key: &str, value: &str, min: u64, max: u64) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|n| (min..=max).contains(n))
        .ok_or_else(|| format!("{key} must be an integer in {min}..={max}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn grpc_listener_configuration_precedence_and_diagnostics() {
        let empty = BTreeMap::new();
        assert!(Config::load(&[], &empty).unwrap().grpc_listen.is_none());
        let dir = PublicConfigDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"grpc_listen":"127.0.0.1:4444"}"#).unwrap();
        let mut args = vec!["--config".into(), file.to_str().unwrap().into()];
        assert_eq!(
            Config::load(&args, &empty)
                .unwrap()
                .grpc_listen
                .unwrap()
                .port(),
            4444
        );
        let env = BTreeMap::from([("GCS_MIMIC_GRPC_LISTEN".into(), "127.0.0.1:5555".into())]);
        assert_eq!(
            Config::load(&args, &env)
                .unwrap()
                .grpc_listen
                .unwrap()
                .port(),
            5555
        );
        args.extend(["--grpc-listen".into(), "127.0.0.1:0".into()]);
        assert_eq!(
            Config::load(&args, &env)
                .unwrap()
                .grpc_listen
                .unwrap()
                .port(),
            0
        );
        for value in [
            "",
            "localhost:4444",
            "http://127.0.0.1:4444",
            "127.0.0.1:65536",
        ] {
            let env = BTreeMap::from([("GCS_MIMIC_GRPC_LISTEN".into(), value.into())]);
            let error = Config::load(&[], &env).unwrap_err().to_string();
            assert!(
                error.contains("GCS_MIMIC_GRPC_LISTEN") && error.contains("127.0.0.1:4444"),
                "{error}"
            );
        }
    }

    #[test]
    fn default_location_validates_precedence_and_native_profile() {
        let empty = BTreeMap::new();
        assert_eq!(Config::load(&[], &empty).unwrap().default_location, "US");
        let dir = PublicConfigDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"default_location":"EU"}"#).unwrap();
        let mut args = vec!["--config".into(), file.to_str().unwrap().into()];
        assert_eq!(Config::load(&args, &empty).unwrap().default_location, "EU");
        std::fs::write(&file, r#"{"default_location":null}"#).unwrap();
        assert_eq!(Config::load(&args, &empty).unwrap().default_location, "US");
        std::fs::write(&file, r#"{"default_location":"EU"}"#).unwrap();
        let env = BTreeMap::from([("GCS_MIMIC_DEFAULT_LOCATION".into(), "ASIA".into())]);
        assert_eq!(Config::load(&args, &env).unwrap().default_location, "ASIA");
        args.extend(["--default-location".into(), "US".into()]);
        assert_eq!(Config::load(&args, &env).unwrap().default_location, "US");
        for value in [" EU", "EU ", "unknown", "MARS-CENTRAL1"] {
            let env = BTreeMap::from([("GCS_MIMIC_DEFAULT_LOCATION".into(), value.into())]);
            let errors = Config::load(&[], &env).unwrap_err();
            assert_eq!(errors.errors[0].key, "GCS_MIMIC_DEFAULT_LOCATION");
            assert!(errors.to_string().contains("US or ASIA-NORTHEAST1"));
        }
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
        let empty = BTreeMap::new();
        assert_eq!(Config::load(&[], &empty).unwrap().log_level, LogLevel::Info);
        let directory = PublicConfigDir::new();
        let file = directory.0.join("settings.json");
        std::fs::write(&file, r#"{"log_level":"debug"}"#).unwrap();
        let mut args = vec!["--config".into(), file.to_str().unwrap().into()];
        assert_eq!(
            Config::load(&args, &empty).unwrap().log_level,
            LogLevel::Debug
        );
        let env = BTreeMap::from([("GCS_MIMIC_LOG_LEVEL".into(), "error".into())]);
        assert_eq!(
            Config::load(&args, &env).unwrap().log_level,
            LogLevel::Error
        );
        args.extend(["--log-level".into(), "warning".into()]);
        assert_eq!(Config::load(&args, &env).unwrap().log_level, LogLevel::Warn);
        for invalid in ["", "trace", "off", "fatal", "INFO", "secret\n"] {
            std::fs::write(&file, serde_json::json!({"log_level":invalid}).to_string()).unwrap();
            let config_args = vec!["--config".into(), file.to_str().unwrap().into()];
            assert!(Config::load(&config_args, &empty).is_err());
            assert!(Config::load(&["--log-level".into(), invalid.into()], &empty).is_err());
            let env = BTreeMap::from([("GCS_MIMIC_LOG_LEVEL".into(), invalid.into())]);
            let report = Config::preflight(&[], &env);
            assert_eq!(report.diagnostics.errors[0].key, "GCS_MIMIC_LOG_LEVEL");
            assert!(!report.diagnostics.to_string().contains("secret"));
            assert_eq!(Config::load(&args, &env).unwrap().log_level, LogLevel::Warn);
        }
    }

    struct PublicConfigDir(PathBuf);

    impl PublicConfigDir {
        fn new() -> Self {
            let mut suffix = [0u8; 8];
            getrandom::fill(&mut suffix).unwrap();
            let path = std::env::temp_dir().join(format!(
                "gcs-mimic-public-config-{}-{:x}",
                std::process::id(),
                u64::from_ne_bytes(suffix)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for PublicConfigDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
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
            assert!(loaded.buckets_file.is_some());
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
        let mut args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_BUCKETS_FILE".into(), "env.json".into())]);
        assert_eq!(
            Config::load(&args, &env).unwrap().buckets_file,
            Some("env.json".into())
        );
        args.extend(["--buckets-file".into(), "cli.json".into()]);
        assert_eq!(
            Config::load(&args, &env).unwrap().buckets_file,
            Some("cli.json".into())
        );
        assert_eq!(Config::default().buckets_file, None);
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
            ("FAKE_GCS_PORT".into(), "8080".into()),
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
        assert_eq!(report.diagnostics.errors.len(), 5);
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
        let file = std::env::temp_dir().join(format!(
            "gcs-mimic-config-count-{}.json",
            std::process::id()
        ));
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
            vec!["--identity-file", "ids.json", "--auth-mode=strict"],
            vec!["--identity-file", "ids.json", "--auth-mode"],
            vec!["--tls-key", "key.pem", "--tls-cert=cert.pem"],
            vec!["--tls-key", "key.pem", "--tls-cert"],
            vec![
                "--tls-cert",
                "cert.pem",
                "--tls-key",
                "key.pem",
                "--scheme=http",
            ],
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
        let report = Config::preflight(&["--auth-mode=strict".into()], &env);
        assert_eq!(report.diagnostics.errors.len(), 1);
        assert!(!report.valid(&["auth_mode"]));
    }

    #[test]
    fn config_file_diagnostics_keep_the_selected_source_and_field() {
        let dir = PublicConfigDir::new();
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
        let dir = PublicConfigDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"public_base_url":"http://file.invalid"}"#).unwrap();
        let mut args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([(
            "GCS_MIMIC_PUBLIC_BASE_URL".into(),
            "https://Env.Invalid:443/".into(),
        )]);
        assert_eq!(
            Config::load(&args, &BTreeMap::new())
                .unwrap()
                .public_base_url
                .as_ref()
                .map(PublicOrigin::as_str),
            Some("http://file.invalid")
        );
        assert_eq!(
            Config::load(&args, &env)
                .unwrap()
                .public_base_url
                .as_ref()
                .map(PublicOrigin::as_str),
            Some("https://env.invalid")
        );
        args.extend([
            "--public-base-url".into(),
            "http://Storage.Invalid:80/".into(),
            "--xml-virtual-host-domains".into(),
            "invalid,storage.invalid".into(),
        ]);
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
        assert!(
            Config::load(&[], &BTreeMap::new())
                .unwrap()
                .xml_virtual_host_domains
                .is_empty()
        );
        let file =
            std::env::temp_dir().join(format!("gcs-mimic-host-config-{}.json", std::process::id()));
        std::fs::write(&file, r#"{"xml_virtual_host_domains":"file.invalid"}"#).unwrap();
        let mut args = vec!["--config".into(), file.to_str().unwrap().into()];
        assert_eq!(
            Config::load(&args, &BTreeMap::new())
                .unwrap()
                .xml_virtual_host_domains,
            ["file.invalid"]
        );
        let env = BTreeMap::from([(
            "GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS".into(),
            "env.invalid".into(),
        )]);
        assert_eq!(
            Config::load(&args, &env).unwrap().xml_virtual_host_domains,
            ["env.invalid"]
        );
        args.extend([
            "--xml-virtual-host-domains".into(),
            "EXAMPLE.invalid, storage.example.invalid".into(),
        ]);
        assert_eq!(
            Config::load(&args, &env).unwrap().xml_virtual_host_domains,
            ["storage.example.invalid", "example.invalid"]
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
        assert_eq!(defaults.tls_handshake_timeout_ms, 5_000);
        let file =
            std::env::temp_dir().join(format!("gcs-mimic-tls-config-{}.json", std::process::id()));
        std::fs::write(&file, r#"{"scheme":"https","tls_cert":"file-cert.pem","tls_key":"file-key.pem","tls_handshake_timeout_ms":100}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let loaded = Config::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(loaded.scheme, Scheme::Https);
        assert_eq!(loaded.tls_cert.as_deref(), Some(Path::new("file-cert.pem")));
        assert_eq!(loaded.tls_key.as_deref(), Some(Path::new("file-key.pem")));
        assert_eq!(loaded.tls_handshake_timeout_ms, 100);
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
        assert_eq!(loaded.tls_cert.as_deref(), Some(Path::new("env-cert.pem")));
        assert_eq!(loaded.tls_key.as_deref(), Some(Path::new("env-key.pem")));
        assert_eq!(loaded.tls_handshake_timeout_ms, 200);
        assert_eq!(
            loaded.api_base_url.as_deref(),
            Some("http://proxy.invalid/base")
        );
        let cli = [
            args,
            vec![
                "--tls-cert".into(),
                "cli-cert.pem".into(),
                "--tls-key".into(),
                "cli-key.pem".into(),
                "--tls-handshake-timeout-ms".into(),
                "300".into(),
            ],
        ]
        .concat();
        let loaded = Config::load(&cli, &env).unwrap();
        assert_eq!(loaded.tls_cert.as_deref(), Some(Path::new("cli-cert.pem")));
        assert_eq!(loaded.tls_key.as_deref(), Some(Path::new("cli-key.pem")));
        assert_eq!(loaded.tls_handshake_timeout_ms, 300);
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
        for invalid in ["0", "60001", "-1", "unlimited"] {
            let mut args = cli.clone();
            *args.last_mut().unwrap() = invalid.into();
            assert!(Config::load(&args, &env).is_err());
        }
        for bound in [1, 60_000] {
            let mut args = cli.clone();
            *args.last_mut().unwrap() = bound.to_string();
            assert_eq!(
                Config::load(&args, &env).unwrap().tls_handshake_timeout_ms,
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
        let path = std::env::temp_dir().join(format!(
            "gcs-mimic-compat-config-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, r#"{"compat_profile":"legacy-emulator"}"#).unwrap();
        let args = vec!["--config".into(), path.to_str().unwrap().into()];
        assert_eq!(
            load_http(&args, &BTreeMap::new()).unwrap().compat_profile,
            CompatProfile::LegacyEmulator
        );
        let env = BTreeMap::from([("GCS_MIMIC_COMPAT_PROFILE".into(), "native".into())]);
        assert_eq!(
            load_http(&args, &env).unwrap().compat_profile,
            CompatProfile::Native
        );
        let args = [
            args,
            vec!["--compat-profile".into(), "legacy-emulator".into()],
        ]
        .concat();
        assert_eq!(
            load_http(&args, &env).unwrap().compat_profile,
            CompatProfile::LegacyEmulator
        );
        for value in ["", "NATIVE", "legacy_emulator", "permissive", "unknown"] {
            assert!(
                load_http(&["--compat-profile".into(), value.into()], &BTreeMap::new())
                    .unwrap_err()
                    .to_string()
                    .contains("compat_profile")
            );
        }
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
    fn transfer_timeout_precedence_and_bounds() {
        for (setting, default, maximum) in [
            ("object_request_timeout_ms", 900_000, 86_400_000),
            ("catalog_wait_timeout_ms", 30_000, 60_000),
            ("keep_alive_timeout_ms", 120_000, 600_000),
        ] {
            let file = std::env::temp_dir()
                .join(format!("gcs-mimic-{setting}-{}.json", std::process::id()));
            let value = |config: Config| match setting {
                "object_request_timeout_ms" => config.object_request_timeout_ms,
                "catalog_wait_timeout_ms" => config.catalog_wait_timeout_ms,
                _ => config.keep_alive_timeout_ms,
            };
            let flag = format!("--{}", setting.replace('_', "-"));
            let key = format!("GCS_MIMIC_{}", setting.to_ascii_uppercase());
            let args = vec!["--config".into(), file.to_str().unwrap().into()];
            std::fs::write(&file, format!(r#"{{"{setting}":100}}"#)).unwrap();
            assert_eq!(value(load_http(&[], &BTreeMap::new()).unwrap()), default);
            assert_eq!(value(load_http(&args, &BTreeMap::new()).unwrap()), 100);
            let env = BTreeMap::from([(key.clone(), "200".into())]);
            assert_eq!(value(load_http(&args, &env).unwrap()), 200);
            let mut cli = args.clone();
            cli.extend([flag.clone(), "300".into()]);
            assert_eq!(value(load_http(&cli, &env).unwrap()), 300);
            for invalid in [
                "0".into(),
                "-1".into(),
                (maximum + 1).to_string(),
                "unlimited".into(),
            ] {
                assert!(load_http(&[flag.clone(), invalid.clone()], &BTreeMap::new()).is_err());
                assert!(load_http(&[], &BTreeMap::from([(key.clone(), invalid.clone())])).is_err());
                std::fs::write(&file, format!(r#"{{"{setting}":"{invalid}"}}"#)).unwrap();
                assert!(load_http(&args, &BTreeMap::new()).is_err());
            }
            for bound in [1, maximum] {
                assert_eq!(
                    value(load_http(&[flag.clone(), bound.to_string()], &BTreeMap::new()).unwrap()),
                    bound
                );
            }
            std::fs::remove_file(file).unwrap();
        }
    }

    #[test]
    fn object_limit_precedence_and_bounds() {
        let file = std::env::temp_dir().join(format!(
            "gcs-mimic-object-config-{}.json",
            std::process::id()
        ));
        std::fs::write(&file, r#"{"max_object_bytes":64}"#).unwrap();
        let mut args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_MAX_OBJECT_BYTES".into(), "128".into())]);
        assert_eq!(
            load_http(&[], &BTreeMap::new()).unwrap().max_object_bytes,
            1_073_741_824
        );
        assert_eq!(
            load_http(&args, &BTreeMap::new()).unwrap().max_object_bytes,
            64
        );
        assert_eq!(load_http(&args, &env).unwrap().max_object_bytes, 128);
        args.extend(["--max-object-bytes".into(), "256".into()]);
        assert_eq!(load_http(&args, &env).unwrap().max_object_bytes, 256);
        for value in ["0", "-1", "5497558138881", "unlimited"] {
            assert!(
                load_http(
                    &["--max-object-bytes".into(), value.into()],
                    &BTreeMap::new()
                )
                .is_err()
            );
        }
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
        let file =
            std::env::temp_dir().join(format!("gcs-mimic-auth-config-{}.json", std::process::id()));
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let defaults = load_http(&[], &BTreeMap::new()).unwrap();
        assert_eq!(defaults.auth_mode, AuthMode::Permissive);
        assert_eq!(defaults.identity_file, None);
        std::fs::write(
            &file,
            r#"{"auth_mode":"strict","identity_file":"file-ids.json"}"#,
        )
        .unwrap();
        let loaded = load_http(&args, &BTreeMap::new()).unwrap();
        assert_eq!(loaded.auth_mode, AuthMode::Strict);
        assert_eq!(
            loaded.identity_file.as_deref(),
            Some(Path::new("file-ids.json"))
        );
        let env = BTreeMap::from([("GCS_MIMIC_IDENTITY_FILE".into(), "env-ids.json".into())]);
        assert_eq!(
            load_http(&args, &env).unwrap().identity_file.as_deref(),
            Some(Path::new("env-ids.json"))
        );
        let mut cli = args.clone();
        cli.extend(["--identity-file".into(), "cli-ids.json".into()]);
        assert_eq!(
            load_http(&cli, &env).unwrap().identity_file.as_deref(),
            Some(Path::new("cli-ids.json"))
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
        for invalid in ["", "Strict", "none", "STRICT"] {
            assert!(
                load_http(&["--auth-mode".into(), invalid.into()], &BTreeMap::new()).is_err(),
                "{invalid}"
            );
        }
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
        let file =
            std::env::temp_dir().join(format!("gcs-mimic-data-config-{}.json", std::process::id()));
        std::fs::write(&file, r#"{"data_dir":"file-data"}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_DATA_DIR".into(), "env-data".into())]);
        assert_eq!(
            load_http(&[], &BTreeMap::new()).unwrap().data_dir,
            Path::new("data")
        );
        assert_eq!(
            load_http(&args, &BTreeMap::new()).unwrap().data_dir,
            Path::new("file-data")
        );
        assert_eq!(
            load_http(&args, &env).unwrap().data_dir,
            Path::new("env-data")
        );
        let mut cli_args = args.clone();
        cli_args.extend(["--data-dir".into(), "cli-data".into()]);
        assert_eq!(
            load_http(&cli_args, &env).unwrap().data_dir,
            Path::new("cli-data")
        );
        assert!(load_http(&["--data-dir".into(), "".into()], &BTreeMap::new()).is_err());
        assert!(
            load_http(
                &[],
                &BTreeMap::from([("GCS_MIMIC_DATA_DIR".into(), "".into())])
            )
            .is_err()
        );
        std::fs::write(&file, r#"{"data_dir":""}"#).unwrap();
        assert!(load_http(&args, &BTreeMap::new()).is_err());
        std::fs::remove_file(file).unwrap();
    }
}
