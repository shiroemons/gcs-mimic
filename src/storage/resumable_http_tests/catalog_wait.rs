use super::*;
use tokio::task::JoinSet;

/// Three times the catalog's admission slots; a request past them used to fail with 503.
const REQUESTS: usize = 96;

async fn saturable() -> Arc<Harness> {
    let config = Config {
        max_connections: 2 * REQUESTS,
        max_object_bytes: 1024,
        ..Default::default()
    };
    Arc::new(Harness::start_with_config(Permissive, None, config).await)
}

#[tokio::test]
async fn requests_past_the_catalog_slots_wait_and_all_succeed() {
    let h = saturable().await;
    let upload = "/upload/storage/v1/b/bucket/o?uploadType=media&name=object";
    assert_eq!(h.request("POST", upload, "", b"bytes").await.status, 200);
    let held = h.state.catalog.hold_admission();
    let mut requests = JoinSet::new();
    for index in 0..REQUESTS {
        let h = h.clone();
        requests.spawn(async move {
            match index % 3 {
                0 => {
                    let read = "/storage/v1/b/bucket/o/object?alt=media";
                    h.request("GET", read, "", b"").await
                }
                1 => {
                    h.request("GET", "/storage/v1/b/bucket/o/object", "", b"")
                        .await
                }
                _ => {
                    let upload = format!(
                        "/upload/storage/v1/b/bucket/o?uploadType=media&name=written-{index}"
                    );
                    h.request("POST", &upload, "", b"written").await
                }
            }
        });
    }
    eventually_async("every request to wait for a catalog slot", || async {
        h.state.catalog.admission_waiters() == REQUESTS
    })
    .await;
    drop(held);
    while let Some(reply) = requests.join_next().await {
        let reply = reply.unwrap();
        assert_eq!(
            reply.status,
            200,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
    }
    Arc::into_inner(h).unwrap().stop().await;
}

#[tokio::test]
async fn a_disconnected_client_stops_waiting_and_its_change_never_runs() {
    let h = saturable().await;
    let held = h.state.catalog.hold_admission();
    let body = br#"{"name":"abandoned"}"#;
    let mut client = TcpStream::connect(&h.address).await.unwrap();
    client
        .write_all(
            format!(
                "POST /storage/v1/b?project=project HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    client.write_all(body).await.unwrap();
    eventually_async("the insert to wait for a catalog slot", || async {
        h.state.catalog.admission_waiters() == 1
    })
    .await;
    drop(client);
    eventually_async("the disconnect to cancel the wait", || async {
        h.state.catalog.admission_waiters() == 0
    })
    .await;
    drop(held);
    assert_eq!(h.state.catalog.free_admission_slots(), 32);
    assert_eq!(
        h.request("GET", "/storage/v1/b/abandoned", "", b"")
            .await
            .status,
        404
    );
    Arc::into_inner(h).unwrap().stop().await;
}
