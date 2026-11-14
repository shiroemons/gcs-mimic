//! Protocol transports. HTTP goes through the external `curl` command, like the
//! dependency checks. A gRPC transport (`grpcurl`) can implement the same trait.

use serde_json::json;
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
    /// 429 retries made by the last `send`, then reset; zero for a transport
    /// that does not retry.
    fn take_retries(&self) -> u32 {
        0
    }
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
        command.args([
            "--disable",
            "--http1.1",
            "--silent",
            "--show-error",
            "--max-time",
            "60",
        ]);
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
            // `curl --head` writes the header block into the body file too.
            body: if request.method == "HEAD" {
                Vec::new()
            } else {
                read_body(body_path)?
            },
        })
    }
}

/// curl may leave no `--output` file for an empty body, so a missing file is an
/// empty body; any other read failure is the harness's and fails the step.
fn read_body(path: &Path) -> Result<Vec<u8>> {
    match fs::read(path) {
        Ok(body) => Ok(body),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("read body: {error}")),
    }
}

/// The proto file `grpcurl` compiles; reflection is not offered by the real
/// service, so the schema comes from the repository's pinned googleapis copy.
const STORAGE_PROTO: &str = "google/storage/v2/storage.proto";
/// The environment variable that carries the bearer token to `grpcurl`. The
/// header on its command line names the variable (`-expand-headers`), so the
/// token never appears in the process arguments.
/// Lets `-format-error` decode `google.rpc.ErrorInfo` and friends in status details.
const ERROR_DETAILS_PROTO: &str = "google/rpc/error_details.proto";
const TOKEN_ENV: &str = "GCSM_DIFF_GRPC_BEARER";
/// `grpcurl` exits with this offset plus the gRPC status code on a failed call.
const STATUS_EXIT_OFFSET: i32 = 64;

/// Whether the `grpcurl` command can be run.
pub(crate) fn grpcurl_available() -> bool {
    Command::new("grpcurl")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The `grpcurl -version` text (for example `grpcurl 1.9.4`), recorded with the
/// reference because its rendering rules shape every gRPC body.
pub(crate) fn grpcurl_version() -> String {
    Command::new("grpcurl")
        .arg("-version")
        .stdin(Stdio::null())
        .output()
        .map(|out| {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            text.trim().to_owned()
        })
        .unwrap_or_else(|_| "grpcurl (not installed)".into())
}

/// Unary and streaming Storage v2 calls through the external `grpcurl` command.
/// A request's `method` is the RPC name and its body the JSON message(s).
///
/// The response is normalized into the HTTP-shaped [`Response`] the rest of
/// the harness compares: `status` is the gRPC status code (0 for OK), and the
/// body is the response message, an array when the call returned several, or
/// `{"code", "message", "details"}` for a failed call.
pub(crate) struct GrpcTransport {
    address: String,
    plaintext: bool,
    bearer: Option<String>,
    proto_root: PathBuf,
    counter: AtomicU64,
}

impl GrpcTransport {
    pub(crate) fn new(
        address: &str,
        plaintext: bool,
        bearer: Option<String>,
        proto_root: &Path,
    ) -> Self {
        Self {
            address: address.to_owned(),
            plaintext,
            bearer,
            proto_root: proto_root.to_owned(),
            counter: AtomicU64::new(0),
        }
    }

    /// Builds the arguments. `-expand-headers` expands every `${NAME}` in every
    /// header from the environment, so a case header containing `${` is refused;
    /// only the authorization header may use it.
    fn args(&self, rpc: &str, headers: &[(String, String)]) -> Result<Vec<String>> {
        if let Some((name, _)) = headers
            .iter()
            .find(|(n, v)| n.contains("${") || v.contains("${"))
        {
            return Err(format!(
                "gRPC metadata {name:?} contains \"${{\", which would be expanded from the environment"
            ));
        }
        let mut args = vec!["-import-path".to_owned()];
        args.push(self.proto_root.display().to_string());
        args.extend([
            "-proto".into(),
            STORAGE_PROTO.into(),
            "-proto".into(),
            ERROR_DETAILS_PROTO.into(),
            "-format-error".into(),
        ]);
        args.extend(["-connect-timeout".into(), "15".into()]);
        args.extend(["-max-time".into(), "60".into()]);
        if self.plaintext {
            args.push("-plaintext".into());
        }
        if self.bearer.is_some() {
            args.push("-expand-headers".into());
            args.extend([
                "-H".into(),
                format!("authorization: Bearer ${{{TOKEN_ENV}}}"),
            ]);
        }
        for (name, value) in headers {
            args.extend(["-H".into(), format!("{name}: {value}")]);
        }
        args.extend([
            "-d".into(),
            "@".into(),
            self.address.clone(),
            rpc.to_owned(),
        ]);
        Ok(args)
    }
}

/// Reads the concatenated JSON values `grpcurl` prints, one per message.
fn parse_messages(stdout: &str) -> Result<Vec<serde_json::Value>> {
    serde_json::Deserializer::from_str(stdout)
        .into_iter::<serde_json::Value>()
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| format!("grpcurl printed output that is not JSON: {e}"))
}

/// Turns a finished `grpcurl` run into a response.
fn grpc_response(exit: Option<i32>, stdout: &str, stderr: &str) -> Result<Response> {
    let mut messages = parse_messages(stdout)?;
    let status = match exit {
        Some(0) => 0,
        Some(code) if (STATUS_EXIT_OFFSET + 1..=STATUS_EXIT_OFFSET + 16).contains(&code) => {
            u16::try_from(code - STATUS_EXIT_OFFSET).unwrap_or(2)
        }
        other => {
            return Err(format!(
                "grpcurl failed ({}): {}",
                other.map_or_else(|| "no exit code".to_owned(), |c| c.to_string()),
                stderr.trim()
            ));
        }
    };
    let value = if status != 0 {
        // With -format-error the status is printed to stderr as JSON. When a
        // detail names a type outside the loaded protos grpcurl cannot render
        // it and prints plain text; the case then continues with the code and
        // the raw text instead of stopping.
        parse_messages(stderr)
            .ok()
            .and_then(|mut parsed| parsed.pop())
            .unwrap_or_else(|| json!({"code": status, "raw": stderr.trim()}))
    } else {
        match messages.len() {
            0 => serde_json::Value::Null,
            1 => messages.remove(0),
            _ => serde_json::Value::Array(messages),
        }
    };
    let body = if value.is_null() {
        Vec::new()
    } else {
        value.to_string().into_bytes()
    };
    Ok(Response {
        status,
        headers: vec![("content-type".into(), "application/grpc+json".into())],
        body,
    })
}

impl Transport for GrpcTransport {
    fn requests(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }

    fn send(&self, request: &Request) -> Result<Response> {
        self.counter.fetch_add(1, Ordering::Relaxed);
        let mut command = Command::new("grpcurl");
        command.args(self.args(&request.method, &request.headers)?);
        // Nothing but what grpcurl needs: the environment may hold other secrets.
        command.env_clear();
        let inherited = ["PATH", "HOME", "SSL_CERT_FILE", "SSL_CERT_DIR"];
        let proxies = ["HTTPS_PROXY", "https_proxy", "GRPC_PROXY"];
        for name in inherited
            .into_iter()
            .chain(proxies.into_iter().filter(|_| !self.plaintext))
        {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if let Some(token) = &self.bearer {
            command.env(TOKEN_ENV, token);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("grpcurl: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let message = request.body.as_deref().unwrap_or(b"{}");
            stdin
                .write_all(message)
                .map_err(|e| format!("grpcurl: {e}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|e| format!("grpcurl: {e}"))?;
        grpc_response(
            output.status.code(),
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_arguments_keep_the_token_out_of_the_command_line() {
        let transport = GrpcTransport::new(
            "storage.googleapis.com:443",
            false,
            Some("s3cret-token".into()),
            Path::new("protos"),
        );
        let args = transport
            .args(
                "google.storage.v2.Storage/GetBucket",
                &[("x-goog-request-params".into(), "bucket=b".into())],
            )
            .unwrap();
        assert!(args.iter().all(|a| !a.contains("s3cret-token")));
        assert!(args.contains(&"-expand-headers".to_owned()));
        assert!(args.contains(&format!("authorization: Bearer ${{{TOKEN_ENV}}}")));
        assert!(args.contains(&"x-goog-request-params: bucket=b".to_owned()));
        assert!(!args.contains(&"-plaintext".to_owned()));
        assert_eq!(args[args.len() - 1], "google.storage.v2.Storage/GetBucket");
        let local = GrpcTransport::new("127.0.0.1:1", true, None, Path::new("protos"));
        let args = local.args("a.B/C", &[]).unwrap();
        let hostile = vec![("x-a".to_owned(), "${HOME}".to_owned())];
        assert!(transport.args("a.B/C", &hostile).is_err());
        assert!(args.contains(&"-plaintext".to_owned()));
        assert!(!args.contains(&"-expand-headers".to_owned()));
    }

    #[test]
    fn grpc_output_becomes_status_and_body() {
        let ok = grpc_response(Some(0), "{\"a\":1}\n{\"a\":2}", "").unwrap();
        assert_eq!(ok.status, 0);
        assert_eq!(ok.body, br#"[{"a":1},{"a":2}]"#);
        let one = grpc_response(Some(0), "{\"a\":1}", "").unwrap();
        assert_eq!(one.body, br#"{"a":1}"#);
        let empty = grpc_response(Some(0), "", "").unwrap();
        assert!(empty.body.is_empty());
        let failed = grpc_response(
            Some(69),
            "",
            "{\"code\":5,\"message\":\"Bucket not found\"}",
        )
        .unwrap();
        assert_eq!(failed.status, 5);
        assert_eq!(failed.body, br#"{"code":5,"message":"Bucket not found"}"#);
        // Exit 1 is a grpcurl or connection failure, not a gRPC status.
        assert!(grpc_response(Some(1), "", "dial failed").is_err());
        let raw = grpc_response(Some(69), "", "ERROR:\n  Code: NotFound\n  Message: x").unwrap();
        assert_eq!(raw.status, 5);
        assert_eq!(
            raw.body,
            br#"{"code":5,"raw":"ERROR:\n  Code: NotFound\n  Message: x"}"#
        );
        assert!(grpc_response(Some(0), "not json", "").is_err());
    }

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
    fn a_missing_body_file_is_an_empty_body_and_other_read_failures_are_errors() {
        let dir = std::env::temp_dir().join(format!(
            "gcsmimic-body-{}",
            crate::differential::random_hex()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("b0"), b"payload").unwrap();
        assert_eq!(read_body(&dir.join("b0")).unwrap(), b"payload");
        assert!(read_body(&dir.join("b1")).unwrap().is_empty());
        // A directory exists but cannot be read as a file.
        let error = read_body(&dir).unwrap_err();
        assert!(error.starts_with("read body: "), "{error}");
        crate::differential::server::remove_dir(&dir);
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
