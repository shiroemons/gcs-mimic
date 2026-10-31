//! One-shot live controls share the server's authorization and never open its catalog.

use gcs_mimic::{
    bounded_file,
    diagnostics::{DiagnosticSource, Diagnostics},
    tls,
};
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode, Uri, body::Bytes, client::conn::http1};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{collections::BTreeMap, io, io::Write, net::IpAddr, path::Path, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};

const RESPONSE_LIMIT: usize = 256 * 1024;
const HELP: &str = "Usage: gcs-mimic sync status|rescan|wait --endpoint http(s)://HOST:PORT [OPTIONS]
       gcs-mimic sync status --endpoint URL --view deletions|findings --revision REVISION [--cursor CURSOR] [--limit N]
       gcs-mimic sync approve --endpoint URL --deletion-set REVISION [OPTIONS]
       gcs-mimic sync conflicts --endpoint URL [--revision REVISION] [--cursor CURSOR] [--limit N]
       gcs-mimic sync resolve --endpoint URL --conflict ID --revision REV --action restore|discard --expected-generation N --operation-id ID
       gcs-mimic sync resolve --endpoint URL --recovery ID --revision REV --target-identity ID --action discard --operation-id ID --confirm-backup-verified ID

Options: --ca-cert PATH, --token-file PATH, --timeout 10s (1ms..60s).
The numeric --timeout-ms form also accepts 1..60000 (default 10000).
For wait, N is the server's fixed-fence observation budget; the client adds one
second to receive the result. Deadline (408) and stopped work (409) fail.
The endpoint must be a local HTTP(S) origin with an explicit port. HTTPS verifies
the embedded development CA or the supplied CA and the endpoint hostname.
The token file contains one local bearer token (at most 8192 bytes), overriding
the CLI-only GCS_MIMIC_SYNC_TOKEN environment variable when set.
Status and deletions print one bounded JSON page. Copy next_cursor unchanged;
approval requires the exact inspected revision, and stale revisions fail.
No data directory, server config, ambient proxy or Google credentials are opened.
Rescan and approval are not retried; a timeout may leave accepted work running.
Inspect status before retrying. Recovery discard requires exactly one matching
--confirm-backup-verified ID or --confirm-discard-without-backup ID.
Unsupported server operations fail visibly.";

struct Options {
    endpoint: Uri,
    path: String,
    body: Option<Value>,
    token: Option<hyper::header::HeaderValue>,
    ca_cert: Option<String>,
    timeout: Duration,
}

pub(super) fn run(args: &[String]) -> Result<(), super::CliError> {
    if args == ["--help"] {
        println!("{HELP}");
        return Ok(());
    }
    let mut options = Options::parse(args)?;
    if options.token.is_none() {
        options.token = std::env::var_os("GCS_MIMIC_SYNC_TOKEN")
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| invalid("GCS_MIMIC_SYNC_TOKEN must be UTF-8"))
                    .and_then(|value| token_value(&value))
            })
            .transpose()?;
    }
    let response = options.execute()?;
    let bytes = serde_json::to_vec(&response.1).map_err(io::Error::other)?;
    let mut output = io::stdout().lock();
    output
        .write_all(&bytes)
        .and_then(|()| output.write_all(b"\n"))?;
    if !response.0.is_success() {
        return Err(fail(format!(
            "Synchronization control returned HTTP {}; inspect its JSON response and preserve pending work.",
            response.0.as_u16()
        )).into());
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, fail(message))
}

fn fail(fix: impl Into<String>) -> Diagnostics {
    Diagnostics::error(DiagnosticSource::Cli, "sync", fix)
}

impl Options {
    fn parse(args: &[String]) -> io::Result<Self> {
        let (command, args) = args.split_first().ok_or_else(|| invalid(HELP))?;
        let mut command = command.as_str();
        let mut values = option_values(args)?;
        if command == "status"
            && let Some(view) = values.remove("--view")
        {
            command = match view {
                "deletions" | "findings" => view,
                _ => return Err(invalid("--view must be deletions or findings")),
            };
        }
        if command == "approve"
            && let Some(value) = values.remove("--deletion-set")
            && values.insert("--revision", value).is_some()
        {
            return Err(invalid("Supply only --deletion-set for approval"));
        }
        let endpoint = origin(
            values
                .remove("--endpoint")
                .ok_or_else(|| invalid("Supply --endpoint http(s)://HOST:PORT"))?,
        )?;
        let ca_cert = values.remove("--ca-cert").map(str::to_owned);
        if ca_cert.is_some() && endpoint.scheme_str() != Some("https") {
            return Err(invalid("--ca-cert requires an HTTPS endpoint"));
        }
        let duration = values.remove("--timeout");
        if duration.is_some() && values.contains_key("--timeout-ms") {
            return Err(invalid("Supply only one timeout option"));
        }
        let timeout = if let Some(duration) = duration {
            timeout_millis(duration)?
        } else {
            values
                .remove("--timeout-ms")
                .unwrap_or("10000")
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=60000).contains(n))
                .ok_or_else(|| invalid("--timeout-ms must be 1 through 60000"))?
        };
        let token = values.remove("--token-file").map(read_token).transpose()?;
        let revision = values.remove("--revision");
        if revision.is_some_and(|value| {
            value.len() != if command == "findings" { 48 } else { 32 }
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(invalid(
                "Use the exact lowercase inspected revision: 48 characters for findings, 32 for deletions/approval/conflicts",
            ));
        }
        let (path, body) = match command {
            "status" if revision.is_none() => ("/_internal/sync".to_owned(), None),
            "rescan" if revision.is_none() => {
                ("/_internal/sync/rescan".to_owned(), Some(json!({})))
            }
            "wait" if revision.is_none() => (
                "/_internal/sync/wait".to_owned(),
                Some(json!({"timeout_ms":timeout})),
            ),
            "resolve" => (
                "/_internal/sync/resolve".to_owned(),
                Some(resolve_body(revision, &mut values)?),
            ),
            "approve" => (
                "/_internal/sync/approve".to_owned(),
                Some(
                    json!({"deletion_set":revision.ok_or_else(|| invalid("Approval requires --revision from an inspected deletion set"))?}),
                ),
            ),
            "deletions" | "findings" | "conflicts" => {
                (page_path(command, revision, &mut values)?, None)
            }
            _ => {
                return Err(invalid(
                    "Use sync status, findings, deletions, conflicts, rescan, approve, resolve or wait with their documented options",
                ));
            }
        };
        if !values.is_empty() {
            return Err(invalid(
                "This sync operation does not accept the remaining options; see sync --help",
            ));
        }
        Ok(Self {
            endpoint,
            path,
            body,
            token,
            ca_cert,
            timeout: Duration::from_millis(timeout + if command == "wait" { 1000 } else { 0 }),
        })
    }

    fn execute(&self) -> io::Result<(StatusCode, Value)> {
        let client = match (self.endpoint.scheme_str(), &self.ca_cert) {
            (Some("https"), Some(path)) => Some(tls::client_config(Path::new(path))?),
            (Some("https"), None) => Some(tls::development_client_config()?),
            _ => None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(async {
            tokio::time::timeout(self.timeout, async {
                let authority = self
                    .endpoint
                    .authority()
                    .ok_or_else(|| invalid("Missing endpoint authority"))?;
                let addresses = tokio::net::lookup_host(authority.as_str()).await?;
                let mut checked = Vec::new();
                for address in addresses {
                    if !local_address(address.ip()) || checked.len() == 8 {
                        return Err(invalid(
                            "Endpoint must resolve only to local addresses, at most eight",
                        ));
                    }
                    checked.push(address);
                }
                let mut connected = Err(invalid("Endpoint has no local address"));
                for address in checked {
                    connected = tokio::net::TcpStream::connect(address).await;
                    if connected.is_ok() {
                        break;
                    }
                }
                let stream = connected?;
                if let Some(client) = client {
                    let host = self
                        .endpoint
                        .host()
                        .ok_or_else(|| invalid("Missing endpoint hostname"))?
                        .trim_matches(['[', ']']);
                    let name =
                        tokio_rustls::rustls::pki_types::ServerName::try_from(host.to_owned())
                            .map_err(|_| invalid("Invalid TLS hostname"))?;
                    let stream = tokio_rustls::TlsConnector::from(client)
                        .connect(name, stream)
                        .await?;
                    self.exchange(stream).await
                } else {
                    self.exchange(stream).await
                }
            })
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "Synchronization request timed out")
            })?
        });
        // A timed-out resolver must not keep this one-shot command alive.
        runtime.shutdown_background();
        result
    }

    async fn exchange(
        &self,
        stream: impl AsyncRead + AsyncWrite + Unpin,
    ) -> io::Result<(StatusCode, Value)> {
        let (mut sender, connection) = http1::Builder::new()
            .max_headers(64)
            .max_buf_size(16 * 1024)
            .handshake(TokioIo::new(stream))
            .await
            .map_err(io::Error::other)?;
        let mut request = Request::builder()
            .method(if self.body.is_some() { "POST" } else { "GET" })
            .uri(&self.path)
            .header(
                "Host",
                self.endpoint
                    .authority()
                    .ok_or_else(|| invalid("Missing endpoint authority"))?
                    .as_str(),
            )
            .header("Connection", "close")
            .header("Accept", "application/json");
        if let Some(token) = &self.token {
            request = request.header("Authorization", token);
        }
        let body = match &self.body {
            Some(body) => {
                request = request.header("Content-Type", "application/json");
                serde_json::to_vec(body).map_err(io::Error::other)?
            }
            None => Vec::new(),
        };
        let request = request
            .body(Full::new(Bytes::from(body)))
            .map_err(io::Error::other)?;
        let response = async {
            let response = sender
                .send_request(request)
                .await
                .map_err(io::Error::other)?;
            let status = response.status();
            let mut body = response.into_body();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.map_err(io::Error::other)?.into_data() {
                    if data.len() > RESPONSE_LIMIT - bytes.len() {
                        return Err(invalid("Synchronization response exceeds 256 KiB"));
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            Ok((
                status,
                serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            ))
        };
        // Driving both futures locally avoids an unobserved connection task.
        let (_, response) = tokio::try_join!(
            async { connection.await.map_err(io::Error::other) },
            response
        )?;
        Ok(response)
    }
}

fn option_values(args: &[String]) -> io::Result<BTreeMap<&str, &str>> {
    let mut values = BTreeMap::new();
    for pair in args.chunks(2) {
        if pair.len() != 2
            || !matches!(
                pair[0].as_str(),
                "--endpoint"
                    | "--ca-cert"
                    | "--token-file"
                    | "--timeout-ms"
                    | "--revision"
                    | "--cursor"
                    | "--limit"
                    | "--conflict"
                    | "--recovery"
                    | "--action"
                    | "--expected-generation"
                    | "--target-identity"
                    | "--operation-id"
                    | "--confirm-backup-verified"
                    | "--confirm-discard-without-backup"
                    | "--deletion-set"
                    | "--view"
                    | "--timeout"
            )
            || pair[1].is_empty()
            || pair[1].len() > 8192
            || values.insert(pair[0].as_str(), pair[1].as_str()).is_some()
        {
            return Err(invalid(
                "Use unique sync option/value pairs; see sync --help",
            ));
        }
    }
    Ok(values)
}

fn page_path(
    command: &str,
    revision: Option<&str>,
    values: &mut BTreeMap<&str, &str>,
) -> io::Result<String> {
    let mut query = Vec::new();
    let path = if command == "conflicts" {
        "/_internal/sync/conflicts"
    } else {
        query.push(format!("view={command}"));
        if revision.is_none() {
            return Err(invalid("Pages require the matching --revision from status"));
        }
        "/_internal/sync"
    };
    if let Some(revision) = revision {
        query.push(format!("revision={revision}"));
    }
    if let Some(cursor) = values.remove("--cursor") {
        query.push(format!(
            "cursor={}",
            gcs_mimic::auth::signature::encode_query_component(cursor)
        ));
    }
    if let Some(limit) = values.remove("--limit") {
        let limit = limit
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=256).contains(n))
            .ok_or_else(|| invalid("--limit must be 1 through 256"))?;
        query.push(format!("limit={limit}"));
    }
    Ok(if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", query.join("&"))
    })
}

fn timeout_millis(value: &str) -> io::Result<u64> {
    let (number, factor) = value
        .strip_suffix("ms")
        .map(|v| (v, 1))
        .or_else(|| value.strip_suffix('s').map(|v| (v, 1000)))
        .ok_or_else(|| invalid("--timeout requires an integer with ms or s suffix"))?;
    number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(factor))
        .filter(|n| (1..=60000).contains(n))
        .ok_or_else(|| invalid("--timeout must be 1ms through 60s"))
}

fn resolve_body(revision: Option<&str>, values: &mut BTreeMap<&str, &str>) -> io::Result<Value> {
    let revision = revision.ok_or_else(|| invalid("Resolve requires the inspected --revision"))?;
    let action = values
        .remove("--action")
        .filter(|v| matches!(*v, "restore" | "discard"))
        .ok_or_else(|| invalid("Resolve requires --action restore or discard"))?;
    let operation = values.remove("--operation-id").filter(|v| v.len() <= 128 && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
        .ok_or_else(|| invalid("Resolve requires --operation-id of at most 128 letters, digits, hyphens or underscores"))?;
    let hex = |v: &&str| {
        v.len() == 32
            && v.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    match (values.remove("--conflict"), values.remove("--recovery")) {
        (Some(id), None) if hex(&id) => {
            let generation = values
                .remove("--expected-generation")
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|n| *n >= 0)
                .ok_or_else(|| {
                    invalid(
                        "Conflict resolution requires --expected-generation N; zero means absent",
                    )
                })?;
            Ok(
                json!({"conflict_id":id,"revision":revision,"action":action,"expected_generation":generation,"operation_id":operation}),
            )
        }
        (None, Some(id)) if hex(&id) && action == "discard" => {
            let identity = values
                .remove("--target-identity")
                .filter(hex)
                .ok_or_else(|| {
                    invalid("Recovery discard requires its inspected --target-identity")
                })?;
            let confirmation = match (
                values.remove("--confirm-backup-verified"),
                values.remove("--confirm-discard-without-backup"),
            ) {
                (Some(value), None) if value == id => "backup_verified",
                (None, Some(value)) if value == id => "discard_without_backup",
                _ => {
                    return Err(invalid(
                        "Recovery discard requires exactly one confirmation whose ID equals --recovery",
                    ));
                }
            };
            Ok(
                json!({"recovery_id":id,"revision":revision,"target_identity":identity,"action":action,"confirmation":confirmation,"operation_id":operation}),
            )
        }
        _ => Err(invalid(
            "Select exactly one --conflict or --recovery; recovery permits only explicit discard",
        )),
    }
}

fn origin(value: &str) -> io::Result<Uri> {
    let uri: Uri = value
        .parse()
        .map_err(|_| invalid("Invalid HTTP(S) endpoint origin"))?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.port_u16().is_none_or(|port| port == 0)
        || uri.host().is_none()
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || uri.path() != "/"
        || uri.query().is_some()
        || value.contains('#')
    {
        return Err(invalid(
            "Supply a bare HTTP(S) origin with an explicit nonzero port",
        ));
    }
    Ok(uri)
}

fn local_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or_else(
            || ip.is_loopback() || ip.is_unique_local(),
            |ip| local_address(IpAddr::V4(ip)),
        ),
    }
}

fn read_token(path: &str) -> io::Result<hyper::header::HeaderValue> {
    let bytes = bounded_file::read(Path::new(path), 8192).map_err(|error| match error {
        bounded_file::ReadError::Io(error) => error,
        bounded_file::ReadError::Limit => invalid("Token file exceeds 8192 bytes"),
    })?;
    let token = std::str::from_utf8(&bytes)
        .map_err(|_| invalid("Token file must contain UTF-8"))?
        .trim_end_matches(['\r', '\n']);
    token_value(token)
}

fn token_value(token: &str) -> io::Result<hyper::header::HeaderValue> {
    if token.is_empty() || token.len() > 8192 || !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(invalid("Token file must contain one nonempty bearer token"));
    }
    hyper::header::HeaderValue::from_str(&format!("Bearer {token}")).map_err(io::Error::other)
}

#[cfg(test)]
mod tests;
