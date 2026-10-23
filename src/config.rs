use crate::auth::AuthMode;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
};

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
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

impl std::str::FromStr for Scheme {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            _ => Err("scheme must be http or https".into()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub scheme: Scheme,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub tls_handshake_timeout_ms: u64,
    pub data_dir: PathBuf,
    pub seed_dir: Option<PathBuf>,
    pub seed_project: String,
    pub api_base_url: String,
    pub xml_virtual_host_domains: Vec<String>,
    pub max_connections: usize,
    pub max_object_bytes: u64,
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
    pub connection_timeout_ms: u64,
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
            listen: "127.0.0.1:4443".parse().unwrap(),
            scheme: Scheme::Https,
            tls_cert: None,
            tls_key: None,
            tls_handshake_timeout_ms: 5_000,
            data_dir: PathBuf::from("data"),
            seed_dir: None,
            seed_project: "local-project".into(),
            api_base_url: "https://127.0.0.1:4443".into(),
            xml_virtual_host_domains: Vec::new(),
            max_connections: 32,
            max_object_bytes: 1024 * 1024 * 1024,
            worker_threads: 2,
            max_blocking_threads: 2,
            connection_timeout_ms: 5_000,
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
    pub fn load(args: &[String], env: &BTreeMap<String, String>) -> Result<Self, String> {
        let mut flags = BTreeMap::new();
        for pair in args.chunks(2) {
            if pair.len() != 2 || !pair[0].starts_with("--") {
                return Err("serve options require --name VALUE pairs".into());
            }
            let key = pair[0][2..].replace('-', "_");
            if flags.insert(key.clone(), pair[1].clone()).is_some() {
                return Err(format!("duplicate option: {}", pair[0]));
            }
        }
        let path = flags
            .remove("config")
            .or_else(|| env.get("GCS_MIMIC_CONFIG").cloned());
        let mut settings = BTreeMap::new();
        if let Some(path) = path {
            let mut bytes = Vec::new();
            File::open(Path::new(&path))
                .map_err(|e| format!("config: {e}"))?
                .take(65_537)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 65_536 {
                return Err("config exceeds 64 KiB".into());
            }
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| format!("config: {e}"))?;
            for (key, value) in value.as_object().ok_or("config must be a JSON object")? {
                let text = if let Some(text) = value.as_str() {
                    text.to_owned()
                } else if value.is_u64() {
                    value.to_string()
                } else {
                    return Err(format!("config {key}: expected string or unsigned integer"));
                };
                settings.insert(key.clone(), text);
            }
        }
        for (key, value) in env {
            if let Some(key) = key.strip_prefix("GCS_MIMIC_")
                && key != "CONFIG"
            {
                settings.insert(key.to_ascii_lowercase(), value.clone());
            }
        }
        settings.extend(flags);
        let explicit_api_base_url = settings.contains_key("api_base_url");
        let tls_options_supplied = settings.keys().any(|key| {
            matches!(
                key.as_str(),
                "tls_cert" | "tls_key" | "tls_handshake_timeout_ms"
            )
        });
        let mut config = Self::default();
        for (key, value) in settings {
            match key.as_str() {
                "scheme" => config.scheme = value.parse()?,
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
                    config.tls_handshake_timeout_ms = bounded(&key, &value, 1, 60_000)?
                }
                "api_base_url" => {
                    let uri: hyper::Uri = value.parse().map_err(|_| "invalid api_base_url")?;
                    if !matches!(uri.scheme_str(), Some("http" | "https"))
                        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
                        || uri.query().is_some()
                        || value.contains('#')
                    {
                        return Err("api_base_url must be an absolute HTTP(S) URL without credentials, query or fragment".into());
                    }
                    config.api_base_url = value.trim_end_matches('/').to_owned();
                }
                "xml_virtual_host_domains" => {
                    if !value.is_empty() {
                        for domain in value.split(',') {
                            let domain = domain.trim().to_ascii_lowercase();
                            validate_dns_name(&domain)?;
                            if config.xml_virtual_host_domains.contains(&domain)
                                || config.xml_virtual_host_domains.len() == 32
                            {
                                return Err("xml_virtual_host_domains must contain at most 32 distinct DNS names".into());
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
                "seed_project" => {
                    crate::json::validate_project(&value)
                        .map_err(|error| format!("seed_project: {}", error.message))?;
                    config.seed_project = value;
                }
                "listen" => {
                    config.listen = value
                        .parse()
                        .map_err(|_| "listen must be an IP:port socket address")?
                }
                "max_connections" => {
                    config.max_connections = bounded(&key, &value, 1, 1024)? as usize
                }
                "max_object_bytes" => {
                    config.max_object_bytes = bounded(&key, &value, 1, 5 * 1024_u64.pow(4))?
                }
                "worker_threads" => config.worker_threads = bounded(&key, &value, 1, 32)? as usize,
                "max_blocking_threads" => {
                    config.max_blocking_threads = bounded(&key, &value, 1, 32)? as usize
                }
                "connection_timeout_ms" => {
                    config.connection_timeout_ms = bounded(&key, &value, 1, 60_000)?
                }
                "object_request_timeout_ms" => {
                    config.object_request_timeout_ms = bounded(&key, &value, 1, 86_400_000)?
                }
                "catalog_wait_timeout_ms" => {
                    config.catalog_wait_timeout_ms = bounded(&key, &value, 1, 60_000)?
                }
                "shutdown_timeout_ms" => {
                    config.shutdown_timeout_ms = bounded(&key, &value, 1, 60_000)?
                }
                "auth_mode" => config.auth_mode = value.parse()?,
                "compat_profile" => config.compat_profile = value.parse()?,
                "identity_file" => {
                    if value.is_empty() {
                        return Err("identity_file must not be empty".into());
                    }
                    config.identity_file = Some(PathBuf::from(value));
                }
                _ => return Err(format!("unknown setting: {key}")),
            }
        }
        match config.scheme {
            Scheme::Http if tls_options_supplied => {
                return Err("TLS settings require scheme=https".into());
            }
            Scheme::Https if config.tls_cert.is_some() != config.tls_key.is_some() => {
                return Err("tls_cert and tls_key must be supplied together".into());
            }
            _ => {}
        }
        if !explicit_api_base_url {
            config.api_base_url = format!("{}://127.0.0.1:4443", config.scheme.as_str());
        }
        // Reject mismatched profiles so a registry is never silently ignored or missing.
        match (config.auth_mode, &config.identity_file) {
            (AuthMode::Strict, None) => {
                return Err("auth_mode=strict requires identity_file".into());
            }
            (AuthMode::Permissive, Some(_)) => {
                return Err("identity_file requires auth_mode=strict".into());
            }
            _ => {}
        }
        Ok(config)
    }
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
    use super::*;

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
        std::fs::remove_file(file).unwrap();
    }

    fn load_http(args: &[String], env: &BTreeMap<String, String>) -> Result<Config, String> {
        let mut args = args.to_vec();
        args.extend(["--scheme".into(), "http".into()]);
        Config::load(&args, env)
    }

    #[test]
    fn tls_configuration_precedence_and_validation() {
        let defaults = Config::default();
        assert_eq!(defaults.scheme, Scheme::Https);
        assert_eq!(defaults.api_base_url, "https://127.0.0.1:4443");
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
        assert_eq!(
            load_http(&[], &BTreeMap::new()).unwrap().api_base_url,
            "http://127.0.0.1:4443"
        );
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
        assert_eq!(loaded.api_base_url, "https://127.0.0.1:4443");
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
        assert_eq!(loaded.api_base_url, "http://proxy.invalid/base");
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
        ] {
            let file = std::env::temp_dir()
                .join(format!("gcs-mimic-{setting}-{}.json", std::process::id()));
            let value = |config: Config| {
                if setting == "object_request_timeout_ms" {
                    config.object_request_timeout_ms
                } else {
                    config.catalog_wait_timeout_ms
                }
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
    fn advertised_endpoint_precedence_and_validation() {
        let env = BTreeMap::from([(
            "GCS_MIMIC_API_BASE_URL".into(),
            "https://example.invalid/base/".into(),
        )]);
        assert_eq!(
            load_http(&[], &env).unwrap().api_base_url,
            "https://example.invalid/base"
        );
        assert_eq!(
            load_http(
                &["--api-base-url".into(), "http://127.0.0.1:1234".into()],
                &env
            )
            .unwrap()
            .api_base_url,
            "http://127.0.0.1:1234"
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
        assert_eq!(
            load_http(&args, &permissive_env).unwrap_err(),
            "identity_file requires auth_mode=strict"
        );
        let mut strict_cli = vec!["--auth-mode".into(), "strict".into()];
        assert_eq!(
            load_http(&strict_cli, &permissive_env).unwrap_err(),
            "auth_mode=strict requires identity_file"
        );
        strict_cli.extend(["--identity-file".into(), "ids.json".into()]);
        assert_eq!(
            load_http(&strict_cli, &permissive_env).unwrap().auth_mode,
            AuthMode::Strict
        );
        assert_eq!(
            load_http(
                &["--identity-file".into(), "ids.json".into()],
                &BTreeMap::new()
            )
            .unwrap_err(),
            "identity_file requires auth_mode=strict"
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
