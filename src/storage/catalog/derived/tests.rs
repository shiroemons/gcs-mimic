use super::cleanup::discard_transfer;
use super::*;
use crate::storage::test_fixtures::{
    PERMISSIVE, body, bucket, live, permissive_upload, read_all, version,
};
use crate::test_support::eventually_async;
use bytes::Bytes;
use md5::{Digest, Md5, digest::common::hazmat::SerializableState};
use std::io::Write;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

#[tokio::test]
async fn md5_availability_survives_restart_without_component_count() {
    let (catalog, path) = setup().await;
    let original = put(&catalog, "source", b"same bytes").await;
    let transfer = ready(&catalog, "source").await;
    let record = catalog
        .publish_derived_prepared(
            &transfer.id,
            destination("multipart"),
            |_, record| {
                record.md5_availability = crate::object::Md5Availability::Unavailable;
                Ok(())
            },
            |_, _| Ok(()),
            None,
        )
        .await
        .unwrap();
    assert!(!record.has_md5());
    assert_eq!(record.component_count, None);
    assert_eq!(record.md5, original.md5);
    assert_eq!(record.crc32c, original.crc32c);
    catalog.close().await.unwrap();
    drop(catalog);
    let catalog = Catalog::open(&path).await.unwrap();
    let reopened = catalog
        .get_object(live("bucket", "multipart"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(reopened, record);
    let selected = source(&catalog, "multipart").await;
    let (activity, _updates) = tokio::sync::watch::channel(Default::default());
    let copy = crate::server::copy::execute(
        &catalog,
        crate::server::copy::CopyJob {
            source: selected,
            source_access: BucketAccess::Permissive,
            destination: destination("copy"),
            source_location: None,
            replay: None,
        },
        crate::server::copy::CopyBudget {
            config: &crate::config::Config::default(),
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(10),
            activity: &activity,
        },
        None,
    )
    .await
    .unwrap();
    assert!(copy.has_md5());
    assert_eq!(copy.md5, original.md5);
    assert_eq!(copy.crc32c, original.crc32c);
    let selected = source(&catalog, "multipart").await;
    let start = super::super::rewrite::RewriteStart {
        binding: super::super::rewrite::RewriteBinding {
            source_bucket: "bucket".into(),
            source_object: "multipart".into(),
            source_generation: record.generation,
            destination_bucket: "bucket".into(),
            destination_object: "rewrite".into(),
            parameters: Default::default(),
            metadata: None,
        },
        destination: DerivedDestination {
            expected: ExpectedHashes {
                md5: Some(original.md5),
                crc32c: Some(original.crc32c),
            },
            ..destination("rewrite")
        },
        source_conditions: Default::default(),
        source_metageneration: record.metageneration,
        source_access: BucketAccess::Permissive,
        source_key_sha256: None,
        budget: 1_048_576,
    };
    let (token, mut owner) = catalog
        .begin_rewrite_session(vec![selected.whole()], u64::MAX, start)
        .await
        .unwrap();
    owner.disarm();
    let result = catalog
        .continue_rewrite(
            &token.to_string(),
            BucketAccess::Permissive,
            BucketAccess::Permissive,
            |record| Ok(serde_json::json!({"generation":record.generation})),
        )
        .await
        .unwrap();
    assert!(result.resource.is_some());
    let rewritten = catalog
        .get_object(live("bucket", "rewrite"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert!(rewritten.has_md5());
    assert_eq!(rewritten.md5, original.md5);
    assert_eq!(rewritten.crc32c, original.crc32c);
    assert_eq!(
        catalog
            .get_object(live("bucket", "multipart"), Default::default(), PERMISSIVE)
            .await
            .unwrap(),
        record
    );
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

async fn setup() -> (Catalog, PathBuf) {
    let path = std::env::temp_dir().join(format!("gcs-mimic-derived-{}", random_id().unwrap()));
    let catalog = Catalog::open(&path).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    (catalog, path)
}

#[tokio::test]
async fn a_transfer_waiting_at_close_fails_and_its_draft_does_not_survive_reopening() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let selected = source(&catalog, "source").await;
    let permits = catalog.hold_admission();
    let worker = catalog.clone();
    let waiting =
        tokio::spawn(async move { worker.begin_derived(vec![selected.whole()], 5).await });
    crate::test_support::eventually_async("the transfer to wait for admission", || async {
        catalog.admission_waiters() == 1
    })
    .await;
    catalog.close().await.unwrap();
    assert!(matches!(
        waiting.await.unwrap(),
        Err(ObjectError::Unavailable(_))
    ));
    drop(permits);
    let catalog = Catalog::open(&path).await.unwrap();
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    assert_eq!(bytes(&catalog, "source").await, b"bytes");
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn a_request_dropped_while_its_transfer_waits_leaves_no_draft() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let selected = source(&catalog, "source").await;
    let permits = catalog.hold_admission();
    let worker = catalog.clone();
    let request = tokio::spawn(async move {
        worker
            .begin_derived_ephemeral(vec![selected.whole()], 5)
            .await
    });
    crate::test_support::eventually_async("the transfer to wait for admission", || async {
        catalog.admission_waiters() == 1
    })
    .await;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    drop(permits);
    crate::test_support::eventually_async("the abandoned transfer to be reclaimed", || async {
        fs::read_dir(path.join("staging")).unwrap().count() == 0 && catalog.session_writers_idle()
    })
    .await;
    let remaining: i64 = catalog
        .run(|connection| {
            sql::scalar(connection, "SELECT count(*) FROM derived_transfers", [])
                .map_err(crate::storage::failure::unavailable)
        })
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn failed_draft_sync_and_begin_commit_release_unreferenced_files() {
    for step in [Step::DerivedDraftSync, Step::DerivedBeginCommit] {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        catalog.inject(step, 0, fault::Fault::Io(std::io::ErrorKind::Other));
        let selected = source(&catalog, "source").await;
        assert!(matches!(
            catalog.begin_derived(vec![selected.whole()], 5).await,
            Err(ObjectError::Io(_))
        ));
        assert_eq!(catalog.pending_faults(), 0);
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        catalog
            .run::<_, CatalogError>(|connection| {
                assert_eq!(
                    sql::scalar::<i64>(connection, "SELECT count(*) FROM derived_transfers", [])?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(bytes(&catalog, "source").await, b"bytes");
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }
}

#[tokio::test]
async fn begin_snapshots_recover_unreferenced_files_or_one_durable_transfer() {
    use super::super::fault::Crash;
    for &crash in Crash::ALL {
        for step in [
            Step::DerivedDraftSync,
            Step::DerivedBeginCommit,
            Step::AfterCommit,
        ] {
            let (catalog, path) = setup().await;
            put(&catalog, "source", b"bytes").await;
            let snapshot = crate::test_support::TestDir::new();
            catalog.inject(step, 0, crash.fault(snapshot.0.clone()));
            let selected = source(&catalog, "source").await;
            let transfer = catalog
                .begin_derived(vec![selected.whole()], 5)
                .await
                .unwrap();
            if step == Step::AfterCommit {
                // Keep the model armed across input pinning, draft creation and the commit.
                fault::hit(&path, step).unwrap();
            }
            assert_eq!(catalog.pending_faults(), 0);
            catalog.cancel_derived(&transfer.id).await.unwrap();
            catalog.close().await.unwrap();
            let recovered = Catalog::open(&snapshot.0).await.unwrap();
            assert_eq!(bytes(&recovered, "source").await, b"bytes");
            if step == Step::AfterCommit {
                assert_eq!(
                    recovered
                        .advance_derived(&transfer.id, 5)
                        .await
                        .unwrap()
                        .offset,
                    5
                );
                recovered
                    .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
                    .await
                    .unwrap();
                assert_eq!(bytes(&recovered, "copy").await, b"bytes");
            } else {
                assert_eq!(
                    recovered.get_derived(&transfer.id).await,
                    Err(ObjectError::DerivedTransferNotFound)
                );
            }
            assert_eq!(fs::read_dir(snapshot.0.join("staging")).unwrap().count(), 0);
            recovered.close().await.unwrap();
            fs::remove_dir_all(path).unwrap();
        }
    }
}

#[tokio::test]
async fn cancel_reports_success_after_commit_when_unlink_fails() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let transfer = ready(&catalog, "source").await;
    catalog.inject(
        Step::DerivedCleanupCommit,
        0,
        fault::Fault::Io(std::io::ErrorKind::Other),
    );
    assert!(matches!(
        catalog.cancel_derived(&transfer.id).await,
        Err(ObjectError::Io(_))
    ));
    assert_eq!(catalog.get_derived(&transfer.id).await.unwrap(), transfer);
    catalog.inject(
        Step::DerivedCleanup,
        0,
        fault::Fault::Io(std::io::ErrorKind::PermissionDenied),
    );
    catalog.cancel_derived(&transfer.id).await.unwrap();
    assert_eq!(catalog.pending_faults(), 0);
    assert_eq!(
        catalog.get_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    assert_eq!(
        catalog.cancel_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    assert_ne!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    let recovered = Catalog::open(&path).await.unwrap();
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    assert_eq!(bytes(&recovered, "source").await, b"bytes");
    assert_eq!(
        recovered.get_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    recovered.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn cancel_snapshots_keep_the_transfer_or_reclaim_all_unreferenced_data() {
    use super::super::fault::Crash;
    for &crash in Crash::ALL {
        for step in [
            Step::DerivedCleanupCommit,
            Step::DerivedCleanup,
            Step::DerivedCleaned,
        ] {
            let (catalog, path) = setup().await;
            put(&catalog, "source", b"bytes").await;
            let transfer = ready(&catalog, "source").await;
            let snapshot = crate::test_support::TestDir::new();
            catalog.inject(step, 0, crash.fault(snapshot.0.clone()));
            catalog.cancel_derived(&transfer.id).await.unwrap();
            assert_eq!(catalog.pending_faults(), 0);
            catalog.close().await.unwrap();
            let recovered = Catalog::open(&snapshot.0).await.unwrap();
            assert_eq!(bytes(&recovered, "source").await, b"bytes");
            if step == Step::DerivedCleanupCommit {
                assert_eq!(recovered.get_derived(&transfer.id).await.unwrap(), transfer);
                recovered
                    .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
                    .await
                    .unwrap();
                assert_eq!(bytes(&recovered, "copy").await, b"bytes");
            } else {
                assert_eq!(
                    recovered.get_derived(&transfer.id).await,
                    Err(ObjectError::DerivedTransferNotFound)
                );
            }
            assert_eq!(fs::read_dir(snapshot.0.join("staging")).unwrap().count(), 0);
            recovered.close().await.unwrap();
            fs::remove_dir_all(path).unwrap();
        }
    }
}

#[tokio::test]
async fn publication_crashes_preserve_a_resumable_transfer_or_one_published_generation() {
    use super::super::fault::Crash;
    let snapshots = crate::test_support::TestDir::new();
    for (kind, crash) in Crash::ALL.iter().enumerate() {
        for (index, step) in [
            Step::BeforePublish,
            Step::AfterPublish,
            Step::BeforeCommit,
            Step::AfterCommit,
        ]
        .into_iter()
        .enumerate()
        {
            let (catalog, path) = setup().await;
            put(&catalog, "source", b"bytes").await;
            let transfer = ready(&catalog, "source").await;
            let snapshot = snapshots.0.join(format!("{kind}-{index}"));
            catalog.inject(step, 0, crash.fault(snapshot.clone()));
            catalog
                .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
                .await
                .unwrap();
            assert_eq!(catalog.pending_faults(), 0);
            catalog.close().await.unwrap();
            let recovered = Catalog::open(&snapshot).await.unwrap();
            if step != Step::AfterCommit {
                assert_eq!(
                    recovered
                        .get_object(live("bucket", "copy"), Default::default(), PERMISSIVE)
                        .await,
                    Err(ObjectError::ObjectNotFound)
                );
                recovered.advance_derived(&transfer.id, 0).await.unwrap();
                recovered
                    .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
                    .await
                    .unwrap();
            }
            assert_eq!(bytes(&recovered, "copy").await, b"bytes");
            let versions: i64 = recovered
                .run(|connection| {
                    sql::scalar(
                        connection,
                        "SELECT count(*) FROM objects WHERE name='copy'",
                        [],
                    )
                    .map_err(object_unavailable)
                })
                .await
                .unwrap();
            assert_eq!(versions, 1);
            recovered.close().await.unwrap();
            fs::remove_dir_all(path).unwrap();
        }
    }
}

#[tokio::test]
async fn transfer_write_flush_and_sync_failures_preserve_the_checkpoint_for_retry() {
    use super::super::fault::Fault;
    for step in [Step::BodyWrite, Step::BodyFlush, Step::BodySync] {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        let transfer = catalog
            .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
            .await
            .unwrap();
        catalog.inject(step, 0, Fault::Io(std::io::ErrorKind::Other));
        assert!(
            catalog.advance_derived(&transfer.id, 5).await.is_err(),
            "{step:?}"
        );
        assert_eq!(catalog.get_derived(&transfer.id).await.unwrap().offset, 0);
        assert_eq!(
            catalog
                .advance_derived(&transfer.id, 5)
                .await
                .unwrap()
                .offset,
            5
        );
        catalog.cancel_derived(&transfer.id).await.unwrap();
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }
}

async fn put(catalog: &Catalog, name: &str, bytes: &'static [u8]) -> ObjectRecord {
    let body = body(vec![Bytes::from_static(bytes)]);
    catalog
        .put_object(permissive_upload("bucket", name, 1_000_000), body)
        .await
        .unwrap()
}

async fn source(catalog: &Catalog, name: &str) -> DerivedSource {
    catalog
        .resolve_derived_source(live("bucket", name), Default::default(), PERMISSIVE)
        .await
        .unwrap()
}

fn destination(name: &str) -> DerivedDestination {
    DerivedDestination {
        bucket: "bucket".into(),
        name: name.into(),
        content_type: "text/plain".into(),
        ..DerivedDestination::new(BucketAccess::Permissive)
    }
}

async fn ready(catalog: &Catalog, name: &str) -> DerivedTransfer {
    let transfer = catalog
        .begin_derived(vec![source(catalog, name).await.whole()], u64::MAX)
        .await
        .unwrap();
    catalog
        .advance_derived(&transfer.id, u64::MAX)
        .await
        .unwrap()
}

async fn bytes(catalog: &Catalog, name: &str) -> Vec<u8> {
    let (_, mut reader) = catalog
        .open_object(live("bucket", name), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    read_all(&mut reader).await
}

#[tokio::test]
async fn pinned_ranges_resume_after_source_deletion_restart_and_uncommitted_tail() {
    let (catalog, path) = setup().await;
    let first = put(&catalog, "first", b"abcdef").await;
    put(&catalog, "second", b"12345").await;
    let transfer = catalog
        .begin_derived(
            vec![
                DerivedSegment {
                    source: source(&catalog, "first").await,
                    offset: 1,
                    length: 4,
                },
                DerivedSegment {
                    source: source(&catalog, "second").await,
                    offset: 2,
                    length: 3,
                },
            ],
            7,
        )
        .await
        .unwrap();
    assert_eq!(
        catalog
            .advance_derived(&transfer.id, 3)
            .await
            .unwrap()
            .offset,
        3
    );
    catalog
        .delete_object(
            version("bucket", "first", first.generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    put(&catalog, "second", b"changed").await;
    let output = stage_path(&path, &transfer.id);
    fs::OpenOptions::new()
        .append(true)
        .open(&output)
        .unwrap()
        .write_all(b"uncommitted")
        .unwrap();
    catalog.close().await.unwrap();
    drop(catalog);
    let catalog = Catalog::open(&path).await.unwrap();
    assert_eq!(fs::metadata(&output).unwrap().len(), 3);
    assert_eq!(
        catalog
            .advance_derived(&transfer.id, 2)
            .await
            .unwrap()
            .offset,
        5
    );
    catalog
        .advance_derived(&transfer.id, u64::MAX)
        .await
        .unwrap();
    let record = catalog
        .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
        .await
        .unwrap();
    assert_eq!(bytes(&catalog, "copy").await, b"bcde345");
    assert_eq!(record.md5, <[u8; 16]>::from(Md5::digest(b"bcde345")));
    assert_eq!(record.crc32c, crc32c::crc32c(b"bcde345"));
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn recovery_restores_renamed_uncommitted_output_and_empty_sources() {
    let (catalog, path) = setup().await;
    put(&catalog, "empty", b"").await;
    let transfer = ready(&catalog, "empty").await;
    objects::publish_blob(
        &stage_path(&path, &transfer.id),
        &transfer.id.blob_path(&path),
    )
    .unwrap();
    catalog.close().await.unwrap();
    drop(catalog);
    let catalog = Catalog::open(&path).await.unwrap();
    let object = catalog
        .publish_derived(&transfer.id, destination("empty-copy"), |_, _| Ok(()))
        .await
        .unwrap();
    assert_eq!(object.size, 0);
    assert_eq!(object.md5, <[u8; 16]>::from(Md5::digest(b"")));
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn destination_race_has_one_winner_and_failed_publication_cleans_staging() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"contents").await;
    let left = ready(&catalog, "source").await;
    let right = ready(&catalog, "source").await;
    let mut target = destination("target");
    target.conditions.if_generation_match = Some(0);
    let (left, right) = tokio::join!(
        catalog.publish_derived(&left.id, target.clone(), |_, _| Ok(())),
        catalog.publish_derived(&right.id, target, |_, _| Ok(())),
    );
    assert!(matches!(
        (&left, &right),
        (Ok(_), Err(ObjectError::PreconditionFailed))
            | (Err(ObjectError::PreconditionFailed), Ok(_))
    ));
    assert_eq!(bytes(&catalog, "target").await, b"contents");
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    let transfer = ready(&catalog, "source").await;
    let mut target = destination("target");
    target.expected.crc32c = Some(0);
    assert_eq!(
        catalog
            .publish_derived(&transfer.id, target, |_, _| Ok(()))
            .await,
        Err(ObjectError::ChecksumMismatch)
    );
    assert_eq!(bytes(&catalog, "target").await, b"contents");
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn callback_source_deletion_rolls_back_with_destination_and_checks_holds() {
    let (catalog, path) = setup().await;
    let original = put(&catalog, "source", b"source").await;
    let target = put(&catalog, "target", b"original").await;
    let connection = Connection::open(path.join("catalog.sqlite3")).unwrap();
    let last_generation: i64 = connection
        .query_row("SELECT last_generation FROM object_generation", [], |row| {
            row.get(0)
        })
        .unwrap();
    let transfer = ready(&catalog, "source").await;
    let generation = original.generation;
    let result = catalog
        .publish_derived(&transfer.id, destination("target"), move |connection, _| {
            Catalog::delete_derived_source_in_transaction(
                connection,
                "bucket",
                "source",
                generation,
                Default::default(),
                PERMISSIVE,
                std::time::SystemTime::now(),
            )?;
            Err(ObjectError::PreconditionFailed)
        })
        .await;
    assert_eq!(result, Err(ObjectError::PreconditionFailed));
    assert_eq!(bytes(&catalog, "target").await, b"original");
    assert_eq!(bytes(&catalog, "source").await, b"source");
    let (last_after_failure, pending): (i64, i64) = connection
        .query_row(
            "SELECT last_generation, (SELECT count(*) FROM pending_blob_deletions) FROM object_generation",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((last_after_failure, pending), (last_generation, 0));
    let current = catalog
        .get_object(live("bucket", "target"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(current, target);
    drop(connection);
    catalog
        .update_object_metadata(
            live("bucket", "source"),
            Default::default(),
            ObjectMetadataUpdate {
                event_based_hold: Some(true),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let transfer = ready(&catalog, "source").await;
    let result = catalog
        .publish_derived(&transfer.id, destination("target"), move |connection, _| {
            Catalog::delete_derived_source_in_transaction(
                connection,
                "bucket",
                "source",
                generation,
                Default::default(),
                PERMISSIVE,
                std::time::SystemTime::now(),
            )
        })
        .await;
    assert!(matches!(result, Err(ObjectError::ActiveHold(_))));
    assert_eq!(bytes(&catalog, "target").await, b"original");
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn all_source_conditions_are_checked_before_pinning_and_ranges_are_validated() {
    let (catalog, path) = setup().await;
    let original = put(&catalog, "source", b"source").await;
    for conditions in [
        ObjectConditions {
            if_generation_match: Some(original.generation + 1),
            ..Default::default()
        },
        ObjectConditions {
            if_generation_not_match: Some(original.generation),
            ..Default::default()
        },
        ObjectConditions {
            if_metageneration_match: Some(2),
            ..Default::default()
        },
        ObjectConditions {
            if_metageneration_not_match: Some(1),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            catalog
                .resolve_derived_source(live("bucket", "source"), conditions, PERMISSIVE)
                .await,
            Err(ObjectError::PreconditionFailed)
        ));
    }
    let denied = BucketAccess::Policy {
        identity: None,
        permission: crate::auth::Permission::ObjectsGet,
        project: None,
    };
    assert!(matches!(
        catalog
            .resolve_derived_source(live("bucket", "source"), Default::default(), &denied)
            .await,
        Err(ObjectError::Denied(refused)) if refused == denied.denied("bucket")
    ));
    assert!(
        catalog
            .begin_derived(
                vec![DerivedSegment {
                    source: source(&catalog, "source").await,
                    offset: 5,
                    length: 2
                }],
                100
            )
            .await
            .is_err()
    );
    assert!(matches!(
        catalog
            .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
            .await,
        Err(ObjectError::TooLarge)
    ));
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn validation_sees_same_name_source_before_retirement_and_completion_sees_destination() {
    let (catalog, path) = setup().await;
    let original = put(&catalog, "same", b"same bytes").await;
    let transfer = ready(&catalog, "same").await;
    let generation = original.generation;
    let mut target = destination("same");
    target.component_count = Some(2);
    let record = catalog
        .publish_derived_with_validation(
            &transfer.id,
            target,
            move |connection, destination| {
                let source = Catalog::check_derived_source_in_transaction(
                    connection,
                    "bucket",
                    "same",
                    Some(generation),
                    Default::default(),
                    PERMISSIVE,
                )?;
                assert_eq!(source.generation, generation);
                assert_ne!(source.generation, destination.generation);
                Ok(())
            },
            move |connection, destination| {
                let live = Catalog::check_derived_source_in_transaction(
                    connection,
                    "bucket",
                    "same",
                    None,
                    Default::default(),
                    PERMISSIVE,
                )?;
                assert_eq!(live, *destination);
                Ok(())
            },
        )
        .await
        .unwrap();
    assert_eq!(record.component_count, Some(2));
    let value =
        crate::json::object_resource(&record, "http://localhost", crate::json::Projection::NoAcl)
            .unwrap();
    assert_eq!(value["componentCount"], 2);
    assert!(value.get("md5Hash").is_none());
    assert!(
        !crate::json::object_media_headers(&record).unwrap()["x-goog-hash"]
            .to_str()
            .unwrap()
            .contains("md5=")
    );
    catalog.close().await.unwrap();
    drop(catalog);
    let catalog = Catalog::open(&path).await.unwrap();
    assert_eq!(
        catalog
            .get_object(live("bucket", "same"), Default::default(), PERMISSIVE)
            .await
            .unwrap(),
        record
    );
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn transfers_past_the_former_quota_survive_reopening_and_recovery_pages_through_them() {
    // Past the 128 of each kind that once failed requests, and more than one recovery page.
    const TRANSFERS: usize = 300;
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let mut persistent = Vec::new();
    for kind in [TransferKind::Ephemeral, TransferKind::Continuable] {
        for _ in 0..TRANSFERS {
            let transfer = catalog
                .begin_derived_with_kind(vec![source(&catalog, "source").await.whole()], 5, kind)
                .await
                .unwrap();
            if kind == TransferKind::Continuable {
                persistent.push(transfer);
            }
        }
    }
    // Each transfer owns its draft and one pinned source.
    assert_eq!(
        fs::read_dir(path.join("staging")).unwrap().count(),
        4 * TRANSFERS
    );
    catalog.close().await.unwrap();
    drop(catalog);
    let catalog = Catalog::open(&path).await.unwrap();
    // Only continuable transfers outlive the process that started them.
    assert_eq!(
        fs::read_dir(path.join("staging")).unwrap().count(),
        2 * TRANSFERS
    );
    for transfer in &persistent {
        assert_eq!(&catalog.get_derived(&transfer.id).await.unwrap(), transfer);
    }
    let progress = catalog
        .advance_derived(&persistent[299].id, 5)
        .await
        .unwrap();
    assert_eq!(progress.offset, 5);
    let ephemeral = catalog
        .begin_derived_ephemeral(vec![source(&catalog, "source").await.whole()], 5)
        .await
        .unwrap();
    catalog.cancel_derived(&ephemeral.id).await.unwrap();
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn restart_reclaims_ephemeral_transfers_and_preserves_continuable_transfers() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let ephemeral = catalog
        .begin_derived_ephemeral(vec![source(&catalog, "source").await.whole()], 5)
        .await
        .unwrap();
    catalog.advance_derived(&ephemeral.id, 2).await.unwrap();
    let persistent = catalog
        .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
        .await
        .unwrap();
    catalog.close().await.unwrap();
    drop(catalog);
    let catalog = Catalog::open(&path).await.unwrap();
    assert_eq!(
        catalog.get_derived(&ephemeral.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    assert_eq!(
        catalog.get_derived(&persistent.id).await.unwrap(),
        persistent
    );
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 2);
    catalog.cancel_derived(&persistent.id).await.unwrap();
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn batch_source_resolution_checks_every_source_and_cleans_partial_pins() {
    let (catalog, path) = setup().await;
    let original = put(&catalog, "source", b"bytes").await;
    let spec = DerivedSourceSpec {
        target: version("bucket", "source", original.generation),
        conditions: Default::default(),
        access: BucketAccess::Permissive,
    };
    let mut missing = spec.clone();
    missing.target.name = "missing".into();
    assert!(matches!(
        catalog
            .resolve_derived_sources(vec![spec.clone(), missing])
            .await,
        Err(ObjectError::ObjectNotFound)
    ));
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    let sources = catalog
        .resolve_derived_sources(vec![spec; 32])
        .await
        .unwrap();
    assert_eq!(sources.len(), 32);
    assert!(sources.iter().all(|source| source.object == original));
    drop(sources);
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn unknown_transfer_ids_cannot_remove_another_transfers_source() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let source = source(&catalog, "source").await;
    let snapshot_id = source.id;
    let transfer = catalog
        .begin_derived(vec![source.whole()], 5)
        .await
        .unwrap();
    assert_eq!(
        catalog.cancel_derived(&snapshot_id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    assert_eq!(
        catalog
            .publish_derived(&snapshot_id, destination("copy"), |_, _| Ok(()))
            .await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    catalog
        .advance_derived(&transfer.id, u64::MAX)
        .await
        .unwrap();
    catalog
        .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
        .await
        .unwrap();
    assert_eq!(bytes(&catalog, "copy").await, b"bytes");
    catalog.close().await.unwrap();
    drop(catalog);
    Catalog::open(&path).await.unwrap().close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn incomplete_and_transient_publication_failures_preserve_resumable_progress() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let transfer = catalog
        .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
        .await
        .unwrap();
    catalog.advance_derived(&transfer.id, 2).await.unwrap();
    assert_eq!(
        catalog
            .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
            .await,
        Err(ObjectError::DerivedTransferIncomplete)
    );
    assert_eq!(catalog.get_derived(&transfer.id).await.unwrap().offset, 2);
    catalog
        .advance_derived(&transfer.id, u64::MAX)
        .await
        .unwrap();
    assert!(matches!(
        catalog
            .publish_derived(&transfer.id, destination("copy"), |_, _| Err(
                object_unavailable("retryable callback failure")
            ))
            .await,
        Err(ObjectError::Unavailable(_))
    ));
    assert_eq!(catalog.get_derived(&transfer.id).await.unwrap().offset, 5);
    catalog.advance_derived(&transfer.id, 0).await.unwrap();
    catalog
        .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
        .await
        .unwrap();
    assert_eq!(bytes(&catalog, "copy").await, b"bytes");
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn request_binding_commits_or_rolls_back_with_transfer_ownership() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let result = catalog
        .begin_derived_with_binding(
            vec![source(&catalog, "source").await.whole()],
            5,
            TransferKind::Continuable,
            |connection, transfer| {
                assert_eq!(inputs(connection, &transfer.id)?.len(), 1);
                connection.execute("UPDATE startup_seed SET complete = 1", [])?;
                Err(ObjectError::PreconditionFailed)
            },
        )
        .await;
    assert_eq!(result, Err(ObjectError::PreconditionFailed));
    assert!(
        !catalog
            .admin(super::super::admin::Origin::Seed)
            .seed_complete()
            .await
            .unwrap()
    );
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    let transfer = catalog
        .begin_derived_with_binding(
            vec![source(&catalog, "source").await.whole()],
            5,
            TransferKind::Continuable,
            |connection, transfer| {
                assert_eq!(inputs(connection, &transfer.id)?.len(), 1);
                connection.execute("UPDATE startup_seed SET complete = 1", [])?;
                Ok(())
            },
        )
        .await
        .unwrap();
    assert!(
        catalog
            .admin(super::super::admin::Origin::Seed)
            .seed_complete()
            .await
            .unwrap()
    );
    catalog.cancel_derived(&transfer.id).await.unwrap();
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn abandoned_handler_guard_waits_for_writer_then_reclaims_transfer() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let transfer = catalog
        .begin_derived_ephemeral(vec![source(&catalog, "source").await.whole()], 5)
        .await
        .unwrap();
    let writer = catalog.derived_guard(&transfer.id).unwrap();
    let request = catalog.guard_derived_request(&transfer.id);
    drop(request);
    tokio::task::yield_now().await;
    assert!(catalog.get_derived(&transfer.id).await.is_ok());
    drop(writer);
    eventually_async(
        "the derived transfer and its staging to be removed",
        || async {
            catalog.get_derived(&transfer.id).await == Err(ObjectError::DerivedTransferNotFound)
                && fs::read_dir(path.join("staging")).unwrap().count() == 0
        },
    )
    .await;
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

/// A copy has no request deadline by default, so its transfer lives as long as it advances, and
/// the lifetime reclaims only one nobody is working on.
#[tokio::test]
async fn advancing_renews_an_ephemeral_lifetime_and_an_idle_transfer_still_expires() {
    use crate::clock::ManualClock;
    use std::{
        sync::Arc,
        time::{Duration, UNIX_EPOCH},
    };
    let (catalog, path) = setup().await;
    catalog.close().await.unwrap();
    drop(catalog);
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH));
    let catalog = Catalog::open_with_clock(&path, clock.clone())
        .await
        .unwrap();
    put(&catalog, "source", b"bytes").await;
    let transfer = catalog
        .begin_derived_ephemeral(vec![source(&catalog, "source").await.whole()], 5)
        .await
        .unwrap();
    let lifetime = Duration::from_millis(EPHEMERAL_LIFETIME_MILLIS.unsigned_abs());
    let within_lifetime = lifetime - Duration::from_secs(1);
    for offset in 1..=3 {
        clock.advance(within_lifetime);
        assert_eq!(
            catalog
                .advance_derived(&transfer.id, 1)
                .await
                .unwrap()
                .offset,
            offset
        );
    }
    clock.advance(lifetime);
    assert_eq!(
        catalog.advance_derived(&transfer.id, 1).await,
        Err(ObjectError::DerivedTransferExpired)
    );
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn injected_clock_expiry_is_enforced_and_maintenance_reclaims_inputs() {
    use crate::clock::ManualClock;
    use std::sync::Arc;
    let (catalog, path) = setup().await;
    catalog.close().await.unwrap();
    drop(catalog);
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH));
    let catalog = Catalog::open_with_clock(&path, clock.clone())
        .await
        .unwrap();
    put(&catalog, "source", b"bytes").await;
    let transfer = catalog
        .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
        .await
        .unwrap();
    assert_eq!(
        catalog.maintain_derived_transfers().await.unwrap(),
        Some(UNIX_EPOCH + std::time::Duration::from_secs(604800))
    );
    let session = catalog
        .create_resumable_session(
            "bucket",
            "upload",
            crate::storage::ResumableOptions::new(BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    let writer = catalog.derived_guard(&transfer.id).unwrap();
    clock.set(UNIX_EPOCH + std::time::Duration::from_secs(604800));
    assert_eq!(
        catalog.get_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferExpired)
    );
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert!(matches!(
        catalog
            .resumable_session(&session.id.to_string(), "bucket")
            .await,
        Err(crate::storage::ResumableError::NotFound)
    ));
    drop(writer);
    assert_eq!(catalog.maintain_derived_transfers().await.unwrap(), None);
    assert_eq!(
        catalog.get_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn recovery_reclaims_every_deferred_deletion_past_one_page() {
    let (catalog, path) = setup().await;
    catalog.close().await.unwrap();
    let database = path.join("catalog.sqlite3");
    let count = |database: &Path| -> i64 {
        rusqlite::Connection::open(database)
            .unwrap()
            .query_row("SELECT count(*) FROM derived_deletions", [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch("WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM ids WHERE n < 300) INSERT INTO derived_deletions SELECT printf('%032x', n), printf('%032x', n) FROM ids")
        .unwrap();
    assert_eq!(count(&database), 300);
    let catalog = Catalog::open(&path).await.unwrap();
    catalog.close().await.unwrap();
    assert_eq!(count(&database), 0);
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn expiry_drains_more_than_one_page_in_one_sweep() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"").await;
    for index in 0..129 {
        let kind = if index % 2 == 0 {
            TransferKind::Continuable
        } else {
            TransferKind::Ephemeral
        };
        catalog
            .begin_derived_with_binding(
                vec![source(&catalog, "source").await.whole()],
                0,
                kind,
                |_, _| Ok(()),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        catalog
            .maintain_derived_transfers_at(i64::MAX)
            .await
            .unwrap(),
        None
    );
    let remaining: i64 = catalog
        .run(|connection| {
            sql::scalar(connection, "SELECT count(*) FROM derived_transfers", [])
                .map_err(object_unavailable)
        })
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn corrupt_transfer_is_discarded_without_blocking_catalog_recovery() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let source = source(&catalog, "source").await;
    let snapshot = source.id.staging_path(&path);
    let transfer = catalog
        .begin_derived(vec![source.whole()], 5)
        .await
        .unwrap();
    fs::remove_file(snapshot).unwrap();
    catalog.close().await.unwrap();
    drop(catalog);
    let catalog = Catalog::open(&path).await.unwrap();
    assert_eq!(
        catalog.get_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    assert_eq!(bytes(&catalog, "source").await, b"bytes");
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn overflowing_source_lengths_are_discarded_during_recovery() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let transfer = catalog
        .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
        .await
        .unwrap();
    let id = transfer.id;
    catalog
        .run::<_, ObjectError>(move |connection| {
            sql::exec(
                connection,
                "UPDATE derived_inputs SET length=?2, size=?2 WHERE transfer_id=?1",
                params![id, i64::MAX],
            )?;
            for position in [1, 2] {
                sql::exec(
                    connection,
                    "INSERT INTO derived_inputs VALUES (?1,?2,?3,0,?4,?4)",
                    params![id, position, BlobId::generate().unwrap(), i64::MAX],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
    drop(catalog);
    let catalog = Catalog::open(&path).await.unwrap();
    assert_eq!(
        catalog.get_derived(&id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    assert_eq!(bytes(&catalog, "source").await, b"bytes");
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn cleanup_failure_preserves_original_publication_error() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    put(&catalog, "copy", b"old").await;
    let source = source(&catalog, "source").await;
    let snapshot = source.id.staging_path(&path);
    let transfer = catalog
        .begin_derived(vec![source.whole()], 5)
        .await
        .unwrap();
    catalog
        .advance_derived(&transfer.id, u64::MAX)
        .await
        .unwrap();
    fs::remove_file(&snapshot).unwrap();
    fs::create_dir(&snapshot).unwrap();
    let mut target = destination("copy");
    target.conditions.if_generation_match = Some(0);
    target.expected.crc32c = Some(0);
    assert_eq!(
        catalog
            .publish_derived(&transfer.id, target, |_, _| Ok(()))
            .await,
        Err(ObjectError::PreconditionFailed)
    );
    assert_eq!(bytes(&catalog, "copy").await, b"old");
    fs::remove_dir(&snapshot).unwrap();
    assert_eq!(
        catalog.cancel_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn recovery_rejects_same_length_corrupted_acknowledged_prefix() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let transfer = catalog
        .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
        .await
        .unwrap();
    catalog.advance_derived(&transfer.id, 3).await.unwrap();
    fs::write(stage_path(&path, &transfer.id), b"bad").unwrap();
    catalog.close().await.unwrap();
    drop(catalog);
    let catalog = Catalog::open(&path).await.unwrap();
    assert_eq!(
        catalog.get_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    assert_eq!(bytes(&catalog, "source").await, b"bytes");
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn committed_callback_deletion_reclaims_source_blob_without_restart() {
    let (catalog, path) = setup().await;
    let original = put(&catalog, "source", b"bytes").await;
    let transfer = ready(&catalog, "source").await;
    catalog
        .publish_derived(&transfer.id, destination("copy"), move |connection, _| {
            Catalog::delete_derived_source_in_transaction(
                connection,
                "bucket",
                "source",
                original.generation,
                Default::default(),
                PERMISSIVE,
                std::time::SystemTime::now(),
            )
        })
        .await
        .unwrap();
    assert_eq!(
        catalog
            .get_object(live("bucket", "source"), Default::default(), PERMISSIVE)
            .await,
        Err(ObjectError::ObjectNotFound)
    );
    assert_eq!(bytes(&catalog, "copy").await, b"bytes");
    let blobs: usize = fs::read_dir(path.join("blobs"))
        .unwrap()
        .map(|shard| fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum();
    assert_eq!(blobs, 1);
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    assert_eq!(catalog.maintain_derived_transfers().await.unwrap(), None);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn callback_reclamation_failure_preserves_success_and_maintenance_retries() {
    let (catalog, path) = setup().await;
    let original = put(&catalog, "source", b"bytes").await;
    let id = catalog
        .run(|connection| {
            Ok::<_, ObjectError>(
                objects::lookup(connection, "bucket", "source")?
                    .unwrap()
                    .blob,
            )
        })
        .await
        .unwrap();
    let blob = id.blob_path(&path);
    let failed_blob = blob.clone();
    let transfer = ready(&catalog, "source").await;
    catalog
        .publish_derived(&transfer.id, destination("copy"), move |connection, _| {
            Catalog::delete_derived_source_in_transaction(
                connection,
                "bucket",
                "source",
                original.generation,
                Default::default(),
                PERMISSIVE,
                std::time::SystemTime::now(),
            )?;
            fs::remove_file(&failed_blob)?;
            fs::create_dir(&failed_blob)?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(bytes(&catalog, "copy").await, b"bytes");
    assert!(catalog.maintain_derived_transfers().await.is_err());
    fs::remove_dir(&blob).unwrap();
    assert_eq!(catalog.maintain_derived_transfers().await.unwrap(), None);
    let tagged = catalog
        .run(|connection| {
            connection
                .query_row("SELECT count(*) FROM derived_deletions", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(object_unavailable)
        })
        .await
        .unwrap();
    assert_eq!(tagged, 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn dropping_begin_after_binding_starts_reclaims_the_ephemeral_transfer() {
    let (catalog, path) = setup().await;
    put(&catalog, "source", b"bytes").await;
    let source = source(&catalog, "source").await;
    let worker = catalog.clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let request = tokio::spawn(async move {
        worker
            .begin_derived_with_binding(
                vec![source.whole()],
                5,
                TransferKind::Ephemeral,
                move |_, transfer| {
                    assert_eq!(
                        transfer
                            .deadline
                            .duration_since(transfer.created)
                            .unwrap()
                            .as_millis(),
                        EPHEMERAL_LIFETIME_MILLIS as u128
                    );
                    entered.send(transfer.id).unwrap();
                    blocked.recv().unwrap();
                    Ok(())
                },
            )
            .await
    });
    let id = started.await.unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    eventually_async(
        "the derived transfer and its staging to be removed",
        || async {
            catalog.get_derived(&id).await == Err(ObjectError::DerivedTransferNotFound)
                && fs::read_dir(path.join("staging")).unwrap().count() == 0
        },
    )
    .await;
    assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn foreign_key_bindings_cascade_on_cancel_expiry_and_quarantine() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("PRAGMA foreign_keys = ON; CREATE TABLE objects(blob_id TEXT); CREATE TABLE pending_blob_deletions(blob_id TEXT PRIMARY KEY);").unwrap();
    connection.execute_batch(SCHEMA).unwrap();
    connection.execute_batch("CREATE TABLE bindings(transfer_id TEXT PRIMARY KEY REFERENCES derived_transfers(id) ON DELETE CASCADE, snapshot TEXT NOT NULL);").unwrap();
    for mode in ["cancel", "expiry", "quarantine"] {
        let id = random_id().unwrap();
        connection
            .execute(
                "INSERT INTO derived_transfers VALUES (?1,0,0,?2,0,0,0,1,0)",
                params![id, Md5::new().serialize().as_slice()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO bindings VALUES (?1, 'request snapshot')",
                [&id],
            )
            .unwrap();
        match mode {
            "cancel" => {
                let _discarded = discard_transfer(&mut connection, &id).unwrap();
            }
            "expiry" => recover(&connection, &std::env::temp_dir(), 1).unwrap(),
            "quarantine" => recover(&connection, &std::env::temp_dir(), 0).unwrap(),
            _ => unreachable!(),
        }
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM bindings", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn migrated_version_nine_catalog_has_no_foreign_key_violations_or_changed_object() {
    let (catalog, path) = setup().await;
    let mut original = put(&catalog, "source", b"bytes").await;
    original.time_created =
        system_time_millis(unix_millis(original.time_created).unwrap()).unwrap();
    original.updated = original.time_created;
    original.owner = None;
    catalog.close().await.unwrap();
    drop(catalog);
    let connection = Connection::open(path.join("catalog.sqlite3")).unwrap();
    super::super::test_support::downgrade_timestamp_precision(&connection);
    connection
        .execute_batch("DROP TABLE rewrite_sessions;")
        .unwrap();
    assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_schema AS schema, pragma_foreign_key_list(schema.name) WHERE schema.type = 'table'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    connection.execute_batch("ALTER TABLE buckets DROP COLUMN public_access_prevention_enforced; ALTER TABLE buckets DROP COLUMN cors; DROP TABLE derived_deletions; DROP TABLE derived_inputs; DROP TABLE derived_transfers; ALTER TABLE objects DROP COLUMN component_count; PRAGMA user_version = 9;").unwrap();
    assert!(
        !connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    drop(connection);
    let catalog = Catalog::open(&path).await.unwrap();
    assert_eq!(
        catalog
            .get_object(live("bucket", "source"), Default::default(), PERMISSIVE)
            .await
            .unwrap(),
        original
    );
    catalog
        .run(|connection| {
            assert_eq!(
                connection
                    .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert!(
                !connection
                    .prepare("PRAGMA foreign_key_check")
                    .unwrap()
                    .exists([])
                    .unwrap()
            );
            Ok::<_, ObjectError>(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
    fs::remove_dir_all(path).unwrap();
}
