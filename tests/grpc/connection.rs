use super::*;
use bytes::Bytes;
use http_body_util::channel::Channel;
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
    S: Fn(hyper::Request<Incoming>) -> F + Send + Sync + 'static,
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
        serve_with_service(server, &config, stopping, service).await;
    });
    (client, stop, task)
}

async fn send_frame(client: &mut TcpStream, kind: u8, flags: u8, stream: u32, data: &[u8]) {
    let length = data.len() as u32;
    let mut header = [0; 9];
    header[..3].copy_from_slice(&length.to_be_bytes()[1..]);
    header[3] = kind;
    header[4] = flags;
    header[5..].copy_from_slice(&stream.to_be_bytes());
    client.write_all(&header).await.unwrap();
    client.write_all(data).await.unwrap();
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

async fn frame(client: &mut TcpStream) -> Option<(u8, u8, Vec<u8>)> {
    let mut header = [0; 9];
    match client.read_exact(&mut header).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return None,
        Err(error) => panic!("HTTP/2 read failed: {error}"),
    }
    let length = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
    assert!(length <= 64 * 1024);
    let mut data = vec![0; length];
    client.read_exact(&mut data).await.unwrap();
    Some((header[3], header[4], data))
}

async fn acknowledge(client: &mut TcpStream, kind: u8, flags: u8, data: &[u8]) {
    if flags & 1 == 0 && matches!(kind, 4 | 6) {
        send_frame(client, kind, 1, 0, if kind == 6 { data } else { &[] }).await;
    }
}

async fn closed_with_goaway(client: &mut TcpStream) {
    timeout(DEADLINE * 10, async {
        let mut goaway = false;
        while let Some((kind, flags, data)) = frame(client).await {
            if kind == 7 {
                assert_eq!(&data[4..8], &[0; 4], "GOAWAY must have NO_ERROR");
                goaway = true;
            }
            acknowledge(client, kind, flags, &data).await;
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
    let (mut sender, body) = Channel::<Bytes, Infallible>::new(1);
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
                .send_data(Bytes::from_static(b"still streaming"))
                .await
                .unwrap();
        }
    }
    drop(sender);
    closed_with_goaway(&mut client).await;
    task.await.unwrap();
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
    let (count, receiver) = watch::channel(1);
    let (_sender, body) = Channel::<Bytes, Infallible>::new(1);
    let body = ResponseBody {
        body: tonic::body::Body::new(body),
        active: Some(StreamGuard(count)),
    };
    drop(body);
    assert_eq!(*receiver.borrow(), 0);
}
