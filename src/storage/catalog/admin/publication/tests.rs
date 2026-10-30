#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::super::tests::{body, request, setup};
use super::*;
use std::io::{Seek, SeekFrom};
use std::{collections::BTreeMap, fs};

fn source(dir: &std::path::Path, bytes: &[u8]) -> File {
    let path = dir.join("source");
    fs::write(&path, bytes).unwrap();
    File::open(path).unwrap()
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
        Some(AdminError::DiskFull)
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
    let previous = (second.clone(), "unused".into());
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
        .update_object_metadata_authorized(
            crate::storage::ObjectSelector {
                bucket: bucket.as_str(),
                name: name.as_str(),
                generation: None,
                conditions: Default::default(),
            },
            patch,
            |_| Ok(()),
            &BucketAccess::Permissive,
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
        content_type: Some(Some("application/custom".into())),
        cache_control: Some(Some("private".into())),
        metadata: Some(Some(BTreeMap::from([(
            "patched".into(),
            Some("value".into()),
        )]))),
        ..Default::default()
    };
    catalog
        .update_object_metadata_authorized(
            crate::storage::ObjectSelector {
                bucket: bucket.as_str(),
                name: name.as_str(),
                generation: None,
                conditions: Default::default(),
            },
            patch,
            |_| Ok(()),
            &BucketAccess::Permissive,
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

#[tokio::test]
async fn publication_admits_only_one_active_and_one_waiting_batch() {
    let (dir, catalog, admin, _, _) = setup().await;
    let a = entry(&admin, &dir.0, "a", b"a").await;
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let worker = admin.clone();
    let active = tokio::spawn(async move { worker.publish_objects(vec![a]).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while catalog.0.publication_batch.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let b = entry(&admin, &dir.0, "b", b"b").await;
    let worker = admin.clone();
    let queued = tokio::spawn(async move { worker.publish_objects(vec![b]).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while catalog.0.publication_requests.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let c = entry(&admin, &dir.0, "c", b"c").await;
    assert!(matches!(
        admin.publish_objects(vec![c]).await,
        Err(AdminError::Busy { .. })
    ));
    assert!(!active.is_finished());
    assert!(!queued.is_finished());
    drop(execution);
    assert!(active.await.unwrap().unwrap().results[0].is_ok());
    assert!(queued.await.unwrap().unwrap().results[0].is_ok());
    catalog.close().await.unwrap();
    assert_eq!(blob_count(&dir.0), 2);
}

#[tokio::test]
async fn publication_cancelled_renamed_batch_cleans_before_shutdown_unlocks() {
    for cancel_request in [false, true] {
        let (dir, catalog, admin, bucket, name) = setup().await;
        let input = entry(&admin, &dir.0, "object", b"cancelled").await;
        let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
        let worker = admin.clone();
        let publishing = tokio::spawn(async move { worker.publish_objects(vec![input]).await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while blob_count(&dir.0) == 0 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
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
    let selected = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let selected = admin.get_object(&bucket, &name, None).await.unwrap();
            if selected.generation != old.generation
                && catalog.0.staged_handles.available_permits() == 256
            {
                break selected;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
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
        matches!(result, Err(AdminError::SourceChanged { fields }) if fields.contains(&"size".into()))
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
