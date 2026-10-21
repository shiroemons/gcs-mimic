use crate::{
    bucket::CatalogError,
    clock::Clock,
    config::Config,
    json::{self, BucketRequest, JsonError, ObjectRequest},
    object::{ObjectError, UploadChunk},
    storage::Catalog,
};
use http_body_util::{BodyExt, Channel, Full, combinators::UnsyncBoxBody};
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
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, watch},
    task::JoinSet,
};

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
                let max_object_bytes = config.max_object_bytes;
                connections.spawn(async move {
                    let (activity, mut progress) = watch::channel(false);
                    let mut builder = http1::Builder::new();
                    builder.keep_alive(false).max_headers(64).max_buf_size(16 * 1024)
                        .timer(TokioTimer::new()).header_read_timeout(timeout);
                    let mut connection = builder.serve_connection(TokioIo::new(stream), service_fn(move |request| {
                        let state = state.clone();
                        let activity = activity.clone();
                        Box::pin(async move { Ok::<_, Infallible>(respond(request, &state, max_object_bytes, &activity).await) })
                    }));
                    let deadline = tokio::time::sleep(timeout);
                    tokio::pin!(deadline);
                    let mut awaiting_catalog = false;
                    loop { tokio::select! {
                        biased;
                        Ok(()) = progress.changed() => {
                            awaiting_catalog = *progress.borrow_and_update();
                            deadline.as_mut().reset(tokio::time::Instant::now() + timeout);
                        },
                        result = std::future::poll_fn(|cx| connection.poll_without_shutdown(cx)) => {
                            if let Err(error) = result { eprintln!("connection error: {error}"); }
                            break;
                        }
                        _ = &mut deadline, if !awaiting_catalog => { eprintln!("connection deadline or object body inactivity timeout exceeded"); break; },
                        _ = stopping.changed() => {
                            std::pin::Pin::new(&mut connection).graceful_shutdown();
                            if let Err(error) = std::future::poll_fn(|cx| connection.poll_without_shutdown(cx)).await { eprintln!("connection drain error: {error}"); }
                            break;
                        }
                    }}
                    // Send FIN after the response, then bound unread-body disposal to avoid a reset
                    // hiding an early rejection from a client still sending a large request.
                    let mut stream = connection.into_parts().io.into_inner();
                    let _ = stream.shutdown().await;
                    let _ = tokio::time::timeout(Duration::from_millis(100),
                        tokio::io::copy(&mut stream.take(256 * 1024), &mut tokio::io::sink())).await;
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

type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;

fn boxed(response: Response<Full<Bytes>>) -> Response<ResponseBody> {
    response.map(|body| body.map_err(|never| match never {}).boxed_unsync())
}

async fn respond(
    request: Request<Incoming>,
    state: &State,
    max_object_bytes: u64,
    activity: &watch::Sender<bool>,
) -> Response<ResponseBody> {
    if let Some(parsed) = json::route_object(request.method(), request.uri()) {
        activity.send_replace(false);
        let result = match parsed {
            Ok(operation) => {
                object_response(request, operation, state, max_object_bytes, activity).await
            }
            Err(error) => Err(error),
        };
        let mut response = result.unwrap_or_else(|error| boxed(error.into_response()));
        activity.send_replace(false);
        response
            .headers_mut()
            .insert("connection", "close".parse().unwrap());
        return response;
    }
    if let Some(parsed) = json::route(request.method(), request.uri()) {
        let result = match parsed {
            Ok(operation) => bucket_response(request, operation, state).await,
            Err(error) => Err(error),
        };
        let mut response = result.unwrap_or_else(JsonError::into_response);
        response
            .headers_mut()
            .insert("connection", "close".parse().unwrap());
        return boxed(response);
    }
    boxed(probe_or_unknown(&request, state))
}

async fn object_response(
    request: Request<Incoming>,
    operation: ObjectRequest,
    state: &State,
    max_bytes: u64,
    activity: &watch::Sender<bool>,
) -> Result<Response<ResponseBody>, JsonError> {
    if !matches!(operation, ObjectRequest::Insert { .. })
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
        ObjectRequest::Insert {
            bucket,
            name,
            params,
        } => {
            let (content_type, expected) = json::parse_object_upload_headers(request.headers())?;
            if request
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .is_some_and(|length| length > max_bytes)
            {
                return Err(ObjectError::TooLarge.into());
            }
            // Complete the advisory check before body-derived errors can race its result.
            activity.send_replace(true);
            let prepared = state
                .catalog
                .prepare_object_upload(&bucket, &name, params.conditions)
                .await;
            activity.send_replace(false);
            let permit = prepared?;
            let (sender, body) = mpsc::channel(2);
            let upload = state.catalog.put_object_prechecked(
                &bucket,
                &name,
                content_type,
                expected,
                params.conditions,
                max_bytes,
                body,
                permit,
            );
            tokio::pin!(upload);
            let record = tokio::select! {
                result = &mut upload => result?,
                result = stream_upload(request.into_body(), &sender, max_bytes, activity) => {
                    if let Err(error) = result {
                        if sender.send(Err(io::Error::other(error.to_string()))).await.is_err() {
                            return Err(upload.await.err().unwrap_or(error).into());
                        }
                        return Err(error.into());
                    }
                    drop(sender);
                    upload.await?
                }
            };
            json::object_resource(&record, &state.api_base_url, params.projection)?
        }
        ObjectRequest::Get {
            bucket,
            name,
            params,
        } => {
            json::validate_object_get_headers(request.headers(), params.media)?;
            if params.media {
                let (record, mut reader) = state
                    .catalog
                    .open_object(&bucket, &name, params.generation, params.conditions)
                    .await?;
                let headers = json::object_media_headers(&record)?;
                let (mut sender, body) = Channel::<Bytes, io::Error>::new(1);
                let activity = activity.clone();
                tokio::spawn(async move {
                    loop {
                        match reader.next_chunk().await {
                            Ok(Some(bytes)) => {
                                if sender.send_data(bytes).await.is_err() {
                                    break;
                                }
                                activity.send_replace(false);
                            }
                            Ok(None) => break,
                            Err(error) => {
                                sender.abort(error);
                                break;
                            }
                        }
                    }
                });
                let mut response = Response::new(body.boxed_unsync());
                *response.headers_mut() = headers;
                return Ok(response);
            }
            let record = state
                .catalog
                .get_object(&bucket, &name, params.generation, params.conditions)
                .await?;
            json::object_resource(&record, &state.api_base_url, params.projection)?
        }
        ObjectRequest::Delete {
            bucket,
            name,
            generation,
            conditions,
        } => {
            json::validate_object_get_headers(request.headers(), false)?;
            state
                .catalog
                .delete_object(&bucket, &name, generation, conditions)
                .await?;
            return Ok(boxed(empty_response(StatusCode::NO_CONTENT)));
        }
    };
    Ok(boxed(
        Response::builder()
            .header("content-type", "application/json; charset=UTF-8")
            .body(Full::new(Bytes::from(value.to_string())))
            .unwrap(),
    ))
}

async fn stream_upload(
    mut body: Incoming,
    sender: &mpsc::Sender<Result<UploadChunk, io::Error>>,
    max_bytes: u64,
    activity: &watch::Sender<bool>,
) -> Result<(), ObjectError> {
    let mut size = 0_u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| ObjectError::BodyAborted(error.to_string()))?;
        let bytes = frame
            .into_data()
            .map_err(|_| ObjectError::BodyAborted("Upload trailers are unsupported".into()))?;
        size = size
            .checked_add(bytes.len() as u64)
            .filter(|size| *size <= max_bytes)
            .ok_or(ObjectError::TooLarge)?;
        activity.send_replace(false);
        // Copy bounded chunks so queued slices cannot retain arbitrarily large transport frames.
        for part in bytes.chunks(64 * 1024) {
            if sender
                .send(Ok(UploadChunk::Data(Bytes::copy_from_slice(part))))
                .await
                .is_err()
            {
                // Preserve the catalog's original failure when it stops accepting the body.
                return Ok(());
            }
        }
    }
    // Body inactivity no longer applies while the completed body is being committed.
    activity.send_replace(true);
    let _ = sender.send(Ok(UploadChunk::End)).await;
    Ok(())
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
