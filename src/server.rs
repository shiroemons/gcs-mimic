use crate::{
    bucket::CatalogError,
    clock::Clock,
    config::Config,
    json::{self, BucketRequest, JsonError},
    storage::Catalog,
};
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
    pub catalog: Catalog,
    pub api_base_url: String,
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
                if let Err(error) = result.expect("nonempty task set") {
                    break Err(io::Error::other(error));
                }
            }
            incoming = listener.accept(), if connections.len() < config.max_connections => {
                let (stream, _) = match incoming {
                    Ok(incoming) => incoming,
                    Err(error) => break Err(error),
                };
                let state = state.clone();
                let mut stopping = stop.subscribe();
                let timeout = Duration::from_millis(config.connection_timeout_ms);
                connections.spawn(async move {
                    let mut builder = http1::Builder::new();
                    builder.keep_alive(false).max_headers(64).max_buf_size(16 * 1024)
                        .timer(TokioTimer::new()).header_read_timeout(timeout);
                    let connection = builder.serve_connection(TokioIo::new(stream), service_fn(move |request| {
                        let state = state.clone();
                        async move { Ok::<_, Infallible>(respond(request, &state).await) }
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
    let drain_result = match tokio::time::timeout(
        Duration::from_millis(config.shutdown_timeout_ms),
        drain,
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "shutdown drain deadline exceeded",
        )),
    };
    if drain_result.is_err() {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    state.catalog.close().await.map_err(io::Error::other)?;
    signal_result.and(drain_result)
}

async fn respond(request: Request<Incoming>, state: &State) -> Response<Full<Bytes>> {
    if let Some(parsed) = json::route(request.method(), request.uri()) {
        let result = match parsed {
            Ok(operation) => bucket_response(request, operation, state).await,
            Err(error) => Err(error),
        };
        let mut response = result.unwrap_or_else(JsonError::into_response);
        response
            .headers_mut()
            .insert("connection", "close".parse().unwrap());
        return response;
    }
    probe_or_unknown(&request, state)
}

async fn bucket_response(
    request: Request<Incoming>,
    operation: BucketRequest,
    state: &State,
) -> Result<Response<Full<Bytes>>, JsonError> {
    if !matches!(operation, BucketRequest::Insert(_))
        && (request.headers().contains_key("transfer-encoding")
            || request
                .headers()
                .get("content-length")
                .is_some_and(|v| v != "0"))
    {
        return Err(JsonError::invalid_argument(
            "This method requires an empty body",
        ));
    }
    let value = match operation {
        BucketRequest::Insert(params) => {
            let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
            let record = json::parse_insert(&bytes, &params.project, state.clock.now())?;
            let record = state.catalog.insert_bucket(record).await?;
            json::bucket_resource(&record, &state.api_base_url, params.projection)?
        }
        BucketRequest::Get { bucket, params } => {
            let record = state
                .catalog
                .get_bucket(&bucket)
                .await?
                .ok_or(CatalogError::NotFound)?;
            let conditions = params.conditions;
            if conditions
                .if_metageneration_match
                .is_some_and(|v| v != record.metageneration)
            {
                return Err(CatalogError::PreconditionFailed.into());
            }
            if conditions.if_metageneration_not_match == Some(record.metageneration) {
                return Ok(empty_response(StatusCode::NOT_MODIFIED));
            }
            json::bucket_resource(&record, &state.api_base_url, params.projection)?
        }
        BucketRequest::List(params) => {
            let limit = params.max_results as usize;
            let mut records = state
                .catalog
                .list_buckets(
                    &params.project,
                    Some(&params.prefix),
                    params.start_after.as_deref(),
                    limit + 1,
                )
                .await?;
            let token = if records.len() > limit {
                records.truncate(limit);
                Some(json::encode_page_token(
                    &params.project,
                    &params.prefix,
                    &records[limit - 1].name,
                )?)
            } else {
                None
            };
            json::bucket_list(
                &records,
                token.as_deref(),
                &state.api_base_url,
                params.projection,
            )?
        }
        BucketRequest::Delete { bucket, conditions } => {
            state
                .catalog
                .delete_bucket(
                    &bucket,
                    conditions.if_metageneration_match,
                    conditions.if_metageneration_not_match,
                )
                .await?;
            return Ok(empty_response(StatusCode::NO_CONTENT));
        }
    };
    Ok(Response::builder()
        .header("content-type", "application/json; charset=UTF-8")
        .body(Full::new(Bytes::from(value.to_string())))
        .unwrap())
}

fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .unwrap()
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
