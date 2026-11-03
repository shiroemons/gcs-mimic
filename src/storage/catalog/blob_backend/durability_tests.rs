use super::*;
use crate::storage::catalog::fault::{self, Fault, Step};
use crate::{
    bucket::BucketAccess,
    storage::{Catalog, ResumableOptions, SessionState, test_fixtures::bucket},
    test_support::TestDir,
};

async fn catalog(data: &Path) -> Catalog {
    let catalog = Catalog::open(data).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    catalog
}

async fn session(catalog: &Catalog) -> crate::storage::ResumableSession {
    catalog
        .create_resumable_session(
            "bucket",
            "object",
            ResumableOptions::new(BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn resumable_creation_directory_sync_survives_crash_and_power_loss() {
    let snapshots = TestDir::new();
    for fault in [
        Fault::Snapshot(snapshots.0.join("crash")),
        Fault::PowerLoss(snapshots.0.join("power-loss")),
    ] {
        let data = TestDir::new();
        let catalog = catalog(&data.0).await;
        let snapshot = match &fault {
            Fault::Snapshot(path) | Fault::PowerLoss(path) => path.clone(),
            _ => unreachable!(),
        };
        fault::arm(&data.0, Step::SessionCreated, 0, fault);
        let session = session(&catalog).await;
        assert_eq!(fault::pending(&data.0), 0);
        catalog.close().await.unwrap();
        assert!(session.blob_id.staging_path(&snapshot).is_file());
        let recovered = Catalog::open(&snapshot).await.unwrap();
        let resumed = recovered
            .resumable_session(&session.id, "bucket")
            .await
            .unwrap();
        assert_eq!(resumed.state, SessionState::Active);
        assert_eq!(resumed.offset, 0);
        assert_eq!(resumed.blob_id, session.blob_id);
        recovered.close().await.unwrap();
    }
}

#[tokio::test]
async fn resumable_cleanup_directory_sync_survives_crash_and_power_loss() {
    let snapshots = TestDir::new();
    for fault in [
        Fault::Snapshot(snapshots.0.join("crash")),
        Fault::PowerLoss(snapshots.0.join("power-loss")),
    ] {
        let data = TestDir::new();
        let catalog = catalog(&data.0).await;
        let session = session(&catalog).await;
        let catalog_io = fault::catalog_thread_io(&data.0);
        let snapshot = match &fault {
            Fault::Snapshot(path) | Fault::PowerLoss(path) => path.clone(),
            _ => unreachable!(),
        };
        fault::arm(&data.0, Step::SessionCleanupSynced, 0, fault);
        catalog
            .cancel_resumable_session(&session.id, "bucket")
            .await
            .unwrap();
        assert_eq!(fault::pending(&data.0), 0);
        catalog.close().await.unwrap();
        assert!(!session.blob_id.staging_path(&snapshot).exists());
        assert_eq!(fault::catalog_thread_io(&data.0), catalog_io);
        let recovered = Catalog::open(&snapshot).await.unwrap();
        assert!(!session.blob_id.staging_path(&snapshot).exists());
        assert!(matches!(
            recovered.resumable_session(&session.id, "bucket").await,
            Err(crate::storage::ResumableError::Cancelled)
        ));
        recovered.close().await.unwrap();
    }
}

#[tokio::test]
async fn resumable_cleanup_before_directory_sync_recovers_after_power_loss_and_crash() {
    use crate::storage::catalog::fault::Crash;
    for crash in Crash::ALL {
        let data = TestDir::new();
        let snapshot = TestDir::new();
        let catalog = catalog(&data.0).await;
        let session = session(&catalog).await;
        fault::arm(
            &data.0,
            Step::SessionCleanupSync,
            0,
            crash.fault(snapshot.0.clone()),
        );
        catalog
            .cancel_resumable_session(&session.id, "bucket")
            .await
            .unwrap();
        assert_eq!(fault::pending(&data.0), 0);
        assert_eq!(
            session.blob_id.staging_path(&snapshot.0).exists(),
            *crash == Crash::PowerLoss
        );
        catalog.close().await.unwrap();
        let recovered = Catalog::open(&snapshot.0).await.unwrap();
        assert!(!session.blob_id.staging_path(&snapshot.0).exists());
        assert!(matches!(
            recovered.resumable_session(&session.id, "bucket").await,
            Err(crate::storage::ResumableError::Cancelled)
        ));
        recovered.close().await.unwrap();
    }
}

#[test]
fn cancelled_cleanup_keeps_its_writer_and_directory_ownership_until_close() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let data = TestDir::new();
        let catalog = catalog(&data.0).await;
        let session = session(&catalog).await;
        let catalog_io = fault::catalog_thread_io(&data.0);
        let (release, held) = std::sync::mpsc::channel::<()>();
        let (started, ready) = tokio::sync::oneshot::channel();
        let blocker = crate::task::spawn_blocking_on(
            &tokio::runtime::Handle::current(),
            "cleanup-test-blocker",
            move || {
                started.send(()).unwrap();
                held.recv().unwrap();
            },
        );
        ready.await.unwrap();
        let worker = catalog.clone();
        let id = session.id.clone();
        let request =
            tokio::spawn(async move { worker.cancel_resumable_session(&id, "bucket").await });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while catalog.0.uploads.lock().unwrap().active == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert!(catalog.try_resumable_writer(&session.id).is_err());
        let worker = catalog.clone();
        let close = tokio::spawn(async move { worker.close().await });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !catalog.0.admission.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!close.is_finished());
        assert!(crate::storage::lock::acquire(&data.0).is_err());
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        close.await.unwrap().unwrap();
        assert!(
            !catalog
                .0
                .session_writers
                .lock()
                .unwrap()
                .contains(&session.id)
        );
        assert!(!session.blob_id.staging_path(&data.0).exists());
        assert_eq!(fault::catalog_thread_io(&data.0), catalog_io);
        let recovered = Catalog::open(&data.0).await.unwrap();
        assert!(matches!(
            recovered.resumable_session(&session.id, "bucket").await,
            Err(crate::storage::ResumableError::Cancelled)
        ));
        recovered.close().await.unwrap();
    });
}

fn io_kind(error: &(dyn std::error::Error + 'static)) -> io::ErrorKind {
    std::iter::successors(Some(error), |error| error.source())
        .find_map(|error| error.downcast_ref::<io::Error>())
        .unwrap()
        .kind()
}

#[tokio::test]
async fn failed_creation_directory_sync_never_commits_a_session() {
    for kind in [io::ErrorKind::Other, io::ErrorKind::StorageFull] {
        let data = TestDir::new();
        let catalog = catalog(&data.0).await;
        fault::arm(&data.0, Step::SessionCreateSync, 0, Fault::Io(kind));
        let error = catalog
            .create_resumable_session(
                "bucket",
                "object",
                ResumableOptions::new(BucketAccess::Permissive),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(io_kind(&error), kind);
        assert_eq!(fault::pending(&data.0), 0);
        let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM resumable_sessions", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
        assert_eq!(fs::read_dir(data.0.join("staging")).unwrap().count(), 0);
        drop(connection);
        catalog.close().await.unwrap();
        let recovered = Catalog::open(&data.0).await.unwrap();
        recovered.close().await.unwrap();
    }
}

#[tokio::test]
async fn failed_cleanup_directory_sync_keeps_the_journal_for_retry() {
    for kind in [io::ErrorKind::Other, io::ErrorKind::StorageFull] {
        let data = TestDir::new();
        let catalog = catalog(&data.0).await;
        let session = session(&catalog).await;
        fault::arm(&data.0, Step::SessionCleanupSync, 0, Fault::Io(kind));
        catalog
            .cancel_resumable_session(&session.id, "bucket")
            .await
            .unwrap();
        assert_eq!(
            catalog.resumable_maintenance_error(),
            Some(format!("catalog I/O: {kind}"))
        );
        assert_eq!(fault::pending(&data.0), 0);
        let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM pending_blob_deletions WHERE blob_id=?1",
                    [session.blob_id],
                    |row| row.get::<_, u64>(0)
                )
                .unwrap(),
            1
        );
        catalog.maintain_resumable_sessions().await.unwrap();
        assert_eq!(catalog.resumable_maintenance_error(), None);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM pending_blob_deletions WHERE blob_id=?1",
                    [session.blob_id],
                    |row| row.get::<_, u64>(0)
                )
                .unwrap(),
            0
        );
        assert!(!session.blob_id.staging_path(&data.0).exists());
        drop(connection);
        catalog.close().await.unwrap();
        let recovered = Catalog::open(&data.0).await.unwrap();
        assert!(matches!(
            recovered.resumable_session(&session.id, "bucket").await,
            Err(crate::storage::ResumableError::Cancelled)
        ));
        recovered.close().await.unwrap();
    }
}
