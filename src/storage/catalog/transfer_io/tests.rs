use super::*;
use crate::object::ObjectKey;
use crate::{
    storage::catalog::fault::{Fault, Step},
    test_support::TestDir,
};
use std::sync::Arc;

#[tokio::test]
async fn a_rolled_back_writer_replaces_both_written_and_buffered_bytes() {
    let dir = TestDir::new();
    let path = dir.0.join("stage");
    let mut output = Output::Disk(objects::StagedFile::new(
        std::fs::File::create(&path).unwrap(),
        Arc::new(()),
        &dir.0,
    ));
    output.write(b"kept").await.unwrap();
    output.finish().await.unwrap();
    output
        .write(&vec![b'x'; super::super::fault::MID_WRITE_BODY_BYTES])
        .await
        .unwrap();
    assert_eq!(
        output.rollback(4).await.unwrap(),
        objects::Rollback::Truncated
    );
    output.write(b"new").await.unwrap();
    output.finish().await.unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"keptnew");
}

#[tokio::test]
async fn a_panicked_writer_defers_rollback_to_reopen() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let mut output = Output::Disk(objects::StagedFile::new(
        std::fs::File::create(dir.0.join("stage")).unwrap(),
        Arc::new(()),
        &dir.0,
    ));
    catalog.inject(Step::BodyWrite, 0, Fault::Panic);
    output.write(b"unacknowledged").await.unwrap();
    assert!(output.finish().await.is_err());
    assert_eq!(
        output.rollback(0).await.unwrap(),
        objects::Rollback::Deferred
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn rollback_flush_and_sync_failures_keep_the_acknowledged_prefix() {
    for (step, kind) in [
        (Step::BodyFlush, io::ErrorKind::StorageFull),
        (Step::BodySync, io::ErrorKind::Other),
    ] {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let id = crate::storage::blob_id::BlobId::generate().unwrap();
        let path = id.staging_path(&dir.0);
        let mut output = Output::Disk(objects::StagedFile::new(
            std::fs::File::create(&path).unwrap(),
            Arc::new(()),
            &dir.0,
        ));
        output.write(b"kept").await.unwrap();
        output.finish().await.unwrap();
        output.write(b"unacknowledged").await.unwrap();
        catalog.inject(step, 0, Fault::Io(kind));
        assert!(output.rollback(4).await.is_err());
        assert_eq!(catalog.pending_faults(), 0, "{step:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"kept", "{step:?}");
        output.rollback(4).await.unwrap();
        drop(output);
        catalog.close().await.unwrap();
    }
}

#[test]
fn dropping_a_queued_write_cannot_modify_a_reopened_stage() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let dir = TestDir::new();
        let path = dir.0.join("stage");
        let lease = Arc::new(());
        let retained = Arc::downgrade(&lease);
        let mut output = Output::Disk(objects::StagedFile::new(
            std::fs::File::create(&path).unwrap(),
            lease,
            &dir.0,
        ));
        let (release, blocked) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || blocked.recv());
        output
            .write(&vec![b'x'; objects::WRITE_BATCH_BYTES])
            .await
            .unwrap();
        drop(output);
        assert!(
            retained.upgrade().is_none(),
            "queued work retained the lease"
        );
        std::fs::write(&path, b"replacement").unwrap();
        release.send(()).unwrap();
        worker.await.unwrap().unwrap();
        let actual = tokio::task::spawn_blocking(move || std::fs::read(path))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(actual, b"replacement");
    });
}

#[tokio::test]
async fn closing_waits_for_a_running_write_after_the_upload_is_dropped() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let lease = Arc::new(catalog.admit_object_upload().unwrap());
    let mut stage = objects::StagedFile::new(
        std::fs::File::create(dir.0.join("stage")).unwrap(),
        lease,
        &dir.0,
    );
    let (entered, release) = super::super::test_fixtures::pause_read(&catalog, Step::BodyWrite);
    stage
        .write(&vec![1; objects::WRITE_BATCH_BYTES])
        .await
        .unwrap();
    entered.await.unwrap();
    drop(stage);
    let mut closing = Box::pin(catalog.close());
    assert!(
        closing
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    assert_eq!(catalog.active_uploads(), 1);
    release.send(()).unwrap();
    closing.await.unwrap();
    assert_eq!(catalog.active_uploads(), 0);
}

#[tokio::test]
async fn dropping_a_running_write_keeps_the_session_locked_until_the_worker_returns() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    crate::storage::test_fixtures::seed_bucket(&catalog).await;
    let session = catalog
        .create_resumable_session(
            ObjectKey::from_parts_unchecked("bucket", "object"),
            crate::storage::ResumableOptions::new(crate::bucket::BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    let id = session.id.to_string();
    let lease = catalog.lock_resumable_session(&id, "bucket").await.unwrap();
    let mut output = Output::open_readable(&catalog, &session.blob_id, 0, lease.guard.clone())
        .await
        .unwrap();
    let (entered, release) = super::super::test_fixtures::pause_read(&catalog, Step::BodyWrite);
    output
        .write(&vec![b'x'; objects::WRITE_BATCH_BYTES])
        .await
        .unwrap();
    entered.await.unwrap();
    drop(lease);
    drop(output);
    // The same single-thread runtime must be free to release the blocked disk worker.
    assert!(catalog.try_resumable_writer(session.id).is_err());
    release.send(()).unwrap();
    crate::test_support::eventually_async("the completed write to release its lease", || async {
        catalog.session_writers_idle()
    })
    .await;
    let next = catalog.lock_resumable_session(&id, "bucket").await.unwrap();
    catalog.prepare_resumable_status(&next).await.unwrap();
    assert_eq!(
        std::fs::metadata(session.blob_id.staging_path(&dir.0))
            .unwrap()
            .len(),
        0
    );
    drop(next);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn rollback_waits_for_a_running_write_before_truncating() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let path = dir.0.join("stage");
    let mut output = Output::Disk(objects::StagedFile::new(
        std::fs::File::create(&path).unwrap(),
        Arc::new(()),
        &dir.0,
    ));
    let (entered, release) = super::super::test_fixtures::pause_read(&catalog, Step::BodyWrite);
    output
        .write(&vec![b'x'; objects::WRITE_BATCH_BYTES])
        .await
        .unwrap();
    entered.await.unwrap();
    let mut rollback = Box::pin(output.rollback(0));
    assert!(
        rollback
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    release.send(()).unwrap();
    rollback.await.unwrap();
    assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn rollback_truncates_even_when_the_pending_batch_fails() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let path = dir.0.join("stage");
    let mut output = Output::Disk(objects::StagedFile::new(
        std::fs::File::create(&path).unwrap(),
        Arc::new(()),
        &dir.0,
    ));
    output.write(b"kept").await.unwrap();
    output.finish().await.unwrap();
    output
        .write(&vec![b'x'; objects::WRITE_BATCH_BYTES])
        .await
        .unwrap();
    output
        .settle_result(Ok::<(), ObjectError>(()))
        .await
        .unwrap();
    catalog.inject(Step::BodyWrite, 0, Fault::Io(io::ErrorKind::StorageFull));
    output
        .write(&vec![b'y'; objects::WRITE_BATCH_BYTES])
        .await
        .unwrap();
    assert!(output.rollback(4).await.is_err());
    assert_eq!(catalog.pending_faults(), 0);
    assert_eq!(std::fs::read(path).unwrap(), b"kept");
    assert!(output.write(b"another tail").await.is_err());
    assert!(output.finish().await.is_err());
    catalog.close().await.unwrap();
}

/// A staged body sends its bytes to storage once 8 MiB are unsynced, so a body larger than a
/// container's memory limit cannot fill it with unwritten page cache. A failure there fails
/// the write, and a body below the window never reaches the step.
#[tokio::test]
async fn a_staged_body_syncs_every_window_and_reports_a_failed_sync() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let id = crate::storage::blob_id::BlobId::generate().unwrap();
    let path = id.staging_path(&dir.0);
    let mut output = Output::Disk(objects::StagedFile::new(
        std::fs::File::create(&path).unwrap(),
        Arc::new(()),
        &dir.0,
    ));
    let megabyte = vec![b'x'; 1024 * 1024];
    catalog.inject(Step::BodyWriteBehind, 0, Fault::Io(io::ErrorKind::Other));
    for _ in 0..7 {
        output.write(&megabyte).await.unwrap();
    }
    assert_eq!(catalog.pending_faults(), 1, "below the window");
    output.write(&megabyte).await.unwrap();
    assert!(
        output
            .settle_result(Ok::<(), ObjectError>(()))
            .await
            .is_err()
    );
    assert_eq!(catalog.pending_faults(), 0, "at the window");
    assert!(output.write(b"retry").await.is_err());
    assert!(output.finish().await.is_err());
    drop(output);
    // A new stage gets a fresh window after a successful write-behind sync.
    let mut output = Output::Disk(objects::StagedFile::new(
        std::fs::File::create(&path).unwrap(),
        Arc::new(()),
        &dir.0,
    ));
    for _ in 0..8 {
        output.write(&megabyte).await.unwrap();
    }
    output
        .settle_result(Ok::<(), ObjectError>(()))
        .await
        .unwrap();
    catalog.inject(Step::BodyWriteBehind, 0, Fault::Io(io::ErrorKind::Other));
    for _ in 0..7 {
        output.write(&megabyte).await.unwrap();
    }
    assert_eq!(catalog.pending_faults(), 1, "a new window");
    assert!(
        async {
            output.write(&megabyte).await?;
            output.settle_result(Ok::<(), ObjectError>(())).await
        }
        .await
        .is_err()
    );
    drop(output);
    catalog.close().await.unwrap();
}
