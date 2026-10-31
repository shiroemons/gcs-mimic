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
        let mut output = Output::Disk(tokio::fs::File::create(&path).await.unwrap());
        output.write(b"kept").await.unwrap();
        output.finish(&catalog).await.unwrap();
        output.write(b"unacknowledged").await.unwrap();
        catalog.inject(step, 0, Fault::Io(kind));
        assert!(output.rollback(&catalog, 4).await.is_err());
        assert_eq!(catalog.pending_faults(), 0, "{step:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"kept", "{step:?}");
        output.rollback(&catalog, 4).await.unwrap();
        drop(output);
        catalog.close().await.unwrap();
    }
}
