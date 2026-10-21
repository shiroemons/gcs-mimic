use gcs_mimic::{
    clock::SystemClock,
    config::Config,
    server::{self, State},
    storage::Catalog,
};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    process::ExitCode,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const HELP: &str = "gcs-mimic — local Cloud Storage emulator foundation
Usage: gcs-mimic serve [--config PATH] [OPTIONS]
       gcs-mimic healthcheck --url http://HOST:PORT/_internal/ready
       gcs-mimic --help | --version

Serve options (CLI > GCS_MIMIC_<UPPER_SNAKE_CASE> environment > JSON config > default):
  --listen IP:PORT             127.0.0.1:4443
  --data-dir PATH              data (exclusive persistent catalog)
  --api-base-url URL           http://127.0.0.1:4443 (fixed advertised endpoint)
  --max-connections N          32 (1..1024)
  --max-object-bytes N         1073741824 (1..5497558138880; streamed upload cap)
  --worker-threads N           2 (1..32)
  --max-blocking-threads N     2 (1..32)
  --connection-timeout-ms N    5000 (1..60000; headers/body inactivity for objects)
  --object-request-timeout-ms N 900000 (1..86400000; absolute upload/media deadline)
  --catalog-wait-timeout-ms N  30000 (1..60000; upload advisory/commit wait)
  --shutdown-timeout-ms N      5000 (1..60000)
  GCS_MIMIC_CONFIG selects a JSON config file; --config overrides it.

HTTP/1 probes, JSON bucket CRUD and streaming JSON object media CRUD are available.
Bucket metadata is limited to 64 KiB. Multipart, resumable, TLS and authentication are unavailable.";

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
        [command, flag, url] if command == "healthcheck" && flag == "--url" => {
            Ok(healthcheck(url)?)
        }
        [command, rest @ ..] if command == "serve" => {
            let env: BTreeMap<_, _> = std::env::vars()
                .filter(|(key, _)| key.starts_with("GCS_MIMIC_"))
                .collect();
            let config = Config::load(rest, &env)?;
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
                let shutdown = async {
                    #[cfg(unix)]
                    tokio::select! { _ = interrupt.recv() => Ok(()), _ = terminate.recv() => Ok(()) }
                    #[cfg(not(unix))]
                    tokio::signal::ctrl_c().await
                };
                let clock = Arc::new(SystemClock);
                let catalog = Catalog::open_with_clock(&config.data_dir, clock.clone()).await.map_err(io::Error::other)?;
                let socket = if config.listen.is_ipv4() { tokio::net::TcpSocket::new_v4()? } else { tokio::net::TcpSocket::new_v6()? };
                socket.bind(config.listen)?;
                let listener = socket.listen(128)?;
                let state = Arc::new(State { ready: AtomicBool::new(true), clock, catalog, api_base_url: config.api_base_url.clone() });
                println!("{}", serde_json::json!({"event":"ready","listen":listener.local_addr()?.to_string(),"protocol":"http1","gcs_api":"json-bucket-object-media-crud","max_connections":config.max_connections,"max_object_bytes":config.max_object_bytes,"worker_threads":config.worker_threads,"max_blocking_threads":config.max_blocking_threads,"connection_timeout_ms":config.connection_timeout_ms,"object_request_timeout_ms":config.object_request_timeout_ms,"catalog_wait_timeout_ms":config.catalog_wait_timeout_ms,"shutdown_timeout_ms":config.shutdown_timeout_ms}));
                server::serve(listener, config, state, shutdown).await?;
                println!("{{\"event\":\"stopped\"}}");
                Ok::<_, io::Error>(())
            })?;
            Ok(())
        }
        _ => Err(format!("invalid command; {HELP}").into()),
    }
}

fn healthcheck(url: &str) -> io::Result<()> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected http://HOST:PORT/_internal/health or /_internal/ready",
        )
    };
    let (authority, path) = url
        .strip_prefix("http://")
        .and_then(|v| v.split_once('/'))
        .ok_or_else(invalid)?;
    if !matches!(path, "_internal/health" | "_internal/ready")
        || authority.contains(['@', '\r', '\n'])
    {
        return Err(invalid());
    }
    let addresses = authority.to_socket_addrs()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut connected = Err(invalid());
    for address in addresses.take(8) {
        connected = TcpStream::connect_timeout(&address, remaining(deadline)?);
        if connected.is_ok() {
            break;
        }
    }
    let mut stream = connected?;
    let request = format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    let mut pending = request.as_bytes();
    while !pending.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        let written = match stream.write(pending) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        pending = &pending[written..];
    }
    let mut response = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        let available = buffer.len().min(16_385 - response.len());
        let read = match stream.read(&mut buffer[..available]) {
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

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|time| !time.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "probe deadline exceeded"))
}
