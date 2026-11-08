use gcs_mimic::{
    auth::Auth,
    clock::{Clock, SystemClock},
    config::Config,
    config_input::{Fix as UrlFix, MAX_CLI_ARGS, UrlStyle, option_pairs},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    tls,
};
use std::{collections::BTreeMap, io, path::Path, process::ExitCode, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
mod startup;
mod sync_cli;
// Binary tests cannot access the library's cfg(test) module.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "The binary uses a subset of the shared test fixtures"
)]
mod test_support;
use startup::phase;

// musl's allocator takes one global lock: with it the container served about 12 000 object reads
// per second whether 8 or 128 connections asked, against 66 000 to 189 000 with this one. Other
// targets keep their C library's allocator, which already scales.
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// With huge pages set to `always`, as in Docker Desktop's VM, one write into the allocator's
/// regions makes 2 MiB resident: the process held 15 MiB idle and 78 MiB after a load, against
/// 8 MiB and 32 MiB without them, at the same speed. Failing only costs that memory, so the result
/// is not checked.
#[cfg(target_env = "musl")]
#[expect(
    unsafe_code,
    reason = "std has no wrapper for prctl; the call passes only integers"
)]
fn disable_transparent_huge_pages() {
    const DISABLE: libc::c_ulong = 1;
    const UNUSED: libc::c_ulong = 0;
    // SAFETY: this option reads no memory and changes only this process's paging policy. The
    // kernel takes the variadic arguments as unsigned longs and rejects unused ones that are not
    // zero, so they are passed at exactly that width.
    let _ = unsafe { libc::prctl(libc::PR_SET_THP_DISABLE, DISABLE, UNUSED, UNUSED, UNUSED) };
}

const HELP: &str = "gcs-mimic — local Cloud Storage emulator foundation
Usage: gcs-mimic serve [--config PATH] [OPTIONS]
       gcs-mimic sync status|findings|deletions|conflicts|rescan|approve|resolve|wait --endpoint URL [OPTIONS]
       gcs-mimic healthcheck [--url URL] [--ca-cert PATH]
       gcs-mimic import fake-gcs-server --source DIR --data-dir NEW [OPTIONS] (see import fake-gcs-server --help)
       gcs-mimic tls ca
       gcs-mimic url public --base-url URL --bucket NAME --object NAME
       gcs-mimic url sign --base-url URL --bucket NAME --object NAME --method GET --expires 15m [--signer ID --private-key PATH]
       gcs-mimic --help | --version

Serve options (CLI > GCS_MIMIC_<UPPER_SNAKE_CASE> environment > JSON config > default):
  --default-location US|EU|ASIA  Native bucket creation default (US); explicit locations win
  --default-soft-delete-retention-seconds N  0 (new buckets without a softDeletePolicy; 0 keeps no deleted data, 1..3155760000 retains it; 604800 is the Cloud Storage default)
  --listen IP:PORT             127.0.0.1:4443
  --log-level LEVEL            info (debug|info|warn|error; warning aliases warn)
  --scheme MODE                https (http|https|both; both uses HTTPS on --listen)
  --listen-http IP:PORT        primary listen IP:8000 (HTTP listener; scheme=both only)
  --grpc-listen IP:PORT        none (optional separate plaintext h2c gRPC listener; e.g. 127.0.0.1:4444)
  --unix-socket PATH           none (Unix HTTP socket; private 0700 parent, scheme=http, no TCP/gRPC)
  --tls-cert PATH              embedded development certificate (custom PEM chain; at most 1 MiB)
  --tls-key PATH               embedded public test key (custom PEM private key; at most 1 MiB)
  --tls-handshake-timeout-ms N  5000 (0 disables; HTTPS listener)
  --data-dir PATH              data (exclusive persistent catalog)
  --backend MODE               disk (disk|memory; memory never opens data-dir, loses data at exit)
  --memory-max-bytes N          67108864 (RAM blob bytes plus chunk overhead; 1..1099511627776)
  --memory-catalog-max-bytes N  16777216 (RAM SQLite pages; 1048576..1099511627776; separate from RSS)
  --seed-dir PATH              none (read-only fixtures; replayed on every startup)
  --default-seed-dir PATH      none (seed root when seed_dir is unset and PATH exists; image: /data)
  --seed-project ID            local-project (owning project for seeded buckets)
  --max-seed-entries N         0 (files and directories in seed_dir; memory grows with them; 0 is unlimited)
  --max-seed-depth N           0 (0..1024; directory levels below a seed bucket; 0 is unlimited)
  --seed-content-type MODE     extension (extension|octet-stream; pinned MIME table)
  --sync-dir PATH              none (existing working tree; initial export after ready)
  --sync-delete-guard BOOL     true (deletion ingestion pending)
  --sync-reinitialize ID       startup only (exact catalog ID; missing marker only)
  --max-sync-entries N         0 (files and directories in sync_dir, also the watches Linux may use; scans take longer, not more memory; 0 is unlimited)
  --max-sync-depth N           0 (0..1024; directory levels below sync_dir, counting the bucket; 0 is unlimited)
  --buckets-file PATH          none (create missing buckets before seed; JSON v1, at most 1 GiB, or 1 KiB per allowed bucket and at least 32 MiB under a bucket limit)
  --max-buckets-per-batch N    0 (buckets one --buckets-file or one seed may declare, not all buckets; memory grows with them; 0 is unlimited)
  --api-base-url URL           Each request's Host, else the bound listener (set to fix the advertised endpoint)
  --public-base-url URL        none (public origin; XML host routing and explicit-config URL helpers)
  --admin-enabled BOOL         false (local management API; isolate the listener when enabled)
  --admin-principal ID         none (strict instance administrator for local management except sync; required with --admin-enabled in strict mode)
  --admin-allowed-hosts LIST   none (comma-separated DNS names allowed to reach /_internal management; at most 32)
  --local-creator-email EMAIL local-user@gcs-mimic.invalid (permissive native: creator recorded as owner and ACL OWNER of new objects; empty disables; grants nothing)
  --xml-virtual-host-domains LIST none (comma-separated XML base DNS names; at most 32)
  --max-connections N          1024 (1..1048576; shared across listeners and TLS handshakes; raises the soft open-file limit to match)
  --max-grpc-streams N         100 (1..65536; concurrent streams per gRPC connection; each can buffer a request message of up to 4 MiB and its encoded response, so lower it to limit memory; requires --grpc-listen)
  --max-object-bytes N         5497558138880 (1..5497558138880; streamed upload cap)
  --max-resumable-sessions N   0 (0..4294967295; active resumable uploads; 0 is unlimited)
  --max-rewrite-sessions N     0 (0..4294967295; unfinished rewrites; 0 is unlimited)
  --worker-threads N           available CPUs from 2 to 8 (1..1024)
  --max-blocking-threads N     8 (1..1024)
  --header-read-timeout-ms N   30000 (0 disables; request head arrival, gRPC handshake and HTTP/2 ping acknowledgement)
  --body-inactivity-timeout-ms N 0 (0 disables; silence while a request body, response body or gRPC stream is in flight)
  --request-timeout-ms N       180000 (0 disables; handler time before a response starts, excluding request body waits)
  --connection-timeout-ms N    none (alias; sets the three settings above, and each of them named explicitly wins)
  --keep-alive-timeout-ms N    120000 (0 disables; idle time between HTTP/1.1 requests and on gRPC connections)
  --object-request-timeout-ms N 0 (0 disables; absolute upload/media deadline)
  --catalog-wait-timeout-ms N  120000 (0 disables; upload admission, advisory and commit waits, a busy upload session, and reset, reseed and seed waits)
  --shutdown-timeout-ms N      5000 (0 disables; waits for unfinished requests without a limit)
  --auth-mode MODE             permissive (permissive|strict; strict needs --identity-file)
  --compat-profile PROFILE     native (native|legacy-emulator; independent of authentication)
  --cors-fallback MODE         none (none|allow-all; XML/public CORS for buckets without stored rules)
  --signed-host-port MODE      exact (exact|lenient; lenient also accepts a V4 signature whose host omits the request's port, as Go storage signs)
  --identity-file PATH         none (JSON identity registry, at most 16 MiB; strict mode, or only id and public_keys with --signed-urls verify)
  --signed-urls MODE           ignore when permissive, verify when strict (ignore|verify; permissive verify checks signed XML requests, then grants permissive access)
  --test-signer MODE           enabled when permissive, disabled when strict (enabled|disabled; built-in test-signer@gcs-mimic.invalid, whose private key is published; strict enabling warns; local tests only)
  --pubsub-emulator-host HOST:PORT none (explicit local HTTP Pub/Sub emulator; PUBSUB_EMULATOR_HOST also accepted)
  --event-pubsub-project-id ID  none (global notification topic project; needs local emulator)
  --event-pubsub-topic ID       none (global notification topic name)
  --event-bucket NAME           all buckets (global notification filter)
  --event-object-prefix PREFIX all object names (global notification filter)
  --event-list LIST            finalize (comma-separated finalize,delete,metadataUpdate,archive; none disables the global rule)
  GCS_MIMIC_CONFIG selects a JSON config file; --config overrides it.

HTTP/1 probes and scoped JSON/XML APIs are available over HTTP or TLS 1.2/1.3.
HTTPS defaults to public local-development credentials for localhost, gcs and loopback IPs.
Supply both --tls-cert and --tls-key to replace them. Export the root with gcs-mimic tls ca.
Clients must trust the issuing CA and match a certificate SAN; public test keys provide no
identity protection outside isolated local development. Healthcheck HTTPS trusts the embedded
development CA by default; --ca-cert PATH replaces that trust with a supplied CA bundle.
HTTP healthcheck rejects --ca-cert. Probes accept /_internal/health, /_internal/ready
or the compatible readiness path /_internal/healthcheck.
Without --url, healthcheck reads GCS_MIMIC_* and the selected JSON config, uses ready,
maps wildcard listeners to loopback and prefers HTTP for scheme=both. Server CLI
overrides require an explicit --url or matching environment/configuration settings.
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
    "Usage: gcs-mimic url sign --base-url URL --bucket NAME --object NAME --method GET|HEAD|PUT|DELETE
       --expires DURATION [--signer ID --private-key PATH] [--header NAME:VALUE]...
       [--query NAME=VALUE]... [--timestamp YYYYMMDDTHHMMSSZ] [--style path|virtual-hosted]

Prints a V4 RSA signed URL for the final HTTP(S) endpoint, retaining non-default ports.
Without --signer and --private-key it signs as the built-in test-signer@gcs-mimic.invalid,
whose private key is published: such URLs authenticate nothing outside local tests.
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
    #[cfg(target_env = "musl")]
    disable_transparent_huge_pages();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let fallback = Diagnostics::error(
                DiagnosticSource::Runtime,
                "runtime",
                "Command failed; check --help and the configured local resources.",
            );
            let diagnostics = error.diagnostics().unwrap_or(&fallback);
            let _ = diagnostics.write(io::stderr().lock());
            ExitCode::FAILURE
        }
    }
}

/// A command failure. Only its diagnostics reach the user; any other failure is reported as a
/// generic runtime diagnostic so internal error text never replaces actionable advice.
#[derive(Debug)]
enum CliError {
    Diagnostics(Diagnostics),
    /// A failure that may carry diagnostics as its inner error, as startup phases do.
    Io(io::Error),
    Import(gcs_mimic::import::ImportError),
}

impl CliError {
    fn diagnostics(&self) -> Option<&Diagnostics> {
        match self {
            Self::Diagnostics(diagnostics) => Some(diagnostics),
            Self::Import(gcs_mimic::import::ImportError::Diagnostics(diagnostics)) => {
                Some(diagnostics)
            }
            Self::Import(_) => None,
            Self::Io(error) => error.get_ref().and_then(|inner| {
                inner.downcast_ref::<Diagnostics>().or_else(|| {
                    inner
                        .downcast_ref::<startup::CatalogAuthenticationFailure>()
                        .map(startup::CatalogAuthenticationFailure::diagnostics)
                })
            }),
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Diagnostics(diagnostics) => diagnostics.fmt(f),
            Self::Io(error) => error.fmt(f),
            Self::Import(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Diagnostics(diagnostics) => Some(diagnostics),
            Self::Io(error) => Some(error),
            Self::Import(error) => Some(error),
        }
    }
}

impl From<Diagnostics> for CliError {
    fn from(diagnostics: Diagnostics) -> Self {
        Self::Diagnostics(diagnostics)
    }
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<gcs_mimic::import::ImportError> for CliError {
    fn from(error: gcs_mimic::import::ImportError) -> Self {
        Self::Import(error)
    }
}

fn cli_args() -> Result<Vec<String>, Diagnostics> {
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .take(MAX_CLI_ARGS)
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
    if args.len() == MAX_CLI_ARGS {
        return Err(Diagnostics::error(
            DiagnosticSource::Cli,
            "input_limit",
            "Configuration accepts at most 1024 supplied settings; remove excess arguments.",
        ));
    }
    Ok(args)
}

fn run() -> Result<(), CliError> {
    let args = cli_args()?;
    match args.as_slice() {
        [command, rest @ ..] if command == "sync" => {
            return sync_cli::run(rest);
        }
        [command, kind, rest @ ..] if command == "import" && kind == "fake-gcs-server" => {
            return gcs_mimic::import::cli(rest).map_err(CliError::from);
        }
        _ => (),
    }
    if gcs_mimic::config_input::starts_with_legacy_flag(&args) {
        return serve(&args);
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
            healthcheck(rest)?;
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
            url_command(rest, url_public)
        }
        [command, kind, help] if command == "url" && kind == "sign" && help == "--help" => {
            println!("{URL_SIGN_HELP}");
            Ok(())
        }
        [command, kind, rest @ ..] if command == "url" && kind == "sign" => {
            url_command(rest, url_sign)
        }
        [command, rest @ ..] if command == "serve" => serve(rest),
        _ => Err(Diagnostics::error(
            DiagnosticSource::Cli,
            "command",
            "Invalid command; use gcs-mimic --help for supported native commands.",
        )
        .into()),
    }
}

/// Prints the URL a `url` helper builds, after reading an explicit `--config` base.
fn url_command(
    args: &[String],
    build: fn(&[String], Option<String>) -> Result<String, UrlFix>,
) -> Result<(), CliError> {
    let configured = helper_public_base(args)?;
    let url = build(args, configured)
        .map_err(|UrlFix(fix)| Diagnostics::error(DiagnosticSource::Cli, "url", fix))?;
    println!("{url}");
    Ok(())
}

fn serve(rest: &[String]) -> Result<(), CliError> {
    let mut report = Config::preflight_os(rest, std::env::vars_os());
    // Validate supplied TLS material before touching the catalog, seed or listener.
    let mut acceptor = None;
    if report.valid(&["scheme", "tls_cert", "tls_key"]) {
        match tls::load_server(&report.config) {
            Ok(value) => acceptor = value,
            Err(error) => report.diagnostics.errors.push(Diagnostic::new(
                DiagnosticSource::Tls,
                error.setting(),
                format!("Cannot load TLS identity: {error}; {}", error.repair()),
            )),
        }
    }
    // Load the registry before binding so a bad identity file never reaches readiness.
    let mut auth = Auth::permissive();
    if report.valid(&["auth_mode", "identity_file", "test_signer", "signed_urls"]) {
        match Auth::load(&report.config) {
            Ok(loaded) => auth = loaded,
            Err(error) => report.diagnostics.errors.push(Diagnostic::new(
                DiagnosticSource::Identity,
                "identity_file",
                error.to_string(),
            )),
        }
    }
    if report.valid(&["data_dir", "seed_dir", "default_seed_dir", "sync_dir"]) {
        let warnings = gcs_mimic::config_input::resolve_default_seed(&mut report.config);
        report.diagnostics.warnings.extend(warnings);
    }
    if report.valid(&["admin_principal"])
        && let (Some(principal), Some(principals)) =
            (&report.config.admin_principal, auth.principals())
        && !principals.iter().any(|identity| identity.id() == principal)
    {
        report.diagnostics.errors.push(Diagnostic::new(DiagnosticSource::Identity, "admin_principal",
            format!("admin_principal {principal:?} is not an id in identity_file; use one of its identities.")));
    }
    // The legacy profile (fake-gcs-server compatible) keeps project-owner objects.
    if report.config.compat_profile == gcs_mimic::config::CompatProfile::Native {
        auth = auth.with_local_creator_email(&report.config.local_creator_email);
    }
    if report.diagnostics.is_err() {
        return Err(report.diagnostics.into());
    }
    auth = auth
        .with_verifier_budget(report.config.max_blocking_threads)
        .with_signed_host_port(report.config.signed_host_port);
    report
        .diagnostics
        .warnings
        .extend(gcs_mimic::fd_limit::raise_for_connections(
            report.config.max_connections,
            report
                .config
                .grpc_listen
                .map(|_| report.config.max_grpc_streams),
        ));
    report.diagnostics.write(io::stderr().lock())?;
    let config = report.config;
    gcs_mimic::diagnostics::set_log_level(config.log_level);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.worker_threads)
        .max_blocking_threads(config.max_blocking_threads)
        .enable_all()
        .build()
        .map_err(|_| {
            phase(
                DiagnosticSource::Runtime,
                "runtime",
                "Cannot create runtime; check worker limits and available system resources.",
            )
        })?;
    runtime.block_on(async move {
        #[cfg(unix)]
        let mut terminate = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        )
        .map_err(|_| {
            phase(
                DiagnosticSource::Runtime,
                "signal",
                "Cannot register termination signal; check the local runtime environment.",
            )
        })?;
        #[cfg(unix)]
        let mut interrupt = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::interrupt(),
        )
        .map_err(|_| {
            phase(
                DiagnosticSource::Runtime,
                "signal",
                "Cannot register interrupt signal; check the local runtime environment.",
            )
        })?;
        #[cfg(windows)]
        let mut terminate = tokio::signal::windows::ctrl_break().map_err(|_| {
            phase(
                DiagnosticSource::Runtime,
                "signal",
                "Cannot register termination signal; check the local runtime environment.",
            )
        })?;
        #[cfg(windows)]
        let mut interrupt = tokio::signal::windows::ctrl_c().map_err(|_| {
            phase(
                DiagnosticSource::Runtime,
                "signal",
                "Cannot register interrupt signal; check the local runtime environment.",
            )
        })?;
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

fn helper_public_base(args: &[String]) -> Result<Option<String>, Diagnostics> {
    let file = option_pairs(args)
        .filter_map(Result::ok)
        .find(|(name, _)| *name == "--config");
    match file {
        Some((_, value)) => {
            gcs_mimic::config::read_public_base_url(value).map_err(|error| Diagnostics {
                errors: vec![error],
                ..Default::default()
            })
        }
        None => Ok(None),
    }
}

fn url_public(args: &[String], configured: Option<String>) -> Result<String, UrlFix> {
    let mut options = BTreeMap::new();
    for pair in option_pairs(args) {
        let (name, value) = pair.map_err(|_| {
            format!("url public options require --name VALUE pairs\n{URL_PUBLIC_HELP}")
        })?;
        if !matches!(
            name,
            "--base-url" | "--config" | "--bucket" | "--object" | "--style"
        ) {
            return Err(format!("unknown url public option: {name}\n{URL_PUBLIC_HELP}").into());
        }
        if options.insert(name, value).is_some() {
            return Err(format!("duplicate option: {name}").into());
        }
    }
    let required = |name| {
        options
            .get(name)
            .copied()
            .ok_or_else(|| format!("missing required option: {name}\n{URL_PUBLIC_HELP}"))
    };
    let build = match options.get("--style").copied().unwrap_or("path").parse()? {
        UrlStyle::Path => gcs_mimic::url::public_object_url,
        UrlStyle::VirtualHosted => gcs_mimic::url::virtual_host_object_url,
    };
    build(
        &url_base(&options, configured)?,
        required("--bucket")?,
        required("--object")?,
    )
    .map_err(|error| error.to_string().into())
}

fn url_base(options: &BTreeMap<&str, &str>, configured: Option<String>) -> Result<String, UrlFix> {
    options.get("--base-url").map(|base| (*base).to_owned()).or(configured)
        .ok_or_else(|| "missing required option: --base-url; provide --base-url URL or --config PATH containing public_base_url.".into())
}

fn url_sign(args: &[String], configured: Option<String>) -> Result<String, UrlFix> {
    let mut options = BTreeMap::new();
    let mut headers = hyper::HeaderMap::new();
    let mut query = Vec::new();
    for pair in option_pairs(args) {
        let (name, value) = pair
            .map_err(|_| format!("url sign options require --name VALUE pairs\n{URL_SIGN_HELP}"))?;
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
                name,
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
                return Err(format!("unknown url sign option: {name}").into());
            }
            if options.insert(name, value).is_some() {
                return Err(format!("duplicate option: {name}").into());
            }
        }
    }
    let required = |name| {
        options
            .get(name)
            .copied()
            .ok_or_else(|| format!("missing required option: {name}\n{URL_SIGN_HELP}"))
    };
    let build = match options.get("--style").copied().unwrap_or("path").parse()? {
        UrlStyle::Path => gcs_mimic::url::public_object_url,
        UrlStyle::VirtualHosted => gcs_mimic::url::virtual_host_object_url,
    };
    let mut url = build(
        &url_base(&options, configured)?,
        required("--bucket")?,
        required("--object")?,
    )
    .map_err(|error| error.to_string())?;
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    let method = required("--method")?
        .parse()
        .map_err(|_| "invalid signing method")?;
    let duration = required("--expires")?;
    let expires = gcs_mimic::config_input::duration(
        duration,
        &[("s", 1), ("m", 60), ("h", 3600), ("d", 86400), ("", 1)],
    )
    .ok()
    .filter(|_| duration.as_bytes().first().is_some_and(u8::is_ascii_digit))
    .ok_or("invalid signing duration")?;
    let now = options
        .get("--timestamp")
        .map(|s| gcs_mimic::auth::signature::parse_timestamp(s))
        .transpose()
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| SystemClock.now());
    let (signer, key) = signing_credentials(&options)?;
    gcs_mimic::auth::signature::sign_url(gcs_mimic::auth::signature::SignUrlRequest {
        url: &url,
        method: &method,
        signer,
        private_key: &key,
        expires,
        now,
        headers,
    })
    .map_err(|error| error.to_string().into())
}

/// The explicit signer and its key file, or the built-in test signer when neither is given. A
/// signer without its key, or a key without its signer, would sign as someone unintended.
fn signing_credentials<'a>(
    options: &BTreeMap<&str, &'a str>,
) -> Result<(&'a str, std::borrow::Cow<'static, [u8]>), UrlFix> {
    use gcs_mimic::auth::test_signer;
    match (options.get("--signer"), options.get("--private-key")) {
        (None, None) => Ok((
            test_signer::ID,
            std::borrow::Cow::Borrowed(test_signer::PRIVATE_KEY_PEM),
        )),
        (Some(signer), Some(path)) => {
            let key = gcs_mimic::bounded_file::read(Path::new(path), 16_384).map_err(|error| {
                match error {
                    gcs_mimic::bounded_file::ReadError::Limit => {
                        "Private key exceeds 16 KiB; provide a bounded RSA PEM file."
                    }
                    gcs_mimic::bounded_file::ReadError::Io(_) => {
                        "Cannot read private key; provide a readable local RSA PEM file."
                    }
                }
            })?;
            Ok((signer, std::borrow::Cow::Owned(key)))
        }
        (Some(_), None) | (None, Some(_)) => Err(format!(
            "--signer and --private-key must be given together; omit both to sign as the built-in test signer {}\n{URL_SIGN_HELP}",
            test_signer::ID
        )
        .into()),
    }
}

fn healthcheck(args: &[String]) -> Result<(), Diagnostics> {
    let repair = |url: &str| {
        format!(
            "Verify the server settings and retry with healthcheck --url {url}{}.",
            if url.starts_with("https://") {
                " --ca-cert ./tls/ca.pem (use the issuing CA for custom TLS; omit --ca-cert for the embedded development CA)"
            } else {
                ""
            }
        )
    };
    let failed = |url: &str, error: io::Error| {
        Diagnostics::error(
            DiagnosticSource::Cli,
            "healthcheck",
            format!(
                "Healthcheck {url} failed: {error}; require HTTP 200 and verify CA trust (including 403 access denial). {}",
                repair(url)
            ),
        )
    };
    let explicit_url = option_pairs(args)
        .filter_map(Result::ok)
        .find_map(|(name, value)| (name == "--url").then(|| value.to_owned()));
    if let Some(url) = explicit_url {
        return probe(args, &url).map_err(|error| failed(&url, error));
    }
    let url = probe_url(&[], &repair)?;
    let Err(error) = probe(args, &url) else {
        return Ok(());
    };
    // Only a failed probe consults PID 1: Rosetta emulates /proc/<pid>/cmdline with a temporary
    // file, which crashes the probe on a read-only root, so ordinary setups never read it.
    let server_args = container_server_args();
    if server_args.is_empty() {
        return Err(failed(&url, error));
    }
    let fallback = probe_url(&server_args, &repair)?;
    if fallback == url {
        return Err(failed(&url, error));
    }
    probe(args, &fallback).map_err(|error| failed(&fallback, error))
}

/// The probe URL for the environment, JSON file and the given server options.
fn probe_url(
    server_args: &[String],
    repair: &dyn Fn(&str) -> String,
) -> Result<String, Diagnostics> {
    let mut report = Config::preflight_os(server_args, std::env::vars_os());
    let url = report.config.healthcheck_url();
    if report.diagnostics.is_err() {
        report.diagnostics.errors.push(Diagnostic::new(
            DiagnosticSource::Cli,
            "healthcheck",
            format!(
                "Cannot derive a reliable probe URL (candidate {url}); fix configuration. {}",
                repair(&url)
            ),
        ));
        return Err(report.diagnostics);
    }
    Ok(url)
}

/// The server options of a gcs-mimic process running as PID 1, as in the container image.
/// A Compose `command` such as fake-gcs-server's `-scheme http` reaches only that process, so
/// the healthcheck falls back to them when the environment's defaults do not answer.
fn container_server_args() -> Vec<String> {
    let Ok(bytes) = std::fs::read("/proc/1/cmdline") else {
        return Vec::new();
    };
    let bytes = bytes.strip_suffix(b"\0").unwrap_or(&bytes);
    let Ok(mut args) = bytes
        .split(|byte| *byte == 0)
        .map(|arg| String::from_utf8(arg.to_vec()))
        .collect::<Result<Vec<_>, _>>()
    else {
        return Vec::new();
    };
    if args.is_empty() || Path::new(&args.remove(0)).file_name() != Some("gcs-mimic".as_ref()) {
        return Vec::new();
    }
    if args.first().is_some_and(|arg| arg == "serve") {
        args.remove(0);
        args
    } else if gcs_mimic::config_input::starts_with_legacy_flag(&args) {
        args
    } else {
        Vec::new()
    }
}

fn probe(args: &[String], url: &str) -> io::Result<()> {
    let mut options = BTreeMap::new();
    let syntax = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "healthcheck accepts [--url URL] [--ca-cert PATH]",
        )
    };
    for pair in option_pairs(args) {
        let (name, value) = pair.map_err(|_| syntax())?;
        if !matches!(name, "--url" | "--ca-cert") {
            return Err(syntax());
        }
        if value.is_empty() || options.insert(name, value).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty or duplicate healthcheck option",
            ));
        }
    }
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected http(s)://HOST:PORT/_internal/health, /_internal/ready or /_internal/healthcheck",
        )
    };
    let uri: hyper::Uri = url.parse().map_err(|_| invalid())?;
    let authority = uri.authority().ok_or_else(invalid)?.as_str();
    let path = uri.path().strip_prefix('/').ok_or_else(invalid)?;
    if !matches!(
        path,
        "_internal/health" | "_internal/ready" | "_internal/healthcheck"
    ) || !matches!(uri.scheme_str(), Some("http" | "https"))
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
        let read = match stream
            .read(
                buffer
                    .get_mut(..available)
                    .ok_or(io::ErrorKind::InvalidData)?,
            )
            .await
        {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if read == 0 {
            break;
        }
        response.extend_from_slice(buffer.get(..read).ok_or(io::ErrorKind::InvalidData)?);
        if response.len() > 16_384 {
            return Err(io::Error::other("probe response exceeds 16 KiB"));
        }
    }
    let boundary = response
        .windows(4)
        .position(|v| v == b"\r\n\r\n")
        .ok_or_else(|| io::Error::other("incomplete probe response"))?;
    let headers = std::str::from_utf8(response.get(..boundary).ok_or(io::ErrorKind::InvalidData)?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_durations_preserve_the_cli_grammar() {
        for (duration, accepted) in [
            ("+5s", false),
            ("5ms", false),
            ("", false),
            ("s", false),
            ("18446744073709551615m", false),
            ("300", true),
            ("5d", true),
        ] {
            let args = [
                "--base-url",
                "https://localhost:4443",
                "--bucket",
                "example-bucket",
                "--object",
                "body",
                "--method",
                "GET",
                "--expires",
                duration,
                "--signer",
                "local-viewer@example.invalid",
                "--private-key",
                "fixtures/signing/test-only-private.pem",
                "--timestamp",
                "20261023T120000Z",
            ]
            .map(str::to_owned);
            let result = url_sign(&args, None);
            assert_eq!(result.is_ok(), accepted, "{duration}: {result:?}");
            if !accepted {
                assert_eq!(result.unwrap_err().0, "invalid signing duration");
            }
        }
    }

    #[test]
    fn signing_defaults_to_the_built_in_test_signer_only_without_both_credentials() {
        let base = [
            "--base-url",
            "http://localhost:4443",
            "--bucket",
            "example-bucket",
            "--object",
            "body",
            "--method",
            "GET",
            "--expires",
            "60",
            "--timestamp",
            "20261023T120000Z",
        ]
        .map(str::to_owned);
        let url = url_sign(&base, None).unwrap();
        assert!(
            url.contains("X-Goog-Credential=test-signer%40gcs-mimic.invalid%2F20261023%2F"),
            "{url}"
        );
        let mut explicit = base.to_vec();
        explicit.extend(
            [
                "--signer",
                "test-signer@gcs-mimic.invalid",
                "--private-key",
                "fixtures/signing/test-only-private.pem",
            ]
            .map(str::to_owned),
        );
        // RSA PKCS#1 v1.5 is deterministic, so the same key and input give the same URL.
        assert_eq!(url_sign(&explicit, None).unwrap(), url);
        for extra in [
            ["--signer", "test-signer@gcs-mimic.invalid"],
            ["--private-key", "fixtures/signing/test-only-private.pem"],
        ] {
            let mut args = base.to_vec();
            args.extend(extra.map(str::to_owned));
            let error = url_sign(&args, None).unwrap_err().0;
            assert!(
                error.starts_with("--signer and --private-key must be given together"),
                "{error}"
            );
        }
    }

    #[test]
    fn help_output_keeps_its_exact_bytes() {
        let digest = ring::digest::digest(&ring::digest::SHA256, format!("{HELP}\n").as_bytes());
        let digest = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            digest,
            "8074968fe820071349f455100bece40106c43b2204a2651addaafc6672f1fe09"
        );
    }

    #[test]
    fn helper_parsers_preserve_the_first_rejected_pair_and_its_diagnostic() {
        let args = ["--unknown", "value", "--trailing"].map(str::to_owned);
        assert_eq!(
            url_public(&args, None).unwrap_err().0,
            format!("unknown url public option: --unknown\n{URL_PUBLIC_HELP}")
        );
        assert_eq!(
            url_sign(&args, None).unwrap_err().0,
            "unknown url sign option: --unknown"
        );
        let args = ["--bucket"].map(str::to_owned);
        assert_eq!(
            url_public(&args, None).unwrap_err().0,
            format!("url public options require --name VALUE pairs\n{URL_PUBLIC_HELP}")
        );
        assert_eq!(
            url_sign(&args, None).unwrap_err().0,
            format!("url sign options require --name VALUE pairs\n{URL_SIGN_HELP}")
        );
    }
}
