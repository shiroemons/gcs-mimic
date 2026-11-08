//! JSON API batching without a second listener connection or catalog transaction.
//!
//! The envelope is bounded by contract and read whole; the response has no size of its own, so
//! each part's response leaves as soon as it is ready instead of accumulating with the others.

use super::{
    ObjectActivity, RequestBody, RequestCtx, ResponseBody, State, boxed,
    chunk_body::{self, ChunkSender, Completion},
};
use crate::{
    auth::ListenerRole,
    config::{Config, RequestEndpoints},
    json::{self, JsonError},
};
use http_body_util::BodyExt;
use hyper::body::{Frame, SizeHint};
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode,
    body::{Body, Bytes},
    header::{CONTENT_TYPE, HeaderValue},
};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

struct BatchBody {
    body: chunk_body::CheckedBody,
    _admission: OwnedSemaphorePermit,
}

impl Body for BatchBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        Pin::new(&mut self.body).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

mod parse;

/// Cloud Storage documents a total batch request below 10 MB.
const REQUEST_LIMIT: usize = 10_000_000;
/// The most one part's response may buffer. Every route a part can reach has already rendered its
/// whole body, so this only guards against a future streaming route. It sits above the largest
/// response that documented limits allow, 1,000 objects with the longest names and 100 ACL entries
/// of about 5 KiB each under `projection=full`: a limit a real response could reach would report a
/// completed operation as a failure.
const PART_RESPONSE_LIMIT: usize = 1024 * 1024 * 1024;
pub(super) const BATCH_CONCURRENCY: usize = 2;
const BOUNDARY: &str = "gcs_mimic_batch_response";
const RESPONSE_CONTENT_TYPE: HeaderValue =
    HeaderValue::from_static("multipart/mixed; boundary=gcs_mimic_batch_response");
static PARSERS: Semaphore = Semaphore::const_new(1);

struct Part {
    id: Option<String>,
    request: Request<RequestBody>,
}

fn invalid(message: &'static str) -> JsonError {
    JsonError::new(StatusCode::BAD_REQUEST, "badRequest", message)
}

pub(super) async fn respond(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Response<ResponseBody> {
    match execute(request, ctx).await {
        Ok(response) => response.map(BodyExt::boxed_unsync),
        Err(error) => boxed(error.into_response()),
    }
}

async fn execute<B: Body<Data = Bytes> + Unpin>(
    request: Request<B>,
    ctx: RequestCtx<'_>,
) -> Result<Response<BatchBody>, JsonError> {
    if request.method() != Method::POST {
        return Err(invalid("Batch requests require POST /batch/storage/v1"));
    }
    let boundary = json::batch_boundary(request.headers())?;
    if request.headers().contains_key("content-encoding") {
        return Err(invalid("Compressed batch envelopes are unsupported"));
    }
    let deadline = ctx.start_object_deadline(super::DeadlineWatch::CatalogOnly);
    let admission =
        tokio::time::timeout_at(deadline, ctx.state.pools.batches.clone().acquire_owned())
            .await
            .map_err(|_| {
                JsonError::service_unavailable(
                    "Timed out waiting for batch capacity; retry after an active response finishes",
                )
            })?
            .map_err(|_| JsonError::service_unavailable("Batch admission is unavailable"))?;
    let (outer, mut body) = request.into_parts();
    if body.size_hint().lower() >= REQUEST_LIMIT as u64 {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let data = frame
            .map_err(|_| invalid("Could not read batch request"))?
            .into_data()
            .map_err(|_| invalid("Batch request trailers are unsupported"))?;
        if data.len() >= REQUEST_LIMIT - bytes.len() {
            return Err(too_large());
        }
        bytes.extend_from_slice(&data);
    }
    let permit = PARSERS.acquire().await.map_err(|_| parser_unavailable())?;
    let api_base_url = ctx.endpoints.api_base_url().to_owned();
    let parts = crate::task::run_blocking("batch-parser", move || {
        let _permit = permit;
        parse::parts(
            &Bytes::from(bytes),
            &boundary,
            &outer.headers,
            outer.uri.query(),
            &api_base_url,
        )
    })
    .await
    .map_err(|_| parser_failed())??;
    // Everything above can still fail the envelope as a whole; from here each part answers alone.
    let (sender, completion, body) =
        chunk_body::checked_channel("Batch response ended before its closing delimiter");
    let runner = PartRunner {
        state: Arc::clone(ctx.state),
        config: ctx.config.clone(),
        endpoints: ctx.endpoints.clone(),
        connection: ctx.activity.clone(),
        listener: ctx.listener,
        sender,
        completion,
    };
    crate::task::spawn_supervised("batch-parts", runner.run(parts));
    let mut response = Response::new(BatchBody {
        body,
        _admission: admission,
    });
    response
        .headers_mut()
        .insert(CONTENT_TYPE, RESPONSE_CONTENT_TYPE);
    Ok(response)
}

/// What the parts need after the handler has returned the response head.
struct PartRunner {
    state: Arc<State>,
    config: Config,
    endpoints: RequestEndpoints,
    connection: watch::Sender<ObjectActivity>,
    /// A part arrived inside the envelope, so it authenticates as on the envelope's listener.
    listener: ListenerRole,
    sender: ChunkSender,
    completion: Completion,
}

impl PartRunner {
    /// Runs the parts in order. The chunk channel holds one part ahead of the socket, so a client
    /// that stops reading stops the remaining parts, and one that leaves cancels them, as closing
    /// the connection cancels any other handler.
    async fn run(self, parts: Vec<Part>) {
        let Self {
            state,
            config,
            endpoints,
            connection,
            listener,
            sender,
            completion,
        } = self;
        let activity = super::new_batch_part_activity();
        let ctx = RequestCtx {
            state: &state,
            config: &config,
            endpoints: &endpoints,
            activity: &activity,
            listener,
        };
        for part in parts {
            let rendered = tokio::select! {
                biased;
                () = sender.closed() => return,
                rendered = render_part(part, ctx, &connection) => rendered,
            };
            if sender.send(Ok(Bytes::from(rendered))).await.is_err() {
                return;
            }
            super::record_progress(&connection);
        }
        completion.mark();
        let closing = format!("--{BOUNDARY}--\r\n");
        let _ = sender.send(Ok(Bytes::from(closing))).await;
    }
}

/// Runs one part and renders it with its MIME framing.
async fn render_part(
    part: Part,
    ctx: RequestCtx<'_>,
    connection: &watch::Sender<ObjectActivity>,
) -> Vec<u8> {
    let _work = super::BatchPartWork::begin(connection);
    // Each part starts its own deadline, without an earlier part's state.
    ctx.activity.send_replace(ObjectActivity::default());
    // Each part renders in the layout its own (inherited-or-overridden) prettyPrint selects, not
    // the layout of the envelope request.
    let format =
        json::ResponseFormat::for_request(part.request.uri().query(), ctx.config.compat_profile);
    let Part { id, request } = part;
    json::with_format(format, async move {
        let response = tokio::select! {
            biased;
            response = dispatch(request, ctx) => response,
            expiry = super::batch_part_expired(ctx.activity, ctx.config.request_timeout) => {
                super::batch_part_timeout(expiry)
            }
        };
        let mut output = Vec::new();
        append_response(&mut output, id.as_deref(), response, PART_RESPONSE_LIMIT).await;
        output
    })
    .await
}

/// A part reaches this module's caller again through the router, so its future type would contain
/// itself. The named boxed type ends that cycle, and lets the spawned runner prove it is `Send`.
fn dispatch<'a>(
    request: Request<RequestBody>,
    ctx: RequestCtx<'a>,
) -> Pin<Box<dyn Future<Output = Response<ResponseBody>> + Send + 'a>> {
    Box::pin(super::routing::respond(request, ctx))
}

fn parser_failed() -> JsonError {
    JsonError::service_unavailable("Batch parser failed")
}

fn parser_unavailable() -> JsonError {
    JsonError::service_unavailable("Batch parser unavailable")
}

fn too_large() -> JsonError {
    JsonError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "uploadTooLarge",
        "Batch request must be smaller than 10 MB",
    )
}

async fn bounded_response(
    response: Response<ResponseBody>,
    cap: usize,
) -> Result<(StatusCode, HeaderMap, Vec<u8>), JsonError> {
    let (parts, mut body) = response.into_parts();
    let header_bytes: usize = parts
        .headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.len() + 4)
        .sum();
    if header_bytes > cap {
        return Err(response_too_large());
    }
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let data = frame
            .map_err(|error| {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Error,
                    "batch_response_stream_failed",
                    serde_json::json!({"cause": error.kind().to_string()}),
                );
                JsonError::service_unavailable("Batched response stream failed")
            })?
            .into_data()
            .map_err(|_| {
                JsonError::service_unavailable("Batched response trailers are unsupported")
            })?;
        if data.len() > cap.saturating_sub(bytes.len() + header_bytes) {
            return Err(response_too_large());
        }
        bytes.extend_from_slice(&data);
    }
    Ok((parts.status, parts.headers, bytes))
}

fn response_too_large() -> JsonError {
    JsonError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "responseTooLarge",
        "Batched response exceeds the local response budget; the operation may have completed",
    )
}

async fn append_response(
    output: &mut Vec<u8>,
    id: Option<&str>,
    response: Response<ResponseBody>,
    cap: usize,
) {
    let (status, headers, body) = match bounded_response(response, cap).await {
        Ok(response) => response,
        Err(error) => {
            let (parts, body) = error.into_response().into_parts();
            let body = match body.collect().await {
                Ok(body) => body.to_bytes(),
                Err(never) => match never {},
            };
            (parts.status, parts.headers, body.to_vec())
        }
    };
    output.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Type: application/http\r\n").as_bytes(),
    );
    if let Some(id) = id {
        let id = if let Some(id) = id.strip_prefix('<').and_then(|id| id.strip_suffix('>')) {
            format!("<response-{id}>")
        } else {
            format!("response-{id}")
        };
        output.extend_from_slice(format!("Content-ID: {id}\r\n").as_bytes());
    }
    output.extend_from_slice(
        format!(
            "\r\nHTTP/1.1 {} {}\r\n",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        )
        .as_bytes(),
    );
    for (name, value) in &headers {
        if !parse::hop_header(name.as_str()) && name != "content-length" {
            output.extend_from_slice(name.as_str().as_bytes());
            output.extend_from_slice(b": ");
            output.extend_from_slice(value.as_bytes());
            output.extend_from_slice(b"\r\n");
        }
    }
    output.extend_from_slice(format!("content-length: {}\r\n\r\n", body.len()).as_bytes());
    output.extend_from_slice(&body);
    output.extend_from_slice(b"\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::Auth,
        storage::Catalog,
        test_support::{TestDir, eventually_async},
        time_limit::TimeLimit,
    };
    use http_body_util::Full;
    use std::{convert::Infallible, time::Duration};

    /// One server's state, without a connection; `activity` stands in for the connection's.
    struct Harness {
        _dir: TestDir,
        state: Arc<State>,
        config: Config,
        endpoints: RequestEndpoints,
        activity: watch::Sender<ObjectActivity>,
    }

    impl Harness {
        async fn start(config: Config) -> Self {
            let dir = TestDir::new();
            let state = Arc::new(State::for_test(
                Catalog::open(&dir.0).await.unwrap(),
                Auth::permissive(),
            ));
            let endpoints = config
                .runtime_endpoints
                .snapshot(&config, "http://localhost")
                .unwrap()
                .without_request();
            let (activity, _) = watch::channel(ObjectActivity::default());
            Self {
                _dir: dir,
                state,
                config,
                endpoints,
                activity,
            }
        }

        fn ctx(&self) -> RequestCtx<'_> {
            RequestCtx {
                state: &self.state,
                config: &self.config,
                endpoints: &self.endpoints,
                activity: &self.activity,
                listener: ListenerRole::Public,
            }
        }

        async fn status(&self, method: &str, uri: &str, body: &'static [u8]) -> StatusCode {
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(RequestBody::Buffered(Full::new(Bytes::from_static(body))))
                .unwrap();
            super::super::routing::respond(request, self.ctx())
                .await
                .status()
        }

        /// A bucket holding one object for each name.
        async fn objects(&self, names: &[&str]) {
            assert_eq!(
                self.status(
                    "POST",
                    "/storage/v1/b?project=local",
                    br#"{"name":"bucket"}"#
                )
                .await,
                StatusCode::OK
            );
            for name in names {
                let uri = format!("/upload/storage/v1/b/bucket/o?uploadType=media&name={name}");
                assert_eq!(self.status("POST", &uri, b"data").await, StatusCode::OK);
            }
        }
    }

    fn envelope(parts: &[String]) -> Request<Full<Bytes>> {
        let parts: String = parts
            .iter()
            .map(|part| {
                format!(
                    "--test\r\nContent-Type: application/http\r\nContent-Transfer-Encoding: binary\r\n\r\n{part}\r\n"
                )
            })
            .collect();
        Request::builder()
            .method("POST")
            .uri("/batch/storage/v1")
            .header("content-type", "multipart/mixed; boundary=test")
            .body(Full::new(Bytes::from(format!("{parts}--test--\r\n"))))
            .unwrap()
    }

    fn delete(name: &str) -> String {
        format!("DELETE /storage/v1/b/bucket/o/{name} HTTP/1.1\r\n\r\n")
    }

    /// A gzip-encoded part, which waits for a decoder a test can keep busy.
    fn gzip_patch() -> String {
        "PATCH /storage/v1/b/bucket HTTP/1.1\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\n\r\n".into()
    }

    async fn next_text(body: &mut BatchBody) -> String {
        let frame = body.frame().await.unwrap().unwrap();
        String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn each_part_leaves_as_its_own_chunk_and_a_stalled_reader_holds_back_later_parts() {
        let harness = Harness::start(Config::default()).await;
        harness.objects(&["a", "b", "c", "d"]).await;
        let gone = |name: &'static str| {
            let uri = format!("/storage/v1/b/bucket/o/{name}");
            let harness = &harness;
            async move { harness.status("GET", &uri, b"").await == StatusCode::NOT_FOUND }
        };
        let response = execute(
            envelope(&[delete("a"), delete("b"), delete("c"), delete("d")]),
            harness.ctx(),
        )
        .await
        .unwrap();
        // The channel holds the first part and the second waits to enter it: nothing reads them.
        eventually_async("the second part to run", || gone("b")).await;
        assert!(!gone("c").await, "the third part ran without a reader");
        assert!(!gone("d").await, "the fourth part ran without a reader");
        let mut body = response.into_body();
        for _ in 0..4 {
            assert!(
                next_text(&mut body)
                    .await
                    .contains("HTTP/1.1 204 No Content")
            );
        }
        assert_eq!(next_text(&mut body).await, format!("--{BOUNDARY}--\r\n"));
        assert!(body.frame().await.is_none());
        assert!(gone("d").await);
        harness.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn a_departed_reader_cancels_the_remaining_parts() {
        let harness = Harness::start(Config::default()).await;
        harness.objects(&["a", "b", "c"]).await;
        let response = execute(
            envelope(&[delete("a"), delete("b"), delete("c")]),
            harness.ctx(),
        )
        .await
        .unwrap();
        let mut body = response.into_body();
        assert!(next_text(&mut body).await.contains("204"));
        drop(body);
        eventually_async("the part runner to stop", || {
            let running = Arc::strong_count(&harness.state) > 1;
            async move { !running }
        })
        .await;
        assert_eq!(
            harness.status("GET", "/storage/v1/b/bucket/o/c", b"").await,
            StatusCode::OK
        );
        harness.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn a_part_past_the_request_limit_answers_alone_and_later_parts_still_run() {
        let harness = Harness::start(Config {
            request_timeout: TimeLimit::from_millis(100),
            ..Config::default()
        })
        .await;
        harness.objects(&["a"]).await;
        let decoders = harness.state.pools.gzip_decoders.available_permits();
        let busy = harness
            .state
            .pools
            .gzip_decoders
            .clone()
            .acquire_many_owned(u32::try_from(decoders).unwrap())
            .await
            .unwrap();
        let response = execute(envelope(&[gzip_patch(), delete("a")]), harness.ctx())
            .await
            .unwrap();
        let mut body = response.into_body();
        let timed_out = next_text(&mut body).await;
        assert!(
            timed_out.contains("HTTP/1.1 503 Service Unavailable"),
            "{timed_out}"
        );
        assert!(timed_out.contains("request_timeout_ms"), "{timed_out}");
        drop(busy);
        assert!(next_text(&mut body).await.contains("204"));
        assert_eq!(next_text(&mut body).await, format!("--{BOUNDARY}--\r\n"));
        harness.state.catalog.close().await.unwrap();
    }

    /// The connection's limit on a silent response body measures the client, and a client cannot
    /// speed up a part, so a part slower than that limit must not close the connection.
    #[tokio::test]
    async fn a_slow_part_keeps_the_connection_under_a_body_inactivity_limit() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = TestDir::new();
        let state = Arc::new(State::for_test(
            Catalog::open(&dir.0).await.unwrap(),
            Auth::permissive(),
        ));
        let decoders = state.pools.gzip_decoders.available_permits();
        let busy = state
            .pools
            .gzip_decoders
            .clone()
            .acquire_many_owned(u32::try_from(decoders).unwrap())
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let config = Config {
            body_inactivity_timeout: TimeLimit::from_millis(100),
            ..Config::default()
        };
        let server = tokio::spawn(super::super::serve_with_tls(
            vec![(listener, None)],
            config,
            state.clone(),
            async move { stopped.await.map_err(io::Error::other) },
        ));
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        let body = envelope(&[gzip_patch()]).into_body();
        let body = body.collect().await.unwrap().to_bytes();
        let head = format!(
            "POST /batch/storage/v1 HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/mixed; boundary=test\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        client.write_all(head.as_bytes()).await.unwrap();
        client.write_all(&body).await.unwrap();
        let mut received = Vec::new();
        let mut chunk = [0; 4096];
        while !received.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = client.read(&mut chunk).await.unwrap();
            assert_ne!(count, 0, "closed before the response head");
            received.extend_from_slice(&chunk[..count]);
        }
        assert!(received.starts_with(b"HTTP/1.1 200 OK\r\n"));
        // Several times the inactivity limit passes while the part waits for its decoder.
        assert!(
            tokio::time::timeout(Duration::from_millis(500), client.read(&mut chunk))
                .await
                .is_err(),
            "the connection moved while the part was waiting"
        );
        drop(busy);
        let closing = format!("--{BOUNDARY}--\r\n");
        while !received
            .windows(closing.len())
            .any(|window| window == closing.as_bytes())
        {
            let count = client.read(&mut chunk).await.unwrap();
            assert_ne!(count, 0, "closed before the closing delimiter");
            received.extend_from_slice(&chunk[..count]);
        }
        drop(client);
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
        state.catalog.close().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_part_expires_after_silence_but_not_during_a_catalog_wait() {
        const LIMIT: u64 = 1000;
        let activity = super::super::new_batch_part_activity();
        let expired = super::super::batch_part_expired(&activity, TimeLimit::from_millis(LIMIT));
        tokio::pin!(expired);
        let almost = Duration::from_millis(LIMIT - 1);
        assert!(tokio::time::timeout(almost, &mut expired).await.is_err());
        // Progress renews the limit.
        super::super::record_progress(&activity);
        assert!(tokio::time::timeout(almost, &mut expired).await.is_err());
        {
            let _wait = super::super::begin_catalog_wait(&activity).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(100 * LIMIT), &mut expired)
                    .await
                    .is_err()
            );
        }
        assert!(tokio::time::timeout(almost, &mut expired).await.is_err());
        let expiry = tokio::time::timeout(Duration::from_millis(2), &mut expired)
            .await
            .unwrap();
        assert_eq!(expiry, super::super::Expiry::Request);
    }

    #[tokio::test(start_paused = true)]
    async fn an_enforced_object_deadline_expires_a_part_despite_progress() {
        let activity = super::super::new_batch_part_activity();
        let deadline = TimeLimit::from_millis(1500).deadline();
        activity.send_modify(|progress| progress.deadline = Some(deadline));
        let expired = super::super::batch_part_expired(&activity, TimeLimit::from_millis(1000));
        tokio::pin!(expired);
        assert!(
            tokio::time::timeout(Duration::from_millis(900), &mut expired)
                .await
                .is_err()
        );
        super::super::record_progress(&activity);
        assert!(
            tokio::time::timeout(Duration::from_millis(599), &mut expired)
                .await
                .is_err()
        );
        let expiry = tokio::time::timeout(Duration::from_millis(2), &mut expired)
            .await
            .unwrap();
        assert_eq!(expiry, super::super::Expiry::ObjectRequest);
        let reply = super::super::batch_part_timeout(expiry);
        let body = reply.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("object_request_timeout_ms"), "{body}");
        assert!(!body.contains("raise request_timeout_ms"), "{body}");
    }

    #[tokio::test]
    async fn batch_admission_bounds_envelopes_before_reading_the_body() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use tonic::codegen::tokio_stream::StreamExt;
        let dir = TestDir::new();
        let state = Arc::new(State::for_test(
            Catalog::open(&dir.0).await.unwrap(),
            Auth::permissive(),
        ));
        let config = Config::default();
        let endpoints = config
            .runtime_endpoints
            .snapshot(&config, "http://localhost")
            .unwrap()
            .without_request();
        let (activity, _) = watch::channel(ObjectActivity::default());
        let ctx = RequestCtx {
            state: &state,
            config: &config,
            endpoints: &endpoints,
            activity: &activity,
            listener: ListenerRole::Public,
        };
        let request = |read: Arc<AtomicBool>| {
            let frame = Ok::<_, Infallible>(Frame::data(Bytes::from_static(
                b"--test\r\nContent-Type: application/http\r\nContent-Transfer-Encoding: binary\r\n\r\nGET /storage/v1/b?project=local HTTP/1.1\r\n\r\n\r\n--test--\r\n",
            )));
            let stream = tonic::codegen::tokio_stream::iter([frame]).map(move |frame| {
                read.store(true, Ordering::Release);
                frame
            });
            Request::builder()
                .method("POST")
                .uri("/batch/storage/v1")
                .header("content-type", "multipart/mixed; boundary=test")
                .body(http_body_util::StreamBody::new(stream))
                .unwrap()
        };
        let permits = state
            .pools
            .batches
            .clone()
            .acquire_many_owned(2)
            .await
            .unwrap();
        let read = Arc::new(AtomicBool::new(false));
        let waiting = execute(request(read.clone()), ctx);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut waiting)
                .await
                .is_err()
        );
        assert!(!read.load(Ordering::Acquire));
        drop(permits);
        let response = waiting.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(read.load(Ordering::Acquire));
        drop(response);

        let short = Config {
            object_request_timeout: crate::time_limit::TimeLimit::from_millis(1),
            ..config.clone()
        };
        let permits = state
            .pools
            .batches
            .clone()
            .acquire_many_owned(2)
            .await
            .unwrap();
        let read = Arc::new(AtomicBool::new(false));
        assert_eq!(
            execute(
                request(read.clone()),
                RequestCtx {
                    config: &short,
                    ..ctx
                }
            )
            .await
            .err()
            .unwrap()
            .status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(!read.load(Ordering::Acquire));
        drop(permits);
        state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn batch_response_holds_capacity_until_consumed_or_dropped() {
        let pool = Arc::new(Semaphore::new(1));
        for consume in [true, false] {
            let admission = pool.clone().acquire_owned().await.unwrap();
            let (sender, completion, body) = chunk_body::checked_channel("ended early");
            sender
                .try_send(Ok(Bytes::from_static(b"response")))
                .unwrap();
            completion.mark();
            drop(sender);
            let body = BatchBody {
                body,
                _admission: admission,
            };
            assert!(pool.clone().try_acquire_owned().is_err());
            if consume {
                assert_eq!(body.collect().await.unwrap().to_bytes(), b"response"[..]);
            } else {
                drop(body);
            }
            assert_eq!(pool.available_permits(), 1);
        }
    }

    #[test]
    fn response_content_type_matches_the_body_boundary() {
        assert_eq!(
            RESPONSE_CONTENT_TYPE.to_str().unwrap(),
            format!("multipart/mixed; boundary={BOUNDARY}")
        );
    }

    #[test]
    fn parser_worker_failures_are_retryable_server_errors() {
        for (error, message) in [
            (parser_failed(), "Batch parser failed"),
            (parser_unavailable(), "Batch parser unavailable"),
        ] {
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(error.reason, "backendError");
            assert_eq!(error.message, message);
        }
    }

    #[tokio::test]
    async fn inner_body_failures_are_retryable_part_errors() {
        for (case, frame, message) in [
            (
                "stream",
                Err(std::io::Error::other("private body failure")),
                "Batched response stream failed",
            ),
            (
                "trailers",
                Ok(hyper::body::Frame::trailers(HeaderMap::new())),
                "Batched response trailers are unsupported",
            ),
        ] {
            let body = http_body_util::StreamBody::new(tonic::codegen::tokio_stream::iter([frame]));
            let mut output = Vec::new();
            append_response(&mut output, None, Response::new(body.boxed_unsync()), 8192).await;
            let text = std::str::from_utf8(&output).expect(case);
            assert!(
                text.contains("\r\nHTTP/1.1 503 Service Unavailable\r\n"),
                "{case}: {text}"
            );
            let body = text.split("\r\n\r\n").nth(2).expect(case);
            let body: serde_json::Value = serde_json::from_str(body).expect(case);
            assert_eq!(body["error"]["code"], 503, "{case}");
            assert_eq!(
                body["error"]["errors"][0]["reason"], "backendError",
                "{case}"
            );
            assert_eq!(body["error"]["message"], message, "{case}");
        }
    }

    #[tokio::test]
    async fn oversized_part_responses_keep_complete_error_framing_and_content_ids() {
        let response = boxed(Response::new(Full::new(Bytes::from(vec![b'x'; 1024]))));
        let mut bytes = Vec::new();
        append_response(&mut bytes, Some("<bounded>"), response, 100).await;
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("Content-ID: <response-bounded>\r\n"));
        assert!(text.contains("HTTP/1.1 413 Payload Too Large\r\n"));
        let (head, body) = text.split_once("\r\n\r\nHTTP/").unwrap();
        assert!(head.starts_with("--gcs_mimic_batch_response\r\n"));
        let (head, body) = body.split_once("\r\n\r\n").unwrap();
        let length = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert_eq!(length, body.len() - 2);
        assert!(body.contains("responseTooLarge"));
        assert!(body.contains("operation may have completed"));
        let mut response = crate::server::empty_response(StatusCode::OK);
        response.headers_mut().insert(
            "x-header",
            hyper::header::HeaderValue::from_static("header-only"),
        );
        let mut output = Vec::new();
        append_response(&mut output, None, boxed(response), 0).await;
        assert!(
            std::str::from_utf8(&output)
                .unwrap()
                .contains("HTTP/1.1 413 Payload Too Large")
        );
    }
}
