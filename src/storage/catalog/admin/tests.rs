use super::*;
use crate::{
    clock::SystemClock,
    test_support::{TestDir, live_len},
};
use bytes::Bytes;
use std::collections::BTreeMap;
use std::{fs, time::Duration};

fn publication_blobs(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(dir.join("blobs"))
        .unwrap()
        .flat_map(|shard| {
            fs::read_dir(shard.unwrap().path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
        })
        .collect()
}

#[tokio::test]
async fn shared_reclamation_preserves_cancelled_session_cleanup_markers() {
    let (dir, catalog, _admin, _bucket, _name) = setup().await;
    let session = catalog
        .create_resumable_session(
            "test-bucket",
            "cancelled",
            super::super::ResumableOptions::new(BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    let stage = dir.0.join("staging").join(&session.blob_id);
    fs::remove_file(&stage).unwrap();
    fs::create_dir(&stage).unwrap();
    let lease = catalog
        .lock_resumable_session(&session.id, "test-bucket")
        .await
        .unwrap();
    catalog.invalidate_resumable_session(&lease).await.unwrap();
    catalog.reclaim_pending().await.unwrap();
    let id = session.blob_id.clone();
    catalog
        .run_upload(move |connection| {
            let pending: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pending_blob_deletions WHERE blob_id = ?1)",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(pending);
            Ok(())
        })
        .await
        .unwrap();
    assert!(stage.is_dir());
    drop(lease);
    fs::remove_dir(&stage).unwrap();
    fs::write(&stage, b"").unwrap();
    catalog.maintain_resumable_sessions().await.unwrap();
    assert!(!stage.exists());
    catalog
        .run_upload(|connection| {
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reclamation_parallel_to_media_derived_and_resumable_keeps_committed_bodies() {
    for route in ["media", "derived", "resumable"] {
        let (dir, catalog, admin, bucket, name) = setup().await;
        let old = admin
            .publish_object(&bucket, &name, request(None), body(b"old"))
            .await
            .unwrap();
        let held = catalog.0.reclamation.clone().acquire_owned().await.unwrap();
        let worker = catalog.clone();
        let cleanup = tokio::spawn(async move { worker.reclaim_pending().await });
        let worker = catalog.clone();
        let publication = tokio::spawn(async move {
            match route {
                "media" => {
                    worker
                        .put_object(
                            "test-bucket",
                            "object",
                            "text/plain".into(),
                            Default::default(),
                            Default::default(),
                            1024,
                            body(b"new"),
                        )
                        .await
                        .unwrap();
                }
                "derived" => {
                    let source = worker
                        .resolve_derived_source(
                            "test-bucket",
                            "object",
                            None,
                            Default::default(),
                            &BucketAccess::Permissive,
                        )
                        .await
                        .unwrap();
                    let transfer = worker.begin_derived(vec![source.whole()], 3).await.unwrap();
                    worker
                        .advance_derived(&transfer.id, u64::MAX)
                        .await
                        .unwrap();
                    let mut destination =
                        super::super::derived::DerivedDestination::new(BucketAccess::Permissive);
                    destination.bucket = "test-bucket".into();
                    destination.name = "object".into();
                    destination.content_type = "text/plain".into();
                    worker
                        .publish_derived(&transfer.id, destination, |_, _| Ok(()))
                        .await
                        .unwrap();
                }
                "resumable" => {
                    let session = worker
                        .create_resumable_session(
                            "test-bucket",
                            "object",
                            super::super::ResumableOptions::new(BucketAccess::Permissive),
                            Some(3),
                        )
                        .await
                        .unwrap();
                    finalize_session(&worker, &session.id, b"new")
                        .await
                        .unwrap();
                }
                _ => unreachable!(),
            }
        });
        let selected = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let record = admin.get_object(&bucket, &name, None).await.unwrap();
                if record.generation != old.generation {
                    break record;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!publication.is_finished());
        drop(held);
        publication.await.unwrap();
        cleanup.await.unwrap().unwrap();
        let (_, mut reader) = catalog
            .open_object("test-bucket", "object", None, Default::default())
            .await
            .unwrap();
        let expected: &[u8] = if route == "derived" { b"old" } else { b"new" };
        assert_eq!(
            reader.next_chunk().await.unwrap().unwrap().as_ref(),
            expected
        );
        assert!(reader.next_chunk().await.unwrap().is_none());
        assert_eq!(publication_blobs(&dir.0).len(), 1);
        catalog.close().await.unwrap();
        let reopened = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(
            reopened
                .admin(Origin::Seed)
                .get_object(&bucket, &name, None)
                .await
                .unwrap(),
            selected
        );
        reopened.close().await.unwrap();
    }
}

#[tokio::test]
async fn publication_reclamation_is_paged_and_rejects_live_journal_entries() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    admin
        .publish_object(&bucket, &name, request(None), body(b"retained"))
        .await
        .unwrap();
    let live_path = publication_blobs(&dir.0).pop().unwrap();
    let live_id = live_path.file_name().unwrap().to_str().unwrap().to_owned();
    let data_dir = dir.0.clone();
    catalog
        .run_upload(move |connection| {
            let transaction = connection.transaction().unwrap();
            for index in 0..140 {
                let id = format!("{index:032x}");
                let path = objects::blob_path(&data_dir, &id).unwrap();
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, b"retired").unwrap();
                transaction
                    .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [id])
                    .unwrap();
            }
            transaction.commit().unwrap();
            Ok(())
        })
        .await
        .unwrap();
    catalog.reclaim_after_commit().await;
    assert_eq!(publication_blobs(&dir.0).len(), 13);
    assert_eq!(catalog.0.execution.available_permits(), 1);
    catalog.reclaim_after_commit().await;
    assert_eq!(publication_blobs(&dir.0), vec![live_path.clone()]);
    let id = live_id.clone();
    catalog
        .run_upload(move |connection| {
            connection
                .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [id])
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    catalog.reclaim_after_commit().await;
    assert_eq!(fs::read(live_path).unwrap(), b"retained");
    catalog
        .run_upload(move |connection| {
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            connection
                .execute(
                    "DELETE FROM pending_blob_deletions WHERE blob_id = ?1",
                    [live_id],
                )
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publication_waits_for_reclamation_without_holding_catalog_execution() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    let old = admin
        .publish_object(&bucket, &name, request(None), body(b"old"))
        .await
        .unwrap();
    let cleanup = catalog.0.reclamation.clone().acquire_owned().await.unwrap();
    let worker = admin.clone();
    let target_bucket = bucket.clone();
    let target_name = name.clone();
    let publication = tokio::spawn(async move {
        worker
            .publish_object(&target_bucket, &target_name, request(None), body(b"new"))
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if admin
                .get_object(&bucket, &name, None)
                .await
                .unwrap()
                .generation
                != old.generation
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!publication.is_finished());
    assert_eq!(publication_blobs(&dir.0).len(), 2);
    drop(cleanup);
    publication.await.unwrap().unwrap();
    assert_eq!(publication_blobs(&dir.0).len(), 1);
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_reclamation_retains_directory_ownership_until_sql_settles() {
    let (dir, catalog, _admin, _bucket, _name) = setup().await;
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let worker = catalog.clone();
    let cleanup = tokio::spawn(async move { worker.reclaim_after_commit().await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while catalog.0.reclamation.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cleanup.abort();
    let _ = cleanup.await;
    let worker = catalog.clone();
    let close = tokio::spawn(async move { worker.close().await });
    tokio::task::yield_now().await;
    assert!(!close.is_finished());
    assert!(Catalog::open(&dir.0).await.is_err());
    drop(execution);
    close.await.unwrap().unwrap();
    Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
}

fn bucket(name: &str, project: &str) -> BucketDefinition {
    BucketDefinition::default_for(&BucketName::new(name).unwrap(), project, &SystemClock).unwrap()
}

pub(super) fn request(condition: Option<GenerationCondition>) -> Publication {
    Publication {
        content_type: "text/plain".into(),
        metadata: Default::default(),
        expected: Default::default(),
        condition,
        max_bytes: 1024,
        mode: PublishMode::Replace,
    }
}

pub(super) fn body(bytes: &'static [u8]) -> mpsc::Receiver<Result<UploadChunk, io::Error>> {
    let (sender, receiver) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(bytes))))
        .unwrap();
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    receiver
}

pub(super) async fn setup() -> (TestDir, Catalog, Admin, BucketName, ObjectName) {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let admin = catalog.admin(Origin::Seed);
    admin
        .ensure_bucket(bucket("test-bucket", "project"))
        .await
        .unwrap();
    (
        dir,
        catalog,
        admin,
        BucketName::new("test-bucket").unwrap(),
        ObjectName::new("object").unwrap(),
    )
}

#[test]
fn admin_names_generations_and_permissions_are_validated() {
    for name in ["", "UPPER", "ab", "a/b", "127.0.0.1"] {
        assert!(BucketName::new(name).is_err(), "{name}");
    }
    for name in ["", ".", "..", "bad\nname"] {
        assert!(ObjectName::new(name).is_err(), "{name:?}");
    }
    assert!(ObjectName::new("日本語 /../literal%").is_ok());
    assert!(Generation::new(0).is_err());
    assert!(Generation::new(-1).is_err());
    assert_eq!(Generation::new(i64::MAX).unwrap().get(), i64::MAX);
    assert_eq!(
        crate::auth::Permission::try_from("storage.objects.get").unwrap(),
        crate::auth::Permission::ObjectsGet
    );
    assert!(crate::auth::Permission::try_from("storage.objects.typo").is_err());
    assert_eq!(WAIT_TIMEOUT, Duration::from_secs(30));
}

#[tokio::test]
async fn admin_ensure_batches_preserve_identity_order_and_rollback() {
    let (dir, catalog, admin, name, _) = setup().await;
    let original = admin.get_bucket(&name).await.unwrap().unwrap();
    let changed = bucket("test-bucket", "different-project");
    let result = admin
        .ensure_buckets(vec![bucket("another-bucket", "project"), changed])
        .await
        .unwrap();
    assert!(matches!(&result[0], EnsureBucket::Created(record) if record.name == "another-bucket"));
    assert_eq!(result[1], EnsureBucket::Existing(original.clone()));
    catalog.run(|connection| {
        connection.execute_batch("CREATE TEMP TRIGGER reject_test_bucket BEFORE INSERT ON buckets WHEN NEW.name = 'rejected-bucket' BEGIN SELECT RAISE(ABORT, 'test rejection'); END;").map_err(unavailable)
    }).await.unwrap();
    assert!(
        admin
            .ensure_buckets(vec![
                bucket("rolled-back", "project"),
                bucket("rejected-bucket", "project")
            ])
            .await
            .is_err()
    );
    assert!(
        admin
            .get_bucket(&BucketName::new("rolled-back").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(admin.get_bucket(&name).await.unwrap(), Some(original));
    assert!(
        admin
            .ensure_buckets(vec![
                bucket("duplicate", "project"),
                bucket("duplicate", "project")
            ])
            .await
            .is_err()
    );
    assert!(
        admin
            .ensure_buckets(vec![bucket("overflow", "project"); MAX_BUCKETS + 1])
            .await
            .is_err()
    );
    assert!(admin.ensure_buckets(vec![]).await.unwrap().is_empty());
    let first = admin.list_buckets("project", None, 1).await.unwrap();
    assert!(first.has_more);
    assert_eq!(first.items[0].name, "another-bucket");
    let second = admin
        .list_buckets(
            "project",
            Some(&BucketName::new(first.items[0].name.clone()).unwrap()),
            1,
        )
        .await
        .unwrap();
    assert!(!second.has_more);
    assert_eq!(second.items[0].name, "test-bucket");
    assert!(
        admin
            .list_buckets("project", None, MAX_PAGE + 1)
            .await
            .is_err()
    );
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert!(
        reopened
            .admin(Origin::BucketInit)
            .get_bucket(&name)
            .await
            .unwrap()
            .is_some()
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn admin_ensure_full_batch_and_concurrent_creates_are_atomic() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let admin = catalog.admin(Origin::BucketInit);
    let records = (0..MAX_BUCKETS)
        .map(|index| bucket(&format!("batch-{index:04}"), "project"))
        .collect::<Vec<_>>();
    let (a, b) = tokio::join!(
        admin.ensure_buckets(records.clone()),
        admin.ensure_buckets(records)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.len(), MAX_BUCKETS);
    assert_eq!(b.len(), MAX_BUCKETS);
    assert_eq!(
        a.iter()
            .chain(&b)
            .filter(|record| matches!(record, EnsureBucket::Created(_)))
            .count(),
        MAX_BUCKETS
    );
    assert_eq!(
        a.iter()
            .map(|record| &record.record().name)
            .collect::<Vec<_>>(),
        b.iter()
            .map(|record| &record.record().name)
            .collect::<Vec<_>>()
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn admin_admission_is_independent_of_saturated_api_slots() {
    let (_dir, catalog, admin, bucket, name) = setup().await;
    let api_calls = catalog
        .0
        .admission
        .clone()
        .acquire_many_owned(32)
        .await
        .unwrap();
    let api_uploads: Vec<_> = (0..64)
        .map(|_| catalog.admit_object_upload().unwrap())
        .collect();
    assert!(catalog.get_bucket("test-bucket").await.is_err());
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let mut waiting = std::pin::pin!(admin.publish_object(
        &bucket,
        &name,
        request(Some(GenerationCondition::Absent)),
        body(b"admin")
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut waiting)
            .await
            .is_err()
    );
    drop(execution);
    let record = waiting.await.unwrap();
    assert_eq!(record.size, 5);
    assert_eq!(admin.origin(), Origin::Seed);
    assert_eq!(
        admin.get_object(&bucket, &name, None).await.unwrap(),
        record
    );
    let page = admin
        .list_objects(&bucket, Default::default(), 1)
        .await
        .unwrap();
    assert_eq!(page.items, [record]);
    assert!(
        admin
            .list_objects(&bucket, Default::default(), MAX_PAGE + 1)
            .await
            .is_err()
    );
    assert!(
        admin
            .list_objects(
                &bucket,
                super::super::ObjectListQuery {
                    bucket: "different".into(),
                    ..Default::default()
                },
                1
            )
            .await
            .is_err()
    );
    drop(api_calls);
    drop(api_uploads);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn admin_wait_deadlines_queue_limit_and_closed_state_are_typed() {
    let (_dir, catalog, admin, bucket, _) = setup().await;
    assert!(matches!(
        catalog
            .wait_admin(catalog.0.admin_reads.clone(), tokio::time::Instant::now())
            .await,
        Err(AdminError::Busy { .. })
    ));
    let held = catalog
        .0
        .admin_uploads
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let error = catalog
        .wait_admin(
            catalog.0.admin_uploads.clone(),
            tokio::time::Instant::now() + Duration::from_millis(20),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, AdminError::Busy { .. }));
    drop(held);
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    assert!(matches!(
        catalog
            .run_admin_until(
                tokio::time::Instant::now() + Duration::from_millis(20),
                |_| Ok(())
            )
            .await,
        Err(AdminError::Busy { .. })
    ));
    drop(execution);
    let queue = catalog
        .0
        .admin_waiters
        .clone()
        .acquire_many_owned(32)
        .await
        .unwrap();
    assert!(matches!(
        admin.get_bucket(&bucket).await,
        Err(AdminError::Busy { .. })
    ));
    drop(queue);
    catalog.close().await.unwrap();
    assert!(!catalog.is_healthy());
    assert_eq!(
        admin.get_bucket(&bucket).await.unwrap_err(),
        AdminError::Closed
    );
}

#[tokio::test]
async fn admin_publication_times_out_at_the_public_admission_deadline() {
    let (_dir, catalog, admin, bucket, name) = setup().await;
    let slots = catalog
        .0
        .admin_uploads
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        WAIT_TIMEOUT + Duration::from_secs(2),
        admin.publish_object(&bucket, &name, request(None), body(b"timeout")),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(AdminError::Busy { .. })));
    assert!(started.elapsed() >= WAIT_TIMEOUT);
    drop(slots);
    assert_eq!(
        admin.get_object(&bucket, &name, None).await.unwrap_err(),
        AdminError::NotFound
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn admin_cancelled_ensure_drains_the_owned_sql_job_before_unlock() {
    let (dir, catalog, admin, _, _) = setup().await;
    let blocker = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let ensure = tokio::spawn(async move {
        admin
            .ensure_bucket(bucket("pending-bucket", "project"))
            .await
    });
    wait_for(|| catalog.0.execution.available_permits() == 0).await;
    ensure.abort();
    let _ = ensure.await;
    let closer = catalog.clone();
    let mut close = tokio::spawn(async move { closer.close().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut close)
            .await
            .is_err()
    );
    assert!(Catalog::open(&dir.0).await.is_err());
    blocker.execute_batch("COMMIT").unwrap();
    drop(blocker);
    close.await.unwrap().unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert!(
        reopened
            .admin(Origin::BucketInit)
            .get_bucket(&BucketName::new("pending-bucket").unwrap())
            .await
            .unwrap()
            .is_some()
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn admin_replace_preserves_conditions_checksums_metadata_and_versions() {
    let (_dir, catalog, admin, bucket, name) = setup().await;
    catalog
        .patch_bucket(
            bucket.as_str(),
            crate::bucket::BucketMetadataUpdate {
                versioning_enabled: Some(true),
                ..Default::default()
            },
            None,
            None,
            &BucketAccess::Admin(admin.grant.clone()),
        )
        .await
        .unwrap();
    let mut first_request = request(Some(GenerationCondition::Absent));
    first_request.metadata.metadata = Some(Some(BTreeMap::from([(
        "custom".into(),
        Some("value".into()),
    )])));
    let first = admin
        .publish_object(&bucket, &name, first_request, body(b"original"))
        .await
        .unwrap();
    assert_eq!(first.metadata["custom"], "value");
    assert_eq!(first.content_type, "text/plain");
    assert_eq!(
        admin
            .publish_object(
                &bucket,
                &name,
                request(Some(GenerationCondition::Absent)),
                body(b"wrong")
            )
            .await
            .unwrap_err(),
        AdminError::Precondition
    );
    let mut invalid_hash = request(None);
    invalid_hash.expected.md5 = Some([0; 16]);
    assert_eq!(
        admin
            .publish_object(&bucket, &name, invalid_hash, body(b"wrong"))
            .await
            .unwrap_err(),
        AdminError::Checksum
    );
    let condition = GenerationCondition::Is(Generation::new(first.generation).unwrap());
    let (a, b) = tokio::join!(
        admin.publish_object(&bucket, &name, request(Some(condition)), body(b"next-a")),
        admin.publish_object(&bucket, &name, request(Some(condition)), body(b"next-b"))
    );
    let next = match (a, b) {
        (Ok(record), Err(AdminError::Precondition))
        | (Err(AdminError::Precondition), Ok(record)) => record,
        other => panic!("unexpected outcomes: {other:?}"),
    };
    assert!(next.generation > first.generation);
    assert!(next.metadata.is_empty());
    let (selected, mut reader) = admin
        .open_object(
            &bucket,
            &name,
            Some(Generation::new(first.generation).unwrap()),
        )
        .await
        .unwrap();
    assert!(selected.time_deleted.is_some());
    let mut archived = first.clone();
    archived.time_deleted = selected.time_deleted;
    assert_eq!(selected, archived);
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"original"[..]);
    assert_eq!(
        admin
            .delete_object(&bucket, &name, condition)
            .await
            .unwrap_err(),
        AdminError::Precondition
    );
    admin
        .delete_object(
            &bucket,
            &name,
            GenerationCondition::Is(Generation::new(next.generation).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(
        admin.get_object(&bucket, &name, None).await.unwrap_err(),
        AdminError::NotFound
    );
    assert_eq!(
        admin
            .get_object(
                &bucket,
                &name,
                Some(Generation::new(first.generation).unwrap())
            )
            .await
            .unwrap(),
        archived
    );
    assert!(
        catalog
            .bucket_access(bucket.as_str(), None, crate::auth::OBJECTS_GET)
            .await
            .is_err()
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn admin_holds_and_disk_full_keep_their_error_types() {
    let (_dir, catalog, admin, bucket, name) = setup().await;
    catalog
        .patch_bucket(
            bucket.as_str(),
            crate::bucket::BucketMetadataUpdate {
                default_event_based_hold: Some(true),
                ..Default::default()
            },
            None,
            None,
            &BucketAccess::Admin(admin.grant.clone()),
        )
        .await
        .unwrap();
    let record = admin
        .publish_object(&bucket, &name, request(None), body(b"held"))
        .await
        .unwrap();
    assert!(record.event_based_hold);
    assert_eq!(
        admin
            .publish_object(&bucket, &name, request(None), body(b"replacement"))
            .await
            .unwrap_err(),
        AdminError::Hold
    );
    assert_eq!(
        admin
            .delete_object(
                &bucket,
                &name,
                GenerationCondition::Is(Generation::new(record.generation).unwrap())
            )
            .await
            .unwrap_err(),
        AdminError::Hold
    );
    let (sender, receiver) = mpsc::channel(1);
    sender
        .try_send(Err(io::Error::from(io::ErrorKind::StorageFull)))
        .unwrap();
    drop(sender);
    assert_eq!(
        admin
            .publish_object(
                &bucket,
                &ObjectName::new("disk-full").unwrap(),
                request(None),
                receiver
            )
            .await
            .unwrap_err(),
        AdminError::DiskFull
    );
    catalog
        .run(|connection| {
            let pages: i64 = connection
                .pragma_query_value(None, "page_count", |row| row.get(0))
                .map_err(unavailable)?;
            connection
                .pragma_update(None, "max_page_count", pages + 2)
                .map_err(unavailable)
        })
        .await
        .unwrap();
    let records = (0..MAX_BUCKETS)
        .map(|index| {
            let mut input = bucket_definition(index);
            input.0.labels = (0..64)
                .map(|label| (format!("label-{label}"), "x".repeat(63)))
                .collect();
            input
        })
        .collect();
    assert_eq!(
        admin.ensure_buckets(records).await.unwrap_err(),
        AdminError::DiskFull
    );
    assert!(
        admin
            .get_bucket(&BucketName::new("full-0000").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        admin.get_object(&bucket, &name, None).await.unwrap(),
        record
    );
    catalog.close().await.unwrap();
}

fn bucket_definition(index: usize) -> BucketDefinition {
    bucket(&format!("full-{index:04}"), "project")
}

async fn wait_for(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn admin_cancelled_body_cleans_staging_before_unlock() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    let (sender, receiver) = mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(Bytes::from_static(b"partial"))))
        .await
        .unwrap();
    let publish = tokio::spawn(async move {
        admin
            .publish_object(&bucket, &name, request(None), receiver)
            .await
    });
    wait_for(|| {
        std::fs::read_dir(dir.0.join("staging"))
            .unwrap()
            .any(|entry| live_len(&entry.unwrap().path()) > 0)
    })
    .await;
    publish.abort();
    let _ = publish.await;
    drop(sender);
    catalog.close().await.unwrap();
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        reopened
            .admin(Origin::Import)
            .get_object(
                &BucketName::new("test-bucket").unwrap(),
                &ObjectName::new("object").unwrap(),
                None
            )
            .await
            .unwrap_err(),
        AdminError::NotFound
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn admin_shutdown_holds_lock_through_renamed_publication() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    let (sender, receiver) = mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(Bytes::from_static(b"committed"))))
        .await
        .unwrap();
    let publish = tokio::spawn(async move {
        admin
            .publish_object(&bucket, &name, request(None), receiver)
            .await
    });
    wait_for(|| {
        std::fs::read_dir(dir.0.join("staging"))
            .unwrap()
            .any(|entry| live_len(&entry.unwrap().path()) > 0)
    })
    .await;
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    sender.send(Ok(UploadChunk::End)).await.unwrap();
    drop(sender);
    wait_for(|| {
        std::fs::read_dir(dir.0.join("blobs"))
            .unwrap()
            .any(|entry| std::fs::read_dir(entry.unwrap().path()).unwrap().count() > 0)
    })
    .await;
    publish.abort();
    let _ = publish.await;
    let closer = catalog.clone();
    let mut close = tokio::spawn(async move { closer.close().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut close)
            .await
            .is_err()
    );
    assert!(Catalog::open(&dir.0).await.is_err());
    drop(execution);
    close.await.unwrap().unwrap();
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    let reopened = Catalog::open(&dir.0).await.unwrap();
    let (_, mut reader) = reopened
        .admin(Origin::Seed)
        .open_object(
            &BucketName::new("test-bucket").unwrap(),
            &ObjectName::new("object").unwrap(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        reader.next_chunk().await.unwrap().unwrap(),
        b"committed"[..]
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn admin_invalid_metadata_cannot_replace_a_readable_generation() {
    let (_dir, catalog, admin, bucket, name) = setup().await;
    let original = admin
        .publish_object(&bucket, &name, request(None), body(b"original"))
        .await
        .unwrap();
    for field in [
        "contentType",
        "contentEncoding",
        "contentDisposition",
        "contentLanguage",
        "cacheControl",
        "custom-key",
        "direct-content-type",
    ] {
        let mut publication = request(None);
        match field {
            "direct-content-type" => {
                publication.content_type = "text/plain\r\nInjected: yes".into()
            }
            "contentType" => publication.metadata.content_type = Some(Some("text/plain\n".into())),
            "contentEncoding" => {
                publication.metadata.content_encoding = Some(Some("gzip\t".into()))
            }
            "contentDisposition" => {
                publication.metadata.content_disposition = Some(Some("inline\0".into()))
            }
            "contentLanguage" => {
                publication.metadata.content_language = Some(Some("x".repeat(101)))
            }
            "cacheControl" => {
                publication.metadata.cache_control = Some(Some("public\u{7f}".into()))
            }
            "custom-key" => {
                publication.metadata.metadata = Some(Some(BTreeMap::from([(
                    String::new(),
                    Some("value".into()),
                )])))
            }
            _ => unreachable!(),
        }
        assert!(
            matches!(
                admin
                    .publish_object(&bucket, &name, publication, body(b"replacement"))
                    .await,
                Err(AdminError::Invalid(_))
            ),
            "{field}"
        );
        assert_eq!(
            admin.get_object(&bucket, &name, None).await.unwrap(),
            original
        );
        let (_, mut reader) = admin.open_object(&bucket, &name, None).await.unwrap();
        assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"original"[..]);
    }
    assert!(matches!(
        catalog
            .put_object(
                bucket.as_str(),
                name.as_str(),
                "bad\r\nheader".into(),
                Default::default(),
                Default::default(),
                1024,
                body(b"bad")
            )
            .await,
        Err(ObjectError::InvalidFixedMetadata(_))
    ));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn admin_corrupt_sqlite_and_stored_values_remain_typed() {
    let (_dir, catalog, admin, bucket, name) = setup().await;
    admin
        .publish_object(&bucket, &name, request(None), body(b"original"))
        .await
        .unwrap();
    for code in [rusqlite::ffi::SQLITE_CORRUPT, rusqlite::ffi::SQLITE_NOTADB] {
        let result: Result<(), AdminError> = catalog
            .run_admin(move |_| {
                Err(unavailable(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(code),
                    None,
                )))
            })
            .await;
        assert!(matches!(result, Err(AdminError::Corrupt(_))));
    }
    catalog
        .run(|connection| {
            connection
                .execute("UPDATE buckets SET labels = 'not-json'", [])
                .map(|_| ())
                .map_err(unavailable)
        })
        .await
        .unwrap();
    assert!(matches!(
        admin.get_bucket(&bucket).await,
        Err(AdminError::Corrupt(_))
    ));
    assert!(matches!(
        admin.list_buckets("project", None, 1).await,
        Err(AdminError::Corrupt(_))
    ));
    catalog
        .run(|connection| {
            connection
                .execute_batch(
                    "UPDATE buckets SET labels = '{}'; UPDATE objects SET metadata = 'not-json'",
                )
                .map_err(unavailable)
        })
        .await
        .unwrap();
    assert!(matches!(
        admin.get_object(&bucket, &name, None).await,
        Err(AdminError::Corrupt(_))
    ));
    assert!(matches!(
        admin.open_object(&bucket, &name, None).await,
        Err(AdminError::Corrupt(_))
    ));
    assert!(matches!(
        admin.list_objects(&bucket, Default::default(), 1).await,
        Err(AdminError::Corrupt(_))
    ));
    catalog.run(|connection| connection.execute_batch("UPDATE objects SET metadata = '{}'; UPDATE bucket_policies SET bindings = 'not-json'").map_err(unavailable)).await.unwrap();
    assert!(matches!(
        admin.open_object(&bucket, &name, None).await,
        Err(AdminError::Corrupt(_))
    ));
    catalog.close().await.unwrap();
}

async fn fail_retirement_with_disk_full(catalog: &Catalog) {
    catalog.run(|connection| {
        connection.execute_batch("CREATE TEMP TRIGGER full_retirement BEFORE DELETE ON objects BEGIN UPDATE buckets SET labels = hex(zeroblob(65536)) WHERE name = 'test-bucket'; END;").map_err(unavailable)?;
        let pages: i64 = connection.pragma_query_value(None, "page_count", |row| row.get(0)).map_err(unavailable)?;
        connection.pragma_update(None, "max_page_count", pages + 2).map_err(unavailable)
    }).await.unwrap();
}

async fn allow_retirement(catalog: &Catalog) {
    catalog
        .run(|connection| {
            connection
                .execute_batch("DROP TRIGGER full_retirement")
                .map_err(unavailable)?;
            connection
                .pragma_update(None, "max_page_count", 1_073_741_823)
                .map_err(unavailable)
        })
        .await
        .unwrap();
}

async fn finalize_session(
    catalog: &Catalog,
    id: &str,
    bytes: &'static [u8],
) -> Result<Option<serde_json::Value>, super::super::ResumableError> {
    let lease = catalog
        .lock_resumable_session(id, "test-bucket")
        .await
        .unwrap();
    let (catalog_wait, _) = tokio::sync::watch::channel(false);
    let chunk = super::super::ResumableChunk {
        start: 0,
        length: bytes.len() as u64,
        total: Some(bytes.len() as u64),
        finalize: true,
        open_end: false,
        options: lease.session.options.clone(),
        catalog_wait,
    };
    catalog
        .write_resumable_session(
            lease,
            chunk,
            body(bytes),
            catalog.admit_object_upload().unwrap(),
            |record| {
                crate::json::object_resource(
                    record,
                    "http://localhost",
                    crate::json::Projection::NoAcl,
                )
                .map_err(ObjectError::backend)
            },
        )
        .await
}

#[tokio::test]
async fn disk_full_during_resumable_retirement_preserves_the_same_session_and_bytes() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    let original = admin
        .publish_object(&bucket, &name, request(None), body(b"original"))
        .await
        .unwrap();
    let mut options = super::super::ResumableOptions::new(BucketAccess::Permissive);
    options.content_type = "text/plain".into();
    let session = catalog
        .create_resumable_session(bucket.as_str(), name.as_str(), options, Some(5))
        .await
        .unwrap();
    fail_retirement_with_disk_full(&catalog).await;
    assert_eq!(
        finalize_session(&catalog, &session.id, b"hello")
            .await
            .unwrap_err(),
        super::super::ResumableError::Object(ObjectError::Io(io::ErrorKind::StorageFull.into()))
    );
    let retained = catalog
        .resumable_session(&session.id, bucket.as_str())
        .await
        .unwrap();
    assert_eq!(retained.state, super::super::SessionState::Active);
    assert_eq!(retained.offset, 5);
    assert_eq!(
        std::fs::read(dir.0.join("staging").join(&session.blob_id)).unwrap(),
        b"hello"
    );
    assert_eq!(
        admin.get_object(&bucket, &name, None).await.unwrap(),
        original
    );
    allow_retirement(&catalog).await;
    assert!(
        finalize_session(&catalog, &session.id, b"hello")
            .await
            .unwrap()
            .is_some()
    );
    let (_, mut reader) = admin.open_object(&bucket, &name, None).await.unwrap();
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"hello"[..]);
    assert!(
        admin
            .get_object(&bucket, &name, None)
            .await
            .unwrap()
            .generation
            > original.generation
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn disk_full_during_derived_retirement_preserves_the_same_transfer_and_bytes() {
    let (_dir, catalog, admin, bucket, name) = setup().await;
    let original = admin
        .publish_object(&bucket, &name, request(None), body(b"original"))
        .await
        .unwrap();
    admin
        .publish_object(
            &bucket,
            &ObjectName::new("source").unwrap(),
            request(None),
            body(b"new bytes"),
        )
        .await
        .unwrap();
    let source = catalog
        .resolve_derived_source(
            bucket.as_str(),
            "source",
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    let transfer = catalog
        .begin_derived(vec![source.whole()], 1024)
        .await
        .unwrap();
    catalog
        .advance_derived(&transfer.id, u64::MAX)
        .await
        .unwrap();
    let destination = || super::super::DerivedDestination {
        bucket: bucket.as_str().into(),
        name: name.as_str().into(),
        content_type: "text/plain".into(),
        ..super::super::DerivedDestination::new(BucketAccess::Permissive)
    };
    fail_retirement_with_disk_full(&catalog).await;
    assert_eq!(
        catalog
            .publish_derived(&transfer.id, destination(), |_, _| Ok(()))
            .await
            .unwrap_err(),
        ObjectError::Io(io::ErrorKind::StorageFull.into())
    );
    assert_eq!(catalog.get_derived(&transfer.id).await.unwrap().offset, 9);
    assert_eq!(
        admin.get_object(&bucket, &name, None).await.unwrap(),
        original
    );
    allow_retirement(&catalog).await;
    catalog.advance_derived(&transfer.id, 0).await.unwrap();
    let published = catalog
        .publish_derived(&transfer.id, destination(), |_, _| Ok(()))
        .await
        .unwrap();
    assert!(published.generation > original.generation);
    let (_, mut reader) = admin.open_object(&bucket, &name, None).await.unwrap();
    assert_eq!(
        reader.next_chunk().await.unwrap().unwrap(),
        b"new bytes"[..]
    );
    catalog.close().await.unwrap();
}
