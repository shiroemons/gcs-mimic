//! Live synchronization controls never open a data directory or confer fixture authority.

use crate::json::JsonErrorResponse;
use crate::storage::{admin::AdminError, failure::BackendCause};
use crate::{
    api_error::ApiError,
    auth::Identity,
    json,
    server::{RequestBody, RequestCtx, json_response_body},
    storage::sync_state::{
        MAX_PAGE, OpaqueId, Ordinal, PageLimit,
        authority::ControlAuthority,
        deletions::{Candidate, DeletionAuthorization, DeletionFence, DeletionSet},
        observation::{ObservationCursor, PauseReason, Summary},
        root::{RootChange, RootError, RootLease, RootStatus},
    },
    sync::{
        Cause, Decision, MirrorState, Reconciliation, UnmappedObjects,
        advice::AtHand,
        control::{
            Command, ControlError, PauseChange, Registration, Unfinished, WaitReason, WaitResult,
        },
        entries::EntryListing,
        handshake,
    },
};
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Bytes};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{borrow::Cow, fmt, sync::Arc};

const RESPONSE_LIMIT: usize = 256 * 1024;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
// Tests hold a control at these points; elsewhere they do nothing.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
use tests::{authorization_checkpoint, authorization_wait_checkpoint};

#[cfg(not(all(test, any(target_os = "linux", target_os = "macos"))))]
#[expect(
    clippy::unused_async,
    reason = "the test build's checkpoint awaits a control, and call sites await both builds' versions"
)]
async fn authorization_wait_checkpoint(
    _: &tokio::sync::watch::Sender<crate::server::ObjectActivity>,
) {
}

#[cfg(not(all(test, any(target_os = "linux", target_os = "macos"))))]
#[expect(
    clippy::unused_async,
    reason = "the test build's checkpoint awaits a control, and call sites await both builds' versions"
)]
async fn authorization_checkpoint() {}

pub(super) mod conflicts;
mod cursor;
mod findings;
mod state;
use cursor::SyncCursor;
use state::ControlState;

struct ControlActivity<'a>(RequestCtx<'a>);
impl Drop for ControlActivity<'_> {
    fn drop(&mut self) {
        self.0.finish_object_request();
    }
}

/// A synchronization control, parsed once from the request path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Status,
    Conflicts,
    /// Approves or rejects the deletion set the body names, at the route the decision names.
    Decide(Decision),
    Rescan,
    Resolve,
    Wait,
}

impl Control {
    fn parse(path: &str) -> Option<Self> {
        match path {
            "/_internal/sync" => Some(Self::Status),
            "/_internal/sync/conflicts" => Some(Self::Conflicts),
            "/_internal/sync/rescan" => Some(Self::Rescan),
            "/_internal/sync/resolve" => Some(Self::Resolve),
            "/_internal/sync/wait" => Some(Self::Wait),
            path => Decision::ALL
                .into_iter()
                .find(|decision| decision.path() == path)
                .map(Self::Decide),
        }
    }

    /// The method the control is documented with.
    fn method(self) -> Method {
        match self {
            Self::Status | Self::Conflicts => Method::GET,
            Self::Decide(_) | Self::Rescan | Self::Resolve | Self::Wait => Method::POST,
        }
    }
}

/// The controls that change synchronization state with a JSON command body.
#[derive(Clone, Copy)]
enum Mutation {
    Decide(Decision),
    Rescan,
}

async fn authorize_controls(
    request: &Request<RequestBody>,
    control: Option<Control>,
    root: &RootLease,
    identity: Option<Identity>,
    ctx: RequestCtx<'_>,
) -> Result<tokio::time::Instant, ApiError> {
    let deadline = ctx.config.catalog_wait_timeout.deadline();
    let authorize = async {
        authorization_wait_checkpoint(ctx.activity).await;
        root.authorize_controls(ctx.config.seed_project.clone(), identity)
            .await
            .map_err(|error| root_error(&error))
    };
    if control == Some(Control::Wait) && request.method() == Method::POST {
        crate::server::begin_catalog_wait_until(ctx.activity, Some(deadline)).await;
        tokio::time::timeout_at(deadline, authorize)
            .await
            .map_err(|_| busy())??;
    } else {
        authorize.await?;
    }
    Ok(deadline)
}

/// The body of an approval or a rejection: the deletion set it decides.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionBody {
    deletion_set: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rescan {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wait {
    timeout_ms: Option<u64>,
    require_no_findings: Option<bool>,
}

/// The answer to `POST /_internal/sync/wait`: the mirror's result, the findings it leaves behind
/// with the revision that lists them, and for a wait that failed, what lets the next one succeed.
#[derive(Serialize)]
struct WaitAnswer<'a> {
    #[serde(flatten)]
    result: &'a WaitResult,
    findings: Option<WaitFindingsAnswer>,
    /// The reason a paused root recorded, which its remedy follows.
    paused_reason: Option<PauseReason>,
    /// For a wait that ended on source entries no object can take or that could not be read,
    /// the first of them, which its remedy is about, listed as an entry report is.
    source_entries: Option<EntryListing<'a>>,
    remedy: Option<String>,
}

/// The objects the tree cannot hold, and the findings revision whose view lists them.
#[derive(Serialize)]
struct WaitFindingsAnswer {
    unmapped: u64,
    revision: String,
}

/// The source entries a wait answer names; the rest are counted as omitted.
const LISTED_SOURCE_ENTRIES: usize = 5;

/// The first entries of `result`'s own pass, for a wait that ended on them, so that whoever
/// waited sees what to correct.
fn source_entries(result: &WaitResult) -> Option<EntryListing<'_>> {
    let pass = result
        .reconciliation
        .as_ref()
        .filter(|_| result.reason == WaitReason::SourceUnrepresentable)?;
    Some(pass.entries().listing(LISTED_SOURCE_ENTRIES)).filter(|listing| !listing.is_empty())
}

/// The revision that binds the findings view: this server session's ID and the observation
/// revision, so that a restart invalidates it as well as any change of a finding.
fn findings_revision(session: &str, observation_revision: i64) -> String {
    format!("{session}{observation_revision:016x}")
}

/// Every answer carries this release's version, refusals included, so that `gcs-mimic sync`
/// can tell a server of another release from a failed request.
pub(super) async fn respond(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Response<Full<Bytes>> {
    let mut response = control(request, ctx)
        .await
        .unwrap_or_else(ApiError::into_response);
    response
        .headers_mut()
        .insert(handshake::VERSION_HEADER, handshake::version());
    response
}

impl From<handshake::Mismatch> for ApiError {
    fn from(mismatch: handshake::Mismatch) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "versionMismatch",
            mismatch.to_string(),
        )
    }
}

async fn control(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let _activity = ControlActivity(ctx);
    // Before authentication and any catalog wait: a client of another release may not even
    // send its credentials the way this one expects, and the version is the cause to report.
    handshake::check(request.headers(), handshake::Peer::Client)?;
    let control = Control::parse(request.uri().path());
    let (registration, root) = ctx
        .state
        .catalog
        .sync_controls()
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "notFound",
                "Directory synchronization is not running",
            )
        })?;
    let caller = ctx.authenticate(request.headers(), None).await?;
    let identity = caller
        .map(|caller| caller.principal().cloned())
        .transpose()?;
    let preparation_deadline =
        authorize_controls(&request, control, &root, identity.clone(), ctx).await?;
    authorization_checkpoint().await;
    // Only a caller that may use the controls learns which routes exist.
    let Some(control) = control else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "notFound",
            "Unknown synchronization control route",
        ));
    };
    let method = control.method();
    if request.method() != method {
        return Err(ApiError::method_not_allowed(
            "Use the method documented for this synchronization control",
            Some(if method == Method::GET { "GET" } else { "POST" }),
        ));
    }
    match control {
        Control::Conflicts => {
            conflicts::list(
                request,
                &root,
                &registration.session,
                ControlAuthority::new(ctx.config.seed_project.clone(), identity),
            )
            .await
        }
        Control::Resolve => conflicts::resolve(request, registration, identity, ctx).await,
        Control::Status => {
            let page = Page::parse(request.uri().query())?;
            let bytes = crate::bounded_body::read_body(request.into_body(), 1).await?;
            if !bytes.is_empty() {
                return Err(ApiError::invalid_argument(
                    "GET synchronization controls do not accept a body",
                ));
            }
            status(
                root,
                &page,
                ctx.state,
                &registration,
                ControlAuthority::new(ctx.config.seed_project.clone(), identity),
            )
            .await
        }
        Control::Decide(decision) => {
            let mutation = Mutation::Decide(decision);
            mutate(request, mutation, registration, identity, ctx).await
        }
        Control::Rescan => mutate(request, Mutation::Rescan, registration, identity, ctx).await,
        Control::Wait => {
            wait(
                request,
                &registration,
                &root,
                identity,
                ctx,
                preparation_deadline,
            )
            .await
        }
    }
}

async fn wait(
    request: Request<RequestBody>,
    registration: &Registration,
    root: &RootLease,
    identity: Option<Identity>,
    ctx: RequestCtx<'_>,
    preparation_deadline: tokio::time::Instant,
) -> Result<Response<Full<Bytes>>, ApiError> {
    if request.uri().query().is_some() {
        return Err(ApiError::invalid_argument(
            "Synchronization waits do not accept query parameters",
        ));
    }
    let bytes = tokio::time::timeout_at(
        preparation_deadline,
        crate::bounded_body::read_body(request.into_body(), 64 * 1024),
    )
    .await
    .map_err(|_| busy())??;
    let value: Wait = serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::invalid_argument(
            "Send an empty JSON object, timeout_ms from 1 through 60000, or the boolean require_no_findings",
        )
    })?;
    let timeout = value.timeout_ms.unwrap_or(10_000);
    if !(1..=60_000).contains(&timeout) {
        return Err(ApiError::invalid_argument(
            "timeout_ms must be 1 through 60000",
        ));
    }
    let unmapped =
        UnmappedObjects::from_require_no_findings(value.require_no_findings.unwrap_or(false));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
    // The bounded handler deadline replaces the request deadline while the server observes work.
    crate::server::begin_catalog_wait_until(ctx.activity, Some(deadline)).await;
    let (reply, receive) = tokio::sync::oneshot::channel();
    let settling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let authority = ControlAuthority::new(ctx.config.seed_project.clone(), identity.clone());
    let command = Command::Wait {
        deadline,
        settling: settling.clone(),
        reply,
        identity,
    };
    // Time spent in line counts: the caller asked for an answer within this timeout.
    let observed = tokio::time::timeout_at(deadline, submit(registration, command, receive)).await;
    let result = match observed {
        Ok(result) => result?.map_err(|error| control_error(&error))?,
        Err(_) if settling.load(std::sync::atomic::Ordering::Acquire) => {
            WaitResult::unfinished(Unfinished::SourceChanging, None, None)
        }
        Err(_) => WaitResult::timeout(),
    }
    .with_unmapped(unmapped);
    // The mirror answers a paused wait with the reason it paused for, which the catalog may not
    // have recorded, and a wait for approval without the deletion set its remedy names.
    let paused_reason = result.paused_reason();
    let deletion_revision = if result.reason == WaitReason::DeletionApproval {
        root.deletion_set_for(authority)
            .await
            .map_err(|error| root_error(&error))?
            .map(|set| set.revision.to_hex())
    } else {
        None
    };
    let findings = result.findings.map(|findings| WaitFindingsAnswer {
        unmapped: findings.unmapped,
        revision: findings_revision(&registration.session, findings.observation_revision),
    });
    let at_hand = AtHand {
        findings_revision: findings.as_ref().map(|findings| findings.revision.as_str()),
        deletion_revision: deletion_revision.as_deref(),
    };
    let answer = WaitAnswer {
        remedy: result.remedy(at_hand),
        findings,
        paused_reason,
        source_entries: source_entries(&result),
        result: &result,
    };
    let mut response = bounded(&super::encode(&answer)?)?;
    if result.timed_out {
        *response.status_mut() = StatusCode::REQUEST_TIMEOUT
    } else if !result.completed {
        *response.status_mut() = StatusCode::CONFLICT
    }
    Ok(response)
}

/// The answer to an accepted approval or rejection of a deletion set.
#[derive(Serialize)]
struct DecisionAccepted {
    operation_id: OpaqueId,
    candidate_count: u64,
}

/// The answer to an accepted rescan, with the reconciliation it waited for.
#[derive(Serialize)]
struct RescanAccepted<'a> {
    operation_id: String,
    completed: bool,
    reason: Option<WaitReason>,
    reconciliation: &'a Reconciliation,
}

async fn mutate(
    request: Request<RequestBody>,
    mutation: Mutation,
    registration: Registration,
    identity: Option<Identity>,
    ctx: RequestCtx<'_>,
) -> Result<Response<Full<Bytes>>, ApiError> {
    if request.uri().query().is_some() {
        return Err(ApiError::invalid_argument(
            "Synchronization mutations do not accept query parameters",
        ));
    }
    let bytes = crate::bounded_body::read_body(request.into_body(), 64 * 1024).await?;
    let value = match mutation {
        Mutation::Decide(decision) => {
            decide(&bytes, decision, &registration, identity, ctx).await?
        }
        Mutation::Rescan => rescan(&bytes, &registration, identity, ctx).await?,
    };
    let mut response = bounded(&value)?;
    *response.status_mut() = StatusCode::ACCEPTED;
    Ok(response)
}

async fn decide(
    bytes: &[u8],
    decision: Decision,
    registration: &Registration,
    identity: Option<Identity>,
    ctx: RequestCtx<'_>,
) -> Result<Value, ApiError> {
    let body: DecisionBody = serde_json::from_slice(bytes)
        .map_err(|_| ApiError::invalid_argument("Send exactly one string deletion_set revision"))?;
    let Ok(revision) = body.deletion_set.parse::<OpaqueId>() else {
        return Err(ApiError::invalid_argument(
            "deletion_set must be the exact revision from status",
        ));
    };
    let (reply, receive) = tokio::sync::oneshot::channel();
    let command = Command::Decide {
        decision,
        revision,
        reply,
        identity,
    };
    let set = observe(ctx, registration, command, receive)
        .await?
        .map_err(|error| control_error(&error))?;
    super::encode(&DecisionAccepted {
        operation_id: set.revision,
        candidate_count: set.candidate_count,
    })
}

async fn rescan(
    bytes: &[u8],
    registration: &Registration,
    identity: Option<Identity>,
    ctx: RequestCtx<'_>,
) -> Result<Value, ApiError> {
    serde_json::from_slice::<Rescan>(bytes)
        .map_err(|_| ApiError::invalid_argument("Send an empty JSON object to request a rescan"))?;
    let mut operation = [0; 16];
    getrandom::fill(&mut operation).map_err(|_| busy())?;
    let (reply, receive) = tokio::sync::oneshot::channel();
    let command = Command::Rescan { reply, identity };
    let summary = observe(ctx, registration, command, receive)
        .await?
        .map_err(|error| control_error(&error))?;
    super::encode(&RescanAccepted {
        operation_id: crate::hashing::encode_lower(&operation),
        completed: !summary.source_changing && !summary.interrupted,
        reason: summary
            .source_changing
            .then_some(WaitReason::SourceChanging),
        reconciliation: &summary,
    })
}

/// Hands a command to the mirror and waits for its reply. The mirror works through commands one at
/// a time behind a short queue, so a caller waits here for its turn. Dropping this future gives up
/// the place in line, and the mirror stopping closes the queue, which wakes every caller in it.
async fn submit<T>(
    registration: &Registration,
    command: Command,
    receive: tokio::sync::oneshot::Receiver<T>,
) -> Result<T, ApiError> {
    registration
        .sender
        .send(command)
        .await
        .map_err(|_| busy())?;
    receive.await.map_err(|_| busy())
}

async fn observe<T>(
    ctx: RequestCtx<'_>,
    registration: &Registration,
    command: Command,
    receive: tokio::sync::oneshot::Receiver<T>,
) -> Result<T, ApiError> {
    crate::server::begin_catalog_wait_until(ctx.activity, None).await;
    tokio::time::timeout_at(
        ctx.config.object_request_timeout.deadline(),
        submit(registration, command, receive),
    )
    .await
    .map_err(|_| busy())?
}

/// What `GET /_internal/sync` shows.
enum Page {
    /// The status summary; the query is empty.
    Summary,
    Deletions(PageQuery<OpaqueId>),
    /// A findings revision is the session ID followed by the observation revision.
    Findings(PageQuery<String>),
}

/// A paged view: the revision it was fetched for, and where the page starts.
struct PageQuery<R> {
    revision: R,
    limit: PageLimit<MAX_PAGE>,
    after: Option<String>,
}

/// The paged view a status query names, with the revision it was fetched for.
enum PagedRevision {
    Deletions(OpaqueId),
    Findings(String),
}

impl Page {
    fn parse(query: Option<&str>) -> Result<Self, ApiError> {
        let mut values = crate::percent::parse_query(query.unwrap_or_default())?;
        if values.is_empty() {
            return Ok(Self::Summary);
        }
        let revision = values.remove("revision");
        let revision = match values.remove("view").as_deref() {
            Some("deletions") => revision
                .and_then(|value| value.parse().ok())
                .map(PagedRevision::Deletions),
            Some("findings") => revision
                .filter(|value| crate::hex::is_lower(value, 48))
                .map(PagedRevision::Findings),
            _ => {
                return Err(ApiError::invalid_argument(
                    "Use view=deletions or view=findings with the corresponding current revision",
                ));
            }
        }
        .ok_or_else(|| {
            ApiError::invalid_argument("A paged view requires its current revision from status")
        })?;
        let limit = parse_limit(values.remove("limit"))?;
        let after = values.remove("cursor");
        if !values.is_empty() {
            return Err(ApiError::invalid_argument(
                "Unknown synchronization query parameter",
            ));
        }
        Ok(match revision {
            PagedRevision::Deletions(revision) => Self::Deletions(PageQuery {
                revision,
                limit,
                after,
            }),
            PagedRevision::Findings(revision) => Self::Findings(PageQuery {
                revision,
                limit,
                after,
            }),
        })
    }
}

/// A page size from the query; an absent one is the largest page.
fn parse_limit(value: Option<String>) -> Result<PageLimit<MAX_PAGE>, ApiError> {
    let Some(value) = value else {
        return Ok(PageLimit::LARGEST);
    };
    value
        .parse::<usize>()
        .ok()
        .and_then(|limit| PageLimit::try_from(limit).ok())
        .ok_or_else(|| ApiError::invalid_argument("limit must be 1 through 256"))
}

/// A control document. Every one leads with the catalog and root it describes; serde_json's
/// `preserve_order` keeps the declaration order on the wire.
#[derive(Serialize)]
struct RootDocument<B> {
    catalog_id: String,
    root_id: String,
    #[serde(flatten)]
    body: B,
}

impl<B> RootDocument<B> {
    fn new(ids: &RootStatus, body: B) -> Self {
        Self {
            catalog_id: ids.catalog_id.to_string(),
            root_id: ids.root_id.to_string(),
            body,
        }
    }
}

/// The cursor prefix that ties a page cursor to this catalog, root and `scope`.
fn cursor_prefix(ids: &RootStatus, scope: impl fmt::Display) -> String {
    format!("{}{}{scope}", ids.catalog_id, ids.root_id)
}

/// Whether a page lost items to the response limit, so its cursor must resume after its last item.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PageFit {
    Whole,
    Halved,
}

/// Renders the longest prefix of `items` (halving it) whose document fits `RESPONSE_LIMIT`.
/// The closure builds the whole document for one prefix; it returns a `Value` rather than a
/// borrowing page type, because a page that borrows the prefix cannot outlive the call.
fn render_halving<T>(
    mut items: Vec<T>,
    mut document: impl FnMut(&[T], PageFit) -> Result<Value, ApiError>,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let mut fit = PageFit::Whole;
    loop {
        if let Some(response) = render_within_limit(&document(&items, fit)?) {
            return Ok(response);
        }
        if items.len() <= 1 {
            return Err(busy());
        }
        items.truncate(items.len() / 2);
        fit = PageFit::Halved;
    }
}

/// The body of `GET /_internal/sync` without a view.
#[derive(Serialize)]
struct StatusSummary<'a> {
    auth_mode: &'static str,
    state: ControlState,
    /// The same value as `sync.state` in a ready answer, which `state` refines while the mirror
    /// runs.
    mirror_state: MirrorState,
    session_id: &'a str,
    /// The directory this process synchronizes.
    root_path: Cow<'a, str>,
    /// How this process changed the synchronized directory at startup, when it did; it is not
    /// stored, so a later process reports none.
    root_change: Option<&'a RootChange>,
    findings_revision: &'a str,
    observation: &'a Summary,
    pending_deletion_revision: Option<OpaqueId>,
    pending_deletion_count: u64,
    /// While the pending set waits for approval, the commands that inspect, approve and reject
    /// it, as a wait and the log give them.
    pending_deletion_remedy: Option<String>,
    deletion_set: Option<&'a DeletionSet>,
    writeback_safety: &'static str,
    conflict_resolution_available: bool,
    stable_wait_available: bool,
}

/// A page of `GET /_internal/sync?view=findings`.
#[derive(Serialize)]
struct FindingsPage<'a> {
    view: &'static str,
    revision: &'a str,
    total_count: u64,
    items: Vec<Value>,
    next_cursor: Option<String>,
}

/// A page of `GET /_internal/sync?view=deletions`.
#[derive(Serialize)]
struct DeletionPage<'a> {
    view: &'static str,
    revision: OpaqueId,
    total_count: u64,
    scan_revision: OpaqueId,
    journal_fence: &'a DeletionFence,
    baseline_count: u64,
    candidate_count: u64,
    guard_enabled: bool,
    min_objects: u64,
    percent: u64,
    items: &'a [Candidate],
    next_cursor: Option<String>,
}

async fn status(
    root: Arc<RootLease>,
    page: &Page,
    state: &crate::server::State,
    registration: &Registration,
    authority: ControlAuthority,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let session = registration.session.as_str();
    let mut after = Ordinal::START;
    loop {
        let recoveries = root
            .recovery_page(after, PageLimit::LARGEST, authority.clone())
            .await
            .map_err(conflicts::error)?;
        let Some(last) = recoveries.last() else { break };
        after = last.ordinal;
    }
    let (observation, published_set) = match page {
        Page::Summary => root
            .published_observation_for(authority.clone())
            .await
            .map_err(|error| root_error(&error))?,
        Page::Deletions(_) | Page::Findings(_) => (
            root.observation_for(authority.clone())
                .await
                .map_err(|error| root_error(&error))?,
            None,
        ),
    };
    let findings_revision = findings_revision(session, observation.revision);
    match page {
        Page::Findings(query) => {
            if query.revision != findings_revision {
                return Err(stale_findings());
            }
            findings_page(&root, query, &findings_revision, &observation, authority).await
        }
        Page::Deletions(query) => {
            let set = root
                .deletion_set_for(authority.clone())
                .await
                .map_err(|error| root_error(&error))?
                .filter(|set| query.revision == set.revision)
                .ok_or_else(stale)?;
            deletion_page(&root, query, set, authority).await
        }
        Page::Summary => {
            let mirror = registration.status.state();
            let set = published_set.as_ref();
            let pending = set.map_or(0, |set| set.candidate_count - set.completed_count);
            let awaiting = set
                .filter(|set| {
                    pending > 0
                        && set.guarded
                        && set.authorization == DeletionAuthorization::Withheld
                })
                .map(|set| set.revision.to_hex());
            let remedy = awaiting.as_deref().map(|revision| {
                crate::sync::control::deletion_approval_remedy(AtHand {
                    deletion_revision: Some(revision),
                    ..AtHand::default()
                })
            });
            let summary = StatusSummary {
                auth_mode: state.auth.mode().as_str(),
                state: ControlState::of(mirror, &observation, set),
                mirror_state: mirror,
                session_id: session,
                root_path: root.watch_path().to_string_lossy(),
                root_change: root.change(),
                findings_revision: &findings_revision,
                observation: &observation,
                pending_deletion_revision: set.filter(|_| pending > 0).map(|set| set.revision),
                pending_deletion_count: pending,
                pending_deletion_remedy: remedy,
                deletion_set: set,
                writeback_safety: "preserve-pending-work",
                conflict_resolution_available: true,
                stable_wait_available: true,
            };
            bounded(&super::encode(&RootDocument::new(&root.status(), summary))?)
        }
    }
}

async fn findings_page(
    root: &RootLease,
    query: &PageQuery<String>,
    revision: &str,
    observation: &Summary,
    authority: ControlAuthority,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let ids = root.status();
    let prefix = cursor_prefix(&ids, revision);
    let after = match &query.after {
        None => ObservationCursor::START,
        Some(cursor) => {
            // A cursor names its view and revision, so one that does not decode here belongs to
            // another revision or view, which the findings answer names alike.
            let Ok(SyncCursor::Findings(after)) = SyncCursor::decode(cursor, &prefix) else {
                return Err(stale_findings());
            };
            after
        }
    };
    let mut items = root
        .observation_page_for(
            observation.revision,
            after,
            query.limit.with_lookahead(),
            authority,
        )
        .await
        .map_err(|error| {
            if matches!(error, RootError::Admin(AdminError::Precondition)) {
                stale_findings()
            } else {
                root_error(&error)
            }
        })?;
    let full = items.len() > query.limit.get();
    items.truncate(query.limit.get());
    let total_count = observation.mapping_findings
        + observation.pending_writeback
        + observation.path_stops
        + observation.unresolved_conflicts
        + u64::from(observation.paused_reason.is_some())
        + u64::from(observation.scan_stop.is_some());
    render_halving(items, |items, fit| {
        let next_cursor = items
            .last()
            .filter(|_| full || fit == PageFit::Halved)
            .map(|item| {
                SyncCursor::Findings(ObservationCursor {
                    category: item.category,
                    ordinal: item.ordinal,
                })
                .encode(&prefix)
            });
        let at_hand = AtHand {
            findings_revision: Some(revision),
            deletion_revision: None,
        };
        let items = items
            .iter()
            .map(|item| findings::value(item, &prefix, observation.writeback.as_ref(), at_hand))
            .collect::<Result<_, _>>()?;
        super::encode(&RootDocument::new(
            &ids,
            FindingsPage {
                view: "findings",
                revision,
                total_count,
                items,
                next_cursor,
            },
        ))
    })
}

async fn deletion_page(
    root: &RootLease,
    query: &PageQuery<OpaqueId>,
    set: DeletionSet,
    authority: ControlAuthority,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let ids = root.status();
    let prefix = cursor_prefix(&ids, set.revision);
    let after = match &query.after {
        None => Ordinal::START,
        Some(cursor) => {
            let SyncCursor::Deletions(ordinal) = SyncCursor::decode(cursor, &prefix)? else {
                return Err(stale());
            };
            ordinal
        }
    };
    let items = root
        .deletion_page_for(set.revision, after, query.limit, authority)
        .await
        .map_err(|error| root_error(&error))?;
    render_halving(items, |items, _| {
        let last = items.last().map_or(after, |item| item.ordinal);
        let next_cursor = u64::try_from(last.get())
            .is_ok_and(|last| last < set.candidate_count)
            .then(|| SyncCursor::Deletions(last).encode(&prefix));
        super::encode(&RootDocument::new(
            &ids,
            DeletionPage {
                view: "deletions",
                revision: set.revision,
                total_count: set.candidate_count,
                scan_revision: set.scan_revision,
                journal_fence: &set.journal_fence,
                baseline_count: set.baseline_count,
                candidate_count: set.candidate_count,
                guard_enabled: set.guard_enabled,
                min_objects: 100,
                percent: 50,
                items,
                next_cursor,
            },
        ))
    })
}

fn bounded(value: &Value) -> Result<Response<Full<Bytes>>, ApiError> {
    render_within_limit(value).ok_or_else(busy)
}

fn render_within_limit(value: &Value) -> Option<Response<Full<Bytes>>> {
    let body = json::render_body(value);
    (body.len() <= RESPONSE_LIMIT).then(|| json_response_body(value, body))
}
fn stale() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "conflict",
        "Synchronization revision changed; rescan, fetch status and inspect the new revision before approval",
    )
}
/// A findings page leads to no approval, so its stale answer names only the status to read
/// again for the revision that lists the findings now.
fn stale_findings() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "conflict",
        "Synchronization findings changed after status reported this findings_revision; fetch status again and list the findings at the findings_revision it reports",
    )
}
fn busy() -> ApiError {
    ApiError::service_unavailable(
        "Synchronization control is busy or closed; retry after the active operation finishes",
    )
}
pub(super) fn root_error(error: &RootError) -> ApiError {
    match error {
        RootError::Admin(error) => admin_error(error).unwrap_or_else(busy),
        RootError::InUse => busy(),
        RootError::Unsupported => ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "notImplemented",
            "Directory synchronization is unsupported on this platform",
        ),
        RootError::RootChanged
        | RootError::MissingMarker
        | RootError::InvalidMarker
        | RootError::ForeignMarker
        | RootError::ReinitializeDenied
        | RootError::ReplacementOccupied { .. } => ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "Synchronization root cannot be verified; preserve the tree and workspace and inspect server diagnostics",
        ),
    }
}

/// Corrupt stored state needs a restored data directory, whichever control found it.
fn corrupt(cause: &BackendCause) -> ApiError {
    crate::object::ObjectError::Corrupt(cause.clone()).into()
}

/// The response an `AdminError` decides on its own; `None` leaves the decision to the caller.
fn admin_error(error: &AdminError) -> Option<ApiError> {
    Some(match error {
        AdminError::Forbidden => crate::auth::AuthFailure::Forbidden.into(),
        // A protected or missing target means the inspected state changed, as a stale revision.
        AdminError::Precondition | AdminError::Hold | AdminError::NotFound => stale(),
        AdminError::Corrupt(cause) => corrupt(cause),
        AdminError::DiskFull(_) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "disk_full",
            "Free disk space without deleting synchronization data, then retry",
        ),
        AdminError::Io(_) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "io_failure",
            "Restore storage access, then retry",
        ),
        AdminError::Busy { .. }
        | AdminError::Closed
        | AdminError::SeedProjectMismatch
        | AdminError::SeedMetadataBudget { .. }
        | AdminError::SourceChanged { .. }
        | AdminError::Invalid(_)
        | AdminError::Checksum
        | AdminError::TooLarge
        | AdminError::BodyAborted(_)
        | AdminError::Backend(_) => return None,
    })
}

/// The response to a queued control that produced no result. The first root or admin failure in
/// the chain decides it; a control that never ran answers as a closed queue does.
pub(super) fn control_error(error: &ControlError) -> ApiError {
    match error {
        ControlError::Failed(error) => error
            .causes()
            .find_map(|cause| match cause {
                Cause::Root(error) => Some(root_error(error)),
                Cause::Admin(error) => admin_error(error),
                Cause::Writeback(_)
                | Cause::Conflict(_)
                | Cause::Stop(_)
                | Cause::Catalog(_)
                | Cause::Sqlite(_)
                | Cause::Io(_)
                | Cause::Other => None,
            })
            .unwrap_or_else(busy),
        // The caller holds sync control authority, so the answer may name the cause, which is
        // what tells a failed resume apart from a busy queue.
        ControlError::RescanFailed {
            reason,
            change,
            error,
        } => ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            format!(
                "The rescan failed, so synchronization {} with paused_reason {}: {}. {}",
                change.paused(),
                reason.as_str(),
                crate::error::Report(error),
                match change {
                    // The failure was transient, so the same rescan may now succeed; the
                    // `sync_resume_failed` event says the same.
                    PauseChange::Kept => crate::sync::control::resume_retry_remedy(),
                    PauseChange::Began | PauseChange::Recorded => {
                        crate::sync::control::pause_remedy(
                            *reason,
                            crate::sync::advice::AtHand::default(),
                        )
                    }
                },
            ),
        ),
        ControlError::Paused => ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            crate::sync::control::paused_control_remedy(),
        ),
        ControlError::Shutdown => busy(),
    }
}
