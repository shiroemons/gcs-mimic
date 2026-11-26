use super::*;
use crate::storage::{
    ChunkEnd, ChunkWait, ResumableChunk, ResumableOptions,
    fault::{Crash, Fault},
};
use crate::test_support::{TestDir, blob_count};
use std::io::ErrorKind;

async fn copy(
    catalog: &Catalog,
    selected: DerivedSource,
    name: &str,
) -> Result<ObjectRecord, ObjectError> {
    let (activity, _updates) = tokio::sync::watch::channel(Default::default());
    crate::server::copy::execute(
        catalog,
        crate::server::copy::CopyJob {
            source: selected,
            source_access: BucketAccess::Permissive,
            destination: destination(name),
            source_location: None,
            replay: None,
        },
        crate::server::copy::CopyBudget {
            config: &crate::config::Config::default(),
            activity: &activity,
            deadline: crate::test_support::hang_deadline(),
        },
    )
    .await
}

async fn resumable_replace(catalog: &Catalog, name: &str) {
    let session = catalog
        .create_resumable_session(
            "bucket",
            name,
            ResumableOptions::new(BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    let lease = catalog
        .lock_resumable_session(&session.id.to_string(), "bucket")
        .await
        .unwrap();
    let chunk = ResumableChunk {
        start: 0,
        length: 3,
        end: ChunkEnd::Exact {
            total: Some(3),
            finalize: true,
        },
        options: lease.session.options.clone(),
        catalog_wait: tokio::sync::watch::channel(ChunkWait::Transfer).0,
    };
    catalog
        .write_resumable_session(
            lease,
            chunk,
            body(vec![Bytes::from_static(b"new")]),
            catalog.admit_object_upload().unwrap(),
            |_| Ok(serde_json::Value::Null),
        )
        .await
        .unwrap();
}

async fn delete(catalog: &Catalog, name: &str) {
    catalog
        .delete_object(live("bucket", name), Default::default(), PERMISSIVE)
        .await
        .unwrap();
}

#[tokio::test]
async fn shared_bodies_survive_replacement_and_deletion_from_either_name() {
    use crate::storage::catalog::{admin::LocalAdmin, open::Backing};
    for memory in [false, true] {
        let root = TestDir::new();
        let backing = if memory {
            Backing::test_memory(root.0.clone())
        } else {
            Backing::test_disk(root.0.clone())
        };
        let catalog = Catalog::open_backend(
            backing,
            std::sync::Arc::new(crate::clock::SystemClock),
            LocalAdmin::Disabled,
        )
        .await
        .unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        for (changed, kept) in [("source", "copy"), ("copy", "source")] {
            let original = put(&catalog, "source", b"original").await;
            let copied = copy(&catalog, source(&catalog, "source").await, "copy")
                .await
                .unwrap();
            assert_ne!(copied.generation, original.generation);
            assert_eq!(copied.hashes(), original.hashes());
            if memory {
                let (_, mut left) = catalog
                    .open_object(live("bucket", "source"), Default::default(), PERMISSIVE)
                    .await
                    .unwrap();
                let (_, mut right) = catalog
                    .open_object(live("bucket", "copy"), Default::default(), PERMISSIVE)
                    .await
                    .unwrap();
                let left = left.next_chunk().await.unwrap().unwrap();
                let right = right.next_chunk().await.unwrap().unwrap();
                assert_eq!(left.as_ptr(), right.as_ptr());
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let left = source(&catalog, "source").await;
                    let right = source(&catalog, "copy").await;
                    assert_eq!(
                        fs::metadata(left.pin.id.staging_path(&root.0))
                            .unwrap()
                            .ino(),
                        fs::metadata(right.pin.id.staging_path(&root.0))
                            .unwrap()
                            .ino()
                    );
                }
            }
            resumable_replace(&catalog, changed).await;
            assert_eq!(bytes(&catalog, changed).await, b"new");
            assert_eq!(bytes(&catalog, kept).await, b"original");
            delete(&catalog, changed).await;
            assert_eq!(bytes(&catalog, kept).await, b"original");
            copy(&catalog, source(&catalog, kept).await, changed)
                .await
                .unwrap();
            delete(&catalog, kept).await;
            assert_eq!(bytes(&catalog, changed).await, b"original");
            delete(&catalog, changed).await;
        }
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn unavailable_links_copy_the_selected_generation_and_reject_failed_snapshot_writes() {
    let (catalog, path) = setup().await;
    let payload = Bytes::from(vec![33; 3 * objects::WRITE_BATCH_BYTES + 1]);
    catalog
        .put_object(
            permissive_upload("bucket", "source", 1_000_000),
            body(vec![payload.clone()]),
        )
        .await
        .unwrap();
    for (step, skip) in [
        (Step::BodyWrite, 1),
        (Step::BodyFlush, 0),
        (Step::BodySync, 0),
    ] {
        catalog.inject(Step::CopyLink, 0, Fault::Io(ErrorKind::Unsupported));
        catalog.inject(step, skip, Fault::Io(ErrorKind::Other));
        assert!(
            catalog
                .resolve_derived_source(live("bucket", "source"), Default::default(), PERMISSIVE)
                .await
                .is_err()
        );
        assert_eq!(catalog.pending_faults(), 0);
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        assert_eq!(bytes(&catalog, "source").await, payload);
    }
    for skip in [1, 0] {
        catalog.inject(Step::CopyLink, skip, Fault::Io(ErrorKind::Unsupported));
        let selected = source(&catalog, "source").await;
        if skip == 0 {
            delete(&catalog, "source").await;
        }
        copy(&catalog, selected, "copy").await.unwrap();
        assert_eq!(bytes(&catalog, "copy").await, payload);
        assert_eq!(catalog.pending_faults(), 0);
    }
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn link_limitations_fall_back_and_io_failures_keep_their_cause() {
    let (catalog, path) = setup().await;
    let original = put(&catalog, "source", b"immutable").await;
    for skip in [0, 1] {
        for kind in [
            ErrorKind::CrossesDevices,
            ErrorKind::Unsupported,
            ErrorKind::TooManyLinks,
            ErrorKind::PermissionDenied,
        ] {
            catalog.inject(Step::CopyLink, skip, Fault::Io(kind));
            let selected = source(&catalog, "source").await;
            let input = selected.pin.id;
            let copied = copy(&catalog, selected, "copy").await.unwrap();
            assert_eq!(copied.hashes(), original.hashes());
            assert_eq!(bytes(&catalog, "copy").await, b"immutable");
            // Publishing this exact snapshot proves that no second draft was filled.
            if skip == 0 {
                assert!(input.blob_path(&path).is_file());
            }
            delete(&catalog, "copy").await;
        }
        for kind in [
            ErrorKind::Other,
            ErrorKind::NotFound,
            ErrorKind::StorageFull,
        ] {
            catalog.inject(Step::CopyLink, skip, Fault::Io(kind));
            let result = match catalog
                .resolve_derived_source(live("bucket", "source"), Default::default(), PERMISSIVE)
                .await
            {
                Ok(selected) => copy(&catalog, selected, "copy").await,
                Err(error) => Err(error),
            };
            let error = result.unwrap_err();
            assert!(
                matches!(&error, ObjectError::Io(cause) if cause.kind() == kind),
                "{error:?}"
            );
            assert!(std::error::Error::source(&error).is_some());
            assert_eq!(catalog.pending_faults(), 0);
            assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
            assert_eq!(blob_count(&path), 1);
        }
    }
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn pin_publication_recovery_retains_only_committed_names_and_bytes() {
    for private in [false, true] {
        for crash in Crash::ALL {
            for step in [
                Step::BeforePublish,
                Step::AfterPublish,
                Step::BeforeCommit,
                Step::AfterCommit,
            ] {
                let (catalog, path) = setup().await;
                put(&catalog, "source", b"immutable bytes").await;
                let snapshot = TestDir::new();
                catalog.inject(step, 0, crash.fault(snapshot.0.clone()));
                if private {
                    catalog.inject(Step::CopyLink, 0, Fault::Io(ErrorKind::Unsupported));
                }
                let selected = source(&catalog, "source").await;
                let pin = selected.publication_pin().await.unwrap().unwrap();
                let transfer = catalog
                    .begin_derived(vec![selected.whole()], u64::MAX)
                    .await
                    .unwrap();
                catalog
                    .publish_derived_prepared(
                        transfer.publication_body(Some(pin)),
                        destination("copy"),
                        |_, _| Ok(()),
                        |_, _| Ok(()),
                        None,
                    )
                    .await
                    .unwrap();
                assert_eq!(catalog.pending_faults(), 0);
                catalog.close().await.unwrap();
                let recovered = Catalog::open(&snapshot.0).await.unwrap();
                assert_eq!(bytes(&recovered, "source").await, b"immutable bytes");
                if step == Step::AfterCommit {
                    assert_eq!(bytes(&recovered, "copy").await, b"immutable bytes");
                    delete(&recovered, "copy").await;
                } else {
                    assert_eq!(
                        recovered
                            .get_object(live("bucket", "copy"), Default::default(), PERMISSIVE)
                            .await,
                        Err(ObjectError::ObjectNotFound)
                    );
                    if private {
                        assert_eq!(
                            recovered.get_derived(&transfer.id).await,
                            Err(ObjectError::DerivedTransferNotFound)
                        );
                    } else {
                        assert_eq!(recovered.get_derived(&transfer.id).await.unwrap().offset, 0);
                        recovered.advance_derived(&transfer.id, 2).await.unwrap();
                        recovered.cancel_derived(&transfer.id).await.unwrap();
                    }
                }
                assert_eq!(bytes(&recovered, "source").await, b"immutable bytes");
                delete(&recovered, "source").await;
                assert_eq!(blob_count(&snapshot.0), 0);
                assert_eq!(fs::read_dir(snapshot.0.join("staging")).unwrap().count(), 0);
                recovered.close().await.unwrap();
                fs::remove_dir_all(path).unwrap();
            }
        }
    }
}

#[tokio::test]
async fn interrupted_private_snapshot_leaves_only_the_selected_source() {
    for crash in Crash::ALL {
        let (catalog, path) = setup().await;
        let payload = Bytes::from(vec![11; 3 * objects::WRITE_BATCH_BYTES + 1]);
        catalog
            .put_object(
                permissive_upload("bucket", "source", 1_000_000),
                body(vec![payload.clone()]),
            )
            .await
            .unwrap();
        let snapshot = TestDir::new();
        catalog.inject(Step::CopyLink, 0, Fault::Io(ErrorKind::Unsupported));
        catalog.inject(Step::BodySync, 0, crash.fault(snapshot.0.clone()));
        copy(&catalog, source(&catalog, "source").await, "copy")
            .await
            .unwrap();
        assert_eq!(catalog.pending_faults(), 0);
        catalog.close().await.unwrap();
        let recovered = Catalog::open(&snapshot.0).await.unwrap();
        assert_eq!(bytes(&recovered, "source").await, payload);
        assert_eq!(
            recovered
                .get_object(live("bucket", "copy"), Default::default(), PERMISSIVE)
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        assert_eq!(fs::read_dir(snapshot.0.join("staging")).unwrap().count(), 0);
        recovered.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }
}

#[tokio::test]
async fn copy_selects_only_finalized_appendable_bytes() {
    let (catalog, path) = setup().await;
    let mut writer = catalog
        .create_appendable(
            "bucket",
            "source",
            ResumableOptions::new(BucketAccess::Permissive),
        )
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"one"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    let target = version("bucket", "source", writer.record().generation);
    for selection in [live("bucket", "source"), target] {
        assert_eq!(
            catalog
                .resolve_derived_source(selection, Default::default(), PERMISSIVE)
                .await
                .err(),
            Some(ObjectError::ObjectNotFound)
        );
    }
    writer
        .append(3, Bytes::from_static(b"two"), 1024)
        .await
        .unwrap();
    writer
        .finalize(None, BucketAccess::Permissive, 1024)
        .await
        .unwrap();
    drop(writer);
    copy(&catalog, source(&catalog, "source").await, "copy")
        .await
        .unwrap();
    assert_eq!(bytes(&catalog, "copy").await, b"onetwo");
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}
