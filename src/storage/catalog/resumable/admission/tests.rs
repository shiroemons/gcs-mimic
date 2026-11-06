use super::*;
use crate::{
    bucket::BucketAccess,
    clock::ManualClock,
    config::Config,
    object::ObjectError,
    storage::catalog::{
        ResumableError,
        test_fixtures::{bucket, fill_cancelled_sessions},
    },
    test_support::TestDir,
};
use std::{num::NonZeroU32, sync::Arc, time::UNIX_EPOCH};

const WINDOW: i64 = MAX_TERMINAL_SESSIONS;

async fn open(dir: &TestDir, clock: &Arc<ManualClock>) -> Catalog {
    Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap()
}

async fn open_with_bucket(dir: &TestDir, clock: &Arc<ManualClock>) -> Catalog {
    let catalog = open(dir, clock).await;
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    catalog
}

/// Finished records must not outgrow a RAM catalog's budget: past it every catalog write fails
/// until the records expire days later. Their metadata decides the size, so counting them is not
/// enough: a few large records fill the budget as surely as many small ones.
#[tokio::test]
async fn finished_sessions_stay_inside_a_small_memory_catalog_budget() {
    const BUDGET: u64 = 1024 * 1024;
    for (metadata_bytes, sessions) in [(0, 1_500), (30_000, 300)] {
        let catalog = Catalog::open_memory_for_server(
            64 * 1024 * 1024,
            BUDGET,
            Arc::new(crate::clock::SystemClock),
            crate::storage::admin::LocalAdmin::Disabled,
        )
        .await
        .unwrap();
        catalog.apply_session_limits(&Config {
            backend: crate::config::Backend::Memory,
            memory_catalog_max_bytes: BUDGET,
            ..Default::default()
        });
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        for index in 0..sessions {
            let mut options = crate::storage::ResumableOptions::new(BucketAccess::Permissive);
            options.metadata.cache_control = Some(Some("x".repeat(metadata_bytes)));
            let session = catalog
                .create_resumable_session(
                    "bucket",
                    &format!("some/typical/object/name-{index}.bin"),
                    options,
                    None,
                )
                .await
                .unwrap();
            catalog
                .cancel_resumable_session(&session.id, "bucket")
                .await
                .unwrap();
        }
        catalog.close().await.unwrap();
    }
}

#[test]
fn trimming_follows_whichever_of_the_count_and_the_bytes_is_over() {
    let window = MAX_TERMINAL_SESSIONS;
    assert_eq!(finished_excess(window, 1, None), 0);
    assert_eq!(finished_excess(window + 3, 1, None), 3);
    assert_eq!(finished_excess(10, 1_000, Some(1_000)), 0);
    // Ten records of 100 bytes, 250 over: three go, rounding up.
    assert_eq!(finished_excess(10, 1_250, Some(1_000)), 3);
    assert_eq!(finished_excess(window + 3, 1, Some(1_000)), 3);
    assert_eq!(finished_excess(0, 0, Some(0)), 0);
}

async fn create(catalog: &Catalog, clock: &ManualClock) -> Result<String, ResumableError> {
    // Distinct initiation times give trimming a deterministic oldest-first order.
    clock.advance(std::time::Duration::from_millis(1));
    catalog
        .create_resumable_session(
            "bucket",
            "object",
            crate::storage::ResumableOptions::new(BucketAccess::Permissive),
            None,
        )
        .await
        .map(|session| session.id)
}

/// The counts admission reads and the counts of the table itself.
async fn counts(catalog: &Catalog) -> ((i64, i64), (i64, i64)) {
    catalog
        .run::<_, CatalogError>(|connection| {
            let pair = |row: &rusqlite::Row<'_>| Ok((row.get(0)?, row.get(1)?));
            Ok((
                sql::row(
                    connection,
                    "SELECT active, terminal FROM temp.resumable_session_counts",
                    [],
                    pair,
                )?,
                sql::row(
                    connection,
                    "SELECT coalesce(sum(state = 'ACTIVE'), 0), coalesce(sum(state != 'ACTIVE'), 0) FROM resumable_sessions",
                    [],
                    pair,
                )?,
            ))
        })
        .await
        .unwrap()
}

#[track_caller]
fn assert_exact((read, actual): ((i64, i64), (i64, i64)), expected: (i64, i64)) {
    assert_eq!(read, actual, "the kept counts drifted from the table");
    assert_eq!(read, expected);
}

#[track_caller]
fn assert_refused(created: Result<String, ResumableError>) {
    assert!(
        matches!(
            &created,
            Err(ResumableError::Object(ObjectError::Unavailable(cause)))
                if cause.failure() == Some(BackendFailure::RetainedSessionLimit)
        ),
        "{created:?}"
    );
}

/// The ID `fill_cancelled_sessions` gives its `n`th row.
fn filled_id(n: i64) -> String {
    format!("{n:064x}")
}

#[tokio::test]
async fn the_oldest_finished_record_past_the_window_goes_first() {
    let (dir, clock) = (TestDir::new(), Arc::new(ManualClock::new(UNIX_EPOCH)));
    let catalog = open_with_bucket(&dir, &clock).await;
    let oldest = create(&catalog, &clock).await.unwrap();
    let kept = create(&catalog, &clock).await.unwrap();
    fill_cancelled_sessions(&catalog, WINDOW).await;
    assert_exact(counts(&catalog).await, (2, WINDOW));
    // At the window nothing is trimmed yet.
    create(&catalog, &clock).await.unwrap();
    assert_eq!(
        catalog.resumable_session(&filled_id(1), "bucket").await,
        Err(ResumableError::Cancelled)
    );
    assert_eq!(
        catalog
            .cancel_resumable_session(&oldest, "bucket")
            .await
            .map_err(|_| ()),
        Ok(())
    );
    assert_exact(counts(&catalog).await, (2, WINDOW + 1));
    create(&catalog, &clock).await.unwrap();
    assert_exact(counts(&catalog).await, (3, WINDOW));
    assert_eq!(
        catalog.resumable_session(&oldest, "bucket").await,
        Err(ResumableError::NotFound)
    );
    assert!(catalog.resumable_session(&kept, "bucket").await.is_ok());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_held_record_is_stepped_over_and_the_next_oldest_goes_instead() {
    let (dir, clock) = (TestDir::new(), Arc::new(ManualClock::new(UNIX_EPOCH)));
    let catalog = open_with_bucket(&dir, &clock).await;
    create(&catalog, &clock).await.unwrap();
    fill_cancelled_sessions(&catalog, WINDOW + 1).await;
    let held = catalog.try_resumable_writer(&filled_id(1)).unwrap();
    create(&catalog, &clock).await.unwrap();
    assert_exact(counts(&catalog).await, (2, WINDOW));
    assert_eq!(
        catalog.resumable_session(&filled_id(1), "bucket").await,
        Err(ResumableError::Cancelled)
    );
    assert_eq!(
        catalog.resumable_session(&filled_id(2), "bucket").await,
        Err(ResumableError::NotFound)
    );
    drop(held);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn records_awaiting_cleanup_stay_while_creation_succeeds_and_the_excess_carries_over() {
    let (dir, clock) = (TestDir::new(), Arc::new(ManualClock::new(UNIX_EPOCH)));
    let catalog = open_with_bucket(&dir, &clock).await;
    create(&catalog, &clock).await.unwrap();
    let excess = 3;
    fill_cancelled_sessions(&catalog, WINDOW + excess).await;
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec(
                connection,
                "INSERT INTO pending_blob_deletions SELECT blob_id FROM resumable_sessions WHERE state = 'CANCELLED'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    // Maintenance still needs these rows, and the client has no way to help: it must not fail.
    create(&catalog, &clock).await.unwrap();
    assert_exact(counts(&catalog).await, (2, WINDOW + excess));
    assert_eq!(
        catalog.resumable_session(&filled_id(1), "bucket").await,
        Err(ResumableError::Cancelled)
    );
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec(connection, "DELETE FROM pending_blob_deletions", [])?;
            Ok(())
        })
        .await
        .unwrap();
    create(&catalog, &clock).await.unwrap();
    assert_exact(counts(&catalog).await, (3, WINDOW));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn one_creation_trims_a_bounded_batch_and_later_ones_finish_the_rest() {
    let (dir, clock) = (TestDir::new(), Arc::new(ManualClock::new(UNIX_EPOCH)));
    let catalog = open_with_bucket(&dir, &clock).await;
    create(&catalog, &clock).await.unwrap();
    let batch = i64::try_from(MAX_TRIMMED_PER_CREATION).unwrap();
    fill_cancelled_sessions(&catalog, WINDOW + batch + 1).await;
    create(&catalog, &clock).await.unwrap();
    assert_exact(counts(&catalog).await, (2, WINDOW + 1));
    create(&catalog, &clock).await.unwrap();
    assert_exact(counts(&catalog).await, (3, WINDOW));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn kept_counts_follow_every_transition_and_are_rebuilt_on_open() {
    let (dir, clock) = (TestDir::new(), Arc::new(ManualClock::new(UNIX_EPOCH)));
    let catalog = open_with_bucket(&dir, &clock).await;
    let first = create(&catalog, &clock).await.unwrap();
    let second = create(&catalog, &clock).await.unwrap();
    assert_exact(counts(&catalog).await, (2, 0));
    catalog
        .cancel_resumable_session(&first, "bucket")
        .await
        .unwrap();
    assert_exact(counts(&catalog).await, (1, 1));
    // A rolled-back change leaves them alone.
    let rolled_back = catalog
        .run::<_, CatalogError>(|connection| {
            let transaction = connection.transaction()?;
            sql::exec(
                &transaction,
                "UPDATE resumable_sessions SET state = 'CANCELLED'",
                [],
            )?;
            let during: i64 = sql::scalar(
                &transaction,
                "SELECT terminal FROM temp.resumable_session_counts",
                [],
            )?;
            transaction.rollback()?;
            Ok(during)
        })
        .await
        .unwrap();
    assert_eq!(rolled_back, 2);
    assert_exact(counts(&catalog).await, (1, 1));
    catalog.close().await.unwrap();
    let catalog = open(&dir, &clock).await;
    assert_exact(counts(&catalog).await, (1, 1));
    catalog
        .run::<_, CatalogError>(move |connection| {
            sql::exec(
                connection,
                "DELETE FROM resumable_sessions WHERE id = ?1",
                [&second],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_exact(counts(&catalog).await, (0, 1));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_configured_limit_refuses_only_past_it_and_never_blocks_reopening() {
    let (dir, clock) = (TestDir::new(), Arc::new(ManualClock::new(UNIX_EPOCH)));
    let limited = |limit| Config {
        max_resumable_sessions: NonZeroU32::new(limit),
        ..Default::default()
    };
    let catalog = open_with_bucket(&dir, &clock).await;
    catalog.apply_session_limits(&limited(2));
    let first = create(&catalog, &clock).await.unwrap();
    create(&catalog, &clock).await.unwrap();
    assert_refused(create(&catalog, &clock).await);
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 2);
    catalog
        .cancel_resumable_session(&first, "bucket")
        .await
        .unwrap();
    create(&catalog, &clock).await.unwrap();
    catalog.close().await.unwrap();
    // More sessions than a lowered limit is a valid catalog: only new ones are refused.
    let catalog = open(&dir, &clock).await;
    catalog.apply_session_limits(&limited(1));
    assert_refused(create(&catalog, &clock).await);
    catalog.close().await.unwrap();
    let catalog = open(&dir, &clock).await;
    create(&catalog, &clock).await.unwrap();
    catalog.close().await.unwrap();
}
