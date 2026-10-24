use gcs_mimic::{
    auth::{Auth, Registry},
    clock::{Clock, SystemClock},
    config::Config,
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    tls,
};
use std::{collections::BTreeMap, io, path::Path, process::ExitCode, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
mod startup;
use startup::phase;

const HELP: &str = "gcs-mimic — local Cloud Storage emulator foundation
Usage: gcs-mimic serve [--config PATH] [OPTIONS]
       gcs-mimic healthcheck --url URL [--ca-cert PATH]
       gcs-mimic tls ca
       gcs-mimic url public --base-url URL --bucket NAME --object NAME
       gcs-mimic url sign --base-url URL --bucket NAME --object NAME --method GET --expires 15m --signer ID --private-key PATH
       gcs-mimic --help | --version

Serve options (CLI > GCS_MIMIC_<UPPER_SNAKE_CASE> environment > JSON config > default):
  --listen IP:PORT             127.0.0.1:4443
  --scheme MODE                https (http|https; one listener)
  --tls-cert PATH              embedded development certificate (custom PEM chain; at most 1 MiB)
  --tls-key PATH               embedded public test key (custom PEM private key; at most 1 MiB)
  --tls-handshake-timeout-ms N  5000 (1..60000; HTTPS only)
  --data-dir PATH              data (exclusive persistent catalog)
  --seed-dir PATH              none (read-only fixtures; fresh data directory only)
  --seed-project ID            local-project (owning project for seeded buckets)
  --api-base-url URL           <scheme>://127.0.0.1:4443 (fixed advertised endpoint)
  --public-base-url URL        none (public origin; XML host routing and explicit-config URL helpers)
  --xml-virtual-host-domains LIST none (comma-separated XML base DNS names; at most 32)
  --max-connections N          32 (1..1024)
  --max-object-bytes N         1073741824 (1..5497558138880; streamed upload cap)
  --worker-threads N           2 (1..32)
  --max-blocking-threads N     2 (1..32)
  --connection-timeout-ms N    5000 (1..60000; headers/body inactivity for objects)
  --object-request-timeout-ms N 900000 (1..86400000; absolute upload/media deadline)
  --catalog-wait-timeout-ms N  30000 (1..60000; upload advisory/commit wait)
  --shutdown-timeout-ms N      5000 (1..60000)
  --auth-mode MODE             permissive (permissive|strict; strict needs --identity-file)
  --compat-profile PROFILE     native (native|legacy-emulator; independent of authentication)
  --identity-file PATH         none (strict-mode JSON identity registry, at most 64 KiB)
  GCS_MIMIC_CONFIG selects a JSON config file; --config overrides it.

HTTP/1 probes and scoped JSON/XML APIs are available over HTTP or TLS 1.2/1.3.
HTTPS defaults to public local-development credentials for localhost, gcs and loopback IPs.
Supply both --tls-cert and --tls-key to replace them. Export the root with gcs-mimic tls ca.
Clients must trust the issuing CA and match a certificate SAN; public test keys provide no
identity protection outside isolated local development. Healthcheck HTTPS trusts the embedded
development CA by default; --ca-cert PATH replaces that trust with a supplied CA bundle.
HTTP healthcheck rejects --ca-cert. Probes accept /_internal/health or /_internal/ready.
Authentication is permissive by default; strict mode checks local bearer tokens and grants.";

const URL_PUBLIC_HELP: &str =
    "Usage: gcs-mimic url public --base-url URL --bucket NAME --object NAME [--style path|virtual-hosted]
       gcs-mimic url public --config PATH --bucket NAME --object NAME [--style path|virtual-hosted]

Prints <base-url>/<bucket>/<encoded object> for the XML-style path endpoint. Bucket and object
are required exactly once, in any order. The name is percent-encoded except for RFC 3986
unreserved characters and '/'; if any '/'-separated segment is '.' or '..', '/' is encoded as %2F.
Style defaults to path; virtual-hosted uses <bucket>.<base-domain>/<object>, without a base path.
Enable the DNS base in --xml-virtual-host-domains; HTTPS needs a matching supplied certificate.
No config file is read unless --config PATH is explicit; its public_base_url supplies the default
base, overridden by --base-url. No environment, data directory or network is read, and no permission
is changed.";

const URL_SIGN_HELP: &str =
    "Usage: gcs-mimic url sign --base-url URL --bucket NAME --object NAME --method GET|HEAD|PUT
       --expires DURATION --signer ID --private-key PATH [--header NAME:VALUE]...
       [--query NAME=VALUE]... [--timestamp YYYYMMDDTHHMMSSZ] [--style path|virtual-hosted]

Prints a V4 RSA signed URL for the final HTTP(S) endpoint, retaining non-default ports.
The base may come from --base-url URL or an explicit --config PATH containing public_base_url.
Default HTTP port 80 and HTTPS port 443 are omitted before signing, as browsers send Host.
Duration is integer seconds, or an integer with s/m/h/d suffix, from 1 second through 7 days.
Private keys must be local RSA PKCS#8 or PKCS#1 PEM files (at most 16 KiB).
Headers may repeat; Host is derived from the final URL and cannot be overridden.
Query names and values are literal, encoded before signing; duplicate names are rejected.
Style defaults to path; virtual-hosted prepends the bucket to a DNS base with no path prefix.
Enable that base in --xml-virtual-host-domains; HTTPS requires a matching supplied certificate.
No config file is read unless --config PATH is explicit; its public_base_url supplies the default
base, overridden by --base-url. No environment, Google IAM call or permission change occurs.";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let nested = error
                .downcast_ref::<io::Error>()
                .and_then(|e| e.get_ref())
                .and_then(|e| e.downcast_ref::<Diagnostics>());
            let fallback = Diagnostics::error(
                DiagnosticSource::Runtime,
                "runtime",
                "Command failed; check --help and the configured local resources.",
            );
            let diagnostics = error
                .downcast_ref::<Diagnostics>()
                .or(nested)
                .unwrap_or(&fallback);
            let _ = diagnostics.write(io::stderr().lock());
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .take(2054)
        .map(|arg| {
            arg.into_string().map_err(|_| {
                Diagnostics::error(
                    DiagnosticSource::Cli,
                    "input",
                    "Options and values must be valid UTF-8; correct the supplied argument.",
                )
            })
        })
        .collect::<Result<_, _>>()?;
    if args.len() == 2054 {
        return Err(Diagnostics::error(
            DiagnosticSource::Cli,
            "input_limit",
            "Configuration accepts at most 1024 supplied settings; remove excess arguments.",
        )
        .into());
    }
    if args.first().is_none_or(|s| s != "serve") {
        let diagnostics = gcs_mimic::config_input::cli_diagnostics(&args);
        if diagnostics.is_err() {
            return Err(diagnostics.into());
        }
    }
    match args.as_slice() {
        [help] if help == "--help" => {
            println!("{HELP}");
            Ok(())
        }
        [version] if version == "--version" => {
            println!("gcs-mimic {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        [command, help] if command == "serve" && help == "--help" => {
            println!("{HELP}");
            Ok(())
        }
        [command, rest @ ..] if command == "healthcheck" => {
            healthcheck(rest).map_err(|error| Diagnostics::error(DiagnosticSource::Cli, "--url",
                if error.kind() == io::ErrorKind::TimedOut { "probe deadline exceeded; check the local endpoint and CA trust." }
                else { "Healthcheck failed; require HTTP 200 from a complete local probe response, and verify the URL and CA trust (including 403 access denial)." }))?;
            Ok(())
        }
        [command, kind] if command == "tls" && kind == "ca" => {
            print!("{}", tls::DEVELOPMENT_CA_PEM);
            Ok(())
        }
        [command, kind, help] if command == "url" && kind == "public" && help == "--help" => {
            println!("{URL_PUBLIC_HELP}");
            Ok(())
        }
        [command, kind, rest @ ..] if command == "url" && kind == "public" => {
            let configured = helper_public_base(rest)?;
            println!(
                "{}",
                url_public(rest, configured).map_err(|fix| Diagnostics::error(
                    DiagnosticSource::Cli,
                    "url",
                    fix
                ))?
            );
            Ok(())
        }
        [command, kind, help] if command == "url" && kind == "sign" && help == "--help" => {
            println!("{URL_SIGN_HELP}");
            Ok(())
        }
        [command, kind, rest @ ..] if command == "url" && kind == "sign" => {
            let configured = helper_public_base(rest)?;
            println!(
                "{}",
                url_sign(rest, configured).map_err(|fix| Diagnostics::error(
                    DiagnosticSource::Cli,
                    "url",
                    fix
                ))?
            );
            Ok(())
        }
        [command, rest @ ..] if command == "serve" => {
            let mut report = Config::preflight_os(rest, std::env::vars_os());
            // Validate supplied TLS material before touching the catalog, seed or listener.
            let mut acceptor = None;
            if report.valid(&["scheme", "tls_cert", "tls_key"]) {
                match tls::load_server(&report.config) {
                    Ok(value) => acceptor = value,
                    Err(_) => report.diagnostics.errors.push(Diagnostic::new(DiagnosticSource::Tls, "tls_cert",
                        "Cannot load TLS identity; provide readable matching PEM tls_cert/tls_key files, each at most 1 MiB, with one private key and a certificate chain.")),
                }
            }
            // Load the registry before binding so a bad identity file never reaches readiness.
            let mut auth = Auth::permissive();
            if report.valid(&["auth_mode", "identity_file"])
                && let Some(path) = &report.config.identity_file
            {
                match Registry::load(path) {
                    Ok(registry) => auth = Auth::strict(registry),
                    Err(_) => report.diagnostics.errors.push(Diagnostic::new(DiagnosticSource::Identity, "identity_file",
                        "Cannot load identity_file; provide a readable valid registry at most 64 KiB with unique ids/tokens, supported project_roles and valid RSA public_keys; remove unknown fields and private keys.")),
                }
            }
            if report.valid(&["data_dir", "seed_dir"]) {
                report
                    .diagnostics
                    .warnings
                    .extend(gcs_mimic::config_input::data_warning(
                        &report.config,
                        Path::new("/data"),
                    ));
            }
            if report.diagnostics.is_err() {
                return Err(report.diagnostics.into());
            }
            report.diagnostics.write(io::stderr().lock())?;
            let config = report.config;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(config.worker_threads)
                .max_blocking_threads(config.max_blocking_threads)
                .enable_all()
                .build().map_err(|_| phase(DiagnosticSource::Runtime, "runtime", "Cannot create runtime; check worker limits and available system resources."))?;
            runtime.block_on(async move {
                #[cfg(unix)]
                let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|_| phase(DiagnosticSource::Runtime, "signal", "Cannot register termination signal; check the local runtime environment."))?;
                #[cfg(unix)]
                let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).map_err(|_| phase(DiagnosticSource::Runtime, "signal", "Cannot register interrupt signal; check the local runtime environment."))?;
                #[cfg(windows)]
                let mut terminate = tokio::signal::windows::ctrl_break().map_err(|_| phase(DiagnosticSource::Runtime, "signal", "Cannot register termination signal; check the local runtime environment."))?;
                #[cfg(windows)]
                let mut interrupt = tokio::signal::windows::ctrl_c().map_err(|_| phase(DiagnosticSource::Runtime, "signal", "Cannot register interrupt signal; check the local runtime environment."))?;
                let shutdown = async {
                    #[cfg(any(unix, windows))]
                    tokio::select! { _ = interrupt.recv() => Ok(()), _ = terminate.recv() => Ok(()) }
                    #[cfg(not(any(unix, windows)))]
                    tokio::signal::ctrl_c().await
                };
                startup::serve(config, auth, acceptor, shutdown).await
            })?;
            Ok(())
        }
        _ => Err(Diagnostics::error(
            DiagnosticSource::Cli,
            "command",
            "Invalid command; use gcs-mimic --help for supported native commands.",
        )
        .into()),
    }
}

fn helper_public_base(args: &[String]) -> Result<Option<String>, Diagnostics> {
    let file = args
        .as_chunks::<2>()
        .0
        .iter()
        .find(|pair| pair[0] == "--config");
    match file {
        Some(pair) => {
            gcs_mimic::config::read_public_base_url(&pair[1]).map_err(|error| Diagnostics {
                errors: vec![error],
                ..Default::default()
            })
        }
        None => Ok(None),
    }
}

fn url_public(args: &[String], configured: Option<String>) -> Result<String, String> {
    let mut options = BTreeMap::new();
    for pair in args.chunks(2) {
        let [name, value] = pair else {
            return Err(format!(
                "url public options require --name VALUE pairs\n{URL_PUBLIC_HELP}"
            ));
        };
        if !matches!(
            name.as_str(),
            "--base-url" | "--config" | "--bucket" | "--object" | "--style"
        ) {
            return Err(format!(
                "unknown url public option: {name}\n{URL_PUBLIC_HELP}"
            ));
        }
        if options.insert(name.as_str(), value.as_str()).is_some() {
            return Err(format!("duplicate option: {name}"));
        }
    }
    let required = |name| {
        options
            .get(name)
            .copied()
            .ok_or_else(|| format!("missing required option: {name}\n{URL_PUBLIC_HELP}"))
    };
    let build = match options.get("--style").copied().unwrap_or("path") {
        "path" => gcs_mimic::url::public_object_url,
        "virtual-hosted" => gcs_mimic::url::virtual_host_object_url,
        _ => return Err("style must be path or virtual-hosted".into()),
    };
    build(
        &url_base(&options, configured)?,
        required("--bucket")?,
        required("--object")?,
    )
}

fn url_base(options: &BTreeMap<&str, &str>, configured: Option<String>) -> Result<String, String> {
    options.get("--base-url").map(|base| (*base).to_owned()).or(configured)
        .ok_or_else(|| "missing required option: --base-url; provide --base-url URL or --config PATH containing public_base_url.".into())
}

fn url_sign(args: &[String], configured: Option<String>) -> Result<String, String> {
    use std::io::Read;
    let mut options = BTreeMap::new();
    let mut headers = hyper::HeaderMap::new();
    let mut query = Vec::new();
    for pair in args.chunks(2) {
        let [name, value] = pair else {
            return Err(format!(
                "url sign options require --name VALUE pairs\n{URL_SIGN_HELP}"
            ));
        };
        if name == "--header" {
            let (key, value) = value.split_once(':').ok_or("header must be NAME:VALUE")?;
            let key = hyper::header::HeaderName::from_bytes(key.as_bytes())
                .map_err(|_| "invalid header name")?;
            headers.append(key, value.parse().map_err(|_| "invalid header value")?);
        } else if name == "--query" {
            let (key, value) = value.split_once('=').ok_or("query must be NAME=VALUE")?;
            if key.is_empty() {
                return Err("query name must not be empty".into());
            }
            query.push(format!(
                "{}={}",
                gcs_mimic::auth::signature::encode_query_component(key),
                gcs_mimic::auth::signature::encode_query_component(value)
            ));
        } else {
            if !matches!(
                name.as_str(),
                "--base-url"
                    | "--config"
                    | "--bucket"
                    | "--object"
                    | "--method"
                    | "--expires"
                    | "--signer"
                    | "--private-key"
                    | "--timestamp"
                    | "--style"
            ) {
                return Err(format!("unknown url sign option: {name}"));
            }
            if options.insert(name.as_str(), value.as_str()).is_some() {
                return Err(format!("duplicate option: {name}"));
            }
        }
    }
    let required = |name| {
        options
            .get(name)
            .copied()
            .ok_or_else(|| format!("missing required option: {name}\n{URL_SIGN_HELP}"))
    };
    let build = match options.get("--style").copied().unwrap_or("path") {
        "path" => gcs_mimic::url::public_object_url,
        "virtual-hosted" => gcs_mimic::url::virtual_host_object_url,
        _ => return Err("style must be path or virtual-hosted".into()),
    };
    let mut url = build(
        &url_base(&options, configured)?,
        required("--bucket")?,
        required("--object")?,
    )?;
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    let method = required("--method")?
        .parse()
        .map_err(|_| "invalid signing method")?;
    let duration = required("--expires")?;
    let (digits, multiplier) = match duration.as_bytes().last() {
        Some(b's') => (&duration[..duration.len() - 1], 1),
        Some(b'm') => (&duration[..duration.len() - 1], 60),
        Some(b'h') => (&duration[..duration.len() - 1], 3600),
        Some(b'd') => (&duration[..duration.len() - 1], 86400),
        _ => (duration, 1),
    };
    let expires = digits
        .parse::<u64>()
        .ok()
        .filter(|_| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.checked_mul(multiplier))
        .ok_or("invalid signing duration")?;
    let now = options
        .get("--timestamp")
        .map(|s| gcs_mimic::auth::signature::parse_timestamp(s))
        .transpose()?
        .unwrap_or_else(|| SystemClock.now());
    let mut key = Vec::new();
    std::fs::File::open(required("--private-key")?)
        .map_err(|_| "Cannot read private key; provide a readable local RSA PEM file.")?
        .take(16385)
        .read_to_end(&mut key)
        .map_err(|_| "Cannot read private key; provide a readable local RSA PEM file.")?;
    gcs_mimic::auth::signature::sign_url(
        &url,
        &method,
        required("--signer")?,
        &key,
        expires,
        now,
        headers,
    )
}

fn healthcheck(args: &[String]) -> io::Result<()> {
    let mut options = BTreeMap::new();
    for pair in args.chunks(2) {
        if pair.len() != 2 || !matches!(pair[0].as_str(), "--url" | "--ca-cert") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "healthcheck requires --url URL [--ca-cert PATH]",
            ));
        }
        if pair[1].is_empty() || options.insert(pair[0].as_str(), pair[1].as_str()).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty or duplicate healthcheck option",
            ));
        }
    }
    let url = options.get("--url").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "healthcheck requires --url URL",
        )
    })?;
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected http(s)://HOST:PORT/_internal/health or /_internal/ready",
        )
    };
    let uri: hyper::Uri = url.parse().map_err(|_| invalid())?;
    let authority = uri.authority().ok_or_else(invalid)?.as_str();
    let path = uri.path().strip_prefix('/').ok_or_else(invalid)?;
    if !matches!(path, "_internal/health" | "_internal/ready")
        || !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.port_u16().is_none()
        || uri.query().is_some()
        || url.contains('#')
        || authority.contains(['@', '\r', '\n'])
    {
        return Err(invalid());
    }
    let client = match (uri.scheme_str(), options.get("--ca-cert")) {
        (Some("https"), Some(path)) => Some(tls::client_config(Path::new(path))?),
        (Some("https"), None) => Some(tls::development_client_config()?),
        (_, Some(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--ca-cert requires an HTTPS healthcheck URL",
            ));
        }
        _ => None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), async {
            let addresses = tokio::net::lookup_host(authority).await?;
            let mut connected = Err(invalid());
            for address in addresses.take(8) {
                connected = tokio::net::TcpStream::connect(address).await;
                if connected.is_ok() {
                    break;
                }
            }
            let stream = connected?;
            if let Some(client) = client {
                let host = uri
                    .host()
                    .ok_or_else(invalid)?
                    .trim_start_matches('[')
                    .trim_end_matches(']');
                let name = tokio_rustls::rustls::pki_types::ServerName::try_from(host.to_owned())
                    .map_err(|_| invalid())?;
                let stream = tokio_rustls::TlsConnector::from(client)
                    .connect(name, stream)
                    .await?;
                probe_response(stream, authority, path).await
            } else {
                probe_response(stream, authority, path).await
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "probe deadline exceeded"))?
    });
    // DNS resolution uses blocking workers; a timed-out one-shot probe must not wait for them.
    runtime.shutdown_background();
    result
}

async fn probe_response(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    authority: &str,
    path: &str,
) -> io::Result<()> {
    let request = format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    let mut response = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        let available = buffer.len().min(16_385 - response.len());
        let read = match stream.read(&mut buffer[..available]).await {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
        if response.len() > 16_384 {
            return Err(io::Error::other("probe response exceeds 16 KiB"));
        }
    }
    let boundary = response
        .windows(4)
        .position(|v| v == b"\r\n\r\n")
        .ok_or_else(|| io::Error::other("incomplete probe response"))?;
    let headers = std::str::from_utf8(&response[..boundary])
        .map_err(|_| io::Error::other("invalid probe headers"))?;
    let mut lines = headers.split("\r\n");
    let status = lines.next().unwrap_or_default();
    if !status.starts_with("HTTP/1.1 200 ") && !status.starts_with("HTTP/1.0 200 ") {
        return Err(io::Error::other("probe did not return HTTP 200"));
    }
    let mut length = None;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("invalid probe header"))?;
        let name =
            hyper::header::HeaderName::from_bytes(name.as_bytes()).map_err(io::Error::other)?;
        hyper::header::HeaderValue::from_str(value).map_err(io::Error::other)?;
        if name == hyper::header::TRANSFER_ENCODING {
            return Err(io::Error::other(
                "chunked probe responses are not supported",
            ));
        }
        if name == hyper::header::CONTENT_LENGTH {
            if length.is_some() {
                return Err(io::Error::other("duplicate probe content length"));
            }
            length = Some(value.trim().parse::<usize>().map_err(io::Error::other)?);
        }
    }
    if length.is_some_and(|n| n != response.len() - boundary - 4) {
        return Err(io::Error::other("incomplete probe body"));
    }
    Ok(())
}
