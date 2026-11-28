//! Where a request stands, shared through a watch channel between the handler and the connection
//! that bounds it. The connection claims an expiry and a handler starts a catalog wait through the
//! same value, so only one of them can win: detached catalog work would otherwise commit behind a
//! timeout reply. Each adapter renders the expiry in its own error format.

use crate::{
    cause::BackendFailure, object::ObjectError, storage::failure::object_unavailable,
    time_limit::TimeLimit,
};
use tokio::sync::watch;

/// Which limit a request outlasted. Recorded with the claim, so the reply names the limit
/// that actually expired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expiry {
    /// The client left the request body silent past `body_inactivity_timeout_ms`.
    BodyInactivity,
    /// The server had no response ready within `request_timeout_ms`.
    Request,
    /// An enforced object transfer outlasted `object_request_timeout_ms`.
    ObjectRequest,
}

/// Whether a connection waits for another request once its response ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AfterResponse {
    KeepAlive,
    /// hyper closes the connection after the response, or the request ended without one.
    Closes,
}

impl AfterResponse {
    /// Reads the close that the response itself announces, as hyper reads it: a `close` token in
    /// any `Connection` field. A timeout reply and an early rejection of a request body announce
    /// the close there. hyper also closes for reasons on the request side, such as a request that
    /// asks to close; such a connection stays in line only until hyper finishes the close.
    pub(crate) fn declared_by(headers: &hyper::HeaderMap) -> Self {
        let closes = headers
            .get_all(hyper::header::CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|token| token.trim().eq_ignore_ascii_case("close"));
        if closes {
            Self::Closes
        } else {
            Self::KeepAlive
        }
    }
}

/// Where a request stands. One value, changed under the channel lock, so a deadline and the
/// start of a catalog wait cannot both win: detached catalog work would otherwise commit behind
/// a timeout reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RequestStage {
    /// The handler is running.
    #[default]
    Running,
    /// The handler waits for catalog work that has its own budget, so deadlines pause.
    AwaitingCatalog,
    /// The connection chose to answer with a timeout for the limit that expired, and is about to
    /// drop the handler.
    Expired(Expiry),
    /// The handler returned, so hyper may have sent the response head and no timeout reply can
    /// follow.
    Responded,
    /// Hyper dropped the response body, after its last byte or a connection failure, and whether
    /// the connection stays open for another request.
    Finished(AfterResponse),
}

/// Whether the handler is waiting for request body bytes that the client has yet to send. A body
/// of any size arrives at the client's pace: every JSON document may reach 16 MiB, and a pause
/// between its bytes says nothing about the server.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum BodyRead {
    #[default]
    NotPending,
    Pending,
}

/// Whether a response that has started is waiting for the server rather than for the client.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ResponseWork {
    #[default]
    Idle,
    /// A batch part is running, under limits of its own (see [`batch_part_expired`]).
    BatchPart,
}

/// The fields change only through the methods and functions here, which keep the claims on a
/// request exclusive.
#[derive(Clone, Copy, Default)]
pub(crate) struct ObjectActivity {
    stage: RequestStage,
    deadline: Option<tokio::time::Instant>,
    body_read: BodyRead,
    response_work: ResponseWork,
}

/// Who decides how long the request's current step takes, which selects the limit on it.
#[derive(Clone, Copy)]
pub(crate) enum RequestStep {
    /// The client is sending or receiving a body. Only silence tells a dead transfer from a slow
    /// one, so stalls here never count against the request deadline.
    ClientPaced,
    /// The server is working toward a response.
    ServerPaced,
}

impl RequestStep {
    pub(crate) fn timeout_message(self) -> &'static str {
        match self {
            Self::ClientPaced => {
                "request or response body made no progress before body_inactivity_timeout_ms"
            }
            Self::ServerPaced => "no response was ready before request_timeout_ms",
        }
    }

    /// The limit that expires when this step outlasts its limit. A response body's silence
    /// cannot be answered, since its head has left, so a client-paced expiry with a reply is
    /// always a silent request body.
    pub(crate) fn expiry(self) -> Expiry {
        match self {
            Self::ClientPaced => Expiry::BodyInactivity,
            Self::ServerPaced => Expiry::Request,
        }
    }
}

impl ObjectActivity {
    /// A request whose response body hyper dropped.
    pub(crate) fn finished(after: AfterResponse) -> Self {
        Self {
            stage: RequestStage::Finished(after),
            ..Self::default()
        }
    }

    pub(crate) fn stage(&self) -> RequestStage {
        self.stage
    }

    /// The enforced object request deadline, if the request has one.
    pub(crate) fn deadline(&self) -> Option<tokio::time::Instant> {
        self.deadline
    }

    pub(crate) fn response_work(&self) -> ResponseWork {
        self.response_work
    }

    pub(crate) fn step(&self) -> RequestStep {
        match self.stage {
            RequestStage::Responded => RequestStep::ClientPaced,
            RequestStage::Running => match self.body_read {
                BodyRead::Pending => RequestStep::ClientPaced,
                BodyRead::NotPending => RequestStep::ServerPaced,
            },
            RequestStage::AwaitingCatalog
            | RequestStage::Expired(_)
            | RequestStage::Finished(_) => RequestStep::ServerPaced,
        }
    }

    /// Records that the handler returned its response.
    pub(crate) fn respond(&mut self) {
        self.stage = RequestStage::Responded;
    }

    /// Records whether the handler waits on the client for request body bytes, returning whether
    /// that changed.
    pub(super) fn set_body_read(&mut self, read: BodyRead) -> bool {
        std::mem::replace(&mut self.body_read, read) != read
    }

    fn end_catalog_wait(&mut self) {
        if self.stage == RequestStage::AwaitingCatalog {
            self.stage = RequestStage::Running;
        }
    }

    /// Starts an object request whose connection enforces `deadline`, or none. An expiry the
    /// connection already claimed stays visible to later catalog waits.
    pub(crate) fn start_object(&mut self, deadline: Option<tokio::time::Instant>) {
        self.end_catalog_wait();
        self.deadline = deadline;
    }

    /// Forgets the object request, keeping only a claimed expiry: a batch can go on to its next
    /// part, whose catalog wait must not start behind the timeout reply.
    pub(super) fn finish_object(&mut self) {
        let stage = match self.stage {
            RequestStage::Expired(expiry) => RequestStage::Expired(expiry),
            RequestStage::Running
            | RequestStage::AwaitingCatalog
            | RequestStage::Responded
            | RequestStage::Finished(_) => RequestStage::Running,
        };
        *self = Self {
            stage,
            ..Self::default()
        };
    }
}

#[cfg(test)]
impl ObjectActivity {
    /// A request in `stage` whose body read is `body_read`, for tests of the limit the connection
    /// derives from them.
    pub(crate) fn in_stage(stage: RequestStage, body_read: BodyRead) -> Self {
        Self {
            stage,
            body_read,
            ..Self::default()
        }
    }
}

/// What came of the connection's attempt to answer a request with a timeout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExpiryClaim {
    /// The handler will be dropped in favor of a timeout reply.
    Granted,
    /// The request moved on, and the connection's next progress event reflects that.
    Refused,
    /// The response may have started, so the connection can only close.
    ResponseStarted,
}

pub(crate) fn claim_expiry(
    activity: &watch::Sender<ObjectActivity>,
    expiry: Expiry,
) -> ExpiryClaim {
    let mut claim = ExpiryClaim::Refused;
    activity.send_if_modified(|progress| match progress.stage {
        RequestStage::Running => {
            progress.stage = RequestStage::Expired(expiry);
            claim = ExpiryClaim::Granted;
            true
        }
        RequestStage::AwaitingCatalog | RequestStage::Expired(_) | RequestStage::Finished(_) => {
            false
        }
        RequestStage::Responded => {
            claim = ExpiryClaim::ResponseStarted;
            false
        }
    });
    claim
}

/// Resolves with the limit that expired once the connection has claimed this request's expiry.
/// The read guard that `wait_for` returns stays inside, so it cannot block a later update of the
/// channel.
pub(crate) async fn expiry_claimed(activity: &watch::Sender<ObjectActivity>) -> Expiry {
    let mut progress = activity.subscribe();
    let claimed = progress
        .wait_for(|progress| matches!(progress.stage, RequestStage::Expired(_)))
        .await
        .ok()
        .and_then(|progress| match progress.stage {
            RequestStage::Expired(expiry) => Some(expiry),
            RequestStage::Running
            | RequestStage::AwaitingCatalog
            | RequestStage::Responded
            | RequestStage::Finished(_) => None,
        });
    match claimed {
        Some(expiry) => expiry,
        // The wait ends without a claim only once every sender is gone, and the caller holds one.
        None => std::future::pending().await,
    }
}

/// Records progress, which renews the limit on the request's current step: a body chunk moved,
/// or a catalog wait ended.
pub(crate) fn record_progress(activity: &watch::Sender<ObjectActivity>) {
    activity.send_modify(ObjectActivity::end_catalog_wait);
}

/// A batch part running behind a started response. The connection's limit on a silent response
/// body measures the client, so it stops counting until this guard drops; the part answers for
/// its own time through [`batch_part_expired`].
#[must_use = "dropping the guard ends the part's work"]
pub(crate) struct BatchPartWork<'a>(&'a watch::Sender<ObjectActivity>);

impl<'a> BatchPartWork<'a> {
    pub(crate) fn begin(connection: &'a watch::Sender<ObjectActivity>) -> Self {
        connection.send_modify(|progress| progress.response_work = ResponseWork::BatchPart);
        Self(connection)
    }
}

impl Drop for BatchPartWork<'_> {
    fn drop(&mut self) {
        self.0
            .send_modify(|progress| progress.response_work = ResponseWork::Idle);
    }
}

/// Starts a batch part's own record of its progress. A part cannot report to the connection: the
/// response head has left, so a timeout could no longer replace it, and handlers end their
/// object requests by moving the stage back to running.
pub(crate) fn new_batch_part_activity() -> watch::Sender<ObjectActivity> {
    watch::Sender::new(ObjectActivity::default())
}

/// Resolves once a batch part has claimed its timeout, as the connection's watchdog does for a
/// whole request. Progress renews the request limit; an enforced object deadline does not; and
/// neither fires during a catalog wait, whose detached work could commit behind the timeout reply.
pub(crate) async fn batch_part_expired(
    activity: &watch::Sender<ObjectActivity>,
    limit: TimeLimit,
) -> Expiry {
    let mut progress = activity.subscribe();
    let mut request_deadline = limit.deadline();
    loop {
        let current = *progress.borrow_and_update();
        let (at, expiry) = match current.deadline {
            Some(deadline) if deadline < request_deadline => (deadline, Expiry::ObjectRequest),
            _ => (request_deadline, Expiry::Request),
        };
        tokio::select! {
            biased;
            changed = progress.changed() => {
                if changed.is_err() {
                    std::future::pending::<()>().await;
                }
                request_deadline = limit.deadline();
            }
            () = tokio::time::sleep_until(at), if current.stage != RequestStage::AwaitingCatalog => {
                if claim_expiry(activity, expiry) == ExpiryClaim::Granted {
                    return expiry;
                }
            }
        }
    }
}

/// A catalog wait in progress, during which request deadlines are paused. Dropping it ends the
/// wait on every path out of the waiting code, including early returns and cancellation.
#[must_use = "dropping the guard ends the catalog wait"]
pub(crate) struct CatalogWaitGuard<'a>(&'a watch::Sender<ObjectActivity>);

impl Drop for CatalogWaitGuard<'_> {
    fn drop(&mut self) {
        record_progress(self.0);
    }
}

/// Starts a catalog wait.
pub(crate) async fn begin_catalog_wait(
    activity: &watch::Sender<ObjectActivity>,
) -> CatalogWaitGuard<'_> {
    claim_catalog_wait(activity).await;
    CatalogWaitGuard(activity)
}

async fn claim_catalog_wait(activity: &watch::Sender<ObjectActivity>) {
    let mut expired = false;
    activity.send_if_modified(|progress| match progress.stage {
        RequestStage::Running | RequestStage::AwaitingCatalog => {
            progress.stage = RequestStage::AwaitingCatalog;
            true
        }
        RequestStage::Expired(_) => {
            expired = true;
            false
        }
        // No handler is running in these stages, so there is nothing to pause.
        RequestStage::Responded | RequestStage::Finished(_) => false,
    });
    if expired {
        // The connection already answers with a timeout. Detached catalog work started
        // now would commit behind that reply, so wait for the connection to drop this
        // handler instead.
        std::future::pending::<()>().await;
    }
}

/// The two limits on one catalog wait: its own budget and the object request deadline. They
/// are raised by different settings, so a timeout names the one that ended the wait; raising
/// `catalog_wait_timeout_ms` cannot outlast the request deadline.
#[derive(Clone, Copy)]
pub(super) struct CatalogWaitLimit {
    budget: tokio::time::Instant,
    request: tokio::time::Instant,
}

impl CatalogWaitLimit {
    /// A wait that starts now. A disabled `budget` leaves only the request deadline.
    pub(super) fn starting_now(budget: TimeLimit, request: tokio::time::Instant) -> Self {
        Self {
            budget: budget.deadline(),
            request,
        }
    }

    pub(super) fn deadline(self) -> tokio::time::Instant {
        self.budget.min(self.request)
    }

    /// The failure of a wait that reached [`Self::deadline`].
    pub(super) fn expired(self) -> BackendFailure {
        if self.budget < self.request {
            BackendFailure::UploadWaitTimeout
        } else {
            BackendFailure::FinalizationTimeout
        }
    }
}

pub(crate) async fn await_upload_catalog<T, E: From<ObjectError>>(
    operation: impl Future<Output = Result<T, E>>,
    timeout: TimeLimit,
    request_deadline: tokio::time::Instant,
    activity: &watch::Sender<ObjectActivity>,
) -> Result<T, E> {
    let _wait = begin_catalog_wait(activity).await;
    let limit = CatalogWaitLimit::starting_now(timeout, request_deadline);
    // A completed body may commit after this wait expires; only an observed commit is success.
    tokio::time::timeout_at(limit.deadline(), operation)
        .await
        .unwrap_or_else(|_| Err(object_unavailable(limit.expired()).into()))
}

#[cfg(test)]
mod tests;
