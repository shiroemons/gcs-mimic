use super::{ObjectActivity, await_upload_catalog};
use crate::{
    bucket::BucketAccess,
    config::Config,
    json,
    object::{ObjectError, ObjectRecord},
    storage::{Catalog, CopyReplay, DerivedDestination, DerivedSource},
};
use std::time::Duration;
use tokio::{sync::watch, time::Instant};

const BYTE_BUDGET: u64 = 16 * 1024 * 1024;

#[cfg(test)]
pub(super) struct Checkpoint {
    after_bytes: u64,
    reached: tokio::sync::oneshot::Sender<crate::storage::DerivedTransfer>,
    resume: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
async fn checkpoint_at(
    progress: &crate::storage::DerivedTransfer,
    checkpoint: &mut Option<Checkpoint>,
) -> Result<(), ObjectError> {
    if checkpoint
        .as_ref()
        .is_some_and(|checkpoint| progress.offset >= checkpoint.after_bytes)
    {
        let checkpoint = checkpoint.take().unwrap();
        checkpoint
            .reached
            .send(progress.clone())
            .map_err(|_| ObjectError::Unavailable("copy checkpoint observer dropped".into()))?;
        checkpoint
            .resume
            .await
            .map_err(|_| ObjectError::Unavailable("copy checkpoint abandoned".into()))?;
    }
    Ok(())
}

/// What one copy reads, where it writes, and the replay record of an idempotent retry.
pub(crate) struct CopyJob {
    pub(crate) source: DerivedSource,
    pub(crate) source_access: BucketAccess,
    pub(crate) destination: DerivedDestination,
    /// The source bucket's location, read only for a composite source.
    pub(crate) source_location: Option<String>,
    pub(crate) replay: Option<CopyReplay>,
}

/// The request limits a copy runs under.
#[derive(Clone, Copy)]
pub(crate) struct CopyBudget<'a> {
    pub(crate) config: &'a Config,
    pub(crate) deadline: Instant,
    pub(crate) activity: &'a watch::Sender<ObjectActivity>,
}

pub(super) async fn execute(
    catalog: &Catalog,
    job: CopyJob,
    budget: CopyBudget<'_>,
    #[cfg(test)] mut checkpoint: Option<Checkpoint>,
) -> Result<ObjectRecord, ObjectError> {
    let CopyJob {
        source,
        source_access,
        destination,
        source_location,
        replay,
    } = job;
    let CopyBudget {
        config,
        deadline,
        activity,
    } = budget;
    let catalog_timeout = Duration::from_millis(config.catalog_wait_timeout_ms);
    if replay.is_none() {
        await_upload_catalog(
            catalog.check_upload(
                &destination.bucket,
                &destination.name,
                destination.conditions,
                &destination.access,
            ),
            catalog_timeout,
            deadline,
            activity,
        )
        .await?;
    }
    let source_record = source.object.clone();
    // shortcut: admission is phase-local; reserve one slot if later-stage rejection under load must be eliminated.
    activity.send_modify(|activity| activity.awaiting_catalog = true);
    let transfer = tokio::time::timeout_at(
        deadline,
        catalog.begin_derived_ephemeral(vec![source.whole()], config.max_object_bytes),
    )
    .await
    .map_err(|_| ObjectError::Unavailable("copy request deadline exceeded".into()))??;
    activity.send_modify(|activity| activity.awaiting_catalog = false);
    let mut owner = catalog.guard_derived_request(&transfer.id);
    // The request owns this future: cancellation can finish an in-flight bounded advance,
    // but cannot start publication after its handler and cleanup guard have been dropped.
    tokio::time::timeout_at(deadline, async {
        let mut progress = transfer.clone();
        #[cfg(test)]
        checkpoint_at(&progress, &mut checkpoint).await?;
        while progress.offset < progress.total {
            activity.send_modify(|activity| activity.awaiting_catalog = true);
            progress = catalog.advance_derived(&transfer.id, BYTE_BUDGET).await?;
            activity.send_modify(|activity| activity.awaiting_catalog = false);
            #[cfg(test)]
            checkpoint_at(&progress, &mut checkpoint).await?;
        }
        Ok::<_, ObjectError>(())
    })
    .await
    .map_err(|_| ObjectError::Unavailable("copy request deadline exceeded".into()))??;
    activity.send_modify(|activity| activity.awaiting_catalog = true);
    // Only catalog admission can time out. Once publication starts, return its durable outcome
    // rather than reporting a timeout while a detached mutation can still commit.
    let result = catalog
        .publish_derived_replayed(
            &transfer.id,
            destination,
            move |connection, destination| {
                Catalog::check_derived_bucket_access_in_transaction(
                    connection,
                    &source_record.bucket,
                    &source_access,
                )?;
                if source_record.component_count.is_some() {
                    let location: String = connection
                        .query_row(
                            "SELECT location FROM buckets WHERE name = ?1",
                            [&destination.bucket],
                            |row| row.get(0),
                        )
                        .map_err(ObjectError::backend)?;
                    destination.component_count = json::derived_component_count(
                        &source_record,
                        &destination.storage_class,
                        json::DestinationLocation::of(source_location.as_deref(), &location),
                    );
                }
                Ok(())
            },
            |_, _| Ok(()),
            Some(deadline.min(Instant::now() + catalog_timeout)),
            replay,
        )
        .await;
    activity.send_modify(|activity| activity.awaiting_catalog = false);
    if result.is_ok() {
        owner.disarm();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bucket::{BucketAccess, BucketRecord},
        object::{ObjectConditions, UploadChunk},
        storage::DerivedTransfer,
    };
    use bytes::Bytes;
    use std::{fs, path::PathBuf, time::UNIX_EPOCH};
    use tokio::sync::{mpsc, oneshot};

    async fn fixture() -> (Catalog, PathBuf) {
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("gcs-mimic-copy-{id}"));
        let catalog = Catalog::open(&path).await.unwrap();
        catalog
            .insert_bucket(BucketRecord {
                name: "bucket".into(),
                project: "project".into(),
                location: "US".into(),
                location_type: "multi-region".into(),
                storage_class: "STANDARD".into(),
                autoclass: None,
                labels: Default::default(),
                versioning_enabled: false,
                default_event_based_hold: false,
                public_access_prevention_enforced: false,
                uniform_bucket_level_access_locked_time: None,
                lifecycle: Vec::new(),
                retention_policy: None,
                soft_delete_policy: None,
                object_retention_enabled: false,
                cors: Vec::new(),
                metageneration: 1,
                time_created: UNIX_EPOCH,
                updated: UNIX_EPOCH,
            })
            .await
            .unwrap();
        put(&catalog, "source", vec![b'a'; BYTE_BUDGET as usize + 7]).await;
        (catalog, path)
    }

    async fn put(catalog: &Catalog, name: &str, bytes: Vec<u8>) -> ObjectRecord {
        let (sender, body) = mpsc::channel(2);
        sender
            .try_send(Ok(UploadChunk::Data(Bytes::from(bytes))))
            .unwrap();
        sender.try_send(Ok(UploadChunk::End)).unwrap();
        catalog
            .put_object(
                "bucket",
                name,
                "text/plain".into(),
                Default::default(),
                Default::default(),
                16 * BYTE_BUDGET,
                body,
            )
            .await
            .unwrap()
    }

    async fn source(catalog: &Catalog) -> DerivedSource {
        let record = catalog
            .get_object("bucket", "source", None, Default::default())
            .await
            .unwrap();
        catalog
            .resolve_derived_source(
                "bucket",
                "source",
                None,
                ObjectConditions {
                    if_generation_match: Some(record.generation),
                    ..Default::default()
                },
                &BucketAccess::Permissive,
            )
            .await
            .unwrap()
    }

    fn destination(source: &DerivedSource, name: &str, body: &[u8]) -> DerivedDestination {
        let (metadata, storage_class, expected) =
            json::derived_metadata(body, &source.object).unwrap();
        DerivedDestination {
            bucket: "bucket".into(),
            name: name.into(),
            content_type: String::new(),
            metadata,
            storage_class,
            expected,
            ..DerivedDestination::new(BucketAccess::Permissive)
        }
    }

    fn start(
        catalog: &Catalog,
        source: DerivedSource,
        source_access: BucketAccess,
        destination: DerivedDestination,
        after_bytes: u64,
        wait_ms: u64,
    ) -> (
        tokio::task::JoinHandle<Result<ObjectRecord, ObjectError>>,
        oneshot::Receiver<DerivedTransfer>,
        oneshot::Sender<()>,
    ) {
        let catalog = catalog.clone();
        let (reached, observed) = oneshot::channel();
        let (resume, paused) = oneshot::channel();
        let request = tokio::spawn(async move {
            let config = Config {
                catalog_wait_timeout_ms: wait_ms,
                ..Default::default()
            };
            let (activity, _updates) = watch::channel(ObjectActivity::default());
            execute(
                &catalog,
                CopyJob {
                    source,
                    source_access,
                    destination,
                    source_location: Some("US".into()),
                    replay: None,
                },
                CopyBudget {
                    config: &config,
                    deadline: Instant::now() + Duration::from_secs(15),
                    activity: &activity,
                },
                Some(Checkpoint {
                    after_bytes,
                    reached,
                    resume: paused,
                }),
            )
            .await
        });
        (request, observed, resume)
    }

    async fn clean(catalog: &Catalog, path: &std::path::Path, id: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while catalog.get_derived(id).await != Err(ObjectError::DerivedTransferNotFound) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn slow_copy_stream_is_not_limited_by_the_catalog_wait_budget() {
        let (catalog, path) = fixture().await;
        let source = source(&catalog).await;
        let expected = source.object.md5;
        let target = destination(&source, "copy", b"");
        let (request, progress, resume) = start(
            &catalog,
            source,
            BucketAccess::Permissive,
            target,
            BYTE_BUDGET,
            250,
        );
        let progress = progress.await.unwrap();
        assert_eq!(progress.offset, BYTE_BUDGET);
        assert!(progress.offset < progress.total);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!request.is_finished());
        assert_eq!(
            catalog
                .get_object("bucket", "copy", None, Default::default())
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        resume.send(()).unwrap();
        let response = request.await.unwrap().unwrap();
        assert_eq!(response.md5, expected);
        assert_eq!(
            catalog
                .get_object("bucket", "copy", None, Default::default())
                .await
                .unwrap(),
            response
        );
        clean(&catalog, &path, &progress.id).await;
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn copy_advisory_failure_does_not_begin_a_transfer() {
        let (catalog, path) = fixture().await;
        let source = source(&catalog).await;
        let existing = put(&catalog, "copy", b"preserved".to_vec()).await;
        let mut target = destination(&source, "copy", b"");
        target.conditions.if_generation_match = Some(0);
        let (request, observed, _resume) =
            start(&catalog, source, BucketAccess::Permissive, target, 0, 5000);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), request)
                .await
                .unwrap()
                .unwrap(),
            Err(ObjectError::PreconditionFailed)
        ));
        assert!(observed.await.is_err());
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        assert_eq!(
            catalog
                .get_object("bucket", "copy", None, Default::default())
                .await
                .unwrap(),
            existing
        );
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn copy_handler_drop_and_advance_failure_reclaim_the_owned_transfer() {
        for drop_handler in [true, false] {
            let (catalog, path) = fixture().await;
            let source = source(&catalog).await;
            let target = destination(&source, "copy", b"");
            let (request, progress, resume) =
                start(&catalog, source, BucketAccess::Permissive, target, 0, 5000);
            let progress = progress.await.unwrap();
            if drop_handler {
                request.abort();
                assert!(request.await.unwrap_err().is_cancelled());
            } else {
                let pinned = fs::read_dir(path.join("staging"))
                    .unwrap()
                    .map(|entry| entry.unwrap())
                    .find(|entry| entry.file_name().to_string_lossy() != progress.id)
                    .unwrap();
                fs::remove_file(pinned.path()).unwrap();
                resume.send(()).unwrap();
                // The removed source is a filesystem failure, classified like any other I/O error.
                assert!(matches!(
                    request.await.unwrap(),
                    Err(ObjectError::Io(cause)) if cause.kind() == std::io::ErrorKind::NotFound
                ));
            }
            clean(&catalog, &path, &progress.id).await;
            assert_eq!(
                catalog
                    .get_object("bucket", "copy", None, Default::default())
                    .await,
                Err(ObjectError::ObjectNotFound)
            );
            catalog.close().await.unwrap();
            fs::remove_dir_all(path).unwrap();
        }
    }

    #[tokio::test]
    async fn copy_pins_source_conditions_at_selection_during_concurrent_replacement() {
        for target_name in ["copy", "source"] {
            let (catalog, path) = fixture().await;
            let source = source(&catalog).await;
            let selected = source.object.clone();
            let target = destination(&source, target_name, b"");
            let (request, progress, resume) = start(
                &catalog,
                source,
                BucketAccess::Permissive,
                target,
                BYTE_BUDGET,
                5000,
            );
            let progress = progress.await.unwrap();
            assert_eq!(
                fs::metadata(path.join("staging").join(&progress.id))
                    .unwrap()
                    .len(),
                BYTE_BUDGET
            );
            let replacement = put(&catalog, "source", b"new source".to_vec()).await;
            assert_ne!(replacement.md5, selected.md5);
            resume.send(()).unwrap();
            let copied = request.await.unwrap().unwrap();
            assert_eq!(copied.md5, selected.md5);
            assert_eq!(copied.crc32c, selected.crc32c);
            assert_eq!(copied.size, selected.size);
            assert!(copied.generation > replacement.generation);
            clean(&catalog, &path, &progress.id).await;
            catalog.close().await.unwrap();
            fs::remove_dir_all(path).unwrap();
        }
    }

    #[tokio::test]
    async fn copy_rechecks_source_read_grant_before_publication() {
        use crate::auth::{Auth, Binding, OBJECTS_GET, Policy, Registry};

        let (catalog, path) = fixture().await;
        let auth = Auth::strict(
            Registry::parse(br#"{"identities":[{"id":"reader","tokens":["token"]}]}"#).unwrap(),
        );
        let mut headers = hyper::HeaderMap::new();
        headers.insert("authorization", "Bearer token".parse().unwrap());
        let caller = auth.authenticate(&headers, None).unwrap().unwrap();
        catalog
            .set_policy(
                "bucket",
                Policy {
                    bindings: vec![Binding {
                        role: "roles/storage.objectViewer".into(),
                        members: vec!["user:reader".into()],
                    }],
                    etag: None,
                },
                &BucketAccess::Permissive,
            )
            .await
            .unwrap();
        let access = catalog
            .bucket_access("bucket", Some(caller.principal().unwrap()), OBJECTS_GET)
            .await
            .unwrap();
        let source = catalog
            .resolve_derived_source("bucket", "source", None, Default::default(), &access)
            .await
            .unwrap();
        let target = destination(&source, "copy", b"");
        let (request, progress, resume) =
            start(&catalog, source, access, target, BYTE_BUDGET, 5000);
        let progress = progress.await.unwrap();
        assert_eq!(progress.offset, BYTE_BUDGET);
        catalog
            .set_policy(
                "bucket",
                Policy {
                    bindings: vec![],
                    etag: None,
                },
                &BucketAccess::Permissive,
            )
            .await
            .unwrap();
        resume.send(()).unwrap();
        let error = request.await.unwrap().unwrap_err();
        assert_eq!(error, ObjectError::Forbidden);
        assert_eq!(
            json::JsonError::from(error).status,
            hyper::StatusCode::FORBIDDEN
        );
        assert_eq!(
            catalog
                .get_object("bucket", "copy", None, Default::default())
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        clean(&catalog, &path, &progress.id).await;
        let connection = rusqlite::Connection::open(path.join("catalog.sqlite3")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM derived_transfers", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(connection);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn copy_rechecks_destination_conditions_after_the_advisory_check() {
        let (catalog, path) = fixture().await;
        let source = source(&catalog).await;
        let mut target = destination(&source, "copy", b"");
        target.conditions.if_generation_match = Some(0);
        let (request, progress, resume) = start(
            &catalog,
            source,
            BucketAccess::Permissive,
            target,
            BYTE_BUDGET,
            5000,
        );
        let progress = progress.await.unwrap();
        let competing = put(&catalog, "copy", b"concurrent destination".to_vec()).await;
        resume.send(()).unwrap();
        assert_eq!(request.await.unwrap(), Err(ObjectError::PreconditionFailed));
        assert_eq!(
            catalog
                .get_object("bucket", "copy", None, Default::default())
                .await
                .unwrap(),
            competing
        );
        clean(&catalog, &path, &progress.id).await;
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn composite_count_uses_the_destination_class_selected_at_commit() {
        let (catalog, path) = fixture().await;
        rusqlite::Connection::open(path.join("catalog.sqlite3")).unwrap().execute("UPDATE objects SET component_count = 3, storage_class = 'NEARLINE' WHERE name = 'source'", []).unwrap();
        let source = source(&catalog).await;
        let target = destination(&source, "copy", b"{}");
        let (request, progress, resume) = start(
            &catalog,
            source,
            BucketAccess::Permissive,
            target,
            BYTE_BUDGET,
            5000,
        );
        let progress = progress.await.unwrap();
        rusqlite::Connection::open(path.join("catalog.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE buckets SET storage_class = 'NEARLINE' WHERE name = 'bucket'",
                [],
            )
            .unwrap();
        resume.send(()).unwrap();
        let copied = request.await.unwrap().unwrap();
        assert_eq!(copied.storage_class, "NEARLINE");
        assert_eq!(copied.component_count, Some(3));
        clean(&catalog, &path, &progress.id).await;
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn timed_out_publication_admission_never_commits_after_the_response() {
        let (catalog, path) = fixture().await;
        let source = source(&catalog).await;
        let length = source.object.size;
        let target = destination(&source, "copy", b"");
        let (request, progress, resume) = start(
            &catalog,
            source,
            BucketAccess::Permissive,
            target,
            length,
            100,
        );
        let progress = progress.await.unwrap();
        assert_eq!(progress.offset, progress.total);
        let (entered, blocked) = oneshot::channel();
        let (release, hold) = std::sync::mpsc::channel();
        let blocking_catalog = catalog.clone();
        let blocker = tokio::spawn(async move {
            blocking_catalog
                .update_object_metadata(
                    "bucket",
                    "source",
                    None,
                    Default::default(),
                    Default::default(),
                    move |_| {
                        entered.send(()).unwrap();
                        hold.recv().unwrap();
                        Ok(())
                    },
                )
                .await
        });
        blocked.await.unwrap();
        resume.send(()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(5), request).await;
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        assert!(matches!(
            response
                .expect("publication admission did not time out")
                .unwrap(),
            Err(ObjectError::Unavailable(_))
        ));
        clean(&catalog, &path, &progress.id).await;
        assert_eq!(
            catalog
                .get_object("bucket", "copy", None, Default::default())
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn shutdown_during_copy_recovers_without_a_destination() {
        let (catalog, path) = fixture().await;
        let source = source(&catalog).await;
        let selected = source.object.clone();
        let target = destination(&source, "copy", b"");
        let (request, progress, _resume) = start(
            &catalog,
            source,
            BucketAccess::Permissive,
            target,
            BYTE_BUDGET,
            5000,
        );
        let progress = progress.await.unwrap();
        assert!(progress.offset > 0 && progress.offset < progress.total);
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open(&path).await.unwrap();
        clean(&catalog, &path, &progress.id).await;
        assert_eq!(
            catalog
                .get_object("bucket", "copy", None, Default::default())
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        assert_eq!(
            catalog
                .get_object("bucket", "source", None, Default::default())
                .await
                .unwrap(),
            selected
        );
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }
}
