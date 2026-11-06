use super::*;
use crate::storage::ObjectUpload;
use crate::storage::test_fixtures::{PERMISSIVE, live, permissive_upload, version};
use crate::{
    clock::SystemClock,
    test_support::{TestDir, eventually_async, eventually_some_async, live_len},
};
use bytes::Bytes;
use std::collections::BTreeMap;
use std::{fs, time::Duration};

const BATCH: usize = 1000;

#[test]
fn administrative_diagnostics_keep_their_existing_messages() {
    for (error, expected) in [
        (
            AdminError::Busy {
                retry_after: Duration::from_secs(2),
            },
            "administrative operation: Busy { retry_after: 2s }",
        ),
        (
            AdminError::DiskFull(io::ErrorKind::StorageFull.into()),
            "administrative operation: DiskFull",
        ),
        (AdminError::Closed, "administrative operation: Closed"),
        (
            AdminError::Precondition,
            "administrative operation: Precondition",
        ),
        (AdminError::Hold, "administrative operation: Hold"),
        (
            AdminError::SeedProjectMismatch,
            "administrative operation: Invalid(\"existing seed bucket belongs to another project\")",
        ),
        (
            AdminError::SourceChanged {
                fields: vec!["size".into()],
            },
            r#"administrative operation: SourceChanged { fields: ["size"] }"#,
        ),
        (
            AdminError::Corrupt("row".into()),
            r#"administrative operation: Corrupt("row")"#,
        ),
        (
            AdminError::Io(io::ErrorKind::PermissionDenied.into()),
            "administrative operation: Io(PermissionDenied)",
        ),
        (AdminError::NotFound, "administrative operation: NotFound"),
        (
            AdminError::Invalid("input".into()),
            r#"administrative operation: Invalid("input")"#,
        ),
        (AdminError::Checksum, "administrative operation: Checksum"),
        (AdminError::TooLarge, "administrative operation: TooLarge"),
        (AdminError::Forbidden, "administrative operation: Forbidden"),
        (
            AdminError::BodyAborted("body".into()),
            r#"administrative operation: BodyAborted("body")"#,
        ),
        (
            AdminError::Backend("database".into()),
            r#"administrative operation: Backend("database")"#,
        ),
    ] {
        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn administrative_failures_retain_io_and_corruption_causes() {
    let io_failure = || io::Error::new(io::ErrorKind::PermissionDenied, "denied fixture");
    for error in [
        AdminError::from(io_failure()),
        AdminError::from(CatalogError::Io(io_failure().into())),
        AdminError::from(ObjectError::Io(io_failure().into())),
    ] {
        let cause = std::error::Error::source(&error)
            .unwrap()
            .downcast_ref::<io::Error>()
            .unwrap();
        assert_eq!(cause.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(cause.to_string(), "denied fixture");
    }
    let invalid = || "bad".parse::<u64>().unwrap_err();
    for error in [
        AdminError::from(CatalogError::Corrupt(BackendCause::from_error(invalid()))),
        AdminError::from(ObjectError::Corrupt(BackendCause::from_error(invalid()))),
    ] {
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .is::<std::num::ParseIntError>()
        );
    }
}

#[test]
fn synchronization_diagnostics_summarize_wrapped_causes() {
    use crate::storage::sync_state::{
        conflicts::Error as ConflictError, root::RootError, writeback::Error as WritebackError,
    };
    for (error, expected) in [
        (
            RootError::Admin(AdminError::Hold),
            "synchronization root: admin operation failed",
        ),
        (RootError::InUse, "synchronization root: InUse"),
        (RootError::Unsupported, "synchronization root: Unsupported"),
        (RootError::RootChanged, "synchronization root: RootChanged"),
        (
            RootError::MissingMarker,
            "synchronization root: MissingMarker",
        ),
        (
            RootError::InvalidMarker,
            "synchronization root: InvalidMarker",
        ),
        (
            RootError::ForeignMarker,
            "synchronization root: ForeignMarker",
        ),
        (
            RootError::ReinitializeDenied,
            "synchronization root: ReinitializeDenied",
        ),
    ] {
        assert_eq!(error.to_string(), expected);
    }
    for (error, expected) in [
        (
            ConflictError::Admin(AdminError::Corrupt("row".into())),
            r#"sync conflict: Admin(Corrupt("row"))"#,
        ),
        (
            ConflictError::Root(RootError::InUse),
            "sync conflict: Root(InUse)",
        ),
        (ConflictError::Capacity, "sync conflict: Capacity"),
        (
            ConflictError::RevisionMismatch,
            "sync conflict: RevisionMismatch",
        ),
        (ConflictError::TargetChanged, "sync conflict: TargetChanged"),
        (
            ConflictError::OperationIdReused,
            "sync conflict: OperationIdReused",
        ),
        (
            ConflictError::InvalidResolution,
            "sync conflict: InvalidResolution",
        ),
    ] {
        assert_eq!(error.to_string(), expected);
    }
    for (error, expected) in [
        (
            WritebackError::Admin(AdminError::Io(io::ErrorKind::PermissionDenied.into())),
            "write-back: admin operation failed",
        ),
        (
            WritebackError::Root(RootError::InUse),
            "write-back: root validation failed",
        ),
        (WritebackError::Capacity, "write-back: Capacity"),
        (
            WritebackError::RecoveryRequired,
            "write-back: RecoveryRequired",
        ),
        (WritebackError::Protected, "write-back: Protected"),
        (
            WritebackError::WorkspaceChanged,
            "write-back: WorkspaceChanged",
        ),
    ] {
        assert_eq!(error.to_string(), expected);
    }
    let error = WritebackError::Admin(AdminError::Io(io::ErrorKind::PermissionDenied.into()));
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<AdminError>()
    );
    let error = RootError::Admin(AdminError::Hold);
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<AdminError>()
    );
}

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
    let stage = session.blob_id.staging_path(&dir.0);
    fs::remove_file(&stage).unwrap();
    fs::create_dir(&stage).unwrap();
    let lease = catalog
        .lock_resumable_session(&session.id, "test-bucket")
        .await
        .unwrap();
    catalog.invalidate_resumable_session(&lease).await.unwrap();
    catalog.reclaim_pending().await.unwrap();
    let id = session.blob_id;
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
                    let upload = permissive_upload("test-bucket", "object", 1024);
                    worker.put_object(upload, body(b"new")).await.unwrap();
                }
                "derived" => {
                    let object = live("test-bucket", "object");
                    let source = worker
                        .resolve_derived_source(object, Default::default(), PERMISSIVE)
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
        let selected = eventually_some_async("a new generation to be selected", || async {
            let record = admin.get_object(&bucket, &name, None).await.unwrap();
            (record.generation != old.generation).then_some(record)
        })
        .await;
        assert!(!publication.is_finished());
        drop(held);
        publication.await.unwrap();
        cleanup.await.unwrap().unwrap();
        let (_, mut reader) = catalog
            .open_object(
                live("test-bucket", "object"),
                Default::default(),
                PERMISSIVE,
            )
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
                let path = id
                    .parse::<crate::storage::BlobId>()
                    .unwrap()
                    .blob_path(&data_dir);
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
    eventually_async("a new generation to be selected", || async {
        admin
            .get_object(&bucket, &name, None)
            .await
            .unwrap()
            .generation
            != old.generation
    })
    .await;
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
    eventually_async("reclamation to hold its permit", || async {
        catalog.0.reclamation.available_permits() == 0
    })
    .await;
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
        crate::auth::Permission::parse_testable("storage.objects.get").unwrap(),
        crate::auth::Permission::ObjectsGet
    );
    assert!(crate::auth::Permission::parse_testable("storage.objects.typo").is_none());
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
    let records = (0..BATCH)
        .map(|index| bucket(&format!("batch-{index:04}"), "project"))
        .collect::<Vec<_>>();
    let (a, b) = tokio::join!(
        admin.ensure_buckets(records.clone()),
        admin.ensure_buckets(records)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.len(), BATCH);
    assert_eq!(b.len(), BATCH);
    assert_eq!(
        a.iter()
            .chain(&b)
            .filter(|record| matches!(record, EnsureBucket::Created(_)))
            .count(),
        BATCH
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

/// The initialization file enforces `max_buckets_per_batch`, so the catalog must not cut in with
/// a count of its own: a batch past 1000 buckets would otherwise still stop here, at startup.
#[tokio::test]
async fn admin_ensure_accepts_a_batch_past_a_thousand_buckets_and_finds_it_after_reopening() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let records = (0..=BATCH)
        .map(|index| bucket(&format!("batch-{index:04}"), "project"))
        .collect::<Vec<_>>();
    let created = catalog
        .admin(Origin::BucketInit)
        .ensure_buckets(records.clone())
        .await
        .unwrap();
    assert_eq!(created.len(), BATCH + 1);
    assert!(
        created
            .iter()
            .all(|record| matches!(record, EnsureBucket::Created(_)))
    );
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    let existing = reopened
        .admin(Origin::BucketInit)
        .ensure_buckets(records)
        .await
        .unwrap();
    assert_eq!(existing.len(), BATCH + 1);
    assert!(
        existing
            .iter()
            .all(|record| matches!(record, EnsureBucket::Existing(_)))
    );
    reopened.close().await.unwrap();
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
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            catalog.get_bucket("test-bucket", PERMISSIVE)
        )
        .await
        .is_err(),
        "API callers wait while the slots are taken"
    );
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
async fn admin_wait_deadlines_and_closed_state_are_typed() {
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
    catalog.close().await.unwrap();
    assert!(!catalog.is_healthy());
    assert_eq!(
        admin.get_bucket(&bucket).await.unwrap_err(),
        AdminError::Closed
    );
}

/// Twice the 32 callers the management line once held before refusing the next one.
const MANY_CALLS: usize = 64;

fn spawn_gets(
    admin: &Admin,
    bucket: &BucketName,
) -> Vec<tokio::task::JoinHandle<Result<Option<BucketRecord>, AdminError>>> {
    (0..MANY_CALLS)
        .map(|_| {
            let (admin, bucket) = (admin.clone(), bucket.clone());
            tokio::spawn(async move { admin.get_bucket(&bucket).await })
        })
        .collect()
}

#[tokio::test]
async fn management_calls_past_every_slot_wait_their_turn_and_all_succeed() {
    let (_dir, catalog, admin, bucket, _) = setup().await;
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let calls = spawn_gets(&admin, &bucket);
    eventually_async("the read slots to fill", || async {
        catalog.0.admin_reads.available_permits() == 0
    })
    .await;
    drop(execution);
    for call in calls {
        assert!(call.await.unwrap().unwrap().is_some());
    }
    assert_eq!(catalog.0.admin_reads.available_permits(), 32);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn closing_wakes_every_management_call_waiting_for_a_slot() {
    let (_dir, catalog, admin, bucket, _) = setup().await;
    let reads = catalog
        .0
        .admin_reads
        .clone()
        .acquire_many_owned(32)
        .await
        .unwrap();
    let calls = spawn_gets(&admin, &bucket);
    tokio::task::yield_now().await;
    let worker = catalog.clone();
    let close = tokio::spawn(async move { worker.close().await });
    // The calls' own deadline is 30 s away, so only the close can end their wait this soon.
    for call in calls {
        let result = tokio::time::timeout(Duration::from_secs(10), call)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err(), AdminError::Closed);
    }
    assert!(!close.is_finished());
    drop(reads);
    close.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_cancelled_management_call_gives_up_its_place_and_nothing_else() {
    let (_dir, catalog, admin, bucket, _) = setup().await;
    let reads = catalog
        .0
        .admin_reads
        .clone()
        .acquire_many_owned(32)
        .await
        .unwrap();
    let mut calls = spawn_gets(&admin, &bucket);
    let last = calls.pop().unwrap();
    tokio::task::yield_now().await;
    for call in &calls {
        call.abort();
    }
    for call in calls {
        assert!(call.await.unwrap_err().is_cancelled());
    }
    drop(reads);
    assert!(last.await.unwrap().unwrap().is_some());
    assert_eq!(catalog.0.admin_reads.available_permits(), 32);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn admin_publication_times_out_at_the_configured_catalog_wait() {
    const WAIT: Duration = Duration::from_millis(50);
    let (_dir, catalog, admin, bucket, name) = setup().await;
    catalog.apply_session_limits(&crate::config::Config {
        catalog_wait_timeout: crate::time_limit::TimeLimit::from_millis(50),
        ..Default::default()
    });
    let slots = catalog
        .0
        .admin_uploads
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        WAIT + Duration::from_secs(10),
        admin.publish_object(&bucket, &name, request(None), body(b"timeout")),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(AdminError::Busy { .. })));
    assert!(started.elapsed() >= WAIT);
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
    eventually_async("the ensure to hold the execution permit", || async {
        catalog.0.execution.available_permits() == 0
    })
    .await;
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
            Default::default(),
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
    let anonymous = catalog
        .bucket_access(bucket.as_str(), None, crate::auth::OBJECTS_GET)
        .await
        .unwrap();
    assert!(matches!(
        catalog
            .get_object(
                version(bucket.as_str(), name.as_str(), first.generation),
                Default::default(),
                &anonymous
            )
            .await,
        Err(crate::object::ObjectError::Forbidden)
    ));
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
            Default::default(),
            &BucketAccess::Admin(admin.grant.clone()),
        )
        .await
        .unwrap();
    let record = admin
        .publish_object(&bucket, &name, request(None), body(b"held"))
        .await
        .unwrap();
    assert_eq!(record.event_based_hold.as_bool(), Some(true));
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
        AdminError::DiskFull(io::ErrorKind::StorageFull.into())
    );
    catalog
        .run(|connection| {
            let pages: i64 = connection.pragma_query_value(None, "page_count", |row| row.get(0))?;
            connection
                .pragma_update(None, "max_page_count", pages + 2)
                .map_err(unavailable)
        })
        .await
        .unwrap();
    let records = (0..BATCH)
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
        AdminError::DiskFull(io::ErrorKind::StorageFull.into())
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
    eventually_async("the publication to stage bytes", || async {
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
    eventually_async("the publication to stage bytes", || async {
        std::fs::read_dir(dir.0.join("staging"))
            .unwrap()
            .any(|entry| live_len(&entry.unwrap().path()) > 0)
    })
    .await;
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    sender.send(Ok(UploadChunk::End)).await.unwrap();
    drop(sender);
    eventually_async("the publication to rename its blob", || async {
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
                ObjectUpload {
                    content_type: "bad\r\nheader".into(),
                    ..permissive_upload(bucket.as_str(), name.as_str(), 1024)
                },
                body(b"bad")
            )
            .await,
        Err(ObjectError::Metadata(
            crate::metadata_error::MetadataError::FixedHeader(_)
        ))
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
        connection.execute_batch("CREATE TEMP TRIGGER full_retirement BEFORE DELETE ON objects BEGIN UPDATE buckets SET labels = hex(zeroblob(65536)) WHERE name = 'test-bucket'; END;")?;
        let pages: i64 = connection.pragma_query_value(None, "page_count", |row| row.get(0))?;
        connection.pragma_update(None, "max_page_count", pages + 2).map_err(unavailable)
    }).await.unwrap();
}

async fn allow_retirement(catalog: &Catalog) {
    catalog
        .run(|connection| {
            connection.execute_batch("DROP TRIGGER full_retirement")?;
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
        std::fs::read(session.blob_id.staging_path(&dir.0)).unwrap(),
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
    let object = live(bucket.as_str(), "source");
    let source = catalog
        .resolve_derived_source(object, Default::default(), PERMISSIVE)
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
#[test]
fn disk_full_preserves_the_original_io_error() {
    let error = AdminError::from(io::Error::new(
        io::ErrorKind::StorageFull,
        "staging sync failed",
    ));
    let cause = crate::error::causes(&error)
        .find_map(|cause| cause.downcast_ref::<io::Error>())
        .unwrap();
    assert_eq!(cause.kind(), io::ErrorKind::StorageFull);
    assert_eq!(cause.to_string(), "staging sync failed");
}
