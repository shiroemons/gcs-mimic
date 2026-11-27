use super::*;
use crate::{
    bucket::{BucketAccess, Versioning},
    storage::{
        AppendWriter, Finality, ObjectListQuery, ResumableOptions,
        catalog::fault::Step,
        test_fixtures::{pause_read, read_all},
    },
};

#[tokio::test]
async fn live_and_version_pages_see_the_committed_overwrite() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let mut definition = bucket("bucket", "project");
    definition.versioning = Versioning::Enabled;
    let bucket = catalog.insert_bucket(definition).await.unwrap();
    let old = put(&catalog, "object", b"old").await;
    let new = put(&catalog, "object", b"new").await;
    for (versions, expected) in [
        (false, vec![new.generation]),
        (true, vec![old.generation, new.generation]),
    ] {
        let query = ObjectListQuery {
            bucket: bucket.name.clone(),
            versions,
            ..Default::default()
        };
        assert_point_read(&catalog, async || {
            let page = catalog
                .list_objects(query.clone(), 100, PERMISSIVE)
                .await
                .unwrap();
            assert_eq!(page.bucket_metageneration, Some(bucket.metageneration));
            assert_eq!(
                page.items
                    .iter()
                    .map(|item| item.generation)
                    .collect::<Vec<_>>(),
                expected
            );
        })
        .await;
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_page_and_its_bucket_metageneration_share_one_snapshot() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let query = ObjectListQuery {
        bucket: "bucket".into(),
        ..Default::default()
    };
    let (reached, release) = pause_read(&catalog, Step::ListedObjectPage);
    let listing = catalog.list_objects(query.clone(), 10, PERMISSIVE);
    tokio::pin!(listing);
    tokio::select! {
        page = &mut listing => panic!("list finished before reading the bucket metageneration: {page:?}"),
        entered = reached => entered.unwrap(),
    }
    catalog
        .run::<_, CatalogError>(|connection| {
            let transaction = sql::deferred_write(connection)?;
            sql::exec(
                &transaction,
                "UPDATE buckets SET metageneration = 2 WHERE name = 'bucket'",
                [],
            )?;
            sql::exec(
                &transaction,
                "UPDATE objects SET content_type = 'text/html' WHERE bucket = 'bucket'",
                [],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    release.send(()).unwrap();
    let original = listing.await.unwrap();
    assert_eq!(original.items[0].content_type, "text/plain");
    assert_eq!(original.bucket_metageneration, Some(1));
    let current = catalog.list_objects(query, 10, PERMISSIVE).await.unwrap();
    assert_eq!(current.items[0].content_type, "text/html");
    assert_eq!(current.bucket_metageneration, Some(2));
    catalog.close().await.unwrap();
}

async fn appendable_with_bytes(dir: &TestDir, bytes: &'static [u8]) -> (Catalog, AppendWriter) {
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    let mut writer = catalog
        .create_appendable(
            crate::object::ObjectKey::from_parts_unchecked("bucket", "object"),
            ResumableOptions::new(BucketAccess::Permissive),
        )
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(bytes), 100)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    (catalog, writer)
}

#[tokio::test]
async fn an_unlinked_appendable_open_retries_and_returns_the_finalized_bytes() {
    let dir = TestDir::new();
    let bytes = b"finalized bytes";
    let (catalog, mut writer) = appendable_with_bytes(&dir, bytes).await;
    let (reached, release) = pause_read(&catalog, Step::AppendableReadSelected);
    let open = catalog.open_readable(
        live("bucket", "object"),
        Default::default(),
        PERMISSIVE,
        None,
    );
    tokio::pin!(open);
    tokio::select! {
        _ = &mut open => panic!("read finished before opening the appendable file"),
        entered = reached => entered.unwrap(),
    }
    let published = writer
        .finalize(None, BucketAccess::Permissive, 100)
        .await
        .unwrap();
    let (writes, reads) = catalog.job_counts();
    release.send(()).unwrap();
    let (record, mut reader, finality) = open.await.unwrap();
    assert_eq!(finality, Finality::Finalized);
    assert_eq!(record, published);
    assert_eq!(read_all(&mut reader).await, bytes);
    assert_eq!(catalog.job_counts(), (writes + 1, reads + 1));
    assert_eq!(catalog.readonly_answers(), 0);
    drop((writer, reader));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn readable_opens_and_metadata_see_the_committed_generation_in_both_states() {
    let dir = TestDir::new();
    let bytes = b"appendable";
    let (catalog, writer) = appendable_with_bytes(&dir, bytes).await;
    let appendable = writer.record().generation;
    drop(writer);
    let finalized = put(&catalog, "finalized", bytes).await;
    for (name, generation, finality) in [
        ("object", appendable, Finality::Unfinalized),
        ("finalized", finalized.generation, Finality::Finalized),
    ] {
        assert_point_read(&catalog, async || {
            let (record, mut reader, state) = catalog
                .open_readable(live("bucket", name), Default::default(), PERMISSIVE, None)
                .await
                .unwrap();
            assert_eq!((record.generation, state), (generation, finality));
            assert_eq!(read_all(&mut reader).await, bytes);
        })
        .await;
        assert_point_read(&catalog, async || {
            let metadata = catalog
                .get_readable(live("bucket", name), Default::default(), PERMISSIVE, None)
                .await
                .unwrap();
            assert_eq!(
                (metadata.record.generation, metadata.finality),
                (generation, finality)
            );
            assert_eq!(metadata.record.size, bytes.len() as u64);
        })
        .await;
    }
    catalog.close().await.unwrap();
}
