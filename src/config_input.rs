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

mod environment;
mod options;
pub use environment::RenamedEnvironmentVariable;
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
    /// Native CLI options, including the independently configurable parts of a listener.
    #[derive(PartialOrd, Ord)]
    pub(crate) enum NativeFlag {
        Port => "port",
        PortHttp => "port-http",
        Scheme => "scheme",
        Host => "host",
        LogLevel => "log-level",
    }
    "unknown native flag"
}

crate::config::string_enum! {
    no_default
    /// Source flags whose translations preserve requests that worked with the source.
    #[derive(PartialOrd, Ord)]
    pub(crate) enum AliasFlag {
        Backend => "backend",
        PublicHost => "public-host",
        Data => "data",
        EventPubsubProjectId => "event.pubsub-project-id",
        EventPubsubTopic => "event.pubsub-topic",
        EventBucket => "event.bucket",
        EventObjectPrefix => "event.object-prefix",
        EventList => "event.list",
        CertLocation => "cert-location",
        PrivateKeyLocation => "private-key-location",
    }
    "unknown flag"
}

crate::config::string_enum! {
    no_default
    /// These source settings cannot be given their original semantics.
    #[derive(PartialOrd, Ord)]
    pub(crate) enum RefusedFlag {
        FilesystemRoot => "filesystem-root",
        ExternalUrl => "external-url",
        CorsHeaders => "cors-headers",
        Location => "location",
    }
    "unknown refused flag"
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ServerFlag {
    Native(NativeFlag),
    Alias(AliasFlag),
    Refused(RefusedFlag),
}

impl ServerFlag {
    fn as_str(self) -> &'static str {
        match self {
            Self::Native(flag) => flag.as_str(),
            Self::Alias(flag) => flag.as_str(),
            Self::Refused(flag) => flag.as_str(),
        }
    }
}

impl std::str::FromStr for ServerFlag {
    type Err = crate::config::InvalidChoice;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse()
            .map(Self::Native)
            .or_else(|_| value.parse().map(Self::Alias))
            .or_else(|_| value.parse().map(Self::Refused))
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
    NativeFlag(NativeFlag),
    /// A flag that stands for a native setting, as supplied.
    Alias(AliasFlag),
    Refused(RefusedFlag),
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
            let flag = name.parse::<ServerFlag>().ok().filter(|_| spelled);
            let filesystem_backend = flag.zip(supplied).is_some_and(|(f, v)| {
                f == ServerFlag::Alias(AliasFlag::Backend) && v == "filesystem"
            });
            let native = spelled && native_name && !filesystem_backend;
            let native_flag =
                command == CliCommand::Serve && matches!(flag, Some(ServerFlag::Native(_)));
            let special = flag.filter(|_| !native);
            let expects_pair = special.is_none() || native_flag;
            let separate = inline.is_none()
                && next.is_some_and(|value| {
                    native
                        || native_flag
                        || !value.starts_with('-')
                        || value.parse::<i64>().is_ok()
                        || (hyphens == 2 && special.is_none())
                });
            let value = inline.or_else(|| {
                if separate {
                    next.map(String::as_str)
                } else {
                    None
                }
            });
            let valid_syntax = (hyphens == 2 || native || native_flag)
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
                kind: match special {
                    Some(ServerFlag::Native(flag)) => InputKind::NativeFlag(flag),
                    Some(ServerFlag::Alias(flag)) => InputKind::Alias(flag),
                    Some(ServerFlag::Refused(flag)) => InputKind::Refused(flag),
                    None => InputKind::Native,
                },
                invalid: (expects_pair && !valid_syntax).then_some(InputError::Syntax),
            };
            let saved_key = if key.len() <= MAX_KEY_BYTES {
                Some(key.to_owned())
            } else {
                None
            };
            if !self.push(input) {
                break;
            }
            if expects_pair
                && !valid_syntax
                && let Some(key) = saved_key
            {
                self.diagnostics.errors.push(Diagnostic::new(DiagnosticSource::Cli, key,
                    format!("Options require --name VALUE pairs; for example {example}. Use --help for supported options.")));
                if !native_name && !native_flag {
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

    /// The special CLI forms in encounter order; their last occurrence wins.
    fn server_flags(&self) -> impl Iterator<Item = (ServerFlag, &Input)> {
        self.entries.iter().filter_map(|input| match input.kind {
            InputKind::NativeFlag(flag) => Some((ServerFlag::Native(flag), input)),
            InputKind::Alias(flag) => Some((ServerFlag::Alias(flag), input)),
            InputKind::Refused(flag) => Some((ServerFlag::Refused(flag), input)),
            InputKind::Native | InputKind::Translated => None,
        })
    }

    /// Helper commands have no server settings to apply these flags to.
    pub(crate) fn diagnose_server_flags(&mut self) {
        let effective: BTreeMap<_, _> = self.server_flags().collect();
        let values = advice_values(&effective);
        let base = TranslationBase::new(&self.entries, &effective);
        let errors: Vec<_> = self
            .server_flags()
            .map(|(flag, input)| {
                Diagnostic::new(
                    input.source,
                    &input.key,
                    format!(
                        "Server options belong to gcs-mimic serve; remove --{} from this command. For the server, {}.",
                        flag.as_str(),
                        flag_advice(flag, &values, &base)
                    ),
                )
            })
            .collect();
        self.diagnostics.errors.extend(errors);
    }

    /// Runs after the file is read so listener parts and conditional translations use the
    /// same precedence and defaults as native settings. Refused flags never reach the config.
    pub(crate) fn resolve_server_flags(&mut self) {
        let effective: BTreeMap<_, _> = self.server_flags().collect();
        let values = advice_values(&effective);
        let base = TranslationBase::new(&self.entries, &effective);
        let mut translated: BTreeMap<&str, Input> = BTreeMap::new();
        let mut refused = BTreeMap::new();
        let mut equivalent = Vec::new();
        for (flag, input) in &effective {
            let Some(resolution) = resolve_flag(*flag, input, &effective, &values, &base) else {
                continue;
            };
            let (setting, value) = match resolution {
                FlagResolution::Accept(setting, value) => (setting, value),
                FlagResolution::Refuse(reason) => {
                    refused.insert(*flag, reason);
                    continue;
                }
            };
            if let ServerFlag::Alias(source) = flag {
                equivalent.extend([
                    format!("--{}", setting.as_str().replace('_', "-")),
                    if *source == AliasFlag::PublicHost && !safe_url(&value) {
                        "<REDACTED>"
                    } else {
                        &value
                    }
                    .to_owned(),
                ]);
            }
            // host and port share one listener, so the first of them names it in diagnostics.
            translated.entry(setting.as_str()).or_insert_with(|| Input {
                source: input.source,
                key: input.key.clone(),
                name: setting.as_str().into(),
                value,
                kind: InputKind::Translated,
                invalid: None,
            });
        }
        let errors: Vec<_> = self
            .server_flags()
            .filter_map(|(flag, input)| {
                let reason = refused.get(&flag)?;
                Some(Diagnostic::new(input.source, &input.key, reason.clone()))
            })
            .collect();
        let overrides = translated
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
        let warnings: Vec<_> = overrides.collect();
        if !equivalent.is_empty() {
            let routing = if effective.contains_key(&ServerFlag::Alias(AliasFlag::PublicHost)) {
                "Requests to a public-host subdomain select a virtual-host bucket; call the JSON API through a host outside those subdomains or set --api-base-url explicitly. "
            } else {
                ""
            };
            // JSON escaping changes these bytes, so a raw log line cannot be pasted safely.
            let fix = if equivalent.iter().any(|value| {
                value
                    .chars()
                    .any(|c| matches!(c, '\'' | '"' | '\\') || c.is_control())
            }) {
                format!(
                    "{routing}Use native_args, the canonical JSON argument array; decode it with jq before passing arguments to the process."
                )
            } else {
                let block = equivalent
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| format!("{} {}", pair[0], shell_value(&pair[1])))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{routing}Use native options: {block}")
            };
            let mut diagnostic = Diagnostic::new(DiagnosticSource::Cli, "native_equivalent", fix);
            diagnostic.native_args = Some(equivalent);
            self.diagnostics.info.push(diagnostic);
        }
        self.diagnostics.errors.extend(errors);
        self.diagnostics.warnings.extend(warnings);
        // Appended after native inputs, so an alias wins within its own source.
        self.entries.extend(translated.into_values());
    }
}

/// Values that advice may quote.
fn advice_values<'a>(effective: &BTreeMap<ServerFlag, &'a Input>) -> BTreeMap<ServerFlag, &'a str> {
    effective
        .iter()
        .map(|(flag, input)| (*flag, input.value.as_str()))
        .collect()
}

/// Empty listener parts retain their defaults; an empty Pub/Sub endpoint deliberately disables it.
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
                && (!input.value.is_empty() || name == "pubsub_emulator_host")
                && !(input.key == "PUBSUB_EMULATOR_HOST"
                    && entries.iter().any(|other| {
                        other.source == DiagnosticSource::Environment
                            && other.key == "GCS_MIMIC_PUBSUB_EMULATOR_HOST"
                    }))
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
    pubsub_emulator_host: bool,
}

impl TranslationBase {
    fn new(entries: &[Input], flags: &BTreeMap<ServerFlag, &Input>) -> Self {
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
            scheme: native_value(entries, "scheme")
                .or_else(|| {
                    flags
                        .get(&ServerFlag::Native(NativeFlag::Scheme))
                        .map(|i| i.value.as_str())
                })
                .and_then(|scheme| scheme.parse().ok()),
            pubsub_emulator_host: native_value(entries, "pubsub_emulator_host")
                .is_some_and(|value| !value.is_empty()),
        }
    }

    /// fake-gcs-server builds `--public-host` URLs with its own scheme, `https` by default. Native
    /// `auto` serves HTTPS too, so the translation keeps that default rather than following
    /// `Scheme::primary`.
    fn public_scheme(&self) -> &'static str {
        match self.scheme {
            Some(Scheme::Http) => "http",
            Some(Scheme::Auto | Scheme::Https | Scheme::Both) | None => "https",
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

enum FlagResolution {
    Accept(Setting, String),
    Refuse(String),
}

/// A malformed native listener stays with its own validator; every source flag resolves once.
fn resolve_flag(
    flag: ServerFlag,
    input: &Input,
    flags: &BTreeMap<ServerFlag, &Input>,
    values: &BTreeMap<ServerFlag, &str>,
    base: &TranslationBase,
) -> Option<FlagResolution> {
    let value = |key| flags.get(&key).map(|input| input.value.as_str());
    let set = |key| value(ServerFlag::Native(key)).filter(|v: &&str| !v.is_empty());
    let text = &input.value;
    let resolved = match flag {
        ServerFlag::Native(NativeFlag::Host)
            if text != "localhost" && text.parse::<IpAddr>().is_err() =>
        {
            FlagResolution::Refuse(
                "host must be a numeric IP address or localhost; use --host 127.0.0.1".into(),
            )
        }
        ServerFlag::Native(NativeFlag::Port | NativeFlag::PortHttp)
            if text.parse::<u16>().is_err() =>
        {
            FlagResolution::Refuse(format!(
                "{} must be a port in 0..65535; supply a numeric port",
                input.key
            ))
        }
        ServerFlag::Native(NativeFlag::Host | NativeFlag::Port) => {
            let host = match set(NativeFlag::Host) {
                Some(host) => host_text(host),
                None => bracketed_ip(base.listen?.ip()),
            };
            let port = match set(NativeFlag::Port) {
                Some(port) => port.to_owned(),
                None => base.listen?.port().to_string(),
            };
            FlagResolution::Accept(Setting::Listen, format!("{host}:{port}"))
        }
        ServerFlag::Native(NativeFlag::PortHttp) if base.scheme != Some(Scheme::Both) => {
            FlagResolution::Refuse(format!(
                "{} requires scheme=both; set --scheme both or remove {}",
                input.key, input.key
            ))
        }
        ServerFlag::Native(NativeFlag::PortHttp) => {
            let host = match set(NativeFlag::Host) {
                Some(host) => host_text(host),
                None => bracketed_ip(base.http_ip?),
            };
            FlagResolution::Accept(Setting::ListenHttp, format!("{host}:{text}"))
        }
        ServerFlag::Native(NativeFlag::Scheme) => {
            FlagResolution::Accept(Setting::Scheme, text.clone())
        }
        ServerFlag::Native(NativeFlag::LogLevel) => {
            FlagResolution::Accept(Setting::LogLevel, text.clone())
        }
        ServerFlag::Alias(source) => resolve_alias(source, text, base),
        ServerFlag::Refused(source) => FlagResolution::Refuse(refused_advice(source, values)),
    };
    Some(resolved)
}

fn resolve_alias(flag: AliasFlag, text: &str, base: &TranslationBase) -> FlagResolution {
    let (setting, value) = match flag {
        AliasFlag::PublicHost => (
            Setting::PublicBaseUrl,
            format!("{}://{text}", base.public_scheme()),
        ),
        AliasFlag::Data => (Setting::SeedDir, text.to_owned()),
        AliasFlag::Backend => (Setting::Backend, "disk".into()),
        AliasFlag::CertLocation => (Setting::TlsCert, text.to_owned()),
        AliasFlag::PrivateKeyLocation => (Setting::TlsKey, text.to_owned()),
        AliasFlag::EventPubsubProjectId
        | AliasFlag::EventPubsubTopic
        | AliasFlag::EventBucket
        | AliasFlag::EventObjectPrefix
        | AliasFlag::EventList
            if !base.pubsub_emulator_host =>
        {
            return FlagResolution::Refuse(event_advice(flag));
        }
        AliasFlag::EventList if text.trim() == "none" => {
            return FlagResolution::Refuse("event.list=none is not a fake-gcs-server event selection. Use native --event-list none to disable the global notification rule deliberately".into());
        }
        AliasFlag::EventPubsubProjectId => (Setting::EventPubsubProjectId, text.to_owned()),
        AliasFlag::EventPubsubTopic => (Setting::EventPubsubTopic, text.to_owned()),
        AliasFlag::EventBucket => (Setting::EventBucket, text.to_owned()),
        AliasFlag::EventObjectPrefix => (Setting::EventObjectPrefix, text.to_owned()),
        AliasFlag::EventList => (Setting::EventList, text.to_owned()),
    };
    FlagResolution::Accept(setting, value)
}

fn shell_value(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn event_advice(flag: AliasFlag) -> String {
    format!(
        "use --{} (GCS_MIMIC_{}) with --pubsub-emulator-host HOST:PORT or PUBSUB_EMULATOR_HOST; fake-gcs-server can publish to Google, but gcs-mimic never publishes to Google",
        flag.as_str().replace('.', "-"),
        flag.as_str().replace(['.', '-'], "_").to_ascii_uppercase()
    )
}

fn component<'a>(
    values: &BTreeMap<ServerFlag, &'a str>,
    key: NativeFlag,
    default: &'a str,
) -> &'a str {
    values
        .get(&ServerFlag::Native(key))
        .copied()
        .filter(|v| !v.is_empty())
        .unwrap_or(default)
}

fn safe_url(value: &str) -> bool {
    value.parse::<hyper::Uri>().is_ok_and(|uri| {
        crate::url::normalize_base_url(value).is_ok()
            && uri.authority().is_some_and(|a| {
                a.host().trim_matches(['[', ']']).parse::<IpAddr>().is_ok()
                    || crate::config::validate_dns_name(a.host()).is_ok()
            })
            && !value.chars().any(char::is_control)
    })
}

fn flag_advice(
    flag: ServerFlag,
    values: &BTreeMap<ServerFlag, &str>,
    base: &TranslationBase,
) -> String {
    match flag {
        ServerFlag::Native(flag) => native_advice(flag, values),
        ServerFlag::Alias(flag) => alias_advice(flag, values, base),
        ServerFlag::Refused(flag) => refused_advice(flag, values),
    }
}

fn native_advice(flag: NativeFlag, values: &BTreeMap<ServerFlag, &str>) -> String {
    let host = component(values, NativeFlag::Host, "0.0.0.0")
        .parse()
        .map_or_else(|_| "<IP_ADDRESS>".into(), bracketed_ip);
    let port = component(values, NativeFlag::Port, "4443")
        .parse::<u16>()
        .map_or_else(|_| "<PORT_0_TO_65535>".into(), |p| p.to_string());
    let scheme = component(values, NativeFlag::Scheme, Scheme::default().as_str());
    let value = values
        .get(&ServerFlag::Native(flag))
        .copied()
        .unwrap_or_default();
    match flag {
        NativeFlag::Host | NativeFlag::Port => format!("use --host IP --port PORT or GCS_MIMIC_LISTEN={host}:{port}; use a valid IP address and port 0..65535"),
        NativeFlag::Scheme if scheme.parse::<Scheme>().is_ok() => format!("use GCS_MIMIC_SCHEME={scheme} or --scheme {scheme}"),
        NativeFlag::Scheme => "use GCS_MIMIC_SCHEME=auto, http, https or both; auto serves HTTP and HTTPS on listen, both serves HTTPS on listen and HTTP on listen_http".into(),
        NativeFlag::PortHttp => {
            let port = component(values, NativeFlag::PortHttp, "8000").parse::<u16>()
                .map_or_else(|_| "<PORT_0_TO_65535>".into(), |p| p.to_string());
            format!("use GCS_MIMIC_SCHEME=both with GCS_MIMIC_LISTEN_HTTP={host}:{port}; the primary GCS_MIMIC_LISTEN serves HTTPS")
        }
        NativeFlag::LogLevel if matches!(value, "debug" | "info" | "warn" | "warning" | "error") => format!("use GCS_MIMIC_LOG_LEVEL={} or --log-level {}; this controls runtime stderr events and informational diagnostics, not validation warnings/errors or stdout lifecycle events", if value == "warning" { "warn" } else { value }, if value == "warning" { "warn" } else { value }),
        NativeFlag::LogLevel => "use GCS_MIMIC_LOG_LEVEL=info or --log-level info; supported levels are debug, info, warn or error".into(),
    }
}

fn alias_advice(
    flag: AliasFlag,
    values: &BTreeMap<ServerFlag, &str>,
    base: &TranslationBase,
) -> String {
    let value = values
        .get(&ServerFlag::Alias(flag))
        .copied()
        .unwrap_or_default();
    match flag {
        AliasFlag::PublicHost => {
            let url = format!("{}://{value}", base.public_scheme());
            let example = crate::url::normalize_public_base_url(&url)
                .unwrap_or_else(|_| "https://storage.example.invalid:4443".into());
            format!("use --public-base-url {} (GCS_MIMIC_PUBLIC_BASE_URL={example}); public-host subdomains select virtual-host buckets, so call the JSON API through a host outside those subdomains or set --api-base-url explicitly", shell_value(&example))
        }
        AliasFlag::Data => "use --seed-dir PATH or GCS_MIMIC_SEED_DIR; fixtures replay on every startup".into(),
        AliasFlag::Backend => "use --backend disk or --backend memory".into(),
        AliasFlag::CertLocation | AliasFlag::PrivateKeyLocation => "supply both --tls-cert PATH (GCS_MIMIC_TLS_CERT) and --tls-key PATH (GCS_MIMIC_TLS_KEY); one-sided overrides fail".into(),
        AliasFlag::EventPubsubProjectId
        | AliasFlag::EventPubsubTopic
        | AliasFlag::EventBucket
        | AliasFlag::EventObjectPrefix
        | AliasFlag::EventList => event_advice(flag),
    }
}

fn refused_advice(flag: RefusedFlag, values: &BTreeMap<ServerFlag, &str>) -> String {
    let value = values
        .get(&ServerFlag::Refused(flag))
        .copied()
        .unwrap_or_default();
    match flag {
        RefusedFlag::ExternalUrl => {
            let example = if safe_url(value) { value } else { "http://gcs:4443" };
            format!("external-url controls only selfLink/mediaLink in fake-gcs-server; resumable upload Location follows the request address. Use --api-base-url {} (GCS_MIMIC_API_BASE_URL={example}), which also fixes resumable upload URLs to that address; choose an address every client can reach", shell_value(example))
        }
        RefusedFlag::FilesystemRoot => "filesystem-root cannot use fake-gcs-server's storage format. Remove it and use --data-dir PATH (GCS_MIMIC_DATA_DIR, /var/lib/gcs-mimic in the image) for native storage; convert a stopped source snapshot with gcs-mimic import fake-gcs-server --source DIR --data-dir NEW".into(),
        RefusedFlag::CorsHeaders => "cors-headers cannot preserve fake-gcs-server's global request-header allowlist. Remove it: permissive mode already allows every origin, strict mode follows each bucket's cors rules, and JSON preflight already allows every valid requested header. Use --cors-fallback allow-all (GCS_MIMIC_CORS_FALLBACK=allow-all) explicitly to allow every origin on XML buckets without rules".into(),
        RefusedFlag::Location => {
            let example = bucket_location(value).map_or_else(|_| "ASIA-NORTHEAST1", |(location, _)| location);
            format!("location cannot preserve fake-gcs-server's server-wide response location. Use --default-location {example} (GCS_MIMIC_DEFAULT_LOCATION={example}) with GCS_MIMIC_COMPAT_PROFILE=native for new buckets without an explicit location; existing buckets and explicit locations win, and legacy-emulator global location defaults remain unsupported")
        }
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
    inputs.diagnose_server_flags();
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
        assert!(matches!(report.config.scheme, crate::config::Scheme::Auto));
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
        inputs.diagnose_server_flags();
        emitted(&inputs.diagnostics)
    }

    const NATIVE: [&str; 5] = ["port", "port-http", "scheme", "host", "log-level"];

    const ALIASES: [&str; 10] = [
        "backend",
        "data",
        "cert-location",
        "private-key-location",
        "public-host",
        "event.pubsub-project-id",
        "event.pubsub-topic",
        "event.bucket",
        "event.object-prefix",
        "event.list",
    ];

    const REFUSED: [&str; 4] = [
        "filesystem-root",
        "external-url",
        "cors-headers",
        "location",
    ];

    #[test]
    fn alias_inventory_and_all_flag_forms_are_diagnosed_even_when_empty() {
        for flag in NATIVE.iter().chain(&ALIASES).chain(&REFUSED) {
            let kind = flag.parse::<ServerFlag>().unwrap();
            assert_eq!(matches!(kind, ServerFlag::Native(_)), NATIVE.contains(flag));
            assert_eq!(matches!(kind, ServerFlag::Alias(_)), ALIASES.contains(flag));
            assert_eq!(
                matches!(kind, ServerFlag::Refused(_)),
                REFUSED.contains(flag)
            );
            for value in ["", "secret-do-not-print\n"] {
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
                        if !matches!(*flag, "data" | "cert-location" | "private-key-location") {
                            assert!(!text.contains("secret-do-not-print"), "{text}");
                        }
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
                        } else if REFUSED.contains(flag)
                            || flag.starts_with("event.")
                            || matches!(*flag, "port" | "port-http" | "host" | "public-host")
                        {
                            assert!(text.contains(&key), "{text}");
                            assert!(report.diagnostics.is_err());
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
            "-public-host",
            "gcs:4443",
            "-backend",
            "filesystem",
            "-data",
            "/data",
            "-log-level",
            "debug",
        ]);
        assert_eq!(server_arguments(&args), Some(args.as_slice()));
        let report = crate::config::Config::preflight(&args, &image);
        assert!(!report.diagnostics.is_err(), "{}", report.diagnostics);
        let config = &report.config;
        assert!(matches!(config.scheme, crate::config::Scheme::Http));
        assert_eq!(config.listen.to_string(), "0.0.0.0:4443");
        assert_eq!(
            config.public_base_url.as_ref().map(|url| url.as_str()),
            Some("http://gcs:4443")
        );
        assert_eq!(config.data_dir, std::path::Path::new("/var/lib/gcs-mimic"));
        assert_eq!(
            config.seed_dir.as_deref(),
            Some(std::path::Path::new("/data"))
        );
        assert!(matches!(
            config.log_level,
            crate::diagnostics::LogLevel::Debug
        ));
        assert_eq!(config.cors_fallback, None);
        assert!(report.diagnostics.warnings.is_empty());
        assert_eq!(report.diagnostics.info.len(), 1);
        let text = emitted(&report.diagnostics);
        for native in [
            "--public-base-url 'http://gcs:4443'",
            "--backend 'disk'",
            "--seed-dir '/data'",
        ] {
            assert!(text.contains(native), "{text}");
        }
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
        assert!(report.diagnostics.is_err());
        has(&report.diagnostics, "-filesystem-root", "--data-dir");
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
        assert!(emitted(&report.diagnostics).contains("never publishes to Google"));
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
                assert!(
                    !report.diagnostics.is_err(),
                    "{args:?}: {}",
                    report.diagnostics
                );
                assert!(report.diagnostics.warnings.is_empty());
                format!("{:?}", report.config)
            });
            assert!(
                configurations.iter().all(|c| *c == configurations[0]),
                "{name}: {configurations:?}"
            );
        }
    }

    #[test]
    fn public_host_translation_follows_the_effective_scheme() {
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
            assert_eq!(
                report.config.public_base_url.as_ref().unwrap().as_str(),
                "http://gcs:4443"
            );
            assert!(
                emitted(&report.diagnostics).contains("--public-base-url 'http://gcs:4443'"),
                "{args:?}: {}",
                report.diagnostics
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
    fn port_http_and_listen_http_require_both() {
        for (args, key) in [
            (&["-scheme", "http", "-port-http", "8001"][..], "-port-http"),
            (&["--port-http", "8001"], "--port-http"),
        ] {
            let report = crate::config::Config::preflight(&strings(args), &BTreeMap::new());
            has(
                &report.diagnostics,
                key,
                &format!("{key} requires scheme=both"),
            );
            has(&report.diagnostics, key, &format!("remove {key}"));
            assert!(report.diagnostics.warnings.is_empty());
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
    fn native_options_are_quiet_and_inexact_flags_are_refused() {
        for args in [
            &["-scheme", "http"][..],
            &["-port", "9000"],
            &["-host", "127.0.0.2"],
            &["-scheme", "both", "-port-http", "8001"],
            &["-backend", "memory"],
            &["-log-level", "warning"],
        ] {
            let report = crate::config::Config::preflight(&strings(args), &BTreeMap::new());
            assert_eq!(emitted(&report.diagnostics), "", "{args:?}");
        }

        for (args, expected) in [
            (
                &["-filesystem-root", "/storage"][..],
                "gcs-mimic import fake-gcs-server",
            ),
            (
                &["-external-url", "http://gcs:4443"],
                "GCS_MIMIC_API_BASE_URL",
            ),
            (&["-cors-headers", "X-Custom"], "--cors-fallback allow-all"),
            (&["-location", "EU"], "--default-location EU"),
        ] {
            let report = crate::config::Config::preflight(&strings(args), &BTreeMap::new());
            assert!(report.diagnostics.is_err());
            assert!(
                report
                    .diagnostics
                    .errors
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
        assert!(advice.contains("--public-base-url 'http://Storage.Invalid:80'"));
        assert_eq!(
            report.config.public_base_url.as_ref().unwrap().as_str(),
            "http://storage.invalid"
        );
        // Helper commands have no server settings, so alias flags still fail there.
        let helper = cli_diagnostics(&strings(&["healthcheck", "-public-host", "x"]));
        assert!(
            helper
                .to_string()
                .contains("call the JSON API through a host outside those subdomains")
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
        assert!(text.contains("permissive mode already allows every origin, strict mode follows each bucket's cors rules"), "{text}");
        assert!(!text.contains("secret"));
        assert!(
            helper_advice(&strings(&["-location", "US-CENTRAL1"]))
                .contains("GCS_MIMIC_DEFAULT_LOCATION=US-CENTRAL1")
        );
        // Invalid listener components fail independently without echoing values.
        assert!(
            report
                .diagnostics
                .errors
                .iter()
                .any(|d| d.key == "-host" && d.fix.contains("host must"))
        );
        let cors = &report
            .diagnostics
            .errors
            .iter()
            .find(|d| d.key == "-cors-headers")
            .unwrap()
            .fix;
        assert!(cors.contains("cannot preserve"));
        assert!(cors.contains("--cors-fallback allow-all"), "{cors}");
        assert_eq!(report.config.cors_fallback, None);
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
                    .errors
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
    fn refused_location_advice_names_the_native_creation_default() {
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
            assert!(report.diagnostics.is_err());
            assert_eq!(report.config.default_location, "US");
            assert_eq!(report.diagnostics.errors[0].key, "-location");
            let advice = if let Some(expected) = expected {
                let advice = emitted(&report.diagnostics);
                assert!(advice.contains(&format!("GCS_MIMIC_DEFAULT_LOCATION={expected}")));
                assert!(advice.contains(&format!("--default-location {expected}")));
                advice
            } else {
                let advice = emitted(&report.diagnostics);
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
