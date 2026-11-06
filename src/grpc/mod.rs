//! Storage transports share domain state, never JSON wire representations.
mod convert;
#[cfg(test)]
pub(crate) mod test_support;
mod upload;
pub(crate) mod v1;
pub(crate) mod v2;
mod validate;

// Generated upstream code retains its own field names and documentation.
#[expect(
    clippy::allow_attributes,
    reason = "which lints fire in generated code varies by target and cfg(test), so an \
              expectation on them could go unfulfilled"
)]
#[allow(
    clippy::all,
    dead_code,
    unreachable_pub,
    rustdoc::broken_intra_doc_links,
    reason = "generated upstream code keeps its own style and documentation links"
)]
pub mod generated {
    include!("generated/mod.rs");
}

use crate::{
    config::Config,
    server::{CountingIo, IdleTurn, ReadActivity, SlotReclaim, State},
    time_limit::TimeLimit,
};
use http_body_util::BodyExt;
use hyper::{body::Incoming, server::conn::http2, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{net::TcpStream, sync::watch, time::Instant};
use tonic::codegen::Service;

/// The length of the fixed client preface that opens every HTTP/2 connection (RFC 9113 3.4).
const HTTP2_PREFACE_BYTES: u64 = 24;
/// How long a peer gets to answer GOAWAY when `header_read_timeout_ms` is disabled. A drain has
/// to end even then, because a dead peer never answers and may hold an open stream.
///
/// Also the most a connection without streams gets, to open its first one or to answer GOAWAY,
/// however long `header_read_timeout_ms` is. A full server holds one waiting client and leaves
/// the rest in the listen backlog, so a silent peer here delays every client behind that one.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

pub(crate) async fn serve_connection(
    stream: TcpStream,
    config: &Config,
    state: Arc<State>,
    stopping: watch::Receiver<bool>,
    slots: &SlotReclaim,
) {
    let legacy = v1::service(
        state.clone(),
        config.max_object_bytes,
        config.compat_profile,
    );
    let service = v2::service(state, config.max_object_bytes, config.compat_profile);
    serve_with_service(stream, config, stopping, slots, move |request| {
        let service = service.clone();
        let mut legacy = legacy.clone();
        async move {
            if request
                .uri()
                .path()
                .starts_with("/google.storage.v1.Storage/")
            {
                if !v1::known_method(request.uri().path()) {
                    return Ok(v2::status::unsupported(
                        "legacy v1 method outside the reference inventory",
                    )
                    .into_http());
                }
                return legacy.call(request).await;
            }
            v2::dispatch(service, request).await
        }
    })
    .await;
}

#[derive(Clone, Copy)]
struct Activity {
    streams: usize,
    last_frame: Instant,
}

struct StreamGuard(watch::Sender<Activity>);

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.0.send_modify(|activity| activity.streams -= 1);
    }
}

struct ResponseBody {
    body: tonic::body::Body,
    active: Option<StreamGuard>,
}

impl hyper::body::Body for ResponseBody {
    type Data = bytes::Bytes;
    type Error = tonic::Status;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let result = Pin::new(&mut self.body).poll_frame(cx);
        if matches!(result, Poll::Ready(None | Some(Err(_)))) || self.body.is_end_stream() {
            self.active.take();
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}

/// How far a connection has come, which selects the limit on its silence.
enum ConnectionPhase<'a> {
    /// Waiting for the client preface.
    Handshake,
    /// The preface arrived and no stream has opened yet. A client calls right after connecting,
    /// so this stays out of the idle line for a moment: with more clients than slots, each new
    /// connection would otherwise lose its slot to the next client before its first call, and
    /// none would ever finish one.
    AwaitingFirstStream { since: Instant },
    /// No stream is open. The place in the idle line lives here, so leaving this phase gives it
    /// up, and passes on any wake it received, on every path.
    Idle { turn: IdleTurn<'a> },
    /// At least one stream is open.
    Streaming,
    /// GOAWAY went out with no stream open. HTTP/2 serves a stream the peer opened before reading
    /// it, so that stream keeps an open stream's limits instead of this phase's short grace.
    ClosingIdle,
    /// GOAWAY went out after a deadline, and the peer gets a bounded time to finish.
    Draining,
    /// GOAWAY went out because the server is stopping.
    Stopping,
}

impl<'a> ConnectionPhase<'a> {
    /// Resolves when an idle connection reaches the front of the idle line, and never in other
    /// phases.
    async fn idle_turn(&mut self) {
        match self {
            Self::Idle { turn } => turn.reached().await,
            Self::Handshake
            | Self::AwaitingFirstStream { .. }
            | Self::Streaming
            | Self::ClosingIdle
            | Self::Draining
            | Self::Stopping => std::future::pending().await,
        }
    }

    /// Moves to the phase that `streams`, the open stream count after some stream activity, puts
    /// the connection in.
    fn after_stream_activity(&mut self, streams: usize, slots: &'a SlotReclaim) {
        match (&*self, streams) {
            (Self::ClosingIdle | Self::Draining | Self::Stopping, _) => {}
            // Activity with no stream left means one just ended, even when a whole stream ran
            // between two polls and this phase is already `Idle`, so this is a new idle period,
            // at the back of the line.
            (
                Self::Handshake
                | Self::AwaitingFirstStream { .. }
                | Self::Idle { .. }
                | Self::Streaming,
                0,
            ) => {
                *self = Self::Idle {
                    turn: slots.join_line(),
                };
            }
            (
                Self::Handshake
                | Self::AwaitingFirstStream { .. }
                | Self::Idle { .. }
                | Self::Streaming,
                _,
            ) => *self = Self::Streaming,
        }
    }
}

/// How long a stopping server lets open streams finish after GOAWAY. Half the outer drain budget
/// stays reserved for connection cleanup and joining. Neither part can be zero, because zero
/// disables a setting, so a connection always gets to see its GOAWAY before closing.
fn shutdown_grace(peer_wait: Duration, shutdown: TimeLimit) -> Duration {
    shutdown
        .duration()
        .map_or(peer_wait, |budget| peer_wait.min(budget / 2))
}

fn http2_builder(config: &Config) -> http2::Builder<TokioExecutor> {
    let mut builder = http2::Builder::new(TokioExecutor::new());
    // What a stream can hold is bounded, but only per stream, so a connection's ceiling grows
    // with the stream limit. Sending holds this buffer and one 64 KiB frame, about 13 MiB across
    // 100 streams. HTTP/2 flow control limits unreleased input to 1 MiB per connection, but tonic
    // releases it into a message of up to 4 MiB per stream, so 100 streams carrying that size at
    // once could reach 400 MiB. Those messages take a client that fills them on every stream.
    builder
        .max_concurrent_streams(config.max_grpc_streams.get())
        .max_header_list_size(16 * 1024)
        .max_send_buf_size(64 * 1024)
        .timer(TokioTimer::new());
    // A zero interval would ping in a loop and a zero timeout would fail the first ping, so a
    // disabled wait turns pings off and leaves dead peers to TCP keep-alive.
    match config.header_read_timeout.duration() {
        Some(wait) => builder.keep_alive_interval(wait).keep_alive_timeout(wait),
        None => builder.keep_alive_interval(None::<Duration>),
    };
    builder
}

/// Counts a stream from its request until its response body ends, and records each of its frames
/// as progress.
fn track_stream<S, F>(
    active: &watch::Sender<Activity>,
    service: &S,
    request: hyper::Request<Incoming>,
) -> impl Future<Output = Result<hyper::Response<ResponseBody>, Infallible>> + use<S, F>
where
    S: Fn(hyper::Request<tonic::body::Body>) -> F,
    F: Future<Output = Result<hyper::Response<tonic::body::Body>, Infallible>>,
{
    active.send_modify(|activity| {
        activity.streams += 1;
        activity.last_frame = Instant::now();
    });
    let guard = StreamGuard(active.clone());
    let incoming = active.clone();
    let outgoing = active.clone();
    let request = request.map(|body| {
        tonic::body::Body::new(body.map_frame(move |frame| {
            incoming.send_modify(|activity| activity.last_frame = Instant::now());
            frame
        }))
    });
    let response = service(request);
    async move {
        let response = response.await?;
        outgoing.send_modify(|activity| activity.last_frame = Instant::now());
        Ok::<_, Infallible>(response.map(|body| ResponseBody {
            body: tonic::body::Body::new(body.map_frame(move |frame| {
                outgoing.send_modify(|activity| activity.last_frame = Instant::now());
                frame
            })),
            active: Some(guard),
        }))
    }
}

async fn serve_with_service<S, F>(
    stream: TcpStream,
    config: &Config,
    mut stopping: watch::Receiver<bool>,
    slots: &SlotReclaim,
    service: S,
) where
    S: Fn(hyper::Request<tonic::body::Body>) -> F + Send + Sync + 'static,
    F: Future<Output = Result<hyper::Response<tonic::body::Body>, Infallible>> + Send + 'static,
{
    let (active, mut progress) = watch::channel(Activity {
        streams: 0,
        last_frame: Instant::now(),
    });
    let header_read = config.header_read_timeout;
    let reads = Arc::new(ReadActivity::default());
    let peer_wait = header_read.duration().unwrap_or(DRAIN_GRACE);
    let idle_grace = peer_wait.min(DRAIN_GRACE);
    let connection = http2_builder(config).serve_connection(
        TokioIo::new(CountingIo {
            inner: stream,
            reads: reads.clone(),
        }),
        service_fn(move |request: hyper::Request<Incoming>| {
            track_stream(&active, &service, request)
        }),
    );
    tokio::pin!(connection);
    // The handshake gets the header deadline. Once the client preface has arrived, a connection
    // without streams is idle and gets the keep-alive deadline.
    let idle = tokio::time::sleep_until(header_read.deadline());
    tokio::pin!(idle);
    let mut phase = ConnectionPhase::Handshake;
    loop {
        tokio::select! {
            biased;
            result = &mut connection => {
                if let Err(error) = result {
                    let (event, message) = if error.is_timeout() {
                        ("grpc_keepalive_timeout", "gRPC peer did not acknowledge an HTTP/2 keepalive ping; check the client connection or raise header_read_timeout_ms, for example 60000")
                    } else {
                        ("grpc_connection_failed", "gRPC connection failed; use a plaintext h2c client and grpc_listen, for example 127.0.0.1:4444")
                    };
                    crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, event, serde_json::json!({"message":message,"cause":error.to_string()}));
                }
                break;
            }
            Ok(()) = stopping.changed() => {
                let deadline = Instant::now() + shutdown_grace(peer_wait, config.shutdown_timeout);
                let deadline = match phase {
                    ConnectionPhase::ClosingIdle | ConnectionPhase::Draining => idle.deadline().min(deadline),
                    ConnectionPhase::Handshake
                    | ConnectionPhase::AwaitingFirstStream { .. }
                    | ConnectionPhase::Idle { .. }
                    | ConnectionPhase::Streaming
                    | ConnectionPhase::Stopping => {
                        connection.as_mut().graceful_shutdown();
                        deadline
                    }
                };
                phase = ConnectionPhase::Stopping;
                idle.as_mut().reset(deadline);
            }
            // Runs before the idle turn, so a stream the connection just accepted always leaves
            // the idle line first and never closes for a waiting client.
            Ok(()) = progress.changed(), if !matches!(phase, ConnectionPhase::Draining | ConnectionPhase::Stopping) => {
                let activity = *progress.borrow_and_update();
                phase.after_stream_activity(activity.streams, slots);
                let deadline = match (&phase, activity.streams) {
                    (ConnectionPhase::ClosingIdle, 0) => Instant::now() + idle_grace,
                    (_, 0) => config.keep_alive_timeout.deadline_from(activity.last_frame),
                    (_, _) => config.body_inactivity_timeout.deadline_from(activity.last_frame),
                };
                idle.as_mut().reset(deadline);
            }
            _ = reads.arrived.notified(), if matches!(phase, ConnectionPhase::Handshake) => {
                if reads.total() >= HTTP2_PREFACE_BYTES {
                    let since = Instant::now();
                    phase = ConnectionPhase::AwaitingFirstStream { since };
                    idle.as_mut().reset(config.keep_alive_timeout.deadline_from(since).min(since + idle_grace));
                }
            }
            () = phase.idle_turn() => {
                if slots.take_request() {
                    crate::diagnostics::emit(crate::diagnostics::LogLevel::Debug, "grpc_idle_reclaimed", serde_json::json!({"message":"gRPC connection was the longest idle while a client waited for a slot; sending GOAWAY; the client reconnects for its next call, or raise max_connections, for example 4096"}));
                    connection.as_mut().graceful_shutdown();
                    phase = ConnectionPhase::ClosingIdle;
                    idle.as_mut().reset(Instant::now() + idle_grace);
                } else {
                    phase = ConnectionPhase::Idle { turn: slots.join_line() };
                }
            }
            _ = &mut idle => {
                let streams = progress.borrow().streams;
                let (next, wait) = match (&phase, streams) {
                    (ConnectionPhase::AwaitingFirstStream { since }, _) => {
                        // Keep-alive may have run out first, and then this deadline fires at once.
                        idle.as_mut().reset(config.keep_alive_timeout.deadline_from(*since));
                        phase = ConnectionPhase::Idle { turn: slots.join_line() };
                        continue;
                    }
                    (ConnectionPhase::Draining | ConnectionPhase::Stopping, _) | (ConnectionPhase::ClosingIdle, 0) => {
                        crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "grpc_drain_timeout", serde_json::json!({"message":"gRPC GOAWAY grace expired with an incomplete handshake or unfinished stream; finish client streams or raise header_read_timeout_ms, for example 60000; for SIGTERM also raise shutdown_timeout_ms, for example 20000","shutdown":matches!(phase, ConnectionPhase::Stopping)}));
                        break;
                    }
                    (ConnectionPhase::Handshake, _) => {
                        crate::diagnostics::emit(crate::diagnostics::LogLevel::Info, "grpc_handshake_timeout", serde_json::json!({"message":"gRPC client sent no HTTP/2 preface before header_read_timeout_ms; sending GOAWAY; use a plaintext h2c client or raise header_read_timeout_ms, for example 60000"}));
                        (ConnectionPhase::Draining, peer_wait)
                    }
                    (ConnectionPhase::Idle { .. } | ConnectionPhase::Streaming, 0) => {
                        crate::diagnostics::emit(crate::diagnostics::LogLevel::Debug, "grpc_idle_timeout", serde_json::json!({"message":"gRPC connection idle deadline exceeded; sending GOAWAY; reconnect or raise keep_alive_timeout_ms, for example 300000"}));
                        (ConnectionPhase::ClosingIdle, idle_grace)
                    }
                    (ConnectionPhase::Idle { .. } | ConnectionPhase::Streaming | ConnectionPhase::ClosingIdle, _) => {
                        crate::diagnostics::emit(crate::diagnostics::LogLevel::Info, "grpc_stream_inactivity_timeout", serde_json::json!({"message":"gRPC stream remained open without request or response frames for body_inactivity_timeout_ms; sending GOAWAY; send periodically, close and reopen the stream, or raise the setting (0 disables it)"}));
                        (ConnectionPhase::Draining, peer_wait)
                    }
                };
                connection.as_mut().graceful_shutdown();
                phase = next;
                idle.as_mut().reset(Instant::now() + wait);
            }
        }
    }
}

#[cfg(test)]
mod tests;
