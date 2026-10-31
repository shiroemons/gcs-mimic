#![deny(clippy::unwrap_used, clippy::expect_used)]

use crate::{
    bucket::bucket_location,
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
};
use std::collections::BTreeMap;

pub(crate) const MAX_SETTINGS: usize = 1024;
pub(crate) const MAX_KEY_BYTES: usize = 256;

pub(crate) const NATIVE_KEYS: &[&str] = &[
    "config",
    "log_level",
    "listen",
    "listen_http",
    "grpc_listen",
    "unix_socket",
    "scheme",
    "tls_cert",
    "tls_key",
    "tls_handshake_timeout_ms",
    "data_dir",
    "backend",
    "memory_max_bytes",
    "memory_catalog_max_bytes",
    "seed_dir",
    "default_seed_dir",
    "seed_project",
    "seed_content_type",
    "sync_dir",
    "sync_delete_guard",
    "sync_reinitialize",
    "default_location",
    "buckets_file",
    "api_base_url",
    "public_base_url",
    "admin_enabled",
    "admin_principal",
    "xml_virtual_host_domains",
    "max_connections",
    "max_object_bytes",
    "worker_threads",
    "max_blocking_threads",
    "connection_timeout_ms",
    "keep_alive_timeout_ms",
    "object_request_timeout_ms",
    "catalog_wait_timeout_ms",
    "shutdown_timeout_ms",
    "auth_mode",
    "compat_profile",
    "cors_fallback",
    "identity_file",
    "pubsub_emulator_host",
    "event_pubsub_project_id",
    "event_pubsub_topic",
    "event_bucket",
    "event_object_prefix",
    "event_list",
];

pub(crate) const LEGACY: &[(&str, &str)] = &[
    ("PORT", "port"),
    ("PORT_HTTP", "port-http"),
    ("BACKEND", "backend"),
    ("FILESYSTEM_ROOT", "filesystem-root"),
    ("PUBLIC_HOST", "public-host"),
    ("EXTERNAL_URL", "external-url"),
    ("SCHEME", "scheme"),
    ("HOST", "host"),
    ("DATA", "data"),
    ("CORS_HEADERS", "cors-headers"),
    ("EVENT_PUBSUB_PROJECT_ID", "event.pubsub-project-id"),
    ("EVENT_PUBSUB_TOPIC", "event.pubsub-topic"),
    ("EVENT_BUCKET", "event.bucket"),
    ("EVENT_OBJECT_PREFIX", "event.object-prefix"),
    ("EVENT_LIST", "event.list"),
    ("LOCATION", "location"),
    ("CERT_LOCATION", "cert-location"),
    ("PRIVATE_KEY_LOCATION", "private-key-location"),
    ("LOG_LEVEL", "log-level"),
];

#[derive(Clone, Debug)]
pub(crate) struct Input {
    pub source: DiagnosticSource,
    pub key: String,
    pub name: String,
    pub value: String,
    pub kind: InputKind,
    pub invalid: Option<InputError>,
}

/// How a supplied setting relates to the native configuration names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputKind {
    Native,
    /// A known fake-gcs-server flag or `FAKE_GCS_*` variable, as supplied.
    Legacy,
    /// A native setting translated from `Legacy` inputs; its key names the legacy input.
    Alias,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputError {
    Syntax,
    Encoding,
}

/// Which command the CLI arguments belong to: only `serve` accepts native server settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CliCommand {
    Serve,
    Helper,
}

#[derive(Default)]
pub(crate) struct Inputs {
    pub entries: Vec<Input>,
    pub diagnostics: Diagnostics,
    count: usize,
    limited: bool,
}

impl Inputs {
    pub(crate) fn push(&mut self, input: Input) -> bool {
        if self.limited {
            return false;
        }
        if self.count == MAX_SETTINGS {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                "input_limit",
                "Configuration accepts at most 1024 supplied settings; remove excess entries.",
            ));
            self.limited = true;
            return false;
        }
        self.count += 1;
        if input.key.len() > MAX_KEY_BYTES {
            self.diagnostics.errors.push(Diagnostic::new(
                input.source,
                "input_limit",
                "Setting keys must contain at most 256 UTF-8 bytes; shorten the key.",
            ));
            self.limited = true;
            return false;
        }
        self.entries.push(input);
        true
    }

    pub(crate) fn cli(&mut self, args: &[String], command: CliCommand) {
        let mut index = 0;
        while let Some(token) = args.get(index) {
            let (key, inline) = token
                .split_once('=')
                .map_or((token.as_str(), None), |(k, v)| (k, Some(v)));
            let hyphens = key.bytes().take_while(|byte| *byte == b'-').count();
            let name = &key[hyphens..];
            let normalized = name.replace('-', "_");
            let native_name =
                command == CliCommand::Serve && NATIVE_KEYS.contains(&normalized.as_str());
            let native = hyphens == 2 && native_name;
            let legacy = !native
                && (1..=2).contains(&hyphens)
                && LEGACY.iter().any(|(_, flag)| *flag == name);
            let next = args.get(index + 1);
            let separate = inline.is_none()
                && next.is_some_and(|value| {
                    native
                        || !value.starts_with('-')
                        || value.parse::<i64>().is_ok()
                        || (hyphens == 2 && !legacy)
                });
            let value = inline.or_else(|| {
                if separate {
                    next.map(String::as_str)
                } else {
                    None
                }
            });
            let valid_syntax = hyphens == 2 && inline.is_none() && separate;
            // fake-gcs-server users expect a valueless switch here, so name the value they need.
            let example = if native_name && normalized == "cors_fallback" {
                "--cors-fallback allow-all"
            } else {
                "--scheme http"
            };
            let input = Input {
                source: DiagnosticSource::Cli,
                key: key.into(),
                name: if legacy {
                    normalized.replace('.', "_")
                } else {
                    normalized
                },
                value: value.unwrap_or_default().into(),
                kind: if legacy {
                    InputKind::Legacy
                } else {
                    InputKind::Native
                },
                invalid: if !legacy && !valid_syntax {
                    Some(InputError::Syntax)
                } else {
                    None
                },
            };
            let saved_key = if key.len() <= MAX_KEY_BYTES {
                Some(key.to_owned())
            } else {
                None
            };
            if !self.push(input) {
                break;
            }
            if !legacy
                && !valid_syntax
                && let Some(key) = saved_key
            {
                self.diagnostics.errors.push(Diagnostic::new(DiagnosticSource::Cli, key,
                    format!("Options require --name VALUE pairs; for example {example}. Use --help for supported options.")));
                if !native_name {
                    self.entries.pop();
                }
            }
            index += if separate { 2 } else { 1 };
        }
    }

    pub(crate) fn environment(&mut self, env: &BTreeMap<String, String>) {
        for (key, value) in env {
            if let Some(suffix) = key.strip_prefix("FAKE_GCS_") {
                let legacy = LEGACY.iter().any(|(name, _)| *name == suffix);
                if !self.push(Input {
                    source: DiagnosticSource::Environment,
                    key: key.clone(),
                    name: suffix.to_ascii_lowercase(),
                    value: value.clone(),
                    kind: if legacy {
                        InputKind::Legacy
                    } else {
                        InputKind::Native
                    },
                    invalid: None,
                }) {
                    break;
                }
            } else if let Some(suffix) = key
                .strip_prefix("GCS_MIMIC_")
                .or_else(|| (key == "PUBSUB_EMULATOR_HOST").then_some("PUBSUB_EMULATOR_HOST"))
                && !self.push(Input {
                    source: DiagnosticSource::Environment,
                    key: key.clone(),
                    name: suffix.to_ascii_lowercase(),
                    value: value.clone(),
                    kind: InputKind::Native,
                    invalid: None,
                })
            {
                break;
            }
        }
    }

    pub(crate) fn environment_os(
        &mut self,
        env: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    ) {
        for (key, value) in env
            .into_iter()
            .filter(|(key, _)| {
                key.as_encoded_bytes().starts_with(b"GCS_MIMIC_")
                    || key.as_encoded_bytes().starts_with(b"FAKE_GCS_")
                    || key == "PUBSUB_EMULATOR_HOST"
            })
            .take(MAX_SETTINGS + 1)
        {
            let Ok(key) = key.into_string() else {
                if !self.push(Input {
                    source: DiagnosticSource::Environment,
                    key: "input".into(),
                    name: "input".into(),
                    value: String::new(),
                    kind: InputKind::Native,
                    invalid: Some(InputError::Encoding),
                }) {
                    break;
                }
                self.entries.pop();
                self.diagnostics.errors.push(Diagnostic::new(
                    DiagnosticSource::Environment,
                    "input",
                    "Setting keys must be valid UTF-8; correct the environment key.",
                ));
                continue;
            };
            let suffix = key.strip_prefix("FAKE_GCS_");
            let legacy = suffix.is_some_and(|s| LEGACY.iter().any(|(known, _)| *known == s));
            let name = suffix
                .or_else(|| key.strip_prefix("GCS_MIMIC_"))
                .or_else(|| (key == "PUBSUB_EMULATOR_HOST").then_some("PUBSUB_EMULATOR_HOST"))
                .unwrap_or_default()
                .to_ascii_lowercase();
            let (value, invalid) = if suffix.is_some() && !legacy {
                (String::new(), None)
            } else {
                match value.into_string() {
                    Ok(value) => (value, None),
                    Err(_) => (String::new(), Some(InputError::Encoding)),
                }
            };
            if !self.push(Input {
                source: DiagnosticSource::Environment,
                key,
                name,
                value,
                kind: if legacy {
                    InputKind::Legacy
                } else {
                    InputKind::Native
                },
                invalid,
            }) {
                break;
            }
        }
    }

    /// The legacy inputs that fake-gcs-server would apply, by name: flags win over environment
    /// variables, and an empty environment value counts as unset.
    fn effective_legacy(&self) -> BTreeMap<String, &Input> {
        let mut effective = BTreeMap::new();
        for source in [DiagnosticSource::Environment, DiagnosticSource::Cli] {
            for input in self
                .entries
                .iter()
                .filter(|i| i.source == source && i.kind == InputKind::Legacy)
            {
                if !input.value.is_empty()
                    || input.invalid.is_some()
                    || source == DiagnosticSource::Cli
                {
                    effective.insert(input.name.clone(), input);
                }
            }
        }
        effective
    }

    /// Rejects legacy inputs given to helper commands, which have no server settings to map to.
    pub(crate) fn diagnose_legacy(&mut self) {
        let effective = self.effective_legacy();
        let values = advice_values(&effective);
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        for input in &self.entries {
            if input.kind == InputKind::Legacy {
                let mut fix = legacy_fix(&input.name, &values);
                if input.invalid == Some(InputError::Encoding) {
                    fix.push_str(" The supplied value is not UTF-8; correct or remove it.");
                }
                errors.push(Diagnostic::new(input.source, &input.key, fix));
            } else if input.key.starts_with("FAKE_GCS_") {
                warnings.push(unknown_legacy_warning(input));
            }
        }
        self.diagnostics.errors.extend(errors);
        self.diagnostics.warnings.extend(warnings);
    }

    /// Translates fake-gcs-server inputs into native settings so that an unchanged
    /// fake-gcs-server deployment starts; each one warns with its native replacement. Event
    /// publishing still fails: a test that waits for notifications would otherwise start and
    /// time out without saying why.
    pub(crate) fn translate_legacy(&mut self) {
        let effective = self.effective_legacy();
        let values = advice_values(&effective);
        let mut aliases: BTreeMap<&str, Input> = BTreeMap::new();
        for (name, input) in &effective {
            let Some((native, value)) = translation(name, &effective) else {
                continue;
            };
            let alias = aliases.entry(native).or_insert_with(|| Input {
                source: input.source,
                key: input.key.clone(),
                name: native.into(),
                value,
                kind: InputKind::Alias,
                invalid: None,
            });
            // host and port share one listener; a flag outranks an environment variable.
            if input.source == DiagnosticSource::Cli {
                alias.source = DiagnosticSource::Cli;
                alias.key.clone_from(&input.key);
            }
            alias.invalid = alias.invalid.or(input.invalid);
        }
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        for input in &self.entries {
            if input.kind == InputKind::Legacy {
                match legacy_outcome(&input.name, &values) {
                    Some(advice) => warnings.push(Diagnostic::new(
                        input.source,
                        &input.key,
                        format!("fake-gcs-server input is accepted; {advice}."),
                    )),
                    None => errors.push(Diagnostic::new(
                        input.source,
                        &input.key,
                        legacy_fix(&input.name, &values),
                    )),
                }
            } else if input.key.starts_with("FAKE_GCS_") {
                warnings.push(unknown_legacy_warning(input));
            }
        }
        for alias in aliases.values() {
            let overridden = self.entries.iter().any(|native| {
                native.kind == InputKind::Native
                    && native.source == alias.source
                    && native.name == alias.name
                    && native.value != alias.value
            });
            if overridden {
                warnings.push(Diagnostic::new(
                    alias.source,
                    &alias.key,
                    format!(
                        "This fake-gcs-server input overrides GCS_MIMIC_{} from the same source; remove one of them.",
                        alias.name.to_ascii_uppercase()
                    ),
                ));
            }
        }
        self.diagnostics.errors.extend(errors);
        self.diagnostics.warnings.extend(warnings);
        // Appended after native inputs, so an alias wins within its own source.
        self.entries.extend(aliases.into_values());
    }
}

fn unknown_legacy_warning(input: &Input) -> Diagnostic {
    Diagnostic::new(
        input.source,
        &input.key,
        "Unknown legacy environment variable is ignored; remove it from the emulator environment if unintended.",
    )
}

/// Values that advice may quote; non-UTF-8 values are replaced by a marker.
fn advice_values<'a>(effective: &'a BTreeMap<String, &'a Input>) -> BTreeMap<&'a str, &'a str> {
    effective
        .iter()
        .map(|(name, input)| {
            let value = if input.invalid == Some(InputError::Encoding) {
                "<invalid UTF-8>"
            } else {
                input.value.as_str()
            };
            (name.as_str(), value)
        })
        .collect()
}

/// The native setting and value that fake-gcs-server's `name` maps to, if any.
fn translation(name: &str, legacy: &BTreeMap<String, &Input>) -> Option<(&'static str, String)> {
    let value = |key: &str| legacy.get(key).map(|input| input.value.as_str());
    let set = |key: &str| value(key).filter(|v| !v.is_empty());
    let host = || match set("host").unwrap_or("0.0.0.0") {
        "localhost" => "127.0.0.1".to_owned(),
        host => match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V6(ip)) => format!("[{ip}]"),
            _ => host.to_owned(),
        },
    };
    let text = value(name)?.to_owned();
    Some(match name {
        "scheme" => ("scheme", text),
        "host" | "port" => (
            "listen",
            format!("{}:{}", host(), set("port").unwrap_or("4443")),
        ),
        "port_http" => ("listen_http", format!("{}:{text}", host())),
        "external_url" => ("api_base_url", text),
        "public_host" => {
            let scheme = if value("scheme") == Some("http") {
                "http"
            } else {
                "https"
            };
            ("public_base_url", format!("{scheme}://{text}"))
        }
        "data" => ("seed_dir", text),
        "backend" => (
            "backend",
            if text == "filesystem" || text.is_empty() {
                "disk".into()
            } else {
                text
            },
        ),
        // filesystem_root is not translated: a reused fake-gcs-server volume is owned by root,
        // so the unprivileged image could not write there and would fail to start.
        "cert_location" => ("tls_cert", text),
        "private_key_location" => ("tls_key", text),
        "cors_headers" => ("cors_fallback", "allow-all".into()),
        "location" => ("default_location", text),
        "event_pubsub_project_id" => ("event_pubsub_project_id", text),
        "event_pubsub_topic" => ("event_pubsub_topic", text),
        "event_bucket" => ("event_bucket", text),
        "event_object_prefix" => ("event_object_prefix", text),
        "event_list" => ("event_list", text),
        // fake-gcs-server uses logrus level names.
        "log_level" => (
            "log_level",
            match text.as_str() {
                "warning" => "warn".into(),
                "trace" => "debug".into(),
                "fatal" | "panic" => "error".into(),
                _ => text,
            },
        ),
        _ => return None,
    })
}

/// Advice for an accepted legacy input, or `None` when it must still be rejected.
fn legacy_outcome(name: &str, values: &BTreeMap<&str, &str>) -> Option<String> {
    let value = values.get(name).copied().unwrap_or_default();
    Some(match name {
        "backend" => match value {
            "memory" => "use --backend memory; the process-local RAM backend has bounded blob/catalog budgets, loses all data at process exit and never opens data_dir".into(),
            "filesystem" | "" => "use --backend disk for the native durable catalog".into(),
            _ => return None,
        },
        "filesystem_root" => "it is ignored; data is kept in GCS_MIMIC_DATA_DIR (/var/lib/gcs-mimic in the image), so mount the data volume there; existing fake-gcs-server objects are not loaded, so convert a stopped snapshot with gcs-mimic import fake-gcs-server; remove this setting".into(),
        "public_host" => {
            let scheme = if values.get("scheme") == Some(&"http") { "http" } else { "https" };
            let example = crate::url::normalize_public_base_url(&format!("{scheme}://{value}"))
                .unwrap_or_else(|_| "https://storage.example.invalid:4443".into());
            format!("use GCS_MIMIC_PUBLIC_BASE_URL={example}; path-style URLs work with any Host")
        }
        "cors_headers" => "browsers may read public URLs from any origin, as in fake-gcs-server; JSON requests already allow every valid header; use GCS_MIMIC_CORS_FALLBACK=allow-all".into(),
        "event_pubsub_project_id" | "event_pubsub_topic" | "event_bucket" | "event_object_prefix" | "event_list" => format!("use GCS_MIMIC_{} and an explicit PUBSUB_EMULATOR_HOST; only local emulator delivery is enabled", name.to_ascii_uppercase()),
        "scheme" | "host" | "port" | "port_http" | "external_url" | "data" | "cert_location"
        | "private_key_location" | "location" | "log_level" => legacy_advice(name, values),
        _ => return None,
    })
}

fn component<'a>(values: &'a BTreeMap<&str, &str>, key: &str, default: &'a str) -> &'a str {
    values
        .get(key)
        .copied()
        .filter(|v| !v.is_empty())
        .unwrap_or(default)
}

fn safe_url(value: &str) -> bool {
    value.parse::<hyper::Uri>().is_ok_and(|uri| {
        matches!(uri.scheme_str(), Some("http" | "https"))
            && uri.authority().is_some_and(|a| {
                !a.as_str().contains('@')
                    && (a.port().is_none() || a.port_u16().is_some())
                    && (a
                        .host()
                        .trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok()
                        || crate::config::validate_dns_name(a.host()).is_ok())
            })
            && uri.query().is_none()
            && !value.contains('#')
            && !value.chars().any(char::is_control)
    })
}

fn legacy_fix(name: &str, values: &BTreeMap<&str, &str>) -> String {
    format!(
        "Legacy input is unsupported; {}.",
        legacy_advice(name, values)
    )
}

fn legacy_advice(name: &str, values: &BTreeMap<&str, &str>) -> String {
    let host = component(values, "host", "0.0.0.0");
    let host = match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(ip)) => format!("[{ip}]"),
        Ok(ip) => ip.to_string(),
        Err(_) => "<IP_ADDRESS>".into(),
    };
    let port = component(values, "port", "4443")
        .parse::<u16>()
        .map_or_else(|_| "<PORT_0_TO_65535>".into(), |p| p.to_string());
    let scheme = component(values, "scheme", "https");
    let value = values.get(name).copied().unwrap_or_default();
    match name {
        "host" | "port" => format!("combine HOST and PORT as GCS_MIMIC_LISTEN={host}:{port}; use a valid IP address and port 0..65535"),
        "scheme" if matches!(scheme, "http" | "https" | "both") => format!("use GCS_MIMIC_SCHEME={scheme} or --scheme {scheme}"),
        "scheme" => "use GCS_MIMIC_SCHEME=http, https or both; both serves HTTPS on listen and HTTP on listen_http".into(),
        "port_http" => {
            let port = component(values, "port_http", "8000").parse::<u16>()
                .map_or_else(|_| "<PORT_0_TO_65535>".into(), |p| p.to_string());
            format!("use GCS_MIMIC_SCHEME=both with GCS_MIMIC_LISTEN_HTTP={host}:{port}; the primary GCS_MIMIC_LISTEN serves HTTPS")
        }
        "external_url" if safe_url(value) => format!("use GCS_MIMIC_API_BASE_URL={value}; preserve the client-reachable proxy prefix"),
        "external_url" => "use GCS_MIMIC_API_BASE_URL=http://gcs:4443/proxy without credentials, query or fragment; unsafe URL values are redacted".into(),
        "public_host" => {
            let scheme = if matches!(scheme, "https" | "both") { "https" } else if scheme == "http" { "http" } else { "<SCHEME>" };
            let url = format!("{scheme}://{value}");
            let example = crate::url::normalize_public_base_url(&url)
                .unwrap_or_else(|_| "https://storage.example.invalid:4443".into());
            format!("Use GCS_MIMIC_PUBLIC_BASE_URL={example} including the scheme; legacy Host matching also requires GCS_MIMIC_COMPAT_PROFILE=legacy-emulator. Native path-style accepts any Host. For SDK XML reads in legacy mode the public authority must match the API endpoint authority; unequal authorities fall back to a hostname bucket")
        }
        "data" => "mount fixtures at /data in the image or use GCS_MIMIC_SEED_DIR=fixtures; seed replays on every startup, overwriting changed objects at fixture names".into(),
        "filesystem_root" => "use GCS_MIMIC_DATA_DIR=data only for native storage; the source format differs. Retain the old root for the separately delivered Import conversion; never mount it as native data".into(),
        "backend" => "the backend selector and memory mode are unsupported; remove this setting and use the default disk-backed native catalog".into(),
        "cert_location" | "private_key_location" => "supply both GCS_MIMIC_TLS_CERT=certs/server.pem and GCS_MIMIC_TLS_KEY=certs/server-key.pem; one-sided overrides fail".into(),
        // Removal comes first: JSON-only callers need nothing else, and allow-all departs from Cloud Storage.
        "cors_headers" => "remove this setting; JSON preflight already allows every valid requested header. Only if browsers read XML/public URLs of buckets without stored CORS rules, also set GCS_MIMIC_CORS_FALLBACK=allow-all or --cors-fallback allow-all".into(),
        "location" => match bucket_location(value) {
            Ok((location, _)) => format!("use GCS_MIMIC_COMPAT_PROFILE=native with GCS_MIMIC_DEFAULT_LOCATION={location} or --default-location {location}; applies only to new buckets without a nonempty location; legacy-emulator global location defaults remain unsupported"),
            Err(_) => "remove this setting; native defaults use GCS_MIMIC_COMPAT_PROFILE=native with a supported GCS_MIMIC_DEFAULT_LOCATION, for example US or ASIA-NORTHEAST1; legacy-emulator global location defaults remain unsupported".into(),
        },
        "log_level" if matches!(value, "debug" | "info" | "warn" | "warning" | "error") => format!("use GCS_MIMIC_LOG_LEVEL={} or --log-level {}; this controls runtime stderr events, not validation diagnostics or stdout lifecycle events", if value == "warning" { "warn" } else { value }, if value == "warning" { "warn" } else { value }),
        "log_level" => "remove this legacy setting; use GCS_MIMIC_LOG_LEVEL=info or --log-level info; supported levels are debug, info, warn or error".into(),
        _ => "event publishing and filters are unsupported; remove this setting; no native publisher alias is available".into(),
    }
}

/// Whether `args` begin with a fake-gcs-server flag, as when a Compose `command` written for
/// fake-gcs-server replaces the image's `serve` command.
pub fn starts_with_legacy_flag(args: &[String]) -> bool {
    args.first().is_some_and(|token| {
        let key = token.split_once('=').map_or(token.as_str(), |(key, _)| key);
        let hyphens = key.bytes().take_while(|byte| *byte == b'-').count();
        let name = &key[hyphens..];
        let native = hyphens == 2 && NATIVE_KEYS.contains(&name.replace('-', "_").as_str());
        !native && (1..=2).contains(&hyphens) && LEGACY.iter().any(|(_, flag)| *flag == name)
    })
}

/// Inspect option-name positions without reading server configuration or environment.
pub fn cli_diagnostics(args: &[String]) -> Diagnostics {
    if matches!(args, [flag] if matches!(flag.as_str(), "--help" | "--version"))
        || matches!(args, [command, flag] if command == "serve" && flag == "--help")
        || matches!(args, [command, kind, flag] if command == "url" && matches!(kind.as_str(), "public" | "sign") && flag == "--help")
    {
        return Diagnostics::default();
    }
    let (offset, command) = match args.first().map(String::as_str) {
        Some("serve") => (1, CliCommand::Serve),
        Some("url") => (2.min(args.len()), CliCommand::Helper),
        Some("healthcheck") => (1, CliCommand::Helper),
        Some("tls") => (2.min(args.len()), CliCommand::Helper),
        _ => (0, CliCommand::Helper),
    };
    let mut inputs = Inputs::default();
    inputs.cli(&args[offset..], command);
    inputs.diagnose_legacy();
    if args.first().is_some_and(|v| v.starts_with('-')) && inputs.diagnostics.is_err() {
        inputs.diagnostics.errors.push(Diagnostic::new(DiagnosticSource::Cli, "command",
            "Compose command replaces the image CMD; use gcs-mimic serve --listen 0.0.0.0:4443 --data-dir /var/lib/gcs-mimic with native --name VALUE pairs, adjusting bind and volume choices as needed."));
    }
    inputs.diagnostics
}

/// Adopts `default_seed_dir` as the seed root when no explicit seed is configured. The image
/// sets it to `/data`, so a fake-gcs-server style mount loads without another variable, while a
/// `/data` used as the data or sync directory keeps its existing role.
pub fn resolve_default_seed(config: &mut crate::config::Config) -> Vec<Diagnostic> {
    let Some(candidate) = config.default_seed_dir.clone() else {
        return Vec::new();
    };
    if config.seed_dir.is_some() {
        return Vec::new();
    }
    let inspect = || -> std::io::Result<bool> {
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        }
        let seed = migration_path(&candidate)?;
        for other in std::iter::once(&config.data_dir)
            .filter(|_| config.backend == crate::config::Backend::Disk)
            .chain(config.sync_dir.as_ref())
        {
            let other = migration_path(other)?;
            if seed.starts_with(&other) || other.starts_with(&seed) {
                return Ok(false);
            }
        }
        std::fs::read_dir(&candidate).map(|_| true)
    };
    match inspect() {
        Ok(true) => {
            config.seed_dir = Some(candidate);
            Vec::new()
        }
        Ok(false) => Vec::new(),
        Err(_) => vec![Diagnostic::new(
            DiagnosticSource::Filesystem,
            "default_seed_dir",
            "Cannot inspect default_seed_dir or establish that it is disjoint from data_dir and sync_dir; fix its permissions or set seed_dir explicitly. No implicit import occurs.",
        )],
    }
}

fn migration_path(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::{
        io,
        path::{Component, PathBuf},
    };
    let mut ancestor = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut missing = Vec::new();
    let resolved = loop {
        match ancestor.canonicalize() {
            Ok(resolved) => break resolved,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(&ancestor).is_ok() {
                    return Err(error);
                }
                let Some(component) = ancestor.components().next_back() else {
                    return Err(error);
                };
                missing.push(component.as_os_str().to_owned());
                if !ancestor.pop() {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    };
    let mut result = resolved;
    for component in missing.into_iter().rev() {
        result.push(component);
    }
    let mut normalized = PathBuf::new();
    for component in result.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use proptest::prelude::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }

    /// Every error and warning as the emitted JSON lines.
    fn emitted(diagnostics: &Diagnostics) -> String {
        let mut bytes = Vec::new();
        diagnostics.write(&mut bytes).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    fn unsupported(diagnostics: &Diagnostics, key: &str) -> bool {
        diagnostics
            .errors
            .iter()
            .any(|d| d.key == key && d.fix.starts_with("Legacy input is unsupported"))
    }

    #[test]
    fn legacy_inventory_and_all_flag_forms_are_diagnosed_even_when_empty() {
        assert_eq!(LEGACY.len(), 19);
        for (suffix, flag) in LEGACY {
            for value in ["", "secret-do-not-print\n"] {
                let key = format!("FAKE_GCS_{suffix}");
                let env = BTreeMap::from([(key.clone(), value.into())]);
                let report = crate::config::Config::preflight(&[], &env);
                let text = emitted(&report.diagnostics);
                assert!(text.contains(&key), "{text}");
                assert!(!text.contains("secret-do-not-print"), "{text}");
                // Event settings now have their own validation; only unknown backend values lack a mapping.
                let rejected = *suffix == "BACKEND" && !value.is_empty();
                assert_eq!(unsupported(&report.diagnostics, &key), rejected, "{text}");
                for hyphens in ["-", "--"] {
                    for equals in [false, true] {
                        let key = format!("{hyphens}{flag}");
                        let args = if equals {
                            vec![format!("{key}={value}")]
                        } else {
                            vec![key.clone(), value.into()]
                        };
                        let report = crate::config::Config::preflight(&args, &BTreeMap::new());
                        let text = emitted(&report.diagnostics);
                        assert!(!text.contains("secret-do-not-print"), "{text}");
                        if key == "--scheme" || key == "--log-level" || key == "--backend" {
                            assert_eq!(report.diagnostics.errors[0].key, key);
                            assert!(report.diagnostics.errors[0].fix.contains(if equals {
                                "--name VALUE pairs"
                            } else if key == "--scheme" {
                                "scheme must"
                            } else if key == "--backend" {
                                "backend must"
                            } else {
                                "log_level must"
                            }));
                        } else {
                            assert!(text.contains(&key), "{text}");
                            let rejected = *suffix == "BACKEND" && !value.is_empty();
                            assert_eq!(unsupported(&report.diagnostics, &key), rejected, "{text}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fake_gcs_server_compose_command_starts_with_translated_settings() {
        let image = BTreeMap::from([
            ("GCS_MIMIC_LISTEN".into(), "0.0.0.0:4443".into()),
            ("GCS_MIMIC_DATA_DIR".into(), "/var/lib/gcs-mimic".into()),
        ]);
        let args = strings(&[
            "-scheme",
            "http",
            "-port",
            "4443",
            "-external-url",
            "http://gcs:4443",
            "-public-host",
            "gcs:4443",
            "-backend",
            "filesystem",
            "-filesystem-root",
            "/storage",
            "-data",
            "/data",
            "-log-level",
            "trace",
            "-cors-headers",
            "X-Custom",
            "-location",
            "asia-northeast1",
        ]);
        assert!(starts_with_legacy_flag(&args));
        let report = crate::config::Config::preflight(&args, &image);
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        let config = &report.config;
        assert!(matches!(config.scheme, crate::config::Scheme::Http));
        assert_eq!(config.listen.to_string(), "0.0.0.0:4443");
        assert_eq!(config.api_base_url.as_deref(), Some("http://gcs:4443"));
        assert!(config.public_base_url.is_some());
        assert_eq!(config.data_dir, std::path::Path::new("/var/lib/gcs-mimic"));
        assert_eq!(
            config.seed_dir.as_deref(),
            Some(std::path::Path::new("/data"))
        );
        assert!(matches!(
            config.log_level,
            crate::diagnostics::LogLevel::Debug
        ));
        assert!(matches!(
            config.cors_fallback,
            crate::config::CorsFallback::AllowAll
        ));
        assert_eq!(config.default_location, "ASIA-NORTHEAST1");
        let warned: Vec<_> = report
            .diagnostics
            .warnings
            .iter()
            .map(|d| d.key.as_str())
            .collect();
        for key in args.iter().filter(|arg| arg.starts_with('-')) {
            assert!(warned.contains(&key.as_str()), "{key}: {warned:?}");
        }
        let text = emitted(&report.diagnostics);
        assert!(text.contains("gcs-mimic import fake-gcs-server"), "{text}");
    }

    #[test]
    fn legacy_aliases_follow_source_precedence_and_keep_rejecting_events() {
        let image = BTreeMap::from([
            ("GCS_MIMIC_LISTEN".into(), "0.0.0.0:4443".into()),
            ("FAKE_GCS_PORT".into(), "8080".into()),
        ]);
        let report = crate::config::Config::preflight(&[], &image);
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        assert_eq!(report.config.listen.to_string(), "0.0.0.0:8080");
        assert!(emitted(&report.diagnostics).contains("overrides GCS_MIMIC_LISTEN"));
        // A flag outranks the environment, and a native flag outranks a legacy variable.
        let report = crate::config::Config::preflight(&strings(&["-host", "localhost"]), &image);
        assert_eq!(report.config.listen.to_string(), "127.0.0.1:8080");
        let report =
            crate::config::Config::preflight(&strings(&["--listen", "127.0.0.3:9000"]), &image);
        assert_eq!(report.config.listen.to_string(), "127.0.0.3:9000");
        let report = crate::config::Config::preflight(
            &strings(&["-backend", "memory", "-filesystem-root", "/storage"]),
            &BTreeMap::new(),
        );
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        assert_eq!(
            report.config.data_dir,
            crate::config::Config::default().data_dir
        );
        assert_eq!(report.config.backend, crate::config::Backend::Memory);
        assert!(emitted(&report.diagnostics).contains("process-local RAM backend"));
        let report = crate::config::Config::preflight(
            &strings(&["-scheme", "http", "-event.pubsub-topic", "uploads"]),
            &BTreeMap::new(),
        );
        assert!(report.diagnostics.is_err());
        assert_eq!(report.diagnostics.errors[0].key, "-event.pubsub-topic");
        assert!(emitted(&report.diagnostics).contains("Event publishing requires"));
        assert_eq!(report.diagnostics.errors.len(), 1, "{}", report.diagnostics);
        for args in [
            &["--scheme", "http"][..],
            &["serve", "-scheme", "http"],
            &["-x", "http"],
            &[],
        ] {
            assert!(!starts_with_legacy_flag(&strings(args)), "{args:?}");
        }
        assert!(starts_with_legacy_flag(&strings(&["--port=8080"])));
    }

    #[test]
    fn legacy_advice_uses_values_precedence_defaults_and_redacts_unsafe_urls() {
        let env = BTreeMap::from([
            ("FAKE_GCS_HOST".into(), "127.0.0.2".into()),
            ("FAKE_GCS_PORT".into(), "8080".into()),
            ("FAKE_GCS_SCHEME".into(), "http".into()),
        ]);
        let args = strings(&[
            "-port=9000",
            "-host",
            "::1",
            "-port",
            "9001",
            "-external-url=http://gcs:8080/proxy",
            "-public-host",
            "storage.invalid:9000",
            "-log-level=warning",
            "-location=EU",
        ]);
        let report = crate::config::Config::preflight(&args, &env);
        let fixes = emitted(&report.diagnostics);
        for expected in [
            "GCS_MIMIC_LISTEN=[::1]:9001",
            "GCS_MIMIC_API_BASE_URL=http://gcs:8080/proxy",
            "GCS_MIMIC_PUBLIC_BASE_URL=http://storage.invalid:9000",
            "GCS_MIMIC_LOG_LEVEL=warn",
            "GCS_MIMIC_DEFAULT_LOCATION=EU",
        ] {
            assert!(fixes.contains(expected), "{expected}: {fixes}");
        }
        let report = crate::config::Config::preflight(
            &[],
            &BTreeMap::from([("FAKE_GCS_PORT".into(), "8080".into())]),
        );
        assert!(emitted(&report.diagnostics).contains("0.0.0.0:8080"));
        let report = crate::config::Config::preflight(
            &strings(&["-scheme", "http", "-public-host", "Storage.Invalid:80"]),
            &BTreeMap::new(),
        );
        let advice = emitted(&report.diagnostics);
        assert!(advice.contains("GCS_MIMIC_PUBLIC_BASE_URL=http://storage.invalid;"));
        assert!(!advice.contains("Storage.Invalid:80"));
        // Helper commands have no server settings, so legacy input still fails there.
        let helper = cli_diagnostics(&strings(&["healthcheck", "-public-host", "x"]));
        assert!(
            helper
                .to_string()
                .contains("GCS_MIMIC_COMPAT_PROFILE=legacy-emulator")
        );
        for url in [
            "http://user:secret@host/path",
            "http://host/?X-Goog-Signature=secret",
            "http://host/#secret",
            "http://host/\nsecret",
        ] {
            let args = vec![
                "-external-url".into(),
                url.into(),
                "-public-host".into(),
                url.into(),
            ];
            let text =
                emitted(&crate::config::Config::preflight(&args, &BTreeMap::new()).diagnostics);
            assert!(!text.contains("secret"), "{text}");
        }
        let report = crate::config::Config::preflight(
            &strings(&[
                "-host",
                "secret",
                "-port",
                "no",
                "-location",
                "US-CENTRAL1",
                "-cors-headers",
                "secret",
            ]),
            &BTreeMap::new(),
        );
        let text = emitted(&report.diagnostics);
        for expected in [
            "<IP_ADDRESS>:<PORT_0_TO_65535>",
            "GCS_MIMIC_DEFAULT_LOCATION=US-CENTRAL1",
            "GCS_MIMIC_CORS_FALLBACK=allow-all",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("secret"));
        // The translated listener is invalid, so startup reports it without echoing values.
        assert!(
            report
                .diagnostics
                .errors
                .iter()
                .any(|d| d.fix.contains("listen must"))
        );
        let cors = &report
            .diagnostics
            .warnings
            .iter()
            .find(|d| d.key == "-cors-headers")
            .unwrap()
            .fix;
        assert!(cors.starts_with("fake-gcs-server input is accepted;"));
    }

    #[test]
    fn option_positions_preserve_native_values_and_equals_do_not_shift_tokens() {
        for kind in ["public", "sign"] {
            for object in ["-data", "-port", "-x"] {
                assert!(
                    !cli_diagnostics(&strings(&[
                        "url", kind, "--object", object, "--bucket", "bucket"
                    ]))
                    .is_err()
                );
            }
        }
        let report = cli_diagnostics(&strings(&[
            "-port=8080",
            "-host",
            "127.0.0.2",
            "--data=fixtures",
            "-x",
            "x",
        ]));
        for key in ["-port", "-host", "--data", "-x", "command"] {
            assert!(report.errors.iter().any(|d| d.key == key));
        }
        let command = report.errors.iter().find(|d| d.key == "command").unwrap();
        assert!(
            command
                .fix
                .contains("serve --listen 0.0.0.0:4443 --data-dir /var/lib/gcs-mimic")
        );
        let report =
            crate::config::Config::preflight(&strings(&["--scheme", "http"]), &BTreeMap::new());
        assert!(!report.diagnostics.is_err());
        let report = crate::config::Config::preflight(
            &strings(&["--scheme=http", "-port", "9000", "-host=::1"]),
            &BTreeMap::new(),
        );
        assert_eq!(report.diagnostics.errors.len(), 1, "{}", report.diagnostics);
        assert_eq!(report.config.listen.to_string(), "[::1]:9000");
        let report = crate::config::Config::preflight(
            &[],
            &BTreeMap::from([("FAKE_GCS_CONFIG".into(), "missing-secret.json".into())]),
        );
        assert!(!report.diagnostics.is_err());
        assert_eq!(report.diagnostics.warnings.len(), 1);
    }

    #[test]
    fn dual_listener_legacy_advice_names_effective_native_settings() {
        let report = crate::config::Config::preflight(
            &[],
            &BTreeMap::from([
                ("FAKE_GCS_SCHEME".into(), "both".into()),
                ("FAKE_GCS_HOST".into(), "::1".into()),
                ("FAKE_GCS_PORT_HTTP".into(), "9000".into()),
            ]),
        );
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        assert_eq!(
            report.config.listen_http.map(|a| a.to_string()).as_deref(),
            Some("[::1]:9000")
        );
        let advice = emitted(&report.diagnostics);
        assert!(advice.contains("GCS_MIMIC_SCHEME=both"));
        assert!(advice.contains("GCS_MIMIC_LISTEN_HTTP=[::1]:9000"));
        assert!(!advice.contains("future"));
        assert!(!advice.contains("second HTTP listener is unsupported"));
    }

    #[test]
    fn option_prefixes_require_exactly_two_native_or_one_or_two_legacy_hyphens() {
        for flag in [
            "---scheme",
            "----scheme",
            "---listen",
            "---port",
            "scheme",
            "port",
        ] {
            let report =
                crate::config::Config::preflight(&strings(&[flag, "http"]), &BTreeMap::new());
            assert!(report.diagnostics.is_err(), "{flag}");
            assert!(
                report
                    .diagnostics
                    .errors
                    .iter()
                    .any(|d| d.key == flag && d.fix.contains("--name VALUE pairs")),
                "{}",
                report.diagnostics
            );
            assert!(!report.diagnostics.to_string().contains("Legacy input"));
        }
        for object in ["---scheme", "----port", "-data", "-port", "-x"] {
            assert!(!cli_diagnostics(&strings(&["url", "public", "--object", object])).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_environment_encoding_follows_effective_precedence() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let env = vec![(
            OsString::from("GCS_MIMIC_WORKER_THREADS"),
            OsString::from_vec(vec![0xff]),
        )];
        let report = crate::config::Config::preflight_os(&strings(&["--worker-threads", "3"]), env);
        assert!(!report.diagnostics.is_err());
        assert_eq!(report.config.worker_threads, 3);
        let file = std::env::temp_dir().join(format!(
            "gcs-mimic-env-selector-{}.json",
            std::process::id()
        ));
        std::fs::write(&file, "{}").unwrap();
        let report = crate::config::Config::preflight_os(
            &["--config".into(), file.to_str().unwrap().into()],
            vec![(
                OsString::from("GCS_MIMIC_CONFIG"),
                OsString::from_vec(vec![0xff]),
            )],
        );
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn supplied_occurrences_and_original_utf8_keys_are_bounded() {
        for count in [1024, 1025] {
            let args: Vec<_> = (0..count).map(|_| "-port=8080".into()).collect();
            let report = crate::config::Config::preflight(&args, &BTreeMap::new());
            assert_eq!(
                report
                    .diagnostics
                    .warnings
                    .iter()
                    .filter(|d| d.key == "-port")
                    .count(),
                1024.min(count)
            );
            assert_eq!(
                report
                    .diagnostics
                    .errors
                    .iter()
                    .any(|d| d.key == "input_limit"),
                count > 1024
            );
        }
        for bytes in [256, 257] {
            let key = format!("--{}", "é".repeat(127)) + if bytes == 257 { "x" } else { "" };
            assert_eq!(key.len(), bytes);
            let report =
                crate::config::Config::preflight(&[key.clone(), "secret".into()], &BTreeMap::new());
            assert_eq!(
                report.diagnostics.errors[0].key,
                if bytes == 256 {
                    key.as_str()
                } else {
                    "input_limit"
                }
            );
        }
    }

    #[test]
    fn default_seed_is_adopted_only_when_present_and_disjoint() {
        let root =
            std::env::temp_dir().join(format!("gcs-mimic-default-seed-{}", std::process::id()));
        std::fs::create_dir_all(root.join("legacy")).unwrap();
        std::fs::write(root.join("legacy/object"), "fixture").unwrap();
        let legacy = root.join("legacy");
        let resolve = |data_dir: std::path::PathBuf,
                       sync_dir: Option<std::path::PathBuf>,
                       default: &std::path::Path| {
            let mut config = crate::config::Config {
                data_dir,
                sync_dir,
                default_seed_dir: Some(default.to_owned()),
                ..Default::default()
            };
            let warnings = resolve_default_seed(&mut config);
            (config.seed_dir, warnings)
        };
        let (seed, warnings) = resolve(root.join("native/new"), None, &legacy);
        assert_eq!(seed.as_deref(), Some(legacy.as_path()));
        assert!(warnings.is_empty());
        std::fs::create_dir(root.join("empty")).unwrap();
        let (seed, _) = resolve(root.join("native"), None, &root.join("empty"));
        assert_eq!(seed.as_deref(), Some(root.join("empty").as_path()));
        for native in [legacy.clone(), root.clone(), legacy.join("missing/new")] {
            assert_eq!(resolve(native, None, &legacy), (None, Vec::new()));
        }
        assert_eq!(
            resolve(root.join("native"), Some(legacy.clone()), &legacy),
            (None, Vec::new())
        );
        assert_eq!(
            resolve(root.join("native"), None, &root.join("missing")),
            (None, Vec::new())
        );
        std::fs::write(root.join("file"), "not a directory").unwrap();
        assert_eq!(
            resolve(root.join("native"), None, &root.join("file")),
            (None, Vec::new())
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&legacy, root.join("alias")).unwrap();
            assert_eq!(
                resolve(root.join("alias/missing"), None, &legacy),
                (None, Vec::new())
            );
            std::os::unix::fs::symlink(root.join("absent"), root.join("dangling")).unwrap();
            let (seed, warnings) = resolve(root.join("dangling/native"), None, &legacy);
            assert_eq!(seed, None);
            assert!(warnings[0].fix.contains("Cannot inspect"));
        }
        let mut explicit = crate::config::Config {
            data_dir: root.join("native"),
            seed_dir: Some(root.join("empty")),
            default_seed_dir: Some(legacy),
            ..Default::default()
        };
        assert!(resolve_default_seed(&mut explicit).is_empty());
        assert_eq!(
            explicit.seed_dir.as_deref(),
            Some(root.join("empty").as_path())
        );
        let mut unset = crate::config::Config {
            data_dir: root.join("native"),
            ..Default::default()
        };
        assert!(resolve_default_seed(&mut unset).is_empty());
        assert_eq!(unset.seed_dir, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_location_advice_uses_effective_native_setting() {
        for (value, expected) in [
            ("US", Some("US")),
            ("EU", Some("EU")),
            ("ASIA", Some("ASIA")),
            ("US-CENTRAL1", Some("US-CENTRAL1")),
            ("us-central1", Some("US-CENTRAL1")),
            ("asia-northeast1", Some("ASIA-NORTHEAST1")),
            ("nam4", Some("NAM4")),
            ("MARS-CENTRAL1", None),
            ("secret", None),
        ] {
            let env = BTreeMap::from([("FAKE_GCS_LOCATION".into(), value.into())]);
            let report = crate::config::Config::preflight(&[], &env);
            assert_eq!(report.diagnostics.is_err(), expected.is_none());
            let advice = emitted(&report.diagnostics);
            assert!(advice.contains("GCS_MIMIC_COMPAT_PROFILE=native"));
            if let Some(expected) = expected {
                assert_eq!(report.config.default_location, expected);
                assert!(advice.contains(&format!("GCS_MIMIC_DEFAULT_LOCATION={expected}")));
                assert!(advice.contains(&format!("--default-location {expected}")));
            } else {
                assert_eq!(report.diagnostics.errors[0].key, "FAKE_GCS_LOCATION");
                assert!(advice.contains("ASIA-NORTHEAST1"));
                assert!(!advice.contains(value));
            }
            assert!(!advice.contains("future"));
            assert!(!advice.contains("secret"));
        }
    }

    proptest! {
        #[test]
        fn arbitrary_legacy_and_native_inputs_never_panic(args in prop::collection::vec(any::<String>(), 0..80), env in prop::collection::btree_map("(FAKE_GCS_|GCS_MIMIC_)[a-zA-Z_-]{0,40}", any::<String>(), 0..40)) {
            let diagnostics = cli_diagnostics(&args);
            let report = crate::config::Config::preflight(&args, &env);
            let mut bytes = Vec::new();
            diagnostics.write(&mut bytes).unwrap();
            report.diagnostics.write(&mut bytes).unwrap();
            for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
                prop_assert!(serde_json::from_slice::<serde_json::Value>(line).is_ok());
            }
        }
    }
}
