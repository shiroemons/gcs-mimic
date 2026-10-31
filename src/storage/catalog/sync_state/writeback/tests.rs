#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::{
    clock::SystemClock,
    object::UploadChunk,
    storage::{
        Catalog,
        admin::{BucketDefinition, GenerationCondition, Origin, Publication, PublishMode},
    },
    test_support::TestDir,
};
use bytes::Bytes;
use std::{fs, io::Seek, time::Duration};

async fn setup() -> (TestDir, TestDir, Catalog, RootLease, Writeback, BucketName) {
    let data = TestDir::new();
    let root = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    catalog
        .admin(Origin::SyncIngest)
        .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
        .await
        .unwrap();
    catalog.enable_change_journal().await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let writeback = lease
        .writeback(Limits::new(1_099_511_627_776, 1000).unwrap())
        .await
        .unwrap();
    (data, root, catalog, lease, writeback, bucket)
}
async fn publish(
    catalog: &Catalog,
    bucket: &BucketName,
    name: &str,
    bytes: &[u8],
) -> crate::object::ObjectRecord {
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    tx.try_send(Ok(UploadChunk::Data(Bytes::copy_from_slice(bytes))))
        .unwrap();
    tx.try_send(Ok(UploadChunk::End)).unwrap();
    catalog
        .admin(Origin::SyncIngest)
        .publish_object(
            bucket,
            &ObjectName::new(name).unwrap(),
            Publication {
                content_type: "application/octet-stream".into(),
                metadata: Default::default(),
                expected: Default::default(),
                condition: None,
                max_bytes: 4096,
                mode: PublishMode::Replace,
            },
            rx,
        )
        .await
        .unwrap()
}
async fn fence(catalog: &Catalog) -> JournalFence {
    catalog.read_changes(None, None, 1).await.unwrap().fence
}

#[tokio::test]
async fn writeback_abandon_unpublished_copy_preserves_external_bytes_and_releases_reservations() {
    let (_data, root, catalog, lease, writeback, bucket) = setup().await;
    let record = publish(&catalog, &bucket, "object", b"api").await;
    let name = ObjectName::new("object").unwrap();
    let (_, reader) = catalog
        .admin(Origin::SyncIngest)
        .open_object(&bucket, &name, None)
        .await
        .unwrap();
    let (pending, proof) = writeback
        .prepare_publish_copy(
            &bucket,
            &name,
            Generation::new(record.generation).unwrap(),
            fence(&catalog).await,
            Some(reader.into_file().await.unwrap()),
        )
        .await
        .unwrap();
    let armed = writeback.arm(pending.token).await.unwrap();
    output(&root, "object", b"external");
    let entry = armed.workspace_entry.name().to_owned();
    assert!(
        writeback
            .file_job(armed.token, move |root, workspace| {
                let parent = fsutil::directory_path_at(
                    root,
                    "test-bucket".as_ref(),
                    fsutil::DirectoryOpen::Existing,
                )?;
                fsutil::durable_replace_between(
                    workspace,
                    entry.as_ref(),
                    &parent,
                    "object".as_ref(),
                    None,
                )
                .map_err(|_| AdminError::Io(std::io::ErrorKind::AlreadyExists))?;
                Ok(())
            })
            .await
            .is_err()
    );
    writeback.abandon_unpublished(proof).await.unwrap();
    assert_eq!(
        fs::read(root.0.join("test-bucket/object")).unwrap(),
        b"external"
    );
    let status = writeback.status().await.unwrap();
    assert_eq!(
        (status.entries, status.reserved_bytes, status.stop),
        (0, 0, Stop::Ready)
    );
    assert!(writeback.page(0, 256).await.unwrap().is_empty());
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writeback_abandon_rejects_installed_and_modified_workspace_bodies() {
    let (_data, root, catalog, lease, writeback, bucket) = setup().await;
    let record = publish(&catalog, &bucket, "object", b"api").await;
    let name = ObjectName::new("object").unwrap();
    let (_, reader) = catalog
        .admin(Origin::SyncIngest)
        .open_object(&bucket, &name, None)
        .await
        .unwrap();
    let (pending, proof) = writeback
        .prepare_publish_copy(
            &bucket,
            &name,
            Generation::new(record.generation).unwrap(),
            fence(&catalog).await,
            Some(reader.into_file().await.unwrap()),
        )
        .await
        .unwrap();
    let armed = writeback.arm(pending.token).await.unwrap();
    let entry = armed.workspace_entry.name().to_owned();
    writeback
        .file_job(armed.token, move |root, workspace| {
            let parent = fsutil::directory_path_at(
                root,
                "test-bucket".as_ref(),
                fsutil::DirectoryOpen::CreateIfMissing,
            )?;
            fsutil::durable_replace_between(
                workspace,
                entry.as_ref(),
                &parent,
                "object".as_ref(),
                None,
            )
            .map_err(|_| AdminError::Precondition)?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        writeback.abandon_unpublished(proof.clone()).await,
        Err(Error::Protected)
    ));
    assert!(
        writeback
            .finish_obsolete_publish(proof.clone())
            .await
            .is_err()
    );
    assert_eq!(writeback.status().await.unwrap().entries, 1);
    assert_eq!(fs::read(root.0.join("test-bucket/object")).unwrap(), b"api");
    publish(&catalog, &bucket, "object", b"new-api").await;
    writeback.finish_obsolete_publish(proof).await.unwrap();
    assert_eq!(writeback.status().await.unwrap().stop, Stop::Ready);
    assert!(writeback.page(0, 256).await.unwrap().is_empty());
    let record = publish(&catalog, &bucket, "another", b"api").await;
    let name = ObjectName::new("another").unwrap();
    let (_, reader) = catalog
        .admin(Origin::SyncIngest)
        .open_object(&bucket, &name, None)
        .await
        .unwrap();
    let (pending, proof) = writeback
        .prepare_publish_copy(
            &bucket,
            &name,
            Generation::new(record.generation).unwrap(),
            fence(&catalog).await,
            Some(reader.into_file().await.unwrap()),
        )
        .await
        .unwrap();
    let entry = root
        .0
        .join(mapping::WORKSPACE)
        .join(pending.workspace_entry.name());
    fs::write(&entry, b"external").unwrap();
    assert!(matches!(
        writeback.abandon_unpublished(proof).await,
        Err(Error::Protected)
    ));
    assert_eq!(fs::read(entry).unwrap(), b"external");
    assert_eq!(writeback.status().await.unwrap().entries, 1);
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}
async fn prepare(
    writeback: &Writeback,
    catalog: &Catalog,
    bucket: &BucketName,
    name: &str,
    record: &crate::object::ObjectRecord,
) -> Pending {
    writeback
        .prepare(
            bucket,
            &ObjectName::new(name).unwrap(),
            Generation::new(record.generation).unwrap(),
            Kind::Publish,
            fence(catalog).await,
        )
        .await
        .unwrap()
}
fn output(root: &TestDir, name: &str, bytes: &[u8]) {
    let path = root.0.join("test-bucket").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[tokio::test]
async fn writeback_durable_receipts_atomically_ack_fingerprints_progress_and_completion() {
    let (_data, root, catalog, lease, writeback, bucket) = setup().await;
    let record = publish(&catalog, &bucket, "binary", b"\0\xffbytes").await;
    let pending = prepare(&writeback, &catalog, &bucket, "binary", &record).await;
    assert!(matches!(
        writeback.durable(pending.token.clone()).await,
        Err(Error::Admin(AdminError::Precondition))
    ));
    let armed = writeback.arm(pending.token.clone()).await.unwrap();
    assert!(writeback.arm(pending.token).await.is_err());
    output(&root, "binary", b"\0\xffbytes");
    let receipt = writeback.durable(armed.token).await.unwrap();
    let current = fence(&catalog).await;
    let complete = writeback.acknowledge(receipt, Some(current)).await.unwrap();
    assert_eq!(complete.phase, Phase::Complete);
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    let observed = catalog
        .sync_state()
        .get_path(binding, "test-bucket/binary".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.generation.get(), record.generation);
    assert_eq!(observed.fingerprint.md5, record.md5);
    let status = writeback.status().await.unwrap();
    assert_eq!(status.progress, Some(current));
    assert_eq!(status.entries, 0);
    assert!(writeback.page(0, 256).await.unwrap().is_empty());
    assert_eq!(writeback.resume().await.unwrap().stop, Stop::Ready);
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn writeback_export_cross_directory_publication_precedes_state_and_retains_displacement() {
    let (_data, root, catalog, lease, writeback, bucket) = setup().await;
    let name = ObjectName::new("nested/body").unwrap();
    let record = publish(&catalog, &bucket, name.as_str(), b"\0\xffexport").await;
    let pending = prepare(&writeback, &catalog, &bucket, name.as_str(), &record).await;
    let armed = writeback.arm(pending.token).await.unwrap();
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    assert!(
        catalog
            .sync_state()
            .get_path(binding.clone(), "test-bucket/nested/body".into())
            .await
            .unwrap()
            .is_none()
    );
    let (_, reader) = catalog
        .admin(Origin::SyncIngest)
        .open_object(&bucket, &name, None)
        .await
        .unwrap();
    let source = reader.into_file().await.unwrap();
    let entry = armed.workspace_entry.name().to_owned();
    writeback
        .file_job(armed.token.clone(), move |root, workspace| {
            fsutil::clone_or_copy_into(&source, workspace, entry.as_ref()).map_err(
                |e| match e {
                    fsutil::FsError::Io(e) => AdminError::from(e),
                    _ => AdminError::Io(std::io::ErrorKind::Unsupported),
                },
            )?;
            let parent = fsutil::directory_path_at(
                root,
                "test-bucket/nested".as_ref(),
                fsutil::DirectoryOpen::CreateIfMissing,
            )?;
            fsutil::durable_replace_between(
                workspace,
                entry.as_ref(),
                &parent,
                "body".as_ref(),
                None,
            )
            .map_err(|e| match e {
                fsutil::FsError::Io(e) => AdminError::from(e),
                _ => AdminError::Io(std::io::ErrorKind::Unsupported),
            })?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fs::read(root.0.join("test-bucket/nested/body")).unwrap(),
        b"\0\xffexport"
    );
    assert!(
        catalog
            .sync_state()
            .get_path(binding.clone(), "test-bucket/nested/body".into())
            .await
            .unwrap()
            .is_none()
    );
    let receipt = writeback.durable(armed.token).await.unwrap();
    assert!(
        catalog
            .sync_state()
            .get_path(binding.clone(), "test-bucket/nested/body".into())
            .await
            .unwrap()
            .is_none()
    );
    writeback.acknowledge(receipt, None).await.unwrap();
    assert_eq!(
        catalog
            .sync_state()
            .get_path(binding, "test-bucket/nested/body".into())
            .await
            .unwrap()
            .unwrap()
            .generation
            .get(),
        record.generation
    );
    writeback.resume().await.unwrap();
    let replacement = publish(&catalog, &bucket, name.as_str(), b"new API bytes").await;
    let pending = prepare(&writeback, &catalog, &bucket, name.as_str(), &replacement).await;
    let armed = writeback.arm(pending.token).await.unwrap();
    let (_, reader) = catalog
        .admin(Origin::SyncIngest)
        .open_object(&bucket, &name, None)
        .await
        .unwrap();
    let source = reader.into_file().await.unwrap();
    let entry = armed.workspace_entry.name().to_owned();
    writeback
        .file_job(armed.token.clone(), move |root, workspace| {
            fsutil::clone_or_copy_into(&source, workspace, entry.as_ref()).map_err(
                |e| match e {
                    fsutil::FsError::Io(e) => AdminError::from(e),
                    _ => AdminError::Io(std::io::ErrorKind::Unsupported),
                },
            )?;
            let parent = fsutil::directory_path_at(
                root,
                "test-bucket/nested".as_ref(),
                fsutil::DirectoryOpen::Existing,
            )?;
            let expected =
                fsutil::stat_identity(&fsutil::open_regular_at(&parent, "body".as_ref())?)
                    .map_err(|e| match e {
                        fsutil::FsError::Io(e) => AdminError::from(e),
                        _ => AdminError::Io(std::io::ErrorKind::Unsupported),
                    })?;
            assert!(matches!(
                fsutil::durable_replace_between(
                    workspace,
                    entry.as_ref(),
                    &parent,
                    "body".as_ref(),
                    Some(&expected)
                )
                .unwrap(),
                fsutil::ReplaceOutcome::Replaced { .. }
            ));
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        writeback.durable(armed.token).await,
        Err(Error::Protected)
    ));
    assert_eq!(
        fs::read(
            root.0
                .join(crate::sync::mapping::WORKSPACE)
                .join(armed.workspace_entry.name())
        )
        .unwrap(),
        b"\0\xffexport"
    );
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    assert_eq!(
        catalog
            .sync_state()
            .get_path(binding, "test-bucket/nested/body".into())
            .await
            .unwrap()
            .unwrap()
            .generation
            .get(),
        record.generation
    );
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn writeback_export_parent_sync_failure_cannot_commit_state() {
    for fail_source in [true, false] {
        let (_data, root, catalog, lease, writeback, bucket) = setup().await;
        let name = ObjectName::new("body").unwrap();
        let record = publish(&catalog, &bucket, name.as_str(), b"durable output").await;
        let pending = prepare(&writeback, &catalog, &bucket, name.as_str(), &record).await;
        let armed = writeback.arm(pending.token).await.unwrap();
        let (_, reader) = catalog
            .admin(Origin::SyncIngest)
            .open_object(&bucket, &name, None)
            .await
            .unwrap();
        let source = reader.into_file().await.unwrap();
        let entry = armed.workspace_entry.name().to_owned();
        let result: Result<(), Error> = writeback
            .file_job(armed.token.clone(), move |root, workspace| {
                fsutil::clone_or_copy_into(&source, workspace, entry.as_ref()).unwrap();
                let parent = fsutil::directory_path_at(
                    root,
                    "test-bucket".as_ref(),
                    fsutil::DirectoryOpen::CreateIfMissing,
                )?;
                let identity =
                    fsutil::stat_identity(if fail_source { workspace } else { &parent }).unwrap();
                *fsutil::FAIL_REPLACE_SYNC.lock().unwrap() =
                    Some((identity.device, identity.inode));
                let result = fsutil::durable_replace_between(
                    workspace,
                    entry.as_ref(),
                    &parent,
                    "body".as_ref(),
                    None,
                );
                *fsutil::FAIL_REPLACE_SYNC.lock().unwrap() = None;
                match result {
                    Err(fsutil::FsError::Io(error)) => Err(error.into()),
                    _ => panic!("parent sync must fail"),
                }
            })
            .await;
        assert!(matches!(result, Err(Error::Admin(AdminError::Io(_)))));
        assert_eq!(
            fs::read(root.0.join("test-bucket/body")).unwrap(),
            b"durable output"
        );
        assert_eq!(
            writeback
                .get(armed.workspace_entry.name())
                .await
                .unwrap()
                .phase,
            Phase::Armed
        );
        assert_eq!(writeback.status().await.unwrap().stop, Stop::IoFailure);
        let binding = catalog.sync_state().binding(&bucket).await.unwrap();
        assert!(
            catalog
                .sync_state()
                .get_path(binding.clone(), "test-bucket/body".into())
                .await
                .unwrap()
                .is_none()
        );
        let receipt = writeback.durable(armed.token).await.unwrap();
        writeback.acknowledge(receipt, None).await.unwrap();
        assert_eq!(
            catalog
                .sync_state()
                .get_path(binding, "test-bucket/body".into())
                .await
                .unwrap()
                .unwrap()
                .generation
                .get(),
            record.generation
        );
        drop(writeback);
        drop(lease);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn writeback_pending_pages_survive_bucket_deletion_recreation_and_restart() {
    let (data, root, catalog, lease, writeback, bucket) = setup().await;
    for index in 0..258 {
        let name = format!("object-{index:03}");
        let r = publish(&catalog, &bucket, &name, b"").await;
        prepare(&writeback, &catalog, &bucket, &name, &r).await;
    }
    let first = writeback.page(0, 256).await.unwrap();
    assert_eq!(first.len(), 256);
    let last = writeback
        .page(first.last().unwrap().ordinal, 256)
        .await
        .unwrap();
    assert_eq!(last.len(), 2);
    for index in 0..258 {
        catalog
            .admin(Origin::SyncIngest)
            .delete_object(
                &bucket,
                &ObjectName::new(format!("object-{index:03}")).unwrap(),
                GenerationCondition::Is(
                    writeback
                        .get(if index < 256 {
                            first[index].token.id()
                        } else {
                            last[index - 256].token.id()
                        })
                        .await
                        .unwrap()
                        .generation,
                ),
            )
            .await
            .unwrap();
    }
    catalog
        .delete_bucket(bucket.as_str(), None, None)
        .await
        .unwrap();
    catalog
        .admin(Origin::BucketInit)
        .ensure_bucket(BucketDefinition::default_for(&bucket, "new-owner", &SystemClock).unwrap())
        .await
        .unwrap();
    let original = first[0].clone();
    assert_eq!(
        writeback.get(original.token.id()).await.unwrap().owner,
        "owner"
    );
    assert!(writeback.arm(original.token.clone()).await.is_err());
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let wb = lease
        .writeback(Limits::new(1024, 1000).unwrap())
        .await
        .unwrap();
    let restored = wb.page(0, 256).await.unwrap();
    assert_eq!(restored.len(), 256);
    assert_eq!(restored[0].owner, "owner");
    assert_eq!(restored[0].incarnation, original.incarnation);
    assert_eq!(restored[0].workspace_entry, original.workspace_entry);
    assert_eq!(wb.status().await.unwrap().stop, Stop::RecoveryRequired);
    assert!(wb.page(-1, 1).await.is_err());
    assert!(wb.page(0, 257).await.is_err());
    drop(wb);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writeback_protected_sparse_body_exceeds_quota_without_cleanup_or_false_ack() {
    let (data, root, catalog, lease, writeback, bucket) = setup().await;
    let record = publish(&catalog, &bucket, "object", b"api").await;
    let pending = prepare(&writeback, &catalog, &bucket, "object", &record).await;
    let armed = writeback.arm(pending.token).await.unwrap();
    let path = root
        .0
        .join(mapping::WORKSPACE)
        .join(armed.workspace_entry.name());
    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .unwrap();
    file.set_len(1_099_511_627_777).unwrap();
    drop(file);
    let protected = writeback.inspect(armed.token.clone()).await.unwrap();
    assert_eq!(protected.phase, Phase::Protected);
    assert_eq!(protected.retained_bytes, Some(1_099_511_627_777));
    let usage = writeback.status().await.unwrap();
    assert_eq!(usage.retained_bytes, 1_099_511_627_777);
    assert_eq!(usage.protected_entries, 1);
    assert_eq!(usage.stop, Stop::ConflictCapacity);
    assert!(matches!(
        writeback.durable(protected.token).await,
        Err(Error::Protected)
    ));
    assert!(writeback.resume().await.is_err());
    assert_eq!(fs::metadata(&path).unwrap().len(), 1_099_511_627_777);
    catalog
        .admin(Origin::SyncIngest)
        .delete_object(
            &bucket,
            &ObjectName::new("object").unwrap(),
            GenerationCondition::Is(Generation::new(record.generation).unwrap()),
        )
        .await
        .unwrap();
    catalog
        .delete_bucket(bucket.as_str(), None, None)
        .await
        .unwrap();
    catalog
        .admin(Origin::BucketInit)
        .ensure_bucket(BucketDefinition::default_for(&bucket, "new-owner", &SystemClock).unwrap())
        .await
        .unwrap();
    let restored = writeback.get(armed.workspace_entry.name()).await.unwrap();
    assert_eq!(restored.owner, "owner");
    assert_eq!(restored.incarnation, armed.incarnation);
    assert_eq!(restored.phase, Phase::Protected);
    assert!(writeback.arm(restored.token).await.is_err());
    assert_eq!(
        writeback.status().await.unwrap().retained_bytes,
        1_099_511_627_777
    );
    assert_eq!(
        writeback.status().await.unwrap().stop,
        Stop::ConflictCapacity
    );
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let wb = lease
        .writeback(Limits::new(1024, 1000).unwrap())
        .await
        .unwrap();
    assert_eq!(wb.status().await.unwrap().retained_bytes, 1_099_511_627_777);
    assert!(wb.resume().await.is_err());
    assert_eq!(
        wb.get(armed.workspace_entry.name()).await.unwrap().owner,
        "owner"
    );
    assert!(path.exists());
    drop(wb);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writeback_rejects_foreign_stale_hold_and_changed_api_winners() {
    let (_data, root, catalog, lease, writeback, bucket) = setup().await;
    let record = publish(&catalog, &bucket, "object", b"api").await;
    let pending = prepare(&writeback, &catalog, &bucket, "object", &record).await;
    let held = publish(&catalog, &bucket, "held", b"held").await;
    catalog
        .update_object_metadata(
            bucket.as_str(),
            "held",
            None,
            Default::default(),
            crate::object::ObjectMetadataUpdate {
                event_based_hold: Some(true),
                ..Default::default()
            },
            |_| Ok(()),
        )
        .await
        .unwrap();
    assert!(matches!(
        writeback
            .prepare(
                &bucket,
                &ObjectName::new("held").unwrap(),
                Generation::new(held.generation).unwrap(),
                Kind::Publish,
                fence(&catalog).await
            )
            .await,
        Err(Error::Admin(AdminError::Hold))
    ));
    let other_root = TestDir::new();
    let other_lease = catalog
        .sync_state()
        .activate_root(&other_root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let other = other_lease
        .writeback(Limits::new(1024, 1000).unwrap())
        .await
        .unwrap();
    assert!(other.arm(pending.token.clone()).await.is_err());
    let armed = writeback.arm(pending.token).await.unwrap();
    output(&root, "object", b"api");
    let moved = other_root.0.join("moved-root");
    fs::rename(&root.0, &moved).unwrap();
    fs::create_dir(&root.0).unwrap();
    assert!(writeback.durable(armed.token.clone()).await.is_err());
    fs::remove_dir(&root.0).unwrap();
    fs::rename(moved, &root.0).unwrap();
    let receipt = writeback.durable(armed.token).await.unwrap();
    catalog
        .update_object_metadata(
            bucket.as_str(),
            "object",
            None,
            Default::default(),
            crate::object::ObjectMetadataUpdate {
                event_based_hold: Some(true),
                ..Default::default()
            },
            |_| Ok(()),
        )
        .await
        .unwrap();
    assert!(matches!(
        writeback.acknowledge(receipt, None).await,
        Err(Error::Admin(AdminError::Hold))
    ));
    catalog
        .update_object_metadata(
            bucket.as_str(),
            "object",
            None,
            Default::default(),
            crate::object::ObjectMetadataUpdate {
                event_based_hold: Some(false),
                ..Default::default()
            },
            |_| Ok(()),
        )
        .await
        .unwrap();
    let pending = writeback.page(0, 1).await.unwrap().remove(0);
    let receipt = writeback.durable(pending.token).await.unwrap();
    let newer = publish(&catalog, &bucket, "object", b"newer").await;
    assert!(writeback.acknowledge(receipt, None).await.is_err());
    assert_eq!(
        catalog
            .admin(Origin::SyncIngest)
            .get_object(&bucket, &ObjectName::new("object").unwrap(), None)
            .await
            .unwrap()
            .generation,
        newer.generation
    );
    assert_eq!(
        writeback.page(0, 256).await.unwrap()[0].phase,
        Phase::Durable
    );
    assert_eq!(writeback.status().await.unwrap().progress, None);
    drop(other);
    drop(other_lease);
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writeback_failed_ack_rolls_back_progress_and_fingerprint_then_retries() {
    let (_data, root, catalog, lease, writeback, bucket) = setup().await;
    let record = publish(&catalog, &bucket, "object", b"api").await;
    let pending = prepare(&writeback, &catalog, &bucket, "object", &record).await;
    let armed = writeback.arm(pending.token).await.unwrap();
    output(&root, "object", b"api");
    let receipt = writeback.durable(armed.token).await.unwrap();
    catalog.run_admin(|c|c.execute_batch("CREATE TEMP TRIGGER refuse_writeback BEFORE UPDATE OF phase ON sync_writeback_ops WHEN NEW.phase=4 BEGIN SELECT RAISE(ABORT,'injected completion failure'); END;").map_err(unavailable)).await.unwrap();
    assert!(
        writeback
            .acknowledge(receipt, Some(fence(&catalog).await))
            .await
            .is_err()
    );
    assert_eq!(writeback.status().await.unwrap().progress, None);
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    assert!(
        catalog
            .sync_state()
            .get_path(binding, "test-bucket/object".into())
            .await
            .unwrap()
            .is_none()
    );
    let old = writeback.page(0, 1).await.unwrap().remove(0);
    assert_eq!(old.phase, Phase::Durable);
    catalog
        .run_admin(|c| {
            c.execute_batch("DROP TRIGGER refuse_writeback")
                .map_err(unavailable)
        })
        .await
        .unwrap();
    let receipt = writeback.durable(old.token).await.unwrap();
    assert_eq!(
        writeback
            .acknowledge(receipt, Some(fence(&catalog).await))
            .await
            .unwrap()
            .phase,
        Phase::Complete
    );
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writeback_owned_descriptors_preserve_zero_binary_bytes_offsets_and_drain() {
    let (data, _root, catalog, lease, writeback, bucket) = setup().await;
    for (index, bytes) in [b"".as_slice(), b"\0\xffbinary".as_slice()]
        .into_iter()
        .enumerate()
    {
        let name = format!("selected-{index}");
        let record = publish(&catalog, &bucket, &name, bytes).await;
        let (_, mut reader) = catalog
            .admin(Origin::SyncIngest)
            .open_object(&bucket, &ObjectName::new(&name).unwrap(), None)
            .await
            .unwrap();
        reader
            .set_range(bytes.len().min(2) as u64, 0)
            .await
            .unwrap();
        let mut descriptor = reader.into_file().await.unwrap();
        let before = descriptor.stream_position().unwrap();
        publish(&catalog, &bucket, &name, b"replacement").await;
        catalog
            .admin(Origin::SyncIngest)
            .delete_object(
                &bucket,
                &ObjectName::new(&name).unwrap(),
                GenerationCondition::Is(
                    Generation::new(
                        catalog
                            .admin(Origin::SyncIngest)
                            .get_object(&bucket, &ObjectName::new(&name).unwrap(), None)
                            .await
                            .unwrap()
                            .generation,
                    )
                    .unwrap(),
                ),
            )
            .await
            .unwrap();
        let copied = lease
            .file_job(move |root| {
                let name = format!("copy-{index}");
                fsutil::clone_or_copy_into(&descriptor, root, name.as_ref())
                    .map_err(AdminError::backend)?;
                assert_eq!(descriptor.stream_position().unwrap(), before);
                Ok(fsutil::open_regular_at(root, name.as_ref())?)
            })
            .await
            .unwrap();
        assert_eq!(fsutil::stat_identity(&copied).unwrap().size, record.size);
        let mut buffer = vec![0; bytes.len()];
        assert_eq!(
            fsutil::read_at(&copied, &mut buffer, 0).unwrap(),
            bytes.len()
        );
        assert_eq!(buffer, bytes);
    }
    let record = publish(&catalog, &bucket, "drain", b"selected").await;
    let pending = prepare(&writeback, &catalog, &bucket, "drain", &record).await;
    let armed = writeback.arm(pending.token).await.unwrap();
    let (_, reader) = catalog
        .admin(Origin::SyncIngest)
        .open_object(&bucket, &ObjectName::new("drain").unwrap(), None)
        .await
        .unwrap();
    let fd = reader.into_file().await.unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::sync_channel(1);
    let worker = writeback.clone();
    let entry = armed.workspace_entry.name().to_owned();
    let task = tokio::spawn(async move {
        worker
            .file_job(armed.token, move |_root, workspace| {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                fsutil::clone_or_copy_into(&fd, workspace, entry.as_ref())
                    .map_err(AdminError::backend)?;
                Ok(())
            })
            .await
    });
    started.await.unwrap();
    task.abort();
    drop(writeback);
    drop(lease);
    let closing = catalog.clone();
    let mut close = tokio::spawn(async move { closing.close().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut close)
            .await
            .is_err()
    );
    assert!(Catalog::open(&data.0).await.is_err());
    release.send(()).unwrap();
    close.await.unwrap().unwrap();
    let reopened = Catalog::open(&data.0).await.unwrap();
    assert!(
        !reopened
            .sync_state()
            .catalog_id()
            .await
            .unwrap()
            .to_string()
            .is_empty()
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn writeback_progress_cannot_skip_pending_work_and_protection_stops_other_operations() {
    let (_data, root, catalog, lease, writeback, bucket) = setup().await;
    let a = publish(&catalog, &bucket, "a", b"a").await;
    let a = prepare(&writeback, &catalog, &bucket, "a", &a).await;
    let b = publish(&catalog, &bucket, "b", b"b").await;
    let b = prepare(&writeback, &catalog, &bucket, "b", &b).await;
    let b = writeback.arm(b.token).await.unwrap();
    assert!(writeback.arm(a.token.clone()).await.is_err());
    output(&root, "b", b"b");
    let receipt = writeback.durable(b.token).await.unwrap();
    assert!(
        writeback
            .acknowledge(receipt, Some(fence(&catalog).await))
            .await
            .is_err()
    );
    assert_eq!(writeback.status().await.unwrap().progress, None);
    let b = writeback.get(b.workspace_entry.name()).await.unwrap();
    let receipt = writeback.durable(b.token).await.unwrap();
    writeback.acknowledge(receipt, None).await.unwrap();
    assert!(writeback.resume().await.is_err());
    let a = writeback.arm(a.token).await.unwrap();
    output(&root, "a", b"a");
    let receipt = writeback.durable(a.token).await.unwrap();
    let current = fence(&catalog).await;
    writeback.acknowledge(receipt, Some(current)).await.unwrap();
    assert_eq!(writeback.status().await.unwrap().progress, Some(current));
    writeback.resume().await.unwrap();
    let c = publish(&catalog, &bucket, "c", b"c").await;
    let c = prepare(&writeback, &catalog, &bucket, "c", &c).await;
    let d = publish(&catalog, &bucket, "d", b"d").await;
    let d = prepare(&writeback, &catalog, &bucket, "d", &d).await;
    let c = writeback.arm(c.token).await.unwrap();
    output(&root, "c", b"c");
    let receipt = writeback.durable(c.token).await.unwrap();
    let entry = root
        .0
        .join(mapping::WORKSPACE)
        .join(c.workspace_entry.name());
    fs::write(&entry, b"late displaced body").unwrap();
    assert!(
        writeback
            .acknowledge(receipt, Some(fence(&catalog).await))
            .await
            .is_err()
    );
    assert_eq!(
        writeback.get(c.workspace_entry.name()).await.unwrap().phase,
        Phase::Protected
    );
    assert_eq!(writeback.status().await.unwrap().retained_bytes, 19);
    assert!(writeback.arm(d.token).await.is_err());
    assert_eq!(writeback.status().await.unwrap().progress, Some(current));
    assert_eq!(fs::read(entry).unwrap(), b"late displaced body");
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writeback_removal_requires_complete_baseline_and_absent_unchanged_api_winner() {
    let (_data, root, catalog, lease, writeback, bucket) = setup().await;
    assert!(writeback.checkpoint(fence(&catalog).await).await.is_err());
    let record = publish(&catalog, &bucket, "remove", b"original").await;
    let pending = prepare(&writeback, &catalog, &bucket, "remove", &record).await;
    let armed = writeback.arm(pending.token).await.unwrap();
    output(&root, "remove", b"original");
    let receipt = writeback.durable(armed.token).await.unwrap();
    writeback.acknowledge(receipt, None).await.unwrap();
    writeback.resume().await.unwrap();
    catalog
        .admin(Origin::SyncIngest)
        .delete_object(
            &bucket,
            &ObjectName::new("remove").unwrap(),
            GenerationCondition::Is(Generation::new(record.generation).unwrap()),
        )
        .await
        .unwrap();
    let selected = Generation::new(record.generation).unwrap();
    assert!(
        writeback
            .prepare(
                &bucket,
                &ObjectName::new("remove").unwrap(),
                selected,
                Kind::Remove,
                fence(&catalog).await
            )
            .await
            .is_err()
    );
    lease.complete_baseline().await.unwrap();
    writeback.checkpoint(fence(&catalog).await).await.unwrap();
    let pending = writeback
        .prepare(
            &bucket,
            &ObjectName::new("remove").unwrap(),
            selected,
            Kind::Remove,
            fence(&catalog).await,
        )
        .await
        .unwrap();
    let armed = writeback.arm(pending.token).await.unwrap();
    fs::remove_file(root.0.join("test-bucket/remove")).unwrap();
    let receipt = writeback.durable(armed.token).await.unwrap();
    let winner = publish(&catalog, &bucket, "remove", b"new API winner").await;
    assert!(writeback.acknowledge(receipt, None).await.is_err());
    catalog
        .admin(Origin::SyncIngest)
        .delete_object(
            &bucket,
            &ObjectName::new("remove").unwrap(),
            GenerationCondition::Is(Generation::new(winner.generation).unwrap()),
        )
        .await
        .unwrap();
    let pending = writeback.page(0, 1).await.unwrap().remove(0);
    let receipt = writeback.durable(pending.token).await.unwrap();
    writeback
        .acknowledge(receipt, Some(fence(&catalog).await))
        .await
        .unwrap();
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    assert!(
        catalog
            .sync_state()
            .get_path(binding, "test-bucket/remove".into())
            .await
            .unwrap()
            .is_none()
    );
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writeback_sqlite_enospc_keeps_displaced_bytes_and_retry_captures_actual_size() {
    let (_data, root, catalog, lease, writeback, bucket) = setup().await;
    let record = publish(&catalog, &bucket, "object", b"api").await;
    let pending = prepare(&writeback, &catalog, &bucket, "object", &record).await;
    let armed = writeback.arm(pending.token).await.unwrap();
    let path = root
        .0
        .join(mapping::WORKSPACE)
        .join(armed.workspace_entry.name());
    fs::write(&path, b"recover after full disk").unwrap();
    catalog.run_admin(|c| {
        c.execute_batch("CREATE TABLE writeback_fill(value BLOB)").map_err(unavailable)?;
        let pages:i64=c.pragma_query_value(None,"page_count",|r|r.get(0)).map_err(unavailable)?;
        c.pragma_update(None,"max_page_count",pages).map_err(unavailable)?;
        loop { match c.execute("INSERT INTO writeback_fill VALUES(zeroblob(4096))",[]) {Ok(_)=>{},Err(e)=>{assert!(matches!(e,rusqlite::Error::SqliteFailure(ref e,_) if e.code==rusqlite::ErrorCode::DiskFull));break;}} }
        c.execute_batch("CREATE TEMP TRIGGER writeback_full BEFORE UPDATE OF phase ON sync_writeback_ops WHEN NEW.phase=2 BEGIN INSERT INTO writeback_fill VALUES(zeroblob(4096)); END;").map_err(unavailable)
    }).await.unwrap();
    assert_eq!(
        writeback.inspect(armed.token.clone()).await.unwrap_err(),
        Error::Admin(AdminError::DiskFull)
    );
    assert_eq!(
        writeback
            .get(armed.workspace_entry.name())
            .await
            .unwrap()
            .phase,
        Phase::Armed
    );
    assert_eq!(
        writeback.status().await.unwrap().stop,
        Stop::RecoveryRequired
    );
    assert_eq!(fs::read(&path).unwrap(), b"recover after full disk");
    catalog
        .run_admin(|c| {
            c.pragma_update(None, "max_page_count", 1_000_000)
                .map_err(unavailable)?;
            c.execute_batch("DROP TRIGGER writeback_full; DROP TABLE writeback_fill")
                .map_err(unavailable)
        })
        .await
        .unwrap();
    let protected = writeback.inspect(armed.token).await.unwrap();
    assert_eq!(protected.phase, Phase::Protected);
    assert_eq!(writeback.status().await.unwrap().retained_bytes, 23);
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writeback_schema_migration_rolls_back_retries_and_preserves_api_seed_root_and_sync() {
    let (data, root, catalog, lease, writeback, bucket) = setup().await;
    let id = catalog.sync_state().catalog_id().await.unwrap();
    let root_id = lease.status().root_id;
    let mut record = publish(&catalog, &bucket, "preserved", b"immutable").await;
    let pending = prepare(&writeback, &catalog, &bucket, "preserved", &record).await;
    let armed = writeback.arm(pending.token).await.unwrap();
    output(&root, "preserved", b"immutable");
    let receipt = writeback.durable(armed.token).await.unwrap();
    writeback.acknowledge(receipt, None).await.unwrap();
    lease.complete_baseline().await.unwrap();
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
    let mut c = Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    crate::storage::catalog::test_support::downgrade_soft_delete_schema(&c);
    record.owner = None;
    let seed: i64 = c
        .query_row("SELECT complete FROM startup_seed", [], |r| r.get(0))
        .unwrap();
    c.execute_batch("ALTER TABLE buckets DROP COLUMN retention_policy; ALTER TABLE buckets DROP COLUMN lifecycle; ALTER TABLE buckets DROP COLUMN uniform_bucket_level_access_locked_time; ALTER TABLE objects DROP COLUMN temporary_hold; ALTER TABLE objects DROP COLUMN custom_time; DROP TABLE sync_findings; DROP TABLE sync_writeback_ops; DROP TABLE sync_writeback_roots; PRAGMA user_version=19; CREATE TABLE sync_writeback_ops(collision)").unwrap();
    assert!(migrate(&mut c).is_err());
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        19
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='sync_writeback_roots'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    c.execute_batch("DROP TABLE sync_writeback_ops; CREATE TABLE writeback_fill(value BLOB)")
        .unwrap();
    let pages: i64 = c
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    c.pragma_update(None, "max_page_count", pages).unwrap();
    loop {
        match c.execute("INSERT INTO writeback_fill VALUES(zeroblob(4096))", []) {
            Ok(_) => {}
            Err(e) => {
                assert!(
                    matches!(e,rusqlite::Error::SqliteFailure(ref e,_) if e.code==rusqlite::ErrorCode::DiskFull)
                );
                break;
            }
        }
    }
    assert!(migrate(&mut c).is_err());
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        19
    );
    c.pragma_update(None, "max_page_count", 1_000_000).unwrap();
    c.execute_batch("DROP TABLE writeback_fill").unwrap();
    {
        let tx = c.transaction().unwrap();
        tx.execute_batch(SCHEMA).unwrap();
        tx.rollback().unwrap();
    }
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        19
    );
    drop(c);
    let catalog = Catalog::open(&data.0).await.unwrap();
    assert_eq!(catalog.sync_state().catalog_id().await.unwrap(), id);
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    assert_eq!(lease.status().root_id, root_id);
    assert_eq!(
        lease.status().baseline,
        super::super::root::Baseline::Complete
    );
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    assert_eq!(
        catalog
            .sync_state()
            .get_path(binding, "test-bucket/preserved".into())
            .await
            .unwrap()
            .unwrap()
            .generation
            .get(),
        record.generation
    );
    let (actual, reader) = catalog
        .admin(Origin::SyncIngest)
        .open_object(&bucket, &ObjectName::new("preserved").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(actual, record);
    drop(reader);
    assert_eq!(
        catalog
            .run_admin(|c| c
                .query_row("SELECT complete FROM startup_seed", [], |r| r
                    .get::<_, i64>(0))
                .map_err(unavailable))
            .await
            .unwrap(),
        seed
    );
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writeback_restart_recovers_each_file_and_catalog_boundary_without_false_progress() {
    for boundary in [
        "prepared",
        "armed",
        "copied",
        "installed",
        "durable",
        "complete",
    ] {
        let (data, root, catalog, lease, writeback, bucket) = setup().await;
        let record = publish(&catalog, &bucket, "boundary", b"selected").await;
        let mut pending = prepare(&writeback, &catalog, &bucket, "boundary", &record).await;
        let id = pending.token.id().to_owned();
        if boundary != "prepared" {
            pending = writeback.arm(pending.token).await.unwrap();
        }
        if boundary == "copied" {
            fs::write(
                root.0
                    .join(mapping::WORKSPACE)
                    .join(pending.workspace_entry.name()),
                b"potential displaced bytes",
            )
            .unwrap();
        }
        if ["installed", "durable", "complete"].contains(&boundary) {
            output(&root, "boundary", b"selected");
        }
        if ["durable", "complete"].contains(&boundary) {
            let receipt = writeback.durable(pending.token).await.unwrap();
            if boundary == "complete" {
                writeback.acknowledge(receipt, None).await.unwrap();
            }
        }
        drop(writeback);
        drop(lease);
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let lease = catalog
            .sync_state()
            .activate_root(&root.0, super::super::root::Activation::Open)
            .await
            .unwrap();
        let writeback = lease
            .writeback(Limits::new(1024, 1000).unwrap())
            .await
            .unwrap();
        let mut pending = writeback.get(&id).await.unwrap();
        assert_eq!(writeback.status().await.unwrap().progress, None);
        if boundary == "copied" {
            pending = writeback.inspect(pending.token).await.unwrap();
            assert_eq!(pending.phase, Phase::Protected);
            assert!(writeback.durable(pending.token).await.is_err());
        } else if boundary != "complete" {
            assert_eq!(
                writeback.status().await.unwrap().stop,
                Stop::RecoveryRequired
            );
            assert!(writeback.resume().await.is_err());
            if pending.phase == Phase::Prepared {
                pending = writeback.arm(pending.token).await.unwrap();
            }
            if ["prepared", "armed"].contains(&boundary) {
                assert!(writeback.durable(pending.token.clone()).await.is_err());
                assert_eq!(writeback.status().await.unwrap().stop, Stop::IoFailure);
                output(&root, "boundary", b"selected");
            }
            let receipt = writeback.durable(pending.token).await.unwrap();
            writeback.acknowledge(receipt, None).await.unwrap();
            writeback.resume().await.unwrap();
        } else {
            assert_eq!(pending.phase, Phase::Complete);
            assert!(writeback.page(0, 256).await.unwrap().is_empty());
        }
        drop(writeback);
        drop(lease);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn writeback_workspace_replacement_keeps_owned_bytes_and_blocks_completion_after_restart() {
    let (data, root, catalog, lease, writeback, bucket) = setup().await;
    let record = publish(&catalog, &bucket, "object", b"api").await;
    let pending = prepare(&writeback, &catalog, &bucket, "object", &record).await;
    let armed = writeback.arm(pending.token).await.unwrap();
    output(&root, "object", b"displaced body");
    let source = fs::File::open(root.0.join("test-bucket/object")).unwrap();
    let entry = armed.workspace_entry.name().to_owned();
    let reserved = root.0.join(mapping::WORKSPACE);
    let moved = root.0.join("moved-workspace");
    let (replace, original) = (reserved.clone(), moved.clone());
    assert!(matches!(
        writeback
            .file_job(armed.token.clone(), move |_, workspace| {
                fs::rename(&replace, &original)?;
                fs::create_dir(&replace)?;
                fsutil::clone_or_copy_into(&source, workspace, entry.as_ref())
                    .map_err(AdminError::backend)?;
                Ok(())
            })
            .await,
        Err(Error::WorkspaceChanged)
    ));
    output(&root, "object", b"api");
    assert_eq!(
        fs::read(moved.join(armed.workspace_entry.name())).unwrap(),
        b"displaced body"
    );
    assert!(writeback.durable(armed.token.clone()).await.is_err());
    assert!(writeback.resume().await.is_err());
    let root_id = lease.status().root_id;
    catalog
        .run_admin(move |c| {
            let state: (i64, bool, bool) = c
                .query_row(
                    "SELECT entries,recovery,io_failure FROM sync_writeback_roots WHERE root_id=?1",
                    [root_id.as_bytes()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(unavailable)?;
            assert_eq!(state, (1, true, true));
            Ok(())
        })
        .await
        .unwrap();
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    assert!(matches!(
        lease.writeback(Limits::new(1024, 1000).unwrap()).await,
        Err(Error::WorkspaceChanged)
    ));
    assert!(reserved.read_dir().unwrap().next().is_none());
    fs::remove_dir(&reserved).unwrap();
    fs::rename(&moved, &reserved).unwrap();
    let writeback = lease
        .writeback(Limits::new(1024, 1000).unwrap())
        .await
        .unwrap();
    let pending = writeback.get(armed.workspace_entry.name()).await.unwrap();
    assert_eq!(pending.phase, Phase::Armed);
    assert_eq!(pending.owner, "owner");
    let protected = writeback.inspect(pending.token).await.unwrap();
    assert_eq!(protected.phase, Phase::Protected);
    assert_eq!(writeback.status().await.unwrap().retained_bytes, 14);
    assert!(writeback.durable(protected.token).await.is_err());
    assert!(writeback.resume().await.is_err());
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writeback_other_operation_success_cannot_clear_failed_entry_or_target_validation() {
    let (data, root, catalog, lease, writeback, bucket) = setup().await;
    let a = publish(&catalog, &bucket, "a", b"a").await;
    let a = prepare(&writeback, &catalog, &bucket, "a", &a).await;
    let b = publish(&catalog, &bucket, "b", b"b").await;
    let b = prepare(&writeback, &catalog, &bucket, "b", &b).await;
    let a = writeback.arm(a.token).await.unwrap();
    let failed = root
        .0
        .join(mapping::WORKSPACE)
        .join(b.workspace_entry.name());
    std::os::unix::fs::symlink("absent", &failed).unwrap();
    assert!(writeback.inspect(b.token.clone()).await.is_err());
    assert!(
        writeback
            .file_job(a.token.clone(), |_, _| -> Result<(), AdminError> {
                panic!("stopped operation must not enter its file job")
            })
            .await
            .is_err()
    );
    output(&root, "a", b"a");
    let receipt = writeback.durable(a.token).await.unwrap();
    assert_eq!(writeback.status().await.unwrap().stop, Stop::IoFailure);
    assert!(writeback.acknowledge(receipt, None).await.is_err());
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let writeback = lease
        .writeback(Limits::new(1024, 1000).unwrap())
        .await
        .unwrap();
    let a = writeback.get(a.workspace_entry.name()).await.unwrap();
    let receipt = writeback.durable(a.token).await.unwrap();
    assert!(writeback.acknowledge(receipt, None).await.is_err());
    assert_eq!(writeback.status().await.unwrap().stop, Stop::IoFailure);
    assert!(writeback.arm(b.token.clone()).await.is_err());
    fs::remove_file(failed).unwrap();
    let b = writeback.inspect(b.token).await.unwrap();
    assert_ne!(writeback.status().await.unwrap().stop, Stop::IoFailure);
    let a = writeback.get(a.workspace_entry.name()).await.unwrap();
    let receipt = writeback.durable(a.token).await.unwrap();
    writeback.acknowledge(receipt, None).await.unwrap();
    let b = writeback.arm(b.token).await.unwrap();
    output(&root, "b", b"wrong");
    assert!(writeback.durable(b.token.clone()).await.is_err());
    let failed = root
        .0
        .join(mapping::WORKSPACE)
        .join(b.workspace_entry.name());
    std::os::unix::fs::symlink("absent", &failed).unwrap();
    assert!(writeback.inspect(b.token.clone()).await.is_err());
    fs::remove_file(failed).unwrap();
    let b = writeback.inspect(b.token).await.unwrap();
    assert_eq!(writeback.status().await.unwrap().stop, Stop::IoFailure);
    output(&root, "b", b"b");
    let receipt = writeback.durable(b.token).await.unwrap();
    writeback.acknowledge(receipt, None).await.unwrap();
    assert_eq!(writeback.resume().await.unwrap().stop, Stop::Ready);
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn writeback_cancelled_job_serializes_separate_handles_until_protected_body_is_recorded() {
    let (data, root, catalog, lease, writeback, bucket) = setup().await;
    let other = lease
        .writeback(Limits::new(1024, 1000).unwrap())
        .await
        .unwrap();
    let record = publish(&catalog, &bucket, "object", b"api").await;
    let pending = prepare(&writeback, &catalog, &bucket, "object", &record).await;
    let armed = writeback.arm(pending.token).await.unwrap();
    output(&root, "object", b"api");
    let source_dir = TestDir::new();
    fs::write(source_dir.0.join("source"), b"late displaced body").unwrap();
    let source = fs::File::open(source_dir.0.join("source")).unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::sync_channel(1);
    let worker = writeback.clone();
    let token = armed.token.clone();
    let entry = armed.workspace_entry.name().to_owned();
    let task = tokio::spawn(async move {
        worker
            .file_job(token, move |_, workspace| {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                fsutil::clone_or_copy_into(&source, workspace, entry.as_ref())
                    .map_err(AdminError::backend)?;
                Ok(())
            })
            .await
    });
    started.await.unwrap();
    task.abort();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            other.durable(armed.token.clone())
        )
        .await
        .is_err()
    );
    let receipt = DurableReceipt {
        token: armed.token.clone(),
        fingerprint: None,
        lease: lease.clone(),
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(30), other.acknowledge(receipt, None))
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), other.resume())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            lease.writeback(Limits::new(1024, 1000).unwrap())
        )
        .await
        .is_err()
    );
    assert_eq!(catalog.0.admin_waiters.available_permits(), 32);
    assert_eq!(
        catalog
            .admin(Origin::SyncIngest)
            .get_object(&bucket, &ObjectName::new("object").unwrap(), None)
            .await
            .unwrap()
            .generation,
        record.generation
    );
    let recovering = other.clone();
    let token = armed.token.clone();
    let mut completion = tokio::spawn(async move { recovering.durable(token).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut completion)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    assert!(matches!(completion.await.unwrap(), Err(Error::Protected)));
    let retained = writeback.get(armed.workspace_entry.name()).await.unwrap();
    assert_eq!(retained.phase, Phase::Protected);
    assert_eq!(retained.owner, "owner");
    assert_eq!(retained.incarnation, armed.incarnation);
    let usage = writeback.status().await.unwrap();
    assert_eq!(usage.retained_bytes, 19);
    assert_eq!(usage.entries, 1);
    assert_eq!(usage.progress, None);
    assert_eq!(usage.stop, Stop::ProtectedBody);
    assert!(other.resume().await.is_err());
    drop(other);
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let writeback = lease
        .writeback(Limits::new(1024, 1000).unwrap())
        .await
        .unwrap();
    assert_eq!(
        writeback
            .get(armed.workspace_entry.name())
            .await
            .unwrap()
            .phase,
        Phase::Protected
    );
    let usage = writeback.status().await.unwrap();
    assert_eq!(usage.retained_bytes, 19);
    assert_eq!(usage.stop, Stop::ProtectedBody);
    assert_eq!(
        fs::read(
            root.0
                .join(mapping::WORKSPACE)
                .join(armed.workspace_entry.name())
        )
        .unwrap(),
        b"late displaced body"
    );
    assert!(writeback.resume().await.is_err());
    drop(writeback);
    drop(lease);
    catalog.close().await.unwrap();
}
