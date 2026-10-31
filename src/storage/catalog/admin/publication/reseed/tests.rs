use super::*;
use crate::{
    clock::SystemClock,
    storage::catalog::{
        admin::tests::{body, request, setup},
        fault::{Fault, Step},
    },
    test_support::TestDir,
};
use std::{fs, io::Read};

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
    let mut batch = admin.seed_batch().unwrap();
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
    let mut batch = admin.seed_batch().unwrap();
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
    let mut batch = admin.seed_batch().unwrap();
    for index in 0..300 {
        add(&mut batch, &dir.0, &format!("new-{index}"), b"new").await;
    }
    assert!(matches!(admin.seed_batch(), Err(AdminError::Busy { .. })));
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
        let mut batch = admin.seed_batch().unwrap();
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
        for power_loss in [false, true] {
            let (dir, catalog, admin, _, _) = setup().await;
            let mut initial = admin.seed_batch().unwrap();
            add(&mut initial, &dir.0, "object", b"old").await;
            initial.commit(SeedMode::Merge).await.unwrap();
            let snapshot = TestDir::new();
            let path = snapshot.0.join("snapshot");
            catalog.inject(
                step,
                0,
                if power_loss {
                    Fault::PowerLoss(path.clone())
                } else {
                    Fault::Snapshot(path.clone())
                },
            );
            let mut batch = admin.seed_batch().unwrap();
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
                "{step:?} {power_loss}"
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
    let mut batch = admin.seed_batch().unwrap();
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
    let mut batch = admin.seed_batch().unwrap();
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
        AdminError::DiskFull
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
    let mut source = source;
    let mut contents = Vec::new();
    source.read_to_end(&mut contents).unwrap();
    assert_eq!(contents, b"source");
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
    let mut batch = admin.seed_batch().unwrap();
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
    let mut batch = admin.seed_batch().unwrap();
    add(&mut batch, &dir.0, "object", b"new").await;
    let upload = catalog.admit_object_upload().unwrap();
    assert!(matches!(
        batch.commit(SeedMode::Merge).await,
        Err(AdminError::Busy { .. })
    ));
    drop(upload);
    assert_eq!(admin.get_object(&bucket, &name, None).await.unwrap(), old);
    assert_eq!(bytes(&admin, "object").await, b"old");
    catalog.close().await.unwrap();
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
        let mut batch = admin.seed_batch().unwrap();
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
    let mut unchanged = admin.seed_batch().unwrap();
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
    let mut batch = admin.seed_batch().unwrap();
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
            Err(AdminError::DiskFull) => {
                full = true;
                break;
            }
            Err(error) => panic!("unexpected error: {error}"),
        }
    }
    assert!(full);
    let mut batch = admin.seed_batch().unwrap();
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
