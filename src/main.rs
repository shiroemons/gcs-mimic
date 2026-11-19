use gcs_mimic::{
    clock::{Clock, SystemClock},
    config::{Config, help},
    config_input::{Fix as UrlFix, MAX_CLI_ARGS, UrlStyle, option_pairs},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    tls,
};
use std::{
    collections::{BTreeMap, btree_map},
    io,
    path::Path,
    process::ExitCode,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
mod buckets_cli;
mod startup;
mod sync_cli;
// Binary tests cannot access the library's cfg(test) module.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "The binary uses a subset of the shared test fixtures"
)]
mod test_support;

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
    "Usage: gcs-mimic url sign --base-url URL --bucket NAME --object NAME --method GET|HEAD|PUT|DELETE|POST
       --expires DURATION [--signer ID --private-key PATH] [--header NAME:VALUE]...
       [--query NAME=VALUE]... [--timestamp YYYYMMDDTHHMMSSZ] [--style path|virtual-hosted]

Prints a V4 RSA signed URL for the final HTTP(S) endpoint, retaining non-default ports.
Without --signer and --private-key it signs as the built-in test-signer@gcs-mimic.invalid,
whose private key is published: such URLs authenticate nothing outside local tests.
The base may come from --base-url URL or an explicit --config PATH containing public_base_url.
Default HTTP port 80 and HTTPS port 443 are omitted before signing, as browsers send Host.
Duration is integer seconds, or an integer with s/m/h/d suffix, from 1 second through 7 days.
Private keys must be local RSA PKCS#8 or PKCS#1 PEM files (at most 16 KiB).
POST signs only a resumable upload start, so it needs --header x-goog-resumable:start.
Headers may repeat; Host is derived from the final URL and cannot be overridden.
Query names and values are literal, encoded before signing; duplicate names are rejected.
Style defaults to path; virtual-hosted prepends the bucket to a DNS base with no path prefix.
Enable that base in --xml-virtual-host-domains; HTTPS requires a matching supplied certificate.
No config file is read unless --config PATH is explicit; its public_base_url supplies the default
base, overridden by --base-url. No environment, Google IAM call or permission change occurs.";

const TEST_SIGNER_HELP: &str = "Usage: gcs-mimic test-signer credentials

Prints a service account key file for the built-in test signer test-signer@gcs-mimic.invalid:
type, project_id local-project, client_email, private_key and the non-Google universe_domain
gcs-emulator.invalid, which keeps Google auth libraries from requesting tokens from Google.
The private key is published with gcs-mimic, so anyone can sign as this identity; use it only
for local tests. The server accepts its signatures in permissive mode with signed_urls=verify,
and in strict mode with test_signer=enabled or when the identity document declares this id.
No configuration, environment or network is read.";

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
            error.exit_code()
        }
    }
}

/// A command failure. Only its diagnostics reach the user; any other failure is reported as a
/// generic runtime diagnostic so internal error text never replaces actionable advice.
#[derive(Debug)]
enum CliError {
    Diagnostics(Diagnostics),
    /// A `sync wait` that did not end ready: its diagnostics, and the exit status that tells
    /// how it ended.
    Wait(Diagnostics, sync_cli::WaitExit),
    /// Authentication setup failed at startup: its diagnostics are reported, and its catalog
    /// causes remain the error's source. Boxed because it holds two catalog errors.
    Authentication(Box<startup::CatalogAuthenticationFailure>),
    /// A failure without advice of its own, such as a closed standard stream.
    Io(io::Error),
    Import(gcs_mimic::import::ImportError),
}

impl CliError {
    fn diagnostics(&self) -> Option<&Diagnostics> {
        match self {
            Self::Diagnostics(diagnostics)
            | Self::Wait(diagnostics, _)
            | Self::Import(gcs_mimic::import::ImportError::Diagnostics(diagnostics)) => {
                Some(diagnostics)
            }
            Self::Authentication(failure) => Some(failure.diagnostics()),
            Self::Import(_) | Self::Io(_) => None,
        }
    }

    /// The status the process exits with: a wait's own, and 1 for any other failure.
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::Wait(_, exit) => ExitCode::from(exit.code()),
            Self::Diagnostics(_) | Self::Authentication(_) | Self::Io(_) | Self::Import(_) => {
                ExitCode::FAILURE
            }
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Diagnostics(diagnostics) | Self::Wait(diagnostics, _) => diagnostics.fmt(f),
            Self::Authentication(failure) => failure.fmt(f),
            Self::Io(error) => error.fmt(f),
            Self::Import(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Diagnostics(diagnostics) | Self::Wait(diagnostics, _) => Some(diagnostics),
            Self::Authentication(failure) => Some(failure.as_ref()),
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

impl From<startup::CatalogAuthenticationFailure> for CliError {
    fn from(failure: startup::CatalogAuthenticationFailure) -> Self {
        Self::Authentication(Box::new(failure))
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
        // Valueless bucket settings would read as options missing a value in the shared check.
        [command, kind, help] if command == "buckets" && kind == "new" && help == "--help" => {
            println!("{}", buckets_cli::HELP);
            return Ok(());
        }
        [command, kind, rest @ ..] if command == "buckets" && kind == "new" => {
            return buckets_cli::run(rest);
        }
        [command, ..] if command == "buckets" => {
            return Err(Diagnostics::error(
                DiagnosticSource::Cli,
                "command",
                "Invalid buckets command; use gcs-mimic buckets new, see gcs-mimic buckets new --help.",
            )
            .into());
        }
        _ => (),
    }
    if gcs_mimic::config_input::requests_version(&args) {
        println!("gcs-mimic {}", gcs_mimic::VERSION);
        return Ok(());
    }
    if let Some(options) = gcs_mimic::config_input::server_arguments(&args) {
        return serve(options);
    }
    let diagnostics = gcs_mimic::config_input::cli_diagnostics(&args);
    if diagnostics.is_err() {
        return Err(diagnostics.into());
    }
    match args.as_slice() {
        [] => Err(Diagnostics::error(
            DiagnosticSource::Cli,
            "command",
            "No command given; start the emulator with gcs-mimic serve, or see gcs-mimic --help.",
        )
        .into()),
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
        [command, help] if command == "test-signer" && help == "--help" => {
            println!("{TEST_SIGNER_HELP}");
            Ok(())
        }
        [command, kind] if command == "test-signer" && kind == "credentials" => {
            println!("{}", gcs_mimic::auth::test_signer::service_account_json());
            Ok(())
        }
        [command, ..] if command == "test-signer" => Err(Diagnostics::error(
            DiagnosticSource::Cli,
            "command",
            format!("Invalid test-signer command; use gcs-mimic test-signer credentials.\n{TEST_SIGNER_HELP}"),
        )
        .into()),
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
    if gcs_mimic::config_input::requests_help(rest) {
        println!("{}", help());
        return Ok(());
    }
    let mut report = Config::preflight_os(rest, std::env::vars_os());
    gcs_mimic::diagnostics::set_log_level(report.config.log_level);
    // Validate supplied TLS material before touching the catalog, seed or listener.
    let mut security = tls::ServerSecurity::plain();
    if report.tls_settings_valid() {
        match tls::ServerSecurity::load(&report.config) {
            Ok(value) => security = value,
            Err(error) => report.diagnostics.errors.push(error.diagnostic()),
        }
    }
    // Load the registry before binding so a bad identity document never reaches readiness.
    let mut auth = report.load_auth();
    if report.default_seed_settings_valid() {
        let warnings = gcs_mimic::config_input::resolve_default_seed(&mut report.config);
        report.diagnostics.warnings.extend(warnings);
    }
    // The fake-gcs-server profile keeps project-owner objects, as fake-gcs-server does.
    if report.config.compat_profile == gcs_mimic::config::CompatProfile::Native
        && let Some(email) = report.config.local_creator_email.as_deref()
    {
        auth = auth.with_local_creator_email(email);
    }
    if report.diagnostics.is_err() {
        return Err(report.diagnostics.into());
    }
    auth = auth.with_verifier_budget(report.config.max_blocking_threads);
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.worker_threads)
        .max_blocking_threads(config.max_blocking_threads)
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostics::error(
                DiagnosticSource::Runtime,
                "runtime",
                "Cannot create runtime; check worker limits and available system resources.",
            )
        })?;
    runtime.block_on(async move {
        #[cfg(unix)]
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|_| signal_unavailable("termination"))?;
        #[cfg(unix)]
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|_| signal_unavailable("interrupt"))?;
        #[cfg(windows)]
        let mut terminate =
            tokio::signal::windows::ctrl_break().map_err(|_| signal_unavailable("termination"))?;
        #[cfg(windows)]
        let mut interrupt =
            tokio::signal::windows::ctrl_c().map_err(|_| signal_unavailable("interrupt"))?;
        let shutdown = async {
            #[cfg(any(unix, windows))]
            tokio::select! { _ = interrupt.recv() => Ok(()), _ = terminate.recv() => Ok(()) }
            #[cfg(not(any(unix, windows)))]
            tokio::signal::ctrl_c().await
        };
        startup::serve(config, auth, security, shutdown).await
    })
}

#[cfg(any(unix, windows))]
fn signal_unavailable(signal: &str) -> Diagnostics {
    Diagnostics::error(
        DiagnosticSource::Runtime,
        "signal",
        format!("Cannot register {signal} signal; check the local runtime environment."),
    )
}

fn helper_public_base(args: &[String]) -> Result<Option<String>, Diagnostics> {
    let file = option_pairs(args)
        .filter_map(Result::ok)
        .find(|(name, _)| *name == "--config");
    match file {
        Some((_, value)) => {
            gcs_mimic::config::read_public_base_url(value).map_err(Diagnostics::from)
        }
        None => Ok(None),
    }
}

/// The options `url public` accepts, each at most once.
const URL_PUBLIC_OPTIONS: [&str; 5] = ["--base-url", "--config", "--bucket", "--object", "--style"];

/// The options `url sign` accepts at most once; `--header` and `--query` may repeat.
const URL_SIGN_OPTIONS: [&str; 10] = [
    "--base-url",
    "--config",
    "--bucket",
    "--object",
    "--method",
    "--expires",
    "--signer",
    "--private-key",
    "--timestamp",
    "--style",
];

/// The options of a `url` helper that may each be given once.
struct UrlOptions<'a> {
    values: BTreeMap<&'a str, &'a str>,
    /// The usage text that a missing option's error repeats.
    help: &'static str,
}

impl<'a> UrlOptions<'a> {
    fn new(help: &'static str) -> Self {
        Self {
            values: BTreeMap::new(),
            help,
        }
    }

    fn insert(&mut self, name: &'a str, value: &'a str) -> Result<(), UrlFix> {
        match self.values.entry(name) {
            btree_map::Entry::Vacant(entry) => {
                entry.insert(value);
                Ok(())
            }
            btree_map::Entry::Occupied(_) => Err(format!("duplicate option: {name}").into()),
        }
    }

    fn get(&self, name: &str) -> Option<&'a str> {
        self.values.get(name).copied()
    }

    fn required(&self, name: &str) -> Result<&'a str, UrlFix> {
        self.get(name)
            .ok_or_else(|| format!("missing required option: {name}\n{}", self.help).into())
    }

    /// The object URL both helpers start from, in the selected style.
    fn object_url(&self, configured: Option<String>) -> Result<String, UrlFix> {
        let build = match self.get("--style").unwrap_or("path").parse()? {
            UrlStyle::Path => gcs_mimic::url::public_object_url,
            UrlStyle::VirtualHosted => gcs_mimic::url::virtual_host_object_url,
        };
        let base = self.get("--base-url").map(str::to_owned).or(configured).ok_or(
            "missing required option: --base-url; provide --base-url URL or --config PATH containing public_base_url.",
        )?;
        build(
            &base,
            self.required("--bucket")?,
            self.required("--object")?,
        )
        .map_err(|error| error.to_string().into())
    }
}

fn url_public(args: &[String], configured: Option<String>) -> Result<String, UrlFix> {
    let mut options = UrlOptions::new(URL_PUBLIC_HELP);
    for pair in option_pairs(args) {
        let (name, value) = pair.map_err(|_| {
            format!("url public options require --name VALUE pairs\n{URL_PUBLIC_HELP}")
        })?;
        if !URL_PUBLIC_OPTIONS.contains(&name) {
            return Err(format!("unknown url public option: {name}\n{URL_PUBLIC_HELP}").into());
        }
        options.insert(name, value)?;
    }
    options.object_url(configured)
}

fn url_sign(args: &[String], configured: Option<String>) -> Result<String, UrlFix> {
    let mut options = UrlOptions::new(URL_SIGN_HELP);
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
            if !URL_SIGN_OPTIONS.contains(&name) {
                return Err(format!("unknown url sign option: {name}").into());
            }
            options.insert(name, value)?;
        }
    }
    let mut url = options.object_url(configured)?;
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    let method = options
        .required("--method")?
        .parse()
        .map_err(|_| "invalid signing method")?;
    let duration = options.required("--expires")?;
    let expires = gcs_mimic::config_input::duration(
        duration,
        &[("s", 1), ("m", 60), ("h", 3600), ("d", 86400), ("", 1)],
    )
    .ok()
    .filter(|_| duration.as_bytes().first().is_some_and(u8::is_ascii_digit))
    .ok_or("invalid signing duration")?;
    let now = options
        .get("--timestamp")
        .map(gcs_mimic::auth::signature::parse_timestamp)
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
    options: &UrlOptions<'a>,
) -> Result<(&'a str, std::borrow::Cow<'static, [u8]>), UrlFix> {
    use gcs_mimic::auth::test_signer;
    match (options.get("--signer"), options.get("--private-key")) {
        (None, None) => Ok((
            test_signer::ID,
            std::borrow::Cow::Borrowed(test_signer::PRIVATE_KEY_PEM.as_bytes()),
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

const HEALTHCHECK_USAGE: &str = "gcs-mimic healthcheck [--url URL] [--ca-cert PATH]";

fn healthcheck(args: &[String]) -> Result<(), Diagnostics> {
    // Options are checked before anything is probed or PID 1 is read, and a mistake in them is
    // reported as one: the advice for an unreachable server would only mislead.
    let options = HealthcheckArgs::try_from(args).map_err(|error| {
        Diagnostics::error(
            DiagnosticSource::Cli,
            "healthcheck",
            format!("Invalid healthcheck options: {error}. Usage: {HEALTHCHECK_USAGE}"),
        )
    })?;
    let ca_cert = options.ca_cert.as_deref();
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
    let failed = |url: &ProbeUrl, error: io::Error| {
        Diagnostics::error(
            DiagnosticSource::Cli,
            "healthcheck",
            format!(
                "Healthcheck {url} failed: {error}; require HTTP 200 and verify CA trust (including 403 access denial). {}",
                repair(&url.to_string())
            ),
        )
    };
    // A ready API whose synchronization is still starting answered, so the PID 1 fallback below
    // never runs for it.
    if let Some(url) = &options.url {
        let body = probe(url, ca_cert).map_err(|error| failed(url, error))?;
        return synchronization_started(url, ca_cert, &body);
    }
    let url = probe_url(&[], &repair)?;
    let error = match probe(&url, ca_cert) {
        Ok(body) => return synchronization_started(&url, ca_cert, &body),
        Err(error) => error,
    };
    // Only a failed probe consults PID 1: Rosetta emulates /proc/<pid>/cmdline with a temporary
    // file, which crashes the probe on a read-only root, so ordinary setups never read it.
    let Some(server_args) = std::fs::read("/proc/1/cmdline")
        .ok()
        .and_then(|cmdline| container_server_args(&cmdline))
    else {
        return Err(failed(&url, error));
    };
    let fallback = probe_url(&server_args, &repair)?;
    if fallback == url {
        return Err(failed(&url, error));
    }
    let body = probe(&fallback, ca_cert).map_err(|error| failed(&fallback, error))?;
    synchronization_started(&fallback, ca_cert, &body)
}

/// The options of `gcs-mimic healthcheck`.
#[derive(Debug)]
struct HealthcheckArgs {
    /// The probe; without it, the configuration and then PID 1 select one.
    url: Option<ProbeUrl>,
    ca_cert: Option<std::path::PathBuf>,
}

impl TryFrom<&[String]> for HealthcheckArgs {
    type Error = HealthcheckUsage;

    fn try_from(args: &[String]) -> Result<Self, Self::Error> {
        let mut url = None;
        let mut ca_cert = None;
        for pair in option_pairs(args) {
            let (name, value) = pair.map_err(|_| HealthcheckUsage::Pairs)?;
            let slot = match name {
                "--url" => &mut url,
                "--ca-cert" => &mut ca_cert,
                _ => return Err(HealthcheckUsage::Unknown(name.to_owned())),
            };
            if value.is_empty() {
                return Err(HealthcheckUsage::Empty(name.to_owned()));
            }
            if slot.replace(value).is_some() {
                return Err(HealthcheckUsage::Duplicate(name.to_owned()));
            }
        }
        let url = url
            .map(str::parse::<ProbeUrl>)
            .transpose()
            .map_err(HealthcheckUsage::Url)?;
        // A URL from the configuration may still turn out to be HTTPS, so only an explicit one
        // settles this before probing.
        if ca_cert.is_some()
            && url
                .as_ref()
                .is_some_and(|url| url.scheme == ProbeScheme::Http)
        {
            return Err(HealthcheckUsage::CaCertNeedsHttps);
        }
        Ok(Self {
            url,
            ca_cert: ca_cert.map(std::path::PathBuf::from),
        })
    }
}

/// Healthcheck options that cannot run.
#[derive(Debug, PartialEq, Eq)]
enum HealthcheckUsage {
    Pairs,
    Unknown(String),
    Empty(String),
    Duplicate(String),
    Url(InvalidProbeUrl),
    CaCertNeedsHttps,
}

impl std::fmt::Display for HealthcheckUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pairs => f.write_str("options require --name VALUE pairs"),
            Self::Unknown(name) => write!(f, "unknown option {name}"),
            Self::Empty(name) => write!(f, "{name} needs a value"),
            Self::Duplicate(name) => write!(f, "{name} is given more than once"),
            Self::Url(error) => write!(f, "--url {error}"),
            Self::CaCertNeedsHttps => f.write_str("--ca-cert requires an HTTPS healthcheck URL"),
        }
    }
}

impl std::error::Error for HealthcheckUsage {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeScheme {
    Http,
    Https,
}

impl ProbeScheme {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

/// A probe of this server: an HTTP(S) origin with an explicit port and a probe path.
#[derive(Debug, PartialEq, Eq)]
struct ProbeUrl {
    scheme: ProbeScheme,
    authority: String,
    /// The host without IPv6 brackets, as TLS names it.
    host: String,
    /// The probe path without its leading slash.
    path: &'static str,
}

impl ProbeUrl {
    fn origin(&self) -> String {
        format!("{}://{}", self.scheme.as_str(), self.authority)
    }
}

impl std::fmt::Display for ProbeUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.origin(), self.path)
    }
}

/// A URL that names no probe of this server.
#[derive(Debug, PartialEq, Eq)]
struct InvalidProbeUrl;

impl std::fmt::Display for InvalidProbeUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("expected http(s)://HOST:PORT/_internal/health, /_internal/ready or /_internal/healthcheck")
    }
}

impl std::error::Error for InvalidProbeUrl {}

impl std::str::FromStr for ProbeUrl {
    type Err = InvalidProbeUrl;

    fn from_str(url: &str) -> Result<Self, Self::Err> {
        let uri: hyper::Uri = url.parse().map_err(|_| InvalidProbeUrl)?;
        let scheme = match uri.scheme_str() {
            Some("http") => ProbeScheme::Http,
            Some("https") => ProbeScheme::Https,
            _ => return Err(InvalidProbeUrl),
        };
        let (Some(authority), Some(host)) = (uri.authority(), uri.host()) else {
            return Err(InvalidProbeUrl);
        };
        let path = match uri.path() {
            "/_internal/health" => "_internal/health",
            "/_internal/ready" => "_internal/ready",
            "/_internal/healthcheck" => "_internal/healthcheck",
            _ => return Err(InvalidProbeUrl),
        };
        if uri.port_u16().is_none()
            || uri.query().is_some()
            || url.contains('#')
            || authority.as_str().contains(['@', '\r', '\n'])
        {
            return Err(InvalidProbeUrl);
        }
        Ok(Self {
            scheme,
            authority: authority.as_str().to_owned(),
            host: host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned(),
            path,
        })
    }
}

/// Passes unless the ready answer `body` reports directory synchronization still `starting`.
/// `starting` lasts until this process's first pass over the tree, which exports existing
/// objects, has finished, so a dependent container started on this healthcheck sees the tree.
fn synchronization_started(
    url: &ProbeUrl,
    ca_cert: Option<&Path>,
    body: &[u8],
) -> Result<(), Diagnostics> {
    use gcs_mimic::sync::{MirrorState, advice};
    let fix = match reported_sync(body) {
        // A mirror past `starting` has ended its first pass. Later trouble is for sync status
        // and the server log to explain: waiting would not end a pause or a stop, and a failing
        // healthcheck would hold back or restart a container whose Storage API serves.
        Ok(
            None
            | Some(
                MirrorState::Running
                | MirrorState::Retrying
                | MirrorState::Paused
                | MirrorState::Stopped,
            ),
        ) => return Ok(()),
        Ok(Some(MirrorState::Starting)) => format!(
            "Healthcheck {url} found the API ready, but sync.state is starting: directory synchronization has not finished this process's first pass, which exports existing objects into the sync directory. The healthcheck passes once the state leaves starting. Follow it, adding --token-file PATH under strict authentication: {}",
            advice::with_connection(
                &advice::Advised::Status.to_string(),
                &probe_connection(url, ca_cert)
            ),
        ),
        Err(error) => {
            let causes =
                std::iter::successors(std::error::Error::source(&error), |cause| cause.source());
            let reason = causes.fold(error.to_string(), |reason, cause| {
                format!("{reason}: {cause}")
            });
            format!(
                "Healthcheck {url} found the API ready, but {reason}. The server runs another gcs-mimic version; probe it with the gcs-mimic of the server's version."
            )
        }
    };
    Err(Diagnostics::error(
        DiagnosticSource::Cli,
        "healthcheck",
        fix,
    ))
}

/// The options that reach the server a healthcheck probed, as `gcs-mimic sync` spells them.
fn probe_connection(url: &ProbeUrl, ca_cert: Option<&Path>) -> String {
    match ca_cert {
        Some(path) => format!("--endpoint {} --ca-cert {}", url.origin(), path.display()),
        None => format!("--endpoint {}", url.origin()),
    }
}

/// The mirror state a ready answer reports. The server adds `sync` only while directory
/// synchronization is enabled; an answer without it, or one that is not a JSON object at all,
/// leaves the API alone to decide, as before the field existed.
fn reported_sync(body: &[u8]) -> Result<Option<gcs_mimic::sync::MirrorState>, UnreadableSync> {
    #[derive(serde::Deserialize)]
    struct ReadySync {
        state: String,
    }
    let Ok(serde_json::Value::Object(mut fields)) = serde_json::from_slice(body) else {
        return Ok(None);
    };
    let Some(sync) = fields.remove("sync") else {
        return Ok(None);
    };
    let ReadySync { state } = serde_json::from_value(sync).map_err(UnreadableSync::Shape)?;
    state.parse().map(Some).map_err(UnreadableSync::State)
}

/// A `sync` object this healthcheck cannot read, as from a server of another version.
#[derive(Debug)]
enum UnreadableSync {
    Shape(serde_json::Error),
    State(gcs_mimic::sync::UnknownMirrorState),
}

impl std::fmt::Display for UnreadableSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Shape(_) => "its sync object is not one this healthcheck reads",
            Self::State(_) => "its sync.state is not one this healthcheck knows",
        })
    }
}

impl std::error::Error for UnreadableSync {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Shape(error) => Some(error),
            Self::State(error) => Some(error),
        }
    }
}

/// The probe URL for the environment, JSON file and the given server options.
fn probe_url(
    server_args: &[String],
    repair: &dyn Fn(&str) -> String,
) -> Result<ProbeUrl, Diagnostics> {
    let mut report = Config::preflight_os(server_args, std::env::vars_os());
    let url = report.config.healthcheck_url();
    match url.parse() {
        Ok(probe) if !report.diagnostics.is_err() => Ok(probe),
        Ok(_) | Err(InvalidProbeUrl) => {
            report.diagnostics.errors.push(Diagnostic::new(
                DiagnosticSource::Cli,
                "healthcheck",
                format!(
                    "Cannot derive a reliable probe URL (candidate {url}); fix configuration. {}",
                    repair(&url)
                ),
            ));
            Err(report.diagnostics)
        }
    }
}

/// The server options in `cmdline`, the NUL-separated `/proc/1/cmdline` of a container's PID 1,
/// when that process is gcs-mimic running the server with at least one option, as in the
/// container image. A Compose `command` such as `--scheme http` reaches only that process, so
/// the healthcheck falls back to them when the environment's defaults do not answer.
fn container_server_args(cmdline: &[u8]) -> Option<Vec<String>> {
    let cmdline = cmdline.strip_suffix(b"\0").unwrap_or(cmdline);
    let args = cmdline
        .split(|byte| *byte == 0)
        .map(|arg| String::from_utf8(arg.to_vec()))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let (program, args) = args.split_first()?;
    if Path::new(program).file_name() != Some("gcs-mimic".as_ref()) {
        return None;
    }
    gcs_mimic::config_input::server_arguments(args)
        .filter(|options| !options.is_empty())
        .map(<[String]>::to_vec)
}

/// The body of the HTTP 200 answer to `url`.
fn probe(url: &ProbeUrl, ca_cert: Option<&Path>) -> io::Result<Vec<u8>> {
    let client = match (url.scheme, ca_cert) {
        (ProbeScheme::Https, Some(path)) => Some(tls::client_config(path)?),
        (ProbeScheme::Https, None) => Some(tls::development_client_config()?),
        // Only a URL from the configuration gets here: an explicit one was refused as usage.
        (ProbeScheme::Http, Some(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--ca-cert requires an HTTPS healthcheck URL",
            ));
        }
        (ProbeScheme::Http, None) => None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), async {
            let addresses = tokio::net::lookup_host(url.authority.as_str()).await?;
            let mut connected = Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the probe host resolved to no address",
            ));
            for address in addresses.take(8) {
                connected = tokio::net::TcpStream::connect(address).await;
                if connected.is_ok() {
                    break;
                }
            }
            let stream = connected?;
            if let Some(client) = client {
                let name = tokio_rustls::rustls::pki_types::ServerName::try_from(url.host.clone())
                    .map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "the probe host is not a valid TLS server name",
                        )
                    })?;
                let stream = tokio_rustls::TlsConnector::from(client)
                    .connect(name, stream)
                    .await?;
                probe_response(stream, &url.authority, url.path).await
            } else {
                probe_response(stream, &url.authority, url.path).await
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "probe deadline exceeded"))?
    });
    // DNS resolution uses blocking workers; a timed-out one-shot probe must not wait for them.
    runtime.shutdown_background();
    result
}

/// The body of an HTTP 200 answer to `GET /{path}`.
async fn probe_response(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    authority: &str,
    path: &str,
) -> io::Result<Vec<u8>> {
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
    Ok(response.split_off(boundary + 4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthcheck_falls_back_only_to_a_gcs_mimic_server_with_options() {
        let options = |args: &[&str]| Some(args.iter().map(|arg| (*arg).to_owned()).collect());
        for (cmdline, expected) in [
            (
                b"/usr/local/bin/gcs-mimic\0--scheme\0http\0".as_slice(),
                options(&["--scheme", "http"]),
            ),
            (
                b"gcs-mimic\0serve\0--listen\x000.0.0.0:9000\0",
                options(&["--listen", "0.0.0.0:9000"]),
            ),
            // The image's own command carries no option, so the first probe already covered it.
            (b"/usr/local/bin/gcs-mimic\0serve\0", None),
            (b"/usr/local/bin/gcs-mimic\0healthcheck\0", None),
            (b"/bin/sh\0-c\0gcs-mimic --scheme http\0", None),
            (b"/usr/local/bin/gcs-mimic\0--scheme\0\xff\0", None),
            (b"", None),
        ] {
            assert_eq!(
                container_server_args(cmdline),
                expected,
                "{}",
                String::from_utf8_lossy(cmdline)
            );
        }
    }

    /// Only a ready answer whose mirror is still starting fails, and its diagnostic gives the
    /// command that follows the mirror. An answer without `sync`, as from a server without a sync
    /// directory or from the liveness probe, passes as before; a `sync` this command cannot read
    /// names a version mismatch instead.
    #[test]
    fn the_healthcheck_fails_only_while_synchronization_starts() {
        use gcs_mimic::sync::MirrorState;
        let fix = |url: &str, ca_cert: Option<&str>, body: &str| {
            let url = url.parse().unwrap();
            let Err(diagnostics) =
                synchronization_started(&url, ca_cert.map(Path::new), body.as_bytes())
            else {
                return None;
            };
            let [error] = diagnostics.errors.as_slice() else {
                panic!("{diagnostics:?}");
            };
            Some(error.fix.clone())
        };
        let ready = "http://127.0.0.1:4443/_internal/ready";
        let answer = |sync: &str| format!(r#"{{"status":"ok","unix_time":1,"sync":{sync}}}"#);
        let state = |state: &str| answer(&format!(r#"{{"state":"{state}"}}"#));
        for body in ["", "ok", "[]", r#"{"status":"ok","unix_time":1}"#] {
            assert_eq!(fix(ready, None, body), None, "{body}");
        }
        for mirror in [
            MirrorState::Running,
            MirrorState::Retrying,
            MirrorState::Paused,
            MirrorState::Stopped,
        ] {
            assert_eq!(fix(ready, None, &state(mirror.as_str())), None, "{mirror}");
        }
        let starting = fix(ready, None, &state("starting")).unwrap();
        assert!(
            starting.starts_with(&format!(
                "Healthcheck {ready} found the API ready, but sync.state is starting: "
            )),
            "{starting}"
        );
        assert!(
            starting.ends_with(
                "under strict authentication: gcs-mimic sync status --endpoint http://127.0.0.1:4443"
            ),
            "{starting}"
        );
        let trusted = fix(
            "https://[::1]:4443/_internal/healthcheck",
            Some("tls/ca.pem"),
            &state("starting"),
        )
        .unwrap();
        assert!(
            trusted.ends_with(
                ": gcs-mimic sync status --endpoint https://[::1]:4443 --ca-cert tls/ca.pem"
            ),
            "{trusted}"
        );
        for (body, reason) in [
            (
                state("Running"),
                r#"its sync.state is not one this healthcheck knows: unknown synchronization state "Running""#,
            ),
            (
                answer(r#"{"state":1}"#),
                "its sync object is not one this healthcheck reads: ",
            ),
            (
                answer(r#""running""#),
                "its sync object is not one this healthcheck reads: ",
            ),
        ] {
            let mismatch = fix(ready, None, &body).unwrap();
            assert!(mismatch.contains(reason), "{mismatch}");
            assert!(mismatch.contains("another gcs-mimic version"), "{mismatch}");
            assert!(!mismatch.contains("sync status"), "{mismatch}");
        }
    }

    /// Options that cannot run are reported as usage before anything is probed or PID 1 is read,
    /// without the advice for an unreachable server, which only a failed probe gets.
    #[test]
    fn healthcheck_usage_errors_come_before_any_probe() {
        let args = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let ready = "http://127.0.0.1:1/_internal/ready";
        for (given, usage) in [
            (
                args(&["--require-sync", "true"]),
                HealthcheckUsage::Unknown("--require-sync".to_owned()),
            ),
            (args(&["--url"]), HealthcheckUsage::Pairs),
            (
                args(&["--url", ""]),
                HealthcheckUsage::Empty("--url".to_owned()),
            ),
            (
                args(&["--url", ready, "--url", ready]),
                HealthcheckUsage::Duplicate("--url".to_owned()),
            ),
            (
                args(&["--url", "http://127.0.0.1:1/anything"]),
                HealthcheckUsage::Url(InvalidProbeUrl),
            ),
            (
                args(&["--url", "http://127.0.0.1/_internal/ready"]),
                HealthcheckUsage::Url(InvalidProbeUrl),
            ),
            (
                args(&["--url", ready, "--ca-cert", "ca.pem"]),
                HealthcheckUsage::CaCertNeedsHttps,
            ),
        ] {
            assert_eq!(
                HealthcheckArgs::try_from(given.as_slice()).unwrap_err(),
                usage
            );
            let diagnostics = healthcheck(&given).unwrap_err();
            let [error] = diagnostics.errors.as_slice() else {
                panic!("{diagnostics:?}");
            };
            assert_eq!(
                error.fix,
                format!("Invalid healthcheck options: {usage}. Usage: {HEALTHCHECK_USAGE}")
            );
        }
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let url = format!("http://{closed}/_internal/ready");
        let diagnostics = healthcheck(&args(&["--url", &url])).unwrap_err();
        let [error] = diagnostics.errors.as_slice() else {
            panic!("{diagnostics:?}");
        };
        assert!(
            error
                .fix
                .starts_with(&format!("Healthcheck {url} failed: ")),
            "{}",
            error.fix
        );
    }

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
        // Both helpers share the once-only options and the missing-option text with its help.
        let args = ["--bucket", "a", "--bucket", "b"].map(str::to_owned);
        assert_eq!(
            url_public(&args, None).unwrap_err().0,
            "duplicate option: --bucket"
        );
        assert_eq!(
            url_sign(&args, None).unwrap_err().0,
            "duplicate option: --bucket"
        );
        // The base is checked before the bucket and object, so it is supplied here.
        let args = ["--base-url", "http://localhost:4443", "--bucket", "a"].map(str::to_owned);
        assert_eq!(
            url_public(&args, None).unwrap_err().0,
            format!("missing required option: --object\n{URL_PUBLIC_HELP}")
        );
        assert_eq!(
            url_sign(&args, None).unwrap_err().0,
            format!("missing required option: --object\n{URL_SIGN_HELP}")
        );
    }
}
