use crate::auth::AuthMode;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub api_base_url: String,
    pub max_connections: usize,
    pub max_object_bytes: u64,
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
    pub connection_timeout_ms: u64,
    pub object_request_timeout_ms: u64,
    pub catalog_wait_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
    pub auth_mode: AuthMode,
    /// Strict-mode identity registry; loaded once at startup, never in permissive mode.
    pub identity_file: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:4443".parse().unwrap(),
            data_dir: PathBuf::from("data"),
            api_base_url: "http://127.0.0.1:4443".into(),
            max_connections: 32,
            max_object_bytes: 1024 * 1024 * 1024,
            worker_threads: 2,
            max_blocking_threads: 2,
            connection_timeout_ms: 5_000,
            object_request_timeout_ms: 900_000,
            catalog_wait_timeout_ms: 30_000,
            shutdown_timeout_ms: 5_000,
            auth_mode: AuthMode::Permissive,
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
        let mut config = Self::default();
        for (key, value) in settings {
            match key.as_str() {
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
                "data_dir" => {
                    if value.is_empty() {
                        return Err("data_dir must not be empty".into());
                    }
                    config.data_dir = PathBuf::from(value);
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
                "identity_file" => {
                    if value.is_empty() {
                        return Err("identity_file must not be empty".into());
                    }
                    config.identity_file = Some(PathBuf::from(value));
                }
                _ => return Err(format!("unknown setting: {key}")),
            }
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
            assert_eq!(value(Config::load(&[], &BTreeMap::new()).unwrap()), default);
            assert_eq!(value(Config::load(&args, &BTreeMap::new()).unwrap()), 100);
            let env = BTreeMap::from([(key.clone(), "200".into())]);
            assert_eq!(value(Config::load(&args, &env).unwrap()), 200);
            let mut cli = args.clone();
            cli.extend([flag.clone(), "300".into()]);
            assert_eq!(value(Config::load(&cli, &env).unwrap()), 300);
            for invalid in [
                "0".into(),
                "-1".into(),
                (maximum + 1).to_string(),
                "unlimited".into(),
            ] {
                assert!(Config::load(&[flag.clone(), invalid.clone()], &BTreeMap::new()).is_err());
                assert!(
                    Config::load(&[], &BTreeMap::from([(key.clone(), invalid.clone())])).is_err()
                );
                std::fs::write(&file, format!(r#"{{"{setting}":"{invalid}"}}"#)).unwrap();
                assert!(Config::load(&args, &BTreeMap::new()).is_err());
            }
            for bound in [1, maximum] {
                assert_eq!(
                    value(
                        Config::load(&[flag.clone(), bound.to_string()], &BTreeMap::new()).unwrap()
                    ),
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
            Config::load(&[], &BTreeMap::new())
                .unwrap()
                .max_object_bytes,
            1_073_741_824
        );
        assert_eq!(
            Config::load(&args, &BTreeMap::new())
                .unwrap()
                .max_object_bytes,
            64
        );
        assert_eq!(Config::load(&args, &env).unwrap().max_object_bytes, 128);
        args.extend(["--max-object-bytes".into(), "256".into()]);
        assert_eq!(Config::load(&args, &env).unwrap().max_object_bytes, 256);
        for value in ["0", "-1", "5497558138881", "unlimited"] {
            assert!(
                Config::load(
                    &["--max-object-bytes".into(), value.into()],
                    &BTreeMap::new()
                )
                .is_err()
            );
        }
        assert_eq!(
            Config::load(
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
            Config::load(&[], &env).unwrap().api_base_url,
            "https://example.invalid/base"
        );
        assert_eq!(
            Config::load(
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
                Config::load(&["--api-base-url".into(), value.into()], &BTreeMap::new()).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn authentication_profile_precedence_and_validation() {
        let file =
            std::env::temp_dir().join(format!("gcs-mimic-auth-config-{}.json", std::process::id()));
        let args = vec!["--config".into(), file.to_str().unwrap().into()];
        let defaults = Config::load(&[], &BTreeMap::new()).unwrap();
        assert_eq!(defaults.auth_mode, AuthMode::Permissive);
        assert_eq!(defaults.identity_file, None);
        std::fs::write(
            &file,
            r#"{"auth_mode":"strict","identity_file":"file-ids.json"}"#,
        )
        .unwrap();
        let loaded = Config::load(&args, &BTreeMap::new()).unwrap();
        assert_eq!(loaded.auth_mode, AuthMode::Strict);
        assert_eq!(
            loaded.identity_file.as_deref(),
            Some(Path::new("file-ids.json"))
        );
        let env = BTreeMap::from([("GCS_MIMIC_IDENTITY_FILE".into(), "env-ids.json".into())]);
        assert_eq!(
            Config::load(&args, &env).unwrap().identity_file.as_deref(),
            Some(Path::new("env-ids.json"))
        );
        let mut cli = args.clone();
        cli.extend(["--identity-file".into(), "cli-ids.json".into()]);
        assert_eq!(
            Config::load(&cli, &env).unwrap().identity_file.as_deref(),
            Some(Path::new("cli-ids.json"))
        );
        // A higher-precedence permissive mode with a lower-precedence registry is a conflict.
        let permissive_env = BTreeMap::from([("GCS_MIMIC_AUTH_MODE".into(), "permissive".into())]);
        assert_eq!(
            Config::load(&args, &permissive_env).unwrap_err(),
            "identity_file requires auth_mode=strict"
        );
        let mut strict_cli = vec!["--auth-mode".into(), "strict".into()];
        assert_eq!(
            Config::load(&strict_cli, &permissive_env).unwrap_err(),
            "auth_mode=strict requires identity_file"
        );
        strict_cli.extend(["--identity-file".into(), "ids.json".into()]);
        assert_eq!(
            Config::load(&strict_cli, &permissive_env)
                .unwrap()
                .auth_mode,
            AuthMode::Strict
        );
        assert_eq!(
            Config::load(
                &["--identity-file".into(), "ids.json".into()],
                &BTreeMap::new()
            )
            .unwrap_err(),
            "identity_file requires auth_mode=strict"
        );
        for invalid in ["", "Strict", "none", "STRICT"] {
            assert!(
                Config::load(&["--auth-mode".into(), invalid.into()], &BTreeMap::new()).is_err(),
                "{invalid}"
            );
        }
        assert!(
            Config::load(
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
            Config::load(&[], &BTreeMap::new()).unwrap().data_dir,
            Path::new("data")
        );
        assert_eq!(
            Config::load(&args, &BTreeMap::new()).unwrap().data_dir,
            Path::new("file-data")
        );
        assert_eq!(
            Config::load(&args, &env).unwrap().data_dir,
            Path::new("env-data")
        );
        let mut cli_args = args.clone();
        cli_args.extend(["--data-dir".into(), "cli-data".into()]);
        assert_eq!(
            Config::load(&cli_args, &env).unwrap().data_dir,
            Path::new("cli-data")
        );
        assert!(Config::load(&["--data-dir".into(), "".into()], &BTreeMap::new()).is_err());
        assert!(
            Config::load(
                &[],
                &BTreeMap::from([("GCS_MIMIC_DATA_DIR".into(), "".into())])
            )
            .is_err()
        );
        std::fs::write(&file, r#"{"data_dir":""}"#).unwrap();
        assert!(Config::load(&args, &BTreeMap::new()).is_err());
        std::fs::remove_file(file).unwrap();
    }
}
