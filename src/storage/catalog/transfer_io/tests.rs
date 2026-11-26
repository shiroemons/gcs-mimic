use super::*;
use crate::{
    storage::catalog::fault::{Fault, Step},
    test_support::TestDir,
};

#[tokio::test]
async fn a_rolled_back_writer_replaces_both_written_and_buffered_bytes() {
    let dir = TestDir::new();
    let path = dir.0.join("stage");
    let mut output = Output::Disk(objects::StagedFile::new(
        std::fs::File::create(&path).unwrap(),
        &dir.0,
    ));
    output.write(b"kept").await.unwrap();
    output.finish().await.unwrap();
    output
        .write(&vec![b'x'; super::super::fault::MID_WRITE_BODY_BYTES])
        .await
        .unwrap();
    output.rollback(4).await.unwrap();
    output.write(b"new").await.unwrap();
    output.finish().await.unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"keptnew");
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
        let mut output = Output::Disk(objects::StagedFile::new(
            std::fs::File::create(&path).unwrap(),
            &dir.0,
        ));
        let (release, blocked) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || blocked.recv());
        output
            .write(&vec![b'x'; objects::WRITE_BATCH_BYTES])
            .await
            .unwrap();
        drop(output);
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
async fn rollback_truncates_even_when_the_pending_batch_fails() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let path = dir.0.join("stage");
    let mut output = Output::Disk(objects::StagedFile::new(
        std::fs::File::create(&path).unwrap(),
        &dir.0,
    ));
    output.write(b"kept").await.unwrap();
    output.finish().await.unwrap();
    output
        .write(&vec![b'x'; objects::WRITE_BATCH_BYTES])
        .await
        .unwrap();
    output.settle().await.unwrap();
    catalog.inject(Step::BodyWrite, 0, Fault::Io(io::ErrorKind::StorageFull));
    output
        .write(&vec![b'y'; objects::WRITE_BATCH_BYTES])
        .await
        .unwrap();
    assert!(output.rollback(4).await.is_err());
    assert_eq!(catalog.pending_faults(), 0);
    assert_eq!(std::fs::read(path).unwrap(), b"kept");
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
        &dir.0,
    ));
    let megabyte = vec![b'x'; 1024 * 1024];
    catalog.inject(Step::BodyWriteBehind, 0, Fault::Io(io::ErrorKind::Other));
    for _ in 0..7 {
        output.write(&megabyte).await.unwrap();
    }
    assert_eq!(catalog.pending_faults(), 1, "below the window");
    output.write(&megabyte).await.unwrap();
    assert!(output.settle().await.is_err());
    assert_eq!(catalog.pending_faults(), 0, "at the window");
    // The failed sync left its window pending, so the next write retries it and succeeds.
    // The window then starts again, and the following 7 MiB stay below it.
    output.write(&megabyte).await.unwrap();
    catalog.inject(Step::BodyWriteBehind, 0, Fault::Io(io::ErrorKind::Other));
    for _ in 0..7 {
        output.write(&megabyte).await.unwrap();
    }
    assert_eq!(catalog.pending_faults(), 1, "a new window");
    assert!(
        async {
            output.write(&megabyte).await?;
            output.settle().await
        }
        .await
        .is_err()
    );
    drop(output);
    catalog.close().await.unwrap();
}
