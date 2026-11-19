//! One-shot live controls share the server's authorization and never open its catalog.

use super::CliError;
use gcs_mimic::{
    bounded_file,
    config::Config,
    config_input::{DurationError, RenamedEnvironmentVariable, duration},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    net::is_local_address,
    sync::{UnmappedObjects, WaitReason, advice, handshake},
    tls,
};
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Request, StatusCode, Uri, body::Bytes, client::conn::http1, http::uri::Authority,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, btree_map},
    ffi::OsString,
    io,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};

const RESPONSE_LIMIT: usize = 256 * 1024;
const HELP: &str = "Usage: gcs-mimic sync status|rescan|wait [--endpoint http(s)://HOST:PORT] [OPTIONS]
       gcs-mimic sync wait [--endpoint URL] [--timeout DURATION] [--require-no-findings]
       gcs-mimic sync status [--endpoint URL] --view deletions|findings [--revision REVISION [--cursor CURSOR]] [--limit N]
       gcs-mimic sync deletions|findings [--endpoint URL] [--revision REVISION [--cursor CURSOR]] [--limit N] (the same pages)
       gcs-mimic sync approve [--endpoint URL] --deletion-set REVISION [OPTIONS]
       gcs-mimic sync conflicts [--endpoint URL] [--revision REVISION] [--cursor CURSOR] [--limit N]
       gcs-mimic sync resolve [--endpoint URL] --conflict ID --revision REV --action restore|discard --expected-generation N --operation-id ID
       gcs-mimic sync resolve [--endpoint URL] --recovery ID --revision REV --target-identity ID --action discard --operation-id ID --confirm-backup-verified ID

Without --endpoint, the command controls the server that the environment
configures, as the healthcheck finds it: GCS_MIMIC_LISTEN and GCS_MIMIC_SCHEME,
with a wildcard listen address reached on loopback. It does not open the config
file GCS_MIMIC_CONFIG selects and asks for --endpoint while that is set. The
commands it advises then leave --endpoint out as well.
Options: --ca-cert PATH, --token-file PATH, --timeout Nms|Ns|Nm (1ms..60s),
or the numeric --timeout-ms N. Rescan defaults to 30s; other controls default
to 10s. Explicit timeout values replace these defaults.
Wait takes up to 60m and defaults to 2m, for a server that is already healthy.
It asks the server again, at most a minute at a time, while the tree is still
changing or one request's deadline passed, and stops at once for a pause,
conflicts, source entries no object can take or that cannot be read, deletions
awaiting approval or findings that need action; it then fails with the reason
and what to run next. It succeeds once every change it can reflect is
reflected. Objects whose names the directory cannot hold stay reachable through
the API: the answer counts them in findings.unmapped, and one line on standard
error names the command that lists them. --require-no-findings, which takes no
value, fails the wait if any object the API holds has a name the directory
cannot hold.
Wait exit status (other commands exit 0 on success and 1 on failure):
  0  Ready: every change is reflected, even with objects it only reports
  1  anything else: an unreachable server, a refused request or option, or
     a server that stopped
  2  someone must act: Paused, SourceUnrepresentable, DeletionApproval or
     FindingsPending, which --require-no-findings also fails with
  3  the timeout passed while work was in progress: Timeout, SourceChanging
     or JournalChanged
The endpoint must be a loopback or private-network HTTP(S) origin with an explicit port.
Use HTTPS outside loopback to protect the token. HTTPS verifies
the embedded development CA or the supplied CA and the endpoint hostname.
The token file contains one local bearer token (at most 8192 bytes), overriding
the CLI-only GCSM_SYNC_TOKEN environment variable when set.
Status and deletions print one bounded JSON page. Without --revision, a view
reads the status first and lists the page at the revision it reports. Copy
next_cursor unchanged with the revision of its page; approval requires the
exact inspected revision, and stale revisions fail.
No data directory, server config file, ambient proxy or Google credentials are opened.
Rescan and approval are not retried; a timeout may leave accepted work running.
Inspect status before retrying. Recovery discard requires exactly one matching
--confirm-backup-verified ID or --confirm-discard-without-backup ID.
Unsupported server operations fail visibly.";

/// A sync control option. Each takes one value, except a switch, which its presence sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Flag {
    Endpoint,
    CaCert,
    TokenFile,
    TimeoutMs,
    Timeout,
    Revision,
    Cursor,
    Limit,
    Conflict,
    Recovery,
    Action,
    ExpectedGeneration,
    TargetIdentity,
    OperationId,
    ConfirmBackupVerified,
    ConfirmDiscardWithoutBackup,
    DeletionSet,
    View,
    RequireNoFindings,
}

impl Flag {
    const ALL: [Self; 19] = [
        Self::Endpoint,
        Self::CaCert,
        Self::TokenFile,
        Self::TimeoutMs,
        Self::Timeout,
        Self::Revision,
        Self::Cursor,
        Self::Limit,
        Self::Conflict,
        Self::Recovery,
        Self::Action,
        Self::ExpectedGeneration,
        Self::TargetIdentity,
        Self::OperationId,
        Self::ConfirmBackupVerified,
        Self::ConfirmDiscardWithoutBackup,
        Self::DeletionSet,
        Self::View,
        Self::RequireNoFindings,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Endpoint => "--endpoint",
            Self::CaCert => "--ca-cert",
            Self::TokenFile => "--token-file",
            Self::TimeoutMs => "--timeout-ms",
            Self::Timeout => "--timeout",
            Self::Revision => "--revision",
            Self::Cursor => "--cursor",
            Self::Limit => "--limit",
            Self::Conflict => "--conflict",
            Self::Recovery => "--recovery",
            Self::Action => "--action",
            Self::ExpectedGeneration => "--expected-generation",
            Self::TargetIdentity => "--target-identity",
            Self::OperationId => "--operation-id",
            Self::ConfirmBackupVerified => "--confirm-backup-verified",
            Self::ConfirmDiscardWithoutBackup => "--confirm-discard-without-backup",
            Self::DeletionSet => "--deletion-set",
            Self::View => "--view",
            Self::RequireNoFindings => "--require-no-findings",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|flag| flag.as_str() == name)
    }

    /// Whether the option is a switch, which takes no value.
    const fn is_switch(self) -> bool {
        matches!(self, Self::RequireNoFindings)
    }
}

/// The refusal of a value given to a switch, which would otherwise read as a misplaced option.
fn switch_value(switch: &str, value: &str) -> Diagnostics {
    fail(format!(
        "{switch} is a switch and takes no value; give it alone, without {value}"
    ))
}

/// The options of one command line. Each step takes what it reads; anything left was not
/// accepted by the selected operation. A switch reads as an empty value.
struct Flags<'a>(BTreeMap<Flag, &'a str>);

impl<'a> Flags<'a> {
    /// Known options, each once, with a nonempty value of at most 8192 bytes, or none for a
    /// switch. No argument stands alone, so one that follows a switch is a value given to it.
    fn read(args: &'a [String]) -> Result<Self, Diagnostics> {
        let refused = || fail("Use unique sync option/value pairs; see sync --help");
        let mut flags = BTreeMap::new();
        let mut rest = args;
        while let [name, tail @ ..] = rest {
            let Some(flag) = Flag::parse(name) else {
                return Err(match name.split_once('=') {
                    Some((switch, value)) if Flag::parse(switch).is_some_and(Flag::is_switch) => {
                        switch_value(switch, &format!("={value}"))
                    }
                    Some(_) | None => refused(),
                });
            };
            let value = if flag.is_switch() {
                if let Some(value) = tail.first().filter(|value| !value.starts_with("--")) {
                    return Err(switch_value(name, value));
                }
                rest = tail;
                ""
            } else {
                let [value, tail @ ..] = tail else {
                    return Err(refused());
                };
                if value.is_empty() || value.len() > 8192 {
                    return Err(refused());
                }
                rest = tail;
                value.as_str()
            };
            match flags.entry(flag) {
                btree_map::Entry::Vacant(entry) => {
                    entry.insert(value);
                }
                btree_map::Entry::Occupied(_) => return Err(refused()),
            }
        }
        Ok(Self(flags))
    }

    fn take(&mut self, flag: Flag) -> Option<&'a str> {
        self.0.remove(&flag)
    }

    fn contains(&self, flag: Flag) -> bool {
        self.0.contains_key(&flag)
    }

    fn finish(self) -> Result<(), Diagnostics> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(fail(
                "This sync operation does not accept the remaining options; see sync --help",
            ))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Status,
    Rescan,
    Wait,
    Approve,
    Resolve,
    List(Listing),
}

/// A paged listing: conflicts, or one view of the status pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listing {
    Conflicts,
    View(View),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Deletions,
    Findings,
}

impl View {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "deletions" => Some(Self::Deletions),
            "findings" => Some(Self::Findings),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Deletions => "deletions",
            Self::Findings => "findings",
        }
    }

    /// The revision of this view's pages that the status `answer` reports.
    fn revision_in(self, answer: &Value) -> Result<&str, Diagnostics> {
        let field = match self {
            Self::Deletions => "pending_deletion_revision",
            Self::Findings => "findings_revision",
        };
        let len = revision_len(Some(Command::List(Listing::View(self))));
        match (self, answer.get(field).and_then(Value::as_str)) {
            (_, Some(revision)) if lower_hex(revision, len) => Ok(revision),
            (Self::Deletions, None) => Err(fail(
                "No deletion set awaits approval: status reports no pending_deletion_revision",
            )),
            (Self::Deletions | Self::Findings, _) => Err(fail(format!(
                "Status reported no {field} this command can read; inspect its JSON response"
            ))),
        }
    }
}

/// A view page read at the revision that the status reports when the command runs.
#[derive(Debug, PartialEq, Eq)]
struct LatestPage {
    view: View,
    paging: Paging,
}

/// The paging options of a listing: where its page starts and how many items it holds.
#[derive(Debug, Default, PartialEq, Eq)]
struct Paging {
    /// The cursor as returned, encoded once for the query.
    cursor: Option<String>,
    limit: Option<usize>,
}

impl Paging {
    fn read(flags: &mut Flags<'_>) -> Result<Self, Diagnostics> {
        let cursor = flags
            .take(Flag::Cursor)
            .map(gcs_mimic::auth::signature::encode_query_component);
        let limit = flags
            .take(Flag::Limit)
            .map(|limit| {
                limit
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=256).contains(n))
                    .ok_or_else(|| fail("--limit must be 1 through 256"))
            })
            .transpose()?;
        Ok(Self { cursor, limit })
    }

    /// The path of the page of `listing` at `revision`.
    fn path(&self, listing: Listing, revision: Option<&str>) -> String {
        let (path, view) = match listing {
            Listing::Conflicts => ("/_internal/sync/conflicts", None),
            Listing::View(view) => (STATUS_PATH, Some(format!("view={}", view.as_str()))),
        };
        let query: Vec<String> = view
            .into_iter()
            .chain(revision.map(|revision| format!("revision={revision}")))
            .chain(
                self.cursor
                    .as_ref()
                    .map(|cursor| format!("cursor={cursor}")),
            )
            .chain(self.limit.map(|limit| format!("limit={limit}")))
            .collect();
        if query.is_empty() {
            path.to_owned()
        } else {
            format!("{path}?{}", query.join("&"))
        }
    }
}

/// The status control, which also lists the views.
const STATUS_PATH: &str = "/_internal/sync";

impl Command {
    /// `None` names no operation. That is reported where the operation is chosen, after the
    /// endpoint, timeout, token and revision, which is when the command has always reported it.
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "status" => Some(Self::Status),
            "rescan" => Some(Self::Rescan),
            "wait" => Some(Self::Wait),
            "approve" => Some(Self::Approve),
            "resolve" => Some(Self::Resolve),
            "conflicts" => Some(Self::List(Listing::Conflicts)),
            name => View::parse(name).map(|view| Self::List(Listing::View(view))),
        }
    }
}

/// Rescans wait for a quiet interval, so they default to 30 s; other controls to 10 s. A wait
/// defaults to 2 minutes, for the changes of a test step on a server that is already healthy:
/// the healthcheck fails until the first pass is done and gives it its own 5-minute start
/// period, since a first pass over a slow bind mount can take several minutes.
fn default_timeout_ms(command: Option<Command>) -> u64 {
    match command {
        Some(Command::Rescan) => 30_000,
        Some(Command::Wait) => 120_000,
        Some(Command::Status | Command::Approve | Command::Resolve | Command::List(_)) | None => {
            10_000
        }
    }
}

/// The longest timeout a command accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimeoutLimit {
    millis: u64,
    /// The same limit as `--timeout` spells it.
    text: &'static str,
}

impl TimeoutLimit {
    /// One request: the server answers a wait within a minute.
    const REQUEST: Self = Self {
        millis: 60_000,
        text: "60s",
    };
    /// A wait asks again, a minute at a time, until its own timeout.
    const WAIT: Self = Self {
        millis: 3_600_000,
        text: "60m",
    };

    fn of(command: Option<Command>) -> Self {
        if command == Some(Command::Wait) {
            Self::WAIT
        } else {
            Self::REQUEST
        }
    }
}

/// The longest one wait request asks the server to observe, which is the server's own limit.
const SERVER_WAIT: Duration = Duration::from_secs(60);
/// The extra time one wait request allows to receive the answer the server sends at its deadline.
const WAIT_MARGIN: Duration = Duration::from_secs(1);
/// How long a wait pauses before it asks again after the journal was rebuilt under it, which
/// the server answers at once.
const JOURNAL_PAUSE: Duration = Duration::from_secs(1);

/// How `sync wait` exits, so that a script tells what ended it without reading the answer.
/// Other commands exit 0 on success and 1 on failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WaitExit {
    /// Every change the wait can reflect is reflected, even while objects it only reports
    /// remain.
    Ready,
    /// No answer names how the wait ended, as for an unreachable server or a refused request,
    /// or the server stopped: like any other failed command.
    Failed,
    /// Waiting does not change what holds synchronization back, so someone must act.
    ActionNeeded,
    /// Work was still in progress when the wait's timeout passed.
    TimedOut,
}

impl WaitExit {
    /// How a wait whose last answer gave `reason`, if any, exits.
    const fn of(reason: Option<WaitReason>) -> Self {
        match reason {
            Some(WaitReason::Ready) => Self::Ready,
            Some(
                WaitReason::Paused
                | WaitReason::SourceUnrepresentable
                | WaitReason::DeletionApproval
                | WaitReason::FindingsPending,
            ) => Self::ActionNeeded,
            // The command asks again while one of these holds, so a wait that ends with one ran
            // out of time.
            Some(WaitReason::Timeout | WaitReason::SourceChanging | WaitReason::JournalChanged) => {
                Self::TimedOut
            }
            Some(WaitReason::Stopped) | None => Self::Failed,
        }
    }

    /// The process exit status.
    pub(super) const fn code(self) -> u8 {
        match self {
            Self::Ready => 0,
            Self::Failed => 1,
            Self::ActionNeeded => 2,
            Self::TimedOut => 3,
        }
    }
}

/// The body of one wait request that asks the server to observe for `millis`, with
/// `require_no_findings` only for a wait that refuses the objects.
fn wait_body(unmapped: UnmappedObjects, millis: u64) -> Value {
    match unmapped {
        UnmappedObjects::Report => json!({"timeout_ms":millis}),
        UnmappedObjects::Refuse => json!({"timeout_ms":millis,"require_no_findings":true}),
    }
}

/// Findings pages are bound to a session revision of 48 digits; the others take 32.
fn revision_len(command: Option<Command>) -> usize {
    if command == Some(Command::List(Listing::View(View::Findings))) {
        48
    } else {
        32
    }
}

/// Where a control request goes. The scheme decided the transport when the endpoint was parsed.
struct Endpoint {
    authority: Authority,
    /// The host for TLS name verification, without IPv6 brackets.
    host: String,
    transport: Transport,
    source: EndpointSource,
}

enum Transport {
    Http,
    Https { ca_cert: Option<PathBuf> },
}

impl Transport {
    const fn scheme(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https { .. } => "https",
        }
    }
}

/// Where the command found its endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndpointSource {
    /// `--endpoint`, which a suggested command names again.
    Given,
    /// The server configuration in the environment, where a suggested command finds it again.
    Environment,
}

impl Endpoint {
    /// The origin requests go to.
    fn origin(&self) -> String {
        format!("{}://{}", self.transport.scheme(), self.authority)
    }

    /// What to check when the server cannot be reached here, which depends on what named it.
    const fn check(&self) -> &'static str {
        match self.source {
            EndpointSource::Given => {
                "check that the server is running and that --endpoint names it"
            }
            EndpointSource::Environment => {
                "check that the server is running and that GCS_MIMIC_LISTEN and GCS_MIMIC_SCHEME give its address, or supply --endpoint"
            }
        }
    }

    /// The failure to connect, which names the origin and what to check, keeping `cause`.
    fn unreachable(&self, cause: io::Error) -> CliError {
        let advice = fail(format!(
            "Cannot connect to the synchronization server at {} ({cause}); {}",
            self.origin(),
            self.check()
        ));
        CliError::Unreachable(advice, cause)
    }

    /// A request that got no answer within `timeout`. The server may have accepted it, so the
    /// advice says to inspect its status before retrying a control.
    fn unanswered(&self, timeout: Duration) -> CliError {
        let advice = fail(format!(
            "The synchronization server at {} did not answer within {:.1}s; {}. A control it accepted may still be running, so inspect its status before retrying",
            self.origin(),
            timeout.as_secs_f64(),
            self.check()
        ));
        let cause = io::Error::new(io::ErrorKind::TimedOut, "synchronization request timed out");
        CliError::Unreachable(advice, cause)
    }

    /// The options that reach this endpoint again, as a suggested command spells them.
    fn options(&self) -> Vec<String> {
        let endpoint = match self.source {
            EndpointSource::Given => Some(format!("--endpoint {}", self.origin())),
            EndpointSource::Environment => None,
        };
        let ca_cert = match &self.transport {
            Transport::Https {
                ca_cert: Some(path),
            } => Some(format!("--ca-cert {}", path.display())),
            Transport::Https { ca_cert: None } | Transport::Http => None,
        };
        endpoint.into_iter().chain(ca_cert).collect()
    }
}

/// The environment variable that selects a server config file, which this command never opens.
const CONFIG_VARIABLE: &str = "GCS_MIMIC_CONFIG";

/// The endpoint of the server that `environment` configures, found the way the healthcheck finds
/// its server, through [`Config::local_origin`]. A config file could move the server, so an
/// environment that selects one, which this command never opens, gives no endpoint.
fn configured_endpoint(
    environment: Vec<(OsString, OsString)>,
    ca_cert: Option<&str>,
) -> Result<Endpoint, Diagnostics> {
    const SUPPLY: &str = "supply --endpoint http(s)://HOST:PORT";
    if environment.iter().any(|(name, _)| name == CONFIG_VARIABLE) {
        return Err(fail(format!(
            "{CONFIG_VARIABLE} selects a config file, which sync does not open to find the server; {SUPPLY}"
        )));
    }
    let report = Config::preflight_os(&[], environment);
    let origin = report.config.local_origin();
    if report.diagnostics.is_err() {
        let mut errors = report.diagnostics.errors;
        errors.push(Diagnostic::new(
            DiagnosticSource::Cli,
            "sync",
            format!(
                "The server configuration in the environment has errors, so sync cannot rely on the endpoint it gives ({origin}); correct them or {SUPPLY}"
            ),
        ));
        return Err(Diagnostics {
            errors,
            ..Diagnostics::default()
        });
    }
    endpoint(&origin, ca_cert, EndpointSource::Environment).map_err(|refusal| {
        fail(match refusal {
            EndpointRefusal::CaCertOverHttp => format!(
                "--ca-cert requires an HTTPS endpoint, and the environment gives {origin}; set GCS_MIMIC_SCHEME as the server has it or {SUPPLY}"
            ),
            EndpointRefusal::Unparsable | EndpointRefusal::NotBare => format!(
                "The environment gives the endpoint {origin}, which sync cannot reach; set GCS_MIMIC_LISTEN as the server has it or {SUPPLY}"
            ),
        })
    })
}

struct Options {
    endpoint: Endpoint,
    /// The first request; for a page at the latest revision, the status that reports it.
    path: String,
    body: Option<Value>,
    latest: Option<LatestPage>,
    token: Option<hyper::header::HeaderValue>,
    /// The token file named on the command line, which a suggested command names again.
    token_file: Option<PathBuf>,
    /// One request's deadline, or for a wait, the deadline of the whole wait.
    timeout: Duration,
    /// `Some` for `sync wait`, which repeats requests until `timeout`.
    wait: Option<UnmappedObjects>,
}

pub(super) fn run(args: &[String]) -> Result<(), CliError> {
    let variable = RenamedEnvironmentVariable::SyncToken;
    if std::env::var_os(variable.old_name()).is_some() {
        return Err(fail(variable.fix()).into());
    }
    if args == ["--help"] {
        println!("{HELP}");
        return Ok(());
    }
    let mut options = Options::parse(args, std::env::vars_os().collect())?;
    if options.token.is_none() {
        let name = variable.name();
        options.token = std::env::var_os(&name)
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| fail(format!("{name} must be UTF-8")))
                    .and_then(|value| token_value(&value, &name))
            })
            .transpose()?;
    }
    let reply = match (options.wait, &options.latest) {
        (Some(unmapped), _) => {
            let started = std::time::Instant::now();
            options.wait_until_settled(unmapped, || started.elapsed())?
        }
        (None, Some(page)) => options.latest_page(page)?,
        (None, None) => options.execute(&options.path, options.body.as_ref(), options.timeout)?,
    };
    // The JSON document is printed whatever the outcome, as the command always has.
    let bytes = serde_json::to_vec(&reply.body).map_err(io::Error::other)?;
    let mut output = io::stdout().lock();
    output
        .write_all(&bytes)
        .and_then(|()| output.write_all(b"\n"))?;
    // A server of another release is the cause of whatever else went wrong, so it comes first.
    if let Err(mismatch) = handshake::check(&reply.headers, handshake::Peer::Server) {
        return Err(fail(mismatch).into());
    }
    let connection = options.connection();
    match options.wait {
        Some(_) => WaitOutcome::read(&reply.body).conclude(reply.status, &connection),
        None if reply.status.is_success() => Ok(()),
        None => Err(http_failure(reply.status, error_message(&reply.body), &connection).into()),
    }
}

/// The message of a failed request's JSON error envelope, `{"error":{"message":..}}`.
fn error_message(body: &Value) -> Option<&str> {
    body.pointer("/error/message").and_then(Value::as_str)
}

/// The failure of a request whose answer names no outcome the command advises on: the server's
/// `message`, whose commands get the options that reached it, or without one, the status alone.
fn http_failure(status: StatusCode, message: Option<&str>, connection: &str) -> Diagnostics {
    let status = status.as_u16();
    fail(match message {
        Some(message) => format!(
            "Synchronization control returned HTTP {status}: {}",
            advice::with_connection(message, connection)
        ),
        None => format!(
            "Synchronization control returned HTTP {status}; inspect its JSON response and preserve pending work."
        ),
    })
}

/// What the command reads from a wait's answer to advise its user.
#[derive(Debug, Default, PartialEq, Eq)]
struct WaitOutcome<'a> {
    reason: Option<WaitReason>,
    /// The objects the tree cannot hold, and the findings revision that lists them.
    unmapped: Option<(u64, &'a str)>,
    paused_reason: Option<&'a str>,
    remedy: Option<&'a str>,
    /// The message of an answer that is a request's failure rather than a wait's end.
    message: Option<&'a str>,
}

impl<'a> WaitOutcome<'a> {
    fn read(body: &'a Value) -> Self {
        let text = |value: &'a Value, key| value.get(key).and_then(Value::as_str);
        let findings = body.get("findings");
        Self {
            reason: text(body, "reason").and_then(|reason| reason.parse().ok()),
            unmapped: findings
                .and_then(|findings| findings.get("unmapped"))
                .and_then(Value::as_u64)
                .zip(findings.and_then(|findings| text(findings, "revision"))),
            paused_reason: text(body, "paused_reason"),
            remedy: text(body, "remedy"),
            message: error_message(body),
        }
    }

    /// Ends the command for a wait whose last answer came with `status`: a ready wait writes its
    /// advice and succeeds, and any other fails with its advice and the [`WaitExit`] of its
    /// reason. An answer that names no reason the command knows fails like any other request.
    fn conclude(&self, status: StatusCode, connection: &str) -> Result<(), CliError> {
        let advice = self.advice(connection);
        match WaitExit::of(self.reason) {
            WaitExit::Ready => {
                advice.write(io::stderr().lock())?;
                Ok(())
            }
            WaitExit::Failed if !advice.is_err() => {
                Err(http_failure(status, self.message, connection).into())
            }
            exit @ (WaitExit::Failed | WaitExit::ActionNeeded | WaitExit::TimedOut) => {
                Err(CliError::Wait(advice, exit))
            }
        }
    }

    /// For a wait that succeeded, one line naming the command that lists the objects the tree
    /// cannot hold, if any remain; for one that failed, an error naming its reason and what to
    /// run next, which names those objects itself when they failed it. The options that reached
    /// this server are filled in.
    fn advice(&self, connection: &str) -> Diagnostics {
        let warnings = self
            .unmapped
            .filter(|(count, _)| *count > 0 && self.reason == Some(WaitReason::Ready))
            .map(|(count, revision)| {
                let (objects, names, stay, them) = if count == 1 {
                    ("object", "its name", "stays", "it")
                } else {
                    ("objects", "their names", "stay", "them")
                };
                Diagnostic::new(
                    DiagnosticSource::Cli,
                    "sync-findings",
                    format!(
                        "{count} {objects} in the API cannot be written to the sync directory because of {names} and {stay} reachable only through the API; list {them} with {}",
                        advice::with_connection(
                            &advice::Advised::Findings(Some(revision)).to_string(),
                            connection
                        )
                    ),
                )
            })
            .into_iter()
            .collect();
        let errors = self
            .reason
            .filter(|reason| *reason != WaitReason::Ready)
            .map(|reason| {
                let paused = self
                    .paused_reason
                    .map_or_else(String::new, |paused| format!(" (paused_reason {paused})"));
                let remedy = self.remedy.map_or_else(String::new, |remedy| {
                    format!(": {}", advice::with_connection(remedy, connection))
                });
                Diagnostic::new(
                    DiagnosticSource::Cli,
                    "sync",
                    format!("sync wait ended with {reason}{paused}{remedy}"),
                )
            })
            .into_iter()
            .collect();
        Diagnostics {
            info: Vec::new(),
            warnings,
            errors,
        }
    }
}

/// A synchronization control's answer, with the headers that carry the server's version.
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

/// The command's single diagnostic: what went wrong is rendered once, here, for the user.
fn fail(fix: impl std::fmt::Display) -> Diagnostics {
    Diagnostics::error(DiagnosticSource::Cli, "sync", fix.to_string())
}

impl Options {
    /// The options of `args`, finding the server in `environment` when no endpoint is given.
    fn parse(args: &[String], environment: Vec<(OsString, OsString)>) -> Result<Self, CliError> {
        let (name, args) = args.split_first().ok_or_else(|| fail(HELP))?;
        let mut flags = Flags::read(args)?;
        let command = match Command::from_name(name) {
            Some(Command::Status) => match flags.take(Flag::View) {
                Some(view) => Some(Command::List(Listing::View(
                    View::parse(view)
                        .ok_or_else(|| fail("--view must be deletions or findings"))?,
                ))),
                None => Some(Command::Status),
            },
            command => command,
        };
        // Approval names the deletion set it approves; --revision belongs to the pages it was
        // read from, so accepting it here too would give approval two spellings.
        if command == Some(Command::Approve) && flags.contains(Flag::Revision) {
            return Err(fail("sync approve takes --deletion-set REVISION, not --revision").into());
        }
        let ca_cert = flags.take(Flag::CaCert);
        let endpoint = match flags.take(Flag::Endpoint) {
            Some(value) => endpoint(value, ca_cert, EndpointSource::Given).map_err(fail)?,
            None => configured_endpoint(environment, ca_cert)?,
        };
        let duration = flags.take(Flag::Timeout);
        if duration.is_some() && flags.contains(Flag::TimeoutMs) {
            return Err(fail("Supply only one timeout option").into());
        }
        let limit = TimeoutLimit::of(command);
        let timeout = match (duration, flags.take(Flag::TimeoutMs)) {
            (Some(duration), _) => timeout_millis(duration, limit)?,
            (None, Some(millis)) => millis
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=limit.millis).contains(n))
                .ok_or_else(|| fail(format!("--timeout-ms must be 1 through {}", limit.millis)))?,
            (None, None) => default_timeout_ms(command),
        };
        let token_file = flags.take(Flag::TokenFile);
        let token = token_file.map(read_token).transpose()?;
        let wait = match command {
            Some(Command::Wait) => Some(
                flags
                    .take(Flag::RequireNoFindings)
                    .map_or(UnmappedObjects::Report, |_| UnmappedObjects::Refuse),
            ),
            Some(
                Command::Status
                | Command::Rescan
                | Command::Approve
                | Command::Resolve
                | Command::List(_),
            )
            | None => None,
        };
        // Approval reads its revision from --deletion-set; anywhere else that option is left
        // over and refused below.
        let revision = flags.take(if command == Some(Command::Approve) {
            Flag::DeletionSet
        } else {
            Flag::Revision
        });
        if revision.is_some_and(|value| !lower_hex(value, revision_len(command))) {
            return Err(fail(
                "Use the exact lowercase inspected revision: 48 characters for findings, 32 for deletions/approval/conflicts",
            )
            .into());
        }
        let ControlRequest { path, body, latest } = request(command, revision, &mut flags)?;
        flags.finish()?;
        Ok(Self {
            endpoint,
            path,
            body,
            latest,
            token,
            token_file: token_file.map(PathBuf::from),
            timeout: Duration::from_millis(timeout),
            wait,
        })
    }

    /// The options that reach this server again, as a suggested command spells them: none when
    /// the command found the server in the environment and needs no CA or token file.
    fn connection(&self) -> String {
        let token_file = self
            .token_file
            .as_ref()
            .map(|path| format!("--token-file {}", path.display()));
        let mut options = self.endpoint.options();
        options.extend(token_file);
        options.join(" ")
    }

    /// Asks the server to wait, at most a minute at a time, until the wait succeeds, ends for a
    /// reason that only someone acting can change, or `timeout` passes by `elapsed`, the time the
    /// wait has spent; returns the last answer. Each continuation is reported on standard error,
    /// so standard output keeps one JSON answer.
    fn wait_until_settled(
        &self,
        unmapped: UnmappedObjects,
        mut elapsed: impl FnMut() -> Duration,
    ) -> Result<Reply, CliError> {
        loop {
            // A request that would start with no time left still asks for the shortest wait the
            // server accepts, rather than for none, which it rejects.
            let slice = self
                .timeout
                .saturating_sub(elapsed())
                .clamp(Duration::from_millis(1), SERVER_WAIT);
            let millis = u64::try_from(slice.as_millis()).unwrap_or(u64::MAX);
            let reply = self.execute(
                &self.path,
                Some(&wait_body(unmapped, millis)),
                slice + WAIT_MARGIN,
            )?;
            let Some(reason) = WaitOutcome::read(&reply.body)
                .reason
                .filter(|reason| reason.is_in_progress())
            else {
                return Ok(reply);
            };
            let left = self.timeout.saturating_sub(elapsed());
            if left.is_zero() {
                return Ok(reply);
            }
            let mut progress = Diagnostics::default();
            progress.info.push(Diagnostic::new(
                DiagnosticSource::Cli,
                "sync-wait",
                format!(
                    "sync wait is still waiting ({reason}); asking the server again with {:.1}s of --timeout left",
                    left.as_secs_f64()
                ),
            ));
            progress.write(io::stderr().lock())?;
            if reason == WaitReason::JournalChanged {
                std::thread::sleep(JOURNAL_PAUSE.min(left));
            }
        }
    }

    /// Reads `page` at the revision that the status reports, and once more if that revision
    /// changed before the page was read, at the one the status then reports.
    fn latest_page(&self, page: &LatestPage) -> Result<Reply, CliError> {
        let reply = self.page_at_reported_revision(page)?;
        if reply.status == StatusCode::CONFLICT {
            return self.page_at_reported_revision(page);
        }
        Ok(reply)
    }

    fn page_at_reported_revision(&self, page: &LatestPage) -> Result<Reply, CliError> {
        let status = self.execute(STATUS_PATH, None, self.timeout)?;
        // A status the command cannot read a revision from is the answer: it is printed, and a
        // server of another release is reported as such.
        if !status.status.is_success()
            || handshake::check(&status.headers, handshake::Peer::Server).is_err()
        {
            return Ok(status);
        }
        let revision = page.view.revision_in(&status.body)?;
        let path = page.paging.path(Listing::View(page.view), Some(revision));
        self.execute(&path, None, self.timeout)
    }

    fn execute(
        &self,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Reply, CliError> {
        let client = match &self.endpoint.transport {
            Transport::Https {
                ca_cert: Some(path),
            } => Some(tls::client_config(path)?),
            Transport::Https { ca_cert: None } => Some(tls::development_client_config()?),
            Transport::Http => None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(async {
            tokio::time::timeout(timeout, async {
                let addresses = tokio::net::lookup_host(self.endpoint.authority.as_str()).await?;
                let mut checked = Vec::new();
                for address in addresses {
                    if !is_local_address(address.ip()) || checked.len() == 8 {
                        return Err(CliError::from(fail(
                            "Endpoint must resolve only to local addresses, at most eight",
                        )));
                    }
                    checked.push(address);
                }
                let mut connected = None;
                let mut refused = None;
                for address in checked {
                    match tokio::net::TcpStream::connect(address).await {
                        Ok(stream) => {
                            connected = Some(stream);
                            break;
                        }
                        Err(error) => refused = Some(error),
                    }
                }
                let stream = match (connected, refused) {
                    (Some(stream), _) => stream,
                    (None, Some(error)) => return Err(self.endpoint.unreachable(error)),
                    (None, None) => return Err(fail("Endpoint has no local address").into()),
                };
                if let Some(client) = client {
                    let name = tokio_rustls::rustls::pki_types::ServerName::try_from(
                        self.endpoint.host.clone(),
                    )
                    .map_err(|_| fail("Invalid TLS hostname"))?;
                    let stream = tokio_rustls::TlsConnector::from(client)
                        .connect(name, stream)
                        .await?;
                    self.exchange(stream, path, body).await
                } else {
                    self.exchange(stream, path, body).await
                }
            })
            .await
            .map_err(|_| self.endpoint.unanswered(timeout))?
        });
        // A timed-out resolver must not keep this one-shot command alive.
        runtime.shutdown_background();
        result
    }

    async fn exchange(
        &self,
        stream: impl AsyncRead + AsyncWrite + Unpin,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Reply, CliError> {
        let (mut sender, connection) = http1::Builder::new()
            .max_headers(64)
            .max_buf_size(16 * 1024)
            .handshake(TokioIo::new(stream))
            .await
            .map_err(io::Error::other)?;
        let mut request = Request::builder()
            .method(if body.is_some() { "POST" } else { "GET" })
            .uri(path)
            .header("Host", self.endpoint.authority.as_str())
            .header("Connection", "close")
            .header("Accept", "application/json")
            .header(handshake::VERSION_HEADER, handshake::version());
        if let Some(token) = &self.token {
            request = request.header("Authorization", token);
        }
        let body = match body {
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
            let (head, mut body) = response.into_parts();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.map_err(io::Error::other)?.into_data() {
                    if data.len() > RESPONSE_LIMIT - bytes.len() {
                        return Err(fail("Synchronization response exceeds 256 KiB").into());
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            // A non-JSON answer has no advice of its own, so it is reported generically.
            let body = response_json(head.status, &bytes)
                .map_err(|error| CliError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))?;
            Ok::<_, CliError>(Reply {
                status: head.status,
                body,
                headers: head.headers,
            })
        };
        // Driving both futures locally avoids an unobserved connection task.
        let (_, response) = tokio::try_join!(
            async {
                connection
                    .await
                    .map_err(|error| CliError::Io(io::Error::other(error)))
            },
            response
        )?;
        Ok(response)
    }
}

#[derive(Debug)]
struct NonJsonResponse {
    status: StatusCode,
    cause: serde_json::Error,
}
impl std::fmt::Display for NonJsonResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Synchronization endpoint returned HTTP {} with a non-JSON body",
            self.status
        )
    }
}
impl std::error::Error for NonJsonResponse {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}
fn response_json(status: StatusCode, bytes: &[u8]) -> Result<Value, NonJsonResponse> {
    serde_json::from_slice(bytes).map_err(|cause| NonJsonResponse { status, cause })
}

/// What a command asks first, and for a view page at the latest revision, the page it then reads.
struct ControlRequest {
    path: String,
    body: Option<Value>,
    latest: Option<LatestPage>,
}

impl ControlRequest {
    fn once(path: &str, body: Option<Value>) -> Self {
        Self {
            path: path.to_owned(),
            body,
            latest: None,
        }
    }
}

/// The request of `command` with `revision`, taking the options it reads from `flags`.
fn request(
    command: Option<Command>,
    revision: Option<&str>,
    flags: &mut Flags<'_>,
) -> Result<ControlRequest, Diagnostics> {
    Ok(match (command, revision) {
        (Some(Command::Status), None) => ControlRequest::once(STATUS_PATH, None),
        (Some(Command::Rescan), None) => {
            ControlRequest::once("/_internal/sync/rescan", Some(json!({})))
        }
        // Each request of a wait carries its own body, with the time left at that request.
        (Some(Command::Wait), None) => ControlRequest::once("/_internal/sync/wait", None),
        (Some(Command::Resolve), revision) => ControlRequest::once(
            "/_internal/sync/resolve",
            Some(resolve_body(revision, flags)?),
        ),
        (Some(Command::Approve), revision) => ControlRequest::once(
            "/_internal/sync/approve",
            Some(
                json!({"deletion_set":revision.ok_or_else(|| fail("Approval requires --deletion-set REVISION from an inspected deletion set"))?}),
            ),
        ),
        (Some(Command::List(Listing::View(view))), None) => {
            let paging = Paging::read(flags)?;
            // A cursor belongs to the revision of the page that returned it, which the status
            // may no longer report.
            if paging.cursor.is_some() {
                return Err(fail(
                    "A --cursor continues the pages of one revision; supply the --revision of the page that returned it",
                ));
            }
            ControlRequest {
                path: STATUS_PATH.to_owned(),
                body: None,
                latest: Some(LatestPage { view, paging }),
            }
        }
        (Some(Command::List(listing)), revision) => {
            ControlRequest::once(&Paging::read(flags)?.path(listing, revision), None)
        }
        (Some(Command::Status | Command::Rescan | Command::Wait), Some(_)) | (None, _) => {
            return Err(fail(
                "Use sync status, findings, deletions, conflicts, rescan, approve, resolve or wait with their documented options",
            ));
        }
    })
}

fn timeout_millis(value: &str, limit: TimeoutLimit) -> Result<u64, Diagnostics> {
    let out_of_range = || fail(format!("--timeout must be 1ms through {}", limit.text));
    let millis =
        duration(value, &[("ms", 1), ("s", 1000), ("m", 60_000)]).map_err(|error| match error {
            DurationError::Unit => fail("--timeout requires an integer with ms, s or m suffix"),
            DurationError::Number(_) | DurationError::Overflow => out_of_range(),
        })?;
    (1..=limit.millis)
        .contains(&millis)
        .then_some(millis)
        .ok_or_else(out_of_range)
}

fn resolve_body(revision: Option<&str>, flags: &mut Flags<'_>) -> Result<Value, Diagnostics> {
    let revision = revision.ok_or_else(|| fail("Resolve requires the inspected --revision"))?;
    let action = flags
        .take(Flag::Action)
        .filter(|v| matches!(*v, "restore" | "discard"))
        .ok_or_else(|| fail("Resolve requires --action restore or discard"))?;
    let operation = flags.take(Flag::OperationId).filter(|v| v.len() <= 128 && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
        .ok_or_else(|| fail("Resolve requires --operation-id of at most 128 letters, digits, hyphens or underscores"))?;
    let is_id = |v: &&str| lower_hex(v, 32);
    match (flags.take(Flag::Conflict), flags.take(Flag::Recovery)) {
        (Some(id), None) if is_id(&id) => {
            let generation = flags
                .take(Flag::ExpectedGeneration)
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|n| *n >= 0)
                .ok_or_else(|| {
                    fail("Conflict resolution requires --expected-generation N; zero means absent")
                })?;
            Ok(
                json!({"conflict_id":id,"revision":revision,"action":action,"expected_generation":generation,"operation_id":operation}),
            )
        }
        (None, Some(id)) if is_id(&id) && action == "discard" => {
            let identity = flags
                .take(Flag::TargetIdentity)
                .filter(is_id)
                .ok_or_else(|| fail("Recovery discard requires its inspected --target-identity"))?;
            let confirmation = match (
                flags.take(Flag::ConfirmBackupVerified),
                flags.take(Flag::ConfirmDiscardWithoutBackup),
            ) {
                (Some(value), None) if value == id => "backup_verified",
                (None, Some(value)) if value == id => "discard_without_backup",
                _ => {
                    return Err(fail(
                        "Recovery discard requires exactly one confirmation whose ID equals --recovery",
                    ));
                }
            };
            Ok(
                json!({"recovery_id":id,"revision":revision,"target_identity":identity,"action":action,"confirmation":confirmation,"operation_id":operation}),
            )
        }
        _ => Err(fail(
            "Select exactly one --conflict or --recovery; recovery permits only explicit discard",
        )),
    }
}

/// Whether `value` is exactly `len` lowercase hexadecimal digits, the only spelling the server
/// accepts for revisions and IDs. The library's own check is private to it.
fn lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Why an endpoint cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndpointRefusal {
    Unparsable,
    NotBare,
    CaCertOverHttp,
}

impl std::fmt::Display for EndpointRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unparsable => "Invalid HTTP(S) endpoint origin",
            Self::NotBare => "Supply a bare HTTP(S) origin with an explicit nonzero port",
            Self::CaCertOverHttp => "--ca-cert requires an HTTPS endpoint",
        })
    }
}

/// The control endpoint: a bare HTTP(S) origin with an explicit nonzero port. A CA certificate
/// needs the HTTPS scheme, which is checked after the origin itself.
fn endpoint(
    value: &str,
    ca_cert: Option<&str>,
    source: EndpointSource,
) -> Result<Endpoint, EndpointRefusal> {
    enum Scheme {
        Http,
        Https,
    }
    let uri: Uri = value.parse().map_err(|_| EndpointRefusal::Unparsable)?;
    let scheme = match uri.scheme_str() {
        Some("http") => Scheme::Http,
        Some("https") => Scheme::Https,
        _ => return Err(EndpointRefusal::NotBare),
    };
    let (Some(authority), Some(host)) = (uri.authority(), uri.host()) else {
        return Err(EndpointRefusal::NotBare);
    };
    if uri.port_u16().is_none_or(|port| port == 0)
        || authority.as_str().contains('@')
        || uri.path() != "/"
        || uri.query().is_some()
        || value.contains('#')
    {
        return Err(EndpointRefusal::NotBare);
    }
    let transport = match (scheme, ca_cert) {
        (Scheme::Http, None) => Transport::Http,
        (Scheme::Http, Some(_)) => return Err(EndpointRefusal::CaCertOverHttp),
        (Scheme::Https, ca_cert) => Transport::Https {
            ca_cert: ca_cert.map(PathBuf::from),
        },
    };
    Ok(Endpoint {
        authority: authority.clone(),
        host: host.trim_matches(['[', ']']).to_owned(),
        transport,
        source,
    })
}

fn read_token(path: &str) -> Result<hyper::header::HeaderValue, CliError> {
    let bytes = bounded_file::read(Path::new(path), 8192).map_err(|error| match error {
        bounded_file::ReadError::Io(error) => CliError::Io(error),
        bounded_file::ReadError::Limit => fail("Token file exceeds 8192 bytes").into(),
    })?;
    let token = std::str::from_utf8(&bytes)
        .map_err(|_| fail("Token file must contain UTF-8"))?
        .trim_end_matches(['\r', '\n']);
    Ok(token_value(token, "Token file")?)
}

fn token_value(token: &str, source: &str) -> Result<hyper::header::HeaderValue, Diagnostics> {
    if token.is_empty() || token.len() > 8192 || !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(fail(format!(
            "{source} must contain one nonempty bearer token"
        )));
    }
    // Unreachable: the token is printable ASCII. The failure names the same defect regardless.
    hyper::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| fail(format!("{source} must contain one nonempty bearer token")))
}

#[cfg(test)]
mod tests;
