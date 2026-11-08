use super::*;
use crate::{
    clock::{Clock, ManualClock},
    storage::{
        catalog::fault::{self, Crash, Fault, Step},
        test_fixtures::{
            PERMISSIVE, PERMISSIVE_RESTORE, body, bucket, live, permissive_upload, read_all,
            version,
        },
    },
    test_support::{TestDir, eventually_async},
};
use bytes::Bytes;
use std::{
    fs, io,
    path::Path,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

async fn retained(data: &Path) -> (Catalog, ObjectTarget<i64>, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH + Duration::from_secs(1000)));
    let catalog = Catalog::open_with_clock(data, clock.clone()).await.unwrap();
    let mut record = bucket("bucket", "project");
    record.soft_delete_policy = Some(crate::bucket::SoftDeletePolicy {
        retention_duration_seconds: 604800,
        effective_time: clock.now(),
    });
    catalog.insert_bucket(record).await.unwrap();
    let record = catalog
        .put_object(
            permissive_upload("bucket", "object", 1000),
            body(vec![Bytes::from_static(b"retained")]),
        )
        .await
        .unwrap();
    catalog
        .delete_object(
            version("bucket", "object", record.generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    (
        catalog,
        ObjectTarget::new("bucket", "object", record.generation),
        clock,
    )
}

fn blob_count(data: &Path) -> usize {
    fs::read_dir(data.join("blobs"))
        .unwrap()
        .map(|shard| fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum()
}

async fn assert_state(catalog: &Catalog, target: ObjectTarget<i64>, committed: bool) {
    assert_eq!(
        catalog
            .get_soft_deleted_object(target, Default::default(), PERMISSIVE)
            .await
            .unwrap()
            .size,
        8
    );
    let object = catalog
        .open_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await;
    if committed {
        let (_, mut reader) = object.unwrap();
        assert_eq!(read_all(&mut reader).await, b"retained");
    } else {
        assert!(matches!(object, Err(ObjectError::ObjectNotFound)));
    }
}

#[tokio::test]
async fn restore_pin_and_commit_crashes_recover_only_committed_versions() {
    for crash in Crash::ALL {
        for step in [
            Step::BeforePublish,
            Step::RestorePinned,
            Step::BeforeCommit,
            Step::AfterCommit,
        ] {
            let data = TestDir::new();
            let snapshot = TestDir::new();
            let (catalog, target, clock) = retained(&data.0).await;
            fault::arm(&data.0, step, 0, crash.fault(snapshot.0.clone()));
            catalog
                .restore_object(target.clone(), Default::default(), PERMISSIVE_RESTORE)
                .await
                .unwrap();
            assert_eq!(fault::pending(&data.0), 0, "{crash:?} {step:?}");
            assert_eq!(fault::catalog_thread_io(&data.0), 0);
            catalog.close().await.unwrap();
            let recovered = Catalog::open_with_clock(&snapshot.0, clock).await.unwrap();
            let committed = step == Step::AfterCommit;
            assert_state(&recovered, target, committed).await;
            assert_eq!(blob_count(&snapshot.0), if committed { 2 } else { 1 });
            let connection = Connection::open(snapshot.0.join("catalog.sqlite3")).unwrap();
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            recovered.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn restore_errors_preserve_retained_bytes_and_ambiguous_commits() {
    for step in [
        Step::BeforePublish,
        Step::RestorePinned,
        Step::BeforeCommit,
        Step::AfterCommit,
    ] {
        let data = TestDir::new();
        let (catalog, target, clock) = retained(&data.0).await;
        fault::arm(&data.0, step, 0, Fault::Io(io::ErrorKind::Other));
        assert!(
            catalog
                .restore_object(target.clone(), Default::default(), PERMISSIVE_RESTORE)
                .await
                .is_err()
        );
        assert_eq!(fault::pending(&data.0), 0);
        let committed = step == Step::AfterCommit;
        assert_state(&catalog, target.clone(), committed).await;
        assert_eq!(blob_count(&data.0), if committed { 2 } else { 1 });
        assert_eq!(fault::catalog_thread_io(&data.0), 0);
        catalog.close().await.unwrap();
        let recovered = Catalog::open_with_clock(&data.0, clock).await.unwrap();
        assert_state(&recovered, target, committed).await;
        recovered.close().await.unwrap();
    }
}

#[tokio::test]
async fn restore_rejections_before_pinning_do_not_discard_unused_blob_ids() {
    let denied = BucketAccess::Policy {
        identity: None,
        permission: crate::auth::Permission::ObjectsRestore,
        project: None,
    };
    for expected in [
        ObjectError::Denied(denied.denied("bucket")),
        ObjectError::ObjectNotFound,
        ObjectError::PreconditionFailed,
    ] {
        let data = TestDir::new();
        let (catalog, mut target, _) = retained(&data.0).await;
        let access = if matches!(expected, ObjectError::Denied(_)) {
            RestoreAccess {
                restore: &denied,
                create: PERMISSIVE,
            }
        } else {
            PERMISSIVE_RESTORE
        };
        let mut conditions = ObjectConditions::default();
        if expected == ObjectError::ObjectNotFound {
            target.name = "missing".into();
        } else if expected == ObjectError::PreconditionFailed {
            conditions.if_generation_match = Some(123);
        }
        fault::arm(&data.0, Step::DiscardProbe, 0, Fault::Busy);
        assert_eq!(
            catalog.restore_object(target, conditions, access).await,
            Err(expected)
        );
        assert_eq!(fault::pending(&data.0), 1);
        // Consume the unused fault only after proving cleanup never reached its probe.
        assert!(fault::hit(&data.0, Step::DiscardProbe).is_err());
        assert_eq!(blob_count(&data.0), 1);
        assert_eq!(fs::read_dir(data.0.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
    }
}

#[test]
fn restore_owns_blocking_publication_through_disconnect_and_close() {
    for disconnect in [false, true] {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let data = TestDir::new();
            let (catalog, target, clock) = retained(&data.0).await;
            let (release, held) = std::sync::mpsc::channel::<()>();
            let (started, ready) = tokio::sync::oneshot::channel();
            let blocker = crate::task::spawn_blocking_on(
                &tokio::runtime::Handle::current(),
                "restore-test-blocker",
                move || {
                    started.send(()).unwrap();
                    held.recv().unwrap();
                },
            );
            ready.await.unwrap();
            let worker = catalog.clone();
            let restore_target = target.clone();
            let request = tokio::spawn(async move {
                worker
                    .restore_object(restore_target, Default::default(), PERMISSIVE_RESTORE)
                    .await
            });
            eventually_async("the restore to stage its body", || async {
                fs::read_dir(data.0.join("staging")).unwrap().count() != 0
            })
            .await;
            if disconnect {
                request.abort();
            }
            let worker = catalog.clone();
            let close = tokio::spawn(async move { worker.close().await });
            eventually_async("close to stop admission", || async {
                catalog.0.admission.is_closed()
            })
            .await;
            assert!(!close.is_finished());
            assert!(crate::storage::lock::acquire(&data.0).is_err());
            release.send(()).unwrap();
            blocker.await.unwrap().unwrap();
            if disconnect {
                assert!(request.await.unwrap_err().is_cancelled());
            } else {
                request.await.unwrap().unwrap();
            }
            close.await.unwrap().unwrap();
            assert_eq!(fs::read_dir(data.0.join("staging")).unwrap().count(), 0);
            let expected = catalog.admit_object_upload().err().unwrap();
            assert_eq!(
                catalog
                    .restore_object(target.clone(), Default::default(), PERMISSIVE_RESTORE)
                    .await
                    .unwrap_err(),
                expected
            );
            let recovered = Catalog::open_with_clock(&data.0, clock).await.unwrap();
            assert_state(&recovered, target, true).await;
            recovered.close().await.unwrap();
        });
    }
}

#[tokio::test]
async fn expired_restore_source_is_rechecked_after_pinning() {
    let data = TestDir::new();
    let (catalog, target, clock) = retained(&data.0).await;
    let request = RestoreRequest {
        target,
        conditions: Default::default(),
        restore: PERMISSIVE.clone(),
        create: PERMISSIVE.clone(),
    };
    let id = crate::storage::BlobId::generate().unwrap();
    let selection = request.clone();
    let backend = catalog.0.backend.clone();
    let time = objects::PublicationTime::read(clock.as_ref()).unwrap();
    let source = catalog
        .run_upload(move |connection| {
            let (source, _) = restore_candidate(connection, &selection, time)?;
            backend.pin(&source.blob, &id)?;
            Ok(source.blob)
        })
        .await
        .unwrap();
    let backend = catalog.0.backend.clone();
    let published =
        crate::task::run_blocking("restore-test-publish", move || backend.publish_pinned(&id))
            .await;
    clock.advance(Duration::from_secs(604801));
    let expired_clock = clock.clone();
    let error = catalog
        .run_upload(move |connection| {
            purge_expired(connection, "bucket", expired_clock.now().try_into()?)?;
            restore_in_transaction(
                connection,
                &request,
                source,
                id,
                published,
                expired_clock.as_ref(),
            )
        })
        .await
        .unwrap_err();
    assert_eq!(error, ObjectError::ObjectNotFound);
    catalog
        .discard_staged(objects::StagedBlob::pin(&catalog.0.backend, &data.0, id))
        .await;
    catalog.reclaim_after_commit().await;
    assert_eq!(blob_count(&data.0), 0);
    catalog.close().await.unwrap();
}
