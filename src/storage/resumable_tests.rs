use crate as gcs_mimic;
use crate::storage::test_fixtures::seed_bucket;
use crate::test_support::TestDir;
use gcs_mimic::storage::admin::{BucketName, Origin};
use gcs_mimic::{
    clock::ManualClock,
    storage::{Catalog, ResumableError, ResumableOptions},
};
use std::{
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

async fn setup(dir: &TestDir, clock: Arc<ManualClock>) -> Catalog {
    let catalog = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    seed_bucket(&catalog).await;
    catalog
}

/// Session IDs are issued in lowercase, so the uppercase spelling of a live session's ID names
/// no session, as an unknown ID does.
#[tokio::test]
async fn an_uppercase_session_id_names_no_session() {
    let dir = TestDir::new();
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH + Duration::from_secs(1000)));
    let catalog = setup(&dir, clock).await;
    let session = catalog
        .create_resumable_session(
            "bucket",
            "object",
            ResumableOptions::new(gcs_mimic::bucket::BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    let upper = session.id.to_string().to_ascii_uppercase();
    assert!(matches!(
        catalog.resumable_session(&upper, "bucket").await,
        Err(ResumableError::NotFound)
    ));
    assert!(matches!(
        catalog.cancel_resumable_session(&upper, "bucket").await,
        Err(ResumableError::NotFound)
    ));
    assert!(
        catalog
            .resumable_session(&session.id.to_string(), "bucket")
            .await
            .is_ok()
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sessions_persist_options_offsets_deadline_and_exclusive_admission() {
    let dir = TestDir::new();
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH + Duration::from_secs(1000)));
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
        ..gcs_mimic::storage::ResumableOptions::new(gcs_mimic::bucket::BucketAccess::Permissive)
    };
    let session = catalog
        .create_resumable_session("bucket", "../name/日本%", options.clone(), Some(9))
        .await
        .unwrap();
    assert_eq!(session.id.to_hex().len(), 64);
    assert_eq!(session.blob_id.to_hex().len(), 32);
    assert_eq!(
        session.deadline.duration_since(session.created).unwrap(),
        Duration::from_secs(604800)
    );
    let mut lease = catalog
        .lock_resumable_session(&session.id.to_string(), "bucket")
        .await
        .unwrap();
    assert!(matches!(
        catalog
            .lock_resumable_session(&session.id.to_string(), "bucket")
            .await,
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
        .resumable_session(&session.id.to_string(), "bucket")
        .await
        .unwrap();
    assert_eq!(loaded.options, options);
    assert_eq!(loaded.offset, 3);
    assert_eq!(loaded.deadline, session.deadline);
    clock.set(session.deadline);
    assert_eq!(
        catalog
            .resumable_session(&session.id.to_string(), "bucket")
            .await
            .unwrap_err(),
        ResumableError::NotFound
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn retained_session_cap_is_atomic_and_rejection_creates_no_file() {
    let dir = TestDir::new();
    let catalog = setup(&dir, Arc::new(ManualClock::new(UNIX_EPOCH))).await;
    catalog.apply_session_limits(&gcs_mimic::config::Config {
        max_resumable_sessions: std::num::NonZeroU32::new(128),
        ..Default::default()
    });
    for _ in 0..127 {
        catalog
            .create_resumable_session(
                "bucket",
                "name",
                gcs_mimic::storage::ResumableOptions::new(
                    gcs_mimic::bucket::BucketAccess::Permissive,
                ),
                None,
            )
            .await
            .unwrap();
    }
    let (a, b) = tokio::join!(
        catalog.create_resumable_session(
            "bucket",
            "a",
            gcs_mimic::storage::ResumableOptions::new(gcs_mimic::bucket::BucketAccess::Permissive),
            None
        ),
        catalog.create_resumable_session(
            "bucket",
            "b",
            gcs_mimic::storage::ResumableOptions::new(gcs_mimic::bucket::BucketAccess::Permissive),
            None
        )
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
            .create_resumable_session(
                "bucket",
                "rejected",
                gcs_mimic::storage::ResumableOptions::new(
                    gcs_mimic::bucket::BucketAccess::Permissive
                ),
                None
            )
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
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH));
    let catalog = setup(&dir, clock.clone()).await;
    let session = catalog
        .create_resumable_session(
            "bucket",
            "name",
            gcs_mimic::storage::ResumableOptions::new(gcs_mimic::bucket::BucketAccess::Permissive),
            Some(4),
        )
        .await
        .unwrap();
    let mut lease = catalog
        .lock_resumable_session(&session.id.to_string(), "bucket")
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
        ResumableError::Cancelled
    );
    clock.set(session.deadline);
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
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH));
    let catalog = setup(&dir, clock.clone()).await;
    catalog.close().await.unwrap();
    let path = dir.0.join("catalog.sqlite3");
    let legacy = rusqlite::Connection::open(&path).unwrap();
    super::catalog::test_support::downgrade_object_schema(&legacy);
    legacy.execute_batch("ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold; DROP TABLE resumable_sessions; DROP TABLE bucket_incarnations; PRAGMA user_version = 6;").unwrap();
    drop(legacy);
    let catalog = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    assert!(
        catalog
            .admin(Origin::Seed)
            .get_bucket(&BucketName::new("bucket").unwrap())
            .await
            .unwrap()
            .is_some()
    );
    let session = catalog
        .create_resumable_session(
            "bucket",
            "name",
            gcs_mimic::storage::ResumableOptions::new(gcs_mimic::bucket::BucketAccess::Permissive),
            None,
        )
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
