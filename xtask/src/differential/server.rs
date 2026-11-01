//! Builds and runs the local gcs-mimic under test.

use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;

/// Builds the binary and returns its path from cargo's own artifact report, so
/// a custom target directory is honoured.
pub(crate) fn build(root: &Path) -> Result<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(root)
        .args(["build", "--locked", "--offline", "--bin", "gcs-mimic"])
        .args(["--message-format", "json-render-diagnostics"])
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cargo build: {e}"))?;
    if !output.status.success() {
        return Err(format!("cargo build failed: {}", output.status));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter(|message| message["target"]["name"] == "gcs-mimic")
        .find_map(|message| message["executable"].as_str().map(PathBuf::from))
        .ok_or_else(|| "cargo did not report the gcs-mimic executable".into())
}

pub(crate) struct LocalServer {
    child: Child,
    pub(crate) endpoint: String,
    /// The plaintext gRPC `host:port`.
    pub(crate) grpc: Option<String>,
}

impl LocalServer {
    /// Starts the native profile on loopback ports chosen by the OS (HTTP and
    /// plaintext gRPC), with the default soft-delete retention off, and waits
    /// for its ready event.
    pub(crate) fn start(binary: &Path, work: &Path) -> Result<Self> {
        let log = work.join("server.log");
        let child = Command::new(binary)
            .env_clear()
            .env("GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS", "0")
            .args(["serve", "--scheme", "http", "--listen", "127.0.0.1:0"])
            .args(["--grpc-listen", "127.0.0.1:0"])
            .args(["--compat-profile", "native", "--data-dir"])
            .arg(work.join("data"))
            .stdout(File::create(&log).map_err(|e| format!("server log: {e}"))?)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("start {}: {e}", binary.display()))?;
        let mut server = Self {
            child,
            endpoint: String::new(),
            grpc: None,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Some(status) = server.child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!("gcs-mimic exited during startup: {status}"));
            }
            let reader = BufReader::new(File::open(&log).map_err(|e| e.to_string())?);
            for line in reader.lines().map_while(std::result::Result::ok) {
                let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if event["event"] == "ready"
                    && let Some(listen) = event["listen"].as_str()
                {
                    server.endpoint = format!("http://{listen}");
                    server.grpc = event["listeners"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|l| l["scheme"] == "h2c")
                        .and_then(|l| l["listen"].as_str())
                        .map(str::to_owned);
                    return Ok(server);
                }
            }
            sleep(Duration::from_millis(50));
        }
        Err("gcs-mimic did not report ready within 30 seconds".into())
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn remove_dir(path: &Path) {
    let _ = fs::remove_dir_all(path);
}
