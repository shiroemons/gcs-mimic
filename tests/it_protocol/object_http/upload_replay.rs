use super::*;
use std::sync::{Arc, Barrier};

#[test]
fn upload_replay_unconditional_response_survives_overwrite_and_restart() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let token = "X-Goog-Gcs-Idempotency-Token: first\r\nContent-Type: text/plain\r\n";
    let query = "name=replay&projection=full";
    let original = server.upload(query, token, b"original");
    original.expect(200);
    assert_eq!(server.upload(query, token, b"original").body, original.body);
    server
        .upload(query, token, b"different")
        .json_error(400, "invalid");
    // The declared hash is not examined (recorded), so the retry replays the first response.
    assert_eq!(
        server
            .upload(
                query,
                &format!("{token}X-Goog-Hash: md5=AAAAAAAAAAAAAAAAAAAAAA==\r\n"),
                b"original",
            )
            .body,
        original.body
    );
    let replacement = server
        .upload(
            "name=replay",
            "X-Goog-Gcs-Idempotency-Token: second\r\n",
            b"new",
        )
        .json();
    assert_ne!(replacement["generation"], original.json()["generation"]);
    assert_eq!(server.upload(query, token, b"original").body, original.body);
    assert_eq!(server.get("replay").json(), replacement);
    server.stop();
    let server = Server::start(&dir, 1000, 5000);
    assert_eq!(server.upload(query, token, b"original").body, original.body);
    assert_eq!(server.get("replay").json(), replacement);
    assert_eq!(server.get("replay?alt=media").body, b"new");
}

#[test]
fn upload_replay_concurrent_requests_publish_one_generation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let barrier = Arc::new(Barrier::new(2));
    let replies = thread::scope(|scope| {
        let handles: Vec<_> = (0..2).map(|_| {
            let barrier = barrier.clone();
            let address = server.address.clone();
            scope.spawn(move || {
                let mut stream = TcpStream::connect(address).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                barrier.wait();
                stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=race HTTP/1.1\r\nHost: local\r\nContent-Length: 7\r\nX-Goog-Gcs-Idempotency-Token: race\r\n\r\npayload").unwrap();
                Reply::framed(stream)
            })
        }).collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    replies[0].expect(200);
    replies[1].expect(200);
    assert_eq!(replies[0].body, replies[1].body);
    assert_eq!(server.get("race").json(), replies[0].json());
    assert_eq!(common::process::staging_count(&dir), 0);
    assert_eq!(common::process::blob_count(&dir), 1);
}

#[test]
fn upload_replay_lost_response_and_recreated_bucket_do_not_duplicate_mutations() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let mut stream = server.stream();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=lost HTTP/1.1\r\nHost: local\r\nContent-Length: 7\r\nX-Goog-Gcs-Idempotency-Token: lost\r\n\r\npayload").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let original = loop {
        let reply = server.get("lost");
        if reply.status == 200 {
            break reply.json();
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    };
    // Discard the original response, then restart before retrying its invocation.
    drop(stream);
    server.stop();
    let server = Server::start(&dir, 1000, 5000);
    let token = "X-Goog-Gcs-Idempotency-Token: lost\r\n";
    assert_eq!(
        server.upload("name=lost", token, b"payload").json(),
        original
    );
    server
        .request("DELETE", "/storage/v1/b/test-bucket/o/lost", "", b"")
        .expect(204);
    server
        .request("DELETE", "/storage/v1/b/test-bucket", "", b"")
        .expect(204);
    server.bucket();
    server
        .upload("name=lost", token, b"payload")
        .json_error(400, "invalid");
    server.get("lost").json_error(404, "notFound");
}
