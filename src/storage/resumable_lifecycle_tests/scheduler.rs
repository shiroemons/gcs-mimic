use super::*;
use crate::storage::catalog::fault;
#[cfg(unix)]
use crate::storage::catalog::fault::{Fault, Step};

struct CountingClock {
    now: Mutex<SystemTime>,
    calls: AtomicUsize,
}

impl CountingClock {
    fn new() -> Self {
        Self {
            now: Mutex::new(UNIX_EPOCH),
            calls: AtomicUsize::new(0),
        }
    }

    async fn start_and_wait_for_busy_pass(&self, catalog: &Catalog) {
        self.calls.store(0, Ordering::SeqCst);
        catalog.start_resumable_maintenance();
        // Creation leaves a notification: both initial passes must reach their future timer.
        eventually_async("both initial passes to reach their timer", || async {
            self.calls.load(Ordering::SeqCst) >= 4
        })
        .await;
    }
}

impl Clock for CountingClock {
    fn now(&self) -> SystemTime {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.now.lock().unwrap()
    }
}

fn count(catalog: &rusqlite::Connection, table: &str) -> i64 {
    let query = match table {
        "sessions" => "SELECT count(*) FROM resumable_sessions",
        "journal" => "SELECT count(*) FROM pending_blob_deletions",
        _ => unreachable!(),
    };
    catalog.query_row(query, [], |row| row.get(0)).unwrap()
}

#[tokio::test]
async fn expired_busy_session_wakes_scheduler_when_its_writer_is_released() {
    let dir = TestDir::new();
    let clock = Arc::new(CountingClock::new());
    let catalog = setup(&dir, clock.clone()).await;
    let session = catalog
        .create_resumable_session(
            "bucket",
            "expired",
            crate::storage::ResumableOptions::new(crate::bucket::BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    let lease = catalog
        .lock_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    *clock.now.lock().unwrap() = session.deadline;
    catalog
        .create_resumable_session(
            "bucket",
            "future",
            crate::storage::ResumableOptions::new(crate::bucket::BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    clock.start_and_wait_for_busy_pass(&catalog).await;
    assert_eq!(count(&connection, "sessions"), 2);
    assert!(lease.staging_path().exists());
    assert!(catalog.resumable_maintenance_error().is_none());
    let catalog_io = fault::catalog_thread_io(&dir.0);
    drop(lease);
    eventually_async("the released expired session to be removed", || async {
        count(&connection, "sessions") == 1
    })
    .await;
    assert!(!session.blob_id.staging_path(&dir.0).exists());
    assert_eq!(fault::catalog_thread_io(&dir.0), catalog_io);
    catalog.close().await.unwrap();
}

#[tokio::test]
#[cfg(unix)]
async fn failed_cleanup_retries_after_writer_release_without_another_request() {
    let dir = TestDir::new();
    let clock = Arc::new(CountingClock::new());
    let catalog = setup(&dir, clock.clone()).await;
    let session = catalog
        .create_resumable_session(
            "bucket",
            "cancelled",
            crate::storage::ResumableOptions::new(crate::bucket::BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    let lease = catalog
        .lock_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    fault::arm(
        &dir.0,
        Step::SessionCleanupSync,
        0,
        Fault::Io(std::io::ErrorKind::StorageFull),
    );
    catalog.invalidate_resumable_session(&lease).await.unwrap();
    assert_eq!(fault::pending(&dir.0), 0);
    assert!(catalog.resumable_maintenance_error().is_some());
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    clock.start_and_wait_for_busy_pass(&catalog).await;
    assert_eq!(count(&connection, "journal"), 1);
    assert_eq!(count(&connection, "sessions"), 1);
    drop(lease);
    eventually_async("the failed cleanup to be retried", || async {
        count(&connection, "journal") == 0 && catalog.resumable_maintenance_error().is_none()
    })
    .await;
    assert!(!session.blob_id.staging_path(&dir.0).exists());
    assert_eq!(count(&connection, "sessions"), 1);
    catalog.close().await.unwrap();
}
