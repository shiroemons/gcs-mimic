use super::super::fault::{Fault, Step};
use super::*;
use crate::{
    clock::{Clock, SystemClock},
    object::UploadChunk,
    storage::admin::{BucketDefinition, BucketName, LocalAdmin},
    test_support::TestDir,
};
use std::{
    io,
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};
use tokio::sync::mpsc;

async fn setup(clock: Arc<dyn Clock>) -> (TestDir, Catalog) {
    let dir = TestDir::new();
    let catalog = Catalog::open_for_server(&dir.0, clock, LocalAdmin::Enabled)
        .await
        .unwrap();
    let admin = catalog.local_admin().unwrap();
    admin
        .ensure_bucket(
            BucketDefinition::default_for(
                &BucketName::new("bucket").unwrap(),
                "project",
                &SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    (dir, catalog)
}

async fn begin(catalog: &Catalog) -> Target {
    let options = Options {
        content_type: "text/plain".into(),
        metadata: Default::default(),
    };
    catalog
        .begin_xml_multipart("bucket", "value", options, BucketAccess::Permissive)
        .await
        .unwrap()
        .target
}

async fn put(
    catalog: &Catalog,
    target: &Target,
    number: u16,
    bytes: &'static [u8],
) -> Result<Part, Error> {
    let lease = catalog
        .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
        .await?;
    let (sender, body) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(bytes::Bytes::from_static(bytes))))
        .unwrap();
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    catalog
        .put_xml_part(
            lease,
            PartUpload {
                number,
                expected: Default::default(),
                max_bytes: 64 * 1024 * 1024,
            },
            body,
        )
        .await
}

async fn complete(catalog: &Catalog, target: &Target, etag: &str) -> Result<ObjectRecord, Error> {
    let lease = catalog
        .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
        .await?;
    catalog
        .complete_xml_multipart(
            lease,
            vec![CompletedPart {
                number: 1,
                etag: etag.into(),
            }],
            64 * 1024 * 1024,
        )
        .await
}

async fn bytes(catalog: &Catalog) -> Vec<u8> {
    let (_, mut reader) = catalog
        .open_object("bucket", "value", None, Default::default())
        .await
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

async fn old_object(catalog: &Catalog) {
    let (sender, body) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(bytes::Bytes::from_static(
            b"previous",
        ))))
        .unwrap();
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    catalog
        .put_object(
            "bucket",
            "value",
            "text/plain".into(),
            Default::default(),
            Default::default(),
            1024,
            body,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn part_eof_lease_serialization_and_reset_busy_preserve_state() {
    let (_dir, catalog) = setup(Arc::new(SystemClock)).await;
    let target = begin(&catalog).await;
    let original = put(&catalog, &target, 1, b"original").await.unwrap();
    let lease = catalog
        .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
        .await
        .unwrap();
    assert!(matches!(
        catalog
            .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
            .await,
        Err(Error::Busy)
    ));
    assert!(matches!(
        catalog.local_admin().unwrap().reset().await,
        Err(super::super::admin::AdminError::Busy { .. })
    ));
    let (sender, body) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(bytes::Bytes::from_static(
            b"incomplete",
        ))))
        .unwrap();
    drop(sender);
    assert!(
        catalog
            .put_xml_part(
                lease,
                PartUpload {
                    number: 1,
                    expected: Default::default(),
                    max_bytes: 1024
                },
                body
            )
            .await
            .is_err()
    );
    let (_, parts, _) = catalog
        .list_xml_parts(target.clone(), BucketAccess::Permissive, 0, 1)
        .await
        .unwrap();
    assert_eq!(parts[0].etag, original.etag);
    let lease = catalog
        .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
        .await
        .unwrap();
    catalog.abort_xml_multipart(lease).await.unwrap();
    assert!(matches!(
        catalog
            .lease_xml_multipart(target, BucketAccess::Permissive)
            .await,
        Err(Error::NoSuchUpload)
    ));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn part_durable_step_failures_keep_prior_part_and_cleanup_after_restart() {
    for step in [
        Step::BodyWrite,
        Step::BodyFlush,
        Step::BodySync,
        Step::Checkpoint,
        Step::BeforeCommit,
        Step::AfterCommit,
    ] {
        let (dir, catalog) = setup(Arc::new(SystemClock)).await;
        let target = begin(&catalog).await;
        let old = put(&catalog, &target, 1, b"old").await.unwrap();
        catalog.inject(step, 0, Fault::Io(io::ErrorKind::StorageFull));
        let result = put(&catalog, &target, 1, b"new part").await;
        assert!(
            matches!(result, Err(Error::Object(ObjectError::Io(_)))),
            "{step:?}: {result:?}"
        );
        assert_eq!(catalog.pending_faults(), 0);
        catalog.close().await.unwrap();
        let reopened = Catalog::open(&dir.0).await.unwrap();
        let (_, parts, _) = reopened
            .list_xml_parts(target, BucketAccess::Permissive, 0, 1000)
            .await
            .unwrap();
        assert_eq!(parts.len(), 1);
        if step == Step::AfterCommit {
            assert_ne!(parts[0].etag, old.etag);
        } else {
            assert_eq!(parts[0].etag, old.etag);
        }
        assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 1);
        reopened.close().await.unwrap();
    }
}

#[tokio::test]
async fn abort_commits_before_deferred_cleanup_and_recovery_retries_its_journal() {
    for step in [Step::MultipartReclaim, Step::MultipartReclaimSync] {
        let (dir, catalog) = setup(Arc::new(SystemClock)).await;
        let target = begin(&catalog).await;
        put(&catalog, &target, 1, b"retired").await.unwrap();
        catalog.inject(step, 0, Fault::Io(io::ErrorKind::PermissionDenied));
        let lease = catalog
            .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
            .await
            .unwrap();
        catalog.abort_xml_multipart(lease).await.unwrap();
        assert_eq!(catalog.pending_faults(), 0);
        assert!(matches!(
            catalog
                .list_xml_parts(target, BucketAccess::Permissive, 0, 1000)
                .await,
            Err(Error::NoSuchUpload)
        ));
        catalog.close().await.unwrap();
        let reopened = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
        reopened.close().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn abort_commit_and_reclaim_crash_snapshots_never_resurrect_retired_parts() {
    for power in [false, true] {
        for step in [
            Step::BeforeCommit,
            Step::AfterCommit,
            Step::MultipartReclaim,
            Step::MultipartReclaimSync,
        ] {
            let (_dir, catalog) = setup(Arc::new(SystemClock)).await;
            let target = begin(&catalog).await;
            put(&catalog, &target, 1, b"retired").await.unwrap();
            let snapshot = TestDir::new();
            let path = snapshot.0.join("data");
            catalog.inject(
                step,
                0,
                if power {
                    Fault::PowerLoss(path.clone())
                } else {
                    Fault::Snapshot(path.clone())
                },
            );
            let lease = catalog
                .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
                .await
                .unwrap();
            catalog.abort_xml_multipart(lease).await.unwrap();
            assert_eq!(catalog.pending_faults(), 0);
            let reopened = Catalog::open(&path).await.unwrap();
            if step == Step::BeforeCommit {
                assert_eq!(
                    reopened
                        .list_xml_parts(target, BucketAccess::Permissive, 0, 1000)
                        .await
                        .unwrap()
                        .1
                        .len(),
                    1
                );
            } else {
                assert!(matches!(
                    reopened
                        .list_xml_parts(target, BucketAccess::Permissive, 0, 1000)
                        .await,
                    Err(Error::NoSuchUpload)
                ));
                assert_eq!(std::fs::read_dir(path.join("staging")).unwrap().count(), 0);
            }
            reopened.close().await.unwrap();
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn completion_durable_step_failures_preserve_old_or_confirm_committed_generation() {
    for step in [
        Step::BodyWrite,
        Step::BodyFlush,
        Step::BodySync,
        Step::BeforePublish,
        Step::AfterPublish,
        Step::BeforeCommit,
        Step::AfterCommit,
    ] {
        let (dir, catalog) = setup(Arc::new(SystemClock)).await;
        old_object(&catalog).await;
        let target = begin(&catalog).await;
        let part = put(&catalog, &target, 1, b"completed").await.unwrap();
        catalog.inject(step, 0, Fault::Io(io::ErrorKind::StorageFull));
        let result = complete(&catalog, &target, &part.etag).await;
        assert!(result.is_err(), "{step:?}");
        assert_eq!(catalog.pending_faults(), 0);
        catalog.close().await.unwrap();
        let reopened = Catalog::open(&dir.0).await.unwrap();
        if step == Step::AfterCommit {
            assert_eq!(bytes(&reopened).await, b"completed");
            assert!(matches!(
                reopened
                    .list_xml_parts(target, BucketAccess::Permissive, 0, 1000)
                    .await,
                Err(Error::NoSuchUpload)
            ));
        } else {
            assert_eq!(bytes(&reopened).await, b"previous");
            assert_eq!(
                reopened
                    .list_xml_parts(target, BucketAccess::Permissive, 0, 1000)
                    .await
                    .unwrap()
                    .1
                    .len(),
                1
            );
        }
        reopened.close().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn multipart_part_and_completion_crash_and_power_loss_snapshots_recover() {
    for power in [false, true] {
        for completing in [false, true] {
            let steps = if completing {
                vec![
                    Step::BodyWrite,
                    Step::BodyFlush,
                    Step::BodySync,
                    Step::BeforePublish,
                    Step::AfterPublish,
                    Step::BeforeCommit,
                    Step::AfterCommit,
                ]
            } else {
                vec![
                    Step::BodyWrite,
                    Step::BodyFlush,
                    Step::BodySync,
                    Step::Checkpoint,
                    Step::BeforeCommit,
                    Step::AfterCommit,
                ]
            };
            for step in steps {
                let (dir, catalog) = setup(Arc::new(SystemClock)).await;
                old_object(&catalog).await;
                let target = begin(&catalog).await;
                let old = put(&catalog, &target, 1, b"old part").await.unwrap();
                let snapshot = TestDir::new();
                let path = snapshot.0.join("data");
                catalog.inject(
                    step,
                    0,
                    if power {
                        Fault::PowerLoss(path.clone())
                    } else {
                        Fault::Snapshot(path.clone())
                    },
                );
                if completing {
                    complete(&catalog, &target, &old.etag).await.unwrap();
                } else {
                    put(&catalog, &target, 1, b"new part").await.unwrap();
                }
                assert_eq!(catalog.pending_faults(), 0, "{power} {completing} {step:?}");
                let reopened = Catalog::open(&path).await.unwrap();
                if completing && step == Step::AfterCommit {
                    assert_eq!(bytes(&reopened).await, b"old part");
                    assert!(matches!(
                        reopened
                            .list_xml_parts(target, BucketAccess::Permissive, 0, 1000)
                            .await,
                        Err(Error::NoSuchUpload)
                    ));
                } else {
                    assert_eq!(bytes(&reopened).await, b"previous");
                    let (_, parts, _) = reopened
                        .list_xml_parts(target, BucketAccess::Permissive, 0, 1000)
                        .await
                        .unwrap();
                    assert_eq!(parts.len(), 1);
                    if !completing && step == Step::AfterCommit {
                        assert_ne!(parts[0].etag, old.etag);
                    } else {
                        assert_eq!(parts[0].etag, old.etag);
                    }
                }
                reopened.close().await.unwrap();
                catalog.close().await.unwrap();
                drop(dir);
            }
        }
    }
}

#[tokio::test]
async fn injected_clock_expiration_and_session_admission_are_bounded() {
    struct Time(Mutex<SystemTime>);
    impl Clock for Time {
        fn now(&self) -> SystemTime {
            *self.0.lock().unwrap()
        }
    }
    let clock = Arc::new(Time(Mutex::new(
        UNIX_EPOCH + Duration::from_secs(1_000_000),
    )));
    let (_dir, catalog) = setup(clock.clone()).await;
    let target = begin(&catalog).await;
    put(&catalog, &target, 1, b"expires").await.unwrap();
    for _ in 1..128 {
        begin(&catalog).await;
    }
    let options = Options {
        content_type: String::new(),
        metadata: Default::default(),
    };
    assert!(
        catalog
            .begin_xml_multipart("bucket", "full", options, BucketAccess::Permissive)
            .await
            .is_err()
    );
    *clock.0.lock().unwrap() += Duration::from_secs(604800);
    assert!(matches!(
        catalog
            .lease_xml_multipart(target, BucketAccess::Permissive)
            .await,
        Err(Error::NoSuchUpload)
    ));
    assert!(
        catalog
            .maintain_xml_multipart(unix_millis(clock.now()).unwrap())
            .await
            .unwrap()
            .is_none()
    );
    begin(&catalog).await;
    catalog.close().await.unwrap();
}

#[test]
fn multipart_errors_preserve_storage_causes() {
    let error = Error::from(ObjectError::Io(
        std::io::Error::other("write failed").into(),
    ));
    assert!(std::error::Error::source(&error).is_some());
}

#[tokio::test]
async fn bucket_delete_wakes_cleanup_and_releases_full_multipart_session_capacity() {
    let (_dir, catalog) = setup(Arc::new(SystemClock)).await;
    let target = begin(&catalog).await;
    put(&catalog, &target, 1, b"hidden part").await.unwrap();
    for _ in 1..128 {
        begin(&catalog).await;
    }
    assert!(
        catalog
            .begin_xml_multipart(
                "bucket",
                "full",
                Options {
                    content_type: String::new(),
                    metadata: Default::default(),
                },
                BucketAccess::Permissive
            )
            .await
            .is_err()
    );
    // Consume the coalesced insertion wake before observing the deletion's own signal.
    catalog.0.maintenance_wake.notified().await;
    let changed = catalog.0.maintenance_wake.notified();
    tokio::pin!(changed);
    changed.as_mut().enable();
    catalog.delete_bucket("bucket", None, None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), changed)
        .await
        .unwrap();
    catalog
        .local_admin()
        .unwrap()
        .ensure_bucket(
            BucketDefinition::default_for(
                &BucketName::new("bucket").unwrap(),
                "project",
                &SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    catalog
        .maintain_xml_multipart(unix_millis(SystemClock.now()).unwrap())
        .await
        .unwrap();
    begin(&catalog).await;
    assert!(matches!(
        catalog
            .lease_xml_multipart(target, BucketAccess::Permissive)
            .await,
        Err(Error::NoSuchUpload)
    ));
    catalog.close().await.unwrap();
}
