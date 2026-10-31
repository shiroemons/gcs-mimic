//! Pins the bound that lets `stream_upload` forward request body frames without copying.

use super::*;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

/// Raising the read buffer raises the memory each upload's channel can retain; review that
/// tradeoff (and `stream_upload`'s zero-copy forwarding) before changing this value.
#[test]
fn connection_read_buffer_stays_16_kib() {
    assert_eq!(CONNECTION_READ_BUFFER, 16 * 1024);
}

/// Serves one request through the production HTTP/1 builder and `stream_upload`, returning
/// the largest chunk the upload channel received and the total body size.
async fn largest_forwarded_chunk(head: &str, body: &[u8]) -> (usize, usize) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let service = service_fn(|request: Request<Incoming>| async move {
            let (sender, mut receiver) = mpsc::channel(2);
            let consumer = tokio::spawn(async move {
                let (mut largest, mut total) = (0, 0);
                while let Some(Ok(UploadChunk::Data(bytes))) = receiver.recv().await {
                    largest = largest.max(bytes.len());
                    total += bytes.len();
                }
                (largest, total)
            });
            let (activity, _progress) = watch::channel(ObjectActivity::default());
            stream_upload(
                RequestBody::Transport(request.into_body()),
                &sender,
                u64::MAX,
                &activity,
                None,
            )
            .await
            .unwrap();
            drop(sender);
            let (largest, total) = consumer.await.unwrap();
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                "{largest} {total}"
            )))))
        });
        http1_builder()
            .serve_connection(TokioIo::new(stream), service)
            .await
            .unwrap();
    });
    let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
    let mut request = head.as_bytes().to_vec();
    request.extend_from_slice(body);
    // One write hands the whole body to the kernel at once, so only hyper's buffer limits
    // the frame size the server observes.
    client.write_all(&request).await.unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    server.await.unwrap();
    let response = String::from_utf8(response).unwrap();
    let (_, result) = response.rsplit_once("\r\n\r\n").unwrap();
    let (largest, total) = result.split_once(' ').unwrap();
    (largest.parse().unwrap(), total.parse().unwrap())
}

/// The frame bound documented on `CONNECTION_READ_BUFFER`. A larger frame would mean each
/// forwarded chunk can retain more of hyper's buffer than the upload path accounts for.
const FRAME_BOUND: usize = 2 * CONNECTION_READ_BUFFER;

#[tokio::test]
async fn upload_frames_stay_below_twice_the_connection_read_buffer() {
    let body = vec![b'x'; 1024 * 1024];
    let head = format!(
        "POST /upload HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let (largest, total) = largest_forwarded_chunk(&head, &body).await;
    assert_eq!(total, body.len());
    assert!(
        (1..FRAME_BOUND).contains(&largest),
        "a {largest}-byte frame reaches the {FRAME_BOUND}-byte bound"
    );

    let mut chunked = format!("{:x}\r\n", body.len()).into_bytes();
    chunked.extend_from_slice(&body);
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    let head = "POST /upload HTTP/1.1\r\nHost: test\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n";
    let (largest, total) = largest_forwarded_chunk(head, &chunked).await;
    assert_eq!(total, body.len());
    assert!(
        (1..FRAME_BOUND).contains(&largest),
        "a {largest}-byte chunked frame reaches the {FRAME_BOUND}-byte bound"
    );
}
