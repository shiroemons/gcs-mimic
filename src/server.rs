use crate::{
    auth::{self, Auth, Caller, ListenerRole},
    bucket::{BucketAccess, CatalogError},
    clock::Clock,
    config::{Config, ConnectionScheme},
    http_conditions::{EtagError, etag_not_modified},
    json::{self, JsonError},
    object::{ObjectError, UploadChunk},
    storage::{
        Catalog,
        failure::{BackendFailure, object_unavailable},
    },
    time_limit::TimeLimit,
    tls::{self, ListenerSecurity},
    xml,
};
use handshake::{Handshake, Undetected};
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
    server::conn::http1,
};
use hyper_util::rt::TokioIo;
use std::{
    convert::Infallible,
    future::Future,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::{Notify, Semaphore, futures::Notified, watch},
    task::JoinSet,
};

pub use crate::grpc::v2::ReadHandles;
pub use crate::xml::CorsWarnings;

pub struct State {
    pub pools: WorkPools,
    pub xml_cors_warned_buckets: CorsWarnings,
    pub grpc_read_handles: ReadHandles,
    pub ready: AtomicBool,
    pub clock: Arc<dyn Clock>,
    pub catalog: Catalog,
    pub api_base_url: String,
    pub default_location: String,
    pub auth: Auth,
}

impl State {
    #[cfg(test)]
    pub(crate) fn for_test(catalog: Catalog, auth: Auth) -> Self {
        Self {
            pools: WorkPools::default(),
            xml_cors_warned_buckets: Default::default(),
            grpc_read_handles: Default::default(),
            ready: AtomicBool::new(true),
            clock: Arc::new(crate::clock::SystemClock),
            catalog,
            api_base_url: "http://localhost:4443".into(),
            default_location: "US".into(),
            auth,
        }
    }

    pub fn is_ready(&self) -> bool {
        if !self.catalog.is_healthy() {
            self.ready.store(false, Ordering::Release);
        }
        // A background loop restarting after a panic is a transient gap, so it does not latch.
        self.ready.load(Ordering::Acquire) && self.catalog.background_tasks_running()
    }
}

/// Per-server admission for CPU work and buffered batch envelopes.
#[derive(Clone)]
pub struct WorkPools {
    pub(crate) gzip_decoders: Arc<Semaphore>,
    pub(crate) batches: Arc<Semaphore>,
}

impl WorkPools {
    pub fn new(config: &Config) -> Self {
        Self {
            gzip_decoders: Arc::new(Semaphore::new(
                config.max_blocking_threads.div_ceil(4).max(1),
            )),
            batches: Arc::new(Semaphore::new(batch::BATCH_CONCURRENCY)),
        }
    }
}

impl Default for WorkPools {
    fn default() -> Self {
        Self::new(&Config::default())
    }
}

pub(crate) use crate::json::RESOURCE_BODY_LIMIT;

mod batch;
mod browser;
mod buckets;
mod context;
mod handshake;
mod host;
mod ingress;
mod objects;
mod routing;
mod transport;
#[cfg(unix)]
pub mod unix;
pub(crate) mod upload_drive;
mod xml_read;
pub(crate) use context::{DeadlineWatch, ObjectCtx, RequestCtx};
pub(crate) use ingress::RequestBody;

#[cfg(test)]
tokio::task_local! { static ACCEPT_FAULTS: Arc<std::sync::Mutex<std::collections::VecDeque<i32>>>; }
#[cfg(test)]
tokio::task_local! { static FORCE_HANDLER_PANIC: Arc<AtomicBool>; }

/// A connection task ends with an error only when it panicked or was aborted.
/// hyper polls request handlers inside that task, so one faulty request must
/// close only its own connection; the accept loop keeps serving the others.
fn report_connection_task_failure(error: &tokio::task::JoinError) {
    let (event, message) = if error.is_panic() {
        (
            "connection_task_panicked",
            "a request handler panicked; its connection was closed and the server keeps serving",
        )
    } else {
        (
            "connection_task_cancelled",
            "a connection task was cancelled",
        )
    };
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Error,
        event,
        serde_json::json!({"message": message}),
    );
}

mod acl;
pub(crate) use acl::may_read as may_read_acl;
pub(crate) use acl::readable_object_acls;
mod admin;
pub(crate) mod chunk_body;
pub(crate) mod copy;
mod gzip;
mod notifications;
mod object_body;
mod project_resources;
pub(crate) mod resumable;
mod retention_access;
pub(crate) use retention_access::authorize_update as authorize_retention_update;
mod rewrite;
mod unsupported;

/// The longest pause in client input that lingering close waits through. An
/// interrupted local upload keeps sending without pauses, while an idle client
/// would otherwise hold a connection slot for the whole lingering time.
const LINGER_IDLE: Duration = Duration::from_secs(1);
/// The cap on discarding input from a client that keeps sending after its
/// response, matching nginx's default `lingering_time`.
const LINGER_TIME: Duration = Duration::from_secs(30);
/// How long writing a timeout reply and closing the socket may take. These steps stay bounded
/// even with every timeout setting disabled, so a client that stopped reading cannot keep the
/// connection after the server has given up on it.
const CLOSE_BUDGET: Duration = Duration::from_secs(5);

/// The response for a request whose deadline expires before its handler answers. The
/// connection picks it when the deadline expires, from the limit that expired: the request's
/// route alone cannot tell a silent client from a slow server. Cloud Storage documents 408 and
/// 503 as retryable; the XML API documents a silent socket as 400 `RequestTimeout`, which
/// clients do not retry, and this reply matches that.
struct DeadlineReply {
    api: ReplyApi,
    payload: Payload,
}

/// Which API's error document a timeout reply carries.
#[derive(Clone, Copy, Debug)]
enum ReplyApi {
    Json,
    /// An XML error omits its document for HEAD.
    Xml(xml::ErrorBody),
}

/// What the request sends, which decides how its whole-request limit is reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Payload {
    /// Object data with a body: `/upload/...`, or an XML PUT or POST.
    Upload,
    Other,
}

/// Which limit a request outlasted. Recorded with the claim, so the reply names the limit
/// that actually expired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expiry {
    /// The client left the request body silent past `body_inactivity_timeout_ms`.
    BodyInactivity,
    /// The server had no response ready within `request_timeout_ms`.
    Request,
    /// An enforced object transfer outlasted `object_request_timeout_ms`.
    ObjectRequest,
}

impl Expiry {
    /// The JSON error for this expiry on a request that sends `payload`, naming the setting that
    /// bounds the expired limit.
    fn json_error(self, payload: Payload) -> JsonError {
        match (self, payload) {
            (Self::BodyInactivity, Payload::Upload | Payload::Other) => request_body_silent(),
            // Only a transfer's whole-request limit reports a broken upload: a server that is
            // slow to answer an upload is still a server-side timeout.
            (Self::ObjectRequest, Payload::Upload) => upload_timed_out(),
            (Self::ObjectRequest, Payload::Other) => object_request_timed_out(),
            (Self::Request, Payload::Upload | Payload::Other) => request_timed_out(),
        }
    }
}

fn request_timed_out() -> JsonError {
    JsonError::service_unavailable(
        "The request timed out before a response was ready; retry the request, or raise request_timeout_ms (0 disables it).",
    )
}

/// The JSON API documents 408 `uploadBrokenConnection` as its only request timeout.
fn request_body_silent() -> JsonError {
    JsonError::new(
        StatusCode::REQUEST_TIMEOUT,
        "uploadBrokenConnection",
        "The request timed out: no request body bytes arrived within body_inactivity_timeout_ms; retry the request, or raise body_inactivity_timeout_ms for slow clients (0 disables it).",
    )
}

/// The XML API documents a silent socket as 400 `RequestTimeout`; this repeats its meaning.
const XML_REQUEST_BODY_SILENT: &str = "Your socket connection to the server was not read from or written to within the timeout period: no request body bytes arrived within body_inactivity_timeout_ms; retry the request, or raise body_inactivity_timeout_ms for slow clients (0 disables it).";

fn upload_timed_out() -> JsonError {
    JsonError::new(
        StatusCode::REQUEST_TIMEOUT,
        "uploadBrokenConnection",
        "The upload timed out: it did not finish within object_request_timeout_ms; retry the request, or raise object_request_timeout_ms for large uploads (0 disables it).",
    )
}

/// A copy, rewrite, compose or media read past its whole-request limit: a server-side timeout.
fn object_request_timed_out() -> JsonError {
    JsonError::service_unavailable(
        "The request timed out: it did not finish within object_request_timeout_ms; retry the request, or raise object_request_timeout_ms for large transfers (0 disables it).",
    )
}

pub(crate) fn has_body(headers: &hyper::HeaderMap) -> bool {
    headers.contains_key(hyper::header::TRANSFER_ENCODING)
        || headers
            .get(hyper::header::CONTENT_LENGTH)
            .is_some_and(|length| {
                length
                    .to_str()
                    .ok()
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    .is_none_or(|length| length != 0)
            })
}

impl DeadlineReply {
    fn new(
        request: &Request<Incoming>,
        config: &Config,
        endpoints: &crate::config::EndpointSnapshot,
    ) -> Self {
        let headers = request.headers();
        let api = match xml::select_object_with_endpoints(request.uri(), headers, config, endpoints)
        {
            Ok(Some(_)) => ReplyApi::Xml(xml::ErrorBody::for_method(request.method())),
            Ok(None) | Err(_) => ReplyApi::Json,
        };
        let upload_route = request.uri().path().starts_with("/upload/")
            || (matches!(api, ReplyApi::Xml(_))
                && matches!(*request.method(), Method::PUT | Method::POST));
        let payload = if has_body(headers) && upload_route {
            Payload::Upload
        } else {
            Payload::Other
        };
        Self { api, payload }
    }

    fn xml_error(&self, expiry: Expiry) -> xml::Error {
        match expiry {
            // The XML API lists a silent socket under 400, apart from the JSON API's 408.
            Expiry::BodyInactivity => xml::Error::request_timeout(XML_REQUEST_BODY_SILENT),
            Expiry::Request | Expiry::ObjectRequest => expiry.json_error(self.payload).into(),
        }
    }

    fn response(self, expiry: Expiry) -> Response<ResponseBody> {
        let mut response = match self.api {
            ReplyApi::Json => boxed(expiry.json_error(self.payload).into_response()),
            ReplyApi::Xml(body) => boxed(self.xml_error(expiry).response(body)),
        };
        response.headers_mut().insert(
            "connection",
            hyper::header::HeaderValue::from_static("close"),
        );
        response
    }
}

/// Ends a request when hyper drops its response body, after the last byte is written
/// or the connection fails, so keep-alive idle time is never charged to the request.
struct RequestFinished {
    activity: watch::Sender<ObjectActivity>,
    reads: Arc<ReadActivity>,
}

/// Counts bytes read from a connection, so the idle wait between keep-alive
/// requests can tell when the client starts its next request head.
#[derive(Default)]
pub(crate) struct ReadActivity {
    read: AtomicU64,
    idle_mark: AtomicU64,
    pub(crate) arrived: Notify,
}

impl ReadActivity {
    fn mark_idle(&self) {
        self.idle_mark
            .store(self.read.load(Ordering::Acquire), Ordering::Release);
    }

    fn arrived_since_idle(&self) -> bool {
        self.read.load(Ordering::Acquire) > self.idle_mark.load(Ordering::Acquire)
    }

    pub(crate) fn total(&self) -> u64 {
        self.read.load(Ordering::Acquire)
    }
}

pub(crate) struct CountingIo<S> {
    pub(crate) inner: S,
    pub(crate) reads: Arc<ReadActivity>,
}

impl<S: AsyncRead + Unpin> AsyncRead for CountingIo<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = std::pin::Pin::new(&mut self.inner).poll_read(cx, buf);
        let read = buf.filled().len() - before;
        if read > 0 {
            self.reads.read.fetch_add(read as u64, Ordering::AcqRel);
            self.reads.arrived.notify_one();
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CountingIo<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl RequestFinished {
    fn track(self, body: ResponseBody) -> ResponseBody {
        TrackedBody {
            body,
            _finished: self,
        }
        .boxed_unsync()
    }
}

/// Forwards every body method, including the exact size hint that lets hyper send
/// Content-Length instead of chunked encoding, while owning the request's end marker.
struct TrackedBody {
    body: ResponseBody,
    _finished: RequestFinished,
}

impl hyper::body::Body for TrackedBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, io::Error>>> {
        std::pin::Pin::new(&mut self.body).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}

impl Drop for RequestFinished {
    fn drop(&mut self) {
        // Bytes read after this point begin the next request head.
        self.reads.mark_idle();
        self.activity.send_replace(ObjectActivity {
            stage: RequestStage::Finished,
            ..ObjectActivity::default()
        });
    }
}

/// Where a request stands. One value, changed under the channel lock, so a deadline and the
/// start of a catalog wait cannot both win: detached catalog work would otherwise commit behind
/// a timeout reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RequestStage {
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
    /// Hyper dropped the response body, after its last byte or a connection failure.
    Finished,
}

/// Whether the handler is waiting for request body bytes that the client has yet to send. A body
/// of any size arrives at the client's pace: every JSON document may reach 16 MiB, and a pause
/// between its bytes says nothing about the server.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum BodyRead {
    #[default]
    NotPending,
    Pending,
}

/// Whether a response that has started is waiting for the server rather than for the client.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ResponseWork {
    #[default]
    Idle,
    /// A batch part is running, under limits of its own (see [`batch_part_expired`]).
    BatchPart,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct ObjectActivity {
    stage: RequestStage,
    deadline: Option<tokio::time::Instant>,
    body_read: BodyRead,
    response_work: ResponseWork,
}

/// Who decides how long the request's current step takes, which selects the limit on it.
#[derive(Clone, Copy)]
enum RequestStep {
    /// The client is sending or receiving a body. Only silence tells a dead transfer from a slow
    /// one, so stalls here never count against the request deadline.
    ClientPaced,
    /// The server is working toward a response.
    ServerPaced,
}

impl RequestStep {
    fn timeout_message(self) -> &'static str {
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
    fn expiry(self) -> Expiry {
        match self {
            Self::ClientPaced => Expiry::BodyInactivity,
            Self::ServerPaced => Expiry::Request,
        }
    }
}

impl ObjectActivity {
    fn step(&self) -> RequestStep {
        match self.stage {
            RequestStage::Responded => RequestStep::ClientPaced,
            RequestStage::Running => match self.body_read {
                BodyRead::Pending => RequestStep::ClientPaced,
                BodyRead::NotPending => RequestStep::ServerPaced,
            },
            RequestStage::AwaitingCatalog | RequestStage::Expired(_) | RequestStage::Finished => {
                RequestStep::ServerPaced
            }
        }
    }

    fn end_catalog_wait(&mut self) {
        if self.stage == RequestStage::AwaitingCatalog {
            self.stage = RequestStage::Running;
        }
    }

    /// Forgets the object request, keeping only a claimed expiry: a batch can go on to its next
    /// part, whose catalog wait must not start behind the timeout reply.
    fn finish_object(&mut self) {
        let stage = match self.stage {
            RequestStage::Expired(expiry) => RequestStage::Expired(expiry),
            RequestStage::Running
            | RequestStage::AwaitingCatalog
            | RequestStage::Responded
            | RequestStage::Finished => RequestStage::Running,
        };
        *self = Self {
            stage,
            ..Self::default()
        };
    }
}

/// What came of the connection's attempt to answer a request with a timeout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExpiryClaim {
    /// The handler will be dropped in favor of a timeout reply.
    Granted,
    /// The request moved on, and the connection's next progress event reflects that.
    Refused,
    /// The response may have started, so the connection can only close.
    ResponseStarted,
}

fn claim_expiry(activity: &watch::Sender<ObjectActivity>, expiry: Expiry) -> ExpiryClaim {
    let mut claim = ExpiryClaim::Refused;
    activity.send_if_modified(|progress| match progress.stage {
        RequestStage::Running => {
            progress.stage = RequestStage::Expired(expiry);
            claim = ExpiryClaim::Granted;
            true
        }
        RequestStage::AwaitingCatalog | RequestStage::Expired(_) | RequestStage::Finished => false,
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
async fn expiry_claimed(activity: &watch::Sender<ObjectActivity>) -> Expiry {
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
            | RequestStage::Finished => None,
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
async fn batch_part_expired(activity: &watch::Sender<ObjectActivity>, limit: TimeLimit) -> Expiry {
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

/// What a batch part answers once [`batch_part_expired`] has claimed its timeout. A part's body
/// is already buffered, so it is never an upload.
fn batch_part_timeout(expiry: Expiry) -> Response<ResponseBody> {
    boxed(expiry.json_error(Payload::Other).into_response())
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
    claim_catalog_wait(activity, |_| {}).await;
    CatalogWaitGuard(activity)
}

/// Starts a catalog wait bounded by `deadline` instead of the request deadline, which lasts until
/// the handler finishes its object request.
pub(crate) async fn begin_catalog_wait_until(
    activity: &watch::Sender<ObjectActivity>,
    deadline: Option<tokio::time::Instant>,
) {
    claim_catalog_wait(activity, |progress| progress.deadline = deadline).await;
}

async fn claim_catalog_wait(
    activity: &watch::Sender<ObjectActivity>,
    update: impl FnOnce(&mut ObjectActivity),
) {
    let mut expired = false;
    activity.send_if_modified(|progress| match progress.stage {
        RequestStage::Running | RequestStage::AwaitingCatalog => {
            progress.stage = RequestStage::AwaitingCatalog;
            update(progress);
            true
        }
        RequestStage::Expired(_) => {
            expired = true;
            false
        }
        // No handler is running in these stages, so there is nothing to pause.
        RequestStage::Responded | RequestStage::Finished => false,
    });
    if expired {
        // The connection already answers with a timeout. Detached catalog work started
        // now would commit behind that reply, so wait for the connection to drop this
        // handler instead.
        std::future::pending::<()>().await;
    }
}

pub async fn serve(
    listener: TcpListener,
    config: impl Into<Arc<Config>>,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let config = config.into();
    let security = tls::load_server(&config).map_err(io::Error::other)?;
    serve_with_tls(vec![(listener, security)], config, state, shutdown).await
}

pub(crate) async fn serve_with_tls(
    listeners: Vec<(TcpListener, ListenerSecurity)>,
    config: impl Into<Arc<Config>>,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    serve_with_tls_and_drain(listeners, config, state, shutdown, async { Ok(()) }).await
}

pub(crate) async fn serve_with_tls_and_drain(
    listeners: Vec<(TcpListener, ListenerSecurity)>,
    config: impl Into<Arc<Config>>,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
    background_drain: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let listeners = TcpListeners {
        public: listeners,
        trusted: None,
        grpc: None,
    };
    serve_with_grpc_and_drain(listeners, config, state, shutdown, background_drain).await
}

/// The TCP listeners of one server. They share `max_connections` and every timeout.
pub struct TcpListeners {
    /// The browser-facing listeners, each with how it secures its connections.
    pub public: Vec<(TcpListener, ListenerSecurity)>,
    /// The plain HTTP listener for the application's own servers, where a request without
    /// credentials acts as `trusted_principal`.
    pub trusted: Option<TcpListener>,
    /// The separate plaintext gRPC listener.
    pub grpc: Option<TcpListener>,
}

pub async fn serve_with_grpc_and_drain(
    listeners: TcpListeners,
    config: impl Into<Arc<Config>>,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
    background_drain: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let TcpListeners {
        public,
        trusted,
        grpc,
    } = listeners;
    let public = public.into_iter().map(|(listener, security)| {
        let serving = Serving {
            security,
            role: ListenerRole::Public,
        };
        (transport::Listener::Tcp(listener), serving)
    });
    // Plain HTTP whatever the scheme: the listener is meant for a private container network.
    let trusted = trusted.map(|listener| {
        let serving = Serving {
            security: ListenerSecurity::Plain,
            role: ListenerRole::Trusted,
        };
        (transport::Listener::Tcp(listener), serving)
    });
    serve_transports(
        public.chain(trusted).collect(),
        grpc,
        config.into(),
        state,
        shutdown,
        background_drain,
    )
    .await
}

/// How the HTTP connections a listener accepts are served.
#[derive(Clone)]
struct Serving {
    security: ListenerSecurity,
    role: ListenerRole,
}

impl Serving {
    /// Selects the protocol of a TCP connection accepted `at`. The wait for its first byte ends
    /// at the header read limit counted from `at`, the limit a silent plain HTTP client has.
    async fn accept_tcp(
        self,
        stream: &tokio::net::TcpStream,
        at: tokio::time::Instant,
        config: &Config,
        stopping: &mut watch::Receiver<bool>,
    ) -> Option<Accepted> {
        let first_byte_by = config.header_read_timeout.deadline_from(at);
        match self.security.select(stream, first_byte_by, stopping).await {
            Ok(handshake) => Some(Accepted {
                handshake,
                role: self.role,
                at,
            }),
            Err(Undetected::Silent) => {
                warn_connection_timeout(
                    "request headers were not received before header_read_timeout_ms",
                );
                None
            }
            // A probe that only opens a connection is not a client fault.
            Err(Undetected::Closed | Undetected::Stopping) => None,
        }
    }

    #[cfg(unix)]
    fn accept_unix(self, at: tokio::time::Instant) -> Accepted {
        Accepted {
            handshake: self.security.for_unix_socket(),
            role: self.role,
            at,
        }
    }
}

/// A connection whose protocol is known.
struct Accepted {
    handshake: Handshake,
    role: ListenerRole,
    /// When the listener accepted it. The TLS handshake and a plain connection's first request
    /// head count from here, so detecting the protocol never extends either.
    at: tokio::time::Instant,
}

/// What the requests of one connection learn from the listener that accepted it.
#[derive(Clone, Copy)]
struct Listening {
    scheme: ConnectionScheme,
    role: ListenerRole,
}

impl Listening {
    /// Adds the headers the listener's role requires to every response its service produces,
    /// errors and timeout replies included.
    fn mark_response(self, headers: &mut hyper::HeaderMap) {
        match self.role {
            ListenerRole::Trusted => browser::protect_trusted_response(headers),
            ListenerRole::Public => {}
        }
    }
}

#[cfg(unix)]
pub async fn serve_with_unix_and_drain(
    listener: unix::Listener,
    config: impl Into<Arc<Config>>,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
    background_drain: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let serving = Serving {
        security: ListenerSecurity::Plain,
        role: ListenerRole::Public,
    };
    serve_transports(
        vec![(transport::Listener::Unix(listener), serving)],
        None,
        config.into(),
        state,
        shutdown,
        background_drain,
    )
    .await
}

async fn serve_transports(
    mut listeners: Vec<(transport::Listener, Serving)>,
    grpc: Option<TcpListener>,
    config: Arc<Config>,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
    background_drain: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    // Connections and requests share one configuration instead of cloning it per request.
    if listeners.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "at least one listener is required",
        ));
    }
    let grpc_index = grpc.as_ref().map(|_| listeners.len());
    if let Some(listener) = grpc {
        // gRPC serves its connections itself; its calls always authenticate as on a public
        // listener, so nothing here applies to them.
        let unused = Serving {
            security: ListenerSecurity::Plain,
            role: ListenerRole::Public,
        };
        listeners.push((transport::Listener::Tcp(listener), unused));
    }
    let (stop, _) = watch::channel(false);
    let slots = Arc::new(SlotReclaim::default());
    let max_connections = config.max_connections;
    let mut connections = JoinSet::new();
    let mut next_listener = 0;
    // Later clients stay in the listen backlog until this one has a slot, so a full server asks
    // for one idle slot at a time.
    let mut held: Option<HeldClient<'_>> = None;
    let starter = ConnectionStarter {
        listeners: &listeners,
        grpc_index,
        config: &config,
        state: &state,
        stop: &stop,
        slots: &slots,
    };
    let pause = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(pause);
    let mut paused = false;
    let mut backoff = transport::AcceptBackoff::default();
    tokio::pin!(shutdown);
    let signal_result = loop {
        tokio::select! {
            result = &mut shutdown => break result,
            () = &mut pause, if paused => paused = false,
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = result {
                    report_connection_task_failure(&error);
                }
                if connections.len() < max_connections
                    && let Some(HeldClient { index, stream, .. }) = held.take()
                    && let Err(error) = starter.start(&mut connections, index, stream)
                {
                    break Err(error);
                }
            }
            incoming = std::future::poll_fn(|cx| poll_incoming(&listeners, next_listener, cx)), if held.is_none() && !paused => {
                let (index, stream) = match incoming {
                    Ok(incoming) => incoming,
                    Err((index, error)) => {
                        next_listener = (index + 1) % listeners.len();
                        match transport::classify(&error) {
                            transport::AcceptFailure::Fatal => break Err(error),
                            transport::AcceptFailure::Connection => {},
                            transport::AcceptFailure::Exhausted => {
                                pause.as_mut().reset(tokio::time::Instant::now() + backoff.exhausted(&error));
                                paused = true;
                            }
                        }
                        continue;
                    }
                };
                backoff.accepted();
                next_listener = (index + 1) % listeners.len();
                if connections.len() >= max_connections {
                    held = Some(HeldClient { index, stream, _request: slots.request() });
                } else if let Err(error) = starter.start(&mut connections, index, stream) {
                    break Err(error);
                }
            }
        }
    };
    drop(held);
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Info,
        "stopping",
        serde_json::json!({}),
    );
    state.ready.store(false, Ordering::Release);
    let mut listener_cleanups = Vec::with_capacity(listeners.len());
    for (listener, _) in listeners {
        listener_cleanups.push(listener.close().await);
    }
    stop.send_replace(true);
    let drain_result = drain_connections(&mut connections, config.shutdown_timeout).await;
    let background_result = background_drain.await;
    let mut result = signal_result.and(drain_result);
    for cleanup in listener_cleanups {
        result = crate::diagnostics::combine_unix_socket_cleanup(result, cleanup);
    }
    result = crate::diagnostics::combine_shutdown_results(result, background_result);
    crate::diagnostics::combine_shutdown_results(
        result,
        state.catalog.close().await.map_err(io::Error::other),
    )
}

/// What the accept loop shares with every connection task it starts.
struct ConnectionStarter<'a> {
    listeners: &'a [(transport::Listener, Serving)],
    grpc_index: Option<usize>,
    config: &'a Arc<Config>,
    state: &'a Arc<State>,
    stop: &'a watch::Sender<bool>,
    slots: &'a Arc<SlotReclaim>,
}

impl ConnectionStarter<'_> {
    fn start(
        &self,
        connections: &mut JoinSet<()>,
        index: usize,
        stream: transport::Stream,
    ) -> io::Result<()> {
        let Some((_, serving)) = self.listeners.get(index) else {
            return Err(io::Error::other("Accepted connection has no listener"));
        };
        let serving = serving.clone();
        let grpc_index = self.grpc_index;
        let state = self.state.clone();
        let config = self.config.clone();
        let mut stopping = self.stop.subscribe();
        let slots = self.slots.clone();
        let accepted_at = tokio::time::Instant::now();
        let connection = async move {
            match stream {
                transport::Stream::Tcp(stream) if grpc_index == Some(index) => {
                    crate::grpc::serve_connection(stream, &config, state, stopping, &slots).await;
                }
                transport::Stream::Tcp(stream) => {
                    let Some(accepted) = serving
                        .accept_tcp(&stream, accepted_at, &config, &mut stopping)
                        .await
                    else {
                        return;
                    };
                    serve_http(stream, accepted, state, config, stopping, slots).await;
                }
                #[cfg(unix)]
                transport::Stream::Unix(stream) => {
                    let accepted = serving.accept_unix(accepted_at);
                    serve_http(stream, accepted, state, config, stopping, slots).await;
                }
            }
        };
        connections.spawn(with_test_hooks(connection));
        Ok(())
    }
}

/// Hands a connection slot from the longest-idle connection of any protocol to a client waiting
/// at a full server, as nginx reuses idle connections. Closing an idle HTTP/1 connection races
/// with a pooled client reusing it, and a POST that loses that race fails, so only as many close
/// as clients need a slot. An idle gRPC connection answers with GOAWAY, which spares a racing
/// call but still costs its client a new connection.
#[derive(Default)]
pub(crate) struct SlotReclaim {
    /// True while a `SlotRequest` lives and no connection has taken it.
    wanted: AtomicBool,
    /// Idle connections wait here in the order they became idle; `notify_one` wakes the oldest.
    /// With nobody waiting it stores a wake for the next connection to become idle.
    idle_line: Notify,
}

/// A client accepted while every slot is taken.
struct HeldClient<'a> {
    index: usize,
    stream: transport::Stream,
    _request: SlotRequest<'a>,
}

/// Asks idle connections for a slot until it drops, when the client got one by any route.
pub(crate) struct SlotRequest<'a>(&'a SlotReclaim);

impl Drop for SlotRequest<'_> {
    fn drop(&mut self) {
        self.0.wanted.store(false, Ordering::Release);
    }
}

impl SlotReclaim {
    pub(crate) fn request(&self) -> SlotRequest<'_> {
        self.wanted.store(true, Ordering::Release);
        self.idle_line.notify_one();
        SlotRequest(self)
    }

    pub(crate) fn join_line(&self) -> IdleTurn<'_> {
        let mut turn = Box::pin(self.idle_line.notified());
        // An unpolled `Notified` is not in line yet, and the line's order is the point.
        turn.as_mut().enable();
        IdleTurn { slots: self, turn }
    }

    /// A wake can outlive the request that sent it, when another connection ended first and
    /// gave the client a slot. Only the connection that takes the request closes.
    pub(crate) fn take_request(&self) -> bool {
        self.wanted.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn is_wanted(&self) -> bool {
        self.wanted.load(Ordering::Acquire)
    }
}

/// A place in the idle line, which a connection holds for as long as it stays idle.
pub(crate) struct IdleTurn<'a> {
    slots: &'a SlotReclaim,
    turn: std::pin::Pin<Box<Notified<'a>>>,
}

impl IdleTurn<'_> {
    /// Resolves at the front of the idle line. The place in line outlives this future, so
    /// `select!` can drop it at any time.
    pub(crate) async fn reached(&mut self) {
        self.turn.as_mut().await;
    }
}

impl Drop for IdleTurn<'_> {
    /// Hands a waiting client's wake to the next connection in line on every way out of the idle
    /// phase. A `Notified` forwards a wake it received while queued, but not the stored one it
    /// took when enabled, so a connection leaving before its next poll would lose that one and
    /// the client would wait for some other connection to end.
    fn drop(&mut self) {
        if self.slots.is_wanted() {
            self.slots.idle_line.notify_one();
        }
    }
}

fn poll_incoming(
    listeners: &[(transport::Listener, Serving)],
    next: usize,
    cx: &mut std::task::Context<'_>,
) -> std::task::Poll<Result<(usize, transport::Stream), (usize, io::Error)>> {
    #[cfg(test)]
    if let Some(code) = ACCEPT_FAULTS
        .try_with(|faults| faults.lock().unwrap().pop_front())
        .ok()
        .flatten()
    {
        return std::task::Poll::Ready(Err((next, io::Error::from_raw_os_error(code))));
    }
    for (index, (listener, _)) in listeners
        .iter()
        .enumerate()
        .cycle()
        .skip(next)
        .take(listeners.len())
    {
        if let std::task::Poll::Ready(result) = listener.poll_accept(cx) {
            return std::task::Poll::Ready(
                result
                    .map(|stream| (index, stream))
                    .map_err(|error| (index, error)),
            );
        }
    }
    std::task::Poll::Pending
}

async fn serve_http(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    accepted: Accepted,
    state: Arc<State>,
    config: Arc<Config>,
    mut stopping: watch::Receiver<bool>,
    slots: Arc<SlotReclaim>,
) {
    let Accepted {
        handshake,
        role,
        at,
    } = accepted;
    match handshake {
        Handshake::Plain => {
            let listening = Listening {
                scheme: ConnectionScheme::Http,
                role,
            };
            serve_connection(stream, listening, at, state, config, stopping, slots).await;
        }
        Handshake::Tls(acceptor) => {
            let handshake = tokio::time::timeout_at(
                config.tls_handshake_timeout.deadline_from(at),
                acceptor.accept(stream),
            );
            let stream = tokio::select! {
                biased;
                _ = stopping.changed() => return,
                result = handshake => match result {
                    Ok(Ok(stream)) => stream,
                    Ok(Err(_)) => { crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "tls_handshake_failed", serde_json::json!({"message":"TLS handshake failed; check client protocol and trust configuration"})); return; }
                    Err(_) => { crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "tls_handshake_timeout", serde_json::json!({"message":"TLS handshake deadline exceeded"})); return; }
                }
            };
            let listening = Listening {
                scheme: ConnectionScheme::Https,
                role,
            };
            // The request head gets its own full limit once the handshake is done.
            let handshaken = tokio::time::Instant::now();
            serve_connection(
                stream, listening, handshaken, state, config, stopping, slots,
            )
            .await;
        }
    }
}

/// Task-locals do not cross spawn, so test hooks are carried into the connection task.
#[cfg(test)]
fn with_test_hooks(connection: impl Future<Output = ()>) -> impl Future<Output = ()> {
    let hook = FORCE_HANDLER_PANIC.try_with(Arc::clone).ok();
    async move {
        match hook {
            Some(hook) => FORCE_HANDLER_PANIC.scope(hook, connection).await,
            None => connection.await,
        }
    }
}

#[cfg(not(test))]
fn with_test_hooks<F: Future<Output = ()>>(connection: F) -> F {
    connection
}

/// Waits for every connection to finish within `limit`, then aborts the rest.
/// A panicked connection is reported and does not stop draining the others.
async fn drain_connections(connections: &mut JoinSet<()>, limit: TimeLimit) -> io::Result<()> {
    let drain = async {
        while let Some(result) = connections.join_next().await {
            if let Err(error) = result {
                report_connection_task_failure(&error);
            }
        }
    };
    let result = tokio::time::timeout_at(limit.deadline(), drain)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "shutdown drain deadline exceeded"));
    if result.is_err() {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    result
}

#[cfg(test)]
mod deadline_claim_tests;
#[cfg(test)]
mod drain_tests;
#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod reclaim_tests;
#[cfg(test)]
mod upload_frame_tests;

/// hyper's per-connection read buffer limit. It bounds how much hyper reserves before each
/// read, not the read itself: one read fills the buffer's spare capacity, which can grow to
/// about twice this value, so HTTP/1 body frames stay below `2 * CONNECTION_READ_BUFFER`
/// (24 KiB observed). [`stream_upload`] forwards frames without copying, so this bound is what
/// keeps the memory an upload's two-slot channel retains small; `upload_frame_tests` fails if
/// the value or the frame bound changes.
const CONNECTION_READ_BUFFER: usize = 16 * 1024;

fn http1_builder() -> http1::Builder {
    let mut builder = http1::Builder::new();
    builder.max_headers(64).max_buf_size(CONNECTION_READ_BUFFER);
    builder
}

async fn serve_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    listening: Listening,
    first_head_from: tokio::time::Instant,
    state: Arc<State>,
    config: Arc<Config>,
    mut stopping: watch::Receiver<bool>,
    slots: Arc<SlotReclaim>,
) {
    let limits = ConnectionLimits::new(&config);
    let (activity, progress) = watch::channel(ObjectActivity::default());
    let reads = Arc::new(ReadActivity::default());
    let builder = http1_builder();
    let service = ConnectionService {
        listening,
        state,
        config,
        activity: activity.clone(),
        reads: reads.clone(),
    };
    let mut connection = builder.serve_connection(
        TokioIo::new(CountingIo {
            inner: stream,
            reads: reads.clone(),
        }),
        service,
    );
    let watch = ConnectionWatch {
        progress,
        activity,
        reads,
        limits,
        first_head_from,
        slots,
    };
    let unread = watch.run(&mut connection, &mut stopping).await;
    // Send FIN after the response, then read and discard what the client still sends,
    // like an HTTP server's lingering close: closing with unread input sends a reset
    // that can discard an early rejection before the client reads it. The phase ends
    // when the client closes or pauses, after the lingering time, or on shutdown.
    let mut stream = connection.into_parts().io.into_inner();
    let _ = tokio::time::timeout(CLOSE_BUDGET, stream.shutdown()).await;
    match unread {
        UnreadInput::MayRemain => discard_lingering_input(&mut stream, &mut stopping).await,
        UnreadInput::None => {}
    }
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Debug,
        "connection_closed",
        serde_json::json!({}),
    );
}

/// The per-request half of a connection: hyper calls it once for every request head.
#[derive(Clone)]
struct ConnectionService {
    listening: Listening,
    state: Arc<State>,
    config: Arc<Config>,
    activity: watch::Sender<ObjectActivity>,
    reads: Arc<ReadActivity>,
}

type ServiceFuture =
    std::pin::Pin<Box<dyn Future<Output = Result<Response<ResponseBody>, Infallible>> + Send>>;

impl hyper::service::Service<Request<Incoming>> for ConnectionService {
    type Response = Response<ResponseBody>;
    type Error = Infallible;
    type Future = ServiceFuture;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        Box::pin(self.clone().respond(request))
    }
}

impl ConnectionService {
    async fn respond(
        self,
        request: Request<Incoming>,
    ) -> Result<Response<ResponseBody>, Infallible> {
        let Self {
            listening,
            state,
            config,
            activity,
            reads,
        } = self;
        #[cfg(test)]
        if FORCE_HANDLER_PANIC
            .try_with(|armed| armed.swap(false, Ordering::AcqRel))
            .unwrap_or(false)
        {
            panic!("injected handler panic");
        }
        // Each request starts its own deadline without an earlier transfer's state.
        activity.send_replace(ObjectActivity::default());
        let finished = RequestFinished {
            activity: activity.clone(),
            reads,
        };
        let sent_body = has_body(request.headers());
        let endpoints = match config
            .runtime_endpoints
            .snapshot(&config, &state.api_base_url)
        {
            Ok(endpoints) => {
                endpoints.for_request(listening.scheme, request.uri(), request.headers())
            }
            Err(_) => {
                let mut response = boxed(
                    JsonError::service_unavailable(
                        "Runtime endpoint state is unavailable; restart the emulator",
                    )
                    .into_response(),
                );
                listening.mark_response(response.headers_mut());
                return Ok(response.map(|body| finished.track(body)));
            }
        };
        let timeout_reply = DeadlineReply::new(&request, &config, endpoints.snapshot());
        let format =
            json::ResponseFormat::for_request(request.uri().query(), config.compat_profile);
        let ctx = RequestCtx {
            state: &state,
            config: &config,
            endpoints: &endpoints,
            activity: &activity,
            listener: listening.role,
        };
        // Checked once per request the listener receives, before routing: batch parts arrive
        // inside an envelope that already passed, and name no Host of their own.
        let admitted = browser::refuse_on_trusted_listener(
            listening.role,
            &request,
            &config.trusted_allowed_hosts,
        );
        // Dropping the handler cancels its work exactly as closing the
        // connection did, so an unfinished upload is still never published.
        let mut response = match admitted {
            Err(error) => boxed(error.into_response()),
            Ok(()) => tokio::select! {
                biased;
                response = json::with_format(
                    format,
                    routing::respond(request.map(|body| RequestBody::transport(body, activity.clone())), ctx),
                ) => response,
                expiry = expiry_claimed(&activity) => timeout_reply.response(expiry),
            },
        };
        // Handlers stop reading a body early only to reject it, and hyper then
        // closes the connection. Announcing the close keeps a pooled client from
        // sending its next request into the closing socket.
        if sent_body && response.status().as_u16() >= 400 {
            response.headers_mut().insert(
                "connection",
                hyper::header::HeaderValue::from_static("close"),
            );
        }
        listening.mark_response(response.headers_mut());
        activity.send_modify(|progress| progress.stage = RequestStage::Responded);
        Ok(response.map(|body| finished.track(body)))
    }
}

/// How long each phase of a connection may last without progress.
#[derive(Clone, Copy)]
struct ConnectionLimits {
    header_read: TimeLimit,
    keep_alive: TimeLimit,
    request: TimeLimit,
    body_inactivity: TimeLimit,
}

impl ConnectionLimits {
    fn new(config: &Config) -> Self {
        Self {
            header_read: config.header_read_timeout,
            keep_alive: config.keep_alive_timeout,
            request: config.request_timeout,
            body_inactivity: config.body_inactivity_timeout,
        }
    }

    fn during_request(&self, activity: &ObjectActivity) -> TimeLimit {
        match activity.step() {
            RequestStep::ClientPaced => self.body_inactivity,
            RequestStep::ServerPaced => self.request,
        }
    }
}

/// What a connection is waiting for, which selects the limit on how long that may take.
enum ConnectionPhase<'a> {
    /// A new connection's first request head.
    FirstHead,
    /// The next request, between keep-alive requests. The place in the idle line lives here, so
    /// leaving this phase gives it up, and passes on any wake it received, on every path.
    Idle { turn: IdleTurn<'a> },
    /// The rest of a next request's head, which has started arriving.
    NextHead,
    /// A request's handler, or its bodies.
    Request,
    /// The write of a timeout reply, bounded by `CLOSE_BUDGET` because the client may not read.
    TimeoutReply,
}

impl ConnectionPhase<'_> {
    /// Resolves when an idle connection reaches the front of the idle line, and never in other
    /// phases.
    async fn idle_turn(&mut self) {
        match self {
            Self::Idle { turn } => turn.reached().await,
            Self::FirstHead | Self::NextHead | Self::Request | Self::TimeoutReply => {
                std::future::pending().await
            }
        }
    }
}

/// Whether the lingering close has client input to discard after the connection stops serving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnreadInput {
    MayRemain,
    /// An idle connection has no unread request bytes.
    None,
}

fn warn_connection_timeout(message: &str) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "connection_timeout",
        serde_json::json!({"message": message}),
    );
}

/// The connection half that enforces connection, keep-alive and request deadlines.
struct ConnectionWatch {
    progress: watch::Receiver<ObjectActivity>,
    /// Writes the expiry claim under the same lock the handler uses to start a catalog wait.
    activity: watch::Sender<ObjectActivity>,
    reads: Arc<ReadActivity>,
    limits: ConnectionLimits,
    /// Where the header read limit of the first request head starts.
    first_head_from: tokio::time::Instant,
    slots: Arc<SlotReclaim>,
}

impl ConnectionWatch {
    /// Drives `connection` until it ends or a deadline closes it.
    async fn run<S: AsyncRead + AsyncWrite + Unpin>(
        self,
        connection: &mut http1::Connection<TokioIo<CountingIo<S>>, ConnectionService>,
        stopping: &mut watch::Receiver<bool>,
    ) -> UnreadInput {
        let Self {
            mut progress,
            activity,
            reads,
            limits,
            first_head_from,
            slots,
        } = self;
        let deadline = tokio::time::sleep_until(limits.header_read.deadline_from(first_head_from));
        // Waited on only after a request sets its deadline, which resets this first.
        let object_deadline = tokio::time::sleep_until(limits.header_read.deadline());
        tokio::pin!(deadline, object_deadline);
        let mut current = ObjectActivity::default();
        let mut phase = ConnectionPhase::FirstHead;
        loop {
            tokio::select! {
                biased;
                Ok(()) = progress.changed(), if !matches!(phase, ConnectionPhase::TimeoutReply) => {
                    let next = *progress.borrow_and_update();
                    if next.deadline != current.deadline && let Some(at) = next.deadline {
                        object_deadline.as_mut().reset(at);
                    }
                    current = next;
                    match current.stage {
                        RequestStage::Running | RequestStage::AwaitingCatalog | RequestStage::Expired(_) | RequestStage::Responded => {
                            phase = ConnectionPhase::Request;
                            deadline.as_mut().reset(limits.during_request(&current).deadline());
                        }
                        // A whole request can run within one poll, so this phase can already be
                        // `Idle` and this is still a new idle period, at the back of the line.
                        RequestStage::Finished => {
                            phase = ConnectionPhase::Idle { turn: slots.join_line() };
                            deadline.as_mut().reset(limits.keep_alive.deadline());
                        }
                    }
                },
                _ = reads.arrived.notified(), if matches!(phase, ConnectionPhase::Idle { .. }) => {
                    if reads.arrived_since_idle() {
                        phase = ConnectionPhase::NextHead;
                        deadline.as_mut().reset(limits.header_read.deadline());
                    }
                },
                _ = &mut object_deadline, if matches!(phase, ConnectionPhase::Request) && current.deadline.is_some() && current.stage != RequestStage::AwaitingCatalog => {
                    match claim_expiry(&activity, Expiry::ObjectRequest) {
                        ExpiryClaim::Granted => {
                            crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "object_request_timeout", serde_json::json!({"message":"object request deadline exceeded"}));
                            deadline.as_mut().reset(tokio::time::Instant::now() + CLOSE_BUDGET);
                            phase = ConnectionPhase::TimeoutReply;
                        }
                        // A catalog wait that started after this branch was chosen pauses the deadline.
                        ExpiryClaim::Refused => continue,
                        ExpiryClaim::ResponseStarted => break UnreadInput::MayRemain,
                    }
                },
                result = std::future::poll_fn(|cx| connection.poll_without_shutdown(cx)) => {
                    // hyper's Display names only the failure kind (body stream, write, parse), never the wrapped I/O detail.
                    if let Err(error) = result { crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "connection_failed", serde_json::json!({"message":"connection failed; check client request framing and transport","cause":error.to_string()})); }
                    break UnreadInput::MayRemain;
                }
                _ = &mut deadline, if current.stage != RequestStage::AwaitingCatalog && current.response_work == ResponseWork::Idle => {
                    match &phase {
                        ConnectionPhase::TimeoutReply => break UnreadInput::MayRemain,
                        // The connection polled just above can have moved the request on, which
                        // decides first and renews this deadline.
                        _ if matches!(progress.has_changed(), Ok(true)) => continue,
                        // A request head that stalls after it started, or never arrives, is a client fault.
                        ConnectionPhase::FirstHead | ConnectionPhase::NextHead => {
                            warn_connection_timeout("request headers were not received before header_read_timeout_ms");
                            break UnreadInput::MayRemain;
                        }
                        // An idle keep-alive connection closes quietly.
                        ConnectionPhase::Idle { .. } => break if reads.arrived_since_idle() { UnreadInput::MayRemain } else { UnreadInput::None },
                        ConnectionPhase::Request => match claim_expiry(&activity, current.step().expiry()) {
                            ExpiryClaim::Granted => {
                                warn_connection_timeout(current.step().timeout_message());
                                deadline.as_mut().reset(tokio::time::Instant::now() + CLOSE_BUDGET);
                                phase = ConnectionPhase::TimeoutReply;
                            }
                            ExpiryClaim::Refused => continue,
                            ExpiryClaim::ResponseStarted => {
                                warn_connection_timeout(current.step().timeout_message());
                                break UnreadInput::MayRemain;
                            }
                        },
                    }
                },
                () = phase.idle_turn() => {
                    if slots.is_wanted() && reads.arrived_since_idle() {
                        // The next request already started arriving, so this slot is not idle:
                        // the waiting client needs the next connection in line, which leaving
                        // the idle phase wakes.
                        phase = ConnectionPhase::NextHead;
                        deadline.as_mut().reset(limits.header_read.deadline());
                    } else if slots.take_request() {
                        break UnreadInput::None;
                    } else {
                        phase = ConnectionPhase::Idle { turn: slots.join_line() };
                    }
                },
                Ok(()) = stopping.changed() => {
                    std::pin::Pin::new(&mut *connection).graceful_shutdown();
                }
            }
        }
    }
}

/// Reads and discards client input until the client closes or pauses, the lingering
/// time ends, or the server shuts down.
async fn discard_lingering_input(
    stream: &mut (impl AsyncRead + Unpin),
    stopping: &mut watch::Receiver<bool>,
) {
    let linger = async {
        let mut discarded = vec![0; 16 * 1024];
        while let Ok(Ok(1..)) = tokio::time::timeout(LINGER_IDLE, stream.read(&mut discarded)).await
        {
        }
    };
    tokio::select! {
        _ = tokio::time::timeout(LINGER_TIME, linger) => {},
        _ = stopping.wait_for(|stopping| *stopping) => {},
    }
}

pub(crate) type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;

pub(crate) fn boxed(response: Response<Full<Bytes>>) -> Response<ResponseBody> {
    response.map(|body| body.map_err(|never| match never {}).boxed_unsync())
}

pub(crate) fn anonymous_error(error: JsonError, caller: Option<Caller<'_>>) -> JsonError {
    if matches!(caller, Some(Caller::Anonymous)) && error.status == StatusCode::FORBIDDEN {
        error.for_anonymous_caller()
    } else {
        error
    }
}

/// Resolve the owning project of `bucket` and authorize `permission` against it.
///
/// The returned access must be passed to the catalog operation, which re-validates it against
/// the owner it observes in its own transaction: the bucket can be deleted and recreated under
/// another project after this lookup. Permissive mode returns `Unchecked` without catalog
/// access. Anonymous callers have no project grants and are evaluated against bucket policy.
pub(crate) async fn authorize_bucket(
    state: &State,
    caller: Option<Caller<'_>>,
    permission: auth::Permission,
    bucket: &str,
) -> Result<BucketAccess, JsonError> {
    let Some(caller) = caller else {
        return Ok(BucketAccess::Permissive);
    };
    let identity = match caller {
        Caller::Anonymous => None,
        Caller::Principal(identity) => Some(identity),
    };
    Ok(state
        .catalog
        .bucket_access(bucket, identity, permission)
        .await?)
}

/// Authorize a project-scoped collection request such as buckets.insert or buckets.list.
fn authorize_project(
    caller: Option<Caller<'_>>,
    permission: auth::Permission,
    project: &str,
) -> Result<(), JsonError> {
    match caller {
        Some(caller) => Ok(caller.authorize(permission, Some(project))?),
        None => Ok(()),
    }
}

/// What a download should fetch while it opens the object.
fn body_read_ahead(request: &Request<RequestBody>) -> crate::object::ReadAhead {
    use crate::object::ReadAhead;
    const WITHOUT_FULL_BODY: [&str; 5] = [
        "range",
        "if-match",
        "if-none-match",
        "if-modified-since",
        "if-unmodified-since",
    ];
    let headers = request.headers();
    if request.method() == Method::GET
        && !WITHOUT_FULL_BODY
            .iter()
            .any(|name| headers.contains_key(*name))
    {
        ReadAhead::Body
    } else {
        ReadAhead::None
    }
}

fn object_body(
    reader: crate::storage::ObjectReader,
    activity: &watch::Sender<ObjectActivity>,
) -> ResponseBody {
    object_body::ObjectBody::new(reader, activity.clone()).boxed_unsync()
}

async fn validate_bodyless_post(
    request: Request<RequestBody>,
    expected: &std::collections::BTreeMap<String, String>,
) -> Result<(), JsonError> {
    // Ruby's generated client repeats URL query parameters in form-encoded POST bodies.
    let form = request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').next() == Some("application/x-www-form-urlencoded"));
    let body = json::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    if !body.is_empty() {
        let params = if form {
            std::str::from_utf8(&body)
                .ok()
                .and_then(|body| json::parse_query(body).ok())
        } else {
            None
        };
        if params.as_ref() != Some(expected) {
            let example = expected
                .iter()
                .next()
                .map(|(key, value)| format!("{key}={value}"))
                .unwrap_or_default();
            return Err(JsonError::invalid_argument(format!(
                "Use an empty body, or form parameters identical to the URL query, for example {example}"
            )));
        }
    }
    Ok(())
}

/// How `stream_upload` stopped reading a body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Streamed {
    /// The body ended after this many bytes, all forwarded to the catalog.
    Complete(u64),
    /// The catalog stopped accepting the body first and reports its own failure, so the bytes
    /// counted so far say nothing about the body's size.
    Stopped,
}

pub(crate) async fn stream_upload(
    mut body: RequestBody,
    sender: &upload_drive::UploadSender,
    max_bytes: u64,
    expected_sha256: Option<[u8; 32]>,
) -> Result<Streamed, ObjectError> {
    let mut size = 0_u64;
    let mut sha256 = expected_sha256.map(|_| ring::digest::Context::new(&ring::digest::SHA256));
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(ObjectError::body_aborted)?;
        let mut bytes = frame
            .into_data()
            .map_err(|_| ObjectError::BodyAborted("Upload trailers are unsupported".into()))?;
        size = size
            .checked_add(bytes.len() as u64)
            .filter(|size| *size <= max_bytes)
            .ok_or(ObjectError::TooLarge)?;
        if let Some(hash) = &mut sha256 {
            hash.update(&bytes);
        }
        // HTTP/1 frames stay below twice CONNECTION_READ_BUFFER, so passing them on without a
        // copy keeps what the two-slot channel retains bounded; larger frames are still split
        // into 64 KiB chunks.
        while !bytes.is_empty() {
            let part = bytes.split_to(bytes.len().min(64 * 1024));
            if sender.send(Ok(UploadChunk::Data(part))).await.is_err() {
                // Preserve the catalog's original failure when it stops accepting the body.
                return Ok(Streamed::Stopped);
            }
        }
    }
    if sha256
        .zip(expected_sha256)
        .is_some_and(|(hash, expected)| hash.finish().as_ref() != expected)
    {
        return Err(ObjectError::ChecksumMismatch);
    }
    Ok(Streamed::Complete(size))
}

pub(crate) async fn await_upload_catalog<T, E: From<ObjectError>>(
    operation: impl Future<Output = Result<T, E>>,
    timeout: TimeLimit,
    request_deadline: tokio::time::Instant,
    activity: &watch::Sender<ObjectActivity>,
) -> Result<T, E> {
    let _wait = begin_catalog_wait(activity).await;
    // A completed body may commit after this wait expires; only an observed commit is success.
    tokio::time::timeout_at(request_deadline.min(timeout.deadline()), operation)
        .await
        .unwrap_or_else(|_| Err(object_unavailable(BackendFailure::UploadWaitTimeout).into()))
}

fn read_etag_not_modified(headers: &hyper::HeaderMap, etag: &str) -> Result<bool, JsonError> {
    etag_not_modified(headers, etag).map_err(|error| match error {
        EtagError::Invalid => JsonError::invalid_parameter("Invalid ETag precondition header"),
        // The header form of the failure carries a shorter message than the query form.
        EtagError::PreconditionFailed => JsonError::new(
            StatusCode::PRECONDITION_FAILED,
            "conditionNotMet",
            "Precondition Failed",
        ),
    })
}

fn not_modified_response(
    etag: Option<&hyper::header::HeaderValue>,
    cache_control: Option<&hyper::header::HeaderValue>,
) -> Response<Full<Bytes>> {
    let mut response = empty_response(StatusCode::NOT_MODIFIED);
    let headers = response.headers_mut();
    if let Some(etag) = etag {
        headers.insert("etag", etag.clone());
    }
    if let Some(cache_control) = cache_control {
        headers.insert("cache-control", cache_control.clone());
    }
    response
}

/// Whether a metadata read's query preconditions (`ifGenerationNotMatch`,
/// `ifMetagenerationNotMatch`) already select a `304 Not Modified` response.
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryPrecondition {
    NotModified,
    Unmatched,
}

/// Varied on first by a publicly readable JSON media response, ahead of the CORS fields.
const ALLOWED_RESOURCES_VARY: &str = "X-Goog-Allowed-Resources";

/// The `Cache-Control` Cloud Storage sends with JSON API resources and lists.
const JSON_RESOURCE_CACHE_CONTROL: &str = "private, max-age=0, must-revalidate, no-transform";

/// The `Cache-Control` of IAM policy reads, whose provider value has not been recorded.
const IAM_POLICY_CACHE_CONTROL: &str = "no-cache";

fn read_metadata_response(
    headers: &hyper::HeaderMap,
    etag: &str,
    mut value: serde_json::Value,
    fields: &json::Fields,
    query: QueryPrecondition,
    cache_control: &'static str,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let not_modified = match read_etag_not_modified(headers, etag) {
        Err(error)
            if error.status == StatusCode::PRECONDITION_FAILED
                && value.get("kind").and_then(serde_json::Value::as_str)
                    == Some("storage#object") =>
        {
            let mut response = error.at_if_match().into_response();
            if let Ok(etag) = etag.parse() {
                response.headers_mut().insert("etag", etag);
            }
            return Ok(response);
        }
        result => result? || query == QueryPrecondition::NotModified,
    };
    // ETags are rendered locally from base64 (JSON) or a quoted policy revision, so the
    // conversion always succeeds; omitting the validator is safer than panicking if that ever
    // changes.
    let etag = hyper::header::HeaderValue::from_str(etag).ok();
    let mut response = if not_modified {
        empty_response(StatusCode::NOT_MODIFIED)
    } else {
        fields.apply(&mut value);
        json_response(&value)
    };
    let response_headers = response.headers_mut();
    if let Some(etag) = etag {
        response_headers.insert("etag", etag);
    }
    response_headers.insert(
        "cache-control",
        hyper::header::HeaderValue::from_static(cache_control),
    );
    Ok(response)
}

/// The cache headers of a `200 OK` JSON API resource response.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JsonCache {
    /// Not recorded for this operation, so no cache header is sent.
    Unrecorded,
    /// Reads of object and bucket metadata and lists.
    Resource,
    /// Cloud Storage forbids every cache, HTTP/1.0 ones included, for recorded object inserts.
    NoStore,
}

/// A `200 OK` JSON API resource with the cache headers its operation records.
fn json_response_cached(value: &serde_json::Value, cache: JsonCache) -> Response<Full<Bytes>> {
    let mut response = json_response(value);
    let headers = response.headers_mut();
    match cache {
        JsonCache::Unrecorded => {}
        JsonCache::Resource => {
            headers.insert(
                "cache-control",
                hyper::header::HeaderValue::from_static(JSON_RESOURCE_CACHE_CONTROL),
            );
        }
        JsonCache::NoStore => {
            headers.insert(
                "cache-control",
                hyper::header::HeaderValue::from_static(json::NO_STORE_CACHE_CONTROL),
            );
            headers.insert(
                "pragma",
                hyper::header::HeaderValue::from_static("no-cache"),
            );
        }
    }
    response
}

/// A resource response narrowed by `fields`. The `ETag` header still names the whole resource,
/// as Cloud Storage sends it with a field-selected body.
fn json_response_selected(
    value: &mut serde_json::Value,
    fields: &json::Fields,
    cache: JsonCache,
) -> Response<Full<Bytes>> {
    let etag = json::etag_header(value);
    fields.apply(value);
    let mut response = json_response_cached(value, cache);
    if let Some(etag) = etag {
        response.headers_mut().insert("etag", etag);
    }
    response
}

pub(crate) fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}

/// A `200 OK` JSON API resource.
fn json_response(value: &serde_json::Value) -> Response<Full<Bytes>> {
    json_response_body(value, json::render_body(value))
}

/// `body` must be `value` rendered in the current request's JSON format.
fn json_response_body(value: &serde_json::Value, body: String) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    response.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/json; charset=UTF-8"),
    );
    if let Some(etag) = json::etag_header(value) {
        response.headers_mut().insert("etag", etag);
    }
    response
}

/// Insert `Content-Range` for a satisfiable range. Decimal positions always form a valid header
/// value, so the conversion cannot fail in practice.
fn insert_content_range(headers: &mut hyper::HeaderMap, offset: u64, length: u64, size: u64) {
    if let Ok(value) = hyper::header::HeaderValue::try_from(format!(
        "bytes {}-{}/{}",
        offset,
        offset + length - 1,
        size
    )) {
        headers.insert("content-range", value);
    }
}

/// Insert `Content-Range` for an unsatisfiable range request.
fn insert_unsatisfied_content_range(headers: &mut hyper::HeaderMap, size: u64) {
    if let Ok(value) = hyper::header::HeaderValue::try_from(format!("bytes */{size}")) {
        headers.insert("content-range", value);
    }
}

fn unknown_route() -> JsonError {
    JsonError::new(StatusCode::NOT_FOUND, "notFound", "Route not found")
}

fn probe(request: &Request<RequestBody>, state: &State) -> Option<Response<Full<Bytes>>> {
    let path = request.uri().path();
    let internal = matches!(
        path,
        "/_internal/health" | "/_internal/ready" | "/_internal/healthcheck"
    );
    if !internal {
        return None;
    }
    let (status, body) = if request.method() != Method::GET && request.method() != Method::HEAD {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            "{\"error\":\"method not allowed\"}".to_owned(),
        )
    } else if has_body(request.headers()) {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "{\"error\":\"probe bodies are not supported\"}".to_owned(),
        )
    } else if matches!(path, "/_internal/ready" | "/_internal/healthcheck") && !state.is_ready() {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "{\"status\":\"not ready\"}".to_owned(),
        )
    } else {
        let now = state
            .clock
            .now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        (
            StatusCode::OK,
            format!("{{\"status\":\"ok\",\"unix_time\":{now}}}"),
        )
    };
    let content_length = hyper::header::HeaderValue::from(body.len());
    let mut response = Response::new(Full::new(if request.method() == Method::HEAD {
        Bytes::new()
    } else {
        Bytes::from(body)
    }));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/json"),
    );
    headers.insert("content-length", content_length);
    if status == StatusCode::METHOD_NOT_ALLOWED {
        headers.insert(
            "allow",
            hyper::header::HeaderValue::from_static("GET, HEAD"),
        );
    }
    Some(response)
}
