#![deny(clippy::unwrap_used, clippy::expect_used)]

use crate::{
    bucket::bucket_location,
    config::{Scheme, Setting},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
};

mod options;
pub use options::{DurationError, Fix, PairError, UrlStyle, duration, option_pairs};

pub(crate) const MAX_SETTINGS: usize = 1024;
/// The extra three pairs reserve two command words and two overflow pairs; equality detects take() truncation.
pub const MAX_CLI_ARGS: usize = 2 * (MAX_SETTINGS + 3);
pub(crate) const MAX_KEY_BYTES: usize = 256;

pub(crate) fn ordered_members<'de, D, V>(
    deserializer: D,
    expecting: &'static str,
    check: impl FnMut(usize, &str) -> bool,
) -> Result<Vec<(String, V)>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: serde::Deserialize<'de>,
{
    struct Members<F, V> {
        expecting: &'static str,
        check: F,
        value: std::marker::PhantomData<V>,
    }
    impl<'de, F: FnMut(usize, &str) -> bool, V: serde::Deserialize<'de>> serde::de::Visitor<'de>
        for Members<F, V>
    {
        type Value = Vec<(String, V)>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.expecting)
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            mut self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut entries = Vec::new();
            while let Some(key) = map.next_key::<String>()? {
                if !(self.check)(entries.len(), &key) {
                    return Err(serde::de::Error::custom(
                        "configuration input limit exceeded",
                    ));
                }
                entries.push((key, map.next_value()?));
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_map(Members {
        expecting,
        check,
        value: std::marker::PhantomData,
    })
}

crate::config::string_enum! {
    no_default
    /// Server options that stand for a native setting under another name, or for part of one.
    /// `Port`, `PortHttp` and `Host` are the documented way to move a listener, because they leave
    /// the rest of its address alone. `Scheme` and `LogLevel` name native settings on `serve`;
    /// they are listed so that a helper command can say they belong to the server.
    #[derive(PartialOrd, Ord)]
    pub(crate) enum AliasFlag {
        Port => "port",
        PortHttp => "port-http",
        Backend => "backend",
        FilesystemRoot => "filesystem-root",
        PublicHost => "public-host",
        ExternalUrl => "external-url",
        Scheme => "scheme",
        Host => "host",
        Data => "data",
        CorsHeaders => "cors-headers",
        EventPubsubProjectId => "event.pubsub-project-id",
        EventPubsubTopic => "event.pubsub-topic",
        EventBucket => "event.bucket",
        EventObjectPrefix => "event.object-prefix",
        EventList => "event.list",
        Location => "location",
        CertLocation => "cert-location",
        PrivateKeyLocation => "private-key-location",
        LogLevel => "log-level",
    }
    "unknown flag"
}

impl AliasFlag {
    /// Whether `value` belongs to this flag alone, so a name it shares with a native setting must
    /// be translated rather than read as that setting.
    fn value_needs_translation(self, value: &str) -> bool {
        self == Self::Backend && value == "filesystem"
    }

    /// Whether `value` is one the native setting takes unchanged. An empty value counts as unset,
    /// and other values get the repair advice along with the native setting's own error.
    fn translates_exactly(self, value: &str) -> bool {
        value.is_empty()
            || match self {
                Self::Host => value == "localhost" || value.parse::<IpAddr>().is_ok(),
                Self::Port | Self::PortHttp => value.parse::<u16>().is_ok(),
                Self::ExternalUrl => safe_url(value),
                Self::Location => bucket_location(value).is_ok(),
                Self::Backend
                | Self::FilesystemRoot
                | Self::PublicHost
                | Self::Scheme
                | Self::Data
                | Self::CorsHeaders
                | Self::EventPubsubProjectId
                | Self::EventPubsubTopic
                | Self::EventBucket
                | Self::EventObjectPrefix
                | Self::EventList
                | Self::CertLocation
                | Self::PrivateKeyLocation
                | Self::LogLevel => true,
            }
    }
}

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
    /// A flag that stands for a native setting, as supplied.
    Alias(AliasFlag),
    /// The native setting an `Alias` input became; its key names that input.
    Translated,
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
    help_requested: bool,
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
            // Checked here so that a value such as `--seed-dir -h` is never mistaken for help.
            if command == CliCommand::Serve
                && matches!(token.as_str(), "-h" | "--h" | "-help" | "--help")
            {
                self.help_requested = true;
                index += 1;
                continue;
            }
            let (key, inline) = token
                .split_once('=')
                .map_or((token.as_str(), None), |(k, v)| (k, Some(v)));
            let hyphens = key.bytes().take_while(|byte| *byte == b'-').count();
            let name = key.trim_start_matches('-');
            let normalized = name.replace('-', "_");
            let native_name = command == CliCommand::Serve && normalized.parse::<Setting>().is_ok();
            let next = args.get(index + 1);
            let supplied = inline.or(next.map(String::as_str));
            // One and two hyphens mean the same, so the count never selects a meaning.
            let spelled = (1..=2).contains(&hyphens);
            let flag = name.parse::<AliasFlag>().ok().filter(|_| spelled);
            let translated = flag
                .zip(supplied)
                .is_some_and(|(f, v)| f.value_needs_translation(v));
            let native = spelled && native_name && !translated;
            let alias = flag.filter(|_| !native);
            let separate = inline.is_none()
                && next.is_some_and(|value| {
                    native
                        || !value.starts_with('-')
                        || value.parse::<i64>().is_ok()
                        || (hyphens == 2 && alias.is_none())
                });
            let value = inline.or_else(|| {
                if separate {
                    next.map(String::as_str)
                } else {
                    None
                }
            });
            let valid_syntax = (hyphens == 2 || native)
                && (separate || (inline.is_some() && command == CliCommand::Serve));
            // fake-gcs-server users expect a valueless switch here, so name the value they need.
            let example = if native_name && normalized == "cors_fallback" {
                "--cors-fallback allow-all"
            } else {
                "--scheme http"
            };
            let input = Input {
                source: DiagnosticSource::Cli,
                key: key.into(),
                name: normalized,
                value: value.unwrap_or_default().into(),
                kind: alias.map_or(InputKind::Native, InputKind::Alias),
                invalid: (alias.is_none() && !valid_syntax).then_some(InputError::Syntax),
            };
            let saved_key = if key.len() <= MAX_KEY_BYTES {
                Some(key.to_owned())
            } else {
                None
            };
            if !self.push(input) {
                break;
            }
            if alias.is_none()
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
        self.environment_os(env.iter().map(|(key, value)| (key.into(), value.into())));
    }

    pub(crate) fn environment_os(
        &mut self,
        env: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    ) {
        for (key, value) in env
            .into_iter()
            .filter(|(key, _)| {
                key.as_encoded_bytes().starts_with(b"GCS_MIMIC_") || key == "PUBSUB_EMULATOR_HOST"
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
            let name = key
                .strip_prefix("GCS_MIMIC_")
                .unwrap_or(&key)
                .to_ascii_lowercase();
            let (value, invalid) = match value.into_string() {
                Ok(value) => (value, None),
                Err(_) => (String::new(), Some(InputError::Encoding)),
            };
            if !self.push(Input {
                source: DiagnosticSource::Environment,
                key,
                name,
                value,
                kind: InputKind::Native,
                invalid,
            }) {
                break;
            }
        }
    }

    /// The alias flags on the command line, in the order given. Collected into a map, the last
    /// occurrence of a flag wins.
    fn alias_flags(&self) -> impl Iterator<Item = (AliasFlag, &Input)> {
        self.entries.iter().filter_map(|input| match input.kind {
            InputKind::Alias(flag) => Some((flag, input)),
            InputKind::Native | InputKind::Translated => None,
        })
    }

    /// Rejects alias flags given to helper commands, which have no server settings to map to.
    pub(crate) fn diagnose_aliases(&mut self) {
        let effective: BTreeMap<_, _> = self.alias_flags().collect();
        let values = advice_values(&effective);
        let errors: Vec<_> = self
            .alias_flags()
            .map(|(flag, input)| {
                Diagnostic::new(
                    input.source,
                    &input.key,
                    format!(
                        "Server options belong to gcs-mimic serve; remove this one from this command. For the server, {}.",
                        alias_advice(flag, &values)
                    ),
                )
            })
            .collect();
        self.diagnostics.errors.extend(errors);
    }

    /// Translates alias flags into native settings. A flag warns only when the translation
    /// leaves something the user needs to know. Incomplete event publishing still fails: a test
    /// that waits for notifications would otherwise start and time out without saying why.
    ///
    /// Runs after the config file is read, because a lone port or host keeps the rest of the
    /// address the file may have chosen.
    pub(crate) fn translate_aliases(&mut self) {
        let effective: BTreeMap<_, _> = self.alias_flags().collect();
        let values = advice_values(&effective);
        let base = TranslationBase::new(&self.entries);
        let mut aliases: BTreeMap<&str, Input> = BTreeMap::new();
        for (flag, input) in &effective {
            let Some((setting, value)) = translation(*flag, &effective, &base) else {
                continue;
            };
            // host and port share one listener, so the first of them names it in diagnostics.
            aliases.entry(setting.as_str()).or_insert_with(|| Input {
                source: input.source,
                key: input.key.clone(),
                name: setting.as_str().into(),
                value,
                kind: InputKind::Translated,
                invalid: None,
            });
        }
        let differences = self.alias_flags().filter_map(|(flag, input)| {
            let difference = alias_difference(flag, &values, &base)?;
            Some(Diagnostic::new(
                input.source,
                &input.key,
                format!("This option works differently here; {difference}."),
            ))
        });
        let overrides = aliases
            .values()
            .filter(|alias| {
                self.entries.iter().any(|native| {
                    native.kind == InputKind::Native
                        && native.source == alias.source
                        && native.name == alias.name
                        && native.value != alias.value
                })
            })
            .map(|alias| {
                Diagnostic::new(
                    alias.source,
                    &alias.key,
                    format!(
                        "This input overrides --{} from the same command line; remove one of them.",
                        alias.name.replace('_', "-")
                    ),
                )
            });
        let warnings: Vec<_> = differences.chain(overrides).collect();
        self.diagnostics.warnings.extend(warnings);
        // Appended after native inputs, so an alias wins within its own source.
        self.entries.extend(aliases.into_values());
    }
}

/// Values that advice may quote.
fn advice_values<'a>(effective: &BTreeMap<AliasFlag, &'a Input>) -> BTreeMap<AliasFlag, &'a str> {
    effective
        .iter()
        .map(|(flag, input)| (*flag, input.value.as_str()))
        .collect()
}

/// The value of the highest-precedence native setting `name`. An empty value counts as absent:
/// Compose's `${VAR:-}` produces one, and it names no address to keep.
fn native_value<'a>(entries: &'a [Input], name: &str) -> Option<&'a str> {
    [
        DiagnosticSource::Cli,
        DiagnosticSource::Environment,
        DiagnosticSource::Config,
    ]
    .into_iter()
    .find_map(|source| {
        entries.iter().find(|input| {
            input.source == source
                && input.kind == InputKind::Native
                && input.name == name
                && !input.value.is_empty()
        })
    })
    .map(|input| input.value.as_str())
}

/// Values fixed by settings other than the flag being translated. Keeping a listener's IP is what
/// makes `--port` safe: the image stays on 0.0.0.0 and a binary on a developer's machine stays on
/// loopback, where an unauthenticated emulator belongs.
struct TranslationBase {
    /// `None` when the configured address is malformed, which its own validation reports.
    listen: Option<SocketAddr>,
    http_ip: Option<IpAddr>,
    scheme: Option<Scheme>,
}

impl TranslationBase {
    fn new(entries: &[Input]) -> Self {
        let listen = native_value(entries, "listen").map_or_else(
            || Some(crate::config::Config::default().listen),
            |address| address.parse().ok(),
        );
        let http_ip = native_value(entries, "listen_http")
            .and_then(|address| address.parse::<SocketAddr>().ok())
            .or(listen)
            .map(|address| address.ip());
        Self {
            listen,
            http_ip,
            scheme: native_value(entries, "scheme").and_then(|scheme| scheme.parse().ok()),
        }
    }

    fn public_scheme(&self) -> &'static str {
        if self.scheme == Some(Scheme::Http) {
            "http"
        } else {
            "https"
        }
    }
}

/// An IP as it appears before `:PORT`.
fn bracketed_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    }
}

/// A supplied bind host as it appears before `:PORT`. A value that is not an address keeps its
/// spelling, so the listener's validation reports it.
fn host_text(host: &str) -> String {
    if host == "localhost" {
        return "127.0.0.1".to_owned();
    }
    host.parse().map_or_else(|_| host.to_owned(), bracketed_ip)
}

/// The native setting and value that `flag` maps to, if any.
fn translation(
    flag: AliasFlag,
    flags: &BTreeMap<AliasFlag, &Input>,
    base: &TranslationBase,
) -> Option<(Setting, String)> {
    let value = |key| flags.get(&key).map(|input| input.value.as_str());
    let set = |key| value(key).filter(|v: &&str| !v.is_empty());
    let text = value(flag)?.to_owned();
    Some(match flag {
        AliasFlag::Host | AliasFlag::Port => {
            let host = match set(AliasFlag::Host) {
                Some(host) => host_text(host),
                None => bracketed_ip(base.listen?.ip()),
            };
            let port = match set(AliasFlag::Port) {
                Some(port) => port.to_owned(),
                None => base.listen?.port().to_string(),
            };
            (Setting::Listen, format!("{host}:{port}"))
        }
        AliasFlag::PortHttp => {
            let host = match set(AliasFlag::Host) {
                Some(host) => host_text(host),
                None => bracketed_ip(base.http_ip?),
            };
            (Setting::ListenHttp, format!("{host}:{text}"))
        }
        AliasFlag::ExternalUrl => (Setting::ApiBaseUrl, text),
        AliasFlag::PublicHost => (
            Setting::PublicBaseUrl,
            format!("{}://{text}", base.public_scheme()),
        ),
        AliasFlag::Data => (Setting::SeedDir, text),
        // Only `filesystem` arrives here; the native backends are native inputs.
        AliasFlag::Backend => (Setting::Backend, "disk".into()),
        AliasFlag::CertLocation => (Setting::TlsCert, text),
        AliasFlag::PrivateKeyLocation => (Setting::TlsKey, text),
        AliasFlag::CorsHeaders => (Setting::CorsFallback, "allow-all".into()),
        AliasFlag::Location => (Setting::DefaultLocation, text),
        AliasFlag::EventPubsubProjectId => (Setting::EventPubsubProjectId, text),
        AliasFlag::EventPubsubTopic => (Setting::EventPubsubTopic, text),
        AliasFlag::EventBucket => (Setting::EventBucket, text),
        AliasFlag::EventObjectPrefix => (Setting::EventObjectPrefix, text),
        AliasFlag::EventList => (Setting::EventList, text),
        // A reused fake-gcs-server volume is owned by root, so the unprivileged image could not
        // write there and would fail to start.
        AliasFlag::FilesystemRoot => return None,
        // These name native settings on `serve`, so they arrive as native inputs.
        AliasFlag::Scheme | AliasFlag::LogLevel => return None,
    })
}

fn event_advice(flag: AliasFlag) -> String {
    format!(
        "use GCS_MIMIC_{} and an explicit PUBSUB_EMULATOR_HOST; only local emulator delivery is enabled",
        flag.as_str().replace(['.', '-'], "_").to_ascii_uppercase()
    )
}

/// What the user needs to know about `flag`, or `None` when its translation is exact.
fn alias_difference(
    flag: AliasFlag,
    values: &BTreeMap<AliasFlag, &str>,
    base: &TranslationBase,
) -> Option<String> {
    let value = values.get(&flag).copied().unwrap_or_default();
    Some(match flag {
        AliasFlag::FilesystemRoot => "it is ignored; data is kept in GCS_MIMIC_DATA_DIR (/var/lib/gcs-mimic in the image), so mount the data volume there; existing fake-gcs-server objects are not loaded, so convert a stopped snapshot with gcs-mimic import fake-gcs-server; remove this setting".into(),
        AliasFlag::PublicHost => {
            let url = format!("{}://{value}", base.public_scheme());
            let example = crate::url::normalize_public_base_url(&url)
                .unwrap_or_else(|_| "https://storage.example.invalid:4443".into());
            format!("use GCS_MIMIC_PUBLIC_BASE_URL={example}; path-style URLs work with any Host")
        }
        AliasFlag::CorsHeaders => "remove this setting: permissive mode already allows every origin, strict mode follows each bucket's cors rules, and JSON requests already allow every valid header".into(),
        AliasFlag::EventPubsubProjectId
        | AliasFlag::EventPubsubTopic
        | AliasFlag::EventBucket
        | AliasFlag::EventObjectPrefix
        | AliasFlag::EventList => event_advice(flag),
        AliasFlag::Port
        | AliasFlag::PortHttp
        | AliasFlag::Backend
        | AliasFlag::ExternalUrl
        | AliasFlag::Scheme
        | AliasFlag::Host
        | AliasFlag::Data
        | AliasFlag::Location
        | AliasFlag::CertLocation
        | AliasFlag::PrivateKeyLocation
        | AliasFlag::LogLevel => {
            if flag.translates_exactly(value) {
                return None;
            }
            alias_advice(flag, values)
        }
    })
}

fn component<'a>(
    values: &BTreeMap<AliasFlag, &'a str>,
    key: AliasFlag,
    default: &'a str,
) -> &'a str {
    values
        .get(&key)
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
                    && (a.host().trim_matches(['[', ']']).parse::<IpAddr>().is_ok()
                        || crate::config::validate_dns_name(a.host()).is_ok())
            })
            && uri.query().is_none()
            && !value.contains('#')
            && !value.chars().any(char::is_control)
    })
}

fn alias_advice(flag: AliasFlag, values: &BTreeMap<AliasFlag, &str>) -> String {
    let host = component(values, AliasFlag::Host, "0.0.0.0")
        .parse()
        .map_or_else(|_| "<IP_ADDRESS>".into(), bracketed_ip);
    let port = component(values, AliasFlag::Port, "4443")
        .parse::<u16>()
        .map_or_else(|_| "<PORT_0_TO_65535>".into(), |p| p.to_string());
    let scheme = component(values, AliasFlag::Scheme, "https");
    let value = values.get(&flag).copied().unwrap_or_default();
    match flag {
        AliasFlag::Host | AliasFlag::Port => format!("combine HOST and PORT as GCS_MIMIC_LISTEN={host}:{port}; use a valid IP address and port 0..65535"),
        AliasFlag::Scheme if matches!(scheme, "http" | "https" | "both") => format!("use GCS_MIMIC_SCHEME={scheme} or --scheme {scheme}"),
        AliasFlag::Scheme => "use GCS_MIMIC_SCHEME=http, https or both; both serves HTTPS on listen and HTTP on listen_http".into(),
        AliasFlag::PortHttp => {
            let port = component(values, AliasFlag::PortHttp, "8000").parse::<u16>()
                .map_or_else(|_| "<PORT_0_TO_65535>".into(), |p| p.to_string());
            format!("use GCS_MIMIC_SCHEME=both with GCS_MIMIC_LISTEN_HTTP={host}:{port}; the primary GCS_MIMIC_LISTEN serves HTTPS")
        }
        AliasFlag::ExternalUrl if safe_url(value) => format!("use GCS_MIMIC_API_BASE_URL={value}; preserve the client-reachable proxy prefix"),
        AliasFlag::ExternalUrl => "use GCS_MIMIC_API_BASE_URL=http://gcs:4443/proxy without credentials, query or fragment; unsafe URL values are redacted".into(),
        AliasFlag::PublicHost => {
            let scheme = if matches!(scheme, "https" | "both") { "https" } else if scheme == "http" { "http" } else { "<SCHEME>" };
            let url = format!("{scheme}://{value}");
            let example = crate::url::normalize_public_base_url(&url)
                .unwrap_or_else(|_| "https://storage.example.invalid:4443".into());
            format!("Use GCS_MIMIC_PUBLIC_BASE_URL={example} including the scheme; legacy Host matching also requires GCS_MIMIC_COMPAT_PROFILE=legacy-emulator. Native path-style accepts any Host. For SDK XML reads in legacy mode the public authority must match the API endpoint authority; unequal authorities fall back to a hostname bucket")
        }
        AliasFlag::Data => "mount fixtures at /data in the image or use GCS_MIMIC_SEED_DIR=fixtures; seed replays on every startup, overwriting changed objects at fixture names".into(),
        AliasFlag::FilesystemRoot => "use GCS_MIMIC_DATA_DIR=data only for native storage; the source format differs. Retain the old root for the separately delivered Import conversion; never mount it as native data".into(),
        AliasFlag::Backend => "backend must be filesystem or memory; use --backend disk or --backend memory".into(),
        AliasFlag::CertLocation | AliasFlag::PrivateKeyLocation => "supply both GCS_MIMIC_TLS_CERT=certs/server.pem and GCS_MIMIC_TLS_KEY=certs/server-key.pem; one-sided overrides fail".into(),
        // Permissive mode already falls back to allow-all, and strict mode should keep Cloud
        // Storage's bucket rules, so neither needs a replacement setting.
        AliasFlag::CorsHeaders => "remove this setting: permissive mode already allows every origin, strict mode follows each bucket's cors rules, and JSON preflight already allows every valid requested header".into(),
        AliasFlag::Location => match bucket_location(value) {
            Ok((location, _)) => format!("use GCS_MIMIC_COMPAT_PROFILE=native with GCS_MIMIC_DEFAULT_LOCATION={location} or --default-location {location}; applies only to new buckets without a nonempty location; legacy-emulator global location defaults remain unsupported"),
            Err(_) => "remove this setting; native defaults use GCS_MIMIC_COMPAT_PROFILE=native with a supported GCS_MIMIC_DEFAULT_LOCATION, for example US or ASIA-NORTHEAST1; legacy-emulator global location defaults remain unsupported".into(),
        },
        AliasFlag::LogLevel if matches!(value, "debug" | "info" | "warn" | "warning" | "error") => format!("use GCS_MIMIC_LOG_LEVEL={} or --log-level {}; this controls runtime stderr events, not validation diagnostics or stdout lifecycle events", if value == "warning" { "warn" } else { value }, if value == "warning" { "warn" } else { value }),
        AliasFlag::LogLevel => "remove this setting; use GCS_MIMIC_LOG_LEVEL=info or --log-level info; supported levels are debug, info, warn or error".into(),
        AliasFlag::EventPubsubProjectId
        | AliasFlag::EventPubsubTopic
        | AliasFlag::EventBucket
        | AliasFlag::EventObjectPrefix
        | AliasFlag::EventList => event_advice(flag),
    }
}

/// The options that start the server, if `args` do: those after the `serve` word, or all of them
/// when the first is an option. `docker run IMAGE --scheme http` and a Compose `command` replace
/// the image's `serve`, and every other command starts with a word, so the option spelling never
/// has to decide which command runs.
pub fn server_arguments(args: &[String]) -> Option<&[String]> {
    match args {
        [command, options @ ..] if command == "serve" => Some(options),
        [first, ..] if first.starts_with('-') => Some(args),
        _ => None,
    }
}

/// Whether `args` ask for the version and nothing else.
pub fn requests_version(args: &[String]) -> bool {
    matches!(args, [flag] if matches!(flag.as_str(), "-version" | "--version"))
}

/// Whether the server options ask for the usage text.
pub fn requests_help(args: &[String]) -> bool {
    let mut inputs = Inputs::default();
    inputs.cli(args, CliCommand::Serve);
    inputs.help_requested
}

/// Inspect option-name positions without reading server configuration or environment.
pub fn cli_diagnostics(args: &[String]) -> Diagnostics {
    if matches!(args, [command, kind, flag] if command == "url" && matches!(kind.as_str(), "public" | "sign") && flag == "--help")
        || matches!(args, [command, flag] if command == "test-signer" && flag == "--help")
    {
        return Diagnostics::default();
    }
    let offset = match args.first().map(String::as_str) {
        Some("healthcheck") => 1,
        Some("url" | "tls" | "test-signer") => 2.min(args.len()),
        _ => 0,
    };
    let mut inputs = Inputs::default();
    inputs.cli(args.get(offset..).unwrap_or_default(), CliCommand::Helper);
    inputs.diagnose_aliases();
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
            if crate::source_tree::paths_overlap_canonical(&seed, &other) {
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
    use super::*;
    use crate::diagnostics::test_support::has;
    use proptest::prelude::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }

    #[test]
    fn fake_gcs_server_variables_are_not_read() {
        let env = BTreeMap::from([
            ("FAKE_GCS_SCHEME".into(), "http".into()),
            ("FAKE_GCS_PORT".into(), "not-a-port".into()),
            ("FAKE_GCS_UNRELATED".into(), "value".into()),
        ]);
        let report = crate::config::Config::preflight(&[], &env);
        assert_eq!(emitted(&report.diagnostics), "");
        assert!(matches!(report.config.scheme, crate::config::Scheme::Https));
        assert_eq!(
            report.config.listen,
            crate::config::Config::default().listen
        );
    }

    /// Every error and warning as the emitted JSON lines.
    fn emitted(diagnostics: &Diagnostics) -> String {
        let mut bytes = Vec::new();
        diagnostics.write(&mut bytes).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    /// What a helper command says about alias flags, which quotes the native settings they
    /// would become on the server.
    fn helper_advice(args: &[String]) -> String {
        let mut inputs = Inputs::default();
        inputs.cli(args, CliCommand::Helper);
        inputs.diagnose_aliases();
        emitted(&inputs.diagnostics)
    }

    /// Flags whose accepted values translate exactly and so report nothing.
    const EXACT: [&str; 11] = [
        "port",
        "port-http",
        "backend",
        "scheme",
        "host",
        "data",
        "location",
        "cert-location",
        "private-key-location",
        "external-url",
        "log-level",
    ];

    /// Flags whose translation leaves something to report.
    const DIFFERENT: [&str; 8] = [
        "filesystem-root",
        "public-host",
        "cors-headers",
        "event.pubsub-project-id",
        "event.pubsub-topic",
        "event.bucket",
        "event.object-prefix",
        "event.list",
    ];

    #[test]
    fn alias_inventory_and_all_flag_forms_are_diagnosed_even_when_empty() {
        for flag in EXACT.iter().chain(&DIFFERENT) {
            assert!(flag.parse::<AliasFlag>().is_ok(), "{flag}");
            for value in ["", "secret-do-not-print\n"] {
                // `data` accepts any path.
                let reported = !EXACT.contains(flag) || (!value.is_empty() && *flag != "data");
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
                        if matches!(*flag, "scheme" | "log-level" | "backend") {
                            assert_eq!(report.diagnostics.errors[0].key, key);
                            assert!(report.diagnostics.errors[0].fix.contains(
                                if *flag == "scheme" {
                                    "scheme must"
                                } else if *flag == "backend" {
                                    "backend must"
                                } else {
                                    "log_level must"
                                }
                            ));
                        } else if reported {
                            assert!(text.contains(&key), "{text}");
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
            "debug",
            "-cors-headers",
            "X-Custom",
            "-location",
            "asia-northeast1",
        ]);
        assert_eq!(server_arguments(&args), Some(args.as_slice()));
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
            Some(crate::config::CorsFallback::AllowAll)
        ));
        assert_eq!(config.default_location, "ASIA-NORTHEAST1");
        let warned: Vec<_> = report
            .diagnostics
            .warnings
            .iter()
            .map(|d| d.key.as_str())
            .collect();
        for key in ["-public-host", "-filesystem-root", "-cors-headers"] {
            assert!(warned.contains(&key), "{key}: {warned:?}");
        }
        for key in [
            "-scheme",
            "-port",
            "-external-url",
            "-backend",
            "-data",
            "-log-level",
            "-location",
        ] {
            assert!(!warned.contains(&key), "{key}: {warned:?}");
        }
        let text = emitted(&report.diagnostics);
        assert!(text.contains("gcs-mimic import fake-gcs-server"), "{text}");
    }

    #[test]
    fn aliases_follow_source_precedence_and_keep_rejecting_events() {
        let image = BTreeMap::from([("GCS_MIMIC_LISTEN".into(), "0.0.0.0:4443".into())]);
        // A flag outranks the environment without a conflict to report.
        let report = crate::config::Config::preflight(&strings(&["-port", "8080"]), &image);
        assert_eq!(emitted(&report.diagnostics), "");
        assert_eq!(report.config.listen.to_string(), "0.0.0.0:8080");
        let report = crate::config::Config::preflight(
            &strings(&["-host", "localhost", "-port", "8080"]),
            &image,
        );
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
        let report = crate::config::Config::preflight(
            &strings(&["-scheme", "http", "-event.pubsub-topic", "uploads"]),
            &BTreeMap::new(),
        );
        assert!(report.diagnostics.is_err());
        assert_eq!(report.diagnostics.errors[0].key, "-event.pubsub-topic");
        assert!(emitted(&report.diagnostics).contains("Event publishing requires"));
        assert_eq!(report.diagnostics.errors.len(), 1, "{}", report.diagnostics);
    }

    #[test]
    fn option_first_command_lines_start_the_server() {
        for args in [
            &["--scheme", "http"][..],
            &["--scheme=http", "--port", "9000"],
            &["--port", "9000", "--scheme", "http"],
            &["--backend", "memory"],
            &["--log-level", "debug"],
            &["--listen", "0.0.0.0:9000"],
            &["-scheme", "http"],
            &["--port=8080"],
            &["--help"],
            &["-x", "http"],
        ] {
            let args = strings(args);
            assert_eq!(server_arguments(&args), Some(args.as_slice()), "{args:?}");
        }
        let args = strings(&["serve", "--scheme", "http"]);
        assert_eq!(server_arguments(&args), Some(&args[1..]));
        assert!(requests_version(&strings(&["--version"])));
        assert!(requests_version(&strings(&["-version"])));
        assert!(!requests_version(&strings(&["--version", "extra"])));
        for args in [
            &[][..],
            &[
                "healthcheck",
                "--url",
                "http://127.0.0.1:1/_internal/health",
            ],
            &["url", "public", "--bucket", "-x"],
            &["tls", "ca"],
            &["scheme"],
        ] {
            assert_eq!(server_arguments(&strings(args)), None, "{args:?}");
        }
    }

    #[test]
    fn option_order_and_value_forms_do_not_change_the_configuration() {
        for args in [
            &["--scheme", "http", "--port", "9000"][..],
            &["--port", "9000", "--scheme", "http"],
            &["--scheme=http", "--port=9000"],
            &["--port=9000", "--scheme=http"],
            &["-scheme", "http", "-port", "9000"],
            &["-scheme=http", "-port=9000"],
            &["--port=9000", "-scheme", "http"],
        ] {
            let report = crate::config::Config::preflight(&strings(args), &BTreeMap::new());
            assert_eq!(emitted(&report.diagnostics), "", "{args:?}");
            assert!(matches!(report.config.scheme, crate::config::Scheme::Http));
            assert_eq!(report.config.listen.to_string(), "127.0.0.1:9000");
        }
        // Only the first equals sign separates the name, so a value may contain others.
        let report =
            crate::config::Config::preflight(&strings(&["--seed-dir=a=b"]), &BTreeMap::new());
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        assert_eq!(
            report.config.seed_dir.as_deref(),
            Some(std::path::Path::new("a=b"))
        );
        let report = crate::config::Config::preflight(&strings(&["--scheme"]), &BTreeMap::new());
        has(&report.diagnostics, "--scheme", "--name VALUE pairs");
    }

    #[test]
    fn one_and_two_hyphens_mean_the_same_for_every_server_option() {
        for (name, value) in [
            ("scheme", "http"),
            ("backend", "memory"),
            ("backend", "filesystem"),
            ("log-level", "debug"),
            ("listen", "127.0.0.2:9000"),
            ("seed-dir", "fixtures"),
            ("data-dir", "-leading-hyphen"),
            ("public-base-url", "http://gcs:4443"),
            ("max-connections", "32"),
            ("port", "9000"),
            ("host", "127.0.0.2"),
            ("data", "fixtures"),
        ] {
            let forms = [
                vec![format!("-{name}"), value.to_owned()],
                vec![format!("--{name}"), value.to_owned()],
                vec![format!("-{name}={value}")],
                vec![format!("--{name}={value}")],
            ];
            let configurations = forms.map(|args| {
                let report = crate::config::Config::preflight(&args, &BTreeMap::new());
                assert_eq!(emitted(&report.diagnostics), "", "{args:?}");
                format!("{:?}", report.config)
            });
            assert!(
                configurations.iter().all(|c| *c == configurations[0]),
                "{name}: {configurations:?}"
            );
        }
    }

    #[test]
    fn a_public_host_follows_the_effective_scheme_however_it_was_spelled() {
        for (args, env) in [
            (&["-scheme", "http", "-public-host", "gcs:4443"][..], None),
            (&["--scheme", "http", "-public-host", "gcs:4443"], None),
            (&["--scheme=http", "--public-host=gcs:4443"], None),
            (
                &["-public-host", "gcs:4443"],
                Some(("GCS_MIMIC_SCHEME", "http")),
            ),
        ] {
            let env: BTreeMap<String, String> = env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            let report = crate::config::Config::preflight(&strings(args), &env);
            assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
            let origin = report.config.public_base_url.unwrap();
            assert!(
                format!("{origin:?}").contains("http://gcs:4443"),
                "{args:?}: {origin:?}"
            );
        }
    }

    #[test]
    fn help_is_recognized_only_at_option_positions() {
        for args in [
            &["-h"][..],
            &["--h"],
            &["-help"],
            &["--help"],
            &["--scheme", "http", "-h"],
            &["-port", "9000", "--help"],
        ] {
            assert!(requests_help(&strings(args)), "{args:?}");
        }
        for args in [
            &[][..],
            &["--scheme", "http"],
            &["--seed-dir", "-h"],
            &["--help=1"],
        ] {
            assert!(!requests_help(&strings(args)), "{args:?}");
        }
    }

    #[test]
    fn a_lone_port_or_host_keeps_the_rest_of_the_listener_address() {
        let none = BTreeMap::new();
        let image = BTreeMap::from([("GCS_MIMIC_LISTEN".into(), "0.0.0.0:4443".into())]);
        let listen = |args: &[&str], env: &BTreeMap<String, String>| {
            let report = crate::config::Config::preflight(&strings(args), env);
            assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
            report.config.listen.to_string()
        };
        // The emulator accepts unauthenticated requests, so a port alone must not expose it.
        assert_eq!(listen(&["--port", "9000"], &none), "127.0.0.1:9000");
        assert_eq!(listen(&["-port", "9000"], &none), "127.0.0.1:9000");
        assert_eq!(listen(&["--port", "9000"], &image), "0.0.0.0:9000");
        assert_eq!(listen(&["--host", "0.0.0.0"], &none), "0.0.0.0:4443");
        assert_eq!(listen(&["--host", "::1"], &none), "[::1]:4443");
        assert_eq!(
            listen(&["--host", "localhost", "--port", "9000"], &image),
            "127.0.0.1:9000"
        );

        let directory = crate::test_support::TestDir::new();
        let file = directory.0.join("config.json");
        std::fs::write(&file, r#"{"listen":"127.0.0.4:4000"}"#).unwrap();
        let file = file.to_str().unwrap();
        assert_eq!(
            listen(&["--config", file, "--port", "9000"], &none),
            "127.0.0.4:9000"
        );

        let args = strings(&["--listen", "127.0.0.3:4000", "--port", "9000"]);
        let report = crate::config::Config::preflight(&args, &none);
        assert_eq!(report.config.listen.to_string(), "127.0.0.3:9000");
        assert!(emitted(&report.diagnostics).contains("overrides --listen"));

        let http = |env: &BTreeMap<String, String>| {
            let args = strings(&["--scheme", "both", "--port-http", "9001"]);
            let report = crate::config::Config::preflight(&args, env);
            assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
            report.config.listen_http.unwrap().to_string()
        };
        assert_eq!(http(&none), "127.0.0.1:9001");
        assert_eq!(http(&image), "0.0.0.0:9001");
        let preset = BTreeMap::from([("GCS_MIMIC_LISTEN_HTTP".into(), "127.0.0.5:8000".into())]);
        assert_eq!(http(&preset), "127.0.0.5:9001");

        // A malformed address is reported under its own name, not replaced by a default.
        let broken = BTreeMap::from([("GCS_MIMIC_LISTEN".into(), "garbage".into())]);
        let report = crate::config::Config::preflight(&strings(&["--port", "9000"]), &broken);
        has(&report.diagnostics, "GCS_MIMIC_LISTEN", "listen must");
        // A complete address on the command line needs nothing from it.
        let args = strings(&["--host", "127.0.0.2", "--port", "9000"]);
        let report = crate::config::Config::preflight(&args, &broken);
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        assert_eq!(report.config.listen.to_string(), "127.0.0.2:9000");
    }

    #[test]
    fn a_port_http_alias_is_ignored_outside_both_but_a_native_listen_http_is_rejected() {
        for (args, key) in [
            (&["-scheme", "http", "-port-http", "8001"][..], "-port-http"),
            (&["--port-http", "8001"], "--port-http"),
        ] {
            let report = crate::config::Config::preflight(&strings(args), &BTreeMap::new());
            assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
            assert_eq!(report.config.listen_http, None);
            assert!(
                report
                    .diagnostics
                    .warnings
                    .iter()
                    .any(|d| d.key == key && d.fix.contains("ignored")),
                "{}",
                report.diagnostics
            );
        }
        let args = strings(&["--scheme", "http", "--listen-http", "127.0.0.1:8001"]);
        let report = crate::config::Config::preflight(&args, &BTreeMap::new());
        has(
            &report.diagnostics,
            "--listen-http",
            "listen_http requires scheme=both",
        );
    }

    #[test]
    fn only_inputs_that_behave_differently_warn() {
        for args in [
            &["-scheme", "http"][..],
            &["-port", "9000"],
            &["-host", "127.0.0.2"],
            &["-scheme", "both", "-port-http", "8001"],
            &["-backend", "filesystem"],
            &["-backend", "memory"],
            &["-data", "fixtures"],
            &["-log-level", "warning"],
            &["-location", "EU"],
            &["-external-url", "http://gcs:4443"],
        ] {
            let report = crate::config::Config::preflight(&strings(args), &BTreeMap::new());
            assert_eq!(emitted(&report.diagnostics), "", "{args:?}");
        }

        for (args, expected) in [
            (
                &["-filesystem-root", "/storage"][..],
                "gcs-mimic import fake-gcs-server",
            ),
            (&["-public-host", "gcs:4443"], "GCS_MIMIC_PUBLIC_BASE_URL"),
            (&["-cors-headers", "X-Custom"], "remove this setting"),
        ] {
            let report = crate::config::Config::preflight(&strings(args), &BTreeMap::new());
            assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
            assert!(
                report
                    .diagnostics
                    .warnings
                    .iter()
                    .any(|d| d.key == args[0] && d.fix.contains(expected)),
                "{}",
                report.diagnostics
            );
        }
    }

    #[test]
    fn helper_commands_point_server_options_at_serve() {
        for (args, expected) in [
            (["healthcheck", "--scheme", "http"], "--scheme http"),
            (["healthcheck", "-backend", "memory"], "--backend memory"),
            (
                ["healthcheck", "-event.bucket", "uploads"],
                "GCS_MIMIC_EVENT_BUCKET",
            ),
        ] {
            let text = cli_diagnostics(&strings(&args)).to_string();
            assert!(text.contains("belong to gcs-mimic serve"), "{text}");
            assert!(text.contains(expected), "{text}");
            assert!(!text.contains("are unsupported"), "{text}");
        }
    }

    #[test]
    fn alias_advice_uses_values_precedence_defaults_and_redacts_unsafe_urls() {
        let args = strings(&[
            "-scheme=http",
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
        let fixes = helper_advice(&args);
        for expected in [
            "GCS_MIMIC_LISTEN=[::1]:9001",
            "GCS_MIMIC_API_BASE_URL=http://gcs:8080/proxy",
            "GCS_MIMIC_PUBLIC_BASE_URL=http://storage.invalid:9000",
            "GCS_MIMIC_LOG_LEVEL=warn",
            "GCS_MIMIC_DEFAULT_LOCATION=EU",
        ] {
            assert!(fixes.contains(expected), "{expected}: {fixes}");
        }
        assert!(helper_advice(&strings(&["-port", "8080"])).contains("0.0.0.0:8080"));
        let report = crate::config::Config::preflight(
            &strings(&["-scheme", "http", "-public-host", "Storage.Invalid:80"]),
            &BTreeMap::new(),
        );
        let advice = emitted(&report.diagnostics);
        assert!(advice.contains("GCS_MIMIC_PUBLIC_BASE_URL=http://storage.invalid;"));
        assert!(!advice.contains("Storage.Invalid:80"));
        // Helper commands have no server settings, so alias flags still fail there.
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
            "permissive mode already allows every origin, strict mode follows each bucket's cors rules",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("secret"));
        assert!(
            helper_advice(&strings(&["-location", "US-CENTRAL1"]))
                .contains("GCS_MIMIC_DEFAULT_LOCATION=US-CENTRAL1")
        );
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
        assert!(cors.starts_with("This option works differently here;"));
        // The alias still selects allow-all, but the advice is to remove it, not to restate it.
        assert!(cors.contains("remove this setting"), "{cors}");
        assert!(!cors.contains("GCS_MIMIC_CORS_FALLBACK"), "{cors}");
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
        // Helper commands take no server options, so each one is named.
        let report = cli_diagnostics(&strings(&[
            "healthcheck",
            "-port=8080",
            "-host",
            "127.0.0.2",
            "--data=fixtures",
            "-x",
            "x",
        ]));
        for key in ["-port", "-host", "--data", "-x"] {
            assert!(report.errors.iter().any(|d| d.key == key));
        }
        // On the server the same forms do not shift the tokens after them.
        let report = crate::config::Config::preflight(
            &strings(&["-port=8080", "-host", "127.0.0.2", "--data=fixtures"]),
            &BTreeMap::new(),
        );
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        assert_eq!(report.config.listen.to_string(), "127.0.0.2:8080");
        assert_eq!(
            report.config.seed_dir.as_deref(),
            Some(std::path::Path::new("fixtures"))
        );
        let report =
            crate::config::Config::preflight(&strings(&["--scheme", "http"]), &BTreeMap::new());
        assert!(!report.diagnostics.is_err());
        let report = crate::config::Config::preflight(
            &strings(&["--scheme=http", "-port", "9000", "-host=::1"]),
            &BTreeMap::new(),
        );
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        assert!(matches!(report.config.scheme, crate::config::Scheme::Http));
        assert_eq!(report.config.listen.to_string(), "[::1]:9000");
    }

    #[test]
    fn dual_listener_alias_advice_names_effective_native_settings() {
        let args = strings(&["-scheme", "both", "-host", "::1", "-port-http", "9000"]);
        let report = crate::config::Config::preflight(&args, &BTreeMap::new());
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        assert_eq!(
            report.config.listen_http.map(|a| a.to_string()).as_deref(),
            Some("[::1]:9000")
        );
        let advice = helper_advice(&args);
        assert!(advice.contains("GCS_MIMIC_SCHEME=both"));
        assert!(advice.contains("GCS_MIMIC_LISTEN_HTTP=[::1]:9000"));
        assert!(!advice.contains("future"));
        assert!(!advice.contains("second HTTP listener is unsupported"));
    }

    #[test]
    fn option_prefixes_accept_one_or_two_hyphens_and_nothing_else() {
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
            has(&report.diagnostics, flag, "--name VALUE pairs");
            assert!(
                !report
                    .diagnostics
                    .to_string()
                    .contains("This option works differently")
            );
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
        let directory = crate::test_support::TestDir::new();
        let file = directory.0.join("config.json");
        std::fs::write(&file, "{}").unwrap();
        let report = crate::config::Config::preflight_os(
            &["--config".into(), file.to_str().unwrap().into()],
            vec![(
                OsString::from("GCS_MIMIC_CONFIG"),
                OsString::from_vec(vec![0xff]),
            )],
        );
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
    }

    #[test]
    fn supplied_occurrences_and_original_utf8_keys_are_bounded() {
        for count in [1024, 1025] {
            let args: Vec<_> = (0..count).map(|_| "-filesystem-root=/x".into()).collect();
            let report = crate::config::Config::preflight(&args, &BTreeMap::new());
            assert_eq!(
                report
                    .diagnostics
                    .warnings
                    .iter()
                    .filter(|d| d.key == "-filesystem-root")
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
        let directory = crate::test_support::TestDir::new();
        let root = directory.0.clone();
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
    }

    #[test]
    fn alias_location_advice_uses_effective_native_setting() {
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
            let args = strings(&["-location", value]);
            let report = crate::config::Config::preflight(&args, &BTreeMap::new());
            assert_eq!(report.diagnostics.is_err(), expected.is_none());
            let advice = if let Some(expected) = expected {
                assert_eq!(report.config.default_location, expected);
                assert_eq!(emitted(&report.diagnostics), "", "{value}");
                let advice = helper_advice(&args);
                assert!(advice.contains(&format!("GCS_MIMIC_DEFAULT_LOCATION={expected}")));
                assert!(advice.contains(&format!("--default-location {expected}")));
                advice
            } else {
                let advice = emitted(&report.diagnostics);
                assert_eq!(report.diagnostics.errors[0].key, "-location");
                assert!(advice.contains("ASIA-NORTHEAST1"));
                assert!(!advice.contains(value));
                advice
            };
            assert!(advice.contains("GCS_MIMIC_COMPAT_PROFILE=native"));
            assert!(!advice.contains("future"));
            assert!(!advice.contains("secret"));
        }
    }

    proptest! {
        #[test]
        fn arbitrary_alias_and_native_inputs_never_panic(args in prop::collection::vec(any::<String>(), 0..80), env in prop::collection::btree_map("(FAKE_GCS_|GCS_MIMIC_)[a-zA-Z_-]{0,40}", any::<String>(), 0..40)) {
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
