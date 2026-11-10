use super::*;
use crate::{
    storage::catalog::fault::{Fault, Step},
    test_support::TestDir,
};

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
            tokio::fs::File::create(&path).await.unwrap(),
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
        tokio::fs::File::create(&path).await.unwrap(),
        &dir.0,
    ));
    let megabyte = vec![b'x'; 1024 * 1024];
    catalog.inject(Step::BodyWriteBehind, 0, Fault::Io(io::ErrorKind::Other));
    for _ in 0..7 {
        output.write(&megabyte).await.unwrap();
    }
    assert_eq!(catalog.pending_faults(), 1, "below the window");
    assert!(output.write(&megabyte).await.is_err());
    assert_eq!(catalog.pending_faults(), 0, "at the window");
    // The failed sync left its window pending, so the next write retries it and succeeds.
    // The window then starts again, and the following 7 MiB stay below it.
    output.write(&megabyte).await.unwrap();
    catalog.inject(Step::BodyWriteBehind, 0, Fault::Io(io::ErrorKind::Other));
    for _ in 0..7 {
        output.write(&megabyte).await.unwrap();
    }
    assert_eq!(catalog.pending_faults(), 1, "a new window");
    assert!(output.write(&megabyte).await.is_err());
    drop(output);
    catalog.close().await.unwrap();
}
