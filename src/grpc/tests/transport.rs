//! The connection driver on a decrypted TLS stream, and the supervised tasks of its streams.

use super::*;
use crate::tls::{Alpn, ServerIdentity};
use tokio_rustls::{TlsConnector, client::TlsStream, rustls::pki_types::ServerName};

/// Accepts one TLS connection the way the gRPC listener does: the handshake first, then HTTP/2
/// on the decrypted stream, whose preface deadline counts from the end of the handshake.
async fn start_tls(config: Config) -> (TlsStream<TcpStream>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = ServerIdentity::load(&Config::default())
        .unwrap()
        .acceptor(Alpn::H2)
        .unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = acceptor.accept(stream).await.unwrap();
        let (_stop, stopping) = watch::channel(false);
        serve_accepted(
            stream,
            &config,
            stopping,
            &Arc::<SlotReclaim>::default(),
            pending_stream,
        )
        .await;
    });
    let mut client = (*crate::tls::development_client_config().unwrap()).clone();
    client.alpn_protocols = vec![b"h2".to_vec()];
    let socket = TcpStream::connect(address).await.unwrap();
    let client = TlsConnector::from(Arc::new(client))
        .connect(ServerName::try_from("localhost").unwrap(), socket)
        .await
        .unwrap();
    (client, task)
}

fn tls_config() -> Config {
    Config {
        keep_alive_timeout: TimeLimit::from_millis(60_000),
        ..config()
    }
}

/// The handshake reads far more than the 24 bytes of the HTTP/2 preface from the socket, yet
/// only decrypted bytes count, so a client that sends no preface over TLS is closed at the
/// preface deadline instead of idling under the keep-alive limit.
#[tokio::test]
async fn grpc_tls_handshake_bytes_never_pass_for_the_preface() {
    let (_client, task) = start_tls(tls_config()).await;
    timeout(DEADLINE * 5, task).await.unwrap().unwrap();
}

/// A preface sent over TLS counts, so the connection outlives the preface deadline and stays
/// idle under keep-alive while pings check the peer.
#[tokio::test]
async fn grpc_tls_preface_counts_decrypted_bytes() {
    let (mut client, task) = start_tls(tls_config()).await;
    handshake(&mut client).await;
    let kinds = frames_for(&mut client, DEADLINE * 4).await;
    assert!(
        !kinds.contains(&7),
        "idle TLS connection got GOAWAY: {kinds:?}"
    );
    assert!(kinds.contains(&6), "no keepalive ping: {kinds:?}");
    assert!(!task.is_finished());
}

/// Opens stream `stream` for `path` with a complete request.
async fn request_path(client: &mut TcpStream, stream: u32, path: &[u8]) {
    let mut headers = vec![0x83, 0x86, 0x04, path.len() as u8];
    headers.extend_from_slice(path);
    send_frame(client, 1, 5, stream, &headers).await;
}

/// The kind of the first frame that arrives for each of `streams`, acknowledging what asks for
/// it on the way.
async fn first_frames(client: &mut TcpStream, streams: [u32; 2]) -> [u8; 2] {
    let mut kinds = [0; 2];
    timeout(DEADLINE * 20, async {
        while kinds.contains(&0) {
            let mut header = [0; 9];
            client.read_exact(&mut header).await.unwrap();
            let length = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
            let mut data = vec![0; length];
            client.read_exact(&mut data).await.unwrap();
            let stream = u32::from_be_bytes(header[5..9].try_into().unwrap()) & 0x7fff_ffff;
            if let Some(index) = streams.iter().position(|wanted| *wanted == stream)
                && kinds[index] == 0
            {
                kinds[index] = header[3];
            }
            acknowledge(client, header[3], header[4], &data).await;
        }
    })
    .await
    .unwrap();
    kinds
}

/// A handler that panics is reported under the stream task's name and ends only its own stream:
/// a stream beside it on the same connection is answered, the connection keeps serving, and the
/// listener that accepted it serves a new connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_panicking_handler_is_reported_and_spares_other_streams_and_connections() {
    const PANIC: &str = "gRPC handler fixture panic";
    let config = Arc::new(Config {
        header_read_timeout: TimeLimit::from_millis(60_000),
        keep_alive_timeout: TimeLimit::from_millis(60_000),
        ..Config::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (_stop, stopping) = watch::channel(false);
    // Serves every connection it accepts in a task of its own, as the server's accept loop does.
    let accept = tokio::spawn(async move {
        let slots = Arc::new(SlotReclaim::default());
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (config, slots, stopping) = (config.clone(), slots.clone(), stopping.clone());
            tokio::spawn(async move {
                let service = |request: hyper::Request<tonic::body::Body>| async move {
                    if request.uri().path() == "/panic" {
                        panic!("{PANIC}");
                    }
                    Ok(hyper::Response::new(tonic::body::Body::empty()))
                };
                serve_accepted(stream, &config, stopping, &slots, service).await;
            });
        }
    });
    let mut client = TcpStream::connect(address).await.unwrap();
    handshake(&mut client).await;
    request_path(&mut client, 1, b"/panic").await;
    request_path(&mut client, 3, b"/").await;
    // The panicking stream is reset (RST_STREAM) and the other is answered (HEADERS).
    assert_eq!(first_frames(&mut client, [1, 3]).await, [3, 1]);
    eventually_async("the stream task panic to be reported", || async {
        crate::task::tests::reported_panics(STREAM_TASK)
            .iter()
            .any(|message| message.contains(PANIC))
    })
    .await;
    request_path(&mut client, 5, b"/").await;
    request_path(&mut client, 7, b"/").await;
    assert_eq!(first_frames(&mut client, [5, 7]).await, [1, 1]);
    let mut next = TcpStream::connect(address).await.unwrap();
    handshake(&mut next).await;
    request_path(&mut next, 1, b"/").await;
    request_path(&mut next, 3, b"/").await;
    assert_eq!(first_frames(&mut next, [1, 3]).await, [1, 1]);
    assert!(!accept.is_finished());
    accept.abort();
}
