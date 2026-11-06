use super::*;
use gcs_mimic::time_limit::TimeLimit;
use tokio::task::JoinSet;

/// Three times the catalog's admission slots; a request past them used to fail with 503.
const REQUESTS: usize = 96;

/// Completing a multipart upload copies its parts in detached work that commits whatever the
/// request reports, so past the catalog wait it must report that commit. A 503 here left a retry
/// with NoSuchUpload for an upload that had succeeded.
#[tokio::test]
async fn a_multipart_completion_past_the_catalog_wait_reports_its_commit() {
    const BUDGET_MILLIS: u64 = 500;
    let h = Harness::start_with_config(
        Permissive,
        None,
        Config {
            catalog_wait_timeout: TimeLimit::from_millis(BUDGET_MILLIS),
            ..Default::default()
        },
    )
    .await;
    let begun = h
        .request("POST", "/bucket/assembled?uploads", "", b"")
        .await;
    assert_eq!(begun.status, 200);
    let body = String::from_utf8(begun.body).unwrap();
    let id = body
        .split("<UploadId>")
        .nth(1)
        .and_then(|rest| rest.split("</UploadId>").next())
        .unwrap();
    let part = h
        .request(
            "PUT",
            &format!("/bucket/assembled?partNumber=1&uploadId={id}"),
            "",
            b"part",
        )
        .await;
    assert_eq!(part.status, 200);
    let manifest = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{}</ETag></Part></CompleteMultipartUpload>",
        part.headers["etag"]
    );
    eventually_async("the part request to release its lease", || async {
        !h.state.catalog.has_session_writers()
    })
    .await;
    let mut client = TcpStream::connect(&h.address).await.unwrap();
    client
        .write_all(
            format!(
                "POST /bucket/assembled?uploadId={id} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
                manifest.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    // The lease comes before the manifest, so the catalog stalls only the completion itself.
    eventually_async("the completion to lease its upload", || async {
        h.state.catalog.has_session_writers()
    })
    .await;
    let held = h.state.catalog.hold_admission();
    client.write_all(manifest.as_bytes()).await.unwrap();
    let mut reply = tokio::spawn(Harness::read_response(client));
    let past_the_budget = Duration::from_millis(2 * BUDGET_MILLIS);
    assert!(
        tokio::time::timeout(past_the_budget, &mut reply)
            .await
            .is_err(),
        "the completion waits for its outcome past the catalog wait"
    );
    drop(held);
    assert_eq!(reply.await.unwrap().status, 200);
    assert_eq!(
        h.request("GET", "/bucket/assembled", "", b"").await.status,
        200
    );
    h.stop().await;
}

fn upload_id(begun: Reply) -> String {
    assert_eq!(begun.status, 200);
    String::from_utf8(begun.body)
        .unwrap()
        .split("<UploadId>")
        .nth(1)
        .and_then(|rest| rest.split("</UploadId>").next())
        .unwrap()
        .to_owned()
}

/// Sends a part's head and the start of its body, then stops, as a slow client does.
async fn stalled_part(h: &Harness, path: &str, start: &[u8], rest: usize) -> TcpStream {
    let mut client = TcpStream::connect(&h.address).await.unwrap();
    let head = format!(
        "PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
        start.len() + rest
    );
    client.write_all(head.as_bytes()).await.unwrap();
    client.write_all(start).await.unwrap();
    client
}

/// Parallel parts from one upload are the normal case for SDK transfer managers. A slow part must
/// not make the others wait, let alone answer 503 once the catalog wait runs out.
#[tokio::test]
async fn a_part_succeeds_while_other_parts_of_its_upload_stay_stalled_on_the_wire() {
    let h = Harness::start_with_config(Permissive, None, Config::default()).await;
    let id = upload_id(h.request("POST", "/bucket/parallel?uploads", "", b"").await);
    let part = |number: u16| format!("/bucket/parallel?partNumber={number}&uploadId={id}");
    let mut first = stalled_part(&h, &part(1), b"one", 4).await;
    let mut second = stalled_part(&h, &part(2), b"two", 4).await;
    eventually_async("both parts to lease their numbers", || async {
        h.state.catalog.multipart_parts_writing(&id) == 2
    })
    .await;

    let third = h.request("PUT", &part(3), "", b"three").await;
    assert_eq!(third.status, 200);

    // A body still streaming stays out of the listing until it commits.
    let listed = h
        .request("GET", &format!("/bucket/parallel?uploadId={id}"), "", b"")
        .await;
    let listed = String::from_utf8(listed.body).unwrap();
    assert!(listed.contains("<PartNumber>3</PartNumber>"), "{listed}");
    assert!(!listed.contains("<PartNumber>1</PartNumber>"), "{listed}");
    let uploads = h.request("GET", "/bucket?uploads", "", b"").await;
    let uploads = String::from_utf8(uploads.body).unwrap();
    assert!(uploads.contains(&id), "{uploads}");

    first.write_all(b"-end").await.unwrap();
    second.write_all(b"-end").await.unwrap();
    assert_eq!(Harness::read_response(first).await.status, 200);
    assert_eq!(Harness::read_response(second).await.status, 200);
    let listed = h
        .request("GET", &format!("/bucket/parallel?uploadId={id}"), "", b"")
        .await;
    let listed = String::from_utf8(listed.body).unwrap();
    assert_eq!(listed.matches("<PartNumber>").count(), 3, "{listed}");
    h.stop().await;
}

/// Past the wait only what really conflicts with the stalled body answers the retryable 503, and
/// that request leaves nothing behind that holds back the next part.
#[tokio::test]
async fn only_a_conflicting_request_answers_503_past_the_catalog_wait() {
    let h = Harness::start_with_config(
        Permissive,
        None,
        Config {
            catalog_wait_timeout: TimeLimit::from_millis(2_000),
            ..Default::default()
        },
    )
    .await;
    let id = upload_id(h.request("POST", "/bucket/busy?uploads", "", b"").await);
    let part = |number: u16| format!("/bucket/busy?partNumber={number}&uploadId={id}");
    let mut stalled = stalled_part(&h, &part(1), b"one", 4).await;
    eventually_async("the part to lease its number", || async {
        h.state.catalog.multipart_parts_writing(&id) == 1
    })
    .await;

    let same_number = h.request("PUT", &part(1), "", b"retry").await;
    assert_eq!(same_number.status, 503);
    let manifest = b"<CompleteMultipartUpload></CompleteMultipartUpload>";
    let whole = h
        .request("POST", &format!("/bucket/busy?uploadId={id}"), "", manifest)
        .await;
    assert_eq!(whole.status, 503);
    assert!(
        String::from_utf8(whole.body)
            .unwrap()
            .contains("<Code>ServiceUnavailable</Code>")
    );
    assert_eq!(h.request("PUT", &part(2), "", b"two").await.status, 200);

    stalled.write_all(b"-end").await.unwrap();
    assert_eq!(Harness::read_response(stalled).await.status, 200);
    h.stop().await;
}

#[tokio::test]
async fn a_part_disconnected_on_the_wire_gives_back_its_number_and_staged_bytes() {
    let h = Harness::start_with_config(Permissive, None, Config::default()).await;
    let id = upload_id(h.request("POST", "/bucket/cut?uploads", "", b"").await);
    let path = format!("/bucket/cut?partNumber=1&uploadId={id}");
    let client = stalled_part(&h, &path, b"start", 100).await;
    eventually_async("the part to lease its number", || async {
        h.state.catalog.multipart_parts_writing(&id) == 1
    })
    .await;
    drop(client);
    eventually_async("the disconnect to release the part", || async {
        !h.state.catalog.has_session_writers()
    })
    .await;
    assert_eq!(
        std::fs::read_dir(h.dir.0.join("staging")).unwrap().count(),
        0
    );
    assert_eq!(h.request("PUT", &path, "", b"retry").await.status, 200);
    h.stop().await;
}

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
