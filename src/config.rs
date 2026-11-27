#![deny(clippy::unwrap_used, clippy::expect_used)]

use crate::config_input::Fix as SettingFix;
use crate::{
    auth::{AuthMode, SignedUrls, TestSigner, signature::SignedHostPort},
    bucket::Location,
    config_input::{CliCommand, Input, InputError, InputKind, Inputs},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics, LogLevel},
    time_limit::TimeLimit,
    url::PublicOrigin,
};
use serde_json::value::RawValue;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
};

mod assemble;
mod auth;
mod document;
mod domains;
mod draft;
mod help;
mod notifications;
mod rejection;
mod runtime;
mod setting;
mod storage;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod unix_tests;
pub use assemble::Config;
pub(crate) use assemble::{Tls, TlsIdentity};
#[cfg(test)]
pub(crate) use auth::Verification;
pub(crate) use auth::{AuthProfile, TrustedCaller};
pub use document::DocumentSource;
pub(crate) use document::{DocumentKind, Form};
pub use domains::VirtualHostDomains;
use domains::{DomainLimit, Inserted};
pub(crate) use draft::ConfigDraft;
pub use draft::ConfigurationReport;
use draft::{Settings, Validation, unknown_setting};
pub use help::help;
pub use runtime::RuntimeEndpoints;
pub(crate) use runtime::{
    ApiBaseSource, ConnectionScheme, EndpointError, EndpointSnapshot, RequestEndpoints,
};
pub(crate) use setting::{MAX_OBJECT_BYTES, MAX_TREE_DEPTH, Setting, Spelled};
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

pub use crate::choice::InvalidChoice;
use crate::choice::string_enum;

string_enum! {
    pub enum CompatProfile {
        #[default]
        Native => "native",
        /// fake-gcs-server's behavior where it differs from Cloud Storage, for migrations.
        FakeGcsServer => "fake-gcs-server",
    }
    "compat_profile must be native or fake-gcs-server";
    withdrawn {
        "legacy-emulator" => "compat_profile=legacy-emulator was renamed; set compat_profile=fake-gcs-server",
    }
}

string_enum! {
    /// Whether stored lifecycle Delete rules execute.
    pub enum LifecycleExecution {
        #[default]
        Enabled => "enabled",
        Disabled => "disabled",
    }
    "lifecycle_execution must be enabled or disabled"
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
    /// What a start does with an existing bucket that the buckets document names, whether the
    /// document comes from `buckets_file` or inline from `buckets`: the document is the source
    /// of truth either way, so its edits apply unless `create` is chosen.
    pub enum BucketsMode {
        /// Creates missing buckets only; existing ones keep every setting.
        Create => "create",
        /// Also applies the members each entry writes to its existing bucket.
        #[default]
        Reconcile => "reconcile",
    }
    "buckets_mode must be create or reconcile"
}

string_enum! {
    pub enum Scheme {
        /// HTTP and HTTPS on the primary listener, chosen by each connection's first byte.
        #[default]
        Auto => "auto",
        Http => "http",
        Https => "https",
        /// HTTPS on the primary listener and HTTP on `listen_http`, as fake-gcs-server's
        /// two-port command line serves them.
        Both => "both",
    }
    "scheme must be auto, http, https or both"
}

string_enum! {
    /// How the gRPC listener secures its connections, with the identity HTTPS uses.
    pub enum GrpcScheme {
        /// h2c or gRPC over TLS, chosen by each connection's first byte.
        #[default]
        Auto => "auto",
        /// Plaintext HTTP/2 only.
        H2c => "h2c",
        /// gRPC over TLS only, negotiating `h2` through ALPN.
        Tls => "tls",
    }
    "grpc_scheme must be auto, h2c or tls"
}

impl GrpcScheme {
    /// The gRPC entry of the ready event's `listeners`: `h2c` wherever plaintext is accepted,
    /// which consumers already search for, and `h2` for TLS only.
    pub fn listener_scheme(self) -> &'static str {
        match self {
            Self::Auto | Self::H2c => "h2c",
            Self::Tls => "h2",
        }
    }
}

impl Scheme {
    /// The scheme of URLs built without a connection to take it from, such as the fallback API
    /// base and the ready event's primary listener. `auto` uses `http`, which every client can
    /// follow without trusting a CA.
    pub fn primary(self) -> &'static str {
        match self {
            Self::Auto | Self::Http => "http",
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
        let setting = key.parse::<Setting>().ok();
        let invalid = || Diagnostic::new(DiagnosticSource::Config, &key, member_fix(setting));
        // The server validates a document; this helper only checks that the member may hold one.
        if document::holds_document(setting) {
            document::member_text(&raw).ok_or_else(invalid)?;
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        if setting_text(&value).is_none() {
            return Err(invalid());
        }
        if setting == Some(Setting::PublicBaseUrl) {
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
/// not take. `setting` is what the member names, if it names one.
fn member_text(setting: Option<Setting>, raw: &RawValue) -> Option<String> {
    if document::holds_document(setting) {
        return document::member_text(raw);
    }
    let value: serde_json::Value = serde_json::from_str(raw.get()).ok()?;
    match setting {
        Some(Setting::DefaultLocation) if value.is_null() => Some(String::new()),
        Some(Setting::AdminEnabled) if value.is_boolean() => Some(value.to_string()),
        Some(Setting::BucketsFile) => value.as_str().map(str::to_owned),
        Some(_) | None => setting_text(&value),
    }
}

fn member_fix(setting: Option<Setting>) -> &'static str {
    if document::holds_document(setting) {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("virtual-host domain must be a DNS name without scheme, port, wildcard or trailing dot")]
pub struct InvalidDnsName;

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
    fn a_withdrawn_value_fails_with_its_own_fix() {
        string_enum! {
            enum Shape {
                #[default]
                Round => "round",
                Square => "square",
            }
            "shape must be round or square";
            withdrawn {
                "circle" => "shape=circle was renamed; use shape=round",
            }
        }
        assert_eq!("round".parse(), Ok(Shape::Round));
        assert_eq!(
            "circle".parse::<Shape>(),
            Err(InvalidChoice("shape=circle was renamed; use shape=round"))
        );
        assert_eq!(
            "oval".parse::<Shape>(),
            Err(InvalidChoice("shape must be round or square"))
        );
        assert_eq!(Shape::default().as_str(), "round");
        assert_eq!(Shape::Square.as_str(), "square");
    }

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
        let error = ConfigDraft::load(&args, &BTreeMap::new()).unwrap_err();
        assert!(error.to_string().contains("admin_principal"));
        let mut configured = args.to_vec();
        configured.extend(["--admin-principal".into(), "admin".into()]);
        ConfigDraft::load(&configured, &BTreeMap::new()).unwrap();
        let args = [
            "--admin-allowed-hosts",
            "Dev.Example.com.,other.example.com",
        ]
        .map(str::to_owned);
        assert_eq!(
            ConfigDraft::load(&args, &BTreeMap::new())
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
            assert!(
                ConfigDraft::load(&args, &BTreeMap::new()).is_err(),
                "{value}"
            );
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
            let error = ConfigDraft::load(&args, &BTreeMap::new()).unwrap_err();
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
            ConfigDraft::load(&args, &BTreeMap::new())
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
        let defaults = ConfigDraft::default();
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
        std::assert_matches!(
            report.auth_profile(),
            Some(AuthProfile::Strict {
                trusted: TrustedCaller::BuiltIn,
                ..
            })
        );
        let report = Config::preflight(
            &paired
                .iter()
                .map(|option| (*option).to_owned())
                .collect::<Vec<_>>(),
            &BTreeMap::new(),
        );
        std::assert_matches!(
            report.auth_profile(),
            Some(AuthProfile::Strict {
                trusted: TrustedCaller::Named(principal),
                ..
            }) if principal == "app@example.invalid"
        );
    }

    /// Diagnostics for several settings come out in native setting name order, not in the order
    /// the options were given or the settings are declared.
    #[test]
    fn diagnostics_follow_native_setting_names() {
        let args: Vec<String> = [
            "--seed-project",
            "bad project",
            "--scheme",
            "bad",
            "--backend",
            "bad",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let report = Config::preflight(&args, &BTreeMap::new());
        let keys: Vec<&str> = report
            .diagnostics
            .errors
            .iter()
            .map(|error| error.key.as_str())
            .collect();
        assert_eq!(
            keys,
            ["--backend", "--scheme", "--seed-project"],
            "{}",
            report.diagnostics
        );
        assert_eq!(
            report.diagnostics.errors[2].fix,
            "seed_project: project must be a nonempty identifier of at most 256 bytes"
        );
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
            ConfigDraft::load(&args, &BTreeMap::new())
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
        assert!(ConfigDraft::default().trusted_allowed_hosts.is_empty());
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
        let config = Config::load(&[], &BTreeMap::new()).unwrap();
        let bound = SocketAddr::V6(std::net::SocketAddrV6::new(
            "fe80::1".parse().unwrap(),
            4443,
            7,
            3,
        ));
        assert_eq!(
            config.effective_api_base_url(bound),
            "http://[fe80::1]:4443"
        );
        assert_eq!(
            config.effective_api_base_url(SocketAddr::V6(std::net::SocketAddrV6::new(
                std::net::Ipv6Addr::UNSPECIFIED,
                4443,
                7,
                3
            ))),
            "http://[::1]:4443"
        );
    }

    #[test]
    fn tcp_listener_settings_assemble_the_selected_tls_identity() {
        let load = |flags: &[&str]| {
            let flags: Vec<_> = flags.iter().map(|flag| (*flag).to_owned()).collect();
            Config::load(&flags, &BTreeMap::new()).unwrap()
        };
        let default = load(&[]);
        std::assert_matches!(
            default.tls().map(Tls::identity),
            Some(TlsIdentity::Development)
        );
        let plain = load(&["--scheme=http"]);
        assert!(plain.tls().is_none());
        assert_eq!(plain.grpc_scheme(), None);
        assert!(plain.grpc_listener().is_none());

        for (flags, grpc_scheme) in [
            (&["--scheme=https"][..], None),
            (
                &[
                    "--scheme=http",
                    "--grpc-listen=127.0.0.1:0",
                    "--grpc-scheme=tls",
                ][..],
                Some(GrpcScheme::Tls),
            ),
        ] {
            let config = load(&[flags, &["--tls-cert=a", "--tls-key=b"]].concat());
            assert_eq!(config.grpc_scheme(), grpc_scheme);
            assert_eq!(
                config.grpc_listener(),
                grpc_scheme.map(|scheme| ("127.0.0.1:0".parse().unwrap(), scheme)),
            );
            let Some(TlsIdentity::Provided { certificate, key }) = config.tls().map(Tls::identity)
            else {
                panic!("expected configured identity for {flags:?}");
            };
            assert_eq!(certificate, Path::new("a"));
            assert_eq!(key, Path::new("b"));
        }
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
            ConfigDraft::load,
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
            ConfigDraft::load,
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
                ConfigDraft::load(&args, &BTreeMap::new())
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
    fn grpc_scheme_follows_precedence_and_requires_grpc_listen() {
        assert_eq!(ConfigDraft::default().grpc_scheme, GrpcScheme::Auto);
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(
            &file,
            r#"{"grpc_scheme":"h2c","grpc_listen":"127.0.0.1:4444"}"#,
        )
        .unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_GRPC_SCHEME".into(), "tls".into())]);
        assert_precedence(
            &args,
            &env,
            &["--grpc-scheme", "auto"],
            [
                GrpcScheme::Auto,
                GrpcScheme::H2c,
                GrpcScheme::Tls,
                GrpcScheme::Auto,
            ],
            ConfigDraft::load,
            |config| config.grpc_scheme,
        );
        assert_rejected(
            "grpc_scheme",
            &["", "TLS", "https", "h2"],
            "grpc_scheme must be auto, h2c or tls",
        );
        assert_rejected("grpc_scheme", &["tls"], "requires grpc_listen");
        // A broken listener setting is the one fault to report, not a missing listener.
        let args = ["--grpc-listen", "localhost:4444", "--grpc-scheme", "tls"].map(str::to_owned);
        let errors = ConfigDraft::load(&args, &BTreeMap::new())
            .unwrap_err()
            .errors;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].key, "--grpc-listen");
    }

    /// The gRPC listener uses the HTTPS identity, so with `scheme=http` the TLS settings stay
    /// valid while it serves TLS, and are refused as before when nothing does.
    #[test]
    fn tls_settings_are_accepted_when_only_the_grpc_listener_uses_them() {
        let tls = [
            "--scheme",
            "http",
            "--tls-cert",
            "cert.pem",
            "--tls-key",
            "key.pem",
            "--tls-handshake-timeout-ms",
            "100",
        ];
        for (grpc, accepted) in [
            (&[][..], false),
            (&["--grpc-listen", "127.0.0.1:0"][..], true),
            (
                &["--grpc-listen", "127.0.0.1:0", "--grpc-scheme", "auto"][..],
                true,
            ),
            (
                &["--grpc-listen", "127.0.0.1:0", "--grpc-scheme", "tls"][..],
                true,
            ),
            (
                &["--grpc-listen", "127.0.0.1:0", "--grpc-scheme", "h2c"][..],
                false,
            ),
        ] {
            let args: Vec<String> = tls
                .iter()
                .chain(grpc)
                .map(|arg| (*arg).to_owned())
                .collect();
            match ConfigDraft::load(&args, &BTreeMap::new()) {
                Ok(config) => {
                    assert!(accepted, "{grpc:?}");
                    assert_eq!(config.tls_handshake_timeout.as_millis(), 100);
                }
                Err(diagnostics) => {
                    assert!(!accepted, "{grpc:?}: {diagnostics}");
                    assert_eq!(diagnostics.errors.len(), 3, "{diagnostics}");
                    assert!(
                        diagnostics.errors.iter().all(|error| error
                            .fix
                            .contains("or grpc_listen with grpc_scheme=auto or tls")),
                        "{diagnostics}"
                    );
                }
            }
        }
        // One path alone is still an error when only the gRPC listener uses TLS.
        let args = [
            "--scheme",
            "http",
            "--grpc-listen",
            "127.0.0.1:0",
            "--tls-cert",
            "cert.pem",
        ]
        .map(str::to_owned);
        let errors = ConfigDraft::load(&args, &BTreeMap::new())
            .unwrap_err()
            .errors;
        assert!(
            errors
                .iter()
                .any(|error| error.fix.contains("must be supplied together")),
            "{errors:?}"
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
        let errors = ConfigDraft::load(&args, &BTreeMap::new())
            .unwrap_err()
            .errors;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].key, "--grpc-listen");
    }

    /// Only an explicit value is configuration; loading applies the default that the settled
    /// test signer selects, which config::auth::tests covers.
    #[test]
    fn signed_host_port_follows_precedence_in_both_auth_modes() {
        assert_eq!(ConfigDraft::default().signed_host_port, None);
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
            ConfigDraft::load,
            |config| config.signed_host_port,
        );
        let identities = dir.0.join("identities.json");
        let lenient = |mode: &[&str]| {
            let mut args = mode.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
            args.extend(["--signed-host-port".into(), "lenient".into()]);
            ConfigDraft::load(&args, &BTreeMap::new())
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
            ConfigDraft::load(&exact, &BTreeMap::new())
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
        assert!(ConfigDraft::load(&args, &BTreeMap::new()).is_err());
    }

    #[test]
    fn cors_fallback_defaults_by_auth_mode_with_precedence_in_both_profiles() {
        let empty = BTreeMap::new();
        let load = |args: &[String], env| {
            ConfigDraft::load(args, env)
                .unwrap()
                .resolved_cors_fallback()
        };
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
            ConfigDraft::load,
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
        for profile in ["native", "fake-gcs-server"] {
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
        assert!(ConfigDraft::load(&args[..2], &empty).is_err());
        // A bare flag names the accepted value instead of an unrelated example.
        let errors = ConfigDraft::load(&["--cors-fallback".into()], &empty).unwrap_err();
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
            ConfigDraft::load,
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
        let error = ConfigDraft::load(&ignored, &empty).unwrap_err().to_string();
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
            ConfigDraft::load,
            |config| config.signed_urls,
        );
        let empty = BTreeMap::new();
        let load = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            ConfigDraft::load(&args, &empty)
        };
        let effective = |args: &[&str]| load(args).unwrap().resolved_signed_urls();
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
        assert_eq!(config.resolved_signed_urls(), SignedUrls::Verify);
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
            ConfigDraft::load(&[], &empty)
                .unwrap()
                .local_creator_email
                .as_deref(),
            Some(DEFAULT_LOCAL_CREATOR_EMAIL)
        );
        let env = BTreeMap::from([(
            "GCS_MIMIC_LOCAL_CREATOR_EMAIL".into(),
            "env@example.test".into(),
        )]);
        assert_eq!(
            ConfigDraft::load(&[], &env)
                .unwrap()
                .local_creator_email
                .as_deref(),
            Some("env@example.test")
        );
        let cli = ["--local-creator-email", ""].map(str::to_owned);
        assert_eq!(
            ConfigDraft::load(&cli, &env).unwrap().local_creator_email,
            None
        );
        for bad in ["plain", "a b@example.test", "@example.test", "user-x"] {
            let args = ["--local-creator-email", bad].map(str::to_owned);
            assert!(
                ConfigDraft::load(&args, &empty)
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
            ConfigDraft::load(&strict, &empty)
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
            ConfigDraft::load(&strict_args, &strict_env)
                .unwrap_err()
                .to_string()
                .contains("applies only to auth_mode=permissive")
        );
        let legacy = ["--compat-profile", "fake-gcs-server"].map(str::to_owned);
        assert!(ConfigDraft::load(&legacy, &empty).is_ok());
        let mut conflict = legacy.to_vec();
        conflict.extend(["--local-creator-email".into(), "a@example.test".into()]);
        assert!(
            ConfigDraft::load(&conflict, &empty)
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
            ConfigDraft::load,
            |config| config.default_location,
        );
        std::fs::write(&file, r#"{"default_location":null}"#).unwrap();
        assert_eq!(
            ConfigDraft::load(&args, &empty).unwrap().default_location,
            "US"
        );
        std::fs::write(&file, r#"{"default_location":"EU"}"#).unwrap();
        assert_rejected(
            "default_location",
            &[" EU", "EU ", "unknown", "MARS-CENTRAL1"],
            "US or ASIA-NORTHEAST1",
        );
        for value in ["US", "EU", "ASIA", "ASIA-NORTHEAST1", "EUR4", "NAM4"] {
            let args = ["--default-location", value].map(str::to_owned);
            assert_eq!(
                ConfigDraft::load(&args, &empty).unwrap().default_location,
                value
            );
            let mut args = args.to_vec();
            args.extend(["--compat-profile".into(), "fake-gcs-server".into()]);
            assert!(
                ConfigDraft::load(&args, &empty)
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
                ConfigDraft::load(&args, &empty).unwrap().default_location,
                expected
            );
        }
        let args = [
            "--config".into(),
            file.to_str().unwrap().into(),
            "--compat-profile".into(),
            "fake-gcs-server".into(),
        ];
        assert!(ConfigDraft::load(&args, &empty).is_err());
        assert!(
            ConfigDraft::load(&["--compat-profile".into(), "fake-gcs-server".into()], &env)
                .is_err()
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
            ConfigDraft::load,
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
            assert_eq!(
                ConfigDraft::load(&args, &env).unwrap().log_level,
                LogLevel::Warn
            );
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
        let result = ConfigDraft::load(
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
            let loaded = ConfigDraft::load(&args, &env).unwrap();
            std::assert_matches!(loaded.buckets, Some(DocumentSource::File(_)));
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
            let errors = ConfigDraft::load(&args, &env).unwrap_err();
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
            ConfigDraft::load,
            |config| config.buckets,
        );
        assert_eq!(ConfigDraft::default().buckets, None);
    }

    #[test]
    fn buckets_mode_defaults_to_reconcile_and_requires_a_buckets_document() {
        let directory = crate::test_support::TestDir::new();
        let file = directory.0.join("config.json");
        std::fs::write(
            &file,
            r#"{"buckets_file":"buckets.json","buckets_mode":"create"}"#,
        )
        .unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_BUCKETS_MODE".into(), "reconcile".into())]);
        assert_precedence(
            &args,
            &env,
            &["--buckets-mode", "create"],
            [
                BucketsMode::Reconcile,
                BucketsMode::Create,
                BucketsMode::Reconcile,
                BucketsMode::Create,
            ],
            ConfigDraft::load,
            |config| config.buckets_mode,
        );
        assert_rejected(
            "buckets_mode",
            &["", "Reconcile", "merge", "true"],
            "create or reconcile",
        );
        // Each source of the mode is refused alone, and the fix names the missing setting.
        std::fs::write(&file, r#"{"buckets_mode":"create"}"#).unwrap();
        for (key, args, env) in [
            (
                "--buckets-mode",
                vec!["--buckets-mode".to_owned(), "reconcile".into()],
                BTreeMap::new(),
            ),
            (
                "GCS_MIMIC_BUCKETS_MODE",
                vec![],
                BTreeMap::from([("GCS_MIMIC_BUCKETS_MODE".into(), "reconcile".into())]),
            ),
            (
                "buckets_mode",
                vec!["--config".into(), file.to_str().unwrap().into()],
                BTreeMap::new(),
            ),
        ] {
            let errors = ConfigDraft::load(&args, &env).unwrap_err().errors;
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert_eq!(errors[0].key, key);
            assert!(
                errors[0].fix.contains("requires buckets_file"),
                "{errors:?}"
            );
        }
        // An invalid selector is reported once, without a second error about the mode.
        let errors = ConfigDraft::load(
            &["--buckets-file", "", "--buckets-mode", "reconcile"].map(str::to_owned),
            &BTreeMap::new(),
        )
        .unwrap_err()
        .errors;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].key, "--buckets-file");
    }

    /// The old name stops startup from every source and names the new one as that source
    /// spells it, so the fix can be copied back.
    #[test]
    fn buckets_file_mode_was_renamed_to_buckets_mode() {
        let directory = crate::test_support::TestDir::new();
        let file = directory.0.join("config.json");
        std::fs::write(
            &file,
            r#"{"buckets_file":"buckets.json","buckets_file_mode":"create"}"#,
        )
        .unwrap();
        for (key, replacement, args, env) in [
            (
                "--buckets-file-mode",
                "use --buckets-mode.",
                [
                    "--buckets-file",
                    "buckets.json",
                    "--buckets-file-mode",
                    "create",
                ]
                .map(str::to_owned)
                .to_vec(),
                BTreeMap::new(),
            ),
            (
                "GCS_MIMIC_BUCKETS_FILE_MODE",
                "use GCS_MIMIC_BUCKETS_MODE.",
                ["--buckets-file", "buckets.json"]
                    .map(str::to_owned)
                    .to_vec(),
                BTreeMap::from([("GCS_MIMIC_BUCKETS_FILE_MODE".into(), "create".into())]),
            ),
            (
                "buckets_file_mode",
                "use buckets_mode.",
                vec!["--config".into(), file.to_str().unwrap().into()],
                BTreeMap::new(),
            ),
        ] {
            let errors = ConfigDraft::load(&args, &env).unwrap_err().errors;
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert_eq!(errors[0].key, key);
            assert_eq!(
                errors[0].fix,
                format!("buckets_file_mode was renamed to buckets_mode; {replacement}")
            );
        }
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
        assert_eq!(report.draft.worker_threads, 3);
        assert_eq!(report.diagnostics.errors.len(), 4);
        assert!(report.valid(&[Setting::WorkerThreads]));
        assert!(!report.valid(&[Setting::AuthMode, Setting::TlsCert]));
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
            assert!(!report.valid(&[Setting::AuthMode, Setting::TlsCert, Setting::Scheme]));
        }
        let env = BTreeMap::from([
            ("GCS_MIMIC_AUTH_MODE".into(), "strict".into()),
            ("GCS_MIMIC_IDENTITY_FILE".into(), "ids.json".into()),
        ]);
        let report = Config::preflight(&["---auth-mode=strict".into()], &env);
        assert_eq!(report.diagnostics.errors.len(), 1);
        assert!(!report.valid(&[Setting::AuthMode]));
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
            ConfigDraft::load,
            |config| {
                config
                    .public_base_url
                    .map(|origin| origin.as_str().to_owned())
            },
        );
        let config = Config::load(&args, &env).unwrap();
        assert_eq!(
            config.public_base_url().map(PublicOrigin::as_str),
            Some("http://storage.invalid")
        );
        assert!(config.public_virtual_host_enabled());
        assert_eq!(
            **config.xml_virtual_host_domains(),
            ["storage.invalid", "invalid"]
        );
        args[5] = "storage.invalid,STORAGE.invalid".into();
        assert!(
            ConfigDraft::load(&args, &env)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        args[5] = (0..32)
            .map(|i| format!("host{i}.invalid"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            ConfigDraft::load(&args, &env)
                .unwrap_err()
                .to_string()
                .contains("at most 32")
        );
        args[3] = "http://host0.invalid".into();
        assert_eq!(
            ConfigDraft::load(&args, &env)
                .unwrap()
                .xml_virtual_host_domains
                .len(),
            32
        );
        args[3] = "http://storage.invalid".into();
        args[5] = "invalid".into();
        args.extend(["--api-base-url".into(), "http://api.storage.invalid".into()]);
        assert!(
            ConfigDraft::load(&args, &env)
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
            let (config, diagnostics) = report.into_config().unwrap();
            assert_eq!(diagnostics.warnings.len(), 1);
            assert!(
                diagnostics.warnings[0]
                    .fix
                    .contains("virtual-host routing is disabled")
            );
            assert!(!config.public_virtual_host_enabled());
            assert_eq!(**config.xml_virtual_host_domains(), ["storage.invalid"]);
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
            ConfigDraft::load,
            |config| config.xml_virtual_host_domains.to_vec(),
        );
        *args.last_mut().unwrap() = String::new();
        assert!(
            ConfigDraft::load(&args, &env)
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
            assert!(ConfigDraft::load(&args, &env).is_err(), "{invalid}");
        }
        *args.last_mut().unwrap() = (0..33)
            .map(|i| format!("host{i}.invalid"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(ConfigDraft::load(&args, &env).is_err());
        args[3] = "example.invalid".into();
        args.extend([
            "--api-base-url".into(),
            "http://api.example.invalid:4443".into(),
        ]);
        assert!(
            ConfigDraft::load(&args, &env)
                .unwrap_err()
                .to_string()
                .contains("api_base_url host selects an XML virtual bucket")
        );
        *args.last_mut().unwrap() = "http://example.invalid:4443".into();
        assert!(ConfigDraft::load(&args, &env).is_ok());
        args[3] = "example.invalid,api.example.invalid".into();
        *args.last_mut().unwrap() = "http://api.example.invalid:4443".into();
        assert!(ConfigDraft::load(&args, &env).is_ok());
        std::fs::remove_file(file).unwrap();
    }

    fn load_http(
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<ConfigDraft, Diagnostics> {
        let mut args = args.to_vec();
        args.extend(["--scheme".into(), "http".into()]);
        ConfigDraft::load(&args, env)
    }

    #[test]
    fn tls_configuration_precedence_and_validation() {
        let defaults = ConfigDraft::default();
        assert_eq!(defaults.scheme, Scheme::Auto);
        assert_eq!(defaults.api_base_url, None);
        let loaded = ConfigDraft::load(&[], &BTreeMap::new()).unwrap();
        assert_eq!(loaded.scheme, Scheme::Auto);
        assert_eq!(loaded.tls_cert, None);
        assert_eq!(loaded.tls_key, None);
        // TLS files choose the certificate; only scheme=https refuses plain HTTP.
        assert_eq!(
            ConfigDraft::load(
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
            Scheme::Auto
        );
        assert_eq!(load_http(&[], &BTreeMap::new()).unwrap().api_base_url, None);
        assert_eq!(defaults.tls_handshake_timeout.as_millis(), 5_000);
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"scheme":"https","tls_cert":"file-cert.pem","tls_key":"file-key.pem","tls_handshake_timeout_ms":100}"#).unwrap();
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let loaded = ConfigDraft::load(&args, &BTreeMap::new()).unwrap();
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
        let loaded = ConfigDraft::load(&args, &env).unwrap();
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
            ConfigDraft::load,
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
                ConfigDraft::load(
                    &args.into_iter().map(String::from).collect::<Vec<_>>(),
                    &BTreeMap::new()
                )
                .is_err()
            );
        }
        for invalid in ["18446744073709551616", "-1", "unlimited"] {
            let mut args = cli.clone();
            *args.last_mut().unwrap() = invalid.into();
            assert!(ConfigDraft::load(&args, &env).is_err());
        }
        for bound in [0, 1, u64::MAX] {
            let mut args = cli.clone();
            *args.last_mut().unwrap() = bound.to_string();
            assert_eq!(
                ConfigDraft::load(&args, &env)
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
            Config::load(&args, &env).unwrap().listen_http(),
            Some("[::1]:8001".parse().unwrap())
        );
        env.insert("GCS_MIMIC_LISTEN_HTTP".into(), "[::1]:8002".into());
        assert_eq!(
            Config::load(&args, &env).unwrap().listen_http(),
            Some("[::1]:8002".parse().unwrap())
        );
        args.extend(["--listen-http".into(), "[::1]:0".into()]);
        assert_eq!(
            Config::load(&args, &env).unwrap().listen_http(),
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
                config.listen_http(),
                Some(SocketAddr::new(config.listen().ip(), 8000))
            );
            assert!(
                config
                    .effective_api_base_url(config.listen())
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
                ConfigDraft::load(&arguments, &BTreeMap::new()).is_err(),
                "{arguments:?}"
            );
        }
        let config = ConfigDraft::load(
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
        assert_eq!(config.listen, config.resolved_listen_http().unwrap());
        let config = ConfigDraft::load(
            &[],
            &BTreeMap::from([("GCS_MIMIC_SCHEME".into(), "both".into())]),
        )
        .unwrap();
        assert_eq!(config.scheme, Scheme::Both);
    }

    #[test]
    fn compat_profile_precedence_and_validation() {
        assert_eq!(ConfigDraft::default().compat_profile, CompatProfile::Native);
        let dir = TestDir::new();
        let path = dir.0.join("config.json");
        std::fs::write(&path, r#"{"compat_profile":"fake-gcs-server"}"#).unwrap();
        let args = vec!["--config".into(), path.to_str().unwrap().into()];
        let env = BTreeMap::from([("GCS_MIMIC_COMPAT_PROFILE".into(), "native".into())]);
        let args = assert_precedence(
            &args,
            &env,
            &["--compat-profile", "fake-gcs-server"],
            [
                CompatProfile::Native,
                CompatProfile::FakeGcsServer,
                CompatProfile::Native,
                CompatProfile::FakeGcsServer,
            ],
            load_http,
            |config| config.compat_profile,
        );
        assert_rejected(
            "compat_profile",
            &["", "NATIVE", "fake_gcs_server", "permissive", "unknown"],
            "compat_profile must be native or fake-gcs-server",
        );
        // The value before 0.20.0 stops startup from every source and names the new one.
        assert_rejected(
            "compat_profile",
            &["legacy-emulator"],
            "compat_profile=legacy-emulator was renamed; set compat_profile=fake-gcs-server",
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
        let default = ConfigDraft::default();
        assert_eq!(
            (default.max_seed_entries, default.max_seed_depth),
            (None, None)
        );
        let args = ["--max-seed-entries", "20000", "--max-seed-depth", "40"].map(str::to_owned);
        let config = ConfigDraft::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(
            (config.max_seed_entries, config.max_seed_depth),
            (limit(20_000), limit(40))
        );
        let env = BTreeMap::from([("GCS_MIMIC_MAX_SEED_DEPTH".to_owned(), "50".to_owned())]);
        assert_eq!(
            ConfigDraft::load(&[], &env).unwrap().max_seed_depth,
            limit(50)
        );
        let args = ["--max-seed-entries", "0", "--max-seed-depth", "0"].map(str::to_owned);
        let config = ConfigDraft::load(&args, &BTreeMap::new()).unwrap();
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
    fn the_seed_watch_interval_defaults_to_a_second_and_zero_stops_the_passes() {
        use std::time::Duration;
        let interval = |args: &[&str], env: &[(&str, &str)]| {
            let args = args.iter().copied().map(str::to_owned).collect::<Vec<_>>();
            let env = env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            ConfigDraft::load(&args, &env).unwrap().seed_watch_interval
        };
        assert_eq!(interval(&[], &[]), Some(Duration::from_secs(1)));
        assert_eq!(
            interval(&["--seed-watch-interval-ms", "250"], &[]),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            interval(&[], &[("GCS_MIMIC_SEED_WATCH_INTERVAL_MS", "86400000")]),
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(interval(&["--seed-watch-interval-ms", "0"], &[]), None);
        assert_rejected(
            "seed_watch_interval_ms",
            &["-1", "86400001"],
            "seed_watch_interval_ms must be an integer in 0..=86400000",
        );
    }

    #[test]
    fn the_bucket_batch_limit_defaults_to_unlimited_and_rejects_values_out_of_range() {
        let limit = NonZeroUsize::new;
        assert_eq!(ConfigDraft::default().max_buckets_per_batch, None);
        let args = ["--max-buckets-per-batch", "1000000"].map(str::to_owned);
        let config = ConfigDraft::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(config.max_buckets_per_batch, limit(1_000_000));
        let env = BTreeMap::from([("GCS_MIMIC_MAX_BUCKETS_PER_BATCH".to_owned(), "1".to_owned())]);
        assert_eq!(
            ConfigDraft::load(&[], &env).unwrap().max_buckets_per_batch,
            limit(1)
        );
        let args = ["--max-buckets-per-batch", "0"].map(str::to_owned);
        let config = ConfigDraft::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(config.max_buckets_per_batch, None);
        let directory = TestDir::new();
        let file = directory.0.join("config.json");
        std::fs::write(&file, r#"{"max_buckets_per_batch":2000}"#).unwrap();
        let args = ["--config".to_owned(), file.to_str().unwrap().to_owned()];
        assert_eq!(
            ConfigDraft::load(&args, &BTreeMap::new())
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

    /// A configuration written for directory synchronization stops startup from every source,
    /// whatever its value, and the fix names the seed directory as that source spells it.
    #[test]
    fn directory_synchronization_settings_were_removed_from_every_source() {
        let dir = TestDir::new();
        for (name, value, json) in [
            ("sync_dir", "tree", r#""tree""#),
            ("sync_delete_guard", "false", "false"),
            (
                "sync_reinitialize",
                "00112233445566778899aabbccddeeff",
                r#""id""#,
            ),
            ("max_sync_entries", "100", "100"),
            ("max_sync_depth", "0", "0"),
            ("sync_token", "secret", r#""secret""#),
        ] {
            let file = dir.0.join(format!("{name}.json"));
            std::fs::write(&file, format!(r#"{{"{name}":{json}}}"#)).unwrap();
            let flag = format!("--{}", name.replace('_', "-"));
            let variable = format!("GCS_MIMIC_{}", name.to_ascii_uppercase());
            for (key, seed_dir, args, env) in [
                (
                    flag.clone(),
                    "--seed-dir",
                    vec![flag, value.to_owned()],
                    BTreeMap::new(),
                ),
                (
                    variable.clone(),
                    "GCS_MIMIC_SEED_DIR",
                    Vec::new(),
                    BTreeMap::from([(variable, value.to_owned())]),
                ),
                (
                    name.to_owned(),
                    "seed_dir",
                    vec!["--config".to_owned(), file.to_str().unwrap().to_owned()],
                    BTreeMap::new(),
                ),
            ] {
                let errors = ConfigDraft::load(&args, &env).unwrap_err().errors;
                assert_eq!(errors.len(), 1, "{errors:?}");
                assert_eq!(errors[0].key, key);
                assert_eq!(
                    errors[0].fix,
                    format!(
                        "{name} was removed with directory synchronization; remove it. {seed_dir} reads a folder without writing to it."
                    )
                );
                assert!(!errors[0].fix.contains(value), "{key}");
            }
        }
    }

    #[test]
    fn session_limits_default_to_unlimited_and_zero_stays_outside_the_config() {
        let default = ConfigDraft::default();
        assert_eq!(default.max_resumable_sessions, None);
        assert_eq!(default.max_rewrite_sessions, None);
        type Read = fn(&ConfigDraft) -> Option<NonZeroU32>;
        let settings: [(&str, Read); 2] = [
            ("max_resumable_sessions", |config| {
                config.max_resumable_sessions
            }),
            ("max_rewrite_sessions", |config| config.max_rewrite_sessions),
        ];
        for (setting, read) in settings {
            let flag = format!("--{}", setting.replace('_', "-"));
            let load = |value: &str| {
                ConfigDraft::load(&[flag.clone(), value.to_owned()], &BTreeMap::new()).unwrap()
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
            ConfigDraft::load(&[], &env).unwrap().max_resumable_sessions,
            NonZeroU32::new(9)
        );
    }

    #[test]
    fn transfer_timeout_precedence_and_bounds() {
        type Field = fn(ConfigDraft) -> TimeLimit;
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
            let value = |config: ConfigDraft| limit(config).as_millis();
            let flag = format!("--{}", setting.replace('_', "-"));
            let key = format!("GCS_MIMIC_{}", setting.to_ascii_uppercase());
            let args = vec!["--config".into(), file.to_str().unwrap().into()];
            std::fs::write(&file, format!(r#"{{"{setting}":100}}"#)).unwrap();
            let env = BTreeMap::from([(key, "200".into())]);
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

    /// The removed alias stops startup from every source and names the three settings that
    /// replace it as that source spells them.
    #[test]
    fn connection_timeout_ms_was_removed_in_favor_of_three_settings() {
        let dir = TestDir::new();
        let file = dir.0.join("config.json");
        std::fs::write(&file, r#"{"connection_timeout_ms":5000}"#).unwrap();
        for (key, replacement, args, env) in [
            (
                "--connection-timeout-ms",
                "--header-read-timeout-ms, --body-inactivity-timeout-ms and --request-timeout-ms",
                ["--connection-timeout-ms", "5000"]
                    .map(str::to_owned)
                    .to_vec(),
                BTreeMap::new(),
            ),
            (
                "GCS_MIMIC_CONNECTION_TIMEOUT_MS",
                "GCS_MIMIC_HEADER_READ_TIMEOUT_MS, GCS_MIMIC_BODY_INACTIVITY_TIMEOUT_MS and GCS_MIMIC_REQUEST_TIMEOUT_MS",
                Vec::new(),
                BTreeMap::from([("GCS_MIMIC_CONNECTION_TIMEOUT_MS".into(), "5000".into())]),
            ),
            (
                "connection_timeout_ms",
                "header_read_timeout_ms, body_inactivity_timeout_ms and request_timeout_ms",
                vec!["--config".into(), file.to_str().unwrap().into()],
                BTreeMap::new(),
            ),
        ] {
            let errors = load_http(&args, &env).unwrap_err().errors;
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert_eq!(errors[0].key, key);
            assert_eq!(
                errors[0].fix,
                format!("connection_timeout_ms was removed; set {replacement} instead.")
            );
        }
    }

    #[test]
    fn capacity_settings_accept_their_raised_bounds_only() {
        for (setting, maximum, value) in [
            (
                "max_connections",
                1_048_576,
                (|config| config.max_connections as u64) as fn(ConfigDraft) -> u64,
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
        let env = BTreeMap::new();
        let default = Config::load(&[], &env).unwrap();
        for (bound, expected) in [
            ("0.0.0.0:54321", "http://127.0.0.1:54321"),
            ("[::]:54321", "http://[::1]:54321"),
            ("127.0.0.2:54321", "http://127.0.0.2:54321"),
            ("[::1]:54321", "http://[::1]:54321"),
        ] {
            assert_eq!(
                default.effective_api_base_url(bound.parse().unwrap()),
                expected
            );
        }
        // Without a connection to take it from, only the TLS-only modes advertise HTTPS.
        for (scheme, expected) in [
            (Scheme::Auto, "http://127.0.0.1:54321"),
            (Scheme::Http, "http://127.0.0.1:54321"),
            (Scheme::Https, "https://127.0.0.1:54321"),
            (Scheme::Both, "https://127.0.0.1:54321"),
        ] {
            let config = Config::load(&["--scheme".into(), scheme.as_str().into()], &env).unwrap();
            assert_eq!(
                config.effective_api_base_url("127.0.0.1:54321".parse().unwrap()),
                expected,
                "{scheme:?}"
            );
        }
        for listen in ["127.0.0.1:0", "0.0.0.0:0"] {
            let args = [
                "--scheme=http".into(),
                "--listen".into(),
                listen.into(),
                "--xml-virtual-host-domains".into(),
                "0.1".into(),
            ];
            assert!(
                Config::load(&args, &env)
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
            let config = Config::load(&explicit, &env).unwrap();
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
            let errors = ConfigDraft::load(&[], &env).unwrap_err();
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
            let errors = ConfigDraft::load(args, env).unwrap_err().to_string();
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
        assert_eq!(
            ConfigDraft::load(&[], &env).unwrap().local_creator_email,
            None
        );
        // Only beside the listener it belongs to, which needs strict mode.
        let env = BTreeMap::from(
            [
                ("GCS_MIMIC_AUTH_MODE", "strict"),
                ("GCS_MIMIC_TRUSTED_LISTEN", "127.0.0.1:0"),
                ("GCS_MIMIC_TRUSTED_ALLOWED_HOSTS", ""),
            ]
            .map(|(key, value)| (key.to_owned(), value.to_owned())),
        );
        assert!(
            ConfigDraft::load(&[], &env)
                .unwrap()
                .trusted_allowed_hosts
                .is_empty()
        );
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = InvalidDnsName;
    assert_eq!(
        error.to_string(),
        "virtual-host domain must be a DNS name without scheme, port, wildcard or trailing dot"
    );
    assert!(std::error::Error::source(&error).is_none());
}
