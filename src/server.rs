use crate::{
    auth::{self, Auth, Caller},
    bucket::{BucketAccess, CatalogError},
    clock::Clock,
    config::Config,
    http_conditions::{EtagError, etag_not_modified},
    json::{self, JsonError},
    object::{ObjectError, UploadChunk},
    storage::Catalog,
    tls, xml,
};
use http_body_util::{BodyExt, Channel, Full, combinators::UnsyncBoxBody};
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
    sync::{mpsc, watch},
    task::JoinSet,
};

pub struct State {
    pub ready: AtomicBool,
    pub clock: Arc<dyn Clock>,
    pub catalog: Catalog,
    pub api_base_url: String,
    pub default_location: String,
    pub auth: Auth,
}

impl State {
    pub fn is_ready(&self) -> bool {
        if !self.catalog.is_healthy() {
            self.ready.store(false, Ordering::Release);
        }
        self.ready.load(Ordering::Acquire)
    }
}

pub const BUCKET_BODY_LIMIT: usize = 64 * 1024;

mod buckets;
mod context;
mod objects;
mod routing;
mod xml_read;
pub(crate) use context::{DeadlineWatch, ObjectCtx, RequestCtx};

#[cfg(test)]
tokio::task_local! { static FORCE_LISTENER_ERROR: bool; }
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
mod copy;
mod gzip;
mod notifications;
mod project_resources;
mod resumable;
mod retention_access;
mod rewrite;
mod unsupported;

/// The longest pause in client input that lingering close waits through. An
/// interrupted local upload keeps sending without pauses, while an idle client
/// would otherwise hold a connection slot for the whole lingering time.
const LINGER_IDLE: Duration = Duration::from_secs(1);
/// The cap on discarding input from a client that keeps sending after its
/// response, matching nginx's default `lingering_time`.
const LINGER_TIME: Duration = Duration::from_secs(30);

/// The response for a request whose deadline expires before its handler answers.
/// Cloud Storage documents both statuses as retryable. An object upload reports
/// the broken upload; other requests, including bucket writes whose small bodies
/// are read at once, report a backend timeout.
struct DeadlineReply {
    xml: bool,
    body: xml::ErrorBody,
    upload: bool,
}

fn has_body(request: &Request<Incoming>) -> bool {
    let headers = request.headers();
    headers.contains_key(hyper::header::TRANSFER_ENCODING)
        || headers
            .get(hyper::header::CONTENT_LENGTH)
            .is_some_and(|length| length != "0")
}

impl DeadlineReply {
    fn new(request: &Request<Incoming>, config: &Config) -> Self {
        let headers = request.headers();
        let xml = matches!(
            xml::select_object(request.uri(), headers, config),
            Ok(Some(_))
        );
        let upload_route = request.uri().path().starts_with("/upload/")
            || (xml && matches!(*request.method(), Method::PUT | Method::POST));
        Self {
            xml,
            body: xml::ErrorBody::for_method(request.method()),
            upload: has_body(request) && upload_route,
        }
    }

    fn response(self) -> Response<ResponseBody> {
        let error = if self.upload {
            JsonError::new(
                StatusCode::REQUEST_TIMEOUT,
                "uploadBrokenConnection",
                "The upload timed out; retry the request, or raise connection_timeout_ms for slow clients or object_request_timeout_ms for large uploads.",
            )
        } else {
            JsonError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "The request timed out before a response was ready; retry the request, or raise connection_timeout_ms.",
            )
        };
        let mut response = if self.xml {
            boxed(xml::Error::from(error).response(self.body))
        } else {
            boxed(error.into_response())
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
    active: Arc<AtomicBool>,
    activity: watch::Sender<ObjectActivity>,
    reads: Arc<ReadActivity>,
}

/// Counts bytes read from a connection, so the idle wait between keep-alive
/// requests can tell when the client starts its next request head.
#[derive(Default)]
struct ReadActivity {
    read: AtomicU64,
    idle_mark: AtomicU64,
    arrived: tokio::sync::Notify,
}

impl ReadActivity {
    fn mark_idle(&self) {
        self.idle_mark
            .store(self.read.load(Ordering::Acquire), Ordering::Release);
    }

    fn arrived_since_idle(&self) -> bool {
        self.read.load(Ordering::Acquire) > self.idle_mark.load(Ordering::Acquire)
    }
}

struct CountingIo<S> {
    inner: S,
    reads: Arc<ReadActivity>,
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
        self.active.store(false, Ordering::Release);
        self.activity.send_replace(ObjectActivity::default());
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct ObjectActivity {
    awaiting_catalog: bool,
    deadline: Option<tokio::time::Instant>,
}

pub async fn serve(
    listener: TcpListener,
    config: impl Into<Arc<Config>>,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let config = config.into();
    let acceptor = tls::load_server(&config)?;
    serve_with_tls(vec![(listener, acceptor)], config, state, shutdown).await
}

pub async fn serve_with_tls(
    listeners: Vec<(TcpListener, Option<tokio_rustls::TlsAcceptor>)>,
    config: impl Into<Arc<Config>>,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    serve_with_tls_and_drain(listeners, config, state, shutdown, async { Ok(()) }).await
}

pub async fn serve_with_tls_and_drain(
    listeners: Vec<(TcpListener, Option<tokio_rustls::TlsAcceptor>)>,
    config: impl Into<Arc<Config>>,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
    background_drain: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    serve_with_grpc_and_drain(listeners, None, config, state, shutdown, background_drain).await
}

pub async fn serve_with_grpc_and_drain(
    mut listeners: Vec<(TcpListener, Option<tokio_rustls::TlsAcceptor>)>,
    grpc: Option<TcpListener>,
    config: impl Into<Arc<Config>>,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
    background_drain: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    // Connections and requests share one configuration instead of cloning it per request.
    let config: Arc<Config> = config.into();
    if listeners.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "at least one listener is required",
        ));
    }
    let grpc_index = grpc.as_ref().map(|_| listeners.len());
    if let Some(listener) = grpc {
        listeners.push((listener, None));
    }
    let (stop, _) = watch::channel(false);
    // True while every connection slot is taken, so idle keep-alive connections
    // give their slots to waiting clients, as nginx reuses idle connections.
    let (full, _) = watch::channel(false);
    let max_connections = config.max_connections;
    let mut connections = JoinSet::new();
    let mut next_listener = 0;
    tokio::pin!(shutdown);
    let signal_result = loop {
        tokio::select! {
            result = &mut shutdown => break result,
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = result {
                    report_connection_task_failure(&error);
                }
                full.send_replace(connections.len() >= max_connections);
            }
            incoming = std::future::poll_fn(|cx| {
                #[cfg(test)]
                if FORCE_LISTENER_ERROR.try_with(|enabled| *enabled).unwrap_or(false) {
                    return std::task::Poll::Ready(Err(io::Error::other("injected listener failure")));
                }
                for offset in 0..listeners.len() {
                    let index = (next_listener + offset) % listeners.len();
                    if let std::task::Poll::Ready(result) = listeners[index].0.poll_accept(cx) {
                        return std::task::Poll::Ready(result.map(|(stream, _)| (index, stream)));
                    }
                }
                std::task::Poll::Pending
            }), if connections.len() < config.max_connections => {
                let (index, stream) = match incoming {
                    Ok(incoming) => incoming,
                    Err(error) => break Err(error),
                };
                next_listener = (index + 1) % listeners.len();
                let state = state.clone();
                let mut stopping = stop.subscribe();
                let saturated = full.subscribe();
                let config = config.clone();
                let acceptor = listeners[index].1.clone();
                let connection = async move {
                    if grpc_index == Some(index) {
                        crate::grpc::serve_connection(stream, &config, state, stopping).await;
                    } else if let Some(acceptor) = acceptor {
                        let handshake = tokio::time::timeout(
                            Duration::from_millis(config.tls_handshake_timeout_ms),
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
                        serve_connection(stream, state, config, stopping, saturated).await;
                    } else {
                        serve_connection(stream, state, config, stopping, saturated).await;
                    }
                };
                connections.spawn(with_test_hooks(connection));
                full.send_replace(connections.len() >= max_connections);
            }
        }
    };
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Info,
        "stopping",
        serde_json::json!({}),
    );
    state.ready.store(false, Ordering::Release);
    drop(listeners);
    stop.send_replace(true);
    let drain_result = drain_connections(
        &mut connections,
        Duration::from_millis(config.shutdown_timeout_ms),
    )
    .await;
    let background_result = background_drain.await;
    crate::diagnostics::combine_shutdown_results(
        crate::diagnostics::combine_shutdown_results(
            signal_result.and(drain_result),
            background_result,
        ),
        state.catalog.close().await.map_err(io::Error::other),
    )
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

/// Waits for every connection to finish within `deadline`, then aborts the rest.
/// A panicked connection is reported and does not stop draining the others.
async fn drain_connections(connections: &mut JoinSet<()>, deadline: Duration) -> io::Result<()> {
    let drain = async {
        while let Some(result) = connections.join_next().await {
            if let Err(error) = result {
                report_connection_task_failure(&error);
            }
        }
    };
    let result = tokio::time::timeout(deadline, drain)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "shutdown drain deadline exceeded"));
    if result.is_err() {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    result
}

#[cfg(test)]
mod drain_tests;

async fn serve_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    state: Arc<State>,
    config: Arc<Config>,
    mut stopping: watch::Receiver<bool>,
    mut saturated: watch::Receiver<bool>,
) {
    let timeout = Duration::from_millis(config.connection_timeout_ms);
    let keep_alive = Duration::from_millis(config.keep_alive_timeout_ms);
    let (activity, progress) = watch::channel(ObjectActivity::default());
    // A request is active from dispatch until hyper drops its response body after
    // the last byte. Request deadlines apply only then. Between keep-alive requests
    // the connection waits for the next byte for keep_alive_timeout_ms; once one
    // arrives, the rest of the request head gets connection_timeout_ms.
    let active = Arc::new(AtomicBool::new(false));
    let served = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(ReadActivity::default());
    // Set once the service returns, after which hyper may already have sent the
    // response head; until then an expired deadline can still be answered.
    let responded = Arc::new(AtomicBool::new(false));
    let (expire, expired) = watch::channel(false);
    let mut builder = http1::Builder::new();
    builder.max_headers(64).max_buf_size(16 * 1024);
    let service = ConnectionService {
        state,
        config,
        activity,
        active: active.clone(),
        served: served.clone(),
        responded: responded.clone(),
        reads: reads.clone(),
        expired,
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
        active,
        served,
        reads,
        responded,
        expire,
        timeout,
        keep_alive,
    };
    let lingering = watch
        .run(&mut connection, &mut stopping, &mut saturated)
        .await;
    // Send FIN after the response, then read and discard what the client still sends,
    // like an HTTP server's lingering close: closing with unread input sends a reset
    // that can discard an early rejection before the client reads it. The phase ends
    // when the client closes or pauses, after the lingering time, or on shutdown.
    let mut stream = connection.into_parts().io.into_inner();
    let _ = tokio::time::timeout(timeout, stream.shutdown()).await;
    if lingering {
        discard_lingering_input(&mut stream, &mut stopping).await;
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
    state: Arc<State>,
    config: Arc<Config>,
    activity: watch::Sender<ObjectActivity>,
    active: Arc<AtomicBool>,
    served: Arc<AtomicBool>,
    responded: Arc<AtomicBool>,
    reads: Arc<ReadActivity>,
    expired: watch::Receiver<bool>,
}

type ServiceFuture =
    std::pin::Pin<Box<dyn Future<Output = Result<Response<ResponseBody>, Infallible>> + Send>>;

impl hyper::service::Service<Request<Incoming>> for ConnectionService {
    type Response = Response<ResponseBody>;
    type Error = Infallible;
    type Future = ServiceFuture;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        self.served.store(true, Ordering::Release);
        Box::pin(self.clone().respond(request))
    }
}

impl ConnectionService {
    async fn respond(
        self,
        request: Request<Incoming>,
    ) -> Result<Response<ResponseBody>, Infallible> {
        let Self {
            state,
            config,
            activity,
            active,
            responded,
            reads,
            mut expired,
            ..
        } = self;
        #[cfg(test)]
        if FORCE_HANDLER_PANIC
            .try_with(|armed| armed.swap(false, Ordering::AcqRel))
            .unwrap_or(false)
        {
            panic!("injected handler panic");
        }
        responded.store(false, Ordering::Release);
        active.store(true, Ordering::Release);
        // Each request starts its own deadline without an earlier transfer's state.
        activity.send_replace(ObjectActivity::default());
        let finished = RequestFinished {
            active,
            activity: activity.clone(),
            reads,
        };
        let expiry = DeadlineReply::new(&request, &config);
        let sent_body = has_body(&request);
        let ctx = RequestCtx {
            state: &state,
            config: &config,
            activity: &activity,
        };
        // Dropping the handler cancels its work exactly as closing the
        // connection did, so an unfinished upload is still never published.
        let mut response = tokio::select! {
            biased;
            response = routing::respond(request, ctx) => response,
            _ = expired.wait_for(|expired| *expired) => expiry.response(),
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
        responded.store(true, Ordering::Release);
        Ok(response.map(|body| finished.track(body)))
    }
}

/// The connection half that enforces connection, keep-alive and request deadlines.
struct ConnectionWatch {
    progress: watch::Receiver<ObjectActivity>,
    active: Arc<AtomicBool>,
    served: Arc<AtomicBool>,
    reads: Arc<ReadActivity>,
    responded: Arc<AtomicBool>,
    expire: watch::Sender<bool>,
    timeout: Duration,
    keep_alive: Duration,
}

impl ConnectionWatch {
    /// Drives `connection` until it ends or a deadline closes it. Returns whether unread
    /// client input may remain, which the lingering close then discards.
    async fn run<S: AsyncRead + AsyncWrite + Unpin>(
        self,
        connection: &mut http1::Connection<TokioIo<CountingIo<S>>, ConnectionService>,
        stopping: &mut watch::Receiver<bool>,
        saturated: &mut watch::Receiver<bool>,
    ) -> bool {
        let Self {
            mut progress,
            active,
            served,
            reads,
            responded,
            expire,
            timeout,
            keep_alive,
        } = self;
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let mut current = ObjectActivity::default();
        let mut draining = false;
        let mut expiring = false;
        let mut next_head = false;
        // An idle connection has no unread request bytes to discard before closing.
        let mut lingering = true;
        loop {
            let requesting = active.load(Ordering::Acquire);
            let idle = served.load(Ordering::Acquire) && !requesting;
            tokio::select! {
                biased;
                Ok(()) = progress.changed(), if !expiring => {
                    current = *progress.borrow_and_update();
                    next_head = false;
                    // A finished request leaves the connection waiting for the next one.
                    let idle = served.load(Ordering::Acquire) && !active.load(Ordering::Acquire);
                    deadline.as_mut().reset(tokio::time::Instant::now() + if idle { keep_alive } else { timeout });
                },
                _ = reads.arrived.notified(), if idle && !next_head && !expiring => {
                    if reads.arrived_since_idle() {
                        next_head = true;
                        deadline.as_mut().reset(tokio::time::Instant::now() + timeout);
                    }
                },
                _ = tokio::time::sleep_until(current.deadline.unwrap_or_else(tokio::time::Instant::now)), if requesting && current.deadline.is_some() && !current.awaiting_catalog && !expiring => {
                    crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "object_request_timeout", serde_json::json!({"message":"object request deadline exceeded"}));
                    if responded.load(Ordering::Acquire) { break; }
                    expire.send_replace(true);
                    deadline.as_mut().reset(tokio::time::Instant::now() + timeout);
                    expiring = true;
                },
                result = std::future::poll_fn(|cx| connection.poll_without_shutdown(cx)) => {
                    // hyper's Display names only the failure kind (body stream, write, parse), never the wrapped I/O detail.
                    if let Err(error) = result { crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "connection_failed", serde_json::json!({"message":"connection failed; check client request framing and transport","cause":error.to_string()})); }
                    break;
                }
                _ = &mut deadline, if !requesting || !current.awaiting_catalog || expiring => {
                    // After an expiry the same budget bounds writing its error response to a client that does not read.
                    if expiring { break; }
                    if !requesting {
                        // An idle keep-alive connection closes quietly; a request head that
                        // stalls after it started, or never arrives, is a client fault.
                        if !idle || next_head {
                            crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "connection_timeout", serde_json::json!({"message":"request headers were not received before connection_timeout_ms"}));
                        }
                        lingering = !idle || reads.arrived_since_idle();
                        break;
                    }
                    crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "connection_timeout", serde_json::json!({"message":"connection deadline or object body inactivity timeout exceeded"}));
                    if responded.load(Ordering::Acquire) { break; }
                    expire.send_replace(true);
                    deadline.as_mut().reset(tokio::time::Instant::now() + timeout);
                    expiring = true;
                },
                _ = saturated.wait_for(|full| *full), if idle && !next_head && !expiring => {
                    // A full server takes the slot back for a waiting client unless the
                    // next request has already started arriving.
                    if reads.arrived_since_idle() {
                        next_head = true;
                        deadline.as_mut().reset(tokio::time::Instant::now() + timeout);
                    } else {
                        lingering = false;
                        break;
                    }
                },
                _ = stopping.changed(), if !draining => {
                    std::pin::Pin::new(&mut *connection).graceful_shutdown();
                    draining = true;
                }
            }
        }
        lingering
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
        auth::AuthFailure::LoginRequired.into()
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
    permission: &str,
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
    permission: &str,
    project: &str,
) -> Result<(), JsonError> {
    match caller {
        Some(caller) => Ok(caller.authorize(permission, Some(project))?),
        None => Ok(()),
    }
}

fn object_body(
    mut reader: crate::storage::ObjectReader,
    activity: &watch::Sender<ObjectActivity>,
) -> ResponseBody {
    let (mut sender, body) = Channel::<Bytes, io::Error>::new(1);
    let activity = activity.clone();
    tokio::spawn(async move {
        loop {
            match reader.next_chunk().await {
                Ok(Some(bytes)) => {
                    if sender.send_data(bytes).await.is_err() {
                        break;
                    }
                    activity.send_modify(|progress| progress.awaiting_catalog = false);
                }
                Ok(None) => break,
                Err(error) => {
                    // hyper reports only that the body was aborted; name the read failure.
                    crate::diagnostics::emit(
                        crate::diagnostics::LogLevel::Warn,
                        "object_body_read_failed",
                        serde_json::json!({
                            "message": "object data read failed after the response started; the client receives a truncated body",
                            "cause": error.kind().to_string(),
                        }),
                    );
                    sender.abort(error);
                    break;
                }
            }
        }
    });
    body.boxed_unsync()
}

async fn validate_bodyless_post(
    request: Request<Incoming>,
    expected: &std::collections::BTreeMap<String, String>,
) -> Result<(), JsonError> {
    // Ruby's generated client repeats URL query parameters in form-encoded POST bodies.
    let form = request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').next() == Some("application/x-www-form-urlencoded"));
    let body = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
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

pub(crate) async fn stream_upload(
    mut body: Incoming,
    sender: &mpsc::Sender<Result<UploadChunk, io::Error>>,
    max_bytes: u64,
    activity: &watch::Sender<ObjectActivity>,
    expected_sha256: Option<[u8; 32]>,
) -> Result<(), ObjectError> {
    let mut size = 0_u64;
    let mut sha256 = expected_sha256.map(|_| ring::digest::Context::new(&ring::digest::SHA256));
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| ObjectError::BodyAborted(error.to_string()))?;
        let mut bytes = frame
            .into_data()
            .map_err(|_| ObjectError::BodyAborted("Upload trailers are unsupported".into()))?;
        size = size
            .checked_add(bytes.len() as u64)
            .filter(|size| *size <= max_bytes)
            .ok_or(ObjectError::TooLarge)?;
        activity.send_modify(|progress| progress.awaiting_catalog = false);
        if let Some(hash) = &mut sha256 {
            hash.update(&bytes);
        }
        // HTTP/1 frames are capped by the 16 KiB connection read buffer, so passing them on
        // without a copy keeps what the two-slot channel retains bounded; larger frames are
        // still split into 64 KiB chunks.
        while !bytes.is_empty() {
            let part = bytes.split_to(bytes.len().min(64 * 1024));
            if sender.send(Ok(UploadChunk::Data(part))).await.is_err() {
                // Preserve the catalog's original failure when it stops accepting the body.
                return Ok(());
            }
        }
    }
    if sha256
        .zip(expected_sha256)
        .is_some_and(|(hash, expected)| hash.finish().as_ref() != expected)
    {
        return Err(ObjectError::ChecksumMismatch);
    }
    Ok(())
}

/// Record body progress for an adapter that reads the request body through its own parser.
pub(crate) fn body_progress(activity: &watch::Sender<ObjectActivity>) {
    activity.send_modify(|progress| progress.awaiting_catalog = false);
}

pub(crate) async fn await_upload_catalog<T, E: From<ObjectError>>(
    operation: impl Future<Output = Result<T, E>>,
    timeout: Duration,
    request_deadline: tokio::time::Instant,
    activity: &watch::Sender<ObjectActivity>,
) -> Result<T, E> {
    activity.send_modify(|progress| progress.awaiting_catalog = true);
    // A completed body may commit after this wait expires; only an observed commit is success.
    let result = tokio::time::timeout_at(
        request_deadline.min(tokio::time::Instant::now() + timeout),
        operation,
    )
    .await
    .unwrap_or_else(|_| {
        Err(ObjectError::Unavailable("upload catalog wait timed out".into()).into())
    });
    activity.send_modify(|progress| progress.awaiting_catalog = false);
    result
}

fn read_etag_not_modified(headers: &hyper::HeaderMap, etag: &str) -> Result<bool, JsonError> {
    etag_not_modified(headers, etag).map_err(|error| match error {
        EtagError::Invalid => JsonError::invalid_parameter("Invalid ETag precondition header"),
        EtagError::PreconditionFailed => ObjectError::PreconditionFailed.into(),
    })
}

fn not_modified_response(
    etag: &hyper::header::HeaderValue,
    cache_control: &hyper::header::HeaderValue,
) -> Response<Full<Bytes>> {
    let mut response = empty_response(StatusCode::NOT_MODIFIED);
    let headers = response.headers_mut();
    headers.insert("etag", etag.clone());
    headers.insert("cache-control", cache_control.clone());
    response
}

/// Whether a metadata read's query preconditions (`ifGenerationNotMatch`,
/// `ifMetagenerationNotMatch`) already select a `304 Not Modified` response.
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryPrecondition {
    NotModified,
    Unmatched,
}

fn read_metadata_response(
    headers: &hyper::HeaderMap,
    etag: &str,
    mut value: serde_json::Value,
    fields: &json::Fields,
    query: QueryPrecondition,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let not_modified =
        read_etag_not_modified(headers, etag)? || query == QueryPrecondition::NotModified;
    let no_cache = hyper::header::HeaderValue::from_static("no-cache");
    // ETags are rendered locally from quoted ASCII digits and base64, so the conversion always
    // succeeds; omitting the validator is safer than panicking if that ever changes.
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
    response_headers.insert("cache-control", no_cache);
    Ok(response)
}

fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}

/// A `200 OK` JSON API resource.
fn json_response(value: &serde_json::Value) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(value.to_string())));
    response.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/json; charset=UTF-8"),
    );
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

fn probe_or_unknown(request: &Request<Incoming>, state: &State) -> Response<Full<Bytes>> {
    let path = request.uri().path();
    let internal = matches!(path, "/_internal/health" | "/_internal/ready");
    let (status, body) = if !internal {
        (
            StatusCode::NOT_FOUND,
            "{\"error\":\"route not implemented\"}".to_owned(),
        )
    } else if request.method() != Method::GET && request.method() != Method::HEAD {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            "{\"error\":\"method not allowed\"}".to_owned(),
        )
    } else if request.headers().contains_key("transfer-encoding")
        || request
            .headers()
            .get("content-length")
            .is_some_and(|v| v != "0")
    {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "{\"error\":\"probe bodies are not supported\"}".to_owned(),
        )
    } else if path == "/_internal/ready" && !state.is_ready() {
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
    response
}
