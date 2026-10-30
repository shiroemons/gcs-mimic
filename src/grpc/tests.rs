use super::*;
use crate::server::chunk_body;
use bytes::Bytes;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};

const DEADLINE: Duration = Duration::from_millis(150);

async fn start<S, F>(service: S) -> (TcpStream, watch::Sender<bool>, JoinHandle<()>)
where
    S: Fn(hyper::Request<tonic::body::Body>) -> F + Send + Sync + 'static,
    F: Future<Output = Result<hyper::Response<tonic::body::Body>, Infallible>> + Send + 'static,
{
    start_with_bound(STREAM_INACTIVITY_TIMEOUT, service).await
}

async fn start_with_bound<S, F>(
    bound: Duration,
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
        let config = Config {
            connection_timeout_ms: DEADLINE.as_millis() as u64,
            ..Config::default()
        };
        serve_with_service(server, &config, stopping, bound, service).await;
    });
    (client, stop, task)
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
        let until = Instant::now() + DEADLINE * 3;
        while let Ok(Some((kind, flags, data))) =
            tokio::time::timeout_at(until, frame(&mut client)).await
        {
            assert_ne!(kind, 7, "open stream must not receive idle GOAWAY");
            acknowledge(&mut client, kind, flags, &data).await;
        }
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
        let (sender, body) = chunk_body::channel();
        let body = Arc::new(std::sync::Mutex::new(Some(body)));
        let (mut client, stop, task) = start_with_bound(DEADLINE * 3, move |mut request| {
            let body = body.clone();
            async move {
                if incoming {
                    while let Some(frame) = request.body_mut().frame().await {
                        frame.unwrap();
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
