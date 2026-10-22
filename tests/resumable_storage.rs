#[allow(dead_code)]
mod common;
use common::TestDir;
use gcs_mimic::{
    bucket::BucketRecord,
    clock::Clock,
    storage::{Catalog, ResumableError, ResumableOptions},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
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
            versioning_enabled: false,
            default_event_based_hold: false,
            metageneration: 1,
            time_created: UNIX_EPOCH,
            updated: UNIX_EPOCH,
        })
        .await
        .unwrap();
    catalog
}

#[tokio::test]
async fn sessions_persist_options_offsets_deadline_and_exclusive_admission() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(
        UNIX_EPOCH + Duration::from_secs(1000),
    )));
    let catalog = setup(&dir, clock.clone()).await;
    let options = ResumableOptions {
        content_type: "application/test".into(),
        origin: Some("https://client.invalid".into()),
        metadata: gcs_mimic::object::ObjectMetadataUpdate {
            replace: true,
            content_type: Some(None),
            content_encoding: Some(None),
            content_disposition: Some(Some("inline".into())),
            metadata: Some(None),
            ..Default::default()
        },
        ..Default::default()
    };
    let session = catalog
        .create_resumable_session("bucket", "../name/日本%", options.clone(), Some(9))
        .await
        .unwrap();
    assert_eq!(session.id.len(), 64);
    assert_eq!(session.blob_id.len(), 32);
    assert_eq!(
        session.deadline.duration_since(session.created).unwrap(),
        Duration::from_secs(604800)
    );
    let mut lease = catalog
        .lock_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    assert!(matches!(
        catalog.lock_resumable_session(&session.id, "bucket").await,
        Err(ResumableError::Object(_))
    ));
    assert!(
        catalog
            .checkpoint_resumable_session(&mut lease, 10, Some(9))
            .await
            .is_err()
    );
    std::fs::write(lease.staging_path(), b"abc").unwrap();
    catalog
        .checkpoint_resumable_session(&mut lease, 3, Some(9))
        .await
        .unwrap();
    drop(lease);
    catalog.close().await.unwrap();
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    let loaded = catalog
        .resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    assert_eq!(loaded.options, options);
    assert_eq!(loaded.offset, 3);
    assert_eq!(loaded.deadline, session.deadline);
    *clock.0.lock().unwrap() = session.deadline;
    assert_eq!(
        catalog
            .resumable_session(&session.id, "bucket")
            .await
            .unwrap_err(),
        ResumableError::NotFound
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn retained_session_cap_is_atomic_and_rejection_creates_no_file() {
    let dir = TestDir::new();
    let catalog = setup(&dir, Arc::new(TestClock(Mutex::new(UNIX_EPOCH)))).await;
    for _ in 0..127 {
        catalog
            .create_resumable_session("bucket", "name", Default::default(), None)
            .await
            .unwrap();
    }
    let (a, b) = tokio::join!(
        catalog.create_resumable_session("bucket", "a", Default::default(), None),
        catalog.create_resumable_session("bucket", "b", Default::default(), None)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(
        a.err().or_else(|| b.err()).unwrap(),
        ResumableError::Object(gcs_mimic::object::ObjectError::Unavailable(_))
    ));
    assert_eq!(
        std::fs::read_dir(dir.0.join("staging")).unwrap().count(),
        128
    );
    assert!(
        catalog
            .create_resumable_session("bucket", "rejected", Default::default(), None)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_dir(dir.0.join("staging")).unwrap().count(),
        128
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn checkpoints_reject_backward_offsets_changed_totals_terminal_and_expired_sessions() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let session = catalog
        .create_resumable_session("bucket", "name", Default::default(), Some(4))
        .await
        .unwrap();
    let mut lease = catalog
        .lock_resumable_session(&session.id, "bucket")
        .await
        .unwrap();
    std::fs::write(lease.staging_path(), b"ab").unwrap();
    catalog
        .checkpoint_resumable_session(&mut lease, 2, Some(4))
        .await
        .unwrap();
    for (offset, total) in [(1, Some(4)), (3, Some(5)), (2, None), (5, Some(4))] {
        assert!(matches!(
            catalog
                .checkpoint_resumable_session(&mut lease, offset, total)
                .await,
            Err(ResumableError::Invalid(_))
        ));
    }
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE resumable_sessions SET state = 'CANCELLED' WHERE id = ?1",
            [&session.id],
        )
        .unwrap();
    assert_eq!(
        catalog
            .checkpoint_resumable_session(&mut lease, 2, Some(4))
            .await
            .unwrap_err(),
        ResumableError::Gone
    );
    *clock.0.lock().unwrap() = session.deadline;
    assert_eq!(
        catalog
            .checkpoint_resumable_session(&mut lease, 2, Some(4))
            .await
            .unwrap_err(),
        ResumableError::NotFound
    );
    drop(lease);
    drop(connection);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn previous_schema_migrates_and_invalid_session_options_are_rejected_read_only() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    catalog.close().await.unwrap();
    let path = dir.0.join("catalog.sqlite3");
    let legacy = rusqlite::Connection::open(&path).unwrap();
    common::downgrade_object_schema(&legacy);
    legacy.execute_batch("ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold; DROP TABLE resumable_sessions; DROP TABLE bucket_incarnations; PRAGMA user_version = 6;").unwrap();
    drop(legacy);
    let catalog = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    assert!(catalog.get_bucket("bucket").await.unwrap().is_some());
    let session = catalog
        .create_resumable_session("bucket", "name", Default::default(), None)
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE resumable_sessions SET options = '{}' WHERE id = ?1",
            [session.id],
        )
        .unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    assert!(Catalog::open(&dir.0).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
