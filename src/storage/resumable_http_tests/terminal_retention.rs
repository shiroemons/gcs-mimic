use super::AuthProfile::Permissive;
use super::Harness;
use crate::storage::test_fixtures::fill_cancelled_sessions;
use std::time::Duration;

/// How many finished records stay before the oldest gives way.
const WINDOW: i64 = 4096;

#[tokio::test]
async fn retained_completions_answer_status_and_final_retries_until_the_window_passes_them() {
    let h = Harness::start(1024, Permissive, None).await;
    let mut completed = Vec::new();
    for index in 0..3 {
        // Distinct initiation times give eviction a deterministic oldest-first order.
        h.clock.advance(Duration::from_millis(1));
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
    // Newer than the real sessions, with the third completion they take the window to one past
    // its size, so the next initiation trims exactly the oldest completion.
    fill_cancelled_sessions(&h.state.catalog, WINDOW - 2).await;
    assert_eq!(h.initiate("name=next", "", b"").await.status, 200);
    // Provider sessions answer status and a replayed final request with the object resource.
    for (path, resource) in [&completed[1], &completed[2]] {
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
    // The oldest result beyond the local retained-result window is no longer known.
    let reply = h
        .request("PUT", &completed[0].0, "Content-Range: bytes */10\r\n", b"")
        .await;
    assert_eq!(reply.status, 404);
    h.stop().await;
}
