//! The accept loop: the listeners one server accepts on, the task it starts for every connection,
//! and the drain once it stops. While every slot is taken, the next client waits for an idle
//! connection to give up its slot, as [`super::slots`] arranges.

#[cfg(test)]
use super::connection::FORCE_HANDLER_PANIC;
use super::{
    Shared,
    connection::{Listening, serve_connection},
    grpc_listener,
    handshake::{Established, ListenerProtocol, Secured, establish_tcp},
    slots::{SlotReclaim, SlotRequest},
    transport,
};
#[cfg(unix)]
use super::{handshake::establish, unix};
use crate::{
    auth::ListenerRole,
    config::{Config, ConnectionScheme, GrpcScheme},
    diagnostics::Diagnostics,
    service::state::State,
    time_limit::TimeLimit,
    tls::ListenerSecurity,
};
use std::{
    future::Future,
    io,
    sync::{Arc, atomic::Ordering},
    task::Poll,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpListener,
    sync::watch,
    task::JoinSet,
};

#[cfg(test)]
tokio::task_local! { static ACCEPT_FAULTS: Arc<std::sync::Mutex<std::collections::VecDeque<i32>>>; }

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

/// A client accepted while every slot is taken.
struct HeldClient<'a> {
    connection: NewConnection,
    _request: SlotRequest<'a>,
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
mod tests;
