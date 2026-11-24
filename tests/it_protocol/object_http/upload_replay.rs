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
    let original = common::input_fixtures::eventually_some("the lost upload committed", || {
        let reply = server.get("lost");
        (reply.status == 200).then(|| reply.json())
    });
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

#[test]
fn upload_replay_token_reused_with_reordered_query_is_rejected_without_a_second_generation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, 1000, 5000);
    server.bucket();
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            "Content-Type: application/json\r\n",
            br#"{"versioning":{"enabled":true}}"#,
        )
        .expect(200);
    let token = "X-Goog-Gcs-Idempotency-Token: spelling\r\n";
    let upload = |query: &str| {
        server.request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?{query}"),
            token,
            b"payload",
        )
    };
    let first = upload("uploadType=media&name=x");
    first.expect(200);
    // An exact retry replays the recorded response.
    assert_eq!(
        upload("uploadType=media&name=x").json()["generation"],
        first.json()["generation"]
    );
    // The same parameters in another order are a different spelling of the request, so the
    // token is rejected instead of being treated as a retry or as a new publication.
    let reordered = upload("name=x&uploadType=media");
    reordered.json_error(400, "invalid");
    assert_eq!(
        reordered.json()["error"]["message"],
        "Idempotency token was reused for a different upload"
    );
    let versions = server
        .request("GET", "/storage/v1/b/test-bucket/o?versions=true", "", b"")
        .json();
    let items = versions["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{versions}");
    assert_eq!(items[0]["generation"], first.json()["generation"]);
}
