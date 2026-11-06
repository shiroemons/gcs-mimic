//! Expiry of soft-deleted objects driven by the maintenance scheduler and an injected clock.

use super::*;
use crate::{
    bucket::SoftDeletePolicy,
    clock::{Clock, ManualClock},
    storage::{
        catalog::fault::{self, Crash, Fault, Step},
        test_fixtures::{
            PERMISSIVE, PERMISSIVE_RESTORE, body, bucket, live, permissive_upload, version,
        },
    },
    test_support::{TestDir, eventually_async},
};
use bytes::Bytes;
use std::{
    fs, io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};

const START: Duration = Duration::from_secs(1000);

fn start_time() -> SystemTime {
    UNIX_EPOCH + START
}

async fn open_disk(data: &Path) -> (Catalog, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(start_time()));
    let catalog = Catalog::open_with_clock(data, clock.clone()).await.unwrap();
    (catalog, clock)
}

async fn open_memory() -> (Catalog, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(start_time()));
    let catalog = Catalog::open_memory_for_server(
        64 * 1024 * 1024,
        16 * 1024 * 1024,
        clock.clone(),
        crate::storage::catalog::admin::LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    (catalog, clock)
}

async fn retaining_bucket(catalog: &Catalog, name: &str, seconds: u64) {
    let mut record = bucket(name, "project");
    record.soft_delete_policy = Some(SoftDeletePolicy {
        retention_duration_seconds: seconds,
        effective_time: catalog.0.clock.now(),
    });
    catalog.insert_bucket(record).await.unwrap();
}

async fn put(catalog: &Catalog, bucket: &str, name: &str) -> i64 {
    catalog
        .put_object(
            permissive_upload(bucket, name, 1000),
            body(vec![Bytes::from_static(b"retained")]),
        )
        .await
        .unwrap()
        .generation
}

async fn soft_delete(catalog: &Catalog, bucket: &str, name: &str) -> i64 {
    let generation = put(catalog, bucket, name).await;
    catalog
        .delete_object(
            version(bucket, name, generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    generation
}

fn blob_files(data: &Path) -> usize {
    fs::read_dir(data.join("blobs"))
        .unwrap()
        .map(|shard| fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum()
}

async fn count(catalog: &Catalog, query: &'static str) -> i64 {
    catalog
        .run::<_, CatalogError>(move |connection| Ok(sql::scalar(connection, query, [])?))
        .await
        .unwrap()
}

async fn soft_deleted_rows(catalog: &Catalog) -> i64 {
    count(
        catalog,
        "SELECT count(*) FROM objects WHERE soft_delete_time IS NOT NULL",
    )
    .await
}

async fn journal_rows(catalog: &Catalog) -> i64 {
    count(catalog, "SELECT count(*) FROM pending_blob_deletions").await
}

async fn readable(catalog: &Catalog, bucket: &str, name: &str, generation: i64) -> bool {
    catalog
        .get_soft_deleted_object(
            ObjectTarget::new(bucket, name, generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .is_ok()
}

fn memory_blobs(catalog: &Catalog) -> usize {
    match &catalog.0.backend {
        super::super::BlobBackend::Memory(store) => store.blob_count(),
        super::super::BlobBackend::Disk(_) => unreachable!("the catalog was opened in memory"),
    }
}

#[tokio::test]
async fn disk_body_stays_until_the_hard_delete_time_and_is_then_removed() {
    let data = TestDir::new();
    let (catalog, clock) = open_disk(&data.0).await;
    retaining_bucket(&catalog, "bucket", 5).await;
    let generation = soft_delete(&catalog, "bucket", "object").await;
    assert_eq!(blob_files(&data.0), 1);

    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(start_time() + Duration::from_secs(5)),
        "the pass reports the hard delete time as its next deadline"
    );
    assert_eq!(blob_files(&data.0), 1);

    clock.advance(Duration::from_micros(4_999_999));
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(start_time() + Duration::from_secs(5))
    );
    assert_eq!(blob_files(&data.0), 1);
    assert!(readable(&catalog, "bucket", "object", generation).await);

    // The first microsecond at which reads hide the row is also the first at which it is purged.
    clock.advance(Duration::from_micros(1));
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert_eq!(blob_files(&data.0), 0);
    assert_eq!(soft_deleted_rows(&catalog).await, 0);
    assert_eq!(journal_rows(&catalog).await, 0);
    assert!(!readable(&catalog, "bucket", "object", generation).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn memory_body_stays_until_the_hard_delete_time_and_is_then_removed() {
    let (catalog, clock) = open_memory().await;
    retaining_bucket(&catalog, "bucket", 5).await;
    let generation = soft_delete(&catalog, "bucket", "object").await;
    assert_eq!(memory_blobs(&catalog), 1);
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(start_time() + Duration::from_secs(5))
    );
    assert_eq!(memory_blobs(&catalog), 1);
    clock.advance(Duration::from_secs(5));
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert_eq!(memory_blobs(&catalog), 0);
    assert_eq!(journal_rows(&catalog).await, 0);
    assert!(!readable(&catalog, "bucket", "object", generation).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn the_longest_retention_gives_a_representable_hard_delete_time() {
    let data = TestDir::new();
    let (catalog, _clock) = open_disk(&data.0).await;
    let longest = crate::object::retention::MAX_RETENTION;
    retaining_bucket(&catalog, "bucket", longest.as_secs()).await;
    let generation = soft_delete(&catalog, "bucket", "object").await;
    let record = catalog
        .get_soft_deleted_object(
            ObjectTarget::new("bucket", "object", generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(record.hard_delete_time, Some(start_time() + longest));
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(start_time() + longest)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_restored_version_survives_the_hard_delete_time_of_its_source() {
    let data = TestDir::new();
    let (catalog, clock) = open_disk(&data.0).await;
    retaining_bucket(&catalog, "bucket", 5).await;
    let generation = soft_delete(&catalog, "bucket", "object").await;
    catalog
        .restore_object(
            ObjectTarget::new("bucket", "object", generation),
            Default::default(),
            PERMISSIVE_RESTORE,
        )
        .await
        .unwrap();
    assert_eq!(blob_files(&data.0), 2);
    clock.advance(Duration::from_secs(5));
    catalog.maintain_resumable_sessions().await.unwrap();
    assert_eq!(blob_files(&data.0), 1);
    catalog
        .open_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn the_next_deadline_is_the_earliest_across_buckets_and_soft_deleted_versions() {
    let data = TestDir::new();
    let (catalog, clock) = open_disk(&data.0).await;
    retaining_bucket(&catalog, "slow", 30).await;
    retaining_bucket(&catalog, "fast", 5).await;
    retaining_bucket(&catalog, "retired", 10).await;
    soft_delete(&catalog, "slow", "object").await;
    soft_delete(&catalog, "fast", "object").await;
    soft_delete(&catalog, "retired", "object").await;
    // The retired bucket's object moves to an internal key that no live bucket can name.
    catalog
        .delete_bucket("retired", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    let next = |seconds| Some(start_time() + Duration::from_secs(seconds));
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        next(5)
    );
    assert_eq!(blob_files(&data.0), 3);

    clock.advance(Duration::from_secs(5));
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        next(10)
    );
    assert_eq!(blob_files(&data.0), 2);

    // The version's object and the version itself expire together here.
    clock.advance(Duration::from_secs(5));
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        next(30)
    );
    assert_eq!(blob_files(&data.0), 1);
    catalog.close().await.unwrap();
}

/// More rows than one purge batch (100) and one reclamation page (128) hold.
const BACKLOG: i64 = 301;

async fn assert_backlog_is_purged(catalog: &Catalog, clock: &ManualClock) {
    retaining_bucket(catalog, "bucket", 5).await;
    for index in 0..BACKLOG {
        soft_delete(catalog, "bucket", &format!("object-{index}")).await;
    }
    assert_eq!(soft_deleted_rows(catalog).await, BACKLOG);
    clock.advance(Duration::from_secs(5));
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert_eq!(soft_deleted_rows(catalog).await, 0);
    assert_eq!(journal_rows(catalog).await, 0);
}

#[tokio::test]
async fn one_pass_clears_a_disk_backlog_beyond_every_batch_limit() {
    let data = TestDir::new();
    let (catalog, clock) = open_disk(&data.0).await;
    assert_backlog_is_purged(&catalog, &clock).await;
    assert_eq!(blob_files(&data.0), 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn one_pass_clears_a_memory_backlog_beyond_every_batch_limit() {
    let (catalog, clock) = open_memory().await;
    assert_backlog_is_purged(&catalog, &clock).await;
    assert_eq!(memory_blobs(&catalog), 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn one_pass_drains_a_soft_deleted_bucket_holding_more_than_a_reclamation_page() {
    let data = TestDir::new();
    let (catalog, clock) = open_disk(&data.0).await;
    retaining_bucket(&catalog, "retired", 5).await;
    for index in 0..BACKLOG {
        soft_delete(&catalog, "retired", &format!("object-{index}")).await;
    }
    catalog
        .delete_bucket("retired", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    clock.advance(Duration::from_secs(5));
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert_eq!(soft_deleted_rows(&catalog).await, 0);
    assert_eq!(journal_rows(&catalog).await, 0);
    assert_eq!(blob_files(&data.0), 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn expiry_reports_no_change_or_notification_for_an_already_reported_deletion() {
    let data = TestDir::new();
    let (catalog, clock) = open_disk(&data.0).await;
    retaining_bucket(&catalog, "bucket", 5).await;
    soft_delete(&catalog, "bucket", "object").await;
    const CHANGES: &str = "SELECT sequence FROM change_journal_state";
    const OUTBOX: &str = "SELECT count(*) FROM notification_outbox";
    let (changes, outbox) = (
        count(&catalog, CHANGES).await,
        count(&catalog, OUTBOX).await,
    );
    clock.advance(Duration::from_secs(5));
    catalog.maintain_resumable_sessions().await.unwrap();
    assert_eq!(soft_deleted_rows(&catalog).await, 0);
    assert_eq!(count(&catalog, CHANGES).await, changes);
    assert_eq!(count(&catalog, OUTBOX).await, outbox);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn the_expiry_lookup_seeks_the_partial_indexes_instead_of_scanning_rows() {
    let data = TestDir::new();
    let (catalog, _clock) = open_disk(&data.0).await;
    let plan = catalog
        .run::<_, CatalogError>(|connection| {
            let mut statement = connection
                .prepare(&format!("EXPLAIN QUERY PLAN {EARLIEST_EXPIRY}"))
                .unwrap();
            let rows = statement
                .query_map([], |row| row.get::<_, String>(3))
                .unwrap();
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>().unwrap())
        })
        .await
        .unwrap();
    assert!(
        plan.iter().any(|step| step.contains("objects_soft_expiry")),
        "{plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step.starts_with("SCAN objects")),
        "{plan:?}"
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_failed_blob_removal_stays_journaled_and_the_next_pass_finishes_it() {
    let data = TestDir::new();
    let (catalog, clock) = open_disk(&data.0).await;
    retaining_bucket(&catalog, "bucket", 5).await;
    soft_delete(&catalog, "bucket", "object").await;
    clock.advance(Duration::from_secs(5));
    fault::arm(
        &data.0,
        Step::ReclaimUnlink,
        0,
        Fault::Io(io::ErrorKind::Other),
    );
    catalog.maintain_resumable_sessions().await.unwrap_err();
    assert!(catalog.resumable_maintenance_error().is_some());
    assert_eq!(soft_deleted_rows(&catalog).await, 0);
    assert_eq!(journal_rows(&catalog).await, 1);
    assert_eq!(blob_files(&data.0), 1);

    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert!(catalog.resumable_maintenance_error().is_none());
    assert_eq!(journal_rows(&catalog).await, 0);
    assert_eq!(blob_files(&data.0), 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_failed_expiry_commit_keeps_the_row_and_the_next_pass_purges_it() {
    let data = TestDir::new();
    let (catalog, clock) = open_disk(&data.0).await;
    retaining_bucket(&catalog, "bucket", 5).await;
    soft_delete(&catalog, "bucket", "object").await;
    clock.advance(Duration::from_secs(5));
    fault::arm(
        &data.0,
        Step::ExpiryCommit,
        0,
        Fault::Io(io::ErrorKind::Other),
    );
    catalog.maintain_resumable_sessions().await.unwrap_err();
    assert_eq!(soft_deleted_rows(&catalog).await, 1);
    assert_eq!(journal_rows(&catalog).await, 0);
    assert_eq!(blob_files(&data.0), 1);

    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert_eq!(soft_deleted_rows(&catalog).await, 0);
    assert_eq!(blob_files(&data.0), 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn crash_at_every_expiry_boundary_recovers_without_losing_the_deletion() {
    for crash in Crash::ALL {
        for step in [
            Step::ExpiryCommit,
            Step::ReclaimUnlink,
            #[cfg(unix)]
            Step::ReclaimSync,
            Step::ReclaimForget,
        ] {
            let data = TestDir::new();
            let snapshot = TestDir::new();
            let (catalog, clock) = open_disk(&data.0).await;
            retaining_bucket(&catalog, "bucket", 5).await;
            soft_delete(&catalog, "bucket", "object").await;
            clock.advance(Duration::from_secs(5));
            fault::arm(&data.0, step, 0, crash.fault(snapshot.0.clone()));
            assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
            assert_eq!(fault::pending(&data.0), 0, "{crash:?} {step:?}");
            assert_eq!(blob_files(&data.0), 0, "{crash:?} {step:?}");
            catalog.close().await.unwrap();

            let recovered = Catalog::open_with_clock(&snapshot.0, clock.clone())
                .await
                .unwrap();
            // Recovery finishes a committed deletion; an uncommitted one is still due and the
            // first pass purges it. Either way the expired bytes are gone.
            assert_eq!(journal_rows(&recovered).await, 0, "{crash:?} {step:?}");
            assert_eq!(
                recovered.maintain_resumable_sessions().await.unwrap(),
                None,
                "{crash:?} {step:?}"
            );
            assert_eq!(soft_deleted_rows(&recovered).await, 0, "{crash:?} {step:?}");
            assert_eq!(blob_files(&snapshot.0), 0, "{crash:?} {step:?}");
            recovered.close().await.unwrap();
        }
    }
}

async fn expect_wake(catalog: &Catalog, operation: impl AsyncFnOnce()) {
    let wake = catalog.0.maintenance_wake.clone();
    let notified = wake.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    operation().await;
    tokio::time::timeout(Duration::from_secs(5), notified)
        .await
        .expect("a new soft delete must wake the maintenance scheduler");
}

#[tokio::test]
async fn delete_overwrite_move_and_restore_wake_the_scheduler() {
    let data = TestDir::new();
    let (catalog, _clock) = open_disk(&data.0).await;
    retaining_bucket(&catalog, "bucket", 5).await;
    let generation = put(&catalog, "bucket", "deleted").await;
    expect_wake(&catalog, async || {
        catalog
            .delete_object(
                version("bucket", "deleted", generation),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
    })
    .await;

    put(&catalog, "bucket", "overwritten").await;
    expect_wake(&catalog, async || {
        put(&catalog, "bucket", "overwritten").await;
    })
    .await;

    put(&catalog, "bucket", "source").await;
    put(&catalog, "bucket", "destination").await;
    expect_wake(&catalog, async || {
        catalog
            .move_object(
                super::super::object_move::MoveNames {
                    bucket: "bucket".into(),
                    source: "source".into(),
                    destination: "destination".into(),
                },
                Default::default(),
                super::super::object_move::MoveAccess {
                    source_read: PERMISSIVE,
                    source_delete: PERMISSIVE,
                    destination_create: PERMISSIVE,
                },
            )
            .await
            .unwrap();
    })
    .await;

    // Restoring over a live version retires it like any other replacement.
    let restored = soft_delete(&catalog, "bucket", "restored").await;
    put(&catalog, "bucket", "restored").await;
    expect_wake(&catalog, async || {
        catalog
            .restore_object(
                ObjectTarget::new("bucket", "restored", restored),
                Default::default(),
                PERMISSIVE_RESTORE,
            )
            .await
            .unwrap();
    })
    .await;
    catalog.close().await.unwrap();
}

/// Counts readings so a test can tell when a maintenance pass has reached its timer.
struct CountingClock {
    time: ManualClock,
    readings: AtomicUsize,
}

impl Clock for CountingClock {
    fn now(&self) -> SystemTime {
        self.readings.fetch_add(1, Ordering::SeqCst);
        self.time.now()
    }
}

#[tokio::test]
async fn a_shorter_retention_wakes_a_scheduler_sleeping_until_a_later_deadline() {
    let data = TestDir::new();
    let clock = Arc::new(CountingClock {
        time: ManualClock::new(start_time()),
        readings: AtomicUsize::new(0),
    });
    let catalog = Catalog::open_with_clock(&data.0, clock.clone())
        .await
        .unwrap();
    retaining_bucket(&catalog, "weekly", 604_800).await;
    retaining_bucket(&catalog, "brief", 1).await;
    soft_delete(&catalog, "weekly", "object").await;
    // A pass reads the clock when it starts and again to turn its deadline into a delay, so two
    // readings put the scheduler on the week-long timer with nothing else due to wake it.
    let before = clock.readings.load(Ordering::SeqCst);
    catalog.start_resumable_maintenance();
    eventually_async("the scheduler to reach its timer", || async {
        clock.readings.load(Ordering::SeqCst) >= before + 2
    })
    .await;

    soft_delete(&catalog, "brief", "object").await;
    // Whichever pass observes the row as due purges it, within one real second of its deadline
    // at worst. A scheduler left on the week-long timer never does.
    clock.time.advance(Duration::from_secs(1));
    eventually_async("the brief retention to expire", || async {
        // The journal row outlives the unlink until the removal is durable.
        blob_files(&data.0) == 1 && journal_rows(&catalog).await == 0
    })
    .await;
    assert_eq!(soft_deleted_rows(&catalog).await, 1);
    catalog.close().await.unwrap();
}
