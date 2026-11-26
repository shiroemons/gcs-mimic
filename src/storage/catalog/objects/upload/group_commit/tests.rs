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
    let writer = catalog
        .0
        .writer
        .execution
        .clone()
        .acquire_owned()
        .await
        .unwrap();
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
        catalog.0.writer.upload_commits.queued().len() == uploads.len()
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
async fn a_panicking_upload_fails_alone_and_its_changes_roll_back() {
    let data = TestDir::new();
    let catalog = open(&data.0).await;
    // The second upload of the batch panics after writing all of its rows.
    fault::arm(&data.0, Step::PublishUpload, 1, Fault::Panic);
    let tasks = queue_uploads(&catalog, &unconditional(&NAMES)).await;
    let outcomes = outcomes(tasks).await;
    assert_eq!(fault::pending(&data.0), 0);
    let failed: Vec<_> = NAMES
        .iter()
        .zip(&outcomes)
        .filter(|(_, outcome)| outcome.is_err())
        .collect();
    let [(failed, outcome)] = failed.as_slice() else {
        panic!("exactly one upload must fail: {outcomes:?}");
    };
    assert!(
        matches!(outcome, Err(ObjectError::Unavailable(_))),
        "{outcome:?}"
    );
    assert_eq!(
        read(&catalog, failed).await,
        Err(ObjectError::ObjectNotFound)
    );
    for (name, outcome) in NAMES.iter().zip(&outcomes) {
        if let Ok(record) = outcome {
            assert_eq!(&read(&catalog, name).await.unwrap(), record);
        }
    }
    assert_eq!(blob_count(&data.0), NAMES.len() - 1);
    put(&catalog, "after", b"ok").await;
    catalog.close().await.unwrap();
}

/// Marks the child process of
/// [`an_upload_that_rolls_back_the_batch_fails_alone_and_the_rest_publish`].
const RETRIED_BATCH: &str = "GCSM_TEST_RETRIED_BATCH";

/// The child runs the batch, and the parent reads the event it logged to its own stderr.
#[tokio::test]
async fn an_upload_that_rolls_back_the_batch_fails_alone_and_the_rest_publish() {
    if std::env::var_os(RETRIED_BATCH).is_some() {
        return retry_a_rolled_back_batch().await;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "storage::catalog::objects::upload::group_commit::tests::an_upload_that_rolls_back_the_batch_fails_alone_and_the_rest_publish",
            "--nocapture",
        ])
        .env(RETRIED_BATCH, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}\n{stderr}"
    );
    let retried: Vec<serde_json::Value> = stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == "upload_batch_retried")
        .collect();
    // One event for the batch; the upload that fails again alone is not retried and reports
    // its failure itself.
    assert_eq!(
        retried,
        [serde_json::json!({
            "event": "upload_batch_retried",
            "level": "warn",
            "uploads": NAMES.len(),
            "cause": "disk I/O error",
            "sqlite_extended_code": rusqlite::ffi::SQLITE_IOERR,
        })],
        "{stderr}"
    );
}

async fn retry_a_rolled_back_batch() {
    let data = TestDir::new();
    let catalog = open(&data.0).await;
    // The second upload of the batch rolls back the whole transaction, as SQLite may after an
    // I/O, full-disk or out-of-memory error, and does so again when it is published on its own.
    fault::arm(&data.0, Step::PublishUpload, 1, Fault::RollBack);
    fault::arm(&data.0, Step::PublishUpload, 1, Fault::RollBack);
    let tasks = queue_uploads(&catalog, &unconditional(&NAMES)).await;
    let outcomes = outcomes(tasks).await;
    // The second fault fires only when the uploads are published one by one.
    assert_eq!(fault::pending(&data.0), 0);
    let failed: Vec<_> = NAMES
        .iter()
        .zip(&outcomes)
        .filter(|(_, outcome)| outcome.is_err())
        .collect();
    let [(failed, outcome)] = failed.as_slice() else {
        panic!("exactly one upload must fail: {outcomes:?}");
    };
    assert!(
        matches!(outcome, Err(ObjectError::Unavailable(_))),
        "{outcome:?}"
    );
    assert_eq!(
        read(&catalog, failed).await,
        Err(ObjectError::ObjectNotFound)
    );
    for (name, outcome) in NAMES.iter().zip(&outcomes) {
        if let Ok(record) = outcome {
            assert_eq!(&read(&catalog, name).await.unwrap(), record);
        }
    }
    assert_eq!(blob_count(&data.0), NAMES.len() - 1);
    put(&catalog, "after", b"ok").await;
    catalog.close().await.unwrap();
}

/// Arms the second upload of the batch to end its transaction, so the uploads are published one
/// by one, and `fault` for the second of their commits: the batch never reaches its own commit.
fn arm_second_commit_after_a_rollback(data: &Path, step: Step, fault: Fault) {
    fault::arm(data, Step::PublishUpload, 1, Fault::RollBack);
    fault::arm(data, step, 1, fault);
}

#[tokio::test]
async fn a_failed_commit_while_publishing_one_by_one_fails_the_rest_and_keeps_what_committed() {
    for (step, committed) in [(Step::BeforeCommit, false), (Step::AfterCommit, true)] {
        let data = TestDir::new();
        let catalog = open(&data.0).await;
        arm_second_commit_after_a_rollback(&data.0, step, Fault::Io(io::ErrorKind::Other));
        let tasks = queue_uploads(&catalog, &unconditional(&NAMES)).await;
        let outcomes = outcomes(tasks).await;
        assert_eq!(fault::pending(&data.0), 0, "{step:?}");
        // The first upload committed on its own before the failure.
        let published: Vec<_> = outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
        let [first] = published.as_slice() else {
            panic!("exactly one upload must succeed at {step:?}: {outcomes:?}");
        };
        assert_eq!(&&read(&catalog, &first.name).await.unwrap(), first);
        // The second upload's commit failed and the third was never tried, so both report the
        // failure. Only a commit that failed after it was written stays published, and its
        // cleanup keeps its blob; the others' blobs are removed.
        let mut kept = 0;
        for (name, outcome) in NAMES.iter().zip(&outcomes) {
            if outcome.is_err() {
                assert!(
                    matches!(outcome, Err(ObjectError::Io(_))),
                    "{step:?} {outcome:?}"
                );
                kept += usize::from(read(&catalog, name).await.is_ok());
            }
        }
        assert_eq!(kept, usize::from(committed), "{step:?}");
        assert_eq!(blob_count(&data.0), 1 + usize::from(committed), "{step:?}");
        put(&catalog, "after", b"ok").await;
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_crash_while_publishing_one_by_one_keeps_only_what_committed() {
    for crash in Crash::ALL {
        for (step, committed) in [(Step::BeforeCommit, 1), (Step::AfterCommit, 2)] {
            let data = TestDir::new();
            let snapshot = TestDir::new();
            let catalog = open(&data.0).await;
            arm_second_commit_after_a_rollback(&data.0, step, crash.fault(snapshot.0.clone()));
            let tasks = queue_uploads(&catalog, &unconditional(&NAMES)).await;
            let mut records: Vec<_> = outcomes(tasks)
                .await
                .into_iter()
                .map(Result::unwrap)
                .collect();
            assert_eq!(fault::pending(&data.0), 0, "{crash:?} {step:?}");
            catalog.close().await.unwrap();
            // Each upload committed on its own in queue order, so generations give the order.
            records.sort_by_key(|record| record.generation);
            let restarted = Catalog::open(&snapshot.0).await.unwrap();
            let (kept, lost) = records.split_at(committed);
            for record in kept {
                let read = read(&restarted, &record.name).await.unwrap();
                assert_eq!(read.generation, record.generation, "{crash:?} {step:?}");
            }
            for record in lost {
                assert_eq!(
                    read(&restarted, &record.name).await,
                    Err(ObjectError::ObjectNotFound),
                    "{crash:?} {step:?}"
                );
            }
            // Recovery reclaims the published blobs that no committed row references.
            assert_eq!(blob_count(&snapshot.0), committed, "{crash:?} {step:?}");
            restarted.close().await.unwrap();
        }
    }
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

/// Marks the child process of [`uploads_publish_without_a_writable_temporary_directory`].
#[cfg(target_vendor = "apple")]
const NO_TEMPORARY_DIRECTORY: &str = "GCSM_TEST_NO_TEMPORARY_DIRECTORY";

/// SQLite creates a temporary file in the first of `SQLITE_TMPDIR`, `TMPDIR`, `/var/tmp`,
/// `/usr/tmp`, `/tmp` and `.` that exists and is writable. A test cannot take the system
/// directories away, so the child points `SQLITE_TMPDIR` at a directory whose path leaves no room
/// in SQLite's 512-byte buffer for a temporary file name: SQLite picks it and then cannot create
/// the file, as on a read-only root where no candidate is writable. Linux creates the file with
/// `O_TMPFILE` and never names it, so only Apple platforms can take temporary files away this way.
#[cfg(target_vendor = "apple")]
#[tokio::test]
async fn uploads_publish_without_a_writable_temporary_directory() {
    if std::env::var_os(NO_TEMPORARY_DIRECTORY).is_some() {
        return publish_long_names_without_temporary_files().await;
    }
    let base = TestDir::new();
    let mut unusable = base.0.clone();
    while unusable.as_os_str().len() < 520 {
        unusable.push("t".repeat(100));
    }
    fs::create_dir_all(&unusable).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "storage::catalog::objects::upload::group_commit::tests::uploads_publish_without_a_writable_temporary_directory",
            "--nocapture",
        ])
        .env(NO_TEMPORARY_DIRECTORY, "1")
        .env("SQLITE_TMPDIR", &unusable)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Publishes batches of uploads whose savepoints each rewrite more existing pages than SQLite
/// keeps in memory before it moves a sub-journal to a temporary file.
/// Checked: with the writer's `temp_store = FILE`, these uploads fail with `SQL logic error`.
#[cfg(target_vendor = "apple")]
async fn publish_long_names_without_temporary_files() {
    let data = TestDir::new();
    assert_no_temporary_files(&data.0);
    let catalog = open(&data.0).await;
    // A name this long leaves room for only a few keys on an index page, so publishing one
    // splits pages that existed before its savepoint opened.
    let names: Vec<String> = (0..192_u32)
        .map(|index| {
            format!(
                "{:08x}-{}",
                index.wrapping_mul(0x9E37_79B9),
                "n".repeat(900)
            )
        })
        .collect();
    let writer = catalog
        .0
        .writer
        .execution
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let tasks: Vec<_> = names
        .iter()
        .map(|name| {
            let catalog = catalog.clone();
            let name = name.clone();
            tokio::spawn(async move {
                let permit = catalog.wait_object_upload().await?;
                let upload = permissive_upload("bucket", &name, 1000);
                let contents = body(vec![Bytes::from_static(b"spill")]);
                catalog
                    .put_object_prechecked(upload, contents, permit)
                    .await
            })
        })
        .collect();
    eventually_async("every upload queued behind the writer", || async {
        catalog.0.writer.upload_commits.queued().len() == names.len()
    })
    .await;
    drop(writer);
    for (name, outcome) in names.iter().zip(outcomes(tasks).await) {
        assert_eq!(outcome.unwrap().name, *name);
    }
    for name in &names {
        read(&catalog, name).await.unwrap();
    }
    catalog.close().await.unwrap();
}

/// Shows that this process cannot create an SQLite temporary file: a sort larger than the page
/// cache would spill to one.
#[cfg(target_vendor = "apple")]
fn assert_no_temporary_files(scratch: &Path) {
    let connection = Connection::open(scratch.join("probe.sqlite3")).unwrap();
    connection
        .execute_batch("PRAGMA temp_store = FILE; PRAGMA cache_size = -1024;")
        .unwrap();
    let sorted = connection.query_row(
        "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i + 1 FROM n WHERE i < 20000) \
         SELECT randomblob(200) AS r FROM n ORDER BY r",
        [],
        |row| row.get::<_, Vec<u8>>(0),
    );
    assert!(sorted.is_err(), "{sorted:?}");
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
