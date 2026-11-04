use super::*;
use crate::{storage::test_fixtures::bucket, test_support::TestDir};

#[cfg(unix)]
use crate::storage::catalog::fault::{Crash, Fault};

#[cfg(unix)]
async fn retired_blob(data: &Path) -> (Catalog, BlobId) {
    use crate::storage::test_fixtures::{body, permissive_upload};
    let catalog = Catalog::open(data).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    catalog
        .put_object(
            permissive_upload("bucket", "object", 1000),
            body(vec![Bytes::from_static(b"old")]),
        )
        .await
        .unwrap();
    let old = catalog
        .run_upload(|connection| Ok(lookup(connection, "bucket", "object")?.unwrap().blob))
        .await
        .unwrap();
    fault::arm(
        data,
        Step::ReclaimUnlink,
        0,
        Fault::Io(io::ErrorKind::Other),
    );
    catalog
        .put_object(
            permissive_upload("bucket", "object", 1000),
            body(vec![Bytes::from_static(b"new")]),
        )
        .await
        .unwrap();
    assert_eq!(fault::pending(data), 0);
    assert_eq!(pending(data), 1);
    (catalog, old)
}

#[cfg(unix)]
#[tokio::test]
async fn reclamation_crash_and_power_loss_finish_each_deletion_boundary() {
    for crash in Crash::ALL {
        for step in [Step::ReclaimUnlink, Step::ReclaimSync, Step::ReclaimForget] {
            let data = TestDir::new();
            let snapshot = TestDir::new();
            let (catalog, old) = retired_blob(&data.0).await;
            fault::arm(&data.0, step, 0, crash.fault(snapshot.0.clone()));
            catalog.reclaim_pending().await.unwrap();
            assert_eq!(fault::pending(&data.0), 0, "{crash:?} {step:?}");
            assert_eq!(pending(&snapshot.0), 1);
            let survives = step == Step::ReclaimUnlink
                || (step == Step::ReclaimSync && *crash == Crash::PowerLoss);
            assert_eq!(
                old.blob_path(&snapshot.0).exists(),
                survives,
                "{crash:?} {step:?}"
            );
            assert_eq!(fault::catalog_thread_io(&data.0), 0);
            catalog.close().await.unwrap();
            let recovered = Catalog::open(&snapshot.0).await.unwrap();
            assert_eq!(pending(&snapshot.0), 0);
            assert!(!old.blob_path(&snapshot.0).exists());
            let (_, mut reader) = recovered
                .open_object(
                    crate::storage::test_fixtures::live("bucket", "object"),
                    Default::default(),
                    crate::storage::test_fixtures::PERMISSIVE,
                )
                .await
                .unwrap();
            assert_eq!(
                crate::storage::test_fixtures::read_all(&mut reader).await,
                b"new"
            );
            recovered.close().await.unwrap();
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn failed_reclamation_sync_and_forget_leave_the_journal_retryable() {
    for step in [Step::ReclaimUnlink, Step::ReclaimSync, Step::ReclaimForget] {
        let data = TestDir::new();
        let (catalog, old) = retired_blob(&data.0).await;
        fault::arm(&data.0, step, 0, Fault::Io(io::ErrorKind::Other));
        assert!(catalog.reclaim_pending().await.is_err());
        assert_eq!(fault::pending(&data.0), 0);
        assert_eq!(pending(&data.0), 1);
        catalog.reclaim_pending().await.unwrap();
        assert_eq!(pending(&data.0), 0);
        assert!(!old.blob_path(&data.0).exists());
        catalog.close().await.unwrap();
    }
}

async fn journaled_catalog(data: &Path) -> Catalog {
    let catalog = Catalog::open(data).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    let root = data.to_owned();
    catalog.run_upload(move |connection| {
        let transaction = connection.transaction().unwrap();
        for (prefix, count) in [("00", 140), ("ff", 200)] {
            for index in 0..count {
                let text = format!("{prefix}{index:030x}");
                let id: BlobId = text.parse().unwrap();
                let path = id.blob_path(&root);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, b"x").unwrap();
                if prefix == "00" {
                    transaction.execute("INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id) VALUES ('bucket',?1,1,1,'text/plain',1,zeroblob(16),0,'STANDARD',0,0,?2)", params![format!("live-{index}"), id]).unwrap();
                }
                transaction.execute("INSERT INTO pending_blob_deletions VALUES (?1)", [id]).unwrap();
            }
        }
        transaction.execute("UPDATE object_generation SET last_generation=1", []).unwrap();
        transaction.commit().unwrap();
        Ok(())
    }).await.unwrap();
    catalog
}

fn pending(data: &Path) -> usize {
    Connection::open(data.join("catalog.sqlite3"))
        .unwrap()
        .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn assert_live_blobs(data: &Path) {
    for index in 0..140 {
        let id: BlobId = format!("00{index:030x}").parse().unwrap();
        assert_eq!(fs::read(id.blob_path(data)).unwrap(), b"x");
    }
}

#[tokio::test]
async fn reclamation_skips_more_than_a_page_of_live_journal_entries() {
    let data = TestDir::new();
    let catalog = journaled_catalog(&data.0).await;
    assert_eq!(pending(&data.0), 340);
    for expected in [212, 140, 140] {
        let before = pending(&data.0);
        let error = catalog.reclaim_pending().await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("00000000000000000000000000000000")
        );
        assert_eq!(pending(&data.0), expected);
        assert!(before - expected <= 128);
        assert_live_blobs(&data.0);
    }
    for index in 0..200 {
        let id: BlobId = format!("ff{index:030x}").parse().unwrap();
        assert!(!id.blob_path(&data.0).exists());
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn startup_drains_reclaimable_journal_rows_before_reporting_a_live_blob() {
    let data = TestDir::new();
    let catalog = journaled_catalog(&data.0).await;
    catalog.close().await.unwrap();
    let error = Catalog::open(&data.0).await.err().unwrap();
    assert!(
        error
            .to_string()
            .contains("00000000000000000000000000000000")
    );
    assert_eq!(pending(&data.0), 140);
    assert_live_blobs(&data.0);
    for index in 0..200 {
        let id: BlobId = format!("ff{index:030x}").parse().unwrap();
        assert!(!id.blob_path(&data.0).exists());
    }
}
