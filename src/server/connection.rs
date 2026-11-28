//! One HTTP/1 connection: the service hyper calls for every request, the marker that ends a
//! request once hyper drops its response body, the watch that drives the connection under the
//! limits of [`super::deadline`], and the lingering close.

use super::{
    Shared, browser,
    deadline::{
        CLOSE_BUDGET, ConnectionLimits, ConnectionPhase, DeadlineReply, warn_connection_timeout,
    },
    routing,
    slots::{IdlePlace, SlotReclaim},
};
use crate::{
    auth::ListenerRole,
    cause::IoCause,
    config::{Config, ConnectionScheme},
    json::{self, ApiError, JsonErrorResponse},
    service::{
        activity::{
            AfterResponse, Expiry, ExpiryClaim, ObjectActivity, RequestStage, ResponseWork,
            claim_expiry, expiry_claimed,
        },
        body::{RequestBody, ResponseBody, boxed, has_body},
        context::RequestCtx,
        state::State,
    },
};
use http_body_util::BodyExt;
use hyper::{
    Request, Response,
    body::{Bytes, Incoming},
    server::conn::http1,
};
use hyper_util::rt::TokioIo;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::{
    convert::Infallible,
    future::Future,
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Notify, watch},
};

#[cfg(test)]
tokio::task_local! { pub(super) static FORCE_HANDLER_PANIC: Arc<AtomicBool>; }

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

/// The longest pause in client input that lingering close waits through. An
/// interrupted local upload keeps sending without pauses, while an idle client
/// would otherwise hold a connection slot for the whole lingering time.
const LINGER_IDLE: Duration = Duration::from_secs(1);
/// The cap on discarding input from a client that keeps sending after its
/// response, matching nginx's default `lingering_time`.
const LINGER_TIME: Duration = Duration::from_secs(30);

/// Ends a request when hyper drops its response body, after the last byte is written
/// or the connection fails, so keep-alive idle time is never charged to the request.
#[must_use = "dropping the guard marks the request finished"]
pub(super) struct RequestFinished {
    activity: watch::Sender<ObjectActivity>,
    reads: Arc<ReadActivity>,
    place: Arc<IdlePlace>,
    after: AfterResponse,
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
    pub(super) fn new(
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
    pub(super) fn attach(mut self, response: Response<ResponseBody>) -> Response<ResponseBody> {
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

/// What the requests of one connection learn from the listener that accepted it.
#[derive(Clone, Copy)]
pub(super) struct Listening {
    pub(super) scheme: ConnectionScheme,
    pub(super) role: ListenerRole,
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

/// hyper returns 431 when the HTTP/1 request head is still incomplete at the read buffer limit.
/// A head that completes in the same read can reach about twice the limit.
///
/// For request bodies, the limit bounds how much hyper reserves before each read. A read
/// fills the buffer's spare capacity, which can grow to about twice this value, so HTTP/1
/// body frames stay below `2 * CONNECTION_READ_BUFFER` (128 KiB).
/// [`stream_upload`](crate::service::upload_drive::stream_upload) forwards slices without
/// copying, so this bound is what keeps the memory an upload's two-slot channel retains small;
/// the tests of this module fail if the value or the frame bound changes.
const CONNECTION_READ_BUFFER: usize = 64 * 1024;

pub(super) fn http1_builder() -> http1::Builder {
    let mut builder = http1::Builder::new();
    builder.max_headers(64).max_buf_size(CONNECTION_READ_BUFFER);
    builder
}

pub(super) async fn serve_connection(
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

/// Whether the lingering close has client input to discard after the connection stops serving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnreadInput {
    MayRemain,
    /// An idle connection has no unread request bytes.
    None,
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

#[cfg(test)]
mod tests;
