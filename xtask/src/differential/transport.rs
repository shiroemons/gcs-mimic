//! Protocol transports. HTTP goes through the external `curl` command, like the
//! dependency checks. A gRPC transport (`grpcurl`) can implement the same trait.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) method: String,
    /// Path and query, already percent-encoded.
    pub(crate) target: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Response {
    pub(crate) status: u16,
    /// Header names keep the case received; duplicates stay separate entries.
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Response {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub(crate) trait Transport {
    fn send(&self, request: &Request) -> Result<Response>;
    /// Requests sent so far, so a run can report what it cost a live service.
    fn requests(&self) -> u64;
}

pub(crate) struct CurlTransport {
    base: String,
    bearer: Option<String>,
    /// Loopback targets must not go through a configured proxy.
    bypass_proxy: bool,
    scratch: PathBuf,
    counter: AtomicU64,
}

impl CurlTransport {
    pub(crate) fn new(
        base: &str,
        bearer: Option<String>,
        bypass_proxy: bool,
        scratch: &Path,
    ) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            bearer,
            bypass_proxy,
            scratch: scratch.to_owned(),
            counter: AtomicU64::new(0),
        }
    }
}

/// Quotes a value for a curl config file.
fn config_quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The URL, headers and credentials go through a config on stdin so the bearer
/// token never appears in the process list.
fn curl_config(url: &str, headers: &[(String, String)], bearer: Option<&str>) -> String {
    let mut config = format!("url = {}\n", config_quote(url));
    for (name, value) in headers {
        config.push_str(&format!(
            "header = {}\n",
            config_quote(&format!("{name}: {value}"))
        ));
    }
    if let Some(token) = bearer {
        config.push_str(&format!(
            "header = {}\n",
            config_quote(&format!("Authorization: Bearer {token}"))
        ));
    }
    config
}

/// Parses the last header block of a `curl --dump-header` file; earlier blocks
/// belong to interim responses such as `100 Continue`.
pub(crate) fn parse_header_dump(dump: &str) -> Vec<(String, String)> {
    let normalized = dump.replace("\r\n", "\n");
    let block = normalized
        .rsplit("\n\n")
        .find(|block| block.starts_with("HTTP/"))
        .unwrap_or("");
    block
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

impl Transport for CurlTransport {
    fn requests(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }

    fn send(&self, request: &Request) -> Result<Response> {
        let id = self.counter.fetch_add(1, Ordering::Relaxed);
        let headers_path = self.scratch.join(format!("h{id}"));
        let body_path = self.scratch.join(format!("b{id}"));
        let request_path = self.scratch.join(format!("r{id}"));
        let outcome = self.exchange(request, &headers_path, &body_path, &request_path);
        for path in [&headers_path, &body_path, &request_path] {
            let _ = fs::remove_file(path);
        }
        outcome
    }
}

impl CurlTransport {
    fn exchange(
        &self,
        request: &Request,
        headers_path: &Path,
        body_path: &Path,
        request_path: &Path,
    ) -> Result<Response> {
        let mut command = Command::new("curl");
        command.args(["--disable", "--silent", "--show-error", "--max-time", "60"]);
        command.args(["--max-filesize", "8388608", "--write-out", "%{http_code}"]);
        if self.bypass_proxy {
            command.args(["--noproxy", "*"]);
        }
        if request.method == "HEAD" {
            command.arg("--head");
        } else {
            command.args(["--request", &request.method]);
        }
        let mut headers = request.headers.clone();
        if let Some(body) = &request.body {
            fs::write(request_path, body).map_err(|e| format!("write request body: {e}"))?;
            command
                .arg("--data-binary")
                .arg(format!("@{}", request_path.display()));
        } else if matches!(request.method.as_str(), "POST" | "PUT" | "PATCH") {
            headers.push(("Content-Length".into(), "0".into()));
        }
        command
            .arg("--dump-header")
            .arg(headers_path)
            .arg("--output")
            .arg(body_path)
            .args(["--config", "-"]);
        let config = curl_config(
            &format!("{}{}", self.base, request.target),
            &headers,
            self.bearer.as_deref(),
        );
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("curl: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(config.as_bytes())
                .map_err(|e| format!("curl: {e}"))?;
        }
        let output = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "curl failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let status = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .map_err(|_| "curl reported no HTTP status".to_owned())?;
        let dump = fs::read_to_string(headers_path).map_err(|e| format!("read headers: {e}"))?;
        Ok(Response {
            status,
            headers: parse_header_dump(&dump),
            body: fs::read(body_path).unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_final_header_block() {
        let dump = "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: a/b\r\nVary: Origin\r\nVary: X-Origin\r\n\r\n";
        assert_eq!(
            parse_header_dump(dump),
            [
                ("Content-Type".to_owned(), "a/b".to_owned()),
                ("Vary".to_owned(), "Origin".to_owned()),
                ("Vary".to_owned(), "X-Origin".to_owned()),
            ]
        );
    }

    #[test]
    fn config_keeps_secrets_out_of_arguments_and_escapes_values() {
        let config = curl_config(
            "https://h/p?q=\"x\"",
            &[("X-A".into(), "b\\c".into())],
            Some("tok"),
        );
        assert!(config.contains("url = \"https://h/p?q=\\\"x\\\"\""));
        assert!(config.contains("header = \"X-A: b\\\\c\""));
        assert!(config.contains("Authorization: Bearer tok"));
    }
}
