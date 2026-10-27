//! Storage transports share domain state, never JSON wire representations.
pub mod v2;

// Generated upstream code retains its own field names and documentation.
#[allow(clippy::all, rustdoc::broken_intra_doc_links)]
pub mod generated {
    include!("generated/mod.rs");
}

use crate::{config::Config, server::State};
use http_body_util::BodyExt;
use hyper::{body::Incoming, server::conn::http2, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::{net::TcpStream, sync::watch};
use tonic::codegen::Service;

pub(crate) async fn serve_connection(
    stream: TcpStream,
    config: &Config,
    state: Arc<State>,
    mut stopping: watch::Receiver<bool>,
) {
    let service = v2::service(state);
    let (activity, mut progress) = watch::channel(());
    let mut builder = http2::Builder::new(TokioExecutor::new());
    builder
        .max_concurrent_streams(8)
        .max_header_list_size(16 * 1024)
        .max_send_buf_size(64 * 1024);
    let connection = builder.serve_connection(
        TokioIo::new(stream),
        service_fn(move |request: hyper::Request<Incoming>| {
            activity.send_replace(());
            let incoming_activity = activity.clone();
            let outgoing_activity = activity.clone();
            let request = request.map(|body| {
                body.map_frame(move |frame| {
                    incoming_activity.send_replace(());
                    frame
                })
            });
            let mut service = service.clone();
            async move {
                if !v2::known_method(request.uri().path()) {
                    return Ok::<_, Infallible>(
                        v2::status::unsupported("unknown service or method").into_http(),
                    );
                }
                let response = service.call(request).await?;
                Ok(response.map(|body| {
                    tonic::body::Body::new(body.map_frame(move |frame| {
                        outgoing_activity.send_replace(());
                        frame
                    }))
                }))
            }
        }),
    );
    tokio::pin!(connection);
    let timeout = Duration::from_millis(config.connection_timeout_ms);
    let idle = tokio::time::sleep(timeout);
    tokio::pin!(idle);
    let mut draining = false;
    loop {
        tokio::select! {
            result = &mut connection => {
                if result.is_err() {
                    crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "grpc_connection_failed", serde_json::json!({"message":"gRPC connection failed; use a plaintext h2c client and grpc_listen, for example 127.0.0.1:4444"}));
                }
                break;
            }
            Ok(()) = progress.changed() => idle.as_mut().reset(tokio::time::Instant::now() + timeout),
            _ = &mut idle => {
                crate::diagnostics::emit(crate::diagnostics::LogLevel::Debug, "grpc_idle_timeout", serde_json::json!({"message":"gRPC connection idle deadline exceeded; reconnect or raise connection_timeout_ms, for example 10000"}));
                break;
            }
            _ = stopping.changed(), if !draining => {
                connection.as_mut().graceful_shutdown();
                draining = true;
            }
        }
    }
}
