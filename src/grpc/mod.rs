//! Storage transports share domain state, never JSON wire representations.
pub mod v1;
pub mod v2;

// Generated upstream code retains its own field names and documentation.
#[allow(clippy::all, rustdoc::broken_intra_doc_links)]
pub mod generated {
    include!("generated/mod.rs");
}

use crate::{config::Config, server::State};
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

const STREAM_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(60);
const UPLOAD_CHUNK_BYTES: usize = 64 * 1024;

/// Splits decoded message data into the upload channel's 64 KiB chunks without copying.
/// Queued chunks share the decoded message, so the 4 MiB decode limit and the bounded
/// upload channel still bound the memory a stream retains.
pub(crate) fn upload_chunks(data: &bytes::Bytes) -> impl Iterator<Item = bytes::Bytes> + '_ {
    (0..data.len())
        .step_by(UPLOAD_CHUNK_BYTES)
        .map(|start| data.slice(start..data.len().min(start + UPLOAD_CHUNK_BYTES)))
}

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
    let service = v2::service(state, config.max_object_bytes);
    serve_with_service(
        stream,
        config,
        stopping,
        STREAM_INACTIVITY_TIMEOUT,
        move |request| {
            let mut service = service.clone();
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
                if !v2::known_method(request.uri().path()) {
                    return Ok(v2::status::unsupported("unknown service or method").into_http());
                }
                service.call(request).await
            }
        },
    )
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

async fn serve_with_service<S, F>(
    stream: TcpStream,
    config: &Config,
    mut stopping: watch::Receiver<bool>,
    stream_timeout: Duration,
    service: S,
) where
    S: Fn(hyper::Request<tonic::body::Body>) -> F + Send + Sync + 'static,
    F: Future<Output = Result<hyper::Response<tonic::body::Body>, Infallible>> + Send + 'static,
{
    let (active, mut progress) = watch::channel(Activity {
        streams: 0,
        last_frame: tokio::time::Instant::now(),
    });
    let timeout = Duration::from_millis(config.connection_timeout_ms);
    let mut builder = http2::Builder::new(TokioExecutor::new());
    builder
        .max_concurrent_streams(8)
        .max_header_list_size(16 * 1024)
        .max_send_buf_size(64 * 1024)
        .timer(TokioTimer::new())
        .keep_alive_interval(timeout)
        .keep_alive_timeout(timeout);
    let connection = builder.serve_connection(
        TokioIo::new(stream),
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
    let idle = tokio::time::sleep(timeout);
    tokio::pin!(idle);
    let mut draining = false;
    let mut shutting_down = false;
    loop {
        tokio::select! {
            biased;
            result = &mut connection => {
                if let Err(error) = result {
                    let (event, message) = if error.is_timeout() {
                        ("grpc_keepalive_timeout", "gRPC peer did not acknowledge an HTTP/2 keepalive ping; check the client connection or raise connection_timeout_ms, for example 10000")
                    } else {
                        ("grpc_connection_failed", "gRPC connection failed; use a plaintext h2c client and grpc_listen, for example 127.0.0.1:4444")
                    };
                    crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, event, serde_json::json!({"message":message,"cause":error.to_string()}));
                }
                break;
            }
            _ = stopping.changed(), if !shutting_down => {
                // Reserve half the outer drain budget for connection cleanup and joining.
                let grace = timeout.min(Duration::from_millis(config.shutdown_timeout_ms) / 2);
                let deadline = tokio::time::Instant::now() + grace;
                let deadline = if draining {
                    idle.deadline().min(deadline)
                } else {
                    connection.as_mut().graceful_shutdown();
                    deadline
                };
                draining = true;
                shutting_down = true;
                idle.as_mut().reset(deadline);
            }
            Ok(()) = progress.changed(), if !draining => {
                let activity = *progress.borrow_and_update();
                let bound = if activity.streams == 0 { timeout } else { stream_timeout };
                idle.as_mut().reset(activity.last_frame + bound);
            }
            _ = &mut idle => {
                if draining {
                    crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "grpc_drain_timeout", serde_json::json!({"message":"gRPC GOAWAY grace expired with an incomplete handshake or unfinished stream; finish client streams or raise connection_timeout_ms, for example 10000; for SIGTERM also raise shutdown_timeout_ms, for example 20000","shutdown":shutting_down}));
                    break;
                }
                if progress.borrow().streams == 0 {
                    crate::diagnostics::emit(crate::diagnostics::LogLevel::Debug, "grpc_idle_timeout", serde_json::json!({"message":"gRPC connection idle deadline exceeded; sending GOAWAY; reconnect or raise connection_timeout_ms, for example 10000"}));
                } else {
                    crate::diagnostics::emit(crate::diagnostics::LogLevel::Info, "grpc_stream_inactivity_timeout", serde_json::json!({"message":"gRPC stream remained open without request or response frames for 60 seconds; sending GOAWAY; send periodically or close and reopen the stream"}));
                }
                connection.as_mut().graceful_shutdown();
                draining = true;
                idle.as_mut().reset(tokio::time::Instant::now() + timeout);
            }
        }
    }
}

#[cfg(test)]
mod tests;
