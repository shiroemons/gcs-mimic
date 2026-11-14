use super::*;
use crate::{bucket::BucketAccess, storage::catalog::resumable::ResumableSession};
use std::time::{Duration, UNIX_EPOCH};

async fn failed_finalization() -> (crate::test_support::TestDir, Catalog, SessionLease) {
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    crate::storage::test_fixtures::seed_bucket(&catalog).await;
    let session = catalog
        .create_resumable_session(
            "bucket",
            "object",
            ResumableOptions::new(BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    let mut lease = catalog
        .lock_resumable_session(&session.id.to_string(), "bucket")
        .await
        .unwrap();
    let stage = session.blob_id.staging_path(&dir.0);
    std::fs::write(&stage, b"prefix-tail").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&stage)
        .unwrap()
        .sync_all()
        .unwrap();
    catalog
        .checkpoint_resumable_session(&mut lease, 6, None)
        .await
        .unwrap();
    objects::publish_blob(&stage, &session.blob_id.blob_path(&dir.0)).unwrap();
    (dir, catalog, lease)
}

fn publication_failure() -> Result<Value, ResumableError> {
    Err(object_unavailable(io::Error::other("publication failed")).into())
}

async fn assert_restored(catalog: &Catalog, lease: &SessionLease) {
    let session = catalog
        .resumable_session(&lease.session.id.to_string(), "bucket")
        .await
        .unwrap();
    assert_eq!(session.state, SessionState::Active);
    assert_eq!(session.offset, 6);
    assert_eq!(
        std::fs::read(session.blob_id.staging_path(&catalog.0.data_dir)).unwrap(),
        b"prefix"
    );
    assert!(!session.blob_id.blob_path(&catalog.0.data_dir).exists());
}

#[tokio::test]
async fn resumable_recovery_failures_preserve_the_checkpoint_for_restart() {
    let steps = [
        Step::ResumableRecoveryRename,
        #[cfg(unix)]
        Step::ResumableRecoveryShardSync,
        #[cfg(unix)]
        Step::ResumableRecoveryStagingSync,
        Step::ResumableRecoveryTruncate,
        Step::ResumableRecoverySync,
    ];
    for step in steps {
        let (dir, catalog, lease) = failed_finalization().await;
        let before = fault::catalog_thread_io(&dir.0);
        catalog.inject(step, 0, fault::Fault::Io(io::ErrorKind::PermissionDenied));
        assert!(
            catalog
                .resolve_resumable_finalization(&lease, publication_failure())
                .await
                .is_err()
        );
        assert_eq!(catalog.pending_faults(), 0, "{step:?}");
        assert_eq!(fault::catalog_thread_io(&dir.0), before, "{step:?}");
        let session = lease.session.clone();
        drop(lease);
        catalog.close().await.unwrap();
        let reopened = Catalog::open(&dir.0).await.unwrap();
        let lease = reopened
            .lock_resumable_session(&session.id.to_string(), "bucket")
            .await
            .unwrap();
        assert_restored(&reopened, &lease).await;
        drop(lease);
        reopened.close().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn resumable_recovery_crash_and_power_loss_restore_the_acknowledged_prefix() {
    for crash in super::super::fault::Crash::ALL {
        for step in [
            Step::ResumableRecoveryRename,
            Step::ResumableRecoveryShardSync,
            Step::ResumableRecoveryStagingSync,
            Step::ResumableRecoveryTruncate,
            Step::ResumableRecoverySync,
            Step::ResumableRecovered,
        ] {
            let (dir, catalog, lease) = failed_finalization().await;
            let snapshot = crate::test_support::TestDir::new();
            let path = snapshot.0.join("data");
            let before = fault::catalog_thread_io(&dir.0);
            catalog.inject(step, 0, crash.fault(path.clone()));
            assert!(
                catalog
                    .resolve_resumable_finalization(&lease, publication_failure())
                    .await
                    .is_err()
            );
            assert_eq!(catalog.pending_faults(), 0, "{crash:?} {step:?}");
            assert_eq!(fault::catalog_thread_io(&dir.0), before);
            assert_restored(&catalog, &lease).await;
            let reopened = Catalog::open(&path).await.unwrap();
            let recovered = reopened
                .lock_resumable_session(&lease.session.id.to_string(), "bucket")
                .await
                .unwrap();
            assert_restored(&reopened, &recovered).await;
            drop(recovered);
            reopened.close().await.unwrap();
            drop(lease);
            catalog.close().await.unwrap();
        }
    }
}

#[test]
fn resumable_retention_overflow_returns_the_publication_metadata_error() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE buckets(name TEXT, retention_policy TEXT); INSERT INTO buckets VALUES('bucket',NULL)").unwrap();
    let mut options = ResumableOptions::new(BucketAccess::Permissive);
    options.content_type = "text/plain".into();
    let session = ResumableSession {
        id: "0".repeat(64).parse().unwrap(),
        blob_id: "0".repeat(32).parse().unwrap(),
        bucket: "bucket".into(),
        name: "object".into(),
        incarnation: "0".repeat(32).parse().unwrap(),
        options: options.clone(),
        total: Some(0),
        offset: 0,
        created: UNIX_EPOCH,
        deadline: UNIX_EPOCH + Duration::from_secs(604800),
        state: SessionState::Active,
        completion: None,
    };
    for period in [10, i64::MAX] {
        connection
            .execute(
                "UPDATE buckets SET retention_policy=?1",
                [serde_json::json!({"retention_period":period}).to_string()],
            )
            .unwrap();
        let record = publication_record(
            &connection,
            &session,
            options.clone(),
            1,
            objects::PublicationDefaults {
                storage_class: crate::bucket::StorageClass::Standard,
                time: UNIX_EPOCH + Duration::from_secs(1),
                event_based_hold: false,
                owner: None,
            },
            Hashes {
                md5: [0; 16],
                crc32c: 0,
            },
            0,
        );
        if period == 10 {
            assert_eq!(
                record.unwrap().retention_expiration_time,
                Some(UNIX_EPOCH + Duration::from_secs(11))
            );
        } else {
            let error = record.unwrap_err();
            assert_eq!(error, ObjectError::InvalidMetadata);
            assert_eq!(error.to_string(), "custom metadata exceeds 8 KiB");
        }
    }
}
