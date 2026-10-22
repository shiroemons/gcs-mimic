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
        atomic::{AtomicBool, AtomicUsize, Ordering},
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

async fn setup(dir: &TestDir, clock: Arc<impl Clock + 'static>) -> Catalog {
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
async fn cancel_reloads_terminal_state_when_transition_no_longer_matches() {
    struct TransitionClock {
        database: std::path::PathBuf,
        expire: bool,
        armed: AtomicBool,
        calls: AtomicUsize,
    }
    impl Clock for TransitionClock {
        fn now(&self) -> SystemTime {
            if !self.armed.load(Ordering::SeqCst) {
                return UNIX_EPOCH;
            }
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call > 0 && self.expire {
                return UNIX_EPOCH + Duration::from_secs(604800);
            }
            if call == 1 {
                // Inject a terminal transition after lease lookup and before cancellation's CAS.
                rusqlite::Connection::open(&self.database)
                    .unwrap()
                    .execute("UPDATE resumable_sessions SET state = 'CANCELLED'", [])
                    .unwrap();
            }
            UNIX_EPOCH
        }
    }
    for expire in [false, true] {
        let dir = TestDir::new();
        let clock = Arc::new(TransitionClock {
            database: dir.0.join("catalog.sqlite3"),
            expire,
            armed: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        });
        let catalog = setup(&dir, clock.clone()).await;
        let session = catalog
            .create_resumable_session("bucket", "name", Default::default(), None)
            .await
            .unwrap();
        clock.armed.store(true, Ordering::SeqCst);
        assert_eq!(
            catalog
                .cancel_resumable_session(&session.id, "bucket")
                .await
                .unwrap_err(),
            if expire {
                ResumableError::NotFound
            } else {
                ResumableError::Gone
            }
        );
        assert!(dir.0.join("staging").join(&session.blob_id).exists());
        catalog.maintain_resumable_sessions().await.unwrap();
        assert!(!dir.0.join("staging").join(&session.blob_id).exists());
        catalog.close().await.unwrap();
    }
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
    assert_eq!(cancel_wire(&catalog, &session.id, b"").await, 499);
    assert!(stage.is_dir());
    assert_eq!(cancel_wire(&catalog, &session.id, b"").await, 410);
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
    catalog.start_resumable_maintenance();
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

async fn prefix(catalog: &Catalog, data: &[u8]) -> gcs_mimic::storage::ResumableSession {
    let session = catalog
        .create_resumable_session("bucket", "name", Default::default(), None)
        .await
        .unwrap();
    let mut lease = catalog
        .lock_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    std::fs::write(lease.staging_path(), data).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(lease.staging_path())
        .unwrap()
        .sync_all()
        .unwrap();
    catalog
        .checkpoint_resumable_session(&mut lease, data.len() as u64, None)
        .await
        .unwrap();
    session
}

#[tokio::test]
async fn restart_discards_write_before_offset_commit_and_preserves_committed_prefix() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let session = prefix(&catalog, b"acknowledged").await;
    let stage = dir.0.join("staging").join(&session.blob_id);
    // Inject the disk state after a file write/sync but before the offset transaction.
    std::fs::write(&stage, b"acknowledged-uncommitted-tail").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&stage)
        .unwrap()
        .sync_all()
        .unwrap();
    catalog.close().await.unwrap();
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&stage).unwrap(), b"acknowledged");
    assert_eq!(
        catalog
            .resumable_session(&session.id, "bucket")
            .await
            .unwrap()
            .offset,
        12
    );
    let mut lease = catalog
        .lock_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    std::fs::write(&stage, b"acknowledged-more").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&stage)
        .unwrap()
        .sync_all()
        .unwrap();
    catalog
        .checkpoint_resumable_session(&mut lease, 17, None)
        .await
        .unwrap();
    drop(lease);
    catalog.close().await.unwrap();
    let catalog = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    assert_eq!(std::fs::read(&stage).unwrap(), b"acknowledged-more");
    assert_eq!(
        catalog
            .resumable_session(&session.id, "bucket")
            .await
            .unwrap()
            .offset,
        17
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn restart_restores_rename_before_commit_instead_of_deleting_acknowledged_bytes() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let session = prefix(&catalog, b"prefix").await;
    let stage = dir.0.join("staging").join(&session.blob_id);
    let shard = dir.0.join("blobs").join(&session.blob_id[..2]);
    std::fs::create_dir_all(&shard).unwrap();
    let blob = shard.join(&session.blob_id);
    // Inject publication's rename-before-Object/session-commit boundary.
    std::fs::rename(&stage, &blob).unwrap();
    catalog.close().await.unwrap();
    let catalog = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    assert_eq!(std::fs::read(&stage).unwrap(), b"prefix");
    assert!(!blob.exists());
    assert_eq!(
        catalog
            .resumable_session(&session.id, "bucket")
            .await
            .unwrap()
            .offset,
        6
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn restart_after_commit_preserves_completed_snapshot_and_live_bytes_past_session_expiry() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let (sender, body) = tokio::sync::mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(hyper::body::Bytes::from_static(
            b"published",
        ))))
        .await
        .unwrap();
    sender.send(Ok(UploadChunk::End)).await.unwrap();
    drop(sender);
    let object = catalog
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
    let session = prefix(&catalog, b"published").await;
    let shard = dir.0.join("blobs").join(&session.blob_id[..2]);
    std::fs::create_dir_all(&shard).unwrap();
    let blob = shard.join(&session.blob_id);
    std::fs::rename(dir.0.join("staging").join(&session.blob_id), &blob).unwrap();
    let completion = gcs_mimic::json::object_resource(
        &object,
        "http://local",
        gcs_mimic::json::Projection::NoAcl,
    )
    .unwrap();
    // Inject the durable state after the combined Object/COMPLETE transaction commits.
    let mut connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let transaction = connection.transaction().unwrap();
    transaction.execute("INSERT INTO pending_blob_deletions SELECT blob_id FROM objects WHERE bucket = 'bucket' AND name = 'name'", []).unwrap();
    transaction
        .execute(
            "UPDATE objects SET blob_id = ?1 WHERE bucket = 'bucket' AND name = 'name'",
            [&session.blob_id],
        )
        .unwrap();
    transaction.execute("UPDATE resumable_sessions SET state = 'COMPLETE', total = offset, completion = ?1 WHERE id = ?2", rusqlite::params![completion.to_string(), session.id]).unwrap();
    transaction.commit().unwrap();
    catalog.close().await.unwrap();
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    assert_eq!(
        catalog
            .resumable_session(&session.id, "bucket")
            .await
            .unwrap()
            .completion,
        Some(completion)
    );
    assert_eq!(cancel_wire(&catalog, &session.id, b"").await, 410);
    assert_eq!(std::fs::read(&blob).unwrap(), b"published");
    *clock.0.lock().unwrap() = session.deadline;
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert_eq!(std::fs::read(&blob).unwrap(), b"published");
    assert_eq!(
        catalog
            .get_object("bucket", "name", None, Default::default())
            .await
            .unwrap(),
        object
    );
    catalog.close().await.unwrap();
    let catalog = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    assert_eq!(std::fs::read(&blob).unwrap(), b"published");
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn missing_short_and_nonfile_acknowledged_prefixes_block_readiness() {
    for damage in ["missing", "short", "directory"] {
        let dir = TestDir::new();
        let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
        let catalog = setup(&dir, clock.clone()).await;
        let session = prefix(&catalog, b"prefix").await;
        catalog.close().await.unwrap();
        let stage = dir.0.join("staging").join(&session.blob_id);
        match damage {
            "missing" => std::fs::remove_file(&stage).unwrap(),
            "short" => std::fs::write(&stage, b"x").unwrap(),
            _ => {
                std::fs::remove_file(&stage).unwrap();
                std::fs::create_dir(&stage).unwrap();
            }
        }
        let error = match Catalog::open_with_clock(&dir.0, clock).await {
            Ok(_) => panic!("damaged prefix became ready"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("acknowledged upload prefix"),
            "{error}"
        );
        let offset: i64 = rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT offset FROM resumable_sessions WHERE id = ?1",
                [&session.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(offset, 6);
    }
}

#[tokio::test]
async fn recovery_pages_active_prefixes_and_reclaims_orphans_and_deferred_blobs() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let mut sessions = Vec::new();
    for _ in 0..33 {
        sessions.push(prefix(&catalog, b"prefix").await);
    }
    let cancelled = prefix(&catalog, b"cancelled").await;
    catalog
        .cancel_resumable_session(&cancelled.id, "bucket")
        .await
        .unwrap();
    catalog.close().await.unwrap();
    std::fs::write(
        dir.0.join("staging").join(&cancelled.blob_id),
        b"cancelled-crash-tail",
    )
    .unwrap();
    let orphan = "a".repeat(32);
    let deferred = "b".repeat(32);
    std::fs::write(dir.0.join("staging").join(&orphan), b"orphan").unwrap();
    for id in [&orphan, &deferred] {
        let shard = dir.0.join("blobs").join(&id[..2]);
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(shard.join(id), b"orphan").unwrap();
    }
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO pending_blob_deletions VALUES (?1)",
            [&deferred],
        )
        .unwrap();
    let catalog = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    assert_eq!(
        std::fs::read_dir(dir.0.join("staging")).unwrap().count(),
        33
    );
    for session in sessions {
        assert_eq!(
            std::fs::read(dir.0.join("staging").join(&session.blob_id)).unwrap(),
            b"prefix"
        );
        assert_eq!(
            catalog
                .resumable_session(&session.id, "bucket")
                .await
                .unwrap()
                .offset,
            6
        );
    }
    for id in [&orphan, &deferred] {
        assert!(!dir.0.join("blobs").join(&id[..2]).join(id).exists());
    }
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writer_lease_is_reacquired_after_drop_and_panic() {
    let dir = TestDir::new();
    let catalog = setup(&dir, Arc::new(TestClock(Mutex::new(UNIX_EPOCH)))).await;
    let session = prefix(&catalog, b"prefix").await;
    let lease = catalog
        .lock_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    drop(lease);
    let lease = catalog
        .lock_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    let panic = tokio::spawn(async move {
        let _lease = lease;
        panic!("lease unwind fixture");
    })
    .await
    .unwrap_err();
    assert!(panic.is_panic());
    drop(
        catalog
            .lock_resumable_session(&session.id, "bucket")
            .await
            .unwrap(),
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn recovery_rejects_ambiguous_active_publication_without_deleting_either_copy() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let session = prefix(&catalog, b"prefix").await;
    catalog.close().await.unwrap();
    let stage = dir.0.join("staging").join(&session.blob_id);
    let shard = dir.0.join("blobs").join(&session.blob_id[..2]);
    std::fs::create_dir_all(&shard).unwrap();
    let blob = shard.join(&session.blob_id);
    std::fs::copy(&stage, &blob).unwrap();
    assert!(Catalog::open_with_clock(&dir.0, clock).await.is_err());
    assert_eq!(std::fs::read(&stage).unwrap(), b"prefix");
    assert_eq!(std::fs::read(&blob).unwrap(), b"prefix");
}

#[tokio::test]
async fn scheduler_retries_failed_cleanup_and_clears_its_error_after_repair() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    catalog.start_resumable_maintenance();
    let session = catalog
        .create_resumable_session("bucket", "name", Default::default(), None)
        .await
        .unwrap();
    let stage = dir.0.join("staging").join(&session.blob_id);
    std::fs::remove_file(&stage).unwrap();
    std::fs::create_dir(&stage).unwrap();
    *clock.0.lock().unwrap() = session.deadline;
    assert!(catalog.maintain_resumable_sessions().await.is_err());
    assert!(catalog.resumable_maintenance_error().is_some());
    // The failure is repaired without explicitly invoking maintenance a second time.
    std::fs::remove_dir(&stage).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 = rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
                .unwrap()
                .query_row("SELECT count(*) FROM resumable_sessions", [], |row| {
                    row.get(0)
                })
                .unwrap();
            if count == 0 && catalog.resumable_maintenance_error().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn cleaned_cancelled_sessions_are_not_reclaimed_again_before_their_deadline() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock).await;
    let session = catalog
        .create_resumable_session("bucket", "cancelled", Default::default(), None)
        .await
        .unwrap();
    catalog
        .cancel_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM pending_blob_deletions WHERE blob_id = ?1",
                [&session.blob_id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let stage = dir.0.join("staging").join(&session.blob_id);
    // A second cleanup would fail here, proving that the successful journal is not replayed.
    std::fs::create_dir(&stage).unwrap();
    for _ in 0..3 {
        catalog
            .create_resumable_session("bucket", "new", Default::default(), None)
            .await
            .unwrap();
        assert_eq!(
            catalog.maintain_resumable_sessions().await.unwrap(),
            Some(session.deadline)
        );
        assert!(catalog.resumable_maintenance_error().is_none());
    }
    assert!(stage.is_dir());
    std::fs::remove_dir(stage).unwrap();
    catalog.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn recovery_rejects_symlink_prefix_without_changing_its_target() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let session = prefix(&catalog, b"prefix").await;
    catalog.close().await.unwrap();
    let stage = dir.0.join("staging").join(&session.blob_id);
    let target = dir.0.join("untouched");
    std::fs::write(&target, b"target-tail").unwrap();
    std::fs::remove_file(&stage).unwrap();
    std::os::unix::fs::symlink(&target, &stage).unwrap();
    assert!(Catalog::open_with_clock(&dir.0, clock).await.is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"target-tail");
}
