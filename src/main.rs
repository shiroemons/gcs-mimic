use gcs_mimic::{
    auth::{Auth, Registry},
    clock::{Clock, SystemClock},
    config::Config,
    server::{self, State},
    storage::Catalog,
    tls,
};
use std::{
    collections::BTreeMap,
    io,
    net::SocketAddr,
    path::Path,
    process::ExitCode,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

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
    "Usage: gcs-mimic url public --base-url URL --bucket NAME --object NAME

Prints <base-url>/<bucket>/<encoded object> for the XML-style path endpoint. Every option is
required exactly once, in any order. The object name is percent-encoded except for RFC 3986
unreserved characters and '/'; if any '/'-separated segment is '.' or '..', '/' is encoded as %2F.
No config file, environment, data directory or network is read, and no permission is changed.";

const URL_SIGN_HELP: &str =
    "Usage: gcs-mimic url sign --base-url URL --bucket NAME --object NAME --method GET|HEAD|PUT
       --expires DURATION --signer ID --private-key PATH [--header NAME:VALUE]...
       [--timestamp YYYYMMDDTHHMMSSZ]

Prints a V4 RSA signed URL for the final HTTP(S) endpoint, retaining non-default ports.
Default HTTP port 80 and HTTPS port 443 are omitted before signing, as browsers send Host.
Duration is integer seconds, or an integer with s/m/h/d suffix, from 1 second through 7 days.
Private keys must be local RSA PKCS#8 or PKCS#1 PEM files (at most 16 KiB).
Headers may repeat; Host is derived from the final URL and cannot be overridden.
No Google IAM call, server configuration read or permission change occurs.";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
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
        [command, rest @ ..] if command == "healthcheck" => Ok(healthcheck(rest)?),
        [command, kind] if command == "tls" && kind == "ca" => {
            print!("{}", tls::DEVELOPMENT_CA_PEM);
            Ok(())
        }
        [command, kind, help] if command == "url" && kind == "public" && help == "--help" => {
            println!("{URL_PUBLIC_HELP}");
            Ok(())
        }
        [command, kind, rest @ ..] if command == "url" && kind == "public" => {
            println!("{}", url_public(rest)?);
            Ok(())
        }
        [command, kind, help] if command == "url" && kind == "sign" && help == "--help" => {
            println!("{URL_SIGN_HELP}");
            Ok(())
        }
        [command, kind, rest @ ..] if command == "url" && kind == "sign" => {
            println!("{}", url_sign(rest)?);
            Ok(())
        }
        [command, rest @ ..] if command == "serve" => {
            let env: BTreeMap<_, _> = std::env::vars()
                .filter(|(key, _)| key.starts_with("GCS_MIMIC_"))
                .collect();
            let config = Config::load(rest, &env)?;
            // Validate supplied TLS material before touching the catalog, seed or listener.
            let acceptor = tls::load_server(&config)?;
            // Load the registry before binding so a bad identity file never reaches readiness.
            let auth = match &config.identity_file {
                Some(path) => Auth::strict(Registry::load(path)?),
                None => Auth::permissive(),
            };
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(config.worker_threads)
                .max_blocking_threads(config.max_blocking_threads)
                .enable_all()
                .build()?;
            runtime.block_on(async move {
                #[cfg(unix)]
                let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
                #[cfg(unix)]
                let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
                #[cfg(windows)]
                let mut terminate = tokio::signal::windows::ctrl_break()?;
                #[cfg(windows)]
                let mut interrupt = tokio::signal::windows::ctrl_c()?;
                let shutdown = async {
                    #[cfg(any(unix, windows))]
                    tokio::select! { _ = interrupt.recv() => Ok(()), _ = terminate.recv() => Ok(()) }
                    #[cfg(not(any(unix, windows)))]
                    tokio::signal::ctrl_c().await
                };
                let clock = Arc::new(SystemClock);
                let catalog = Catalog::open_with_clock(&config.data_dir, clock.clone()).await.map_err(io::Error::other)?;
                let seed = match gcs_mimic::seed::import(&catalog, &config, clock.as_ref()).await {
                    Ok(seed) => seed,
                    Err(error) => {
                        catalog.close().await.map_err(io::Error::other)?;
                        return Err(error);
                    }
                };
                tokio::pin!(shutdown);
                let listener = tokio::select! {
                    biased;
                    result = &mut shutdown => {
                        catalog.close().await.map_err(io::Error::other)?;
                        result?;
                        println!("{{\"event\":\"stopped\"}}");
                        return Ok(());
                    }
                    listener = bind_listener(config.listen) => listener?,
                };
                catalog.start_resumable_maintenance();
                let state = Arc::new(State { ready: AtomicBool::new(true), clock, catalog, api_base_url: config.api_base_url.clone(), auth });
                let mut ready = serde_json::json!({"event":"ready","listen":listener.local_addr()?.to_string(),"protocol":"http1","gcs_api":"json-bucket-object-media-crud","max_connections":config.max_connections,"max_object_bytes":config.max_object_bytes,"worker_threads":config.worker_threads,"max_blocking_threads":config.max_blocking_threads,"connection_timeout_ms":config.connection_timeout_ms,"object_request_timeout_ms":config.object_request_timeout_ms,"catalog_wait_timeout_ms":config.catalog_wait_timeout_ms,"shutdown_timeout_ms":config.shutdown_timeout_ms,"auth_mode":state.auth.mode().as_str()});
                if let Some(count) = state.auth.identity_count() {
                    ready["identities"] = count.into();
                }
                ready["seed"] = seed;
                ready["scheme"] = config.scheme.as_str().into();
                ready["tls_handshake_timeout_ms"] = config.tls_handshake_timeout_ms.into();
                println!("{ready}");
                server::serve_with_tls(listener, config, state, shutdown, acceptor).await?;
                println!("{{\"event\":\"stopped\"}}");
                Ok::<_, io::Error>(())
            })?;
            Ok(())
        }
        _ => Err(format!("invalid command; {HELP}").into()),
    }
}

async fn bind_listener(address: SocketAddr) -> io::Result<tokio::net::TcpListener> {
    let socket = if address.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    // Linux socket(7) excludes live listeners; BSD address reuse can steal wildcard traffic.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    socket.set_reuseaddr(true)?;
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    if let Err(error) = socket.bind(address) {
        bind_after_time_wait(&socket, address, error).await?;
    }
    #[cfg(not(all(unix, not(any(target_os = "linux", target_os = "android")))))]
    socket.bind(address)?;
    socket.listen(128)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
async fn bind_after_time_wait(
    socket: &tokio::net::TcpSocket,
    address: SocketAddr,
    original: io::Error,
) -> io::Result<()> {
    if original.kind() != io::ErrorKind::AddrInUse {
        return Err(original);
    }
    // Wait out TIME_WAIT without weakening exclusive binding or waiting indefinitely.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(65);
    let mut probe = address;
    if address.ip().is_unspecified() {
        probe.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    let mut logged = false;
    loop {
        let connected = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(250)),
            tokio::net::TcpStream::connect(probe),
        )
        .await;
        if matches!(connected, Ok(Ok(_))) || tokio::time::Instant::now() >= deadline {
            return Err(original);
        }
        if !logged {
            eprintln!(
                "{}",
                serde_json::json!({"event":"bind_wait","listen":address.to_string(),"timeout_ms":65000,"reason":"address_in_use"})
            );
            logged = true;
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(250)),
        )
        .await;
        if tokio::time::Instant::now() >= deadline {
            return Err(original);
        }
        match socket.bind(address) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
            Err(error) => return Err(error),
        }
    }
}

fn url_public(args: &[String]) -> Result<String, String> {
    let mut options = BTreeMap::new();
    for pair in args.chunks(2) {
        let [name, value] = pair else {
            return Err(format!(
                "url public options require --name VALUE pairs\n{URL_PUBLIC_HELP}"
            ));
        };
        if !matches!(name.as_str(), "--base-url" | "--bucket" | "--object") {
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
    gcs_mimic::url::public_object_url(
        required("--base-url")?,
        required("--bucket")?,
        required("--object")?,
    )
}

fn url_sign(args: &[String]) -> Result<String, String> {
    use std::io::Read;
    let mut options = BTreeMap::new();
    let mut headers = hyper::HeaderMap::new();
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
        } else {
            if !matches!(
                name.as_str(),
                "--base-url"
                    | "--bucket"
                    | "--object"
                    | "--method"
                    | "--expires"
                    | "--signer"
                    | "--private-key"
                    | "--timestamp"
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
    let url = gcs_mimic::url::public_object_url(
        required("--base-url")?,
        required("--bucket")?,
        required("--object")?,
    )?;
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
        .map_err(|e| format!("private key: {e}"))?
        .take(16385)
        .read_to_end(&mut key)
        .map_err(|e| format!("private key: {e}"))?;
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
