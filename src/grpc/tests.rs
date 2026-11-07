use super::*;
use crate::{server::chunk_body, test_support::eventually_async};
use bytes::Bytes;
mod memory;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};

const DEADLINE: Duration = Duration::from_millis(150);

fn limit(duration: Duration) -> TimeLimit {
    TimeLimit::from_millis(duration.as_millis() as u64)
}

fn config() -> Config {
    Config {
        header_read_timeout: limit(DEADLINE),
        keep_alive_timeout: limit(DEADLINE),
        ..Config::default()
    }
}

async fn start<S, F>(service: S) -> (TcpStream, watch::Sender<bool>, JoinHandle<()>)
where
    S: Fn(hyper::Request<tonic::body::Body>) -> F + Send + Sync + 'static,
    F: Future<Output = Result<hyper::Response<tonic::body::Body>, Infallible>> + Send + 'static,
{
    start_with(config(), service).await
}

async fn start_with_bound<S, F>(
    bound: Duration,
    service: S,
) -> (TcpStream, watch::Sender<bool>, JoinHandle<()>)
where
    S: Fn(hyper::Request<tonic::body::Body>) -> F + Send + Sync + 'static,
    F: Future<Output = Result<hyper::Response<tonic::body::Body>, Infallible>> + Send + 'static,
{
    let config = Config {
        body_inactivity_timeout: limit(bound),
        ..config()
    };
    start_with(config, service).await
}

async fn start_with<S, F>(
    config: Config,
    service: S,
) -> (TcpStream, watch::Sender<bool>, JoinHandle<()>)
where
    S: Fn(hyper::Request<tonic::body::Body>) -> F + Send + Sync + 'static,
    F: Future<Output = Result<hyper::Response<tonic::body::Body>, Infallible>> + Send + 'static,
{
    start_sharing(config, Arc::default(), service).await
}

async fn start_sharing<S, F>(
    config: Config,
    slots: Arc<SlotReclaim>,
    service: S,
) -> (TcpStream, watch::Sender<bool>, JoinHandle<()>)
where
    S: Fn(hyper::Request<tonic::body::Body>) -> F + Send + Sync + 'static,
    F: Future<Output = Result<hyper::Response<tonic::body::Body>, Infallible>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, _) = listener.accept().await.unwrap();
    let (stop, stopping) = watch::channel(false);
    let task = tokio::spawn(async move {
        serve_with_service(server, &config, stopping, &slots, service).await;
    });
    (client, stop, task)
}

/// Reads frames for `wait`, acknowledging what asks for it, and returns the kinds seen.
async fn frames_for(client: &mut TcpStream, wait: Duration) -> Vec<u8> {
    let until = Instant::now() + wait;
    let mut kinds = Vec::new();
    while let Ok(Some((kind, flags, data))) = tokio::time::timeout_at(until, frame(client)).await {
        kinds.push(kind);
        acknowledge(client, kind, flags, &data).await;
    }
    kinds
}

/// A service whose every stream stays open without a response.
fn pending_stream(
    _: hyper::Request<tonic::body::Body>,
) -> std::future::Pending<Result<hyper::Response<tonic::body::Body>, Infallible>> {
    std::future::pending()
}

// One write per frame: a peer that already closed accepts the first write and resets the
// connection, so a frame split across writes would fail on its second half.
fn encode_frame(kind: u8, flags: u8, stream: u32, data: &[u8]) -> Vec<u8> {
    let length = data.len() as u32;
    let mut frame = Vec::with_capacity(9 + data.len());
    frame.extend_from_slice(&length.to_be_bytes()[1..]);
    frame.extend_from_slice(&[kind, flags]);
    frame.extend_from_slice(&stream.to_be_bytes());
    frame.extend_from_slice(data);
    frame
}

async fn send_frame(
    client: &mut (impl AsyncWriteExt + Unpin),
    kind: u8,
    flags: u8,
    stream: u32,
    data: &[u8],
) {
    client
        .write_all(&encode_frame(kind, flags, stream, data))
        .await
        .unwrap();
}

async fn handshake(client: &mut TcpStream) {
    client
        .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .await
        .unwrap();
    send_frame(client, 4, 0, 0, &[]).await;
}

async fn request(client: &mut TcpStream, stream: u32) {
    // Static HPACK indexes encode POST, http and a literal path of /.
    send_frame(client, 1, 5, stream, &[0x83, 0x86, 0x04, 1, b'/']).await;
}

async fn read_frame(
    client: &mut (impl AsyncReadExt + Unpin),
) -> std::io::Result<Option<(u8, u8, Vec<u8>)>> {
    let mut header = [0; 9];
    match client.read_exact(&mut header).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
    assert!(length <= 64 * 1024);
    let mut data = vec![0; length];
    client.read_exact(&mut data).await?;
    Ok(Some((header[3], header[4], data)))
}

async fn frame(client: &mut (impl AsyncReadExt + Unpin)) -> Option<(u8, u8, Vec<u8>)> {
    read_frame(client)
        .await
        .unwrap_or_else(|error| panic!("HTTP/2 read failed: {error}"))
}

fn acknowledgement(kind: u8, flags: u8, data: &[u8]) -> Option<Vec<u8>> {
    (flags & 1 == 0 && matches!(kind, 4 | 6))
        .then(|| encode_frame(kind, 1, 0, if kind == 6 { data } else { &[] }))
}

async fn acknowledge(client: &mut (impl AsyncWriteExt + Unpin), kind: u8, flags: u8, data: &[u8]) {
    if let Some(frame) = acknowledgement(kind, flags, data) {
        client.write_all(&frame).await.unwrap();
    }
}

fn peer_closed(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
    )
}

async fn closed_with_goaway(client: &mut TcpStream) {
    timeout(DEADLINE * 10, async {
        let mut goaway = false;
        loop {
            let (kind, flags, data) = match read_frame(client).await {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                // Acknowledging a keepalive PING read just before GOAWAY can reach a peer that
                // has already closed, which resets the connection after GOAWAY was delivered.
                Err(error) if goaway && peer_closed(&error) => break,
                Err(error) => panic!("HTTP/2 read failed: {error}"),
            };
            if kind == 7 {
                assert_eq!(&data[4..8], &[0; 4], "GOAWAY must have NO_ERROR");
                goaway = true;
            }
            if let Some(frame) = acknowledgement(kind, flags, &data)
                && let Err(error) = client.write_all(&frame).await
            {
                assert!(peer_closed(&error), "HTTP/2 write failed: {error}");
            }
        }
        assert!(goaway, "the peer must receive GOAWAY before EOF");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn grpc_idle_connection_closes_with_goaway() {
    let (mut client, _stop, task) =
        start(|_| async { Ok(hyper::Response::new(tonic::body::Body::empty())) }).await;
    handshake(&mut client).await;
    closed_with_goaway(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn grpc_idle_never_cuts_in_flight_streams() {
    let release = Arc::new(Notify::new());
    let (sender, body) = chunk_body::channel();
    let body = Arc::new(std::sync::Mutex::new(Some(body)));
    let entered = Arc::new(Notify::new());
    let (mut client, _stop, task) = start({
        let release = release.clone();
        let entered = entered.clone();
        move |_| {
            let release = release.clone();
            let entered = entered.clone();
            let body = body.clone();
            async move {
                entered.notify_one();
                release.notified().await;
                Ok(hyper::Response::new(tonic::body::Body::new(
                    body.lock().unwrap().take().unwrap(),
                )))
            }
        }
    })
    .await;
    handshake(&mut client).await;
    request(&mut client, 1).await;
    timeout(DEADLINE * 5, entered.notified()).await.unwrap();
    // Cover both the pending response future and the live response body.
    for phase in 0..2 {
        let kinds = frames_for(&mut client, DEADLINE * 3).await;
        assert!(!kinds.contains(&7), "open stream got GOAWAY: {kinds:?}");
        assert!(!task.is_finished());
        if phase == 0 {
            release.notify_one();
            sender
                .send(Ok(Bytes::from_static(b"still streaming")))
                .await
                .unwrap();
        }
    }
    drop(sender);
    closed_with_goaway(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn grpc_silent_open_stream_is_bounded() {
    for pending_response in [true, false] {
        let (_sender, body) = chunk_body::channel();
        let body = Arc::new(std::sync::Mutex::new(Some(body)));
        // Keepalive PINGs repeat every DEADLINE, so a bound between two of them keeps the
        // silence close from coinciding with a PING that the client must acknowledge.
        let bound = DEADLINE * 7 / 2;
        let (mut client, _stop, task) = start_with_bound(bound, move |_| {
            let body = body.clone();
            async move {
                if pending_response {
                    std::future::pending::<()>().await;
                }
                Ok(hyper::Response::new(tonic::body::Body::new(
                    body.lock().unwrap().take().unwrap(),
                )))
            }
        })
        .await;
        handshake(&mut client).await;
        request(&mut client, 1).await;
        // PING acknowledgements keep the peer alive, but must not renew the silence bound.
        let started = Instant::now();
        closed_with_goaway(&mut client).await;
        assert!(started.elapsed() >= bound);
        timeout(DEADLINE * 2, task).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn grpc_active_stream_survives_past_bound() {
    for incoming in [true, false] {
        let closing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let service_closing = closing.clone();
        let (sender, body) = chunk_body::channel();
        let body = Arc::new(std::sync::Mutex::new(Some(body)));
        let (mut client, stop, task) = start_with_bound(DEADLINE * 3, move |mut request| {
            let body = body.clone();
            let closing = service_closing.clone();
            async move {
                if incoming {
                    while let Some(frame) = request.body_mut().frame().await {
                        if let Err(error) = frame {
                            assert!(
                                closing.load(std::sync::atomic::Ordering::Acquire),
                                "request body failed before client teardown: {error}"
                            );
                            break;
                        }
                    }
                }
                Ok(hyper::Response::new(tonic::body::Body::new(
                    body.lock().unwrap().take().unwrap(),
                )))
            }
        })
        .await;
        handshake(&mut client).await;
        send_frame(&mut client, 1, 4, 1, &[0x83, 0x86, 0x04, 1, b'/']).await;
        let (mut reader, mut writer) = client.into_split();
        let (ack, mut acks) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        let producer = tokio::spawn(async move {
            let mut tick = tokio::time::interval(DEADLINE);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        if incoming {
                            send_frame(&mut writer, 0, 0, 1, b"progress").await;
                        } else {
                            sender.send(Ok(Bytes::from_static(b"progress"))).await.unwrap();
                        }
                    }
                    Some(data) = acks.recv() => {
                        send_frame(&mut writer, 6, 1, 0, &data).await;
                    }
                }
            }
        });
        let until = Instant::now() + DEADLINE * 7;
        while let Ok(next) = tokio::time::timeout_at(until, frame(&mut reader)).await {
            let (kind, flags, data) = next.expect("active stream must remain connected");
            assert_ne!(kind, 7, "progress must prevent silence GOAWAY");
            if kind == 6 && flags & 1 == 0 {
                ack.send(data).await.unwrap();
            }
        }
        assert!(!task.is_finished());
        closing.store(true, std::sync::atomic::Ordering::Release);
        producer.abort();
        let _ = producer.await;
        stop.send_replace(true);
        timeout(DEADLINE * 5, task).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn grpc_dead_peer_is_detected_by_keepalive() {
    let (mut client, _stop, task) = start(|_| async {
        std::future::pending::<Result<hyper::Response<tonic::body::Body>, Infallible>>().await
    })
    .await;
    handshake(&mut client).await;
    request(&mut client, 1).await;
    timeout(DEADLINE * 5, async {
        loop {
            let (kind, flags, data) = frame(&mut client).await.unwrap();
            if kind == 6 {
                assert_eq!(flags, 0, "server must probe the open stream's peer");
                break;
            }
            acknowledge(&mut client, kind, flags, &data).await;
        }
    })
    .await
    .unwrap();
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
    // Hyper may send a final error GOAWAY before closing on keepalive failure.
    client.read_to_end(&mut Vec::new()).await.unwrap();
}

#[tokio::test]
async fn grpc_goaway_has_bounded_grace() {
    for stop_during_handshake in [false, true] {
        let (mut client, stop, task) = start(|_| async {
            std::future::pending::<Result<hyper::Response<tonic::body::Body>, Infallible>>().await
        })
        .await;
        if !stop_during_handshake {
            handshake(&mut client).await;
            request(&mut client, 1).await;
            sleep(DEADLINE / 4).await;
        }
        stop.send_replace(true);
        if !stop_during_handshake {
            closed_with_goaway(&mut client).await;
        }
        timeout(DEADLINE * 5, task).await.unwrap().unwrap();
    }
    // Idle shutdown must also stop a peer that never completes its preface.
    let (_client, _stop, task) =
        start(|_| async { Ok(hyper::Response::new(tonic::body::Body::empty())) }).await;
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
}

#[tokio::test]
async fn grpc_idle_connection_outlives_the_handshake_deadline() {
    let config = Config {
        keep_alive_timeout: TimeLimit::from_millis(60_000),
        ..config()
    };
    let (mut client, _stop, task) = start_with(config, pending_stream).await;
    handshake(&mut client).await;
    let kinds = frames_for(&mut client, DEADLINE * 4).await;
    assert!(!kinds.contains(&7), "idle connection got GOAWAY: {kinds:?}");
    // The ping wait stays on the header deadline, so pings still check the peer meanwhile.
    assert!(kinds.contains(&6), "no keepalive ping: {kinds:?}");
    assert!(!task.is_finished());
}

#[tokio::test]
async fn grpc_missing_preface_is_bounded_by_the_handshake_deadline_alone() {
    let config = Config {
        keep_alive_timeout: TimeLimit::from_millis(60_000),
        ..config()
    };
    let (_client, _stop, task) = start_with(config, pending_stream).await;
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
}

/// Zero must disable, not expire at once, and no value may overflow time arithmetic or reach
/// hyper as a zero interval. Dead peers then depend on TCP keep-alive.
#[tokio::test]
async fn grpc_disabled_and_largest_limits_keep_every_phase_open() {
    for value in [TimeLimit::DISABLED, TimeLimit::from_millis(u64::MAX)] {
        for open_stream in [true, false] {
            let config = Config {
                header_read_timeout: value,
                keep_alive_timeout: value,
                body_inactivity_timeout: value,
                shutdown_timeout: limit(DEADLINE),
                ..Config::default()
            };
            let (mut client, stop, mut task) = start_with(config, pending_stream).await;
            if open_stream {
                handshake(&mut client).await;
                request(&mut client, 1).await;
                let kinds = frames_for(&mut client, DEADLINE * 3).await;
                assert!(!kinds.contains(&7), "{value:?}: GOAWAY in {kinds:?}");
                assert!(!kinds.contains(&6), "{value:?}: PING in {kinds:?}");
            }
            // Without a preface the client stays silent: acknowledging the server's SETTINGS
            // would make the connection fail.
            let open = timeout(DEADLINE * 3, &mut task).await;
            assert!(open.is_err(), "{value:?} closed the connection");
            // Stopping still ends the connection after GOAWAY.
            stop.send_replace(true);
            if open_stream {
                closed_with_goaway(&mut client).await;
            }
            timeout(DEADLINE * 5, task).await.unwrap().unwrap();
        }
    }
}

/// The server's SETTINGS names the limit, that many streams run at once, and it refuses the next.
async fn assert_stream_limit(config: Config, limit: u32) {
    let entered = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counter = entered.clone();
    let (mut client, _stop, _task) = start_with(config, move |request| {
        counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        pending_stream(request)
    })
    .await;
    handshake(&mut client).await;
    let (kind, flags, settings) = frame(&mut client).await.unwrap();
    assert_eq!(
        (kind, flags),
        (4, 0),
        "the server speaks first with SETTINGS"
    );
    let advertised = settings
        .as_chunks::<6>()
        .0
        .iter()
        .find(|setting| setting[..2] == [0, 3])
        .map(|setting| u32::from_be_bytes(setting[2..].try_into().unwrap()));
    assert_eq!(advertised, Some(limit));
    acknowledge(&mut client, kind, flags, &settings).await;
    for stream in 0..=limit {
        request(&mut client, 2 * stream + 1).await;
    }
    timeout(DEADLINE * 20, async {
        loop {
            let (kind, flags, data) = frame(&mut client).await.unwrap();
            if kind == 3 {
                assert_eq!(data, [0, 0, 0, 7], "the stream past the limit is refused");
                break;
            }
            acknowledge(&mut client, kind, flags, &data).await;
        }
    })
    .await
    .unwrap();
    eventually_async("the streams within the limit to start", || {
        let entered = entered.clone();
        async move { entered.load(std::sync::atomic::Ordering::Acquire) == limit }
    })
    .await;
}

#[tokio::test]
async fn grpc_stream_limit_defaults_to_100_streams_per_connection() {
    let config = Config {
        // No keepalive ping can fail while the test writes every stream.
        header_read_timeout: TimeLimit::from_millis(60_000),
        keep_alive_timeout: TimeLimit::from_millis(60_000),
        ..Config::default()
    };
    assert_stream_limit(config, 100).await;
}

#[tokio::test]
async fn grpc_stream_limit_follows_the_setting() {
    let config = Config {
        header_read_timeout: TimeLimit::from_millis(60_000),
        keep_alive_timeout: TimeLimit::from_millis(60_000),
        max_grpc_streams: std::num::NonZeroU32::new(2).unwrap(),
        ..Config::default()
    };
    assert_stream_limit(config, 2).await;
}

type Reply =
    Pin<Box<dyn Future<Output = Result<hyper::Response<tonic::body::Body>, Infallible>> + Send>>;

/// A service that answers at once, except that `/hold` stays open until `release` fires.
#[derive(Clone, Default)]
struct HoldGate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl HoldGate {
    fn service(
        &self,
    ) -> impl Fn(hyper::Request<tonic::body::Body>) -> Reply + Send + Sync + 'static {
        let gate = self.clone();
        move |request| {
            let gate = gate.clone();
            Box::pin(async move {
                if request.uri().path() == "/hold" {
                    gate.entered.notify_one();
                    gate.release.notified().await;
                }
                Ok(hyper::Response::new(tonic::body::Body::empty()))
            })
        }
    }
}

/// A connection whose only closing reason in these tests is a client waiting for its slot.
fn reclaim_config() -> Config {
    Config {
        keep_alive_timeout: TimeLimit::from_millis(60_000),
        ..config()
    }
}

async fn hold(client: &mut TcpStream, gate: &HoldGate, stream: u32) {
    // Static HPACK indexes encode POST and http, followed by a literal path of /hold.
    let headers = [0x83, 0x86, 0x04, 5, b'/', b'h', b'o', b'l', b'd'];
    send_frame(client, 1, 5, stream, &headers).await;
    timeout(DEADLINE * 20, gate.entered.notified())
        .await
        .unwrap();
}

/// Reads frames, acknowledging what asks for it, until the first of `kind` and returns it.
async fn frame_of_kind(client: &mut TcpStream, wanted: u8) -> Vec<u8> {
    timeout(DEADLINE * 20, async {
        loop {
            let (kind, flags, data) = frame(client).await.expect("the connection stays open");
            if kind == wanted {
                return data;
            }
            acknowledge(client, kind, flags, &data).await;
        }
    })
    .await
    .unwrap()
}

/// Completes one stream on a new connection, and leaves it idle in the line behind any other
/// connection that a test made idle before it.
async fn idle_after_one_stream(client: &mut TcpStream) {
    handshake(client).await;
    request(client, 1).await;
    frame_of_kind(client, 1).await;
    let kinds = frames_for(client, DEADLINE / 2).await;
    assert!(!kinds.contains(&7), "GOAWAY in {kinds:?}");
}

#[tokio::test]
async fn grpc_idle_connection_gives_its_slot_to_a_waiting_client() {
    let slots = Arc::<SlotReclaim>::default();
    let gate = HoldGate::default();
    let (mut client, _stop, task) =
        start_sharing(reclaim_config(), slots.clone(), gate.service()).await;
    idle_after_one_stream(&mut client).await;
    let _waiting = slots.request();
    closed_with_goaway(&mut client).await;
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
    assert!(!slots.is_wanted(), "the connection took the request");
}

#[tokio::test]
async fn grpc_oldest_idle_connection_gives_way_first_and_only_one_per_client() {
    let slots = Arc::<SlotReclaim>::default();
    let gate = HoldGate::default();
    let (mut oldest, _stop, oldest_task) =
        start_sharing(reclaim_config(), slots.clone(), gate.service()).await;
    idle_after_one_stream(&mut oldest).await;
    let (mut younger, _stop, younger_task) =
        start_sharing(reclaim_config(), slots.clone(), gate.service()).await;
    idle_after_one_stream(&mut younger).await;
    let waiting = slots.request();
    closed_with_goaway(&mut oldest).await;
    timeout(DEADLINE * 5, oldest_task).await.unwrap().unwrap();
    drop(waiting);
    let kinds = frames_for(&mut younger, DEADLINE * 2).await;
    assert!(
        !kinds.contains(&7),
        "younger connection got GOAWAY: {kinds:?}"
    );
    assert!(!younger_task.is_finished());
    let _next = slots.request();
    closed_with_goaway(&mut younger).await;
    timeout(DEADLINE * 5, younger_task).await.unwrap().unwrap();
}

/// The wake also outlives a client that got its slot another way, and must not close anything.
#[tokio::test]
async fn grpc_wake_that_outlives_its_request_closes_nothing() {
    let slots = Arc::<SlotReclaim>::default();
    let gate = HoldGate::default();
    let (mut client, _stop, task) =
        start_sharing(reclaim_config(), slots.clone(), gate.service()).await;
    idle_after_one_stream(&mut client).await;
    drop(slots.request());
    let kinds = frames_for(&mut client, DEADLINE * 2).await;
    assert!(!kinds.contains(&7), "GOAWAY in {kinds:?}");
    assert!(!task.is_finished());
    // It went back in line, so a real request still reaches it.
    let _waiting = slots.request();
    closed_with_goaway(&mut client).await;
}

#[tokio::test]
async fn grpc_open_stream_is_not_reclaimed_until_it_ends() {
    let slots = Arc::<SlotReclaim>::default();
    let gate = HoldGate::default();
    let (mut client, _stop, task) =
        start_sharing(reclaim_config(), slots.clone(), gate.service()).await;
    idle_after_one_stream(&mut client).await;
    hold(&mut client, &gate, 3).await;
    let _waiting = slots.request();
    let kinds = frames_for(&mut client, DEADLINE * 3).await;
    assert!(!kinds.contains(&7), "open stream got GOAWAY: {kinds:?}");
    assert!(!task.is_finished());
    assert!(
        slots.is_wanted(),
        "the request stays for another connection"
    );
    // Ending the stream makes the connection idle, and the stored wake reaches it there.
    gate.release.notify_one();
    frame_of_kind(&mut client, 1).await;
    closed_with_goaway(&mut client).await;
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
    assert!(!slots.is_wanted());
}

#[tokio::test]
async fn grpc_stream_start_leaves_the_idle_line_for_the_next_connection() {
    let slots = Arc::<SlotReclaim>::default();
    let gate = HoldGate::default();
    let (mut oldest, _stop, oldest_task) =
        start_sharing(reclaim_config(), slots.clone(), gate.service()).await;
    idle_after_one_stream(&mut oldest).await;
    let (mut younger, _stop, younger_task) =
        start_sharing(reclaim_config(), slots.clone(), gate.service()).await;
    idle_after_one_stream(&mut younger).await;
    hold(&mut oldest, &gate, 3).await;
    let kinds = frames_for(&mut oldest, DEADLINE / 2).await;
    assert!(!kinds.contains(&7), "GOAWAY in {kinds:?}");
    let _waiting = slots.request();
    closed_with_goaway(&mut younger).await;
    timeout(DEADLINE * 5, younger_task).await.unwrap().unwrap();
    let kinds = frames_for(&mut oldest, DEADLINE * 2).await;
    assert!(!kinds.contains(&7), "busy connection got GOAWAY: {kinds:?}");
    assert!(!oldest_task.is_finished());
}

#[tokio::test]
async fn grpc_stream_start_passes_a_received_wake_to_the_next_in_line() {
    let slots = SlotReclaim::default();
    let mut oldest = ConnectionPhase::Idle {
        turn: slots.join_line(),
    };
    let mut younger = slots.join_line();
    let _waiting = slots.request();
    oldest.after_stream_activity(1, &slots);
    assert!(matches!(oldest, ConnectionPhase::Streaming));
    timeout(DEADLINE * 20, younger.reached()).await.unwrap();
    assert!(slots.take_request());
}

/// A stream the peer opens before reading GOAWAY was accepted. Cutting it at the idle grace would
/// fail a call the protocol promised to serve, just because a client waited for a slot.
#[tokio::test]
async fn grpc_stream_racing_the_reclaim_goaway_is_served_past_the_grace() {
    let slots = Arc::<SlotReclaim>::default();
    let gate = HoldGate::default();
    let (mut client, _stop, task) =
        start_sharing(reclaim_config(), slots.clone(), gate.service()).await;
    idle_after_one_stream(&mut client).await;
    let _waiting = slots.request();
    // The PING behind this GOAWAY stays unanswered, so the server still accepts the next stream,
    // as it does from a peer that has not read the GOAWAY yet.
    frame_of_kind(&mut client, 7).await;
    hold(&mut client, &gate, 3).await;
    frames_for(&mut client, DEADLINE * 3).await;
    assert!(!task.is_finished(), "the raced stream was cut");
    gate.release.notify_one();
    frame_of_kind_before_close(&mut client, 1).await;
    read_to_close(&mut client).await;
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
}

#[tokio::test]
async fn grpc_silent_stream_that_raced_the_reclaim_goaway_stays_bounded() {
    let slots = Arc::<SlotReclaim>::default();
    let gate = HoldGate::default();
    let config = Config {
        body_inactivity_timeout: limit(DEADLINE * 2),
        ..reclaim_config()
    };
    let (mut client, _stop, task) = start_sharing(config, slots.clone(), gate.service()).await;
    idle_after_one_stream(&mut client).await;
    let _waiting = slots.request();
    frame_of_kind(&mut client, 7).await;
    hold(&mut client, &gate, 3).await;
    read_to_close(&mut client).await;
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
}

/// Reads to the first frame of `wanted` and writes nothing, for a server that closes right after
/// sending it. An acknowledgement reaching the closed socket resets the connection, and Windows
/// then discards the frames not read yet.
async fn frame_of_kind_before_close(client: &mut TcpStream, wanted: u8) {
    timeout(DEADLINE * 20, async {
        while frame(client).await.expect("the connection stays open").0 != wanted {}
    })
    .await
    .unwrap();
}

/// Reads to the end of a connection whose GOAWAY the test already consumed.
async fn read_to_close(client: &mut TcpStream) {
    timeout(DEADLINE * 20, async {
        loop {
            match read_frame(client).await {
                Ok(Some((kind, flags, data))) => {
                    if let Some(frame) = acknowledgement(kind, flags, &data)
                        && let Err(error) = client.write_all(&frame).await
                    {
                        assert!(peer_closed(&error), "HTTP/2 write failed: {error}");
                    }
                }
                Ok(None) => break,
                Err(error) if peer_closed(&error) => break,
                Err(error) => panic!("HTTP/2 read failed: {error}"),
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn grpc_stopping_bounds_a_stream_that_raced_the_reclaim_goaway() {
    let slots = Arc::<SlotReclaim>::default();
    let gate = HoldGate::default();
    let (mut client, stop, task) =
        start_sharing(reclaim_config(), slots.clone(), gate.service()).await;
    idle_after_one_stream(&mut client).await;
    let _waiting = slots.request();
    frame_of_kind(&mut client, 7).await;
    hold(&mut client, &gate, 3).await;
    stop.send_replace(true);
    read_to_close(&mut client).await;
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
}

/// A silent peer cannot hold the slot for the longer `header_read_timeout_ms`, which would also
/// hold every client queued behind the waiting one.
#[tokio::test]
async fn grpc_reclaimed_connection_with_a_silent_peer_ends_within_the_short_grace() {
    let slots = Arc::<SlotReclaim>::default();
    let gate = HoldGate::default();
    let config = Config {
        header_read_timeout: TimeLimit::from_millis(60_000),
        ..reclaim_config()
    };
    let (mut client, _stop, task) = start_sharing(config, slots.clone(), gate.service()).await;
    idle_after_one_stream(&mut client).await;
    let _waiting = slots.request();
    frame_of_kind(&mut client, 7).await;
    // The client acknowledges nothing from here on, and pings stay far away on the long limit.
    timeout(DRAIN_GRACE * 3, task).await.unwrap().unwrap();
}

/// With more clients than slots, reclaiming each new connection at its preface would hand the
/// slot back and forth without any client ever finishing a call.
#[tokio::test]
async fn grpc_new_connection_keeps_its_slot_while_it_opens_its_first_stream() {
    let slots = Arc::<SlotReclaim>::default();
    let gate = HoldGate::default();
    let config = Config {
        header_read_timeout: TimeLimit::from_millis(60_000),
        keep_alive_timeout: TimeLimit::from_millis(60_000),
        ..Config::default()
    };
    let _waiting = slots.request();
    let (mut client, _stop, task) = start_sharing(config, slots.clone(), gate.service()).await;
    handshake(&mut client).await;
    let kinds = frames_for(&mut client, DEADLINE * 2).await;
    assert!(!kinds.contains(&7), "new connection got GOAWAY: {kinds:?}");
    assert!(slots.is_wanted());
    // Having served a call, it can give way like any other idle connection.
    request(&mut client, 1).await;
    frame_of_kind(&mut client, 1).await;
    closed_with_goaway(&mut client).await;
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
}

/// Pools connect ahead of their calls, so an unused connection still gives way after its grace.
#[tokio::test]
async fn grpc_connection_that_never_opens_a_stream_joins_the_idle_line() {
    let slots = Arc::<SlotReclaim>::default();
    let gate = HoldGate::default();
    let (mut client, _stop, task) =
        start_sharing(reclaim_config(), slots.clone(), gate.service()).await;
    handshake(&mut client).await;
    let _waiting = slots.request();
    closed_with_goaway(&mut client).await;
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
}

/// The grace before joining the line must not stretch a shorter keep-alive limit.
#[tokio::test]
async fn grpc_keep_alive_shorter_than_the_first_stream_grace_still_closes_on_time() {
    let config = Config {
        header_read_timeout: TimeLimit::from_millis(60_000),
        keep_alive_timeout: limit(DEADLINE),
        ..Config::default()
    };
    let (mut client, _stop, task) = start_with(config, pending_stream).await;
    handshake(&mut client).await;
    closed_with_goaway(&mut client).await;
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
}

#[test]
fn grpc_shutdown_grace_is_never_zero() {
    let peer_wait = Duration::from_secs(5);
    for (shutdown_ms, expected) in [
        (0, peer_wait),
        (u64::MAX, peer_wait),
        (20_000, peer_wait),
        (5_000, Duration::from_millis(2_500)),
        (1, Duration::from_micros(500)),
    ] {
        assert_eq!(
            shutdown_grace(peer_wait, TimeLimit::from_millis(shutdown_ms)),
            expected
        );
    }
}

#[test]
fn grpc_response_drop_releases_stream_count() {
    let (count, receiver) = watch::channel(Activity {
        streams: 1,
        last_frame: Instant::now(),
    });
    let (_sender, body) = chunk_body::channel();
    let body = ResponseBody {
        body: tonic::body::Body::new(body),
        active: Some(StreamGuard(count)),
    };
    drop(body);
    assert_eq!(receiver.borrow().streams, 0);
}

#[test]
fn upload_chunks_split_message_data_at_channel_chunk_size() {
    use super::upload::{CHUNK_BYTES as UPLOAD_CHUNK_BYTES, chunks as upload_chunks};
    let data = Bytes::from(
        (0..(2 * UPLOAD_CHUNK_BYTES + 3))
            .map(|i| i as u8)
            .collect::<Vec<_>>(),
    );
    let chunks: Vec<_> = upload_chunks(&data).collect();
    assert_eq!(
        chunks.iter().map(Bytes::len).collect::<Vec<_>>(),
        [UPLOAD_CHUNK_BYTES, UPLOAD_CHUNK_BYTES, 3]
    );
    assert_eq!(chunks.concat(), data);
    let exact = data.slice(..UPLOAD_CHUNK_BYTES);
    assert_eq!(
        upload_chunks(&exact).collect::<Vec<_>>(),
        std::slice::from_ref(&exact)
    );
    assert_eq!(upload_chunks(&Bytes::new()).count(), 0);
}
