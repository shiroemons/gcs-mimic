use super::*;
use crate::storage::test_fixtures::{PERMISSIVE, bucket, permissive_upload};
use crate::test_support::{TestDir, eventually_async};
use std::sync::mpsc;
use tokio::{sync::oneshot, task::JoinSet};

/// Far past the admission slots and the former limit that failed callers at the 33rd call.
const CALLERS: usize = 128;

async fn open(dir: &TestDir) -> Catalog {
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    catalog
}

/// Occupies the catalog thread until the returned sender fires, so every later call queues.
async fn block_thread(catalog: &Catalog) -> (mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
    let (release, held) = mpsc::channel::<()>();
    let (started, running) = oneshot::channel();
    let worker = catalog.clone();
    let blocker = tokio::spawn(async move {
        worker
            .run::<_, CatalogError>(move |_| {
                started.send(()).unwrap();
                held.recv().unwrap();
                Ok(())
            })
            .await
            .unwrap();
    });
    running.await.unwrap();
    (release, blocker)
}

#[tokio::test]
async fn callers_past_the_queue_wait_and_every_read_and_write_completes() {
    let dir = TestDir::new();
    let catalog = open(&dir).await;
    let (release, blocker) = block_thread(&catalog).await;
    let mut calls = JoinSet::new();
    for index in 0..CALLERS {
        let catalog = catalog.clone();
        calls.spawn(async move {
            // A read that stays on the serialized connection, which is what the thread blocks.
            if index % 2 == 0 {
                catalog.get_policy("bucket", PERMISSIVE).await.map(|_| ())
            } else {
                catalog
                    .insert_bucket(bucket(&format!("bucket-{index}"), "project"))
                    .await
                    .map(|_| ())
            }
        });
    }
    // One slot runs the blocker; the rest of the slots queue for execution.
    let expected = CALLERS - (MAX_IN_FLIGHT - 1);
    eventually_async("the callers past the slots to wait", || async {
        catalog.admission_waiters() == expected
    })
    .await;
    assert!(calls.try_join_next().is_none());
    release.send(()).unwrap();
    blocker.await.unwrap();
    while let Some(result) = calls.join_next().await {
        result.unwrap().unwrap();
    }
    assert_eq!(catalog.free_admission_slots(), MAX_IN_FLIGHT);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_dropped_caller_gives_up_its_place_without_running_or_consuming_a_slot() {
    let dir = TestDir::new();
    let catalog = open(&dir).await;
    let held = catalog.hold_admission();
    let worker = catalog.clone();
    let abandoned = tokio::spawn(async move {
        worker
            .insert_bucket(bucket("abandoned", "project"))
            .await
            .map(|_| ())
    });
    eventually_async("the caller to wait for admission", || async {
        catalog.admission_waiters() == 1
    })
    .await;
    abandoned.abort();
    assert!(abandoned.await.unwrap_err().is_cancelled());
    assert_eq!(catalog.admission_waiters(), 0);
    drop(held);
    assert_eq!(catalog.free_admission_slots(), MAX_IN_FLIGHT);
    assert_eq!(
        catalog.get_bucket("abandoned", PERMISSIVE).await.unwrap(),
        None
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn closing_fails_waiters_at_once_and_runs_the_calls_already_admitted() {
    let dir = TestDir::new();
    let catalog = open(&dir).await;
    let (release, blocker) = block_thread(&catalog).await;
    let mut calls = JoinSet::new();
    for _ in 0..CALLERS {
        let catalog = catalog.clone();
        // A read that stays on the serialized connection, which is what the thread blocks.
        calls.spawn(async move { catalog.get_policy("bucket", PERMISSIVE).await });
    }
    let waiting = CALLERS - (MAX_IN_FLIGHT - 1);
    eventually_async("the callers past the slots to wait", || async {
        catalog.admission_waiters() == waiting
    })
    .await;
    let worker = catalog.clone();
    let close = tokio::spawn(async move { worker.close().await });
    // The admission waiters finish while the thread is still blocked: nothing they could hold
    // keeps close from reaching the execution permit.
    let mut closed = 0;
    while closed < waiting {
        let result = calls.join_next().await.unwrap().unwrap();
        assert!(
            matches!(&result, Err(CatalogError::Unavailable(cause)) if *cause == "catalog is closed"),
            "{result:?}"
        );
        closed += 1;
    }
    assert!(!close.is_finished());
    release.send(()).unwrap();
    blocker.await.unwrap();
    let mut admitted = 0;
    while let Some(result) = calls.join_next().await {
        result.unwrap().unwrap();
        admitted += 1;
    }
    assert_eq!(admitted, MAX_IN_FLIGHT - 1);
    close.await.unwrap().unwrap();
}

/// A pass that failed here used to count against readiness, so a busy server could report itself
/// unhealthy for nothing more than being busy.
#[tokio::test]
async fn a_maintenance_pass_waits_its_turn_instead_of_failing() {
    let dir = TestDir::new();
    let catalog = open(&dir).await;
    let held = catalog.hold_admission();
    let worker = catalog.clone();
    let pass = tokio::spawn(async move { worker.maintain_resumable_sessions().await });
    eventually_async("the pass to wait for admission", || async {
        catalog.admission_waiters() == 1
    })
    .await;
    assert!(!pass.is_finished());
    drop(held);
    pass.await.unwrap().unwrap();
    assert_eq!(catalog.resumable_maintenance_error(), None);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn derived_transfers_wait_for_admission_while_the_queue_is_full() {
    let dir = TestDir::new();
    let catalog = open(&dir).await;
    catalog
        .put_object_prechecked(
            permissive_upload("bucket", "source", 5),
            crate::storage::test_fixtures::body(vec![bytes::Bytes::from_static(b"bytes")]),
            catalog.admit_object_upload().unwrap(),
        )
        .await
        .unwrap();
    let source = catalog
        .resolve_derived_source(
            crate::storage::test_fixtures::live("bucket", "source"),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let held = catalog.hold_admission();
    let worker = catalog.clone();
    let transfer = tokio::spawn(async move {
        worker
            .begin_derived_ephemeral(vec![source.whole()], 5)
            .await
    });
    eventually_async("the transfer to wait for admission", || async {
        catalog.admission_waiters() == 1
    })
    .await;
    assert!(!transfer.is_finished());
    drop(held);
    let transfer = transfer.await.unwrap().unwrap();
    catalog.cancel_derived(&transfer.id).await.unwrap();
    catalog.close().await.unwrap();
}
