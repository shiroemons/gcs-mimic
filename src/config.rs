use std::{collections::BTreeMap, fs::File, io::Read, net::SocketAddr, path::Path};

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub max_connections: usize,
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
    pub connection_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:4443".parse().unwrap(),
            max_connections: 32,
            worker_threads: 2,
            max_blocking_threads: 2,
            connection_timeout_ms: 5_000,
            shutdown_timeout_ms: 5_000,
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
                "listen" => {
                    config.listen = value
                        .parse()
                        .map_err(|_| "listen must be an IP:port socket address")?
                }
                "max_connections" => {
                    config.max_connections = bounded(&key, &value, 1, 1024)? as usize
                }
                "worker_threads" => config.worker_threads = bounded(&key, &value, 1, 32)? as usize,
                "max_blocking_threads" => {
                    config.max_blocking_threads = bounded(&key, &value, 1, 32)? as usize
                }
                "connection_timeout_ms" => {
                    config.connection_timeout_ms = bounded(&key, &value, 1, 60_000)?
                }
                "shutdown_timeout_ms" => {
                    config.shutdown_timeout_ms = bounded(&key, &value, 1, 60_000)?
                }
                _ => return Err(format!("unknown setting: {key}")),
            }
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
