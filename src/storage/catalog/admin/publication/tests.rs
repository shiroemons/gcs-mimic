use super::super::tests::{body, request, setup};
use super::*;
use crate::object::Patch;
use crate::storage::test_fixtures::{PERMISSIVE, live};
use crate::test_support::{eventually_async, eventually_some_async};
use std::io::{Seek, SeekFrom};
use std::{collections::BTreeMap, fs};

#[tokio::test]
async fn nested_file_jobs_fail_without_consuming_another_worker() {
    let workers = Arc::new(FileWorkers::new().unwrap());
    let runtime = tokio::runtime::Handle::current();
    let nested = workers.clone();
    let error: AdminError = workers
        .run(move || runtime.block_on(nested.run(|| Ok::<_, AdminError>(()))))
        .await
        .unwrap_err();
    assert!(crate::error::causes(&error).any(|cause| cause.is::<NestedFileJob>()));
    let error = workers
        .run::<(), AdminError>(|| panic!("injected file job panic"))
        .await;
    assert!(error.is_err());
    let (first, second) = tokio::join!(
        workers.run(|| Ok::<_, AdminError>(1)),
        workers.run(|| Ok::<_, AdminError>(2))
    );
    assert_eq!((first.unwrap(), second.unwrap()), (1, 2));
}

fn source(dir: &std::path::Path, bytes: &[u8]) -> File {
    let path = dir.join("source");
    fs::write(&path, bytes).unwrap();
    File::open(path).unwrap()
}

#[test]
fn staging_ownership_distinguishes_creation_collision_from_later_failure() {
    let dir = crate::test_support::TestDir::new();
    let input = source(&dir.0, b"source");
    let stage = dir.0.join("stage");
    fs::write(&stage, b"other owner").unwrap();
    assert!(matches!(
        stage_file(&input, &stage, ExpectedHashes::default(), 1024),
        Err(StageFailure::NotCreated(AdminError::Io(_)))
    ));
    assert_eq!(fs::read(&stage).unwrap(), b"other owner");
    fs::remove_file(&stage).unwrap();
    FORCE_COPY.set(true);
    let path = dir.0.join("source");
    COPY_HOOK
        .with_borrow_mut(|hook| *hook = Some(Box::new(move || fs::write(path, b"x").unwrap())));
    assert!(matches!(
        stage_file(&input, &stage, ExpectedHashes::default(), 1024),
        Err(StageFailure::Created(AdminError::SourceChanged { .. }))
    ));
    FORCE_COPY.set(false);
    assert_eq!(fs::read(stage).unwrap(), b"source");
}

#[test]
fn already_exists_after_staging_creation_still_requires_cleanup() {
    use crate::storage::catalog::fault::{self, Fault, Step};
    let dir = crate::test_support::TestDir::new();
    fs::create_dir(dir.0.join("staging")).unwrap();
    let input = source(&dir.0, b"source");
    let stage = dir.0.join("staging/value");
    FORCE_COPY.set(true);
    fault::arm(
        &dir.0,
        Step::BodyWrite,
        0,
        Fault::Io(io::ErrorKind::AlreadyExists),
    );
    assert!(
        matches!(stage_file(&input, &stage, ExpectedHashes::default(), 1024), Err(StageFailure::Created(AdminError::Io(cause))) if cause.kind() == io::ErrorKind::AlreadyExists)
    );
    FORCE_COPY.set(false);
    assert!(stage.exists());
    assert_eq!(fs::read(dir.0.join("source")).unwrap(), b"source");
}

#[tokio::test]
async fn staging_sync_already_exists_releases_the_owned_file() {
    use crate::storage::catalog::fault::{Fault, Step};
    let (dir, catalog, admin, _, _) = setup().await;
    catalog.inject(Step::BodySync, 0, Fault::Io(io::ErrorKind::AlreadyExists));
    let error = admin
        .stage_from_file(source(&dir.0, b"source"), ExpectedHashes::default(), 1024)
        .await
        .err()
        .unwrap();
    assert!(matches!(error, AdminError::Io(cause) if cause.kind() == io::ErrorKind::AlreadyExists));
    assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    assert_eq!(fs::read(dir.0.join("source")).unwrap(), b"source");
    catalog.close().await.unwrap();
}

async fn entry(admin: &Admin, dir: &std::path::Path, name: &str, bytes: &[u8]) -> BatchPublication {
    let staged = admin
        .stage_from_file(source(dir, bytes), ExpectedHashes::default(), 1024 * 1024)
        .await
        .unwrap();
    BatchPublication {
        bucket: BucketName::new("test-bucket").unwrap(),
        name: ObjectName::new(name).unwrap(),
        request: request(None),
        staged,
    }
}

fn blob_count(dir: &std::path::Path) -> usize {
    fs::read_dir(dir.join("blobs"))
        .unwrap()
        .map(|shard| fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum()
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectorySync {
    Pending,
    Complete,
}

#[cfg(unix)]
async fn cleanup_snapshot(crash: crate::storage::catalog::fault::Crash, sync: DirectorySync) {
    use crate::storage::catalog::fault::{self, Crash, Step};
    let (dir, catalog, admin, _, _) = setup().await;
    let staged = admin
        .stage_from_file(
            source(&dir.0, b"private bytes"),
            ExpectedHashes::default(),
            1024,
        )
        .await
        .unwrap();
    let id = staged.id.unwrap();
    catalog.0.backend.publish_draft(&id).unwrap();
    let snapshot = crate::test_support::TestDir::new();
    catalog.inject(Step::AfterCommit, 0, crash.fault(snapshot.0.clone()));
    if sync == DirectorySync::Complete {
        cleanup(&catalog, id).await.unwrap();
    } else {
        catalog.0.backend.remove(&id).unwrap();
    }
    // This explicit trigger snapshots cleanup, independently of publication transactions.
    fault::hit(&dir.0, Step::AfterCommit).unwrap();
    assert_eq!(catalog.pending_faults(), 0, "{crash:?} {sync:?}");
    let path = id.blob_path(&snapshot.0);
    let exists = path.exists();
    let bytes = exists.then(|| fs::read(&path).unwrap());
    staged.discard().await.unwrap();
    catalog.close().await.unwrap();
    assert_eq!(
        exists,
        crash == Crash::PowerLoss && sync == DirectorySync::Pending,
        "{crash:?} {sync:?}"
    );
    if let Some(bytes) = bytes {
        assert_eq!(bytes, b"private bytes", "{crash:?} {sync:?}");
    }
    let recovered = Catalog::open(&snapshot.0).await.unwrap();
    assert_eq!(blob_count(&snapshot.0), 0, "{crash:?} {sync:?}");
    assert_eq!(
        fs::read_dir(snapshot.0.join("staging")).unwrap().count(),
        0,
        "{crash:?} {sync:?}"
    );
    recovered.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn cleanup_unlink_is_volatile_until_directory_sync() {
    for &crash in crate::storage::catalog::fault::Crash::ALL {
        cleanup_snapshot(crash, DirectorySync::Pending).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cleanup_directory_sync_survives_crash_and_power_loss() {
    for &crash in crate::storage::catalog::fault::Crash::ALL {
        cleanup_snapshot(crash, DirectorySync::Complete).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cleanup_directory_sync_failure_reaches_discard_and_drop() {
    use crate::storage::catalog::fault::{Fault, Step};
    for explicit in [true, false] {
        let (dir, catalog, admin, _, _) = setup().await;
        let staged = admin
            .stage_from_file(
                source(&dir.0, b"private bytes"),
                ExpectedHashes::default(),
                1024,
            )
            .await
            .unwrap();
        let id = staged.id.unwrap();
        catalog.0.backend.publish_draft(&id).unwrap();
        catalog.inject(Step::AdminCleanupSync, 0, Fault::Io(io::ErrorKind::Other));
        if explicit {
            let error = staged.discard().await.unwrap_err();
            assert_eq!(
                crate::error::causes(&error)
                    .find_map(|cause| cause.downcast_ref::<io::Error>())
                    .unwrap()
                    .kind(),
                io::ErrorKind::Other
            );
        } else {
            drop(staged);
        }
        catalog.close().await.unwrap();
        assert_eq!(catalog.pending_faults(), 0);
        assert!(!id.blob_path(&dir.0).exists());
        let recovered = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(blob_count(&dir.0), 0);
        recovered.close().await.unwrap();
    }
}

#[tokio::test]
async fn publication_acl_rejection_preserves_object_allocator_and_acl() {
    for restriction in ["invalid", "uniform", "public-prevention"] {
        let (dir, catalog, admin, bucket, name) = setup().await;
        let original = admin
            .publish_object(&bucket, &name, request(None), body(b"original"))
            .await
            .unwrap();
        let previous_acl = catalog
            .acl_projection("test-bucket", vec![("object".into(), original.generation)])
            .await
            .unwrap()
            .unwrap()
            .objects;
        if restriction != "invalid" {
            let update = crate::bucket::BucketMetadataUpdate {
                uniform_bucket_level_access_enabled: (restriction == "uniform")
                    .then_some(true.into()),
                public_access_prevention_enforced: (restriction == "public-prevention")
                    .then_some(true),
                ..Default::default()
            };
            catalog
                .patch_bucket("test-bucket", update, Default::default(), PERMISSIVE)
                .await
                .unwrap();
        }
        let last = catalog
            .run_admin(|connection| {
                connection
                    .query_row("SELECT last_generation FROM object_generation", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(unavailable)
            })
            .await
            .unwrap();
        let mut replacement = entry(&admin, &dir.0, "object", b"replacement").await;
        replacement.request.metadata.acl = Some(vec![crate::acl::AclEntry {
            entity: "allUsers".into(),
            role: if restriction == "invalid" {
                crate::acl::AclRole::Writer
            } else {
                crate::acl::AclRole::Reader
            },
        }]);
        let result = admin.publish_objects(vec![replacement]).await.unwrap();
        assert!(result.results[0].is_err(), "{restriction}");
        assert_eq!(
            admin.get_object(&bucket, &name, None).await.unwrap(),
            original
        );
        assert_eq!(
            catalog
                .acl_projection("test-bucket", vec![("object".into(), original.generation)])
                .await
                .unwrap()
                .unwrap()
                .objects,
            previous_acl
        );
        assert_eq!(
            catalog
                .run_admin(|connection| {
                    connection
                        .query_row("SELECT last_generation FROM object_generation", [], |row| {
                            row.get::<_, i64>(0)
                        })
                        .map_err(unavailable)
                })
                .await
                .unwrap(),
            last
        );
        catalog.close().await.unwrap();
    }
}

/// Each panic once ended its worker silently; with two workers, the third job then had no
/// thread left to run it.
#[tokio::test]
async fn a_panicking_file_job_fails_its_caller_and_keeps_both_workers() {
    let workers = FileWorkers::new().unwrap();
    for _ in 0..2 {
        let error = workers
            .run(|| -> Result<(), AdminError> { panic!("publication fixture panic") })
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("publication-file job panicked"),
            "{error}"
        );
    }
    // Each job waits for the other, so both finish only while two workers run them at once.
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let meet = |barrier: Arc<std::sync::Barrier>| {
        move || {
            barrier.wait();
            Ok::<_, AdminError>(())
        }
    };
    let (first, second) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            workers.run(meet(barrier.clone())),
            workers.run(meet(barrier.clone())),
        )
    })
    .await
    .expect("both workers still serve");
    first.unwrap();
    second.unwrap();
    assert_eq!(
        crate::task::tests::reported_panics("publication-file"),
        ["publication fixture panic", "publication fixture panic"]
    );
}

#[tokio::test]
async fn publication_disk_full_rolls_back_the_batch_and_journal() {
    let (dir, catalog, admin, bucket, _) = setup().await;
    catalog.enable_change_journal().await.unwrap();
    let fence = catalog.read_changes(None, None, 256).await.unwrap().fence;
    let a = entry(&admin, &dir.0, "a", b"a").await;
    let mut b = entry(&admin, &dir.0, "b", b"b").await;
    b.request.metadata.metadata = Some(Some(BTreeMap::from([(
        "large".into(),
        Some("x".repeat(7_000)),
    )])));
    catalog
        .run_upload(|connection| {
            let pages: i64 = connection
                .pragma_query_value(None, "page_count", |row| row.get(0))
                .map_err(objects_error)?;
            connection
                .pragma_update(None, "max_page_count", pages)
                .map_err(objects_error)
        })
        .await
        .unwrap();
    assert_eq!(
        admin
            .publish_objects_with_budget(vec![a, b], Duration::from_secs(1))
            .await
            .err(),
        Some(AdminError::DiskFull(io::ErrorKind::StorageFull.into()))
    );
    assert_eq!(
        admin
            .get_object(&bucket, &ObjectName::new("a").unwrap(), None)
            .await,
        Err(AdminError::NotFound)
    );
    assert_eq!(
        catalog.read_changes(None, None, 256).await.unwrap().fence,
        fence
    );
    assert_eq!(blob_count(&dir.0), 0);
    assert_eq!(catalog.0.staged_handles.available_permits(), 256);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn publication_batch_keeps_per_element_validation_and_rejects_foreign_handles() {
    let (dir, catalog, admin, _, _) = setup().await;
    let a = entry(&admin, &dir.0, "a", b"a").await;
    let mut bad = entry(&admin, &dir.0, "bad", b"b").await;
    bad.request.expected.crc32c = Some(0);
    let mut large = entry(&admin, &dir.0, "large", b"large").await;
    large.request.max_bytes = 1;
    let result = admin
        .publish_objects_with_budget(vec![a, bad, large], Duration::from_secs(1))
        .await
        .unwrap();
    assert!(result.results[0].is_ok());
    assert_eq!(result.results[1], Err(AdminError::Checksum));
    assert_eq!(result.results[2], Err(AdminError::TooLarge));
    let (other_dir, other_catalog, other, _, _) = setup().await;
    let foreign = entry(&other, &other_dir.0, "foreign", b"foreign").await;
    assert!(matches!(
        admin.publish_objects(vec![foreign]).await,
        Err(AdminError::Invalid(_))
    ));
    other_catalog.close().await.unwrap();
    assert_eq!(blob_count(&dir.0), 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn publication_streaming_modes_preserve_patches_and_require_all_hash_fields() {
    let (_dir, catalog, admin, bucket, name) = setup().await;
    let mut initial = request(None);
    initial.content_type = "application/initial".into();
    let first = admin
        .publish_object(&bucket, &name, initial, body(b"same"))
        .await
        .unwrap();
    let mut inherit = request(None);
    inherit.mode = PublishMode::InheritAndSkipIfSameHash;
    let second = admin
        .publish_object(&bucket, &name, inherit, body(b"diff"))
        .await
        .unwrap();
    assert_ne!(first.generation, second.generation);
    assert_eq!(second.content_type, first.content_type);
    let previous = objects::StoredObject {
        record: second.clone(),
        blob: crate::storage::blob_id::BlobId::generate().unwrap(),
    };
    assert!(objects::skip_same(
        PublishMode::SkipIfSameHash,
        Some(&previous),
        second.size,
        second.md5,
        second.crc32c
    ));
    assert!(!objects::skip_same(
        PublishMode::SkipIfSameHash,
        Some(&previous),
        second.size + 1,
        second.md5,
        second.crc32c
    ));
    assert!(!objects::skip_same(
        PublishMode::SkipIfSameHash,
        Some(&previous),
        second.size,
        [0; 16],
        second.crc32c
    ));
    assert!(!objects::skip_same(
        PublishMode::SkipIfSameHash,
        Some(&previous),
        second.size,
        second.md5,
        second.crc32c ^ 1
    ));
    let patch = ObjectMetadataUpdate {
        event_based_hold: Some(true),
        ..Default::default()
    };
    catalog
        .update_object_metadata(
            live(bucket.as_str(), name.as_str()),
            Default::default(),
            patch,
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let mut skip = request(None);
    skip.mode = PublishMode::SkipIfSameHash;
    assert_eq!(
        admin
            .publish_object(&bucket, &name, skip, body(b"diff"))
            .await
            .unwrap_err(),
        AdminError::Hold
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn publication_inherits_metadata_patched_after_staging() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    admin
        .publish_object(&bucket, &name, request(None), body(b"old"))
        .await
        .unwrap();
    let mut input = entry(&admin, &dir.0, "object", b"new").await;
    input.request.mode = PublishMode::InheritFromReplaced;
    let patch = ObjectMetadataUpdate {
        content_type: Patch::Set("application/custom".into()),
        cache_control: Patch::Set("private".into()),
        metadata: Some(Some(BTreeMap::from([(
            "patched".into(),
            Some("value".into()),
        )]))),
        ..Default::default()
    };
    catalog
        .update_object_metadata(
            live(bucket.as_str(), name.as_str()),
            Default::default(),
            patch,
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let result = admin.publish_objects(vec![input]).await.unwrap();
    let record = result.results.into_iter().next().unwrap().unwrap();
    assert_eq!(record.content_type, "application/custom");
    assert_eq!(record.cache_control.as_deref(), Some("private"));
    assert_eq!(record.metadata["patched"], "value");
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn publication_equal_hash_keeps_generation_metadata_and_journal() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    catalog.enable_change_journal().await.unwrap();
    let first = admin
        .publish_object(&bucket, &name, request(None), body(b"same"))
        .await
        .unwrap();
    let fence = catalog.read_changes(None, None, 256).await.unwrap().fence;
    let mut input = entry(&admin, &dir.0, "object", b"same").await;
    input.request.mode = PublishMode::SkipIfSameHash;
    input.request.content_type = "application/ignored".into();
    let result = admin.publish_objects(vec![input]).await.unwrap();
    assert_eq!(result.results.into_iter().next().unwrap().unwrap(), first);
    assert_eq!(
        catalog.read_changes(None, None, 256).await.unwrap().fence,
        fence
    );
    assert_eq!(blob_count(&dir.0), 1);
    let mut same = request(Some(GenerationCondition::Absent));
    same.mode = PublishMode::SkipIfSameHash;
    assert_eq!(
        admin
            .publish_object(&bucket, &name, same, body(b"same"))
            .await
            .unwrap_err(),
        AdminError::Precondition
    );
    let mut same = request(None);
    same.mode = PublishMode::SkipIfSameHash;
    assert_eq!(
        admin
            .publish_object(&bucket, &name, same, body(b"same"))
            .await
            .unwrap(),
        first
    );
    assert_eq!(blob_count(&dir.0), 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn publication_batch_isolates_failed_elements_and_cleans_unused_blobs() {
    let (dir, catalog, admin, bucket, _) = setup().await;
    catalog.enable_change_journal().await.unwrap();
    let name = ObjectName::new("exists").unwrap();
    let old = admin
        .publish_object(&bucket, &name, request(None), body(b"old"))
        .await
        .unwrap();
    let a = entry(&admin, &dir.0, "a", b"a").await;
    let mut b = entry(&admin, &dir.0, "exists", b"b").await;
    b.request.condition = Some(GenerationCondition::Absent);
    let c = entry(&admin, &dir.0, "c", b"c").await;
    let result = admin
        .publish_objects_with_budget(vec![a, b, c], Duration::from_secs(1))
        .await
        .unwrap();
    assert!(result.results[0].is_ok());
    assert_eq!(result.results[1], Err(AdminError::Precondition));
    assert!(result.results[2].is_ok());
    assert!(result.unattempted.is_empty());
    assert_eq!(admin.get_object(&bucket, &name, None).await.unwrap(), old);
    assert_eq!(blob_count(&dir.0), 3);
    assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn publication_budget_returns_retryable_handles_and_releases_sql_lane() {
    let (dir, catalog, admin, bucket, _) = setup().await;
    let a = entry(&admin, &dir.0, "a", b"a").await;
    let b = entry(&admin, &dir.0, "b", b"b").await;
    let result = admin
        .publish_objects_with_budget(vec![a, b], Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.unattempted.len(), 1);
    assert!(result.budget_exceeded);
    assert!(admin.get_bucket(&bucket).await.unwrap().is_some());
    assert_eq!(catalog.0.staged_handles.available_permits(), 255);
    let result = admin.publish_objects(result.unattempted).await.unwrap();
    assert_eq!(result.results.len(), 1);
    assert!(result.results[0].is_ok());
    assert_eq!(catalog.0.staged_handles.available_permits(), 256);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn publication_partial_batch_allows_staging_without_a_second_batch() {
    let (dir, catalog, admin, _, _) = setup().await;
    let first = entry(&admin, &dir.0, "a", b"a").await;
    let held = catalog
        .0
        .publication_batch
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let worker = admin.clone();
    let publishing = tokio::spawn(async move { worker.publish_objects(vec![first]).await });
    tokio::task::yield_now().await;
    let next = entry(&admin, &dir.0, "b", b"b").await;
    assert!(!publishing.is_finished());
    assert_eq!(catalog.0.staged_handles.available_permits(), 254);
    drop(held);
    assert!(publishing.await.unwrap().unwrap().results[0].is_ok());
    assert!(admin.publish_objects(vec![next]).await.unwrap().results[0].is_ok());
    catalog.close().await.unwrap();
}

/// Past the one active and one waiting batch that publication once held before refusing the next.
const QUEUED_BATCHES: usize = 8;
/// Long enough that no test reaches it unless the behaviour under test is broken.
const LONG_WAIT: crate::time_limit::TimeLimit = crate::time_limit::TimeLimit::from_millis(60_000);

/// Staged batches, each spawned so that it stands in the publication line with its lease.
async fn spawn_batches(
    admin: &Admin,
    dir: &std::path::Path,
) -> Vec<tokio::task::JoinHandle<Result<BatchResult, AdminError>>> {
    let mut batches = Vec::new();
    for index in 0..QUEUED_BATCHES {
        let name = format!("queued-{index}");
        let input = entry(admin, dir, &name, name.as_bytes()).await;
        let worker = admin.clone();
        batches.push(tokio::spawn(async move {
            worker.publish_objects(vec![input]).await
        }));
        tokio::task::yield_now().await;
    }
    batches
}

#[tokio::test]
async fn publication_past_the_active_batch_takes_turns_and_every_batch_commits() {
    let (dir, catalog, admin, _, _) = setup().await;
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let batches = spawn_batches(&admin, &dir.0).await;
    eventually_async("the batches to fill the request slots", || async {
        catalog.0.publication_requests.available_permits() == 0
    })
    .await;
    assert!(batches.iter().all(|batch| !batch.is_finished()));
    drop(execution);
    for batch in batches {
        assert!(batch.await.unwrap().unwrap().results[0].is_ok());
    }
    catalog.close().await.unwrap();
    assert_eq!(blob_count(&dir.0), QUEUED_BATCHES);
}

#[tokio::test]
async fn a_reset_drains_the_publications_waiting_in_line() {
    let (dir, catalog, admin, bucket, _) = setup().await;
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let batches = spawn_batches(&admin, &dir.0).await;
    let worker = admin.clone();
    let reset = tokio::spawn(async move { worker.reset_until(LONG_WAIT.deadline()).await });
    eventually_async("the reset to line up", || async {
        catalog.0.admin_reads.available_permits() == 31
    })
    .await;
    drop(execution);
    // Each batch waits under the lease of its staged object, so the reset can start only once
    // the line has moved on: neither may wait for the other.
    for batch in batches {
        assert!(batch.await.unwrap().unwrap().results[0].is_ok());
    }
    reset.await.unwrap().unwrap();
    assert!(admin.get_bucket(&bucket).await.unwrap().is_none());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_seed_commit_and_a_publication_behind_it_end_by_their_deadlines() {
    let (dir, catalog, admin, _, _) = setup().await;
    let mut batch = admin.seed_batch().await.unwrap();
    let input = entry(&admin, &dir.0, "behind", b"behind").await;
    batch
        .object(
            BucketName::new("test-bucket").unwrap(),
            ObjectName::new("seeded").unwrap(),
            request(None),
            source(&dir.0, b"seeded"),
        )
        .await
        .unwrap();
    catalog.apply_session_limits(&crate::config::Config {
        catalog_wait_timeout: crate::time_limit::TimeLimit::from_millis(500),
        ..Default::default()
    });
    // The publication holds a lease the commit must drain and waits for the batch slot the commit
    // holds. Neither can finish the other, so only the deadline may end this.
    let worker = admin.clone();
    let publication = tokio::spawn(async move { worker.publish_objects(vec![input]).await });
    let commit = batch.commit(SeedMode::Merge);
    let (publication, commit) = tokio::join!(publication, commit);
    let (publication, commit) = (publication.unwrap().err(), commit.err());
    assert!(
        matches!(
            (&publication, &commit),
            (
                Some(AdminError::Busy { .. }),
                None | Some(AdminError::Busy { .. })
            ) | (None, Some(AdminError::Busy { .. }))
        ),
        "{publication:?} {commit:?}"
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn closing_wakes_every_publication_waiting_for_its_turn() {
    let (dir, catalog, admin, _, _) = setup().await;
    let requests = catalog
        .0
        .publication_requests
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let batches = spawn_batches(&admin, &dir.0).await;
    let worker = catalog.clone();
    let close = tokio::spawn(async move { worker.close().await });
    // Their own deadline is 30 s away, so only the close can end the wait this soon.
    for batch in batches {
        let result = tokio::time::timeout(Duration::from_secs(10), batch)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.err(), Some(AdminError::Closed));
    }
    assert!(!close.is_finished());
    drop(requests);
    close.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_cancelled_publication_gives_up_its_place_and_returns_its_lease() {
    let (dir, catalog, admin, bucket, _) = setup().await;
    let requests = catalog
        .0
        .publication_requests
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let batches = spawn_batches(&admin, &dir.0).await;
    for batch in &batches {
        batch.abort();
    }
    for batch in batches {
        assert!(batch.await.err().is_some_and(|error| error.is_cancelled()));
    }
    drop(requests);
    assert_eq!(catalog.0.publication_requests.available_permits(), 2);
    assert_eq!(catalog.0.publication_batch.available_permits(), 1);
    // A lease left behind by a cancelled wait would keep this reset out until it gives up.
    admin.reset_until(LONG_WAIT.deadline()).await.unwrap();
    assert!(admin.get_bucket(&bucket).await.unwrap().is_none());
    catalog.close().await.unwrap();
    assert_eq!(blob_count(&dir.0), 0);
}

#[tokio::test]
async fn publication_cancelled_renamed_batch_cleans_before_shutdown_unlocks() {
    for cancel_request in [false, true] {
        let (dir, catalog, admin, bucket, name) = setup().await;
        let input = entry(&admin, &dir.0, "object", b"cancelled").await;
        let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
        let worker = admin.clone();
        let publishing = tokio::spawn(async move { worker.publish_objects(vec![input]).await });
        eventually_async("the batch to rename its blob", || async {
            blob_count(&dir.0) != 0
        })
        .await;
        if cancel_request {
            publishing.abort();
        }
        let worker = catalog.clone();
        let close = tokio::spawn(async move { worker.close().await });
        tokio::task::yield_now().await;
        assert!(!close.is_finished());
        assert!(Catalog::open(&dir.0).await.is_err());
        drop(execution);
        let result = publishing.await;
        if cancel_request {
            assert!(matches!(result, Err(error) if error.is_cancelled()));
        } else {
            assert!(result.unwrap().unwrap().results[0].is_ok());
        }
        tokio::time::timeout(Duration::from_secs(3), close)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(blob_count(&dir.0), 1);
        assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
        let reopened = Catalog::open(&dir.0).await.unwrap();
        let (_, mut reader) = reopened
            .admin(Origin::Seed)
            .open_object(&bucket, &name, None)
            .await
            .unwrap();
        assert_eq!(
            reader.next_chunk().await.unwrap().unwrap(),
            b"cancelled"[..]
        );
        assert!(reader.next_chunk().await.unwrap().is_none());
        reopened.close().await.unwrap();
    }
}

#[tokio::test]
async fn publication_shutdown_drains_accepted_batch_through_retired_blob_reclamation() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    let old = admin
        .publish_object(&bucket, &name, request(None), body(b"old"))
        .await
        .unwrap();
    let input = entry(&admin, &dir.0, "object", b"new").await;
    let held = catalog.0.reclamation.clone().acquire_owned().await.unwrap();
    let worker = admin.clone();
    let publishing = tokio::spawn(async move { worker.publish_objects(vec![input]).await });
    let selected = eventually_some_async("the batch to select a new generation", || async {
        let selected = admin.get_object(&bucket, &name, None).await.unwrap();
        (selected.generation != old.generation
            && catalog.0.staged_handles.available_permits() == 256)
            .then_some(selected)
    })
    .await;
    assert!(!publishing.is_finished());
    assert_eq!(catalog.0.publication_requests.available_permits(), 1);
    let worker = catalog.clone();
    let close = tokio::spawn(async move { worker.close().await });
    tokio::task::yield_now().await;
    assert!(!close.is_finished());
    assert!(Catalog::open(&dir.0).await.is_err());
    drop(held);
    assert!(publishing.await.unwrap().unwrap().results[0].is_ok());
    close.await.unwrap().unwrap();
    assert_eq!(blob_count(&dir.0), 1);
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        reopened
            .admin(Origin::Import)
            .get_object(&bucket, &name, None)
            .await
            .unwrap(),
        selected
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn publication_file_checksums_caps_staging_visibility_and_discard() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    let input = source(&dir.0, &[42; 131_073]);
    let staged = admin
        .stage_from_file(input, ExpectedHashes::default(), 200_000)
        .await
        .unwrap();
    assert_eq!(staged.size(), 131_073);
    assert_eq!(staged.hashes().crc32c, crc32c::crc32c(&[42; 131_073]));
    assert_eq!(
        admin.get_object(&bucket, &name, None).await,
        Err(AdminError::NotFound)
    );
    assert_eq!(blob_count(&dir.0), 0);
    std::thread::spawn(move || drop(staged)).join().unwrap();
    let bad = ExpectedHashes {
        md5: Some([0; 16]),
        crc32c: None,
    };
    assert!(matches!(
        admin
            .stage_from_file(source(&dir.0, b"bytes"), bad, 100)
            .await,
        Err(AdminError::Checksum)
    ));
    assert!(matches!(
        admin
            .stage_from_file(source(&dir.0, b"bytes"), ExpectedHashes::default(), 4)
            .await,
        Err(AdminError::TooLarge)
    ));
    let record = admin
        .publish_from_file(&bucket, &name, request(None), source(&dir.0, b"file"))
        .await
        .unwrap();
    assert_eq!(record.size, 4);
    let staged = admin
        .stage_from_file(source(&dir.0, b"discard"), ExpectedHashes::default(), 100)
        .await
        .unwrap();
    staged.discard().await.unwrap();
    catalog.close().await.unwrap();
    assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        reopened
            .admin(Origin::Import)
            .get_object(&bucket, &name, None)
            .await
            .unwrap(),
        record
    );
    reopened.close().await.unwrap();
}

#[test]
fn publication_mutable_source_is_rejected_and_fallback_hashes_written_buffers() {
    let dir = crate::test_support::TestDir::new();
    let bytes = [73; 131_073];
    let input = source(&dir.0, &bytes);
    let mut shared = input.try_clone().unwrap();
    shared.seek(SeekFrom::Start(17)).unwrap();
    COPY_HOOK.with_borrow_mut(|hook| {
        *hook = Some(Box::new(move || {
            shared.seek(SeekFrom::Start(0)).unwrap();
        }))
    });
    let stage = dir.0.join("snapshot");
    FORCE_COPY.set(true);
    let (size, hashes) = stage_file(&input, &stage, ExpectedHashes::default(), 200_000).unwrap();
    assert_eq!(size, bytes.len() as u64);
    assert_eq!(hashes.crc32c, crc32c::crc32c(&fs::read(stage).unwrap()));
    let input = source(&dir.0, &bytes);
    let path = dir.0.join("source");
    COPY_HOOK.with_borrow_mut(|hook| {
        *hook = Some(Box::new(move || fs::write(path, b"changed").unwrap()))
    });
    let result = stage_file(
        &input,
        &dir.0.join("drift"),
        ExpectedHashes::default(),
        200_000,
    );
    assert!(
        matches!(result, Err(StageFailure::Created(AdminError::SourceChanged { fields })) if fields.contains(&"size".into()))
    );
    FORCE_COPY.set(false);
}

#[test]
fn publication_file_staging_does_not_wait_for_shared_blocking_pool() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (dir, catalog, admin, _, _) = setup().await;
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = oneshot::channel();
        let blocked = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
        });
        ready.await.unwrap();
        let staged = tokio::time::timeout(
            Duration::from_secs(2),
            admin.stage_from_file(source(&dir.0, b"file"), ExpectedHashes::default(), 100),
        )
        .await
        .unwrap()
        .unwrap();
        release.send(()).unwrap();
        blocked.await.unwrap();
        staged.discard().await.unwrap();
        catalog.close().await.unwrap();
    });
}

#[tokio::test]
async fn publication_handle_budget_backpressures_and_shutdown_waits_for_cleanup() {
    let (dir, catalog, admin, _, _) = setup().await;
    let mut handles = Vec::new();
    for _ in 0..256 {
        handles.push(
            admin
                .stage_from_file(source(&dir.0, b"x"), ExpectedHashes::default(), 100)
                .await
                .unwrap(),
        );
    }
    assert_eq!(catalog.0.staged_handles.available_permits(), 0);
    let worker = admin.clone();
    let input = source(&dir.0, b"next");
    let next = tokio::spawn(async move {
        worker
            .stage_from_file(input, ExpectedHashes::default(), 100)
            .await
    });
    tokio::task::yield_now().await;
    assert!(!next.is_finished());
    handles.pop().unwrap().discard().await.unwrap();
    handles.push(next.await.unwrap().unwrap());
    let worker = catalog.clone();
    let close = tokio::spawn(async move { worker.close().await });
    tokio::task::yield_now().await;
    assert!(!close.is_finished());
    drop(handles);
    tokio::time::timeout(Duration::from_secs(10), close)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}
