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
    server::{CountingIo, ReadActivity, State},
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
use tokio::{net::TcpStream, sync::watch};
use tonic::codegen::Service;

/// The length of the fixed client preface that opens every HTTP/2 connection (RFC 9113 3.4).
const HTTP2_PREFACE_BYTES: u64 = 24;
/// How long a peer gets to answer GOAWAY when `header_read_timeout_ms` is disabled. A drain has
/// to end even then, because a dead peer never answers and may hold an open stream.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

pub(crate) async fn serve_connection(
    stream: TcpStream,
    config: &Config,
    state: Arc<State>,
    stopping: watch::Receiver<bool>,
) {
    let legacy = v1::service(
        state.clone(),
        config.max_object_bytes,
        config.compat_profile,
    );
    let service = v2::service(state, config.max_object_bytes, config.compat_profile);
    serve_with_service(stream, config, stopping, move |request| {
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
    last_frame: tokio::time::Instant,
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectionPhase {
    /// Waiting for the client preface.
    Handshake,
    /// Serving streams, or idle between them.
    Open,
    /// GOAWAY went out after a deadline, and the peer gets a bounded time to finish.
    Draining,
    /// GOAWAY went out because the server is stopping.
    Stopping,
}

/// How long a stopping server lets open streams finish after GOAWAY. Half the outer drain budget
/// stays reserved for connection cleanup and joining. Neither part can be zero, because zero
/// disables a setting, so a connection always gets to see its GOAWAY before closing.
fn shutdown_grace(peer_wait: Duration, shutdown: TimeLimit) -> Duration {
    shutdown
        .duration()
        .map_or(peer_wait, |budget| peer_wait.min(budget / 2))
}

fn http2_builder(ping_wait: TimeLimit) -> http2::Builder<TokioExecutor> {
    let mut builder = http2::Builder::new(TokioExecutor::new());
    builder
        .max_concurrent_streams(8)
        .max_header_list_size(16 * 1024)
        .max_send_buf_size(64 * 1024)
        .timer(TokioTimer::new());
    // A zero interval would ping in a loop and a zero timeout would fail the first ping, so a
    // disabled wait turns pings off and leaves dead peers to TCP keep-alive.
    match ping_wait.duration() {
        Some(wait) => builder.keep_alive_interval(wait).keep_alive_timeout(wait),
        None => builder.keep_alive_interval(None::<Duration>),
    };
    builder
}

async fn serve_with_service<S, F>(
    stream: TcpStream,
    config: &Config,
    mut stopping: watch::Receiver<bool>,
    service: S,
) where
    S: Fn(hyper::Request<tonic::body::Body>) -> F + Send + Sync + 'static,
    F: Future<Output = Result<hyper::Response<tonic::body::Body>, Infallible>> + Send + 'static,
{
    let (active, mut progress) = watch::channel(Activity {
        streams: 0,
        last_frame: tokio::time::Instant::now(),
    });
    let header_read = config.header_read_timeout;
    let reads = Arc::new(ReadActivity::default());
    let peer_wait = header_read.duration().unwrap_or(DRAIN_GRACE);
    let connection = http2_builder(header_read).serve_connection(
        TokioIo::new(CountingIo {
            inner: stream,
            reads: reads.clone(),
        }),
        service_fn(move |request: hyper::Request<Incoming>| {
            active.send_modify(|activity| {
                activity.streams += 1;
                activity.last_frame = tokio::time::Instant::now();
            });
            let guard = StreamGuard(active.clone());
            let incoming = active.clone();
            let outgoing = active.clone();
            let request = request.map(|body| {
                tonic::body::Body::new(body.map_frame(move |frame| {
                    incoming
                        .send_modify(|activity| activity.last_frame = tokio::time::Instant::now());
                    frame
                }))
            });
            let response = service(request);
            async move {
                let response = response.await?;
                outgoing.send_modify(|activity| activity.last_frame = tokio::time::Instant::now());
                Ok::<_, Infallible>(response.map(|body| ResponseBody {
                    body: tonic::body::Body::new(body.map_frame(move |frame| {
                        outgoing.send_modify(|activity| {
                            activity.last_frame = tokio::time::Instant::now()
                        });
                        frame
                    })),
                    active: Some(guard),
                }))
            }
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
                let deadline = tokio::time::Instant::now() + shutdown_grace(peer_wait, config.shutdown_timeout);
                let deadline = match phase {
                    ConnectionPhase::Draining => idle.deadline().min(deadline),
                    ConnectionPhase::Handshake | ConnectionPhase::Open | ConnectionPhase::Stopping => {
                        connection.as_mut().graceful_shutdown();
                        deadline
                    }
                };
                phase = ConnectionPhase::Stopping;
                idle.as_mut().reset(deadline);
            }
            Ok(()) = progress.changed(), if matches!(phase, ConnectionPhase::Handshake | ConnectionPhase::Open) => {
                phase = ConnectionPhase::Open;
                let activity = *progress.borrow_and_update();
                let limit = match activity.streams {
                    0 => config.keep_alive_timeout,
                    _ => config.body_inactivity_timeout,
                };
                idle.as_mut().reset(limit.deadline_from(activity.last_frame));
            }
            _ = reads.arrived.notified(), if phase == ConnectionPhase::Handshake => {
                if reads.total() >= HTTP2_PREFACE_BYTES {
                    phase = ConnectionPhase::Open;
                    idle.as_mut().reset(config.keep_alive_timeout.deadline());
                }
            }
            _ = &mut idle => {
                match phase {
                    ConnectionPhase::Draining | ConnectionPhase::Stopping => {
                        crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "grpc_drain_timeout", serde_json::json!({"message":"gRPC GOAWAY grace expired with an incomplete handshake or unfinished stream; finish client streams or raise header_read_timeout_ms, for example 60000; for SIGTERM also raise shutdown_timeout_ms, for example 20000","shutdown":phase == ConnectionPhase::Stopping}));
                        break;
                    }
                    ConnectionPhase::Handshake => {
                        crate::diagnostics::emit(crate::diagnostics::LogLevel::Info, "grpc_handshake_timeout", serde_json::json!({"message":"gRPC client sent no HTTP/2 preface before header_read_timeout_ms; sending GOAWAY; use a plaintext h2c client or raise header_read_timeout_ms, for example 60000"}));
                    }
                    ConnectionPhase::Open => {
                        let streams = progress.borrow().streams;
                        match streams {
                            0 => crate::diagnostics::emit(crate::diagnostics::LogLevel::Debug, "grpc_idle_timeout", serde_json::json!({"message":"gRPC connection idle deadline exceeded; sending GOAWAY; reconnect or raise keep_alive_timeout_ms, for example 300000"})),
                            _ => crate::diagnostics::emit(crate::diagnostics::LogLevel::Info, "grpc_stream_inactivity_timeout", serde_json::json!({"message":"gRPC stream remained open without request or response frames for body_inactivity_timeout_ms; sending GOAWAY; send periodically, close and reopen the stream, or raise the setting (0 disables it)"})),
                        }
                    }
                }
                connection.as_mut().graceful_shutdown();
                phase = ConnectionPhase::Draining;
                idle.as_mut().reset(tokio::time::Instant::now() + peer_wait);
            }
        }
    }
}

#[cfg(test)]
mod tests;
