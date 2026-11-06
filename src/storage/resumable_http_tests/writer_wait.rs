use super::*;

/// Starts a chunk on its own connection and sends only part of it, so the handler keeps the
/// session's writer until the connection ends.
async fn stalled_chunk(h: &Harness, path: &str) -> TcpStream {
    let id = path.split("upload_id=").nth(1).unwrap();
    let session = h
        .state
        .catalog
        .resumable_session(id, "bucket")
        .await
        .unwrap();
    let stage = session.blob_id.staging_path(&h.dir.0);
    let mut stream = TcpStream::connect(&h.address).await.unwrap();
    stream.write_all(format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 262144\r\nContent-Range: bytes 0-262143/*\r\n\r\n").as_bytes()).await.unwrap();
    stream.write_all(b"a").await.unwrap();
    eventually_async("the stalled writer to stage its first byte", || async {
        tokio::fs::metadata(&stage).await.unwrap().len() > 0
    })
    .await;
    stream
}

/// Serves with the writer wait the server configures at startup.
async fn with_writer_wait(catalog_wait_timeout_ms: u64) -> Arc<Harness> {
    let config = Config {
        max_object_bytes: 1024 * 1024,
        catalog_wait_timeout_ms,
        ..Default::default()
    };
    let h = Harness::start_with_config(Permissive, None, config.clone()).await;
    h.state.catalog.apply_session_limits(&config);
    Arc::new(h)
}

#[tokio::test]
async fn a_retry_waits_for_the_abandoned_chunk_and_then_resumes_the_session() {
    let h = with_writer_wait(60_000).await;
    let init = h.initiate("name=retried", "", b"").await;
    let path = h.path(&init);
    let stalled = stalled_chunk(&h, &path).await;
    let retry = {
        let (h, path) = (h.clone(), path.clone());
        tokio::spawn(async move {
            h.request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
                .await
        })
    };
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert!(!retry.is_finished(), "the retry waits for the writer");
    drop(stalled);
    let status = retry.await.unwrap();
    assert_eq!(status.status, 308);
    assert!(!status.headers.contains_key("range"));
    assert_eq!(
        h.request("PUT", &path, "Content-Range: bytes 0-4/5\r\n", b"bytes")
            .await
            .status,
        200
    );
    Arc::into_inner(h).unwrap().stop().await;
}

#[tokio::test]
async fn a_cancellation_waits_for_the_abandoned_chunk() {
    let h = with_writer_wait(60_000).await;
    let init = h.initiate("name=cancelled", "", b"").await;
    let path = h.path(&init);
    let stalled = stalled_chunk(&h, &path).await;
    let cancel = {
        let (h, path) = (h.clone(), path.clone());
        tokio::spawn(async move { h.request("DELETE", &path, "", b"").await })
    };
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert!(
        !cancel.is_finished(),
        "the cancellation waits for the writer"
    );
    drop(stalled);
    assert_eq!(cancel.await.unwrap().status, 499);
    Arc::into_inner(h).unwrap().stop().await;
}

#[tokio::test]
async fn a_writer_that_keeps_working_past_the_budget_reports_a_busy_session() {
    let h = with_writer_wait(100).await;
    let init = h.initiate("name=busy", "", b"").await;
    let path = h.path(&init);
    let stalled = stalled_chunk(&h, &path).await;
    let reply = h
        .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
        .await;
    assert_eq!(reply.status, 503);
    let message = reply.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(message.contains("upload session is busy"), "{message}");
    assert!(!message.contains("catalog wait timed out"), "{message}");
    drop(stalled);
    Arc::into_inner(h).unwrap().stop().await;
}
