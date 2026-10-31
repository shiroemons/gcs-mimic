use super::Harness;
use std::time::Duration;

#[tokio::test]
async fn retained_completions_answer_status_and_final_retries_after_the_active_cap() {
    let h = Harness::start(1024, false, None).await;
    let mut completed = Vec::new();
    for index in 0..130 {
        // Distinct initiation times give eviction a deterministic oldest-first order.
        *h.clock.0.lock().unwrap() += Duration::from_millis(1);
        let init = h.initiate(&format!("name=object{index}"), "", b"").await;
        assert_eq!(init.status, 200, "initiation {index}");
        let path = h.path(&init);
        let reply = h
            .request(
                "PUT",
                &path,
                "Content-Range: bytes 0-9/10\r\n",
                b"0123456789",
            )
            .await;
        assert_eq!(reply.status, 200);
        completed.push((path, reply.body));
    }
    // Provider sessions answer status and a replayed final request with the object resource.
    for (path, resource) in [&completed[1], &completed[129]] {
        for (range, body) in [
            ("bytes */10", b"".as_slice()),
            ("bytes */*", b"".as_slice()),
            ("bytes 0-9/10", b"0123456789".as_slice()),
        ] {
            let reply = h
                .request("PUT", path, &format!("Content-Range: {range}\r\n"), body)
                .await;
            assert_eq!(reply.status, 200, "{range}");
            assert_eq!(&reply.body, resource, "{range}");
        }
    }
    // The oldest result beyond the local retained-result cap is no longer known.
    let reply = h
        .request("PUT", &completed[0].0, "Content-Range: bytes */10\r\n", b"")
        .await;
    assert_eq!(reply.status, 404);
    h.stop().await;
}
