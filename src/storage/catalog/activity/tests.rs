use super::*;
use crate::{
    bucket::BucketAccess,
    clock::SystemClock,
    config::Config,
    object::ObjectError,
    storage::{
        ResumableOptions,
        admin::{Admin, BucketDefinition, BucketName, LocalAdmin},
        catalog::{
            AppendError, ResumableError,
            fault::{Fault, Step},
        },
        test_fixtures::{PERMISSIVE, body, bucket, live, permissive_upload},
    },
    test_support::{TestDir, joined},
    time_limit::TimeLimit,
};
use bytes::Bytes;
use std::sync::{Mutex, atomic::AtomicBool, mpsc};
use tokio::{sync::oneshot, task::JoinHandle};

type UploadBody = tokio::sync::mpsc::Receiver<Result<crate::object::UploadChunk, std::io::Error>>;

const BUCKET: &str = "activity-bucket";
/// Long enough that no test reaches it unless the behaviour under test is broken.
const LONG: TimeLimit = TimeLimit::from_millis(60_000);

async fn setup() -> (TestDir, Catalog, Admin) {
    let dir = TestDir::new();
    let catalog = Catalog::open_for_server(&dir.0, Arc::new(SystemClock), LocalAdmin::Enabled)
        .await
        .unwrap();
    let admin = catalog.local_admin().unwrap();
    admin
        .ensure_bucket(
            BucketDefinition::default_for(
                &BucketName::new(BUCKET).unwrap(),
                "project",
                &SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    (dir, catalog, admin)
}

/// Lets spawned callers run up to their first real wait. A caller that wrongly answers instead
/// of waiting has finished by the time this returns.
async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

/// Holds the next commit open until the returned sender fires, so a job stays exclusive for as
/// long as a test needs it to.
fn block_next_commit(catalog: &Catalog) -> (oneshot::Receiver<()>, mpsc::Sender<()>) {
    let (entered, reached) = oneshot::channel();
    let (release, released) = mpsc::channel();
    catalog.inject(
        Step::BeforeCommit,
        0,
        Fault::Block {
            entered: Arc::new(Mutex::new(Some(entered))),
            release: Arc::new(Mutex::new(released)),
        },
    );
    (reached, release)
}

fn spawn_reset(admin: &Admin) -> JoinHandle<Result<(), AdminError>> {
    let admin = admin.clone();
    tokio::spawn(async move { admin.reset_until(LONG.deadline()).await.map(drop) })
}

fn spawn_new_upload(catalog: &Catalog) -> JoinHandle<Result<(), ObjectError>> {
    let catalog = catalog.clone();
    tokio::spawn(async move { catalog.wait_object_upload().await.map(drop) })
}

fn one_chunk(bytes: &'static [u8]) -> UploadBody {
    body(vec![Bytes::from_static(bytes)])
}

fn closed_upload(result: Result<(), ObjectError>) -> bool {
    matches!(result, Err(ObjectError::Unavailable(cause))
        if cause.failure() == Some(BackendFailure::CatalogClosed))
}

#[tokio::test]
async fn requests_during_a_running_reset_wait_and_then_see_the_cleared_state() {
    let (_dir, catalog, admin) = setup().await;
    let session = catalog
        .create_resumable_session(
            BUCKET,
            "pending",
            ResumableOptions::new(BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    catalog
        .put_object(permissive_upload(BUCKET, "value", 16), one_chunk(b"value"))
        .await
        .unwrap();
    let (reached, release) = block_next_commit(&catalog);
    let reset = spawn_reset(&admin);
    reached.await.unwrap();

    let reader = catalog.clone();
    let read = tokio::spawn(async move { reader.get_bucket(BUCKET, PERMISSIVE).await });
    let writer = catalog.clone();
    let write = tokio::spawn(async move { writer.insert_bucket(bucket("other", "project")).await });
    let uploader = catalog.clone();
    let upload = tokio::spawn(async move {
        let permit = uploader.wait_object_upload().await?;
        uploader
            .insert_bucket(bucket("uploaded", "project"))
            .await?;
        uploader
            .put_object_prechecked(
                permissive_upload("uploaded", "value", 16),
                one_chunk(b"fresh"),
                permit,
            )
            .await
    });
    let locker = catalog.clone();
    let id = session.id.to_string();
    let lock =
        tokio::spawn(async move { locker.lock_resumable_session(&id, BUCKET).await.map(drop) });
    let source = catalog.clone();
    let pin = tokio::spawn(async move {
        source
            .resolve_derived_source(live(BUCKET, "value"), Default::default(), PERMISSIVE)
            .await
            .map(drop)
    });
    let appender = catalog.clone();
    let append = tokio::spawn(async move {
        appender
            .create_appendable(
                BUCKET,
                "appended",
                ResumableOptions::new(BucketAccess::Permissive),
            )
            .await
            .map(drop)
    });
    settle().await;
    for (name, finished) in [
        ("read", read.is_finished()),
        ("write", write.is_finished()),
        ("upload", upload.is_finished()),
        ("lock", lock.is_finished()),
        ("pin", pin.is_finished()),
        ("append", append.is_finished()),
    ] {
        assert!(!finished, "{name} answered during the reset");
    }

    release.send(()).unwrap();
    joined(reset).await.unwrap();
    assert_eq!(joined(read).await.unwrap(), None);
    joined(write).await.unwrap();
    assert_eq!(joined(upload).await.unwrap().size, 5);
    assert_eq!(
        catalog
            .get_object(live("uploaded", "value"), Default::default(), PERMISSIVE)
            .await
            .unwrap()
            .size,
        5
    );
    assert!(matches!(joined(lock).await, Err(ResumableError::NotFound)));
    assert!(matches!(
        joined(pin).await,
        Err(ObjectError::BucketNotFound)
    ));
    assert!(matches!(
        joined(append).await,
        Err(AppendError::Object(ObjectError::BucketNotFound))
    ));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_copy_replay_that_meets_a_starting_job_waits_for_it_instead_of_failing() {
    let (_dir, catalog, admin) = setup().await;
    catalog
        .put_object(
            permissive_upload(BUCKET, "source", 16),
            one_chunk(b"source"),
        )
        .await
        .unwrap();
    // Holding the permit lines the replay's first job up ahead of the reset, so the reset owns the
    // catalog by the time that job's result gets back to the replay.
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let worker = catalog.clone();
    let replay = tokio::spawn(async move {
        worker
            .prepare_copy_replay(
                BUCKET,
                "target",
                Default::default(),
                &BucketAccess::Permissive,
                crate::storage::CopyReplaySource {
                    bucket: BUCKET,
                    name: "source",
                    generation: None,
                    access: &BucketAccess::Permissive,
                },
                crate::storage::ReplayInvocation {
                    token: Some("invocation"),
                    request: "copy",
                },
            )
            .await
            .map(drop)
    });
    settle().await;
    let (reached, release) = block_next_commit(&catalog);
    let reset = spawn_reset(&admin);
    settle().await;
    drop(execution);
    reached.await.unwrap();
    settle().await;
    assert!(!replay.is_finished());
    release.send(()).unwrap();
    joined(reset).await.unwrap();
    // It then runs against the state the reset left.
    assert!(matches!(
        joined(replay).await,
        Err(ObjectError::BucketNotFound)
    ));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_client_waiting_for_a_session_writer_also_waits_out_an_exclusive_job() {
    let (_dir, catalog, _admin) = setup().await;
    catalog.apply_session_limits(&Config {
        catalog_wait_timeout: LONG,
        ..Default::default()
    });
    let session = catalog
        .create_resumable_session(
            BUCKET,
            "pending",
            ResumableOptions::new(BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    let exclusive = catalog.wait_exclusive(LONG.deadline()).await.unwrap();
    let locker = catalog.clone();
    let lock = tokio::spawn(async move {
        locker
            .lock_resumable_session(&session.id.to_string(), BUCKET)
            .await
            .map(drop)
    });
    settle().await;
    assert!(!lock.is_finished());
    drop(exclusive);
    joined(lock).await.unwrap();
    catalog.close().await.unwrap();
}

/// Every uploader keeps a lease at each point where another task can run, so the gate would never
/// open by chance: only the reservation lets the job in. The readers queue for SQL, which must
/// not hold the gate shut either.
#[tokio::test]
async fn a_steady_stream_of_new_work_cannot_starve_an_exclusive_job() {
    let (_dir, catalog, admin) = setup().await;
    let stop = Arc::new(AtomicBool::new(false));
    let mut load = Vec::new();
    for _ in 0..8 {
        let (uploader, stopped) = (catalog.clone(), stop.clone());
        load.push(tokio::spawn(async move {
            while !stopped.load(Ordering::Acquire) {
                let permit = uploader.wait_object_upload().await.unwrap();
                tokio::task::yield_now().await;
                drop(permit);
            }
        }));
        let (reader, stopped) = (catalog.clone(), stop.clone());
        load.push(tokio::spawn(async move {
            while !stopped.load(Ordering::Acquire) {
                reader.get_bucket(BUCKET, PERMISSIVE).await.unwrap();
            }
        }));
    }
    settle().await;
    admin
        .reset_until(TimeLimit::from_millis(10_000).deadline())
        .await
        .unwrap();
    stop.store(true, Ordering::Release);
    // The stream resumes once the job is done, and each worker then sees the stop.
    for worker in load {
        joined(worker).await;
    }
    catalog.close().await.unwrap();
}

/// A multi-threaded runtime lets a release land between a waiter's check and its sleep, which is
/// the window a lost wakeup would need. The spin moves the release across that window, from before
/// the waiters start to after they are asleep.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_waiter_misses_the_end_of_an_exclusive_job() {
    let (_dir, catalog, _admin) = setup().await;
    for round in 0..400 {
        let exclusive = catalog.wait_exclusive(LONG.deadline()).await.unwrap();
        let waiters: Vec<_> = (0..4).map(|_| spawn_new_upload(&catalog)).collect();
        for _ in 0..(round % 40) * 256 {
            std::hint::spin_loop();
        }
        drop(exclusive);
        for waiter in waiters {
            joined(waiter).await.unwrap();
        }
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn closing_fails_lease_waiters_at_once_while_an_exclusive_job_runs() {
    let (_dir, catalog, _admin) = setup().await;
    let exclusive = catalog.wait_exclusive(LONG.deadline()).await.unwrap();
    let new = spawn_new_upload(&catalog);
    let continuing = catalog.clone();
    let under_way =
        tokio::spawn(async move { continuing.continue_object_upload().await.map(drop) });
    settle().await;
    assert!(!new.is_finished());
    assert!(!under_way.is_finished());
    let closer = catalog.clone();
    let close = tokio::spawn(async move { closer.close().await });
    // The job still owns the execution permit, so close itself cannot finish yet.
    assert!(closed_upload(joined(new).await));
    assert!(closed_upload(joined(under_way).await));
    assert!(!close.is_finished());
    drop(exclusive);
    close.await.unwrap().unwrap();
}

#[tokio::test]
async fn closing_ends_a_draining_job_and_the_callers_lined_up_behind_it() {
    let (_dir, catalog, admin) = setup().await;
    let upload = catalog.admit_object_upload().unwrap();
    let reset = spawn_reset(&admin);
    settle().await;
    let new = spawn_new_upload(&catalog);
    settle().await;
    assert!(!reset.is_finished());
    assert!(!new.is_finished());
    let closer = catalog.clone();
    let close = tokio::spawn(async move { closer.close().await });
    assert!(closed_upload(joined(new).await));
    assert_eq!(joined(reset).await, Err(AdminError::Closed));
    // Close waits for the admitted upload rather than cutting it off.
    assert!(!close.is_finished());
    drop(upload);
    close.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_closed_catalog_refuses_new_work_at_once_without_waiting_for_anything() {
    let (_dir, catalog, admin) = setup().await;
    catalog.close().await.unwrap();
    assert!(closed_upload(catalog.wait_object_upload().await.map(drop)));
    assert_eq!(
        catalog
            .admin_activity(Work::New, LONG.deadline())
            .await
            .map(drop)
            .unwrap_err(),
        AdminError::Closed
    );
    assert!(admin.seed_batch().await.is_err());
}

#[tokio::test]
async fn a_dropped_wait_leaves_nothing_behind() {
    let (_dir, catalog, _admin) = setup().await;
    let exclusive = catalog.wait_exclusive(LONG.deadline()).await.unwrap();
    let waiter = spawn_new_upload(&catalog);
    settle().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    drop(exclusive);
    assert_eq!(catalog.active_uploads(), 0);
    drop(catalog.wait_object_upload().await.unwrap());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn cancelling_a_draining_job_lets_the_callers_behind_it_through() {
    let (_dir, catalog, admin) = setup().await;
    let upload = catalog.admit_object_upload().unwrap();
    let reset = spawn_reset(&admin);
    settle().await;
    let new = spawn_new_upload(&catalog);
    settle().await;
    assert!(!new.is_finished());
    reset.abort();
    assert!(reset.await.unwrap_err().is_cancelled());
    joined(new).await.unwrap();
    assert!(!catalog.0.activity.is_reserved());
    drop(upload);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_job_that_gives_up_lets_the_callers_behind_it_through() {
    let (_dir, catalog, admin) = setup().await;
    let upload = catalog.admit_object_upload().unwrap();
    let reset = admin.reset_until(TimeLimit::from_millis(50).deadline());
    let new = spawn_new_upload(&catalog);
    assert!(matches!(reset.await, Err(AdminError::Busy { .. })));
    joined(new).await.unwrap();
    drop(upload);
    catalog.close().await.unwrap();
}

/// A reservation that stopped these would wait for the very lease it is draining, so the job
/// would only ever end at its deadline.
#[tokio::test]
async fn work_under_way_finishes_ahead_of_a_draining_job() {
    let (_dir, catalog, admin) = setup().await;
    let upload = catalog.admit_object_upload().unwrap();
    let reset = spawn_reset(&admin);
    settle().await;
    assert!(catalog.0.activity.is_reserved());

    drop(catalog.admit_object_upload().unwrap());
    drop(catalog.continue_object_upload().await.unwrap());
    let record = catalog
        .put_object_prechecked(
            permissive_upload(BUCKET, "value", 16),
            one_chunk(b"value"),
            upload,
        )
        .await
        .unwrap();
    assert_eq!(record.size, 5);
    joined(reset).await.unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_held_source_finishes_its_transfer_setup_ahead_of_a_draining_job() {
    let (_dir, catalog, admin) = setup().await;
    catalog
        .put_object(permissive_upload(BUCKET, "value", 16), one_chunk(b"value"))
        .await
        .unwrap();
    let source = catalog
        .resolve_derived_source(live(BUCKET, "value"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    let reset = spawn_reset(&admin);
    settle().await;
    assert!(catalog.0.activity.is_reserved());
    catalog
        .begin_derived_ephemeral(vec![source.whole()], 16)
        .await
        .unwrap();
    joined(reset).await.unwrap();
    catalog.close().await.unwrap();
}

/// Maintenance skips its turn on a running job. A draining job must not make it skip, because
/// nothing would wake it to try again when the job gives up.
#[tokio::test]
async fn the_form_that_never_waits_reports_a_running_job_but_ignores_a_draining_one() {
    let (_dir, catalog, admin) = setup().await;
    let upload = catalog.admit_object_upload().unwrap();
    let reset = spawn_reset(&admin);
    settle().await;
    drop(catalog.activity().unwrap());
    drop(upload);
    joined(reset).await.unwrap();
    let exclusive = catalog.wait_exclusive(LONG.deadline()).await.unwrap();
    assert!(catalog.activity().is_err());
    drop(exclusive);
    drop(catalog.activity().unwrap());
    catalog.close().await.unwrap();
}

/// The wait used to end after one second no matter how long the caller allowed, so any upload
/// that outlasted it failed the reset.
#[tokio::test(start_paused = true)]
async fn a_reset_waits_for_as_long_as_its_deadline_allows() {
    let catalog = Catalog::open_memory_for_server(
        1024,
        1024 * 1024,
        Arc::new(SystemClock),
        LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    let admin = catalog.local_admin().unwrap();
    let upload = catalog.admit_object_upload().unwrap();
    let mut reset = std::pin::pin!(admin.reset_until(TimeLimit::from_millis(3_600_000).deadline()));
    assert!(
        tokio::time::timeout(Duration::from_secs(120), &mut reset)
            .await
            .is_err()
    );
    drop(upload);
    reset.await.unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_disabled_catalog_wait_leaves_administrative_waits_unlimited() {
    let catalog = Catalog::open_memory_for_server(
        1024,
        1024 * 1024,
        Arc::new(SystemClock),
        LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    catalog.apply_session_limits(&Config {
        catalog_wait_timeout: TimeLimit::DISABLED,
        ..Default::default()
    });
    let admin = catalog.local_admin().unwrap();
    let name = BucketName::new(BUCKET).unwrap();
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let mut lookup = std::pin::pin!(admin.get_bucket(&name));
    // Past the embedded default of 30 seconds and the server default of 120.
    assert!(
        tokio::time::timeout(Duration::from_secs(600), &mut lookup)
            .await
            .is_err()
    );
    drop(execution);
    assert_eq!(lookup.await.unwrap(), None);
    catalog.close().await.unwrap();
}

/// The gate shut as an exclusive job would shut it, with an execution permit of its own: the SQL
/// job in front of the gate still runs, as it does when the job takes over right after that SQL.
fn exclusive_job_after_the_next_sql(catalog: &Catalog) -> ExclusiveGuard {
    ExclusiveGuard {
        _gate: catalog.0.activity.lock.clone().try_write_owned().unwrap(),
        _reservation: catalog.0.activity.reserve(),
        _execution: Arc::new(tokio::sync::Semaphore::new(1))
            .try_acquire_owned()
            .unwrap(),
    }
}

#[tokio::test]
async fn a_copy_token_waits_for_an_exclusive_job_after_reading_its_source() {
    let (_dir, catalog, _admin) = setup().await;
    let exclusive = exclusive_job_after_the_next_sql(&catalog);
    let copy = tokio::spawn({
        let catalog = catalog.clone();
        async move {
            catalog
                .prepare_copy_replay(
                    BUCKET,
                    "target",
                    Default::default(),
                    &BucketAccess::Permissive,
                    crate::storage::catalog::upload_replay::CopyReplaySource {
                        bucket: BUCKET,
                        name: "source",
                        generation: Some(1),
                        access: &BucketAccess::Permissive,
                    },
                    crate::storage::catalog::upload_replay::ReplayInvocation {
                        token: Some("token"),
                        request: "copy",
                    },
                )
                .await
                .map(drop)
        }
    });
    settle().await;
    assert!(!copy.is_finished());
    drop(exclusive);
    joined(copy).await.unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn maintenance_skips_a_writer_while_an_exclusive_job_runs() {
    let (_dir, catalog, _admin) = setup().await;
    let exclusive = exclusive_job_after_the_next_sql(&catalog);
    let claim = catalog.claim_writer(
        crate::storage::catalog::resumable::writers::WriterKey::Resumable(
            "0".repeat(64).parse().unwrap(),
        ),
        crate::storage::catalog::resumable::writers::KeyOrigin::Catalog,
    );
    assert!(matches!(
        claim,
        Err(crate::storage::catalog::resumable::writers::ClaimError::Busy)
    ));
    drop(exclusive);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn management_calls_give_up_at_the_configured_catalog_wait() {
    let (_dir, catalog, admin) = setup().await;
    catalog.apply_session_limits(&Config {
        catalog_wait_timeout: TimeLimit::from_millis(50),
        ..Default::default()
    });
    let name = BucketName::new(BUCKET).unwrap();
    let exclusive = catalog
        .wait_exclusive(TimeLimit::from_millis(10_000).deadline())
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(10), admin.get_bucket(&name))
            .await
            .unwrap(),
        Err(AdminError::Busy { .. })
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(10), admin.seed_batch())
            .await
            .unwrap(),
        Err(AdminError::Busy { .. })
    ));
    drop(exclusive);
    admin.get_bucket(&name).await.unwrap();
    catalog.close().await.unwrap();
}
