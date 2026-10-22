#[allow(dead_code)]
mod common;

use common::TestDir;
use gcs_mimic::{
    bucket::BucketRecord,
    clock::Clock,
    json::resumable_cancel::cancel_response,
    object::UploadChunk,
    storage::{Catalog, ResumableError},
};
use http_body_util::BodyExt;
use hyper::{Request, body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use std::{
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

struct TestClock(Mutex<SystemTime>);
impl Clock for TestClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().unwrap()
    }
}

async fn setup(dir: &TestDir, clock: Arc<TestClock>) -> Catalog {
    let catalog = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    catalog
        .insert_bucket(BucketRecord {
            name: "bucket".into(),
            project: "project".into(),
            location: "US".into(),
            location_type: "multi-region".into(),
            storage_class: "STANDARD".into(),
            labels: Default::default(),
            metageneration: 1,
            time_created: UNIX_EPOCH,
            updated: UNIX_EPOCH,
        })
        .await
        .unwrap();
    catalog
}

async fn cancel_wire(catalog: &Catalog, id: &str, body: &[u8]) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let catalog = catalog.clone();
    let server_id = id.to_owned();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut connection = http1::Builder::new().serve_connection(
            TokioIo::new(stream),
            service_fn(move |request: Request<Incoming>| {
                let catalog = catalog.clone();
                let id = server_id.clone();
                async move {
                    let response = cancel_response(request, "bucket", &id, &catalog)
                        .await
                        .unwrap_or_else(|error| error.into_response());
                    Ok::<_, Infallible>(response.map(|body| body.boxed_unsync()))
                }
            }),
        );
        (&mut connection).await.unwrap();
        let mut stream = connection.into_parts().io.into_inner();
        stream.shutdown().await.unwrap();
        // Match the production server's bounded unread-body disposal after early rejection.
        let _ = tokio::time::timeout(
            Duration::from_millis(100),
            tokio::io::copy(&mut stream.take(256 * 1024), &mut tokio::io::sink()),
        )
        .await;
    });
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(format!("DELETE /upload/storage/v1/b/bucket/o?upload_id={id} HTTP/1.1\r\nHost: local\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    task.await.unwrap();
    std::str::from_utf8(&response)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn anonymous_cancel_removes_staging_and_terminal_errors_follow_original_deadline() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let (sender, body) = tokio::sync::mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(hyper::body::Bytes::from_static(
            b"old bytes",
        ))))
        .await
        .unwrap();
    sender.send(Ok(UploadChunk::End)).await.unwrap();
    drop(sender);
    let old = catalog
        .put_object(
            "bucket",
            "name",
            "text/plain".into(),
            Default::default(),
            Default::default(),
            1024,
            body,
        )
        .await
        .unwrap();
    let session = catalog
        .create_resumable_session("bucket", "name", Default::default(), None)
        .await
        .unwrap();
    let stage = dir.0.join("staging").join(&session.blob_id);
    assert_eq!(cancel_wire(&catalog, &session.id, b"nonempty").await, 400);
    assert!(stage.exists());
    assert_eq!(cancel_wire(&catalog, &session.id, b"").await, 499);
    assert!(!stage.exists());
    assert_eq!(
        catalog
            .get_object("bucket", "name", None, Default::default())
            .await
            .unwrap(),
        old
    );
    assert_eq!(cancel_wire(&catalog, &session.id, b"").await, 410);
    assert_eq!(
        catalog
            .resumable_session(&session.id, "bucket")
            .await
            .unwrap_err(),
        ResumableError::Gone
    );
    assert_eq!(cancel_wire(&catalog, &"f".repeat(64), b"").await, 404);
    *clock.0.lock().unwrap() = session.deadline;
    assert_eq!(cancel_wire(&catalog, &session.id, b"").await, 404);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn completed_session_cannot_be_cancelled_or_have_its_bytes_removed() {
    let dir = TestDir::new();
    let catalog = setup(&dir, Arc::new(TestClock(Mutex::new(UNIX_EPOCH)))).await;
    let session = catalog
        .create_resumable_session("bucket", "name", Default::default(), Some(0))
        .await
        .unwrap();
    let stage = dir.0.join("staging").join(&session.blob_id);
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap().execute("UPDATE resumable_sessions SET state = 'COMPLETE', completion = '{}', total = 0 WHERE id = ?1", [&session.id]).unwrap();
    assert_eq!(cancel_wire(&catalog, &session.id, b"").await, 410);
    assert!(stage.exists());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn cancel_obeys_writer_lease_and_only_one_cancellation_wins() {
    let dir = TestDir::new();
    let catalog = setup(&dir, Arc::new(TestClock(Mutex::new(UNIX_EPOCH)))).await;
    let session = catalog
        .create_resumable_session("bucket", "name", Default::default(), None)
        .await
        .unwrap();
    let lease = catalog
        .lock_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    assert!(matches!(
        catalog
            .cancel_resumable_session(&session.id, "bucket")
            .await,
        Err(ResumableError::Object(_))
    ));
    assert!(lease.staging_path().exists());
    drop(lease);
    let (a, b) = tokio::join!(
        catalog.cancel_resumable_session(&session.id, "bucket"),
        catalog.cancel_resumable_session(&session.id, "bucket")
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn expiry_is_fixed_at_seven_days_and_checkpoint_never_renews_it() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let session = catalog
        .create_resumable_session("bucket", "name", Default::default(), None)
        .await
        .unwrap();
    assert_eq!(session.deadline, UNIX_EPOCH + Duration::from_secs(604800));
    *clock.0.lock().unwrap() = session.deadline - Duration::from_secs(1);
    let mut lease = catalog
        .lock_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    catalog
        .checkpoint_resumable_session(&mut lease, 0, Some(1))
        .await
        .unwrap();
    assert_eq!(
        catalog
            .resumable_session(&session.id, "bucket")
            .await
            .unwrap()
            .deadline,
        session.deadline
    );
    *clock.0.lock().unwrap() = session.deadline;
    assert_eq!(
        catalog
            .resumable_session(&session.id, "bucket")
            .await
            .unwrap_err(),
        ResumableError::NotFound
    );
    assert!(
        catalog
            .checkpoint_resumable_session(&mut lease, 0, Some(1))
            .await
            .is_err()
    );
    drop(lease);
    assert_eq!(cancel_wire(&catalog, &session.id, b"").await, 404);
    *clock.0.lock().unwrap() += Duration::from_secs(1);
    assert_eq!(
        catalog
            .resumable_session(&session.id, "bucket")
            .await
            .unwrap_err(),
        ResumableError::NotFound
    );
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert!(!dir.0.join("staging").join(session.blob_id).exists());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn expiry_cleanup_runs_before_reopen_returns_and_releases_retained_slots() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    for _ in 0..128 {
        catalog
            .create_resumable_session("bucket", "name", Default::default(), None)
            .await
            .unwrap();
    }
    catalog.close().await.unwrap();
    *clock.0.lock().unwrap() = UNIX_EPOCH + Duration::from_secs(604800);
    let catalog = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    catalog
        .create_resumable_session("bucket", "new", Default::default(), None)
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn cleanup_failure_is_visible_durable_and_retryable_without_losing_the_slot() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let session = catalog
        .create_resumable_session("bucket", "name", Default::default(), None)
        .await
        .unwrap();
    let stage = dir.0.join("staging").join(&session.blob_id);
    std::fs::remove_file(&stage).unwrap();
    std::fs::create_dir(&stage).unwrap();
    assert!(
        catalog
            .cancel_resumable_session(&session.id, "bucket")
            .await
            .is_err()
    );
    assert!(catalog.resumable_maintenance_error().is_some());
    *clock.0.lock().unwrap() = session.deadline;
    assert!(catalog.maintain_resumable_sessions().await.is_err());
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT state FROM resumable_sessions WHERE id = ?1",
                [&session.id],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "CANCELLED"
    );
    catalog.close().await.unwrap();
    assert!(
        Catalog::open_with_clock(&dir.0, clock.clone())
            .await
            .is_err()
    );
    std::fs::remove_dir(&stage).unwrap();
    let catalog = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert!(catalog.resumable_maintenance_error().is_none());
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM resumable_sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn expiry_waits_for_writer_and_retries_after_lease_release() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let session = catalog
        .create_resumable_session("bucket", "name", Default::default(), None)
        .await
        .unwrap();
    let lease = catalog
        .lock_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    *clock.0.lock().unwrap() = session.deadline;
    assert!(catalog.maintain_resumable_sessions().await.is_err());
    assert!(lease.staging_path().exists());
    drop(lease);
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert!(!dir.0.join("staging").join(session.blob_id).exists());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn idle_scheduler_sleeps_and_new_sessions_rearm_due_cleanup() {
    struct CountingClock {
        now: Mutex<SystemTime>,
        calls: AtomicUsize,
    }
    impl Clock for CountingClock {
        fn now(&self) -> SystemTime {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.now.lock().unwrap()
        }
    }
    let dir = TestDir::new();
    let clock = Arc::new(CountingClock {
        now: Mutex::new(UNIX_EPOCH),
        calls: AtomicUsize::new(0),
    });
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while clock.calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let calls = clock.calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(clock.calls.load(Ordering::SeqCst), calls);
    catalog
        .insert_bucket(BucketRecord {
            name: "bucket".into(),
            project: "project".into(),
            location: "US".into(),
            location_type: "multi-region".into(),
            storage_class: "STANDARD".into(),
            labels: Default::default(),
            metageneration: 1,
            time_created: UNIX_EPOCH,
            updated: UNIX_EPOCH,
        })
        .await
        .unwrap();
    let first = catalog
        .create_resumable_session("bucket", "first", Default::default(), None)
        .await
        .unwrap();
    *clock.now.lock().unwrap() = first.deadline;
    catalog
        .create_resumable_session("bucket", "second", Default::default(), None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while dir.0.join("staging").join(&first.blob_id).exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    catalog.close().await.unwrap();
    let reopened = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    reopened.close().await.unwrap();
}
