//! One-shot live controls share the server's authorization and never open its catalog.

use super::{CliError, local_server};
use gcs_mimic::{
    bounded_file,
    config_input::{DurationError, RenamedEnvironmentVariable, duration},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    net::is_local_address,
    sync::{Decision, UnmappedObjects, WaitReason, advice, handshake},
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
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};

const RESPONSE_LIMIT: usize = 256 * 1024;
const HELP: &str = "Usage: gcs-mimic sync status|rescan|wait [--endpoint http(s)://HOST:PORT] [OPTIONS]
       gcs-mimic sync wait [--endpoint URL] [--timeout DURATION] [--require-no-findings]
       gcs-mimic sync status [--endpoint URL] --view deletions|findings [--revision REVISION [--cursor CURSOR]] [--limit N]
       gcs-mimic sync deletions|findings [--endpoint URL] [--revision REVISION [--cursor CURSOR]] [--limit N] (the same pages)
       gcs-mimic sync reject [--endpoint URL] [--deletion-set REVISION] [OPTIONS]
       gcs-mimic sync approve [--endpoint URL] --deletion-set REVISION [OPTIONS]
       gcs-mimic sync conflicts [--endpoint URL] [--revision REVISION] [--cursor CURSOR] [--limit N]
       gcs-mimic sync resolve [--endpoint URL] --conflict ID --revision REV --action restore|discard --expected-generation N --operation-id ID
       gcs-mimic sync resolve [--endpoint URL] --recovery ID --revision REV --target-identity ID --action discard --operation-id ID --confirm-backup-verified ID

Without --endpoint, the command controls the server the configuration gives,
as the healthcheck finds it: GCS_MIMIC_LISTEN, GCS_MIMIC_SCHEME and the config
file GCS_MIMIC_CONFIG selects, with a wildcard listen address reached on
loopback. When no request reaches a server there, it tries once more where the
options of a gcs-mimic server running as the container's PID 1 point, as for
docker run IMAGE --scheme https. The commands it advises then leave --endpoint
out as well. A failed request names the URL it tried and the setting to check.
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
next_cursor unchanged with the revision of its page; approve requires the exact
inspected revision, and stale revisions fail.
Reject keeps the deletion set's objects and writes their files back, recreating
a missing bucket directory; wait answers once they are back. Without
--deletion-set it rejects the set that the status reports as waiting, naming it
on standard error, and fails when none waits, so that a script can run
sync reject && sync wait. Approve deletes the objects. After a failed mount, fix
the mount and restart the server first.
No data directory, ambient proxy or Google credentials are opened; the server
config file is read only to find the server when --endpoint is not given.
Rescan, reject and approve are not retried; a timeout may leave accepted work
running.
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
        match self {
            Self::RequireNoFindings => true,
            Self::Endpoint
            | Self::CaCert
            | Self::TokenFile
            | Self::TimeoutMs
            | Self::Timeout
            | Self::Revision
            | Self::Cursor
            | Self::Limit
            | Self::Conflict
            | Self::Recovery
            | Self::Action
            | Self::ExpectedGeneration
            | Self::TargetIdentity
            | Self::OperationId
            | Self::ConfirmBackupVerified
            | Self::ConfirmDiscardWithoutBackup
            | Self::DeletionSet
            | Self::View => false,
        }
    }
}

/// The refusal of a value given to a switch, which would otherwise read as a misplaced option.
fn switch_value(switch: &str, value: &str) -> Diagnostics {
    fail(format!(
        "{switch} is a switch and takes no value; give it alone, without {value}"
    ))
}

/// What the command line gave an option: a switch stands alone, and any other option has a
/// value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Given<'a> {
    Switch,
    Value(&'a str),
}

/// The options of one command line. Each step takes what it reads; anything left was not
/// accepted by the selected operation.
struct Flags<'a>(BTreeMap<Flag, Given<'a>>);

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
            let given = if flag.is_switch() {
                if let Some(value) = tail.first().filter(|value| !value.starts_with("--")) {
                    return Err(switch_value(name, value));
                }
                rest = tail;
                Given::Switch
            } else {
                let [value, tail @ ..] = tail else {
                    return Err(refused());
                };
                if value.is_empty() || value.len() > 8192 {
                    return Err(refused());
                }
                rest = tail;
                Given::Value(value)
            };
            match flags.entry(flag) {
                btree_map::Entry::Vacant(entry) => {
                    entry.insert(given);
                }
                btree_map::Entry::Occupied(_) => return Err(refused()),
            }
        }
        Ok(Self(flags))
    }

    /// The value given to `flag`, which takes one.
    fn take(&mut self, flag: Flag) -> Option<&'a str> {
        match self.0.remove(&flag)? {
            Given::Value(value) => Some(value),
            // Only a switch stands alone, and a switch is read through `switched`.
            Given::Switch => None,
        }
    }

    /// Whether the switch `flag` was given.
    fn switched(&mut self, flag: Flag) -> bool {
        matches!(self.0.remove(&flag), Some(Given::Switch))
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
    /// `approve` or `reject`, which share every step but the route.
    Decide(Decision),
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

    /// The revision of this view's pages that the status `answer` reports. A refusal names the
    /// status command with `connection`, the options that reached the server.
    fn revision_in<'a>(self, answer: &'a Value, connection: &str) -> Result<&'a str, Diagnostics> {
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
                "Status reported no {field} this command can read; check what it reports: {}",
                advice::with_connection(&advice::Advised::Status.to_string(), connection)
            ))),
        }
    }
}

/// The deletion set that a status answer reports as waiting for a decision. The status gives a
/// remedy for its pending set only while that set waits, so a set that runs, or ran, is none.
struct WaitingSet<'a> {
    revision: &'a str,
    files: Option<u64>,
}

impl<'a> WaitingSet<'a> {
    fn in_status(status: &'a Value) -> Option<Self> {
        status
            .get("pending_deletion_remedy")
            .filter(|remedy| remedy.is_string())?;
        let revision = status
            .get("pending_deletion_revision")
            .and_then(Value::as_str)
            .filter(|revision| lower_hex(revision, DELETION_REVISION_LEN))?;
        Some(Self {
            revision,
            files: status.get("pending_deletion_count").and_then(Value::as_u64),
        })
    }

    fn files(&self) -> String {
        match self.files {
            Some(1) => "1 file".to_owned(),
            Some(count) => format!("{count} files"),
            None => "its files".to_owned(),
        }
    }
}

/// The length of a deletion set's revision, which a decision takes.
const DELETION_REVISION_LEN: usize = 32;

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
/// The wait control, which each request of a wait asks with the time left.
const WAIT_PATH: &str = "/_internal/sync/wait";

impl Command {
    /// `None` names no operation. That is reported where the operation is chosen, after the
    /// endpoint, timeout, token and revision, which is when the command has always reported it.
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "status" => Some(Self::Status),
            "rescan" => Some(Self::Rescan),
            "wait" => Some(Self::Wait),
            "resolve" => Some(Self::Resolve),
            "conflicts" => Some(Self::List(Listing::Conflicts)),
            name => name
                .parse()
                .ok()
                .map(Self::Decide)
                .or_else(|| View::parse(name).map(|view| Self::List(Listing::View(view)))),
        }
    }

    /// The decision a command takes on a deletion set. Such a command names the set with
    /// `--deletion-set`; `--revision` belongs to the pages it was read from, so accepting it
    /// too would give the decision two spellings.
    const fn decision(self) -> Option<Decision> {
        match self {
            Self::Decide(decision) => Some(decision),
            Self::Status | Self::Rescan | Self::Wait | Self::Resolve | Self::List(_) => None,
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
        Some(Command::Status | Command::Decide(_) | Command::Resolve | Command::List(_)) | None => {
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

/// How a `sync wait` that did not end ready exits, so that a script tells what ended it without
/// reading the answer. A ready wait exits 0, and other commands exit 0 on success and 1 on
/// failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WaitFailure {
    /// No answer names how the wait ended, as for an unreachable server or a refused request,
    /// or the server stopped: like any other failed command.
    Failed,
    /// Waiting does not change what holds synchronization back, so someone must act.
    ActionNeeded,
    /// Work was still in progress when the wait's timeout passed.
    TimedOut,
}

impl WaitFailure {
    /// How a wait whose last answer gave `reason`, if any, fails; nothing for one that ended
    /// ready, even while objects it only reports remain.
    const fn of(reason: Option<WaitReason>) -> Option<Self> {
        match reason {
            Some(WaitReason::Ready) => None,
            Some(
                WaitReason::Paused
                | WaitReason::SourceUnrepresentable
                | WaitReason::DeletionApproval
                | WaitReason::FindingsPending,
            ) => Some(Self::ActionNeeded),
            // The command asks again while one of these holds, so a wait that ends with one ran
            // out of time.
            Some(WaitReason::Timeout | WaitReason::SourceChanging | WaitReason::JournalChanged) => {
                Some(Self::TimedOut)
            }
            Some(WaitReason::Stopped) | None => Some(Self::Failed),
        }
    }

    /// The process exit status.
    pub(super) const fn code(self) -> u8 {
        match self {
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
#[derive(Clone, Debug)]
struct Endpoint {
    authority: Authority,
    /// The host for TLS name verification, without IPv6 brackets.
    host: String,
    transport: Transport,
    source: EndpointSource,
}

/// Two endpoints are the same when requests to them go to the same origin, wherever the command
/// found them.
impl PartialEq for Endpoint {
    fn eq(&self, other: &Self) -> bool {
        self.origin() == other.origin()
    }
}

#[derive(Clone, Debug)]
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

    /// The CA certificate `--ca-cert` named.
    fn ca_cert(&self) -> Option<&Path> {
        match self {
            Self::Https { ca_cert } => ca_cert.as_deref(),
            Self::Http => None,
        }
    }

    /// The TLS configuration that verifies the server, or `None` for plain HTTP.
    fn client(&self) -> Result<Option<Arc<tokio_rustls::rustls::ClientConfig>>, ExchangeFailure> {
        match self {
            Self::Https {
                ca_cert: Some(path),
            } => tls::client_config(path).map(Some),
            Self::Https { ca_cert: None } => tls::development_client_config().map(Some),
            Self::Http => Ok(None),
        }
        .map_err(ExchangeFailure::Trust)
    }
}

/// Where the command found its endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndpointSource {
    /// `--endpoint`, which a suggested command names again.
    Given,
    /// The server configuration in the environment and the config file it selects, where a
    /// suggested command finds it again.
    Environment,
    /// The options of the gcs-mimic server running as the container's PID 1, which the command
    /// tried after the configured server could not be reached, and so does a suggested command.
    Container,
}

impl EndpointSource {
    /// What gave the endpoint, as advice names it.
    const fn named_by(self) -> &'static str {
        match self {
            Self::Given => "--endpoint",
            Self::Environment => {
                "the server configuration (GCS_MIMIC_LISTEN, GCS_MIMIC_SCHEME or the config file GCS_MIMIC_CONFIG selects)"
            }
            Self::Container => "the command line of the gcs-mimic server running as PID 1",
        }
    }

    /// What else the user can do when the endpoint was found rather than given.
    const fn instead(self) -> &'static str {
        match self {
            Self::Given => "",
            Self::Environment | Self::Container => ", or supply --endpoint",
        }
    }
}

/// Why one request to the synchronization server got no answer the command can read. Each
/// keeps its cause as `source()`; [`Endpoint::failed`] names the server and what to check.
#[derive(Debug)]
pub(super) enum ExchangeFailure {
    /// The CA certificate that verifies the server could not be loaded.
    Trust(io::Error),
    Resolve(io::Error),
    /// The host resolved to an address outside loopback and private networks, or to more than
    /// eight, which the command refuses before connecting.
    NotLocal,
    NoAddress,
    /// No address of the host accepted a connection.
    Connect(io::Error),
    TlsName(tokio_rustls::rustls::pki_types::InvalidDnsNameError),
    Tls(io::Error),
    Request(hyper::http::Error),
    Http(hyper::Error),
    TooLarge,
    NonJson(NonJsonResponse),
    Timeout(Duration),
}

impl ExchangeFailure {
    /// Whether the request reached no server that could act on it, so that another server may
    /// be tried: nothing answered, or what answered spoke neither the TLS nor the HTTP the
    /// command spoke, as a TLS listener does to plain HTTP.
    fn reached_no_server(&self) -> bool {
        match self {
            Self::Resolve(_) | Self::NoAddress | Self::Connect(_) | Self::Tls(_) => true,
            Self::Http(error) => error.is_parse(),
            Self::Trust(_)
            | Self::NotLocal
            | Self::TlsName(_)
            | Self::Request(_)
            | Self::TooLarge
            | Self::NonJson(_)
            | Self::Timeout(_) => false,
        }
    }
}

impl std::fmt::Display for ExchangeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trust(_) => f.write_str("the CA certificate that verifies it cannot be loaded"),
            Self::Resolve(_) => f.write_str("its host name does not resolve"),
            Self::NotLocal => f.write_str(
                "its host resolves to an address outside loopback and private networks, or to more than eight",
            ),
            Self::NoAddress => f.write_str("its host resolves to no address"),
            Self::Connect(_) => f.write_str("the connection failed"),
            Self::TlsName(_) => f.write_str("its host is not a name TLS can verify"),
            Self::Tls(_) => f.write_str("the TLS handshake failed"),
            Self::Request(_) => f.write_str("the request cannot be formed"),
            Self::Http(_) => f.write_str("the HTTP exchange failed"),
            Self::TooLarge => write!(f, "its answer exceeds {} KiB", RESPONSE_LIMIT / 1024),
            Self::NonJson(answer) => write!(
                f,
                "it answered HTTP {} with a body that is not JSON",
                answer.status.as_u16()
            ),
            Self::Timeout(timeout) => {
                write!(f, "it did not answer within {:.1}s", timeout.as_secs_f64())
            }
        }
    }
}

impl std::error::Error for ExchangeFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Trust(error) | Self::Resolve(error) | Self::Connect(error) | Self::Tls(error) => {
                Some(error)
            }
            Self::TlsName(error) => Some(error),
            Self::Request(error) => Some(error),
            Self::Http(error) => Some(error),
            Self::NonJson(answer) => Some(answer),
            Self::NotLocal | Self::NoAddress | Self::TooLarge | Self::Timeout(_) => None,
        }
    }
}

impl Endpoint {
    /// The origin requests go to.
    fn origin(&self) -> String {
        format!("{}://{}", self.transport.scheme(), self.authority)
    }

    /// A connection to the first address of the host that accepts one. Every address must be
    /// local, and there may be at most eight, before any is tried.
    async fn connect(&self) -> Result<tokio::net::TcpStream, ExchangeFailure> {
        let addresses = tokio::net::lookup_host(self.authority.as_str())
            .await
            .map_err(ExchangeFailure::Resolve)?;
        let mut checked = Vec::new();
        for address in addresses {
            if !is_local_address(address.ip()) || checked.len() == 8 {
                return Err(ExchangeFailure::NotLocal);
            }
            checked.push(address);
        }
        let mut refused = None;
        for address in checked {
            match tokio::net::TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => refused = Some(error),
            }
        }
        Err(refused.map_or(ExchangeFailure::NoAddress, ExchangeFailure::Connect))
    }

    /// The one failure of a request to this server: the origin, what went wrong with its cause,
    /// and the setting to check, which depends on the failure and on what named the server.
    fn failed(&self, failure: ExchangeFailure) -> CliError {
        let named_by = self.source.named_by();
        let instead = self.source.instead();
        let check = match &failure {
            ExchangeFailure::Trust(_) => "check that --ca-cert names a readable PEM certificate of the CA that issued the server's certificate".to_owned(),
            ExchangeFailure::Resolve(_)
            | ExchangeFailure::NotLocal
            | ExchangeFailure::NoAddress
            | ExchangeFailure::TlsName(_) => {
                format!("check the address in {named_by}{instead}")
            }
            ExchangeFailure::Connect(_) | ExchangeFailure::Timeout(_) => {
                format!("check that the server is running at the address in {named_by}{instead}")
            }
            ExchangeFailure::Tls(_) => format!(
                "check that the server serves HTTPS, the scheme in {named_by}, and that --ca-cert names the CA that issued its certificate when it has its own{instead}"
            ),
            ExchangeFailure::Http(_) => format!(
                "check that the server serves {}, the scheme in {named_by}{instead}",
                self.transport.scheme().to_uppercase()
            ),
            ExchangeFailure::TooLarge | ExchangeFailure::NonJson(_) => {
                format!("check that {named_by} names a gcs-mimic server{instead}")
            }
            ExchangeFailure::Request(_) => "report this as a defect of gcs-mimic".to_owned(),
        };
        // An answer that never arrived may still be on its way, unless what answered spoke
        // another protocol: the server may have accepted the request.
        let accepted = match &failure {
            ExchangeFailure::Http(_) | ExchangeFailure::Timeout(_)
                if !failure.reached_no_server() =>
            {
                ". A control it accepted may still be running, so inspect its status before retrying"
            }
            ExchangeFailure::Trust(_)
            | ExchangeFailure::Resolve(_)
            | ExchangeFailure::NotLocal
            | ExchangeFailure::NoAddress
            | ExchangeFailure::Connect(_)
            | ExchangeFailure::TlsName(_)
            | ExchangeFailure::Tls(_)
            | ExchangeFailure::Request(_)
            | ExchangeFailure::Http(_)
            | ExchangeFailure::TooLarge
            | ExchangeFailure::NonJson(_)
            | ExchangeFailure::Timeout(_) => "",
        };
        let causes =
            std::iter::successors(std::error::Error::source(&failure), |cause| cause.source())
                .map(|cause| format!(" ({cause})"))
                .collect::<String>();
        let advice = fail(format!(
            "Cannot use the synchronization server at {}: {failure}{causes}; {check}{accepted}",
            self.origin()
        ));
        CliError::Unreachable(advice, failure)
    }

    /// The options that reach this endpoint again, as a suggested command spells them.
    fn options(&self) -> Vec<String> {
        let endpoint = match self.source {
            EndpointSource::Given => Some(format!("--endpoint {}", self.origin())),
            EndpointSource::Environment | EndpointSource::Container => None,
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

/// The endpoint of the server that `environment`, the config file it selects and `server_args`
/// configure, found as the healthcheck finds its server, through [`local_server::origin`].
fn configured_endpoint(
    server_args: &[String],
    environment: Vec<(OsString, OsString)>,
    ca_cert: Option<&Path>,
    source: EndpointSource,
) -> Result<Endpoint, Diagnostics> {
    const SUPPLY: &str = "supply --endpoint http(s)://HOST:PORT";
    let origin = local_server::origin(server_args, environment).map_err(|unreliable| {
        let mut errors = unreliable.diagnostics.errors;
        errors.push(diagnostic(
            "sync",
            format!(
                "The server configuration has errors, so sync cannot rely on the endpoint it gives ({}); correct them or {SUPPLY}",
                unreliable.origin
            ),
        ));
        Diagnostics {
            errors,
            ..Diagnostics::default()
        }
    })?;
    let named_by = source.named_by();
    endpoint(&origin, ca_cert, source).map_err(|refusal| {
        fail(match refusal {
            EndpointRefusal::CaCertOverHttp => format!(
                "--ca-cert requires an HTTPS endpoint, and {named_by} gives {origin}; set GCS_MIMIC_SCHEME as the server has it or {SUPPLY}"
            ),
            EndpointRefusal::Unparsable | EndpointRefusal::NotBare => format!(
                "The endpoint {origin}, which {named_by} gives, is not one sync can reach; set GCS_MIMIC_LISTEN as the server has it or {SUPPLY}"
            ),
        })
    })
}

struct Options {
    endpoint: Endpoint,
    plan: Plan,
    /// The decision the command takes on a deletion set, which an accepted answer reports on.
    decision: Option<Decision>,
    token: Option<hyper::header::HeaderValue>,
    /// The token file named on the command line, which a suggested command names again.
    token_file: Option<PathBuf>,
    /// One request's deadline, or for a wait, the deadline of the whole wait.
    timeout: Duration,
}

/// What a command asks of the server.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// `sync status`: the summary, whose source entries the command also names on standard error.
    Status,
    /// One request to `path`, with `body` for a control.
    Once { path: String, body: Option<Value> },
    /// `sync wait`, which asks again until its timeout, treating objects whose names the tree
    /// cannot hold by this policy.
    Wait(UnmappedObjects),
    /// A view page at the revision the status reports when the command runs.
    Latest(LatestPage),
    /// A decision given no `--deletion-set`, on the set the status reports as waiting.
    Waiting(Decision),
}

impl Plan {
    fn once(path: &str, body: Option<Value>) -> Self {
        Self::Once {
            path: path.to_owned(),
            body,
        }
    }
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
    let environment: Vec<_> = std::env::vars_os().collect();
    let mut options = Options::parse(args, environment.clone())?;
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
    let (endpoint, reply) = options.reply(environment)?;
    // The JSON document is printed whatever the outcome, as the command always has.
    let line = json_line(&reply.body).map_err(io::Error::other)?;
    io::stdout().lock().write_all(&line)?;
    // A server of another release is the cause of whatever else went wrong, so it comes first.
    if let Err(mismatch) = handshake::check(&reply.headers, handshake::Peer::Server) {
        return Err(fail(mismatch).into());
    }
    let connection = options.connection(&endpoint);
    match options.plan {
        Plan::Wait(_) => WaitOutcome::read(&reply.body).conclude(reply.status, &connection),
        Plan::Status if reply.status.is_success() => {
            status_advice(&reply.body).write(io::stderr().lock())?;
            Ok(())
        }
        Plan::Once { .. } | Plan::Latest(_) | Plan::Waiting(_) if reply.status.is_success() => {
            if let Some(decision) = options.decision {
                decided(decision, &reply.body, &connection).write(io::stderr().lock())?;
            }
            Ok(())
        }
        Plan::Status | Plan::Once { .. } | Plan::Latest(_) | Plan::Waiting(_) => {
            Err(http_failure(reply.status, error_message(&reply.body), &connection).into())
        }
    }
}

/// What the command says once the server accepted `decision` with `answer`. Either decision goes
/// on after the answer, so the command counts what it touches and names the wait that answers
/// once it is done: a rejection writes the set's files back, and an approval deletes its
/// objects, which nothing in synchronization brings back, while their buckets stay.
fn decided(decision: Decision, answer: &Value, connection: &str) -> Diagnostics {
    let count = answer.get("candidate_count").and_then(Value::as_u64);
    let counted = |one: &str, many: &str, unknown: &str| match count {
        Some(1) => format!("1 {one}"),
        Some(count) => format!("{count} {many}"),
        None => unknown.to_owned(),
    };
    let wait = advice::with_connection(&advice::Advised::Wait.to_string(), connection);
    let line = match decision {
        Decision::Approve => diagnostic(
            "sync-approve",
            format!(
                "The server deletes {} of the approved deletion set; their buckets stay, and synchronization cannot bring the objects back; {wait} answers once the deletions are done",
                counted("object", "objects", "the objects")
            ),
        ),
        Decision::Reject => diagnostic(
            "sync-reject",
            format!(
                "The server writes {} of the rejected deletion set back from the objects; {wait} answers once the write-back is done",
                counted("file", "files", "the files")
            ),
        ),
    };
    Diagnostics {
        info: vec![line],
        ..Diagnostics::default()
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
    /// The first source entries a wait that ended on them names, each as `path [reason]`, and
    /// how many there were.
    source_entries: Option<(Vec<String>, u64)>,
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
            source_entries: body.get("source_entries").and_then(listed_source_entries),
            remedy: text(body, "remedy"),
            message: error_message(body),
        }
    }

    /// Ends the command for a wait whose last answer came with `status`: a ready wait writes its
    /// advice and succeeds, and any other fails with its advice and the [`WaitFailure`] of its
    /// reason. An answer that names no reason the command knows fails like any other request.
    fn conclude(&self, status: StatusCode, connection: &str) -> Result<(), CliError> {
        let advice = self.advice(connection);
        match WaitFailure::of(self.reason) {
            None => {
                advice.write(io::stderr().lock())?;
                Ok(())
            }
            Some(WaitFailure::Failed) if !advice.is_err() => {
                Err(http_failure(status, self.message, connection).into())
            }
            Some(
                failure @ (WaitFailure::Failed | WaitFailure::ActionNeeded | WaitFailure::TimedOut),
            ) => Err(CliError::Wait(advice, failure)),
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
                let objects = advice::UnmappedCount::from(count);
                diagnostic(
                    "sync-findings",
                    format!(
                        "{objects} and {} reachable only through the API; list {} with {}",
                        objects.stay(),
                        objects.them(),
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
                let entries = self
                    .source_entries
                    .as_ref()
                    .map_or_else(String::new, |(first, total)| listed_entries(first, *total));
                let remedy = self.remedy.map_or_else(String::new, |remedy| {
                    format!(": {}", advice::with_connection(remedy, connection))
                });
                diagnostic(
                    "sync",
                    format!("sync wait ended with {reason}{paused}{entries}{remedy}"),
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

/// The first entries of a `source_entries` listing, each as `path [reason]`, and their total.
fn listed_source_entries(listing: &Value) -> Option<(Vec<String>, u64)> {
    let text = |value: &Value, key| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let first = listing
        .get("entries")?
        .as_array()?
        .iter()
        .take(LISTED_ENTRIES)
        .filter_map(|entry| {
            Some(format!(
                "{} [{}]",
                text(entry, "path")?,
                text(entry, "reason")?
            ))
        })
        .collect();
    Some((first, listing.get("total")?.as_u64()?))
}

/// The source entries one line names; the rest are counted.
const LISTED_ENTRIES: usize = 5;

/// For `sync status`, a warning that names the first source entries the latest complete scan
/// could not take in, which hold back deletion detection for the whole directory until each is
/// corrected; nothing when there are none. Standard output keeps the JSON answer, which lists
/// more of them.
fn status_advice(body: &Value) -> Diagnostics {
    let warnings = body
        .get("source_entries")
        .and_then(listed_source_entries)
        .filter(|(_, total)| *total > 0)
        .map(|(first, total)| {
            diagnostic(
                "sync-entries",
                format!(
                    "The latest scan could not take in some source entries{}; until each is renamed, replaced or moved away, no deletion in the directory is detected. source_entries in this status lists them with their reasons",
                    listed_entries(&first, total)
                ),
            )
        })
        .into_iter()
        .collect();
    Diagnostics {
        info: Vec::new(),
        warnings,
        errors: Vec::new(),
    }
}

/// The `first` source entries of `total` that a wait ended on, as its error line names them.
fn listed_entries(first: &[String], total: u64) -> String {
    let noun = if total == 1 { "entry" } else { "entries" };
    let more = total.saturating_sub(u64::try_from(first.len()).unwrap_or(u64::MAX));
    let more = if more > 0 {
        format!(", and {more} more")
    } else {
        String::new()
    };
    format!(" ({total} source {noun}: {}{more})", first.join(", "))
}

/// A synchronization control's answer, with the headers that carry the server's version.
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

/// `text` with each control character written as its escape, such as `\u{1b}` for ESC. A line
/// can repeat what a server answered or what the environment and the command line hold: the
/// names of files anyone can put in the sync directory, remedies and messages that embed them,
/// and the version a server reports. Unescaped, such text could move the cursor, recolor or
/// rewrite the terminal that shows the line. The JSON envelope the diagnostics are written in
/// escapes only U+0000 through U+001F; DEL and the C1 controls, such as U+009B (CSI), would pass
/// through it.
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

/// One diagnostic of the command under `key`, with its text escaped by [`printable`]: every line
/// the command writes to standard error is made here, so that none repeats a control character.
fn diagnostic(key: &str, fix: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::new(DiagnosticSource::Cli, key, printable(&fix.to_string()))
}

/// The command's single diagnostic: what went wrong is rendered once, here, for the user.
fn fail(fix: impl std::fmt::Display) -> Diagnostics {
    diagnostic("sync", fix).into()
}

/// Writes JSON as serde_json's compact form does, and escapes DEL and the C1 controls as
/// `\u007f` through `\u009f` as well, which serde_json writes as they are. The answer the command
/// prints repeats names anyone can give a file or an object, which could otherwise drive the
/// terminal that shows it; a JSON reader reads the same string either way.
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

/// `value` as one line of JSON that a terminal shows as it is.
fn json_line(value: &Value) -> Result<Vec<u8>, serde_json::Error> {
    let mut line = Vec::new();
    serde::Serialize::serialize(
        value,
        &mut serde_json::Serializer::with_formatter(&mut line, TerminalSafe),
    )?;
    line.push(b'\n');
    Ok(line)
}

impl Options {
    /// The options of `args`, finding the server in `environment` when no endpoint is given.
    fn parse(args: &[String], environment: Vec<(OsString, OsString)>) -> Result<Self, CliError> {
        // The help is the command's own text, so its lines stay as they are, unescaped.
        let (name, args) = args
            .split_first()
            .ok_or_else(|| Diagnostics::error(DiagnosticSource::Cli, "sync", HELP))?;
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
        let decision = command.and_then(Command::decision);
        if let Some(decision) = decision
            && flags.contains(Flag::Revision)
        {
            return Err(fail(format!(
                "sync {} takes --deletion-set REVISION, not --revision",
                decision.as_str()
            ))
            .into());
        }
        let ca_cert = flags.take(Flag::CaCert).map(Path::new);
        let endpoint = match flags.take(Flag::Endpoint) {
            Some(value) => endpoint(value, ca_cert, EndpointSource::Given).map_err(fail)?,
            None => configured_endpoint(&[], environment, ca_cert, EndpointSource::Environment)?,
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
        // A decision reads its revision from --deletion-set; anywhere else that option is left
        // over and refused below.
        let revision = flags.take(if decision.is_some() {
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
        let plan = request(command, revision, &mut flags)?;
        flags.finish()?;
        Ok(Self {
            endpoint,
            plan,
            decision,
            token,
            token_file: token_file.map(PathBuf::from),
            timeout: Duration::from_millis(timeout),
        })
    }

    /// The options that reach `endpoint` again, as a suggested command spells them: none when
    /// the command found the server without --endpoint and needs no CA or token file.
    fn connection(&self, endpoint: &Endpoint) -> String {
        let token_file = self
            .token_file
            .as_ref()
            .map(|path| format!("--token-file {}", path.display()));
        let mut options = endpoint.options();
        options.extend(token_file);
        options.join(" ")
    }

    /// The command's answer, and the server that gave it. Without --endpoint that is the server
    /// the configuration gives, or, when no request reached a server there, the one the
    /// container's PID 1 runs, as the healthcheck finds it.
    fn reply(&self, environment: Vec<(OsString, OsString)>) -> Result<(Endpoint, Reply), CliError> {
        match self.endpoint.source {
            EndpointSource::Environment => local_server::with_container_fallback(
                self.endpoint.clone(),
                |endpoint| self.answer(endpoint),
                |error| matches!(error, CliError::Unreachable(_, failure) if failure.reached_no_server()),
                |server_args| {
                    let ca_cert = self.endpoint.transport.ca_cert();
                    configured_endpoint(
                        server_args,
                        environment,
                        ca_cert,
                        EndpointSource::Container,
                    )
                    .map_err(CliError::from)
                },
            ),
            EndpointSource::Given | EndpointSource::Container => {
                Ok((self.endpoint.clone(), self.answer(&self.endpoint)?))
            }
        }
    }

    /// The answer of the command's requests to `endpoint`.
    fn answer(&self, endpoint: &Endpoint) -> Result<Reply, CliError> {
        match &self.plan {
            Plan::Wait(unmapped) => {
                let started = std::time::Instant::now();
                self.wait_until_settled(endpoint, *unmapped, || started.elapsed())
            }
            Plan::Latest(page) => self.latest_page(endpoint, page),
            Plan::Waiting(decision) => self.decide_waiting(endpoint, *decision),
            Plan::Status => self.execute(endpoint, STATUS_PATH, None, self.timeout),
            Plan::Once { path, body } => self.execute(endpoint, path, body.as_ref(), self.timeout),
        }
    }

    /// Decides on the set that the status reports as waiting, for a command given no
    /// `--deletion-set`. A rejection names that set and its files on standard error and sends
    /// its revision, which the server still matches against the set that waits then, so a set
    /// replaced in between is refused with 409 as any stale revision is; with no set waiting it
    /// fails, so that a script does not wait for a write-back that never started. An approval
    /// deletes, so it is never sent for a revision nobody inspected: it fails, naming the set
    /// that waits and the view that lists it.
    fn decide_waiting(&self, endpoint: &Endpoint, decision: Decision) -> Result<Reply, CliError> {
        let status = self.execute(endpoint, STATUS_PATH, None, self.timeout)?;
        // A status the command cannot read a set from is the answer, as for a view's page.
        if !status.status.is_success()
            || handshake::check(&status.headers, handshake::Peer::Server).is_err()
        {
            return Ok(status);
        }
        let connection = self.connection(endpoint);
        let waiting = WaitingSet::in_status(&status.body);
        let required =
            "sync approve requires --deletion-set REVISION from an inspected deletion set";
        match (decision, waiting) {
            (Decision::Reject, Some(set)) => {
                let mut rejecting = Diagnostics::default();
                rejecting.info.push(diagnostic(
                    "sync-reject",
                    format!("Rejecting deletion set {} ({})", set.revision, set.files()),
                ));
                rejecting.write(io::stderr().lock())?;
                self.execute(
                    endpoint,
                    decision.path(),
                    Some(&json!({ "deletion_set": set.revision })),
                    self.timeout,
                )
            }
            (Decision::Reject, None) => Err(fail(format!(
                "No deletion set waits for a decision, so there is nothing to reject; the status says what synchronization waits for: {}",
                advice::with_connection(&advice::Advised::Status.to_string(), &connection)
            ))
            .into()),
            (Decision::Approve, Some(set)) => Err(fail(format!(
                "{required}. Deletion set {} waits for a decision, with {}; list it before you approve it: {}",
                set.revision,
                set.files(),
                advice::with_connection(
                    &advice::Advised::Deletions(Some(set.revision)).to_string(),
                    &connection
                )
            ))
            .into()),
            (Decision::Approve, None) => Err(fail(format!(
                "{required}, and no deletion set waits for a decision now"
            ))
            .into()),
        }
    }

    /// Asks the server to wait, at most a minute at a time, until the wait succeeds, ends for a
    /// reason that only someone acting can change, or `timeout` passes by `elapsed`, the time the
    /// wait has spent; returns the last answer. Each continuation is reported on standard error,
    /// so standard output keeps one JSON answer.
    fn wait_until_settled(
        &self,
        endpoint: &Endpoint,
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
                endpoint,
                WAIT_PATH,
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
            progress.info.push(diagnostic(
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
    fn latest_page(&self, endpoint: &Endpoint, page: &LatestPage) -> Result<Reply, CliError> {
        let reply = self.page_at_reported_revision(endpoint, page)?;
        if reply.status == StatusCode::CONFLICT {
            return self.page_at_reported_revision(endpoint, page);
        }
        Ok(reply)
    }

    fn page_at_reported_revision(
        &self,
        endpoint: &Endpoint,
        page: &LatestPage,
    ) -> Result<Reply, CliError> {
        let status = self.execute(endpoint, STATUS_PATH, None, self.timeout)?;
        // A status the command cannot read a revision from is the answer: it is printed, and a
        // server of another release is reported as such.
        if !status.status.is_success()
            || handshake::check(&status.headers, handshake::Peer::Server).is_err()
        {
            return Ok(status);
        }
        let revision = page
            .view
            .revision_in(&status.body, &self.connection(endpoint))?;
        let path = page.paging.path(Listing::View(page.view), Some(revision));
        self.execute(endpoint, &path, None, self.timeout)
    }

    /// One request to `endpoint` and its answer, within `timeout`.
    fn execute(
        &self,
        endpoint: &Endpoint,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Reply, CliError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(async {
            tokio::time::timeout(timeout, self.round_trip(endpoint, path, body))
                .await
                .unwrap_or(Err(ExchangeFailure::Timeout(timeout)))
        });
        // A timed-out resolver must not keep this one-shot command alive.
        runtime.shutdown_background();
        result.map_err(|failure| endpoint.failed(failure))
    }

    async fn round_trip(
        &self,
        endpoint: &Endpoint,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Reply, ExchangeFailure> {
        let client = endpoint.transport.client()?;
        let stream = endpoint.connect().await?;
        let Some(client) = client else {
            return self.exchange(endpoint, stream, path, body).await;
        };
        let name = tokio_rustls::rustls::pki_types::ServerName::try_from(endpoint.host.clone())
            .map_err(ExchangeFailure::TlsName)?;
        let stream = tokio_rustls::TlsConnector::from(client)
            .connect(name, stream)
            .await
            .map_err(ExchangeFailure::Tls)?;
        self.exchange(endpoint, stream, path, body).await
    }

    async fn exchange(
        &self,
        endpoint: &Endpoint,
        stream: impl AsyncRead + AsyncWrite + Unpin,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Reply, ExchangeFailure> {
        let (mut sender, connection) = http1::Builder::new()
            .max_headers(64)
            .max_buf_size(16 * 1024)
            .handshake(TokioIo::new(stream))
            .await
            .map_err(ExchangeFailure::Http)?;
        let mut request = Request::builder()
            .method(if body.is_some() { "POST" } else { "GET" })
            .uri(path)
            .header("Host", endpoint.authority.as_str())
            .header("Connection", "close")
            .header("Accept", "application/json")
            .header(handshake::VERSION_HEADER, handshake::version());
        if let Some(token) = &self.token {
            request = request.header("Authorization", token);
        }
        // A JSON value renders as JSON text, which cannot fail.
        let body = match body {
            Some(body) => {
                request = request.header("Content-Type", "application/json");
                body.to_string()
            }
            None => String::new(),
        };
        let request = request
            .body(Full::new(Bytes::from(body)))
            .map_err(ExchangeFailure::Request)?;
        let response = async {
            let response = sender
                .send_request(request)
                .await
                .map_err(ExchangeFailure::Http)?;
            let (head, mut body) = response.into_parts();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.map_err(ExchangeFailure::Http)?.into_data() {
                    if data.len() > RESPONSE_LIMIT - bytes.len() {
                        return Err(ExchangeFailure::TooLarge);
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            let body = response_json(head.status, &bytes).map_err(ExchangeFailure::NonJson)?;
            Ok(Reply {
                status: head.status,
                body,
                headers: head.headers,
            })
        };
        // Driving both futures locally avoids an unobserved connection task.
        let (_, response) = tokio::try_join!(
            async { connection.await.map_err(ExchangeFailure::Http) },
            response
        )?;
        Ok(response)
    }
}

#[derive(Debug)]
pub(super) struct NonJsonResponse {
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

/// What `command` with `revision` asks, taking the options it reads from `flags`.
fn request(
    command: Option<Command>,
    revision: Option<&str>,
    flags: &mut Flags<'_>,
) -> Result<Plan, Diagnostics> {
    Ok(match (command, revision) {
        (Some(Command::Status), None) => Plan::Status,
        (Some(Command::Rescan), None) => Plan::once("/_internal/sync/rescan", Some(json!({}))),
        // Each request of a wait carries its own body, with the time left at that request.
        (Some(Command::Wait), None) => Plan::Wait(if flags.switched(Flag::RequireNoFindings) {
            UnmappedObjects::Refuse
        } else {
            UnmappedObjects::Report
        }),
        (Some(Command::Resolve), revision) => Plan::once(
            "/_internal/sync/resolve",
            Some(resolve_body(revision, flags)?),
        ),
        (Some(Command::Decide(decision)), Some(revision)) => {
            Plan::once(decision.path(), Some(json!({ "deletion_set": revision })))
        }
        (Some(Command::Decide(decision)), None) => Plan::Waiting(decision),
        (Some(Command::List(Listing::View(view))), None) => {
            let paging = Paging::read(flags)?;
            // A cursor belongs to the revision of the page that returned it, which the status
            // may no longer report.
            if paging.cursor.is_some() {
                return Err(fail(
                    "A --cursor continues the pages of one revision; supply the --revision of the page that returned it",
                ));
            }
            Plan::Latest(LatestPage { view, paging })
        }
        (Some(Command::List(listing)), revision) => {
            Plan::once(&Paging::read(flags)?.path(listing, revision), None)
        }
        (Some(Command::Status | Command::Rescan | Command::Wait), Some(_)) | (None, _) => {
            return Err(fail(
                "Use sync status, findings, deletions, conflicts, rescan, approve, reject, resolve or wait with their documented options",
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
    ca_cert: Option<&Path>,
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
            ca_cert: ca_cert.map(Path::to_path_buf),
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
