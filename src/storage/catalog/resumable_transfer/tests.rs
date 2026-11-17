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
fn resumable_retention_overflow_reports_the_stored_period_as_corrupt() {
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
        running_hash: None,
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
            assert!(matches!(error, ObjectError::Corrupt(_)), "{error:?}");
            assert_eq!(
                error.to_string(),
                "object storage corrupt: Stored retention period overflows the expiration time"
            );
        }
    }
}

async fn append(catalog: &Catalog, id: &str, start: u64, bytes: &'static [u8]) -> Written {
    let lease = catalog.lock_resumable_session(id, "bucket").await.unwrap();
    let chunk = ResumableChunk {
        start,
        length: bytes.len() as u64,
        end: ChunkEnd::Exact {
            total: Some(8),
            finalize: start + bytes.len() as u64 == 8,
        },
        options: lease.session.options.clone(),
        catalog_wait: watch::channel(ChunkWait::Transfer).0,
    };
    let body = crate::storage::test_fixtures::body(vec![bytes::Bytes::from_static(bytes)]);
    catalog
        .write_resumable_session(
            lease,
            chunk,
            body,
            catalog.admit_object_upload().unwrap(),
            |record| serde_json::to_value(record.hashes()).map_err(ObjectError::backend),
        )
        .await
        .unwrap()
}

fn digest(bytes: &[u8]) -> Hashes {
    let mut hasher = Hasher::default();
    hasher.update(bytes);
    hasher.finish()
}

#[tokio::test]
async fn a_session_staged_before_running_digests_reports_none_and_still_completes() {
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    crate::storage::test_fixtures::seed_bucket(&catalog).await;
    let id = catalog
        .create_resumable_session(
            "bucket",
            "object",
            ResumableOptions::new(BucketAccess::Permissive),
            Some(8),
        )
        .await
        .unwrap()
        .id
        .to_string();
    let Written::Open(progress) = append(&catalog, &id, 0, b"part").await else {
        panic!("a partial chunk leaves the upload open");
    };
    assert_eq!(
        progress,
        Progress {
            offset: 4,
            hashes: Some(digest(b"part"))
        }
    );
    catalog.close().await.unwrap();
    // Schema 55 had no running digest column.
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    super::super::test_support::downgrade_running_hash_schema(&connection);
    connection.pragma_update(None, "user_version", 55).unwrap();
    drop(connection);

    let catalog = Catalog::open(&dir.0).await.unwrap();
    let session = catalog.resumable_session(&id, "bucket").await.unwrap();
    assert_eq!(
        session.progress().unwrap(),
        Progress {
            offset: 4,
            hashes: None
        }
    );
    // Completion digests the staged bytes, so the stored checksums cover the whole object.
    let Written::Complete(resource) = append(&catalog, &id, 4, b"tail").await else {
        panic!("the final chunk completes the upload");
    };
    assert_eq!(resource, serde_json::to_value(digest(b"parttail")).unwrap());
    catalog.close().await.unwrap();
}
