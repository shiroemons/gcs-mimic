use super::*;
use crate::{storage::test_fixtures::bucket, test_support::TestDir};

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
