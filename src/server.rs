use crate::{clock::Clock, config::Config};
use http_body_util::Full;
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    convert::Infallible,
    future::Future,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};
use tokio::{net::TcpListener, sync::watch, task::JoinSet};

pub struct State {
    pub ready: AtomicBool,
    pub clock: Arc<dyn Clock>,
}

pub async fn serve(
    listener: TcpListener,
    config: Config,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let (stop, _) = watch::channel(false);
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    let signal_result = loop {
        tokio::select! {
            result = &mut shutdown => break result,
            result = connections.join_next(), if !connections.is_empty() => {
                result.expect("nonempty task set").map_err(io::Error::other)?;
            }
            incoming = listener.accept(), if connections.len() < config.max_connections => {
                let (stream, _) = incoming?;
                let state = state.clone();
                let mut stopping = stop.subscribe();
                let timeout = Duration::from_millis(config.connection_timeout_ms);
                connections.spawn(async move {
                    let mut builder = http1::Builder::new();
                    builder.keep_alive(false).max_headers(64).max_buf_size(16 * 1024)
                        .timer(TokioTimer::new()).header_read_timeout(timeout);
                    let connection = builder.serve_connection(TokioIo::new(stream), service_fn(move |request| {
                        let state = state.clone();
                        async move { Ok::<_, Infallible>(respond(&request, &state)) }
                    }));
                    tokio::pin!(connection);
                    tokio::select! {
                        result = &mut connection => {
                            if let Err(error) = result { eprintln!("connection error: {error}"); }
                        }
                        _ = tokio::time::sleep(timeout) => eprintln!("connection deadline exceeded"),
                        _ = stopping.changed() => {
                            connection.as_mut().graceful_shutdown();
                            if let Err(error) = connection.await { eprintln!("connection drain error: {error}"); }
                        }
                    }
                });
            }
        }
    };
    state.ready.store(false, Ordering::Release);
    drop(listener);
    stop.send_replace(true);
    let drain = async {
        while let Some(result) = connections.join_next().await {
            result.map_err(io::Error::other)?;
        }
        Ok::<_, io::Error>(())
    };
    match tokio::time::timeout(Duration::from_millis(config.shutdown_timeout_ms), drain).await {
        Ok(result) => result?,
        Err(_) => {
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "shutdown drain deadline exceeded",
            ));
        }
    }
    signal_result
}

fn respond(request: &Request<Incoming>, state: &State) -> Response<Full<Bytes>> {
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
    } else if path == "/_internal/ready" && !state.ready.load(Ordering::Acquire) {
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
    let mut response = Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("content-length", body.len())
        .header("connection", "close");
    if status == StatusCode::METHOD_NOT_ALLOWED {
        response = response.header("allow", "GET, HEAD");
    }
    response
        .body(Full::new(if request.method() == Method::HEAD {
            Bytes::new()
        } else {
            Bytes::from(body)
        }))
        .unwrap()
}
