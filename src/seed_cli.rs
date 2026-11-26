//! `gcs-mimic seed status|wait`: what the passes that replay the seed directory while the server
//! runs report, read through the local administration routes as a script reads it.

use crate::{
    CliError,
    local_server::{self, InvalidOrigin, Origin, Scheme, Untrusted},
};
use gcs_mimic::{
    config_input::{DurationError, duration},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    seed::{DEFAULT_WAIT, MAX_WAIT},
};
use http_body_util::{BodyExt, Empty};
use hyper::{Method, Request, StatusCode, body::Bytes, header};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};

pub(crate) const HELP: &str = "Usage: gcs-mimic seed status [--endpoint http(s)://HOST:PORT] [--ca-cert PATH]
       gcs-mimic seed wait [--endpoint http(s)://HOST:PORT] [--ca-cert PATH] [--timeout DURATION] [--require-clean]

Reads the passes that replay seed_dir while the server runs, through the local
administration routes, which need admin_enabled on the server. Each command
prints the server's answer as one line of JSON.
status prints whether passes run, their interval and the report of the last pass.
wait starts a pass and prints the report of the first pass that started after it
and left no deletion waiting for its second proof, so every change made to
seed_dir before the command is applied or reported. --timeout bounds the wait:
Nms, Ns or Nm, 1ms..10m, 60s by default. The wait fails when no such pass ends in
time, when the pass could not run or did not list the whole tree, which deletes
nothing, and with --require-clean, which takes no value, also when the pass left
an entry as it was; problems lists those entries.
Without --endpoint, the commands find the server as healthcheck does: through
GCS_MIMIC_LISTEN, GCS_MIMIC_SCHEME and the config file GCS_MIMIC_CONFIG selects,
with a wildcard listen address reached on loopback, and when no server answers
there, through the options of a gcs-mimic server running as the container's PID 1.
HTTPS trusts the embedded development CA unless --ca-cert names the CA that
issued the server's certificate. No data directory or seed directory is opened.
Exit status: 0 on success, 1 on any failure; a report that arrived is printed first.";

const USAGE: &str = "gcs-mimic seed status|wait [--endpoint http(s)://HOST:PORT] [--ca-cert PATH] [--timeout DURATION] [--require-clean]";

const STATUS_PATH: &str = "/_internal/seed";
const WAIT_PATH: &str = "/_internal/seed/wait";

/// The longest answer read. A report lists at most a hundred problems, each with a path below
/// the seed directory and a reason.
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
/// How long `seed status` waits for its answer.
const STATUS_DEADLINE: Duration = Duration::from_secs(10);
/// How much longer than its wait `seed wait` waits for the answer, which the server sends once
/// that wait ends.
const ANSWER_MARGIN: Duration = Duration::from_secs(5);

/// What the command asks the server.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Status,
    Wait { timeout: Duration, require: Require },
}

/// What `seed wait` requires of the pass it receives, beyond that it ran over the whole tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Require {
    Nothing,
    /// The pass left no entry as it was.
    Clean,
}

impl Command {
    fn request(&self, authority: &str) -> Result<Request<Empty<Bytes>>, hyper::http::Error> {
        let request = Request::builder().header(header::HOST, authority);
        match self {
            Self::Status => request.method(Method::GET).uri(STATUS_PATH),
            Self::Wait { timeout, .. } => request
                .method(Method::POST)
                .uri(format!("{WAIT_PATH}?timeoutMs={}", timeout.as_millis()))
                .header(header::CONTENT_LENGTH, 0),
        }
        .body(Empty::new())
    }

    /// How long the command waits for the answer.
    fn deadline(&self) -> Duration {
        match self {
            Self::Status => STATUS_DEADLINE,
            Self::Wait { timeout, .. } => *timeout + ANSWER_MARGIN,
        }
    }
}

/// The options of `gcs-mimic seed`.
#[derive(Debug, PartialEq, Eq)]
struct Options {
    command: Command,
    /// The server `--endpoint` names; without it, the configuration and then PID 1 name one.
    endpoint: Option<Origin>,
    ca_cert: Option<PathBuf>,
}

impl TryFrom<&[String]> for Options {
    type Error = Usage;

    fn try_from(args: &[String]) -> Result<Self, Self::Error> {
        let (wait, args) = match args {
            [command, args @ ..] if command == "status" => (false, args),
            [command, args @ ..] if command == "wait" => (true, args),
            _ => return Err(Usage::Command),
        };
        let mut endpoint = None;
        let mut ca_cert = None;
        let mut timeout = None;
        let mut require = Require::Nothing;
        let mut args = args.iter();
        while let Some(name) = args.next() {
            if wait && name == "--require-clean" {
                if require == Require::Clean {
                    return Err(Usage::Duplicate(name.clone()));
                }
                require = Require::Clean;
                continue;
            }
            let slot = match name.as_str() {
                "--endpoint" => &mut endpoint,
                "--ca-cert" => &mut ca_cert,
                "--timeout" if wait => &mut timeout,
                _ => return Err(Usage::Unknown(name.clone())),
            };
            let value = args.next().ok_or_else(|| Usage::Missing(name.clone()))?;
            if value.is_empty() {
                return Err(Usage::Missing(name.clone()));
            }
            if slot.replace(value.as_str()).is_some() {
                return Err(Usage::Duplicate(name.clone()));
            }
        }
        let endpoint = endpoint
            .map(str::parse::<Origin>)
            .transpose()
            .map_err(|InvalidOrigin| Usage::Endpoint)?;
        // A configured endpoint may still turn out to be HTTPS, so only a given one settles this
        // before any request.
        if ca_cert.is_some()
            && endpoint
                .as_ref()
                .is_some_and(|origin| origin.scheme == Scheme::Http)
        {
            return Err(Usage::CaCertNeedsHttps);
        }
        let command = if wait {
            Command::Wait {
                timeout: timeout.map_or(Ok(DEFAULT_WAIT), wait_timeout)?,
                require,
            }
        } else {
            Command::Status
        };
        Ok(Self {
            command,
            endpoint,
            ca_cert: ca_cert.map(PathBuf::from),
        })
    }
}

fn wait_timeout(value: &str) -> Result<Duration, Usage> {
    let millis =
        duration(value, &[("ms", 1), ("s", 1000), ("m", 60_000)]).map_err(|error| match error {
            DurationError::Unit => Usage::TimeoutUnit,
            DurationError::Number(_) | DurationError::Overflow => Usage::TimeoutRange,
        })?;
    Some(Duration::from_millis(millis))
        .filter(|timeout| (Duration::from_millis(1)..=MAX_WAIT).contains(timeout))
        .ok_or(Usage::TimeoutRange)
}

/// Options that cannot run.
#[derive(Debug, PartialEq, Eq)]
enum Usage {
    Command,
    Unknown(String),
    Missing(String),
    Duplicate(String),
    Endpoint,
    CaCertNeedsHttps,
    TimeoutUnit,
    TimeoutRange,
}

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command => f.write_str("use seed status or seed wait"),
            Self::Unknown(name) => write!(f, "unknown option {name}"),
            Self::Missing(name) => write!(f, "{name} needs a value"),
            Self::Duplicate(name) => write!(f, "{name} is given more than once"),
            Self::Endpoint => f.write_str("--endpoint must be http(s)://HOST:PORT"),
            Self::CaCertNeedsHttps => f.write_str("--ca-cert requires an HTTPS endpoint"),
            Self::TimeoutUnit => f.write_str("--timeout requires an integer with ms, s or m"),
            Self::TimeoutRange => f.write_str("--timeout must be 1ms through 10m"),
        }
    }
}

impl std::error::Error for Usage {}

/// Options that cannot run are reported with the usage, before any server is looked for.
impl From<Usage> for Diagnostics {
    fn from(usage: Usage) -> Self {
        fail(format!("Invalid seed command: {usage}. Usage: {USAGE}"))
    }
}

/// Where the command found its server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Given,
    /// The server configuration in the environment and the config file it selects.
    Configuration,
    /// The options of the gcs-mimic server running as the container's PID 1, which the command
    /// tries once the configured server could not be reached.
    Container,
}

impl Source {
    /// What named the server, as advice names it.
    const fn named_by(self) -> &'static str {
        match self {
            Self::Given => "--endpoint",
            Self::Configuration => {
                "the server configuration (GCS_MIMIC_LISTEN, GCS_MIMIC_SCHEME or the config file GCS_MIMIC_CONFIG selects)"
            }
            Self::Container => "the command line of the gcs-mimic server running as PID 1",
        }
    }

    /// What else the user can do when the server was found rather than given.
    const fn instead(self) -> &'static str {
        match self {
            Self::Given => "",
            Self::Configuration | Self::Container => ", or supply --endpoint",
        }
    }
}

/// A server and what named it. Two are the same when requests to them go to the same origin.
#[derive(Clone, Debug)]
struct Endpoint {
    origin: Origin,
    source: Source,
}

impl PartialEq for Endpoint {
    fn eq(&self, other: &Self) -> bool {
        self.origin == other.origin
    }
}

impl Endpoint {
    /// The server that `server_args`, the environment and the config file it selects configure,
    /// found as the healthcheck finds its server.
    fn configured(server_args: &[String], source: Source) -> Result<Self, CliError> {
        const SUPPLY: &str = "supply --endpoint http(s)://HOST:PORT";
        let origin =
            local_server::origin(server_args, std::env::vars_os()).map_err(|unreliable| {
                let mut diagnostics = unreliable.diagnostics;
                diagnostics.errors.push(diagnostic(format!(
                    "The server configuration has errors, so seed cannot rely on the endpoint it gives ({}); correct them or {SUPPLY}",
                    unreliable.origin
                )));
                diagnostics
            })?;
        let origin = origin.parse().map_err(|InvalidOrigin| {
            fail(format!(
                "The endpoint {origin}, which {} gives, is not one seed can reach; set GCS_MIMIC_LISTEN as the server has it or {SUPPLY}",
                source.named_by()
            ))
        })?;
        Ok(Self { origin, source })
    }

    /// The failure of a request to this server: the origin, what went wrong with its causes, and
    /// the setting to check.
    fn failed(&self, failure: ExchangeFailure) -> CliError {
        let named_by = self.source.named_by();
        let instead = self.source.instead();
        let check = match &failure {
            ExchangeFailure::Trust(_) => "check that --ca-cert names a readable PEM certificate of the CA that issued the server's certificate".to_owned(),
            ExchangeFailure::CaCertOverHttp => {
                format!("--ca-cert requires HTTPS; set GCS_MIMIC_SCHEME as the server has it{instead}")
            }
            ExchangeFailure::Connect(_) | ExchangeFailure::Timeout(_) => {
                format!("check that the server is running at the address in {named_by}{instead}")
            }
            ExchangeFailure::Tls(_) => format!(
                "check that the server serves HTTPS, the scheme in {named_by}, and that --ca-cert names the CA that issued its certificate when it has its own{instead}"
            ),
            ExchangeFailure::Http(_) => format!(
                "check that the server serves {}, the scheme in {named_by}{instead}",
                self.origin.scheme.as_str().to_uppercase()
            ),
            ExchangeFailure::TooLarge | ExchangeFailure::NonJson(..) => {
                format!("check that {named_by} names a gcs-mimic server{instead}")
            }
            ExchangeFailure::Request(_) => "report this as a defect of gcs-mimic".to_owned(),
        };
        let causes =
            std::iter::successors(std::error::Error::source(&failure), |cause| cause.source())
                .map(|cause| format!(" ({cause})"))
                .collect::<String>();
        let advice = fail(format!(
            "Cannot use the server at {}: {failure}{causes}; {check}",
            self.origin
        ));
        CliError::Unanswered(advice, failure)
    }
}

/// Why one request got no answer the command can read. Each keeps its cause as `source()`;
/// [`Endpoint::failed`] names the server and what to check.
#[derive(Debug)]
pub(super) enum ExchangeFailure {
    /// The CA certificate that verifies the server could not be loaded.
    Trust(Box<gcs_mimic::tls::TlsError>),
    /// `--ca-cert` names a CA for a server that its configuration reaches over plain HTTP.
    CaCertOverHttp,
    /// The host resolved to no address that accepted a connection.
    Connect(io::Error),
    Tls(io::Error),
    Request(hyper::http::Error),
    Http(hyper::Error),
    TooLarge,
    NonJson(StatusCode, serde_json::Error),
    Timeout(Duration),
}

impl std::fmt::Display for ExchangeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trust(_) => f.write_str("the CA certificate that verifies it cannot be loaded"),
            Self::CaCertOverHttp => f.write_str("it serves plain HTTP, which takes no --ca-cert"),
            Self::Connect(_) => f.write_str("the connection failed"),
            Self::Tls(_) => f.write_str("the TLS handshake failed"),
            Self::Request(_) => f.write_str("the request cannot be formed"),
            Self::Http(_) => f.write_str("the HTTP exchange failed"),
            Self::TooLarge => write!(f, "its answer exceeds {} MiB", RESPONSE_LIMIT >> 20),
            Self::NonJson(status, _) => write!(
                f,
                "it answered HTTP {} with a body that is not JSON",
                status.as_u16()
            ),
            Self::Timeout(deadline) => {
                write!(f, "it did not answer within {:.1}s", deadline.as_secs_f64())
            }
        }
    }
}

impl std::error::Error for ExchangeFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Trust(error) => Some(error.as_ref()),
            Self::Connect(error) | Self::Tls(error) => Some(error),
            Self::Request(error) => Some(error),
            Self::Http(error) => Some(error),
            Self::NonJson(_, error) => Some(error),
            Self::CaCertOverHttp | Self::TooLarge | Self::Timeout(_) => None,
        }
    }
}

/// A server's answer.
#[derive(Debug)]
struct Reply {
    status: StatusCode,
    body: Value,
}

pub(super) fn run(args: &[String]) -> Result<(), CliError> {
    if matches!(args, [help] | [_, help] if help == "--help") {
        println!("{HELP}");
        return Ok(());
    }
    let options = Options::try_from(args).map_err(Diagnostics::from)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let ask = |endpoint: &Endpoint| {
        runtime
            .block_on(exchange(
                &endpoint.origin,
                options.ca_cert.as_deref(),
                &options.command,
            ))
            .map(|reply| (endpoint.clone(), reply))
            .map_err(|failure| endpoint.failed(failure))
    };
    let answer = match &options.endpoint {
        Some(origin) => ask(&Endpoint {
            origin: origin.clone(),
            source: Source::Given,
        }),
        None => Endpoint::configured(&[], Source::Configuration).and_then(|configured| {
            local_server::with_container_fallback(&configured, ask, |server_args| {
                Endpoint::configured(server_args, Source::Container)
            })
        }),
    };
    // Name resolution uses blocking workers; a command that gave up must not wait for them.
    runtime.shutdown_background();
    let (endpoint, reply) = answer?;
    conclude(&options.command, &endpoint, &reply)
}

/// What the command makes of the server's answer: a report is printed, and a failure is
/// explained with what to do next.
fn conclude(command: &Command, endpoint: &Endpoint, reply: &Reply) -> Result<(), CliError> {
    if reply.status != StatusCode::OK {
        let message = reply
            .body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("no message");
        let next = match (reply.status, command) {
            (StatusCode::NOT_FOUND, _) => format!(
                "; start the server with --admin-enabled true, and check that {} names a gcs-mimic server of this release{}",
                endpoint.source.named_by(),
                endpoint.source.instead()
            ),
            (StatusCode::SERVICE_UNAVAILABLE, Command::Wait { .. }) => {
                "; run seed wait again, or give it a longer --timeout".to_owned()
            }
            _ => String::new(),
        };
        return Err(fail(format!(
            "The server at {} answered HTTP {}: {message}{next}",
            endpoint.origin,
            reply.status.as_u16()
        ))
        .into());
    }
    write_line(&mut io::stdout().lock(), &reply.body)?;
    match command {
        Command::Status => Ok(()),
        Command::Wait { require, .. } => {
            unsettled(&reply.body, *require).map_or(Ok(()), |reason| Err(fail(reason).into()))
        }
    }
}

/// Why the pass a wait received fails the command: it could not run or did not list the whole
/// tree, so a removed file may keep its object, or `require` asks for a clean pass and it left an
/// entry as it was.
fn unsettled(report: &Value, require: Require) -> Option<&'static str> {
    if report.get("error").is_some() {
        return Some(
            "The pass over seed_dir could not run, as error says; fix seed_dir and run seed wait again",
        );
    }
    if report.get("complete") != Some(&Value::Bool(true)) {
        return Some(
            "The pass did not list the whole of seed_dir, so it deleted nothing; problems says why",
        );
    }
    (require == Require::Clean && report.get("clean") != Some(&Value::Bool(true))).then_some(
        "The pass left entries of seed_dir as they were; problems lists them, and --require-clean fails on any",
    )
}

/// One request to the server at `origin`, verified with the CA at `ca_cert` over HTTPS.
async fn exchange(
    origin: &Origin,
    ca_cert: Option<&Path>,
    command: &Command,
) -> Result<Reply, ExchangeFailure> {
    let client = origin
        .tls_client(ca_cert)
        .map_err(|untrusted| match untrusted {
            Untrusted::Load(error) => ExchangeFailure::Trust(Box::new(error)),
            Untrusted::CaCertOverHttp => ExchangeFailure::CaCertOverHttp,
        })?;
    let deadline = command.deadline();
    tokio::time::timeout(deadline, async {
        let stream = crate::connect(&origin.authority)
            .await
            .map_err(ExchangeFailure::Connect)?;
        match client {
            Some(client) => {
                let stream = crate::connect_tls(client, &origin.host, stream)
                    .await
                    .map_err(ExchangeFailure::Tls)?;
                send(stream, &origin.authority, command).await
            }
            None => send(stream, &origin.authority, command).await,
        }
    })
    .await
    .map_err(|_| ExchangeFailure::Timeout(deadline))?
}

async fn send(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    authority: &str,
    command: &Command,
) -> Result<Reply, ExchangeFailure> {
    let request = command
        .request(authority)
        .map_err(ExchangeFailure::Request)?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(ExchangeFailure::Http)?;
    let exchange = async {
        let response = sender
            .send_request(request)
            .await
            .map_err(ExchangeFailure::Http)?;
        let status = response.status();
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let Ok(data) = frame.map_err(ExchangeFailure::Http)?.into_data() else {
                continue;
            };
            if bytes.len() + data.len() > RESPONSE_LIMIT {
                return Err(ExchangeFailure::TooLarge);
            }
            bytes.extend_from_slice(&data);
        }
        let body = serde_json::from_slice(&bytes)
            .map_err(|cause| ExchangeFailure::NonJson(status, cause))?;
        Ok(Reply { status, body })
    };
    // The connection is driven in this future, so the deadline drops the socket at once.
    tokio::pin!(connection, exchange);
    tokio::select! {
        biased;
        result = &mut exchange => result,
        result = &mut connection => {
            result.map_err(ExchangeFailure::Http)?;
            exchange.await
        }
    }
}

/// `text` with every control character escaped. Diagnostics repeat the server's messages, which
/// can name files anyone can put in the seed directory; unescaped, such text could move the
/// cursor or rewrite the terminal that shows the line. The JSON the diagnostics are written in
/// escapes only U+0000 through U+001F, not DEL or the C1 controls such as U+009B (CSI).
fn printable(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_control() {
            escaped.extend(character.escape_debug());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

fn diagnostic(fix: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::new(DiagnosticSource::Cli, "seed", printable(&fix.to_string()))
}

/// The command's single diagnostic.
fn fail(fix: impl std::fmt::Display) -> Diagnostics {
    diagnostic(fix).into()
}

/// Writes JSON as serde_json's compact form does, and escapes DEL and the C1 controls as `\u007f`
/// through `\u009f` as well, which serde_json writes as they are. A report repeats paths anyone
/// can give a file, which could otherwise drive the terminal that shows it; a JSON reader reads
/// the same string either way.
struct TerminalSafe;

impl serde_json::ser::Formatter for TerminalSafe {
    fn write_string_fragment<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        let unsafe_control = |character: char| matches!(character, '\u{7f}'..='\u{9f}');
        for piece in fragment.split_inclusive(unsafe_control) {
            let mut characters = piece.chars();
            match characters.next_back() {
                Some(last) if unsafe_control(last) => {
                    writer.write_all(characters.as_str().as_bytes())?;
                    write!(writer, "\\u{:04x}", u32::from(last))?;
                }
                Some(_) | None => writer.write_all(piece.as_bytes())?,
            }
        }
        Ok(())
    }
}

/// Writes `value` as one line of JSON that a terminal shows as it is.
fn write_line(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    serde::Serialize::serialize(
        value,
        &mut serde_json::Serializer::with_formatter(&mut *writer, TerminalSafe),
    )
    .map_err(io::Error::from)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn seed_options_name_one_command_and_its_bounds() {
        assert_eq!(
            Options::try_from(args(&["status"]).as_slice()).unwrap(),
            Options {
                command: Command::Status,
                endpoint: None,
                ca_cert: None,
            }
        );
        assert_eq!(
            Options::try_from(
                args(&[
                    "wait",
                    "--require-clean",
                    "--timeout",
                    "10m",
                    "--endpoint",
                    "https://[::1]:4443/",
                    "--ca-cert",
                    "ca.pem",
                ])
                .as_slice()
            )
            .unwrap(),
            Options {
                command: Command::Wait {
                    timeout: MAX_WAIT,
                    require: Require::Clean,
                },
                endpoint: Some(Origin {
                    scheme: Scheme::Https,
                    authority: "[::1]:4443".to_owned(),
                    host: "::1".to_owned(),
                }),
                ca_cert: Some(PathBuf::from("ca.pem")),
            }
        );
        let wait = Options::try_from(args(&["wait", "--timeout", "1ms"]).as_slice()).unwrap();
        assert_eq!(
            wait.command,
            Command::Wait {
                timeout: Duration::from_millis(1),
                require: Require::Nothing,
            }
        );
        for (given, usage) in [
            (args(&[]), Usage::Command),
            (args(&["rescan"]), Usage::Command),
            (
                args(&["status", "--require-clean"]),
                Usage::Unknown("--require-clean".to_owned()),
            ),
            (
                args(&["status", "--timeout", "1s"]),
                Usage::Unknown("--timeout".to_owned()),
            ),
            (
                args(&["wait", "--endpoint"]),
                Usage::Missing("--endpoint".to_owned()),
            ),
            (
                args(&["wait", "--endpoint", ""]),
                Usage::Missing("--endpoint".to_owned()),
            ),
            (
                args(&["wait", "--require-clean", "--require-clean"]),
                Usage::Duplicate("--require-clean".to_owned()),
            ),
            (
                args(&["wait", "--timeout", "1s", "--timeout", "2s"]),
                Usage::Duplicate("--timeout".to_owned()),
            ),
            (args(&["wait", "--timeout", "5"]), Usage::TimeoutUnit),
            (args(&["wait", "--timeout", "0ms"]), Usage::TimeoutRange),
            (args(&["wait", "--timeout", "601s"]), Usage::TimeoutRange),
            (
                args(&["status", "--endpoint", "http://127.0.0.1:1/_internal/seed"]),
                Usage::Endpoint,
            ),
            (
                args(&["status", "--endpoint", "http://127.0.0.1"]),
                Usage::Endpoint,
            ),
            (
                args(&["status", "--endpoint", "ftp://127.0.0.1:1"]),
                Usage::Endpoint,
            ),
            (
                args(&[
                    "status",
                    "--endpoint",
                    "http://127.0.0.1:1",
                    "--ca-cert",
                    "ca.pem",
                ]),
                Usage::CaCertNeedsHttps,
            ),
        ] {
            assert_eq!(
                Options::try_from(given.as_slice()).unwrap_err(),
                usage,
                "{given:?}"
            );
        }
    }

    /// A path in a report is printed so that a terminal shows it and a JSON reader reads it as
    /// it is.
    #[test]
    fn a_printed_report_escapes_the_controls_a_terminal_would_obey() {
        let report = json!({"problems": [{"path": "bucket/\u{9b}31m\u{7f}\n", "reason": "é"}]});
        let mut line = Vec::new();
        write_line(&mut line, &report).unwrap();
        let text = String::from_utf8(line).unwrap();
        assert_eq!(
            text,
            "{\"problems\":[{\"path\":\"bucket/\\u009b31m\\u007f\\n\",\"reason\":\"é\"}]}\n"
        );
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), report);
        assert_eq!(printable("a\u{9b}b\n"), "a\\u{9b}b\\n");
    }

    /// A wait succeeds once the pass listed the whole tree, so that every removed file was
    /// deleted or reported; `--require-clean` also fails on an entry the pass left as it was.
    #[test]
    fn a_wait_fails_on_a_pass_that_did_not_list_the_tree_and_when_asked_on_any_problem() {
        let clean = json!({"complete": true, "clean": true, "problems": []});
        let left =
            json!({"complete": true, "clean": false, "problems": [{"path": "b/x", "reason": "r"}]});
        let partial = json!({"complete": false, "clean": false, "problems": [{"reason": "r"}]});
        let failed = json!({"complete": false, "clean": false, "problems": [], "error": "gone"});
        for require in [Require::Nothing, Require::Clean] {
            assert_eq!(unsettled(&clean, require), None);
            assert!(unsettled(&partial, require).is_some());
            assert!(unsettled(&failed, require).is_some());
        }
        assert_eq!(unsettled(&left, Require::Nothing), None);
        assert!(unsettled(&left, Require::Clean).is_some());
    }
}
