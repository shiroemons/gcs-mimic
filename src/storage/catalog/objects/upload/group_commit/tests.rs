use super::*;
use crate::storage::catalog::fault::{Crash, Fault};
use crate::storage::test_fixtures::{PERMISSIVE, body, bucket, live, permissive_upload};
use crate::test_support::{TestDir, eventually_async, joined};

const NAMES: [&str; 3] = ["first", "second", "third"];

async fn open(data: &Path) -> Catalog {
    let catalog = Catalog::open(data).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    catalog
}

async fn put(catalog: &Catalog, name: &str, contents: &'static [u8]) -> ObjectRecord {
    catalog
        .put_object(
            permissive_upload("bucket", name, 1000),
            body(vec![Bytes::from_static(contents)]),
        )
        .await
        .unwrap()
}

async fn read(catalog: &Catalog, name: &str) -> Result<ObjectRecord, ObjectError> {
    catalog
        .get_object(live("bucket", name), Default::default(), PERMISSIVE)
        .await
}

fn blob_count(data: &Path) -> usize {
    fs::read_dir(data.join("blobs"))
        .unwrap()
        .map(|shard| fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum()
}

/// Holds the writer, starts an upload of each name whose body is the name itself, and returns
/// once every commit waits in the queue, so releasing the writer publishes them as one batch.
async fn queue_uploads(
    catalog: &Catalog,
    uploads: &[(&'static str, ObjectConditions)],
) -> Vec<tokio::task::JoinHandle<Result<ObjectRecord, ObjectError>>> {
    let writer = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let tasks = uploads
        .iter()
        .map(|&(name, conditions)| {
            let catalog = catalog.clone();
            tokio::spawn(async move {
                let permit = catalog.wait_object_upload().await?;
                let upload = ObjectUpload {
                    conditions,
                    ..permissive_upload("bucket", name, 1000)
                };
                let contents = body(vec![Bytes::from_static(name.as_bytes())]);
                catalog
                    .put_object_prechecked(upload, contents, permit)
                    .await
            })
        })
        .collect();
    eventually_async("every upload queued behind the writer", || async {
        catalog.0.upload_commits.queued().len() == uploads.len()
    })
    .await;
    drop(writer);
    tasks
}

async fn outcomes(
    tasks: Vec<tokio::task::JoinHandle<Result<ObjectRecord, ObjectError>>>,
) -> Vec<Result<ObjectRecord, ObjectError>> {
    let mut outcomes = Vec::with_capacity(tasks.len());
    for task in tasks {
        outcomes.push(joined(task).await);
    }
    outcomes
}

fn unconditional(names: &[&'static str]) -> Vec<(&'static str, ObjectConditions)> {
    names
        .iter()
        .map(|&name| (name, Default::default()))
        .collect()
}

#[tokio::test]
async fn queued_uploads_publish_together_and_a_failed_precondition_fails_alone() {
    let data = TestDir::new();
    let catalog = open(&data.0).await;
    let existing = put(&catalog, "existing", b"old").await;
    let mismatch = ObjectConditions {
        if_generation_match: Some(existing.generation + 1),
        ..Default::default()
    };
    let tasks = queue_uploads(
        &catalog,
        &[
            ("first", ObjectConditions::default()),
            ("existing", mismatch),
            ("second", ObjectConditions::default()),
        ],
    )
    .await;
    let [first, refused, second] = <[_; 3]>::try_from(outcomes(tasks).await).unwrap();
    let (first, second) = (first.unwrap(), second.unwrap());
    assert_eq!(refused, Err(ObjectError::PreconditionFailed));
    assert_ne!(first.generation, second.generation);
    assert_eq!(read(&catalog, "first").await.unwrap(), first);
    assert_eq!(read(&catalog, "second").await.unwrap(), second);
    // The refused upload's savepoint rolled back only its own work.
    assert_eq!(read(&catalog, "existing").await.unwrap(), existing);
    assert_eq!(blob_count(&data.0), 3);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_failed_batch_commit_fails_every_queued_upload_and_keeps_none_of_their_blobs() {
    let data = TestDir::new();
    let catalog = open(&data.0).await;
    // One failed commit fails all three, which only a shared transaction can do.
    fault::arm(
        &data.0,
        Step::BeforeCommit,
        0,
        Fault::Io(io::ErrorKind::Other),
    );
    let tasks = queue_uploads(&catalog, &unconditional(&NAMES)).await;
    for outcome in outcomes(tasks).await {
        assert!(matches!(outcome, Err(ObjectError::Io(_))), "{outcome:?}");
    }
    assert_eq!(fault::pending(&data.0), 0);
    for name in NAMES {
        assert_eq!(read(&catalog, name).await, Err(ObjectError::ObjectNotFound));
    }
    assert_eq!(blob_count(&data.0), 0);
    put(&catalog, "after", b"ok").await;
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn an_error_after_a_batch_commit_keeps_every_upload_it_published() {
    let data = TestDir::new();
    let catalog = open(&data.0).await;
    fault::arm(
        &data.0,
        Step::AfterCommit,
        0,
        Fault::Io(io::ErrorKind::Other),
    );
    let tasks = queue_uploads(&catalog, &unconditional(&NAMES)).await;
    for outcome in outcomes(tasks).await {
        assert!(matches!(outcome, Err(ObjectError::Io(_))), "{outcome:?}");
    }
    // Each upload's cleanup finds its committed reference and keeps the blob.
    for name in NAMES {
        assert_eq!(read(&catalog, name).await.unwrap().size, name.len() as u64);
    }
    assert_eq!(blob_count(&data.0), NAMES.len());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_crash_around_a_batch_commit_keeps_all_or_none_of_its_uploads() {
    for crash in Crash::ALL {
        for (step, committed) in [(Step::BeforeCommit, false), (Step::AfterCommit, true)] {
            let data = TestDir::new();
            let snapshot = TestDir::new();
            let catalog = open(&data.0).await;
            fault::arm(&data.0, step, 0, crash.fault(snapshot.0.clone()));
            let tasks = queue_uploads(&catalog, &unconditional(&NAMES)).await;
            for outcome in outcomes(tasks).await {
                outcome.unwrap();
            }
            assert_eq!(fault::pending(&data.0), 0, "{crash:?} {step:?}");
            catalog.close().await.unwrap();
            let restarted = Catalog::open(&snapshot.0).await.unwrap();
            for name in NAMES {
                let outcome = read(&restarted, name).await;
                if committed {
                    assert_eq!(
                        outcome.unwrap().size,
                        name.len() as u64,
                        "{crash:?} {step:?}"
                    );
                } else {
                    assert_eq!(
                        outcome,
                        Err(ObjectError::ObjectNotFound),
                        "{crash:?} {step:?}"
                    );
                }
            }
            // Recovery reclaims the published blobs that no committed row references.
            let expected = if committed { NAMES.len() } else { 0 };
            assert_eq!(blob_count(&snapshot.0), expected, "{crash:?} {step:?}");
            restarted.close().await.unwrap();
        }
    }
}
