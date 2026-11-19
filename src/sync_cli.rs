//! One-shot live controls share the server's authorization and never open its catalog.

use super::CliError;
use gcs_mimic::{
    bounded_file,
    config_input::{DurationError, RenamedEnvironmentVariable, duration, option_pairs},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    net::is_local_address,
    sync::{WaitReason, handshake},
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
    io,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};

const RESPONSE_LIMIT: usize = 256 * 1024;
const HELP: &str = "Usage: gcs-mimic sync status|rescan|wait --endpoint http(s)://HOST:PORT [OPTIONS]
       gcs-mimic sync status --endpoint URL --view deletions|findings --revision REVISION [--cursor CURSOR] [--limit N]
       gcs-mimic sync deletions|findings --endpoint URL --revision REVISION [--cursor CURSOR] [--limit N] (the same pages)
       gcs-mimic sync approve --endpoint URL --deletion-set REVISION [OPTIONS]
       gcs-mimic sync conflicts --endpoint URL [--revision REVISION] [--cursor CURSOR] [--limit N]
       gcs-mimic sync resolve --endpoint URL --conflict ID --revision REV --action restore|discard --expected-generation N --operation-id ID
       gcs-mimic sync resolve --endpoint URL --recovery ID --revision REV --target-identity ID --action discard --operation-id ID --confirm-backup-verified ID

Options: --ca-cert PATH, --token-file PATH, --timeout Nms|Ns|Nm (1ms..60s),
or the numeric --timeout-ms N. Rescan defaults to 30s; other controls default
to 10s. Explicit timeout values replace these defaults.
Wait takes up to 60m and defaults to 10m. It asks the server again, at most a
minute at a time, while the tree is still changing or one request's deadline
passed, and stops at once for a pause, conflicts, deletions awaiting approval
or findings that need action; it then fails with the reason and what to run
next. It succeeds once every change it can reflect is reflected. Objects whose
names the directory cannot hold stay reachable through the API: the answer
counts them in findings.unmapped, and one line on standard error names the
command that lists them. With --require-no-findings true, any such object
fails the wait as FindingsPending.
The endpoint must be a loopback or private-network HTTP(S) origin with an explicit port.
Use HTTPS outside loopback to protect the token. HTTPS verifies
the embedded development CA or the supplied CA and the endpoint hostname.
The token file contains one local bearer token (at most 8192 bytes), overriding
the CLI-only GCSM_SYNC_TOKEN environment variable when set.
Status and deletions print one bounded JSON page. Copy next_cursor unchanged;
approval requires the exact inspected revision, and stale revisions fail.
No data directory, server config, ambient proxy or Google credentials are opened.
Rescan and approval are not retried; a timeout may leave accepted work running.
Inspect status before retrying. Recovery discard requires exactly one matching
--confirm-backup-verified ID or --confirm-discard-without-backup ID.
Unsupported server operations fail visibly.";

/// A sync control option; each takes one value.
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
}

/// The options of one command line. Each step takes what it reads; anything left was not
/// accepted by the selected operation.
struct Flags<'a>(BTreeMap<Flag, &'a str>);

impl<'a> Flags<'a> {
    /// Known options, each once, with a nonempty value of at most 8192 bytes.
    fn read(args: &'a [String]) -> Result<Self, Diagnostics> {
        let refused = || fail("Use unique sync option/value pairs; see sync --help");
        let mut flags = BTreeMap::new();
        for pair in option_pairs(args) {
            let (name, value) = pair.map_err(|_| refused())?;
            let Some(flag) = Flag::parse(name) else {
                return Err(refused());
            };
            if value.is_empty() || value.len() > 8192 {
                return Err(refused());
            }
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
}

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
/// defaults to 10 minutes, which leaves time for a first pass over the thousand objects of a
/// local-development tree, which can take several minutes on a slow bind mount.
fn default_timeout_ms(command: Option<Command>) -> u64 {
    match command {
        Some(Command::Rescan) => 30_000,
        Some(Command::Wait) => 600_000,
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

/// What `sync wait` does about objects whose names the sync directory cannot hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unmapped {
    /// Succeed, and name the command that lists them.
    Report,
    /// Fail as `FindingsPending` while any remain: `--require-no-findings true`.
    Refuse,
}

impl Unmapped {
    fn parse(value: &str) -> Result<Self, Diagnostics> {
        match value.parse::<bool>() {
            Ok(true) => Ok(Self::Refuse),
            Ok(false) => Ok(Self::Report),
            Err(_) => Err(fail("--require-no-findings must be true or false")),
        }
    }

    /// The body of one wait request that asks the server to observe for `millis`.
    fn body(self, millis: u64) -> Value {
        match self {
            Self::Report => json!({"timeout_ms":millis}),
            Self::Refuse => json!({"timeout_ms":millis,"require_no_findings":true}),
        }
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
}

enum Transport {
    Http,
    Https { ca_cert: Option<PathBuf> },
}

impl Endpoint {
    /// The options that reach this endpoint again, as a suggested command spells them.
    fn options(&self) -> String {
        match &self.transport {
            Transport::Http => format!("--endpoint http://{}", self.authority),
            Transport::Https { ca_cert: None } => format!("--endpoint https://{}", self.authority),
            Transport::Https {
                ca_cert: Some(path),
            } => format!(
                "--endpoint https://{} --ca-cert {}",
                self.authority,
                path.display()
            ),
        }
    }
}

struct Options {
    endpoint: Endpoint,
    path: String,
    body: Option<Value>,
    token: Option<hyper::header::HeaderValue>,
    /// The token file named on the command line, which a suggested command names again.
    token_file: Option<PathBuf>,
    /// One request's deadline, or for a wait, the deadline of the whole wait.
    timeout: Duration,
    /// `Some` for `sync wait`, which repeats requests until `timeout`.
    wait: Option<Unmapped>,
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
    let mut options = Options::parse(args)?;
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
    let reply = match options.wait {
        Some(unmapped) => {
            let started = std::time::Instant::now();
            options.wait_until_settled(unmapped, || started.elapsed())?
        }
        None => options.execute(options.body.as_ref(), options.timeout)?,
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
    let outcome = options
        .wait
        .is_some()
        .then(|| WaitOutcome::read(&reply.body));
    let advice = outcome
        .as_ref()
        .map_or_else(Diagnostics::default, |outcome| {
            outcome.advice(&options.connection())
        });
    if reply.status.is_success() {
        advice.write(io::stderr().lock())?;
        return Ok(());
    }
    // A wait that names its reason says what to run next; anything else is reported generically.
    if advice.is_err() {
        return Err(advice.into());
    }
    Err(fail(format!(
        "Synchronization control returned HTTP {}; inspect its JSON response and preserve pending work.",
        reply.status.as_u16()
    ))
    .into())
}

/// What the command reads from a wait's answer to advise its user.
#[derive(Debug, Default, PartialEq, Eq)]
struct WaitOutcome<'a> {
    reason: Option<WaitReason>,
    /// The objects the tree cannot hold, and the findings revision that lists them.
    unmapped: Option<(u64, &'a str)>,
    paused_reason: Option<&'a str>,
    remedy: Option<&'a str>,
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
        }
    }

    /// One line naming the command that lists the objects the tree cannot hold, if any remain,
    /// and for a wait that failed, an error naming its reason and what to run next, with the
    /// options that reached this server filled in.
    fn advice(&self, connection: &str) -> Diagnostics {
        let warnings = self
            .unmapped
            .filter(|(count, _)| *count > 0)
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
                        "{count} {objects} in the API cannot be written to the sync directory because of {names} and {stay} reachable only through the API; list {them} with gcs-mimic sync status --view findings --revision {revision} {connection}"
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
                    format!(": {}", remedy.replace("--endpoint URL", connection))
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
    fn parse(args: &[String]) -> Result<Self, CliError> {
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
        let endpoint = endpoint(
            flags
                .take(Flag::Endpoint)
                .ok_or_else(|| fail("Supply --endpoint http(s)://HOST:PORT"))?,
            flags.take(Flag::CaCert),
        )?;
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
                    .map_or(Ok(Unmapped::Report), Unmapped::parse)?,
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
        let (path, body) = match (command, revision) {
            (Some(Command::Status), None) => ("/_internal/sync".to_owned(), None),
            (Some(Command::Rescan), None) => ("/_internal/sync/rescan".to_owned(), Some(json!({}))),
            // Each request of a wait carries its own body, with the time left at that request.
            (Some(Command::Wait), None) => ("/_internal/sync/wait".to_owned(), None),
            (Some(Command::Resolve), revision) => (
                "/_internal/sync/resolve".to_owned(),
                Some(resolve_body(revision, &mut flags)?),
            ),
            (Some(Command::Approve), revision) => (
                "/_internal/sync/approve".to_owned(),
                Some(
                    json!({"deletion_set":revision.ok_or_else(|| fail("Approval requires --deletion-set REVISION from an inspected deletion set"))?}),
                ),
            ),
            (Some(Command::List(listing)), revision) => {
                (page_path(listing, revision, &mut flags)?, None)
            }
            (Some(Command::Status | Command::Rescan | Command::Wait), Some(_)) | (None, _) => {
                return Err(fail(
                    "Use sync status, findings, deletions, conflicts, rescan, approve, resolve or wait with their documented options",
                )
                .into());
            }
        };
        flags.finish()?;
        Ok(Self {
            endpoint,
            path,
            body,
            token,
            token_file: token_file.map(PathBuf::from),
            timeout: Duration::from_millis(timeout),
            wait,
        })
    }

    /// The options that reach this server again, as a suggested command spells them.
    fn connection(&self) -> String {
        let endpoint = self.endpoint.options();
        match &self.token_file {
            Some(path) => format!("{endpoint} --token-file {}", path.display()),
            None => endpoint,
        }
    }

    /// Asks the server to wait, at most a minute at a time, until the wait succeeds, ends for a
    /// reason that only someone acting can change, or `timeout` passes by `elapsed`, the time the
    /// wait has spent; returns the last answer. Each continuation is reported on standard error,
    /// so standard output keeps one JSON answer.
    fn wait_until_settled(
        &self,
        unmapped: Unmapped,
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
            let reply = self.execute(Some(&unmapped.body(millis)), slice + WAIT_MARGIN)?;
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

    fn execute(&self, body: Option<&Value>, timeout: Duration) -> Result<Reply, CliError> {
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
                let mut connected: Result<tokio::net::TcpStream, CliError> =
                    Err(fail("Endpoint has no local address").into());
                for address in checked {
                    connected = tokio::net::TcpStream::connect(address)
                        .await
                        .map_err(CliError::from);
                    if connected.is_ok() {
                        break;
                    }
                }
                let stream = connected?;
                if let Some(client) = client {
                    let name = tokio_rustls::rustls::pki_types::ServerName::try_from(
                        self.endpoint.host.clone(),
                    )
                    .map_err(|_| fail("Invalid TLS hostname"))?;
                    let stream = tokio_rustls::TlsConnector::from(client)
                        .connect(name, stream)
                        .await?;
                    self.exchange(stream, body).await
                } else {
                    self.exchange(stream, body).await
                }
            })
            .await
            .map_err(|_| {
                CliError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Synchronization request timed out",
                ))
            })?
        });
        // A timed-out resolver must not keep this one-shot command alive.
        runtime.shutdown_background();
        result
    }

    async fn exchange(
        &self,
        stream: impl AsyncRead + AsyncWrite + Unpin,
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
            .uri(&self.path)
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

fn page_path(
    listing: Listing,
    revision: Option<&str>,
    flags: &mut Flags<'_>,
) -> Result<String, Diagnostics> {
    let mut query = Vec::new();
    let path = match listing {
        Listing::Conflicts => "/_internal/sync/conflicts",
        Listing::View(view) => {
            query.push(format!("view={}", view.as_str()));
            if revision.is_none() {
                return Err(fail("Pages require the matching --revision from status"));
            }
            "/_internal/sync"
        }
    };
    if let Some(revision) = revision {
        query.push(format!("revision={revision}"));
    }
    if let Some(cursor) = flags.take(Flag::Cursor) {
        query.push(format!(
            "cursor={}",
            gcs_mimic::auth::signature::encode_query_component(cursor)
        ));
    }
    if let Some(limit) = flags.take(Flag::Limit) {
        let limit = limit
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=256).contains(n))
            .ok_or_else(|| fail("--limit must be 1 through 256"))?;
        query.push(format!("limit={limit}"));
    }
    Ok(if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", query.join("&"))
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

/// The control endpoint: a bare HTTP(S) origin with an explicit nonzero port. A CA certificate
/// needs the HTTPS scheme, which is checked after the origin itself.
fn endpoint(value: &str, ca_cert: Option<&str>) -> Result<Endpoint, Diagnostics> {
    enum Scheme {
        Http,
        Https,
    }
    let uri: Uri = value
        .parse()
        .map_err(|_| fail("Invalid HTTP(S) endpoint origin"))?;
    let not_bare = || fail("Supply a bare HTTP(S) origin with an explicit nonzero port");
    let scheme = match uri.scheme_str() {
        Some("http") => Scheme::Http,
        Some("https") => Scheme::Https,
        _ => return Err(not_bare()),
    };
    let (Some(authority), Some(host)) = (uri.authority(), uri.host()) else {
        return Err(not_bare());
    };
    if uri.port_u16().is_none_or(|port| port == 0)
        || authority.as_str().contains('@')
        || uri.path() != "/"
        || uri.query().is_some()
        || value.contains('#')
    {
        return Err(not_bare());
    }
    let transport = match (scheme, ca_cert) {
        (Scheme::Http, None) => Transport::Http,
        (Scheme::Http, Some(_)) => return Err(fail("--ca-cert requires an HTTPS endpoint")),
        (Scheme::Https, ca_cert) => Transport::Https {
            ca_cert: ca_cert.map(PathBuf::from),
        },
    };
    Ok(Endpoint {
        authority: authority.clone(),
        host: host.trim_matches(['[', ']']).to_owned(),
        transport,
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
