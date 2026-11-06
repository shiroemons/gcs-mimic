use super::*;
#[cfg(unix)]
use crate::storage::catalog::fault::Crash;
use crate::{
    clock::SystemClock,
    storage::catalog::{
        admin::tests::{body, request, setup},
        fault::{Fault, Step},
    },
    test_support::{TestDir, eventually_async, joined},
};
use std::fs;

#[tokio::test]
async fn the_metadata_budget_grows_with_the_allowed_tree_and_refuses_past_it() {
    const MIB: usize = 1024 * 1024;
    assert_eq!(metadata_budget(1), 8 * MIB);
    assert_eq!(metadata_budget(10_000), 8 * MIB);
    assert_eq!(metadata_budget(10_001), 16 * MIB);
    assert_eq!(metadata_budget(usize::MAX), usize::MAX);
    let (_dir, catalog, admin, _, _) = setup().await;
    let mut batch = admin.seed_batch_until(None, 10_001).await.unwrap();
    batch.charge_metadata(16 * MIB).unwrap();
    let AdminError::Invalid(message) = batch.charge_metadata(1).unwrap_err() else {
        panic!("a charge past the budget is invalid input");
    };
    assert!(message.contains("16 MiB"), "{message}");
    assert!(message.contains("max_seed_entries"), "{message}");
    drop(batch);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn seed_project_mismatch_preserves_the_existing_diagnostic() {
    let (_dir, catalog, admin, _, _) = setup().await;
    let mut batch = admin.seed_batch().await.unwrap();
    batch
        .bucket(
            BucketDefinition::default_for(
                &BucketName::new("test-bucket").unwrap(),
                "other-project",
                &SystemClock,
            )
            .unwrap(),
        )
        .unwrap();
    let error = batch.commit(SeedMode::Merge).await.err().unwrap();
    assert_eq!(error, AdminError::SeedProjectMismatch);
    assert_eq!(
        error.to_string(),
        "administrative operation: Invalid(\"existing seed bucket belongs to another project\")"
    );
    catalog.close().await.unwrap();
}

async fn add(batch: &mut SeedBatch, dir: &Path, name: &str, bytes: &[u8]) {
    let path = dir.join("source");
    fs::write(&path, bytes).unwrap();
    batch
        .object(
            BucketName::new("test-bucket").unwrap(),
            ObjectName::new(name).unwrap(),
            request(None),
            File::open(path).unwrap(),
        )
        .await
        .unwrap();
}

fn bucket() -> BucketDefinition {
    BucketDefinition::default_for(
        &BucketName::new("test-bucket").unwrap(),
        "project",
        &SystemClock,
    )
    .unwrap()
}

async fn bytes(admin: &Admin, name: &str) -> Vec<u8> {
    let (_, mut reader) = admin
        .open_object(
            &BucketName::new("test-bucket").unwrap(),
            &ObjectName::new(name).unwrap(),
            None,
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

#[tokio::test]
async fn atomic_seed_late_invalid_metadata_rolls_back_buckets_objects_and_allocator() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    let old = admin
        .publish_object(&bucket, &name, request(None), body(b"old"))
        .await
        .unwrap();
    let mut batch = admin.seed_batch().await.unwrap();
    batch
        .bucket(
            BucketDefinition::default_for(
                &BucketName::new("empty-bucket").unwrap(),
                "project",
                &SystemClock,
            )
            .unwrap(),
        )
        .unwrap();
    add(&mut batch, &dir.0, "first", b"new").await;
    add(&mut batch, &dir.0, "object", b"new").await;
    batch.entries[1].request.content_type = "invalid\ncontent".into();
    assert!(matches!(
        batch.commit(SeedMode::Merge).await,
        Err(AdminError::Invalid(_))
    ));
    assert!(
        admin
            .get_bucket(&BucketName::new("empty-bucket").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        admin
            .get_object(&bucket, &ObjectName::new("first").unwrap(), None)
            .await,
        Err(AdminError::NotFound)
    );
    assert_eq!(admin.get_object(&bucket, &name, None).await.unwrap(), old);
    let next = admin
        .publish_object(&bucket, &name, request(None), body(b"next"))
        .await
        .unwrap();
    assert!(next.generation > old.generation);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn atomic_seed_readers_identity_unrelated_objects_and_commit_recovery() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    let old = admin
        .publish_object(&bucket, &name, request(None), body(b"old"))
        .await
        .unwrap();
    admin
        .publish_object(
            &bucket,
            &ObjectName::new("unrelated").unwrap(),
            request(None),
            body(b"keep"),
        )
        .await
        .unwrap();
    let identity = catalog.sync_state().catalog_id().await.unwrap();
    let (_, mut reader) = admin.open_object(&bucket, &name, None).await.unwrap();
    let mut batch = admin.seed_batch().await.unwrap();
    batch.bucket(super::tests::bucket()).unwrap();
    add(&mut batch, &dir.0, "object", b"new").await;
    catalog.inject(Step::AfterCommit, 0, Fault::Io(io::ErrorKind::Other));
    let counts = batch.commit(SeedMode::Merge).await.unwrap();
    assert_eq!(counts.replaced, 1);
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"old"[..]);
    drop(reader);
    assert_eq!(bytes(&admin, "object").await, b"new");
    assert_eq!(bytes(&admin, "unrelated").await, b"keep");
    assert!(
        admin
            .get_object(&bucket, &name, None)
            .await
            .unwrap()
            .generation
            > old.generation
    );
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(reopened.sync_state().catalog_id().await.unwrap(), identity);
    assert_eq!(bytes(&reopened.admin(Origin::Seed), "object").await, b"new");
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn atomic_seed_cancellation_and_concurrent_admission_preserve_old_namespace() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    let old = admin
        .publish_object(&bucket, &name, request(None), body(b"old"))
        .await
        .unwrap();
    let mut batch = admin.seed_batch().await.unwrap();
    for index in 0..300 {
        add(&mut batch, &dir.0, &format!("new-{index}"), b"new").await;
    }
    let limit = tokio::time::Instant::now() + Duration::from_millis(50);
    assert!(matches!(
        admin.seed_batch_until(Some(limit), 10_000).await,
        Err(AdminError::Busy { .. })
    ));
    assert!(matches!(admin.reset().await, Err(AdminError::Busy { .. })));
    drop(batch);
    // Close waits on the shared batch ownership until the one cleanup task has drained all entries.
    catalog.close().await.unwrap();
    assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    let reopened = Catalog::open(&dir.0).await.unwrap();
    let admin = reopened.admin(Origin::Seed);
    assert_eq!(admin.get_object(&bucket, &name, None).await.unwrap(), old);
    assert_eq!(
        admin
            .get_object(&bucket, &ObjectName::new("new-0").unwrap(), None)
            .await,
        Err(AdminError::NotFound)
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn atomic_seed_write_flush_sync_and_commit_failures_keep_committed_bytes() {
    for step in [
        Step::BodyFlush,
        Step::BodySync,
        Step::BeforePublish,
        Step::AfterPublish,
        Step::BeforeCommit,
    ] {
        let (dir, catalog, admin, bucket, name) = setup().await;
        let old = admin
            .publish_object(&bucket, &name, request(None), body(b"old"))
            .await
            .unwrap();
        let mut batch = admin.seed_batch().await.unwrap();
        if matches!(step, Step::BodyFlush | Step::BodySync) {
            catalog.inject(step, 0, Fault::Io(io::ErrorKind::StorageFull));
            let path = dir.0.join("source");
            fs::write(&path, b"new").unwrap();
            assert!(
                batch
                    .object(
                        bucket.clone(),
                        name.clone(),
                        request(None),
                        File::open(path).unwrap()
                    )
                    .await
                    .is_err()
            );
            drop(batch);
        } else {
            add(&mut batch, &dir.0, "object", b"new").await;
            catalog.inject(step, 0, Fault::Io(io::ErrorKind::StorageFull));
            assert!(batch.commit(SeedMode::Merge).await.is_err());
        }
        assert_eq!(
            admin.get_object(&bucket, &name, None).await.unwrap(),
            old,
            "{step:?}"
        );
        assert_eq!(bytes(&admin, "object").await, b"old", "{step:?}");
        assert_eq!(catalog.pending_faults(), 0, "{step:?}");
        catalog.close().await.unwrap();
        let reopened = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(
            bytes(&reopened.admin(Origin::Seed), "object").await,
            b"old",
            "{step:?}"
        );
        reopened.close().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn atomic_seed_power_loss_and_process_crash_snapshots_recover_whole_namespaces() {
    for step in [
        Step::BeforePublish,
        Step::AfterPublish,
        Step::BeforeCommit,
        Step::AfterCommit,
    ] {
        for &crash in Crash::ALL {
            let (dir, catalog, admin, _, _) = setup().await;
            let mut initial = admin.seed_batch().await.unwrap();
            add(&mut initial, &dir.0, "object", b"old").await;
            initial.commit(SeedMode::Merge).await.unwrap();
            let snapshot = TestDir::new();
            let path = snapshot.0.join("snapshot");
            catalog.inject(step, 0, crash.fault(path.clone()));
            let mut batch = admin.seed_batch().await.unwrap();
            add(&mut batch, &dir.0, "object", b"new").await;
            add(&mut batch, &dir.0, "second", b"new").await;
            batch.commit(SeedMode::Merge).await.unwrap();
            assert_eq!(catalog.pending_faults(), 0);
            catalog.close().await.unwrap();
            let recovered = Catalog::open(path).await.unwrap();
            let admin = recovered.admin(Origin::Seed);
            let committed = step == Step::AfterCommit;
            assert_eq!(
                bytes(&admin, "object").await,
                if committed { b"new" } else { b"old" },
                "{step:?} {crash:?}"
            );
            assert_eq!(
                admin
                    .get_object(
                        &BucketName::new("test-bucket").unwrap(),
                        &ObjectName::new("second").unwrap(),
                        None
                    )
                    .await
                    .is_ok(),
                committed
            );
            recovered.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn atomic_ram_reseed_reserves_old_and_new_and_keeps_open_readers() {
    let dir = TestDir::new();
    let catalog = Catalog::open_memory_for_server(
        2048,
        8 * 1024 * 1024,
        Arc::new(SystemClock),
        super::super::super::LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    let admin = catalog.admin(Origin::Seed);
    admin.ensure_bucket(bucket()).await.unwrap();
    let target = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("object").unwrap();
    let old = admin
        .publish_object(&target, &name, request(None), body(b"old"))
        .await
        .unwrap();
    let identity = catalog.sync_state().catalog_id().await.unwrap();
    let (_, mut reader) = admin.open_object(&target, &name, None).await.unwrap();
    let mut batch = admin.seed_batch().await.unwrap();
    batch.bucket(bucket()).unwrap();
    add(&mut batch, &dir.0, "replacement", b"new").await;
    let counts = batch.commit(SeedMode::Replace).await.unwrap();
    assert_eq!(counts.objects, 1);
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"old"[..]);
    drop(reader);
    assert_eq!(catalog.sync_state().catalog_id().await.unwrap(), identity);
    assert_eq!(
        admin.get_object(&target, &name, None).await,
        Err(AdminError::NotFound)
    );
    assert!(
        admin
            .get_object(&target, &ObjectName::new("replacement").unwrap(), None)
            .await
            .unwrap()
            .generation
            > old.generation
    );
    assert_eq!(bytes(&admin, "replacement").await, b"new");
    let mut batch = admin.seed_batch().await.unwrap();
    let path = dir.0.join("large");
    fs::write(&path, [7; 2048]).unwrap();
    let mut large = request(None);
    large.max_bytes = 4096;
    assert_eq!(
        batch
            .object(
                target.clone(),
                name.clone(),
                large,
                File::open(path).unwrap()
            )
            .await
            .unwrap_err(),
        AdminError::DiskFull(io::ErrorKind::StorageFull.into())
    );
    drop(batch);
    assert_eq!(bytes(&admin, "replacement").await, b"new");
    catalog.close().await.unwrap();
}

#[test]
fn seed_stage_failed_copy_write_never_changes_the_source() {
    let dir = TestDir::new();
    fs::create_dir(dir.0.join("staging")).unwrap();
    let path = dir.0.join("source");
    fs::write(&path, b"source").unwrap();
    let source = File::open(&path).unwrap();
    FORCE_COPY.set(true);
    crate::storage::catalog::fault::arm(
        &dir.0,
        Step::BodyWrite,
        0,
        Fault::Io(io::ErrorKind::StorageFull),
    );
    assert!(
        stage_file(
            &source,
            &dir.0.join("staging/value"),
            ExpectedHashes::default(),
            1024
        )
        .is_err()
    );
    FORCE_COPY.set(false);
    // Windows positional reads move the handle's cursor, so check the file by position and by
    // a fresh open rather than by reading on from the shared cursor.
    let mut contents = [0; 6];
    assert_eq!(
        crate::fsutil::read_at(&source, &mut contents, 0).unwrap(),
        6
    );
    assert_eq!(&contents, b"source");
    assert_eq!(fs::read(&path).unwrap(), b"source");
}

#[tokio::test]
async fn atomic_seed_cancelled_before_sql_and_active_upload_leave_no_new_namespace() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let (dir, catalog, admin, bucket, name) = setup().await;
    let old = admin
        .publish_object(&bucket, &name, request(None), body(b"old"))
        .await
        .unwrap();
    let mut batch = admin.seed_batch().await.unwrap();
    add(&mut batch, &dir.0, "object", b"new").await;
    let entry = batch.entries.pop().unwrap();
    batch
        .entries
        .push(prepare_entry(catalog.clone(), entry).await.unwrap());
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let mut commit = Box::pin(batch.commit(SeedMode::Merge));
    assert!(matches!(
        commit
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    drop(commit);
    drop(execution);
    // Acquiring the complete request budget waits for cancelled batch cleanup without a timer or polling loop.
    let drain = catalog
        .0
        .publication_requests
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    drop(drain);
    assert_eq!(admin.get_object(&bucket, &name, None).await.unwrap(), old);
    let limit = tokio::time::Instant::now() + Duration::from_millis(200);
    let mut batch = admin.seed_batch_until(Some(limit), 10_000).await.unwrap();
    add(&mut batch, &dir.0, "object", b"new").await;
    let upload = catalog.admit_object_upload().unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), batch.commit(SeedMode::Merge))
            .await
            .unwrap(),
        Err(AdminError::Busy { retry_after }) if retry_after == Duration::from_secs(1)
    ));
    assert!(tokio::time::Instant::now() >= limit);
    drop(upload);
    assert_eq!(admin.get_object(&bucket, &name, None).await.unwrap(), old);
    assert_eq!(bytes(&admin, "object").await, b"old");
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn atomic_seed_commit_waits_for_an_active_upload_and_allows_reads_meanwhile() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    let old = admin
        .publish_object(&bucket, &name, request(None), body(b"old"))
        .await
        .unwrap();
    let mut batch = admin.seed_batch().await.unwrap();
    add(&mut batch, &dir.0, "object", b"new").await;
    // A prepared entry lets commit reach the exclusive wait without file work in between.
    let entry = batch.entries.pop().unwrap();
    batch
        .entries
        .push(prepare_entry(catalog.clone(), entry).await.unwrap());
    let upload = catalog.admit_object_upload().unwrap();
    let mut commit = std::pin::pin!(batch.commit(SeedMode::Merge));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut commit)
            .await
            .is_err()
    );
    assert_eq!(admin.get_object(&bucket, &name, None).await.unwrap(), old);
    drop(upload);
    let counts = tokio::time::timeout(Duration::from_secs(3), commit)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((counts.objects, counts.replaced), (0, 1));
    assert_eq!(bytes(&admin, "object").await, b"new");
    catalog.close().await.unwrap();
}

/// Holds the commit open at its durable boundary, so requests arrive while the batch owns the
/// catalog. They must wait for it rather than fail, and then find what it published.
#[tokio::test]
async fn requests_during_a_seed_commit_wait_and_then_see_what_it_published() {
    for mode in [SeedMode::Merge, SeedMode::Replace] {
        let (dir, catalog, admin, bucket, name) = setup().await;
        admin
            .publish_object(&bucket, &name, request(None), body(b"old"))
            .await
            .unwrap();
        let mut batch = admin.seed_batch().await.unwrap();
        batch.bucket(self::bucket()).unwrap();
        add(&mut batch, &dir.0, "object", b"new").await;
        let (entered, reached) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        catalog.inject(
            Step::BeforeCommit,
            0,
            Fault::Block {
                entered: Arc::new(std::sync::Mutex::new(Some(entered))),
                release: Arc::new(std::sync::Mutex::new(released)),
            },
        );
        let commit = tokio::spawn(batch.commit(mode));
        reached.await.unwrap();

        let reader = admin.clone();
        let (read_bucket, read_name) = (bucket.clone(), name.clone());
        let read =
            tokio::spawn(async move { reader.get_object(&read_bucket, &read_name, None).await });
        let uploader = catalog.clone();
        let upload = tokio::spawn(async move {
            let permit = uploader.wait_object_upload().await?;
            uploader
                .put_object_prechecked(
                    crate::storage::test_fixtures::permissive_upload(
                        "test-bucket",
                        "uploaded",
                        1024,
                    ),
                    body(b"fresh"),
                    permit,
                )
                .await
        });
        let starter = catalog.clone();
        let session = tokio::spawn(async move {
            starter
                .create_resumable_session(
                    "test-bucket",
                    "session",
                    crate::storage::ResumableOptions::new(crate::bucket::BucketAccess::Permissive),
                    None,
                )
                .await
                .map(drop)
        });
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert!(!read.is_finished());
        assert!(!upload.is_finished());
        assert!(!session.is_finished());

        release.send(()).unwrap();
        joined(commit).await.unwrap();
        assert_eq!(joined(read).await.unwrap().size, 3);
        assert_eq!(joined(upload).await.unwrap().size, 5);
        joined(session).await.unwrap();
        assert_eq!(bytes(&admin, "object").await, b"new");
        assert_eq!(bytes(&admin, "uploaded").await, b"fresh");
        catalog.close().await.unwrap();
    }
}

/// A batch keeps its own lease while it stages, so the job it may be holding back must not hold
/// the next file back in turn.
#[tokio::test]
async fn a_batch_keeps_staging_while_a_reset_waits_for_it() {
    let (dir, catalog, admin, _bucket, _name) = setup().await;
    let mut batch = admin.seed_batch().await.unwrap();
    let resetting = admin.clone();
    let reset = tokio::spawn(async move {
        resetting
            .reset_until(crate::time_limit::TimeLimit::from_millis(60_000).deadline())
            .await
            .map(drop)
    });
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert!(!reset.is_finished());
    add(&mut batch, &dir.0, "object", b"new").await;
    drop(batch);
    joined(reset).await.unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn seed_admission_waits_for_a_competing_batch_to_finish() {
    let (_dir, catalog, admin, _bucket, _name) = setup().await;
    let first = admin.seed_batch().await.unwrap();
    let mut second = std::pin::pin!(admin.seed_batch());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut second)
            .await
            .is_err()
    );
    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(3), second)
        .await
        .unwrap()
        .unwrap();
    drop(second);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_seed_waits_release_admission_and_allow_reset() {
    let (dir, catalog, admin, _bucket, _name) = setup().await;
    let requests = catalog.0.publication_requests.available_permits();
    let first = admin.seed_batch().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), admin.seed_batch())
            .await
            .is_err()
    );
    drop(first);
    let mut batch = admin.seed_batch().await.unwrap();
    add(&mut batch, &dir.0, "object", b"new").await;
    let entry = batch.entries.pop().unwrap();
    batch
        .entries
        .push(prepare_entry(catalog.clone(), entry).await.unwrap());
    let upload = catalog.admit_object_upload().unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), batch.commit(SeedMode::Merge))
            .await
            .is_err()
    );
    drop(upload);
    // The cancelled commit hands its staged object to a detached cleanup, which owns the request.
    eventually_async("the cancelled batch's request to be returned", || async {
        catalog.0.publication_requests.available_permits() == requests
    })
    .await;
    // Cancelled waits leave no lease behind, so an exclusive reset can still proceed.
    tokio::time::timeout(Duration::from_secs(3), admin.reset())
        .await
        .unwrap()
        .unwrap();
    catalog.close().await.unwrap();
}

/// Past the two requests a seed could once share before the next one was refused.
const QUEUED_SEEDS: usize = 8;

/// A seed that runs a whole batch once its turn comes. It names its bucket, so it succeeds on the
/// empty catalog a reset leaves as well.
fn spawn_seed(
    admin: &Admin,
    directory: &Path,
    index: usize,
) -> tokio::task::JoinHandle<Result<Counts, AdminError>> {
    let (admin, directory) = (admin.clone(), directory.to_owned());
    tokio::spawn(async move {
        let mut batch = admin.seed_batch().await?;
        batch.bucket(BucketDefinition::default_for(
            &BucketName::new("test-bucket")?,
            "project",
            &SystemClock,
        )?)?;
        let name = format!("queued-{index}");
        add(&mut batch, &directory, &name, name.as_bytes()).await;
        batch.commit(SeedMode::Merge).await
    })
}

#[tokio::test]
async fn seeds_past_the_active_batch_take_turns_and_all_commit() {
    let (dir, catalog, admin, _bucket, _name) = setup().await;
    let active = admin.seed_batch().await.unwrap();
    let seeds: Vec<_> = (0..QUEUED_SEEDS)
        .map(|index| spawn_seed(&admin, &dir.0, index))
        .collect();
    eventually_async("the seeds to fill the request slots", || async {
        catalog.0.publication_requests.available_permits() == 0
    })
    .await;
    drop(active);
    for seed in seeds {
        assert_eq!(seed.await.unwrap().unwrap().objects, 1);
    }
    for index in 0..QUEUED_SEEDS {
        let name = format!("queued-{index}");
        assert_eq!(bytes(&admin, &name).await, name.as_bytes());
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_reset_passes_the_seeds_waiting_in_line_and_they_run_after_it() {
    let (dir, catalog, admin, _bucket, _name) = setup().await;
    let active = admin.seed_batch().await.unwrap();
    let seeds: Vec<_> = (0..QUEUED_SEEDS)
        .map(|index| spawn_seed(&admin, &dir.0, index))
        .collect();
    eventually_async("the seeds to fill the request slots", || async {
        catalog.0.publication_requests.available_permits() == 0
    })
    .await;
    let worker = admin.clone();
    let reset = tokio::spawn(async move {
        worker
            .reset_until(crate::time_limit::TimeLimit::from_millis(60_000).deadline())
            .await
    });
    eventually_async("the reset to line up", || async {
        catalog.0.admin_reads.available_permits() == 31
    })
    .await;
    // Only the active batch's lease stands in the reset's way: seeds in line hold none.
    drop(active);
    // Within the seeds' own wait of 30 s, so a reset held up by the line fails here.
    tokio::time::timeout(Duration::from_secs(20), reset)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for seed in seeds {
        assert_eq!(seed.await.unwrap().unwrap().objects, 1);
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn closing_wakes_every_seed_waiting_for_its_turn() {
    let (dir, catalog, admin, _bucket, _name) = setup().await;
    let requests = catalog
        .0
        .publication_requests
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let seeds: Vec<_> = (0..QUEUED_SEEDS)
        .map(|index| spawn_seed(&admin, &dir.0, index))
        .collect();
    tokio::task::yield_now().await;
    let worker = catalog.clone();
    let close = tokio::spawn(async move { worker.close().await });
    // Their own deadline is 30 s away, so only the close can end the wait this soon.
    for seed in seeds {
        let result = tokio::time::timeout(Duration::from_secs(10), seed)
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
async fn atomic_seed_protected_and_importing_namespaces_roll_back_new_buckets_and_objects() {
    let (dir, catalog, admin, bucket, name) = setup().await;
    let mut held = request(None);
    held.metadata.temporary_hold = Some(true);
    let old = admin
        .publish_object(&bucket, &name, held, body(b"old"))
        .await
        .unwrap();
    for mode in [SeedMode::Merge, SeedMode::Replace] {
        let mut batch = admin.seed_batch().await.unwrap();
        batch
            .bucket(
                BucketDefinition::default_for(
                    &BucketName::new("new-bucket").unwrap(),
                    "project",
                    &SystemClock,
                )
                .unwrap(),
            )
            .unwrap();
        add(&mut batch, &dir.0, "first", b"new").await;
        add(&mut batch, &dir.0, "object", b"new").await;
        assert!(matches!(batch.commit(mode).await, Err(AdminError::Hold)));
        assert!(
            admin
                .get_bucket(&BucketName::new("new-bucket").unwrap())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            admin
                .get_object(&bucket, &ObjectName::new("first").unwrap(), None)
                .await,
            Err(AdminError::NotFound)
        );
        assert_eq!(admin.get_object(&bucket, &name, None).await.unwrap(), old);
    }
    let mut unchanged = admin.seed_batch().await.unwrap();
    add(&mut unchanged, &dir.0, "object", b"old").await;
    assert_eq!(
        unchanged.commit(SeedMode::Merge).await.unwrap().unchanged,
        1
    );
    // A real journal binding supplies the incomplete-import guard even when no bucket has yet published.
    let other = TestDir::new();
    let importing = Catalog::open(&other.0).await.unwrap();
    importing
        .import_journal()
        .begin(crate::storage::import_journal::ImportBinding {
            source: [1; 32],
            fingerprint: [2; 32],
            expected_elements: 1,
        })
        .await
        .unwrap();
    let admin = importing.admin(Origin::Seed);
    let mut batch = admin.seed_batch().await.unwrap();
    batch.bucket(super::tests::bucket()).unwrap();
    assert!(matches!(
        batch.commit(SeedMode::Merge).await,
        Err(AdminError::Precondition)
    ));
    assert!(
        admin
            .get_bucket(&BucketName::new("test-bucket").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    importing.close().await.unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn atomic_ram_reseed_replaces_a_full_catalog_without_growing_a_deletion_journal() {
    let source = TestDir::new();
    let catalog = Catalog::open_memory_for_server(
        1024 * 1024,
        1024 * 1024,
        Arc::new(SystemClock),
        super::super::super::LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    let admin = catalog.admin(Origin::Seed);
    admin.ensure_bucket(bucket()).await.unwrap();
    let target = BucketName::new("test-bucket").unwrap();
    for index in 0..200 {
        admin
            .publish_object(
                &target,
                &ObjectName::new(format!("old-{index:04}")).unwrap(),
                request(None),
                body(b"old"),
            )
            .await
            .unwrap();
    }
    let (_, mut reader) = admin
        .open_object(&target, &ObjectName::new("old-0000").unwrap(), None)
        .await
        .unwrap();
    let mut full = false;
    for index in 0..200 {
        let name = BucketName::new(format!("bucket-{index:04}")).unwrap();
        let mut definition = BucketDefinition::default_for(&name, "project", &SystemClock).unwrap();
        definition.0.labels = (0..64)
            .map(|index| (format!("label-{index:02}"), "v".repeat(63)))
            .collect();
        match admin.ensure_bucket(definition).await {
            Ok(_) => {}
            Err(AdminError::DiskFull(_)) => {
                full = true;
                break;
            }
            Err(error) => panic!("unexpected error: {error}"),
        }
    }
    assert!(full);
    let mut batch = admin.seed_batch().await.unwrap();
    batch.bucket(bucket()).unwrap();
    add(&mut batch, &source.0, "replacement", b"new").await;
    batch.commit(SeedMode::Replace).await.unwrap();
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"old"[..]);
    assert_eq!(bytes(&admin, "replacement").await, b"new");
    drop(reader);
    assert_eq!(
        admin
            .get_object(&target, &ObjectName::new("old-0000").unwrap(), None)
            .await,
        Err(AdminError::NotFound)
    );
    let pending = catalog
        .run_admin(|connection| {
            connection
                .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(unavailable)
        })
        .await
        .unwrap();
    assert_eq!(pending, 0);
    admin
        .publish_object(
            &target,
            &ObjectName::new("after-reseed").unwrap(),
            request(None),
            body(b"after"),
        )
        .await
        .unwrap();
    assert_eq!(bytes(&admin, "replacement").await, b"new");
    assert_eq!(bytes(&admin, "after-reseed").await, b"after");
    catalog.close().await.unwrap();
}
