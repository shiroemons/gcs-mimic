use crate::{
    auth::{self, Auth, Caller},
    bucket::{BucketAccess, CatalogError},
    clock::Clock,
    config::{CompatProfile, Config},
    http_conditions::{EtagError, etag_not_modified},
    json::{self, BucketRequest, JsonError, ObjectRequest},
    object::{ObjectConditions, ObjectError, UploadChunk},
    storage::Catalog,
    tls, xml,
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

#[cfg(test)]
tokio::task_local! { static FORCE_LISTENER_ERROR: bool; }

#[path = "server/copy.rs"]
mod copy;
#[path = "server/resumable.rs"]
mod resumable;
#[path = "server/rewrite.rs"]
mod rewrite;
mod unsupported;

#[derive(Clone, Copy, Default)]
pub(crate) struct ObjectActivity {
    awaiting_catalog: bool,
    deadline: Option<tokio::time::Instant>,
}

pub async fn serve(
    listener: TcpListener,
    config: Config,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let acceptor = tls::load_server(&config)?;
    serve_with_tls(vec![(listener, acceptor)], config, state, shutdown).await
}

pub async fn serve_with_tls(
    listeners: Vec<(TcpListener, Option<tokio_rustls::TlsAcceptor>)>,
    config: Config,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    serve_with_tls_and_drain(listeners, config, state, shutdown, async { Ok(()) }).await
}

pub async fn serve_with_tls_and_drain(
    listeners: Vec<(TcpListener, Option<tokio_rustls::TlsAcceptor>)>,
    config: Config,
    state: Arc<State>,
    shutdown: impl Future<Output = io::Result<()>>,
    background_drain: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    if listeners.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "at least one listener is required",
        ));
    }
    let (stop, _) = watch::channel(false);
    let mut connections = JoinSet::new();
    let mut next_listener = 0;
    tokio::pin!(shutdown);
    let signal_result = loop {
        tokio::select! {
            result = &mut shutdown => break result,
            result = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = result.expect("nonempty task set") {
                    break Err(io::Error::other(error));
                }
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
                let config = config.clone();
                let acceptor = listeners[index].1.clone();
                connections.spawn(async move {
                    if let Some(acceptor) = acceptor {
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
                        serve_connection(stream, state, config, stopping).await;
                    } else {
                        serve_connection(stream, state, config, stopping).await;
                    }
                });
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
    let background_result = background_drain.await;
    crate::diagnostics::combine_shutdown_results(
        crate::diagnostics::combine_shutdown_results(
            signal_result.and(drain_result),
            background_result,
        ),
        state.catalog.close().await.map_err(io::Error::other),
    )
}

#[cfg(test)]
#[path = "server/drain_tests.rs"]
mod drain_tests;

async fn serve_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    state: Arc<State>,
    config: Config,
    mut stopping: watch::Receiver<bool>,
) {
    let timeout = Duration::from_millis(config.connection_timeout_ms);
    let (activity, mut progress) = watch::channel(ObjectActivity::default());
    let mut builder = http1::Builder::new();
    builder
        .keep_alive(false)
        .max_headers(64)
        .max_buf_size(16 * 1024)
        .timer(TokioTimer::new())
        .header_read_timeout(timeout);
    let mut connection = builder.serve_connection(
        TokioIo::new(stream),
        service_fn(move |request| {
            let state = state.clone();
            let activity = activity.clone();
            let config = config.clone();
            Box::pin(async move {
                Ok::<_, Infallible>(respond(request, &state, &config, &activity).await)
            })
        }),
    );
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut current = ObjectActivity::default();
    let mut draining = false;
    loop {
        tokio::select! {
            biased;
            Ok(()) = progress.changed() => {
                current = *progress.borrow_and_update();
                deadline.as_mut().reset(tokio::time::Instant::now() + timeout);
            },
            _ = tokio::time::sleep_until(current.deadline.unwrap_or_else(tokio::time::Instant::now)), if current.deadline.is_some() && !current.awaiting_catalog => {
                crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "object_request_timeout", serde_json::json!({"message":"object request deadline exceeded"}));
                break;
            },
            result = std::future::poll_fn(|cx| connection.poll_without_shutdown(cx)) => {
                // hyper's Display names only the failure kind (body stream, write, parse), never the wrapped I/O detail.
                if let Err(error) = result { crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "connection_failed", serde_json::json!({"message":"connection failed; check client request framing and transport","cause":error.to_string()})); }
                break;
            }
            _ = &mut deadline, if !current.awaiting_catalog => { crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "connection_timeout", serde_json::json!({"message":"connection deadline or object body inactivity timeout exceeded"})); break; },
            _ = stopping.changed(), if !draining => {
                std::pin::Pin::new(&mut connection).graceful_shutdown();
                draining = true;
            }
        }
    }
    // Send FIN after the response, then bound unread-body disposal to avoid a reset
    // hiding an early rejection from a client still sending a large request.
    let mut stream = connection.into_parts().io.into_inner();
    let _ = tokio::time::timeout(timeout, stream.shutdown()).await;
    let _ = tokio::time::timeout(
        Duration::from_millis(100),
        tokio::io::copy(&mut stream.take(256 * 1024), &mut tokio::io::sink()),
    )
    .await;
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Debug,
        "connection_closed",
        serde_json::json!({}),
    );
}

pub(crate) type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;

pub(crate) fn boxed(response: Response<Full<Bytes>>) -> Response<ResponseBody> {
    response.map(|body| body.map_err(|never| match never {}).boxed_unsync())
}

async fn respond(
    request: Request<Incoming>,
    state: &State,
    config: &Config,
    activity: &watch::Sender<ObjectActivity>,
) -> Response<ResponseBody> {
    let selected = match xml::select_object(request.uri(), request.headers(), config) {
        Ok(selected) => selected,
        Err(error) => return boxed(error.response(request.method() == Method::HEAD)),
    };
    if selected.is_none() && json::cors::is_route(request.uri()) {
        let preflight = json::cors::is_preflight(request.method(), request.headers());
        let mut headers = match json::cors::response_headers(request.headers(), preflight) {
            Ok(headers) => headers,
            Err(error) => return boxed(error.into_response()),
        };
        let mut response = if preflight {
            let status = json::cors::preflight_status(request.headers());
            if status == StatusCode::NOT_FOUND {
                headers.insert("access-control-allow-credentials", "true".parse().unwrap());
                boxed(
                    JsonError::new(
                        StatusCode::NOT_FOUND,
                        "notFound",
                        "Missing CORS request method",
                    )
                    .into_response(),
                )
            } else {
                boxed(Response::new(Full::new(Bytes::new())))
            }
        } else {
            respond_routed(request, state, config, activity, selected).await
        };
        response.headers_mut().extend(headers);
        response
            .headers_mut()
            .insert("connection", "close".parse().unwrap());
        return response;
    }
    // CORS must also wrap invalid object names and signed queries without authorizing them.
    if let Some(object) = selected.as_ref().filter(|object| {
        !object.name().is_empty()
            && json::validate_bucket_name_for_profile(&object.bucket, config.compat_profile).is_ok()
    }) {
        let has_origin = request.headers().contains_key("origin");
        let preflight = request.method() == Method::OPTIONS && has_origin;
        let cors = if has_origin {
            match state.catalog.get_bucket(&object.bucket).await {
                Ok(Some(bucket)) => {
                    xml::cors(request.headers(), request.method(), &bucket.cors, preflight)
                }
                Ok(None) => hyper::HeaderMap::new(),
                Err(error) => {
                    return boxed(
                        xml::Error::from(JsonError::from(error))
                            .response(request.method() == Method::HEAD),
                    );
                }
            }
        } else {
            hyper::HeaderMap::new()
        };
        let mut response = if preflight {
            boxed(Response::new(Full::new(Bytes::new())))
        } else {
            respond_routed(request, state, config, activity, selected).await
        };
        response.headers_mut().extend(cors);
        if !response.headers().contains_key("vary") {
            response.headers_mut().insert(
                "vary",
                if preflight {
                    "Origin, Access-Control-Request-Method, Access-Control-Request-Headers"
                } else {
                    "Origin"
                }
                .parse()
                .unwrap(),
            );
        }
        response
            .headers_mut()
            .insert("connection", "close".parse().unwrap());
        return response;
    }
    respond_routed(request, state, config, activity, selected).await
}

async fn respond_routed(
    request: Request<Incoming>,
    state: &State,
    config: &Config,
    activity: &watch::Sender<ObjectActivity>,
    selected: Option<xml::ObjectPath>,
) -> Response<ResponseBody> {
    // Preflight and CORS metadata do not authorize access; actual API dispatch authenticates
    // before operation validation, body reads and authorized catalog operations.
    let authenticate = || {
        state
            .auth
            .authenticate(request.headers(), request.uri().query())
            .map_err(JsonError::from)
    };
    if selected.is_none()
        && let Some(route) = unsupported::route(request.method(), request.uri())
    {
        return boxed(
            authenticate()
                .and_then(|_| {
                    Err(JsonError::new(
                        StatusCode::BAD_REQUEST,
                        "badRequest",
                        format!("gcs-mimic does not support Google JSON API route {route}; remove this API call and use a supported operation (for example, GET /storage/v1/b/BUCKET/o)."),
                    ))
                })
                .unwrap_or_else(JsonError::into_response),
        );
    }
    if let Some(parsed) = xml::route(request.method(), config, selected) {
        let head = request.method() == Method::HEAD;
        let put = request.method() == Method::PUT;
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(config.object_request_timeout_ms);
        activity.send_replace(ObjectActivity {
            awaiting_catalog: false,
            deadline: Some(deadline),
        });
        let result = match xml::authenticate(
            &state.auth,
            request.method(),
            request.uri(),
            request.headers(),
            state.clock.as_ref(),
        ) {
            Ok(caller) => match parsed {
                Ok(operation) if put => {
                    xml::put::respond(
                        request, operation, caller, state, config, activity, deadline,
                    )
                    .await
                }
                Ok(operation) => xml_response(request, operation, caller, state, activity).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        if head || put || result.is_err() {
            activity.send_replace(ObjectActivity::default());
        }
        let mut response = result.unwrap_or_else(|error| boxed(error.response(head)));
        response
            .headers_mut()
            .insert("connection", "close".parse().unwrap());
        return response;
    }
    if let Some(parsed) =
        json::resumable::session_route(request.method(), request.uri(), config.compat_profile)
    {
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(config.object_request_timeout_ms);
        activity.send_replace(ObjectActivity {
            awaiting_catalog: false,
            deadline: Some(deadline),
        });
        let result = match parsed {
            Ok((bucket, id)) => {
                resumable::session_response(
                    request, &bucket, &id, state, config, activity, deadline,
                )
                .await
            }
            Err(error) => Err(error),
        };
        activity.send_replace(ObjectActivity::default());
        let mut response = result.unwrap_or_else(|error| boxed(error.into_response()));
        response
            .headers_mut()
            .insert("connection", "close".parse().unwrap());
        return response;
    }
    if let Some(parsed) =
        json::rewrite::route(request.method(), request.uri(), config.compat_profile)
    {
        let parsed = authenticate().and_then(|caller| parsed.map(|operation| (operation, caller)));
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(config.object_request_timeout_ms);
        activity.send_replace(ObjectActivity {
            awaiting_catalog: false,
            deadline: Some(deadline),
        });
        let result = match parsed {
            Ok((operation, caller)) => rewrite::respond(
                request, operation, caller, state, config, activity, deadline,
            )
            .await
            .map_err(|error| anonymous_error(error, caller)),
            Err(error) => Err(error),
        };
        activity.send_replace(ObjectActivity::default());
        let mut response = result.unwrap_or_else(|error| boxed(error.into_response()));
        response
            .headers_mut()
            .insert("connection", "close".parse().unwrap());
        return response;
    }
    if let Some(parsed) =
        json::route_object_with_profile(request.method(), request.uri(), config.compat_profile)
    {
        let parsed = authenticate().and_then(|caller| parsed.map(|operation| (operation, caller)));
        let media = matches!(&parsed, Ok((ObjectRequest::Get { params, .. }, _)) if params.media);
        let transfer = media
            || matches!(
                &parsed,
                Ok((
                    ObjectRequest::Insert { .. }
                        | ObjectRequest::Copy(_)
                        | ObjectRequest::Compose { .. }
                        | ObjectRequest::Multipart { .. }
                        | ObjectRequest::Resumable { .. },
                    _
                ))
            );
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(config.object_request_timeout_ms);
        activity.send_replace(ObjectActivity {
            awaiting_catalog: false,
            deadline: transfer.then_some(deadline),
        });
        let result = match parsed {
            Ok((operation, caller)) => object_response(
                request, operation, caller, state, config, activity, deadline,
            )
            .await
            .map_err(|error| anonymous_error(error, caller)),
            Err(error) => Err(error),
        };
        if !media || result.is_err() {
            activity.send_replace(ObjectActivity::default());
        }
        let mut response = result.unwrap_or_else(|error| boxed(error.into_response()));
        response
            .headers_mut()
            .insert("connection", "close".parse().unwrap());
        return response;
    }
    if let Some(parsed) = json::route_with_profile(
        request.method(),
        request.uri(),
        config.compat_profile,
        &config.seed_project,
    ) {
        let result = match authenticate().and_then(|caller| parsed.map(|op| (op, caller))) {
            Ok((operation, caller)) => {
                bucket_response(request, operation, caller, state, config.compat_profile)
                    .await
                    .map_err(|error| anonymous_error(error, caller))
            }
            Err(error) => Err(error),
        };
        let mut response = result
            .map_err(|mut error| {
                if config.compat_profile == CompatProfile::LegacyEmulator
                    && error.status == StatusCode::BAD_REQUEST
                    && matches!(error.reason, "invalidArgument" | "parseError")
                {
                    error.reason = "invalid";
                }
                if config.compat_profile == CompatProfile::LegacyEmulator
                    && error.status == StatusCode::NOT_FOUND
                    && error.reason == "notFound"
                {
                    error.message = "Not Found".into();
                }
                error
            })
            .unwrap_or_else(JsonError::into_response);
        if config.compat_profile == CompatProfile::LegacyEmulator
            && response.status() != StatusCode::NOT_MODIFIED
        {
            let (mut parts, body) = response.into_parts();
            let mut bytes = body.collect().await.unwrap().to_bytes().to_vec();
            if !bytes.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            parts
                .headers
                .insert("content-type", "application/json".parse().unwrap());
            response = Response::from_parts(parts, Full::new(Bytes::from(bytes)));
        }
        response
            .headers_mut()
            .insert("connection", "close".parse().unwrap());
        return boxed(response);
    }
    boxed(probe_or_unknown(&request, state))
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

#[allow(
    clippy::too_many_arguments,
    reason = "Each request owns its transport, caller, state and deadline context"
)]
async fn object_response(
    request: Request<Incoming>,
    operation: ObjectRequest,
    caller: Option<Caller<'_>>,
    state: &State,
    config: &Config,
    activity: &watch::Sender<ObjectActivity>,
    deadline: tokio::time::Instant,
) -> Result<Response<ResponseBody>, JsonError> {
    let max_bytes = config.max_object_bytes;
    let fields = if matches!(operation, ObjectRequest::Delete { .. }) {
        json::Fields::from_query(None, "Object")?
    } else {
        json::Fields::from_query(
            request.uri().query(),
            if matches!(operation, ObjectRequest::List(_)) {
                "Objects"
            } else {
                "Object"
            },
        )?
    };
    let catalog_timeout = Duration::from_millis(config.catalog_wait_timeout_ms);
    let (permission, bucket) = match &operation {
        ObjectRequest::Copy(params) => (auth::OBJECTS_CREATE, &params.bucket),
        ObjectRequest::List(params) => (auth::OBJECTS_LIST, &params.query.bucket),
        ObjectRequest::Update { bucket, .. } => (auth::OBJECTS_UPDATE, bucket),
        ObjectRequest::Compose { bucket, .. }
        | ObjectRequest::Insert { bucket, .. }
        | ObjectRequest::Multipart { bucket, .. }
        | ObjectRequest::Resumable { bucket, .. } => (auth::OBJECTS_CREATE, bucket),
        ObjectRequest::Get { bucket, .. } => (auth::OBJECTS_GET, bucket),
        ObjectRequest::Delete { bucket, .. } => (auth::OBJECTS_DELETE, bucket),
    };
    // Authorization precedes header/body validation, conditions and existence checks.
    // Replacing a live object additionally needs storage.objects.delete. The access carries
    // that decision, and publication enforces it inside the catalog transaction if the name is
    // live, so a concurrent create cannot be overwritten.
    let access = authorize_bucket(state, caller, permission, bucket).await?;
    let query = json::parse_query(request.uri().query().unwrap_or_default())?;
    if query.contains_key("predefinedAcl") || query.contains_key("destinationPredefinedAcl") {
        let record = state
            .catalog
            .get_bucket(bucket)
            .await?
            .ok_or(CatalogError::NotFound)?;
        json::reject_predefined_acl(
            request.uri().query(),
            record.uniform_bucket_level_access_locked_time.is_some(),
            true,
        )?;
    }
    json::validate_read_condition_headers(
        request.headers(),
        matches!(operation, ObjectRequest::Get { .. }),
    )?;
    if !matches!(
        operation,
        ObjectRequest::Insert { .. }
            | ObjectRequest::Compose { .. }
            | ObjectRequest::Multipart { .. }
            | ObjectRequest::Resumable { .. }
            | ObjectRequest::Update { .. }
            | ObjectRequest::Copy(_)
    ) && (request.headers().contains_key("transfer-encoding")
        || request
            .headers()
            .get("content-length")
            .is_some_and(|v| v != "0"))
    {
        return Err(JsonError::invalid_argument(
            "This method requires an empty body",
        ));
    }
    let mut value = match operation {
        ObjectRequest::Copy(params) => {
            json::validate_object_get_headers(request.headers(), false)?;
            for name in [
                "content-md5",
                "x-goog-hash",
                "range",
                "if-range",
                "if-modified-since",
                "if-unmodified-since",
                "x-goog-gcs-idempotency-token",
            ] {
                if request.headers().contains_key(name) {
                    return Err(JsonError::unsupported_object(name));
                }
            }
            let source_access =
                authorize_bucket(state, caller, auth::OBJECTS_GET, &params.source_bucket).await?;
            let source = state
                .catalog
                .resolve_derived_source(
                    &params.source_bucket,
                    &params.source_name,
                    params.source_generation,
                    params.source_conditions,
                    &source_access,
                )
                .await?;
            let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
            let (metadata, storage_class, expected) =
                json::derived_metadata(&bytes, &source.object)?;
            let source_location = if source.object.component_count.is_some() {
                Some(
                    state
                        .catalog
                        .get_bucket(&params.source_bucket)
                        .await?
                        .ok_or(ObjectError::BucketNotFound)?
                        .location,
                )
            } else {
                None
            };
            let projection = params.projection;
            let record = copy::execute(
                &state.catalog,
                source,
                source_access,
                crate::storage::DerivedDestination {
                    bucket: params.bucket,
                    name: params.name,
                    content_type: String::new(),
                    metadata,
                    storage_class,
                    expected,
                    conditions: params.conditions,
                    access,
                    component_count: None,
                },
                source_location,
                config,
                deadline,
                activity,
                #[cfg(test)]
                None,
            )
            .await?;
            json::object_resource(&record, &state.api_base_url, projection)?
        }
        ObjectRequest::Compose {
            bucket,
            name,
            conditions,
        } => {
            json::validate_object_get_headers(request.headers(), false)?;
            for header in [
                "if-range",
                "if-modified-since",
                "if-unmodified-since",
                "range",
                "content-md5",
                "x-goog-hash",
            ] {
                if request.headers().contains_key(header) {
                    return Err(JsonError::unsupported_object(header));
                }
            }
            let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
            let body = json::compose::parse_compose(&bytes, &bucket, &name)?;
            activity.send_modify(|progress| progress.awaiting_catalog = true);
            let record = state
                .catalog
                .compose_object(
                    crate::storage::DerivedDestination {
                        bucket,
                        name,
                        conditions,
                        access,
                        metadata: body.destination,
                        content_type: String::new(),
                        storage_class: None,
                        expected: Default::default(),
                        component_count: None,
                    },
                    body.sources,
                    body.delete_sources,
                    max_bytes,
                )
                .await?;
            json::object_resource(&record, &state.api_base_url, json::Projection::NoAcl)?
        }
        ObjectRequest::Resumable {
            bucket,
            name,
            params,
            content_encoding,
        } => {
            return resumable::initiate(
                request,
                &bucket,
                name,
                params,
                content_encoding,
                access,
                state,
                config,
                activity,
                deadline,
            )
            .await;
        }
        ObjectRequest::List(params) => {
            json::validate_object_get_headers(request.headers(), false)?;
            let page = state
                .catalog
                .list_objects_authorized(params.query.clone(), params.max_results, &access)
                .await?;
            json::object_list(&page, &params, &state.api_base_url)?
        }
        ObjectRequest::Update {
            bucket,
            name,
            params,
            replace,
        } => {
            json::validate_object_get_headers(request.headers(), false)?;
            // shortcut: SDK invocation tokens do not replay responses; use metageneration conditions for retry safety.
            let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
            let update = json::parse_object_metadata(&bytes, replace)?;
            let base = state.api_base_url.clone();
            let record = state
                .catalog
                .update_object_metadata_authorized(
                    &bucket,
                    &name,
                    params.generation,
                    params.conditions,
                    update.update,
                    move |record| {
                        json::ObjectMetadataRequest::validate_unchanged(
                            &update.unchanged,
                            record,
                            &base,
                        )
                    },
                    &access,
                )
                .await?;
            json::object_resource(&record, &state.api_base_url, params.projection)?
        }
        ObjectRequest::Multipart {
            bucket,
            name,
            params,
        } => {
            let (_, expected) =
                json::parse_object_upload_headers(request.headers(), params.conditions)?;
            let boundary = json::multipart_boundary(request.headers())?;
            // The parser's JSON, header, delimiter-padding and discard caps total less than 256 KiB.
            if request
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .is_some_and(|length| length > max_bytes.saturating_add(256 * 1024))
            {
                return Err(ObjectError::TooLarge.into());
            }
            let permit = state.catalog.admit_object_upload()?;
            // The final name may be in metadata; check the bucket before reading any body.
            await_upload_catalog(
                state.catalog.check_upload(
                    &bucket,
                    name.as_deref().unwrap_or(""),
                    if name.is_some() {
                        params.conditions
                    } else {
                        Default::default()
                    },
                    &access,
                ),
                catalog_timeout,
                deadline,
                activity,
            )
            .await?;
            let body = request.into_body().map_frame(|frame| {
                activity.send_modify(|progress| progress.awaiting_catalog = false);
                frame
            });
            let mut multipart = json::Multipart::new(body, &boundary);
            let bytes = multipart.metadata().await?;
            let name_known = name.is_some();
            let (name, metadata, expected) = json::multipart_metadata(&bytes, name, expected)?;
            if !name_known {
                await_upload_catalog(
                    state
                        .catalog
                        .check_upload(&bucket, &name, params.conditions, &access),
                    catalog_timeout,
                    deadline,
                    activity,
                )
                .await?;
            }
            let content_type = multipart
                .headers(false)
                .await?
                .unwrap_or_else(|| "application/octet-stream".into());
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
                metadata,
                access,
            );
            tokio::pin!(upload);
            let streaming = async {
                let mut size = 0_u64;
                while let Some(bytes) = multipart.next_chunk().await? {
                    size = size
                        .checked_add(bytes.len() as u64)
                        .filter(|size| *size <= max_bytes)
                        .ok_or_else(|| JsonError::from(ObjectError::TooLarge))?;
                    if sender.send(Ok(UploadChunk::Data(bytes))).await.is_err() {
                        return Ok::<_, JsonError>(expected);
                    }
                }
                let expected = multipart.checksums(expected).await?;
                multipart.finish().await?;
                Ok(expected)
            };
            let record = tokio::select! {
                result = &mut upload => result?,
                result = streaming => {
                    let expected = match result {
                        Ok(expected) => expected,
                        Err(error) => {
                            let _ = sender.send(Err(io::Error::other(error.to_string()))).await;
                            drop(sender);
                            // Await cleanup before returning a framing error to the client.
                            match await_upload_catalog(upload, catalog_timeout, deadline, activity).await {
                                Ok(_) | Err(ObjectError::BodyAborted(_)) => {},
                                Err(storage_error) => return Err(storage_error.into()),
                            }
                            return Err(error);
                        }
                    };
                    await_upload_catalog(async {
                        let _ = sender.send(Ok(UploadChunk::EndWithHashes(expected))).await;
                        drop(sender);
                        upload.await
                    }, catalog_timeout, deadline, activity).await?
                }
            };
            json::object_resource(&record, &state.api_base_url, params.projection)?
        }
        ObjectRequest::Insert {
            bucket,
            name,
            params,
        } => {
            let (content_type, expected) =
                json::parse_object_upload_headers(request.headers(), params.conditions)?;
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
            let permit = await_upload_catalog(
                state
                    .catalog
                    .prepare_object_upload(&bucket, &name, params.conditions, &access),
                catalog_timeout,
                deadline,
                activity,
            )
            .await?;
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
                Default::default(),
                access,
            );
            tokio::pin!(upload);
            let record = tokio::select! {
                result = &mut upload => result?,
                result = stream_upload(request.into_body(), &sender, max_bytes, activity, None) => {
                    if let Err(error) = result {
                        if sender.send(Err(io::Error::other(error.to_string()))).await.is_err() {
                            return Err(upload.await.err().unwrap_or(error).into());
                        }
                        return Err(error.into());
                    }
                    await_upload_catalog(async {
                        let _ = sender.send(Ok(UploadChunk::End)).await;
                        drop(sender);
                        upload.await
                    }, catalog_timeout, deadline, activity).await?
                }
            };
            json::object_resource(&record, &state.api_base_url, params.projection)?
        }
        ObjectRequest::Get {
            bucket,
            name,
            params,
        } => {
            // Match failures precede both query and HTTP cache validators on one snapshot.
            let mixed = request.headers().contains_key("if-match")
                || request.headers().contains_key("if-none-match");
            let conditions = if mixed {
                ObjectConditions {
                    if_generation_not_match: None,
                    if_metageneration_not_match: None,
                    ..params.conditions
                }
            } else {
                params.conditions
            };
            if params.media {
                json::validate_object_get_headers(request.headers(), true)?;
                let (record, mut reader) = state
                    .catalog
                    .open_object_authorized(&bucket, &name, params.generation, conditions, &access)
                    .await?;
                let mut headers = json::object_media_headers(&record)?;
                if reader.publicly_readable {
                    headers.insert(
                        "cache-control",
                        record
                            .cache_control
                            .as_deref()
                            .unwrap_or("public, max-age=3600")
                            .parse()
                            .map_err(|_| {
                                JsonError::new(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "backendError",
                                    "Invalid stored Cache-Control",
                                )
                            })?,
                    );
                }
                json::validate_object_media_encoding(request.headers(), &record)?;
                if read_etag_not_modified(request.headers(), &json::object_etag(&record))?
                    || params.conditions.if_generation_not_match == Some(record.generation)
                    || params.conditions.if_metageneration_not_match == Some(record.metageneration)
                {
                    return Ok(boxed(not_modified_response(
                        &headers["etag"],
                        &headers["cache-control"],
                    )));
                }
                let range = match json::object_media_range(request.headers(), record.size) {
                    Ok(range) => range,
                    Err(error) => {
                        let mut response = error
                            .into_response()
                            .map(|body| body.map_err(|never| match never {}).boxed_unsync());
                        response.headers_mut().insert(
                            "content-range",
                            format!("bytes */{}", record.size).parse().unwrap(),
                        );
                        return Ok(response);
                    }
                };
                if let Some((offset, length)) = range {
                    reader.set_range(offset, length).await.map_err(|_| {
                        JsonError::new(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "backendError",
                            "Cannot seek object data",
                        )
                    })?;
                    headers.insert("content-length", length.into());
                    headers.insert(
                        "content-range",
                        format!("bytes {}-{}/{}", offset, offset + length - 1, record.size)
                            .parse()
                            .unwrap(),
                    );
                }
                let mut response = Response::new(object_body(reader, activity));
                if range.is_some() {
                    *response.status_mut() = StatusCode::PARTIAL_CONTENT;
                }
                *response.headers_mut() = headers;
                return Ok(response);
            }
            json::validate_object_get_headers(request.headers(), false)?;
            let record = state
                .catalog
                .get_object_authorized(&bucket, &name, params.generation, conditions, &access)
                .await?;
            return Ok(boxed(read_metadata_response(
                request.headers(),
                &json::object_etag(&record),
                json::object_resource(&record, &state.api_base_url, params.projection)?,
                &fields,
                params.conditions.if_generation_not_match == Some(record.generation)
                    || params.conditions.if_metageneration_not_match == Some(record.metageneration),
            )?));
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
                .delete_object_authorized(&bucket, &name, generation, conditions, &access)
                .await?;
            return Ok(boxed(empty_response(StatusCode::NO_CONTENT)));
        }
    };
    fields.apply(&mut value);
    Ok(boxed(
        Response::builder()
            .header("content-type", "application/json; charset=UTF-8")
            .body(Full::new(Bytes::from(value.to_string())))
            .unwrap(),
    ))
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

async fn xml_response(
    request: Request<Incoming>,
    mut operation: xml::ReadRequest,
    caller: Option<Caller<'_>>,
    state: &State,
    activity: &watch::Sender<ObjectActivity>,
) -> Result<Response<ResponseBody>, xml::Error> {
    let access = authorize_bucket(state, caller, auth::OBJECTS_GET, &operation.bucket)
        .await
        .map_err(|error| anonymous_error(error, caller))?;
    xml::headers(request.headers(), &mut operation)?;
    let (record, mut reader) = state
        .catalog
        .open_object_authorized(
            &operation.bucket,
            &operation.name,
            operation.generation,
            operation.conditions,
            &access,
        )
        .await
        .map_err(|error| anonymous_error(error.into(), caller))?;
    let mut headers = xml::media_headers(&record, reader.publicly_readable)?;
    headers.extend(operation.response_overrides);
    json::validate_object_media_encoding(request.headers(), &record)?;
    let head = request.method() == Method::HEAD;
    let range = match json::object_media_range(request.headers(), record.size) {
        Ok(range) => range,
        Err(_) if head => {
            // HEAD's Range only adds Content-Range; status and representation length stay unchanged.
            headers.insert(
                "content-range",
                format!("bytes */{}", record.size).parse().unwrap(),
            );
            None
        }
        Err(error) => {
            let mut response = boxed(xml::Error::from(error).response(head));
            response.headers_mut().insert(
                "content-range",
                format!("bytes */{}", record.size).parse().unwrap(),
            );
            return Ok(response);
        }
    };
    if let Some((offset, length)) = range {
        if !head {
            reader.set_range(offset, length).await.map_err(|_| {
                xml::Error::from(JsonError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Cannot seek object data",
                ))
            })?;
            headers.insert("content-length", length.into());
        }
        headers.insert(
            "content-range",
            format!("bytes {}-{}/{}", offset, offset + length - 1, record.size)
                .parse()
                .unwrap(),
        );
    }
    let mut response = if head {
        boxed(empty_response(StatusCode::OK))
    } else {
        Response::new(object_body(reader, activity))
    };
    if range.is_some() && !head {
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    }
    *response.headers_mut() = headers;
    Ok(response)
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
        let bytes = frame
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
    if sha256
        .zip(expected_sha256)
        .is_some_and(|(hash, expected)| hash.finish().as_ref() != expected)
    {
        return Err(ObjectError::ChecksumMismatch);
    }
    Ok(())
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

async fn bucket_response(
    request: Request<Incoming>,
    operation: BucketRequest,
    caller: Option<Caller<'_>>,
    state: &State,
    profile: CompatProfile,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let access = match &operation {
        BucketRequest::Insert(params) => {
            authorize_project(caller, auth::BUCKETS_CREATE, &params.project)?;
            BucketAccess::Permissive
        }
        BucketRequest::List(params) => {
            authorize_project(caller, auth::BUCKETS_LIST, &params.project)?;
            BucketAccess::Permissive
        }
        BucketRequest::Get { bucket, .. } => {
            authorize_bucket(state, caller, auth::BUCKETS_GET, bucket).await?
        }
        BucketRequest::Patch { bucket, .. } | BucketRequest::Update { bucket, .. } => {
            authorize_bucket(state, caller, auth::BUCKETS_UPDATE, bucket).await?
        }
        BucketRequest::Delete { bucket, .. } => {
            authorize_bucket(state, caller, auth::BUCKETS_DELETE, bucket).await?
        }
        BucketRequest::GetIam { bucket } => {
            authorize_bucket(state, caller, auth::BUCKETS_GET_IAM, bucket).await?
        }
        BucketRequest::SetIam { bucket } => {
            authorize_bucket(state, caller, auth::BUCKETS_SET_IAM, bucket).await?
        }
        BucketRequest::TestIam { .. } => BucketAccess::Permissive,
    };
    json::validate_read_condition_headers(
        request.headers(),
        matches!(
            operation,
            BucketRequest::Get { .. } | BucketRequest::GetIam { .. }
        ),
    )?;
    if !matches!(
        operation,
        BucketRequest::Insert(_)
            | BucketRequest::SetIam { .. }
            | BucketRequest::Patch { .. }
            | BucketRequest::Update { .. }
    ) && (request.headers().contains_key("transfer-encoding")
        || request
            .headers()
            .get("content-length")
            .is_some_and(|v| v != "0"))
    {
        return Err(JsonError::invalid_argument(
            "This method requires an empty body",
        ));
    }
    let fields = if matches!(operation, BucketRequest::TestIam { .. }) {
        json::test_permissions_query(request.uri().query())?.1
    } else if matches!(operation, BucketRequest::Delete { .. }) {
        json::Fields::from_query(None, "Bucket")?
    } else {
        let schema = match operation {
            BucketRequest::List(_) => "Buckets",
            BucketRequest::GetIam { .. } | BucketRequest::SetIam { .. } => "Policy",
            _ => "Bucket",
        };
        json::Fields::from_query(request.uri().query(), schema)?
    };
    let replace = matches!(operation, BucketRequest::Update { .. });
    let acl_query = request.uri().query().map(str::to_owned);
    let mut value = match operation {
        BucketRequest::TestIam {
            bucket,
            permissions,
        } => {
            // This method requires no permission; existence is independent of policy visibility.
            state
                .catalog
                .get_policy(&bucket, &BucketAccess::Permissive)
                .await?;
            let mut granted = Vec::new();
            for permission in permissions {
                match authorize_bucket(state, caller, &permission, &bucket).await {
                    Ok(_) => granted.push(permission),
                    Err(error) if error.status == StatusCode::FORBIDDEN => {}
                    Err(error) => return Err(error),
                }
            }
            serde_json::json!({"kind": "storage#testIamPermissionsResponse", "permissions": granted})
        }
        BucketRequest::Patch { bucket, params } | BucketRequest::Update { bucket, params } => {
            let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
            let update = if replace {
                json::parse_bucket_replacement(&bytes, profile)?
            } else {
                json::parse_bucket_patch(&bytes, profile)?
            };
            let record = state
                .catalog
                .get_bucket_authorized(&bucket, &access)
                .await?
                .ok_or(CatalogError::NotFound)?;
            json::reject_predefined_acl(
                acl_query.as_deref(),
                record.uniform_bucket_level_access_locked_time.is_some()
                    || update.uniform_bucket_level_access_enabled == Some(true),
                false,
            )?;
            let record = if replace {
                state
                    .catalog
                    .replace_bucket(
                        &bucket,
                        update,
                        params.conditions.if_metageneration_match,
                        params.conditions.if_metageneration_not_match,
                        &access,
                    )
                    .await
            } else {
                state
                    .catalog
                    .patch_bucket(
                        &bucket,
                        update,
                        params.conditions.if_metageneration_match,
                        params.conditions.if_metageneration_not_match,
                        &access,
                    )
                    .await
            }
            .map_err(|error| bucket_catalog_error(error, profile, None))?;
            json::bucket_resource_with_profile(
                &record,
                &state.api_base_url,
                params.projection,
                profile,
            )?
        }
        BucketRequest::GetIam { bucket } => {
            let policy = state.catalog.get_policy(&bucket, &access).await?;
            return read_metadata_response(
                request.headers(),
                &format!(
                    "\"{}\"",
                    policy.etag.as_deref().expect("stored policy etag")
                ),
                json::policy_resource(&policy, &bucket),
                &fields,
                false,
            );
        }
        BucketRequest::SetIam { bucket } => {
            let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
            let policy = json::parse_policy(&bytes)?;
            let policy = state.catalog.set_policy(&bucket, policy, &access).await?;
            json::policy_resource(&policy, &bucket)
        }
        BucketRequest::Insert(params) => {
            let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
            let record = json::parse_insert_with_profile(
                &bytes,
                &params.project,
                state.clock.now(),
                profile,
                &state.default_location,
            )?;
            let name = record.name.clone();
            json::reject_predefined_acl(
                acl_query.as_deref(),
                record.uniform_bucket_level_access_locked_time.is_some(),
                false,
            )?;
            let record = state
                .catalog
                .insert_bucket(record)
                .await
                .map_err(|error| bucket_catalog_error(error, profile, Some(&name)))?;
            json::bucket_resource_with_profile(
                &record,
                &state.api_base_url,
                params.projection,
                profile,
            )?
        }
        BucketRequest::Get { bucket, params } => {
            let record = state
                .catalog
                .get_bucket_authorized(&bucket, &access)
                .await?
                .ok_or_else(|| bucket_catalog_error(CatalogError::NotFound, profile, None))?;
            let conditions = params.conditions;
            if conditions
                .if_metageneration_match
                .is_some_and(|v| v != record.metageneration)
            {
                return Err(CatalogError::PreconditionFailed.into());
            }
            let query_not_modified =
                conditions.if_metageneration_not_match == Some(record.metageneration);
            if query_not_modified
                && !request.headers().contains_key("if-match")
                && !request.headers().contains_key("if-none-match")
            {
                return Ok(empty_response(StatusCode::NOT_MODIFIED));
            }
            return read_metadata_response(
                request.headers(),
                &json::bucket_etag(&record),
                json::bucket_resource_with_profile(
                    &record,
                    &state.api_base_url,
                    params.projection,
                    profile,
                )?,
                &fields,
                query_not_modified,
            );
        }
        BucketRequest::List(params) => {
            let limit = params.max_results as usize;
            let page = state
                .catalog
                .list_buckets_page(
                    &params.project,
                    Some(&params.prefix),
                    params.start_after.as_deref(),
                    limit,
                )
                .await?;
            let token = if page.has_more {
                Some(json::encode_page_token(
                    &params.project,
                    &params.prefix,
                    &page.items.last().expect("nonempty continuation page").name,
                )?)
            } else {
                None
            };
            json::bucket_list_with_profile(
                &page.items,
                token.as_deref(),
                &state.api_base_url,
                params.projection,
                profile,
            )?
        }
        BucketRequest::Delete { bucket, conditions } => {
            state
                .catalog
                .delete_bucket_authorized(
                    &bucket,
                    conditions.if_metageneration_match,
                    conditions.if_metageneration_not_match,
                    &access,
                )
                .await
                .map_err(|error| bucket_catalog_error(error, profile, None))?;
            return Ok(if profile == CompatProfile::LegacyEmulator {
                Response::builder()
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from_static(b"null\n")))
                    .unwrap()
            } else {
                empty_response(StatusCode::NO_CONTENT)
            });
        }
    };
    fields.apply(&mut value);
    Ok(Response::builder()
        .header("content-type", "application/json; charset=UTF-8")
        .body(Full::new(Bytes::from(value.to_string())))
        .unwrap())
}

fn bucket_catalog_error(
    error: CatalogError,
    profile: CompatProfile,
    name: Option<&str>,
) -> JsonError {
    if profile == CompatProfile::LegacyEmulator {
        match error {
            CatalogError::NotFound => {
                return JsonError::new(StatusCode::NOT_FOUND, "notFound", "Not Found");
            }
            CatalogError::NotEmpty => {
                return JsonError::new(
                    StatusCode::PRECONDITION_FAILED,
                    "conditionNotMet",
                    "bucket must be empty prior to deletion",
                );
            }
            CatalogError::AlreadyExists => {
                return JsonError::new(
                    StatusCode::CONFLICT,
                    "conflict",
                    format!(
                        "A Cloud Storage bucket named '{}' already exists. Try another name. Bucket names must be globally unique across all Google Cloud projects, including those outside of your organization.",
                        name.unwrap_or_default()
                    ),
                );
            }
            _ => {}
        }
    }
    error.into()
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
    Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .header("etag", etag)
        .header("cache-control", cache_control)
        .body(Full::new(Bytes::new()))
        .unwrap()
}

fn read_metadata_response(
    headers: &hyper::HeaderMap,
    etag: &str,
    mut value: serde_json::Value,
    fields: &json::Fields,
    query_not_modified: bool,
) -> Result<Response<Full<Bytes>>, JsonError> {
    if read_etag_not_modified(headers, etag)? || query_not_modified {
        return Ok(not_modified_response(
            &etag.parse().unwrap(),
            &hyper::header::HeaderValue::from_static("no-cache"),
        ));
    }
    fields.apply(&mut value);
    Ok(Response::builder()
        .header("content-type", "application/json; charset=UTF-8")
        .header("etag", etag)
        .header("cache-control", "no-cache")
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
