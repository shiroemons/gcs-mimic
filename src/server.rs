use crate::json::JsonErrorResponse;
use crate::{
    auth::{self, Caller, ListenerRole},
    bucket::{BucketAccess, CatalogError},
    cause::IoCause,
    config::{Config, ConnectionScheme, GrpcScheme},
    diagnostics::Diagnostics,
    http_conditions::{EtagError, etag_not_modified},
    json,
    json::ApiError,
    object::ObjectError,
    storage::Catalog,
    time_limit::TimeLimit,
    tls::ListenerSecurity,
    xml,
};
#[cfg(unix)]
use handshake::establish;
use handshake::{Established, ListenerProtocol, Secured, establish_tcp};
use http_body_util::{BodyExt, Full};
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
    task::Poll,
    time::{Duration, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::{Notify, futures::OwnedNotified, watch},
    task::JoinSet,
};

use crate::service::{
    access::bucket_access,
    activity::{
        AfterResponse, Expiry, ExpiryClaim, ObjectActivity, RequestStage, RequestStep,
        ResponseWork, claim_expiry, expiry_claimed,
    },
    body::{RequestBody, ResponseBody, boxed, empty_response},
    chunk_body,
    context::{DeadlineWatch, ObjectCtx, RequestCtx},
    object_body::ObjectBody,
    state::State,
};

pub(crate) use crate::bounded_body::RESOURCE_BODY_LIMIT;

mod batch;
mod browser;
mod buckets;
// Crate-visible for the test helper that the gRPC handler tests reset a panicking stream with.
pub(crate) mod grpc_listener;
mod handshake;
mod host;
mod ingress;
mod json_route;
mod objects;
mod routing;
mod transport;
#[cfg(unix)]
pub(crate) mod unix;
mod xml_read;
#[cfg(test)]
pub(crate) use routing::bucket_answer;

#[cfg(test)]
tokio::task_local! { static ACCEPT_FAULTS: Arc<std::sync::Mutex<std::collections::VecDeque<i32>>>; }
#[cfg(test)]
tokio::task_local! { static FORCE_HANDLER_PANIC: Arc<AtomicBool>; }

/// The next accept error a test queued for the accept loop.
#[cfg(test)]
fn injected_accept_fault() -> Option<io::Error> {
    ACCEPT_FAULTS
        .try_with(|faults| faults.lock().unwrap().pop_front())
        .ok()
        .flatten()
        .map(io::Error::from_raw_os_error)
}

#[cfg(not(test))]
fn injected_accept_fault() -> Option<io::Error> {
    None
}

/// Panics in the request handler once when a test armed it, to exercise panic isolation.
#[cfg(test)]
fn injected_handler_panic() {
    if FORCE_HANDLER_PANIC
        .try_with(|armed| armed.swap(false, Ordering::AcqRel))
        .unwrap_or(false)
    {
        panic!("injected handler panic");
    }
}

#[cfg(not(test))]
fn injected_handler_panic() {}

/// A connection task ends with an error only when it panicked or was aborted.
/// hyper polls request handlers inside that task, so one faulty request must
/// close only its own connection; the accept loop keeps serving the others.
fn report_connection_task_failure(error: &tokio::task::JoinError) {
    let (event, message) = if error.is_panic() {
        (
            crate::diagnostics::Event::ConnectionTaskPanicked,
            "a request handler panicked; its connection was closed and the server keeps serving",
        )
    } else {
        (
            crate::diagnostics::Event::ConnectionTaskCancelled,
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
mod admin;
mod notifications;
mod project_resources;
pub(crate) mod resumable;
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

/// The JSON error for `expiry` on a request that sends `payload`, naming the setting that bounds
/// the expired limit.
fn json_timeout(expiry: Expiry, payload: Payload) -> ApiError {
    match (expiry, payload) {
        (Expiry::BodyInactivity, Payload::Upload | Payload::Other) => request_body_silent(),
        // Only a transfer's whole-request limit reports a broken upload: a server that is
        // slow to answer an upload is still a server-side timeout.
        (Expiry::ObjectRequest, Payload::Upload) => upload_timed_out(),
        (Expiry::ObjectRequest, Payload::Other) => object_request_timed_out(),
        (Expiry::Request, Payload::Upload | Payload::Other) => request_timed_out(),
    }
}

const REQUEST_TIMED_OUT: &str = "The request timed out before a response was ready; retry the request, or raise request_timeout_ms (0 disables it).";

fn request_timed_out() -> ApiError {
    ApiError::service_unavailable(REQUEST_TIMED_OUT)
}

/// The JSON API documents 408 `uploadBrokenConnection` as its only request timeout.
fn request_body_silent() -> ApiError {
    ApiError::new(
        StatusCode::REQUEST_TIMEOUT,
        "uploadBrokenConnection",
        "The request timed out: no request body bytes arrived within body_inactivity_timeout_ms; retry the request, or raise body_inactivity_timeout_ms for slow clients (0 disables it).",
    )
}

/// The XML API documents a silent socket as 400 `RequestTimeout`; this repeats its meaning.
const XML_REQUEST_BODY_SILENT: &str = "Your socket connection to the server was not read from or written to within the timeout period: no request body bytes arrived within body_inactivity_timeout_ms; retry the request, or raise body_inactivity_timeout_ms for slow clients (0 disables it).";

const UPLOAD_TIMED_OUT: &str = "The upload timed out: it did not finish within object_request_timeout_ms; retry the request, or raise object_request_timeout_ms for large uploads (0 disables it).";

fn upload_timed_out() -> ApiError {
    ApiError::new(
        StatusCode::REQUEST_TIMEOUT,
        "uploadBrokenConnection",
        UPLOAD_TIMED_OUT,
    )
}

const OBJECT_REQUEST_TIMED_OUT: &str = "The request timed out: it did not finish within object_request_timeout_ms; retry the request, or raise object_request_timeout_ms for large transfers (0 disables it).";

/// A copy, rewrite, compose or media read past its whole-request limit: a server-side timeout.
fn object_request_timed_out() -> ApiError {
    ApiError::service_unavailable(OBJECT_REQUEST_TIMED_OUT)
}

/// Whether the request declares a body: any `Transfer-Encoding`, or a `Content-Length` that is
/// not the number zero. `Content-Length: 00` declares none, as hyper frames it.
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

/// Refuses a declared body on a JSON method that takes none, read as [`has_body`] reads it.
/// Cloud Storage accepted `Content-Length: 00` on a bucket read, an object read and an object
/// delete (recorded 2026-11-15).
fn require_empty_body(headers: &hyper::HeaderMap) -> Result<(), ApiError> {
    if has_body(headers) {
        return Err(ApiError::invalid_argument(
            "This method requires an empty body",
        ));
    }
    Ok(())
}

impl DeadlineReply {
    /// The reply for the API that serves `target`, which `request` now addresses.
    fn new<B>(request: &Request<B>, target: &routing::Target) -> Self {
        let headers = request.headers();
        let api = if target.answers_xml() {
            ReplyApi::Xml(xml::ErrorBody::for_method(request.method()))
        } else {
            ReplyApi::Json
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
        match (expiry, self.payload) {
            // The XML API lists a silent socket under 400, apart from the JSON API's 408.
            (Expiry::BodyInactivity, _) => xml::Error::request_timeout(XML_REQUEST_BODY_SILENT),
            (Expiry::Request, _) => xml::Error::service_unavailable(REQUEST_TIMED_OUT),
            (Expiry::ObjectRequest, Payload::Upload) => {
                xml::Error::upload_broken_connection(UPLOAD_TIMED_OUT)
            }
            (Expiry::ObjectRequest, Payload::Other) => {
                xml::Error::service_unavailable(OBJECT_REQUEST_TIMED_OUT)
            }
        }
    }

    fn response(self, expiry: Expiry) -> Response<ResponseBody> {
        let mut response = match self.api {
            ReplyApi::Json => boxed(json_timeout(expiry, self.payload).into_response()),
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
#[must_use = "dropping the guard marks the request finished"]
struct RequestFinished {
    activity: watch::Sender<ObjectActivity>,
    reads: Arc<ReadActivity>,
    place: Arc<IdlePlace>,
    after: AfterResponse,
}

/// The place in the idle line that a request's end takes for its connection, until the
/// connection's watch moves it into the idle phase.
///
/// hyper drops a response body once its last bytes are buffered and before it flushes them, so a
/// connection takes its place before its client can see the response end, and connections whose
/// clients finished one after another stand in that order. Joining the line only when the watch
/// next ran let a busy runtime queue a connection that finished later ahead of an older one, so a
/// waiting client closed the younger connection while the older one kept its slot.
struct IdlePlace {
    slots: Arc<SlotReclaim>,
    taken: std::sync::Mutex<Option<IdleTurn>>,
}

impl IdlePlace {
    fn new(slots: Arc<SlotReclaim>) -> Self {
        Self {
            slots,
            taken: std::sync::Mutex::new(None),
        }
    }

    /// Takes a place at the back of the line, giving up one that an earlier request took.
    fn join(&self) {
        let turn = self.slots.join_line();
        let earlier = self.lock().replace(turn);
        drop(earlier);
    }

    /// The place the last request's end took, or a new one at the back of the line.
    fn take(&self) -> IdleTurn {
        let taken = self.lock().take();
        taken.unwrap_or_else(|| self.slots.join_line())
    }

    /// Gives up the place that a request's end took once the next request has started, so a busy
    /// connection holds no place and passes on any wake it received.
    fn clear(&self) {
        let taken = self.lock().take();
        drop(taken);
    }

    /// Every use replaces the whole value, so a panic while it was held left nothing half done.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<IdleTurn>> {
        crate::sync::lock(&self.taken)
    }
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
    fn new(
        activity: watch::Sender<ObjectActivity>,
        reads: Arc<ReadActivity>,
        place: Arc<IdlePlace>,
    ) -> Self {
        Self {
            activity,
            reads,
            place,
            // Until a response is attached: hyper drops a request without one only as its
            // connection ends.
            after: AfterResponse::Closes,
        }
    }

    /// Puts the request's end into the response body, where hyper drops it, and records whether
    /// the response keeps the connection open.
    fn attach(mut self, response: Response<ResponseBody>) -> Response<ResponseBody> {
        self.after = AfterResponse::declared_by(response.headers());
        response.map(|body| {
            TrackedBody {
                body,
                _finished: self,
            }
            .boxed_unsync()
        })
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
    type Error = IoCause;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
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
        // A connection that closes never turns idle, so its end takes no place in line and gives
        // up one that an earlier request's end took. The place is taken before the watch can see
        // the end, so it never joins the line a second time.
        match self.after {
            AfterResponse::KeepAlive => self.place.join(),
            AfterResponse::Closes => self.place.clear(),
        }
        self.activity
            .send_replace(ObjectActivity::finished(self.after));
    }
}

/// What a batch part answers once [`batch_part_expired`](crate::service::activity::batch_part_expired)
/// has claimed its timeout. A part's body is already buffered, so it is never an upload.
fn batch_part_timeout(expiry: Expiry) -> Response<ResponseBody> {
    boxed(json_timeout(expiry, Payload::Other).into_response())
}

/// The listeners one server accepts on. They share `max_connections` and every timeout.
pub enum Listeners {
    Tcp(TcpListeners),
    /// A private local socket, served as plain HTTP on a public listener.
    #[cfg(unix)]
    Unix(unix::Listener),
}

impl From<TcpListeners> for Listeners {
    fn from(listeners: TcpListeners) -> Self {
        Self::Tcp(listeners)
    }
}

#[cfg(unix)]
impl From<unix::Listener> for Listeners {
    fn from(listener: unix::Listener) -> Self {
        Self::Unix(listener)
    }
}

/// The TCP listeners of one server. They share `max_connections` and every timeout.
pub struct TcpListeners {
    /// The browser-facing listeners, each with how it secures its connections.
    pub(crate) public: Vec<(TcpListener, ListenerSecurity)>,
    /// The plain HTTP listener for the application's own servers, where a request without
    /// credentials acts as `trusted_principal`.
    pub(crate) trusted: Option<TcpListener>,
    /// The separate gRPC listener, with how it secures its connections.
    pub(crate) grpc: Option<GrpcListener>,
}

/// The bound gRPC listener and its configured transport.
pub(crate) struct GrpcListener {
    pub(crate) listener: TcpListener,
    pub(crate) security: ListenerSecurity,
    pub(crate) scheme: GrpcScheme,
}

impl TcpListeners {
    /// One browser-facing listener, without the trusted and gRPC listeners.
    pub fn public(listener: TcpListener, security: ListenerSecurity) -> Self {
        Self {
            public: vec![(listener, security)],
            trusted: None,
            grpc: None,
        }
    }

    /// The listeners as served: the public ones in order, then the trusted and gRPC ones.
    fn into_entries(self) -> Result<Vec<Entry>, Diagnostics> {
        let Self {
            public,
            trusted,
            grpc,
        } = self;
        let mut entries: Vec<Entry> = public
            .into_iter()
            .map(|(listener, security)| {
                Entry::Http(
                    transport::Listener::Tcp(listener),
                    Serving::public(security),
                )
            })
            .chain(trusted.map(|listener| {
                Entry::Http(transport::Listener::Tcp(listener), Serving::trusted())
            }))
            .collect();
        if entries.is_empty() {
            return Err(Diagnostics::startup_failure(&io::Error::new(
                io::ErrorKind::InvalidInput,
                "at least one listener is required",
            )));
        }
        entries.extend(grpc.map(Entry::Grpc));
        Ok(entries)
    }
}

/// When a server stops, and the work it waits for once its connections drain.
pub struct Stop<S, D> {
    /// Resolves when the server should stop accepting; its error becomes the server's result.
    pub(crate) signal: S,
    /// Work outside the server that ends before the catalog closes.
    pub(crate) background: D,
}

impl<S> Stop<S, std::future::Ready<io::Result<()>>> {
    /// Stops on `signal`, with no background work to wait for.
    pub fn on(signal: S) -> Self {
        Self {
            signal,
            background: std::future::ready(Ok(())),
        }
    }
}

/// Serves `listeners` until `stop.signal` resolves, then drains the connections, waits for
/// `stop.background` and closes the catalog.
pub async fn serve<S, D>(
    listeners: impl Into<Listeners>,
    config: impl Into<Arc<Config>>,
    state: Arc<State>,
    stop: Stop<S, D>,
) -> Result<(), Diagnostics>
where
    S: Future<Output = io::Result<()>>,
    D: Future<Output = io::Result<()>>,
{
    let entries = match listeners.into() {
        Listeners::Tcp(listeners) => listeners.into_entries()?,
        #[cfg(unix)]
        Listeners::Unix(listener) => vec![Entry::Http(
            transport::Listener::Unix(listener),
            Serving::public(ListenerSecurity::Plain),
        )],
    };
    serve_entries(entries, config.into(), state, stop).await
}

/// A listener and how its connections are served.
enum Entry {
    /// HTTP/1 connections, served as `Serving` says.
    Http(transport::Listener, Serving),
    /// The gRPC listener, with how it secures its connections. gRPC serves its connections
    /// itself, and its calls always authenticate as on a public listener, so it needs no role.
    Grpc(GrpcListener),
}

/// A connection one entry accepted, with what serving it needs.
enum NewConnection {
    Http(transport::Stream, Serving),
    Grpc(tokio::net::TcpStream, ListenerSecurity, GrpcScheme),
}

impl Entry {
    fn poll_accept(&self, cx: &mut std::task::Context<'_>) -> Poll<io::Result<NewConnection>> {
        match self {
            Self::Http(listener, serving) => listener
                .poll_accept(cx)
                .map_ok(|stream| NewConnection::Http(stream, serving.clone())),
            Self::Grpc(GrpcListener {
                listener,
                security,
                scheme,
            }) => transport::poll_accept_tcp(listener, cx)
                .map_ok(|stream| NewConnection::Grpc(stream, security.clone(), *scheme)),
        }
    }

    async fn close(self) -> Result<(), transport::CloseError> {
        match self {
            Self::Http(listener, _) => listener.close().await,
            Self::Grpc(..) => Ok(()),
        }
    }
}

/// What every connection task shares with the server that accepted it.
#[derive(Clone)]
struct Shared {
    state: Arc<State>,
    config: Arc<Config>,
    slots: Arc<SlotReclaim>,
}

/// How the HTTP connections a listener accepts are served.
#[derive(Clone)]
struct Serving {
    security: ListenerSecurity,
    role: ListenerRole,
}

impl Serving {
    fn public(security: ListenerSecurity) -> Self {
        Self {
            security,
            role: ListenerRole::Public,
        }
    }

    /// Plain HTTP whatever the scheme: the listener is meant for a private container network.
    fn trusted() -> Self {
        Self {
            security: ListenerSecurity::Plain,
            role: ListenerRole::Trusted,
        }
    }
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

async fn serve_entries(
    entries: Vec<Entry>,
    config: Arc<Config>,
    state: Arc<State>,
    stop: Stop<impl Future<Output = io::Result<()>>, impl Future<Output = io::Result<()>>>,
) -> Result<(), Diagnostics> {
    let Stop {
        signal: shutdown,
        background: background_drain,
    } = stop;
    let (stop, _) = watch::channel(false);
    // Connections and requests share one configuration instead of cloning it per request.
    let shared = Shared {
        state,
        config,
        slots: Arc::new(SlotReclaim::default()),
    };
    let max_connections = shared.config.max_connections();
    let mut connections = JoinSet::new();
    let mut next_listener = 0;
    // Later clients stay in the listen backlog until this one has a slot, so a full server asks
    // for one idle slot at a time.
    let mut held: Option<HeldClient<'_>> = None;
    let starter = ConnectionStarter {
        shared: &shared,
        stop: &stop,
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
                    && let Some(HeldClient { connection, .. }) = held.take()
                {
                    starter.start(&mut connections, connection);
                }
            }
            incoming = std::future::poll_fn(|cx| poll_incoming(&entries, next_listener, cx)), if held.is_none() && !paused => {
                let (index, connection) = match incoming {
                    Ok(incoming) => incoming,
                    Err((index, error)) => {
                        next_listener = (index + 1) % entries.len();
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
                next_listener = (index + 1) % entries.len();
                if connections.len() >= max_connections {
                    held = Some(HeldClient { connection, _request: shared.slots.request() });
                } else {
                    starter.start(&mut connections, connection);
                }
            }
        }
    };
    drop(held);
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Info,
        crate::diagnostics::Event::Stopping,
        serde_json::json!({}),
    );
    let Shared { state, config, .. } = &shared;
    state.ready.store(false, Ordering::Release);
    let mut listener_cleanups = Vec::with_capacity(entries.len());
    for entry in entries {
        listener_cleanups.push(entry.close().await);
    }
    stop.send_replace(true);
    let drain_result = drain_connections(&mut connections, config.shutdown_timeout()).await;
    let background_result = background_drain.await;
    let mut result = signal_result
        .and(drain_result)
        .map_err(|error| Diagnostics::startup_failure(&error));
    for cleanup in listener_cleanups {
        result = crate::diagnostics::combine_unix_socket_cleanup(result, cleanup);
    }
    result = crate::diagnostics::combine_shutdown_results(result, background_result);
    crate::diagnostics::combine_shutdown_results(result, state.catalog.close().await)
}

/// What the accept loop shares with every connection task it starts.
struct ConnectionStarter<'a> {
    shared: &'a Shared,
    stop: &'a watch::Sender<bool>,
}

impl ConnectionStarter<'_> {
    /// Starts the task that serves `connection`. Protocol detection and the TLS handshake run in
    /// that task, never in the accept loop, so a slow client holds only its own slot.
    fn start(&self, connections: &mut JoinSet<()>, connection: NewConnection) {
        let shared = self.shared.clone();
        let mut stopping = self.stop.subscribe();
        let accepted_at = tokio::time::Instant::now();
        let task = async move {
            let config = &shared.config;
            match connection {
                NewConnection::Grpc(stream, security, scheme) => {
                    let Some(established) = establish_tcp(
                        stream,
                        security,
                        accepted_at,
                        config,
                        &mut stopping,
                        ListenerProtocol::Grpc,
                    )
                    .await
                    else {
                        return;
                    };
                    serve_grpc(established, scheme, shared, stopping).await;
                }
                NewConnection::Http(transport::Stream::Tcp(stream), serving) => {
                    let Some(established) = establish_tcp(
                        stream,
                        serving.security,
                        accepted_at,
                        config,
                        &mut stopping,
                        ListenerProtocol::Http,
                    )
                    .await
                    else {
                        return;
                    };
                    serve_http(established, serving.role, shared, stopping).await;
                }
                #[cfg(unix)]
                NewConnection::Http(transport::Stream::Unix(stream), serving) => {
                    let Some(established) = establish(
                        stream,
                        serving.security.for_unix_socket(),
                        accepted_at,
                        config,
                        &mut stopping,
                        ListenerProtocol::Http,
                    )
                    .await
                    else {
                        return;
                    };
                    serve_http(established, serving.role, shared, stopping).await;
                }
            }
        };
        connections.spawn(with_test_hooks(task));
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
    /// With nobody waiting it stores a wake for the next connection to become idle. Shared so
    /// that a response body, which owns everything it holds, can take a place in line.
    idle_line: Arc<Notify>,
}

/// A client accepted while every slot is taken.
struct HeldClient<'a> {
    connection: NewConnection,
    _request: SlotRequest<'a>,
}

/// Asks idle connections for a slot until it drops, when the client got one by any route.
#[must_use = "dropping the guard withdraws the connection slot request"]
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

    pub(crate) fn join_line(self: &Arc<Self>) -> IdleTurn {
        let mut turn = Box::pin(Arc::clone(&self.idle_line).notified_owned());
        // An unpolled `Notified` is not in line yet, and the line's order is the point.
        turn.as_mut().enable();
        IdleTurn {
            slots: Arc::clone(self),
            turn,
        }
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
#[must_use = "dropping the guard gives up the connection's place in the idle line"]
pub(crate) struct IdleTurn {
    slots: Arc<SlotReclaim>,
    turn: std::pin::Pin<Box<OwnedNotified>>,
}

impl IdleTurn {
    /// Resolves at the front of the idle line. The place in line outlives this future, so
    /// `select!` can drop it at any time.
    pub(crate) async fn reached(&mut self) {
        self.turn.as_mut().await;
    }
}

impl Drop for IdleTurn {
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

/// The next connection any entry accepted, polling from `next` so that every entry gets its
/// turn, with the index of the entry that accepted it.
fn poll_incoming(
    entries: &[Entry],
    next: usize,
    cx: &mut std::task::Context<'_>,
) -> Poll<Result<(usize, NewConnection), (usize, io::Error)>> {
    if let Some(error) = injected_accept_fault() {
        return Poll::Ready(Err((next, error)));
    }
    for (index, entry) in entries
        .iter()
        .enumerate()
        .cycle()
        .skip(next)
        .take(entries.len())
    {
        if let Poll::Ready(result) = entry.poll_accept(cx) {
            return Poll::Ready(
                result
                    .map(|connection| (index, connection))
                    .map_err(|error| (index, error)),
            );
        }
    }
    Poll::Pending
}

async fn serve_http(
    established: Established<impl AsyncRead + AsyncWrite + Unpin>,
    role: ListenerRole,
    shared: Shared,
    stopping: watch::Receiver<bool>,
) {
    let Established { stream, since } = established;
    match stream {
        Secured::Plain(stream) => {
            let listening = Listening {
                scheme: ConnectionScheme::Http,
                role,
            };
            serve_connection(stream, listening, since, shared, stopping).await;
        }
        Secured::Tls(stream) => {
            let listening = Listening {
                scheme: ConnectionScheme::Https,
                role,
            };
            serve_connection(stream, listening, since, shared, stopping).await;
        }
    }
}

async fn serve_grpc(
    established: Established<impl AsyncRead + AsyncWrite + Unpin + Send + 'static>,
    scheme: GrpcScheme,
    shared: Shared,
    stopping: watch::Receiver<bool>,
) {
    let Shared {
        state,
        config,
        slots,
    } = shared;
    let Established { stream, since } = established;
    match stream {
        Secured::Plain(stream) => {
            grpc_listener::serve_connection(
                stream, since, scheme, &config, state, stopping, &slots,
            )
            .await;
        }
        Secured::Tls(stream) => {
            grpc_listener::serve_connection(
                stream, since, scheme, &config, state, stopping, &slots,
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
mod catalog_wait_tests;
#[cfg(test)]
mod deadline_tests;
#[cfg(test)]
mod drain_tests;
#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod reclaim_tests;
#[cfg(test)]
mod upload_frame_tests;

/// hyper returns 431 when the HTTP/1 request head is still incomplete at the read buffer limit.
/// A head that completes in the same read can reach about twice the limit.
///
/// For request bodies, the limit bounds how much hyper reserves before each read. A read
/// fills the buffer's spare capacity, which can grow to about twice this value, so HTTP/1
/// body frames stay below `2 * CONNECTION_READ_BUFFER` (128 KiB).
/// [`stream_upload`](crate::service::upload_drive::stream_upload) forwards slices without
/// copying, so this bound is what keeps the memory an upload's two-slot channel retains small;
/// `upload_frame_tests` fails if the value or the frame bound changes.
const CONNECTION_READ_BUFFER: usize = 64 * 1024;

fn http1_builder() -> http1::Builder {
    let mut builder = http1::Builder::new();
    builder.max_headers(64).max_buf_size(CONNECTION_READ_BUFFER);
    builder
}

async fn serve_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    listening: Listening,
    first_head_from: tokio::time::Instant,
    shared: Shared,
    mut stopping: watch::Receiver<bool>,
) {
    let Shared {
        state,
        config,
        slots,
    } = shared;
    let limits = ConnectionLimits::new(&config);
    let (activity, progress) = watch::channel(ObjectActivity::default());
    let reads = Arc::new(ReadActivity::default());
    let place = Arc::new(IdlePlace::new(Arc::clone(&slots)));
    let builder = http1_builder();
    let service = ConnectionService {
        listening,
        state,
        config,
        activity: activity.clone(),
        reads: reads.clone(),
        place: Arc::clone(&place),
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
        place,
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
    crate::diagnostics::emit_lazy(
        crate::diagnostics::LogLevel::Debug,
        crate::diagnostics::Event::ConnectionClosed,
        || serde_json::json!({}),
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
    place: Arc<IdlePlace>,
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
            place,
        } = self;
        injected_handler_panic();
        // Each request starts its own deadline without an earlier transfer's state.
        activity.send_replace(ObjectActivity::default());
        let finished = RequestFinished::new(activity.clone(), reads, place);
        let sent_body = has_body(request.headers());
        let endpoints = if let Ok(endpoints) = config
            .runtime_endpoints()
            .snapshot(&config, &state.api_base_url)
        {
            endpoints.for_request(listening.scheme, request.uri(), request.headers())
        } else {
            let mut response = boxed(
                ApiError::service_unavailable(
                    "Runtime endpoint state is unavailable; restart the emulator",
                )
                .into_response(),
            );
            listening.mark_response(response.headers_mut());
            return Ok(finished.attach(response));
        };
        let mut request = request;
        let (target, uri) = routing::Target::classify(&mut request, &config, endpoints.snapshot());
        let timeout_reply = DeadlineReply::new(&request, &target);
        let format = json::ResponseFormat::for_request(&uri, config.compat_profile());
        let ctx = RequestCtx {
            uri: &uri,
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
            config.trusted_allowed_hosts(),
        );
        // Dropping the handler cancels its work exactly as closing the
        // connection did, so an unfinished upload is still never published.
        let mut response = if let Err(error) = admitted {
            boxed(error.into_response())
        } else {
            tokio::select! {
                biased;
                response = json::with_format(
                    format,
                    routing::respond_classified(
                        request.map(|body| RequestBody::transport(body, activity.clone())),
                        target,
                        ctx,
                    ),
                ) => response,
                expiry = expiry_claimed(&activity) => timeout_reply.response(expiry),
            }
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
        activity.send_modify(ObjectActivity::respond);
        Ok(finished.attach(response))
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
            header_read: config.header_read_timeout(),
            keep_alive: config.keep_alive_timeout(),
            request: config.request_timeout(),
            body_inactivity: config.body_inactivity_timeout(),
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
enum ConnectionPhase {
    /// A new connection's first request head.
    FirstHead,
    /// The next request, between keep-alive requests. The place in the idle line lives here, so
    /// leaving this phase gives it up, and passes on any wake it received, on every path.
    Idle { turn: IdleTurn },
    /// The rest of a next request's head, which has started arriving.
    NextHead,
    /// A request's handler, or its bodies.
    Request,
    /// The write of a reply after which hyper closes the connection, such as a timeout reply,
    /// bounded by `CLOSE_BUDGET` because the client may not read.
    ClosingReply,
}

impl ConnectionPhase {
    /// The phase a connection enters when its request ends, and when that phase expires. Only a
    /// connection that stays open turns idle: one that closes waits for its reply to be written
    /// outside the idle line, so a waiting client's wake cannot reach it and close it before the
    /// lingering close, and the rest of a rejected body cannot pass for a next request head.
    fn after_request(
        end: AfterResponse,
        place: &IdlePlace,
        keep_alive: TimeLimit,
    ) -> (Self, tokio::time::Instant) {
        match end {
            AfterResponse::KeepAlive => (Self::Idle { turn: place.take() }, keep_alive.deadline()),
            AfterResponse::Closes => (
                Self::ClosingReply,
                tokio::time::Instant::now() + CLOSE_BUDGET,
            ),
        }
    }

    /// Resolves when an idle connection reaches the front of the idle line, and never in other
    /// phases.
    async fn idle_turn(&mut self) {
        match self {
            Self::Idle { turn } => turn.reached().await,
            Self::FirstHead | Self::NextHead | Self::Request | Self::ClosingReply => {
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
        crate::diagnostics::Event::ConnectionTimeout,
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
    /// Where each request's end leaves the connection's place in the idle line.
    place: Arc<IdlePlace>,
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
            place,
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
                Ok(()) = progress.changed(), if !matches!(phase, ConnectionPhase::ClosingReply) => {
                    let next = *progress.borrow_and_update();
                    if next.deadline() != current.deadline() && let Some(at) = next.deadline() {
                        object_deadline.as_mut().reset(at);
                    }
                    current = next;
                    match current.stage() {
                        RequestStage::Running | RequestStage::AwaitingCatalog | RequestStage::Expired(_) | RequestStage::Responded => {
                            // The next request can start before this watch sees the last one end,
                            // and a busy connection gives up the place that end took.
                            place.clear();
                            phase = ConnectionPhase::Request;
                            deadline.as_mut().reset(limits.during_request(&current).deadline());
                        }
                        // A whole request can run within one poll, so this phase can already be
                        // `Idle` and this is still a new idle period, in the place its end took.
                        RequestStage::Finished(end) => {
                            let (next, expires) = ConnectionPhase::after_request(end, &place, limits.keep_alive);
                            phase = next;
                            deadline.as_mut().reset(expires);
                        }
                    }
                },
                _ = reads.arrived.notified(), if matches!(phase, ConnectionPhase::Idle { .. }) => {
                    if reads.arrived_since_idle() {
                        phase = ConnectionPhase::NextHead;
                        deadline.as_mut().reset(limits.header_read.deadline());
                    }
                },
                _ = &mut object_deadline, if matches!(phase, ConnectionPhase::Request) && current.deadline().is_some() && current.stage() != RequestStage::AwaitingCatalog => {
                    match claim_expiry(&activity, Expiry::ObjectRequest) {
                        ExpiryClaim::Granted => {
                            crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, crate::diagnostics::Event::ObjectRequestTimeout, serde_json::json!({"message":"object request deadline exceeded"}));
                            deadline.as_mut().reset(tokio::time::Instant::now() + CLOSE_BUDGET);
                            phase = ConnectionPhase::ClosingReply;
                        }
                        // A catalog wait that started after this branch was chosen pauses the deadline.
                        ExpiryClaim::Refused => {}
                        ExpiryClaim::ResponseStarted => break UnreadInput::MayRemain,
                    }
                },
                result = std::future::poll_fn(|cx| connection.poll_without_shutdown(cx)) => {
                    // hyper's Display names only the failure kind (body stream, write, parse), never the wrapped I/O detail.
                    if let Err(error) = result { crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, crate::diagnostics::Event::ConnectionFailed, serde_json::json!({"message":"connection failed; check client request framing and transport","cause":error.to_string()})); }
                    break UnreadInput::MayRemain;
                }
                _ = &mut deadline, if current.stage() != RequestStage::AwaitingCatalog && current.response_work() == ResponseWork::Idle => {
                    match &phase {
                        ConnectionPhase::ClosingReply => break UnreadInput::MayRemain,
                        // The connection polled just above can have moved the request on, which
                        // decides first and renews this deadline.
                        _ if matches!(progress.has_changed(), Ok(true)) => {}
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
                                phase = ConnectionPhase::ClosingReply;
                            }
                            ExpiryClaim::Refused => {}
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

/// Moves the links of an object resource stored by an earlier request to this request's origin.
/// The stored links name the Host that finished the work, and a retry can arrive through another
/// address: a browser finishes on a published port, a peer asks through the service name. An
/// explicit base left them correct, so the stored response stays exactly as it was.
fn rebase_stored_links(
    endpoints: &crate::config::RequestEndpoints,
    resource: &mut serde_json::Value,
) {
    match endpoints.api_base_source() {
        crate::config::ApiBaseSource::Explicit => {}
        crate::config::ApiBaseSource::Derived => {
            json::rebase_object_links(resource, endpoints.api_base_url());
        }
    }
}

pub(crate) fn anonymous_error(error: ApiError, caller: Option<Caller<'_>>) -> ApiError {
    if matches!(caller, Some(Caller::Anonymous)) && error.status() == StatusCode::FORBIDDEN {
        error.for_anonymous_caller()
    } else {
        error
    }
}

/// Authorize a project-scoped collection request such as buckets.insert or buckets.list.
fn authorize_project(
    caller: Option<Caller<'_>>,
    permission: auth::Permission,
    project: &str,
) -> Result<(), ApiError> {
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
    ObjectBody::new(reader, activity.clone()).boxed_unsync()
}

/// Whether a POST that takes no body also accepts an empty JSON object as one.
#[derive(Clone, Copy)]
pub(crate) enum EmptyJsonBody {
    /// An official client sends `{}` here: google-cloud-cpp 3.13.0 does on objects.restore.
    Accepted,
    /// No pinned client sends one here, so it stays an unknown body.
    Refused,
}

/// The declared type of a body sent to a POST that takes none.
#[derive(Clone, Copy)]
enum BodylessPostType {
    Form,
    Json,
    Other,
}

async fn validate_bodyless_post(
    request: Request<RequestBody>,
    expected: &std::collections::BTreeMap<String, String>,
    empty_json: EmptyJsonBody,
) -> Result<(), ApiError> {
    // Ruby's generated client repeats URL query parameters in form-encoded POST bodies, and the
    // C++ client sends an empty JSON object to restore. Neither carries a value the URL query
    // lacks; any other body could, so it is still refused.
    let declared = match request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
    {
        Some("application/x-www-form-urlencoded") => BodylessPostType::Form,
        Some("application/json") => BodylessPostType::Json,
        _ => BodylessPostType::Other,
    };
    let body = crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    let accepted = body.is_empty()
        || match declared {
            BodylessPostType::Form => {
                std::str::from_utf8(&body)
                    .ok()
                    .and_then(|body| crate::percent::parse_query(body).ok())
                    .as_ref()
                    == Some(expected)
            }
            BodylessPostType::Json => match empty_json {
                EmptyJsonBody::Accepted => {
                    serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&body)
                        .is_ok_and(|members| members.is_empty())
                }
                EmptyJsonBody::Refused => false,
            },
            BodylessPostType::Other => false,
        };
    if !accepted {
        let example = expected
            .first_key_value()
            .map_or_default(|(key, value)| format!("{key}={value}"));
        let allowed = match empty_json {
            EmptyJsonBody::Accepted => "an empty body, an empty JSON object,",
            EmptyJsonBody::Refused => "an empty body,",
        };
        return Err(ApiError::invalid_argument(format!(
            "Use {allowed} or form parameters identical to the URL query, for example {example}"
        )));
    }
    Ok(())
}

/// Rejects headers that do not apply to a derived write (copy, compose or rewrite), naming the
/// first of `names` the request carries.
fn reject_headers(headers: &hyper::HeaderMap, names: &[&str]) -> Result<(), ApiError> {
    match names.iter().find(|name| headers.contains_key(**name)) {
        Some(name) => Err(ApiError::unsupported_object(name)),
        None => Ok(()),
    }
}

/// An `If-Match` or `If-None-Match` header that does not parse as entity tags.
fn invalid_etag_header() -> ApiError {
    ApiError::invalid_parameter("Invalid ETag precondition header")
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

/// The `Cache-Control` of IAM policy reads, whose provider value has not been recorded.
const IAM_POLICY_CACHE_CONTROL: &str = "no-cache";

/// What a metadata read that evaluates `If-Match` and `If-None-Match` returns, which sets its
/// `Cache-Control` and the form of a failed `If-Match`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValidatedRead {
    Object,
    Bucket,
    Policy,
    /// A bucket, default object or object ACL listing, validated by its owner's entity tag.
    AclList,
}

impl ValidatedRead {
    fn cache_control(self) -> &'static str {
        match self {
            Self::Object | Self::Bucket | Self::AclList => json::RESOURCE_CACHE_CONTROL,
            Self::Policy => IAM_POLICY_CACHE_CONTROL,
        }
    }

    /// Whether a failed `If-Match` names the header and carries the ETag, as Cloud Storage
    /// answered an object metadata read (case obj-download-validators) and the ACL listings
    /// (case bkt-probe-collection-validators). No failing bucket or policy read was recorded.
    fn names_failed_if_match(self) -> bool {
        match self {
            Self::Object | Self::AclList => true,
            Self::Bucket | Self::Policy => false,
        }
    }
}

fn read_metadata_response(
    headers: &hyper::HeaderMap,
    etag: &str,
    mut value: serde_json::Value,
    fields: &json::Fields,
    query: QueryPrecondition,
    read: ValidatedRead,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let not_modified = match etag_not_modified(headers, etag) {
        Ok(not_modified) => not_modified || query == QueryPrecondition::NotModified,
        Err(EtagError::Invalid) => return Err(invalid_etag_header()),
        Err(EtagError::PreconditionFailed) => {
            // The header form of the failure carries a shorter message than the query form.
            let error = ApiError::new(
                StatusCode::PRECONDITION_FAILED,
                "conditionNotMet",
                "Precondition Failed",
            );
            if !read.names_failed_if_match() {
                return Err(error);
            }
            let mut response = error.at_if_match().into_response();
            if let Ok(etag) = etag.parse() {
                response.headers_mut().insert("etag", etag);
            }
            return Ok(response);
        }
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
        hyper::header::HeaderValue::from_static(read.cache_control()),
    );
    Ok(response)
}

/// The `fields` selection a response applies, read from the request query as the operation
/// says.
fn response_fields(
    uri: &crate::percent::RequestUri,
    selection: json::FieldSelection<'_>,
) -> Result<json::Fields, ApiError> {
    match selection {
        json::FieldSelection::Ignored => Ok(json::Fields::whole()),
        json::FieldSelection::Query(schema) => json::Fields::from_query(uri, schema),
        json::FieldSelection::Parsed(fields) => Ok(fields.clone()),
    }
}

/// A `200 OK` JSON API resource with the cache headers its operation records.
fn json_response_cached(
    value: &serde_json::Value,
    cache: json::ResponseCache,
) -> Response<Full<Bytes>> {
    let mut response = json_response(value);
    let headers = response.headers_mut();
    match cache {
        json::ResponseCache::Unrecorded => {}
        json::ResponseCache::Resource => {
            headers.insert(
                "cache-control",
                hyper::header::HeaderValue::from_static(json::RESOURCE_CACHE_CONTROL),
            );
        }
        json::ResponseCache::NoStore => {
            headers.insert(
                "cache-control",
                hyper::header::HeaderValue::from_static(
                    crate::object::media_headers::NO_STORE_CACHE_CONTROL,
                ),
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
    cache: json::ResponseCache,
) -> Response<Full<Bytes>> {
    let etag = json::etag_header(value);
    fields.apply(value);
    let mut response = json_response_cached(value, cache);
    if let Some(etag) = etag {
        response.headers_mut().insert("etag", etag);
    }
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

fn unknown_route() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "notFound", "Route not found")
}

/// What a probe path asks: whether the process answers, or whether the API is ready.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Probe {
    Liveness,
    /// `/_internal/ready` and its compatible alias `/_internal/healthcheck`.
    Readiness,
}

impl Probe {
    fn parse(path: &str) -> Option<Self> {
        match path {
            "/_internal/health" => Some(Self::Liveness),
            "/_internal/ready" | "/_internal/healthcheck" => Some(Self::Readiness),
            _ => None,
        }
    }
}

fn probe(request: &Request<RequestBody>, state: &State) -> Option<Response<Full<Bytes>>> {
    let probe = Probe::parse(request.uri().path())?;
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
    } else if probe == Probe::Readiness && !state.is_ready() {
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
