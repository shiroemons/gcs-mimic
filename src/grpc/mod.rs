//! Storage transports share domain state, never JSON wire representations.
pub mod v2;

// Generated upstream code retains its own field names and documentation.
#[allow(clippy::all, rustdoc::broken_intra_doc_links)]
pub mod generated {
    include!("generated/mod.rs");
}

use crate::{config::Config, server::State};
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

pub(crate) async fn serve_connection(
    stream: TcpStream,
    config: &Config,
    state: Arc<State>,
    stopping: watch::Receiver<bool>,
) {
    let service = v2::service(state);
    serve_with_service(stream, config, stopping, move |request| {
        let mut service = service.clone();
        async move {
            if !v2::known_method(request.uri().path()) {
                return Ok(v2::status::unsupported("unknown service or method").into_http());
            }
            service.call(request).await
        }
    })
    .await;
}

struct StreamGuard(watch::Sender<usize>);

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.0.send_modify(|count| *count -= 1);
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
    service: S,
) where
    S: Fn(hyper::Request<Incoming>) -> F + Send + Sync + 'static,
    F: Future<Output = Result<hyper::Response<tonic::body::Body>, Infallible>> + Send + 'static,
{
    let (active, mut progress) = watch::channel(0usize);
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
            active.send_modify(|count| *count += 1);
            let guard = StreamGuard(active.clone());
            let response = service(request);
            async move {
                let response = response.await?;
                Ok::<_, Infallible>(response.map(|body| ResponseBody {
                    body,
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
            Ok(()) = progress.changed(), if !draining => idle.as_mut().reset(tokio::time::Instant::now() + timeout),
            _ = &mut idle, if draining || *progress.borrow() == 0 => {
                if draining {
                    crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "grpc_drain_timeout", serde_json::json!({"message":"gRPC GOAWAY grace expired with an incomplete handshake or unfinished stream; finish client streams or raise connection_timeout_ms, for example 10000; for SIGTERM also raise shutdown_timeout_ms, for example 20000","shutdown":shutting_down}));
                    break;
                }
                crate::diagnostics::emit(crate::diagnostics::LogLevel::Debug, "grpc_idle_timeout", serde_json::json!({"message":"gRPC connection idle deadline exceeded; sending GOAWAY; reconnect or raise connection_timeout_ms, for example 10000"}));
                connection.as_mut().graceful_shutdown();
                draining = true;
                idle.as_mut().reset(tokio::time::Instant::now() + timeout);
            }
        }
    }
}

#[cfg(test)]
#[path = "../../tests/grpc/connection.rs"]
mod tests;
