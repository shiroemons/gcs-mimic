use super::appendable::AppendError;
use super::*;

async fn create(catalog: &Catalog) -> appendable::AppendWriter {
    catalog
        .create_appendable(
            "test-bucket",
            "growing",
            resumable::ResumableOptions::new(BucketAccess::Permissive),
        )
        .await
        .unwrap()
}

async fn takeover(catalog: &Catalog, generation: i64) -> appendable::AppendWriter {
    catalog
        .take_over_appendable(
            "test-bucket",
            "growing",
            generation,
            None,
            Default::default(),
            BucketAccess::Permissive,
        )
        .await
        .unwrap()
}

async fn open(catalog: &Catalog) -> (crate::object::ObjectRecord, objects::ObjectReader) {
    catalog
        .open_appendable(
            "test-bucket",
            "growing",
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap()
}

async fn bytes(mut reader: objects::ObjectReader) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

#[tokio::test]
async fn ram_appendable_prefix_snapshots_takeover_and_catalogs_are_isolated() {
    let catalog = setup(8192, false).await;
    let other = setup(8192, false).await;
    let mut writer = create(&catalog).await;
    let mut separate = create(&other).await;
    let generation = writer.record().generation;
    writer
        .append(0, Bytes::from_static(b"abc"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    separate
        .append(0, Bytes::from_static(b"other"), 1024)
        .await
        .unwrap();
    separate.flush().await.unwrap();
    let (_, snapshot) = open(&catalog).await;
    writer
        .append(3, Bytes::from_static(b"unflushed"), 1024)
        .await
        .unwrap();
    assert_eq!(bytes(open(&catalog).await.1).await, b"abc");
    assert!(matches!(
        catalog.admin(admin::Origin::Seed).reset().await,
        Err(AdminError::Busy { .. })
    ));
    let handle = writer.write_handle().to_vec();
    let mut current = takeover(&catalog, generation).await;
    assert!(matches!(writer.flush().await, Err(AppendError::Superseded)));
    assert!(matches!(
        catalog
            .take_over_appendable(
                "test-bucket",
                "growing",
                generation,
                Some(&handle),
                Default::default(),
                BucketAccess::Permissive
            )
            .await,
        Err(AppendError::Superseded)
    ));
    assert!(matches!(
        current.append(4, Bytes::from_static(b"gap"), 1024).await,
        Err(AppendError::OffsetGap { expected: 3 })
    ));
    current
        .append(0, Bytes::from_static(b"oldXYZ"), 1024)
        .await
        .unwrap();
    current.flush().await.unwrap();
    assert_eq!(bytes(snapshot).await, b"abc");
    assert_eq!(bytes(open(&catalog).await.1).await, b"abcXYZ");
    assert_eq!(bytes(open(&other).await.1).await, b"other");
    assert!(
        current
            .finalize(Some(0), BucketAccess::Permissive, 1024)
            .await
            .is_err()
    );
    let record = current
        .finalize(
            Some(crc32c::crc32c(b"abcXYZ")),
            BucketAccess::Permissive,
            1024,
        )
        .await
        .unwrap();
    assert_eq!(record.generation, generation);
    assert_eq!(record.crc32c, crc32c::crc32c(b"abcXYZ"));
    assert!(matches!(
        current.append(0, Bytes::from_static(b"abc"), 1024).await,
        Err(AppendError::Superseded)
    ));
    assert!(matches!(
        current.flush().await,
        Err(AppendError::Superseded)
    ));
    assert!(matches!(
        catalog
            .take_over_appendable(
                "test-bucket",
                "growing",
                generation,
                None,
                Default::default(),
                BucketAccess::Permissive
            )
            .await,
        Err(AppendError::Finalized)
    ));
    assert_eq!(
        bytes(
            catalog
                .open_object("test-bucket", "growing", None, Default::default())
                .await
                .unwrap()
                .1
        )
        .await,
        b"abcXYZ"
    );
    drop((writer, current, separate));
    catalog.close().await.unwrap();
    other.close().await.unwrap();
}

#[tokio::test]
async fn ram_appendable_aggregate_quota_preserves_prefix_and_finalization_for_retry() {
    let catalog = setup(1000, false).await;
    let mut writer = create(&catalog).await;
    writer
        .append(0, Bytes::from_static(b"abcd"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    let (_, snapshot) = open(&catalog).await;
    put(&catalog, Default::default(), true).await.unwrap();
    writer
        .append(4, Bytes::from_static(b"efgh"), 1024)
        .await
        .unwrap();
    assert!(writer.flush().await.is_err());
    assert_eq!(open(&catalog).await.0.size, 4);
    assert_eq!(bytes(open(&catalog).await.1).await, b"abcd");
    catalog
        .delete_object_authorized(
            "test-bucket",
            "value",
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    let mut current = takeover(&catalog, writer.record().generation).await;
    assert!(
        current
            .append(4, Bytes::from(vec![7; 2000]), 4096)
            .await
            .is_err()
    );
    current
        .append(4, Bytes::from_static(b"efgh"), 1024)
        .await
        .unwrap();
    current.flush().await.unwrap();
    assert_eq!(bytes(open(&catalog).await.1).await, b"abcdefgh");
    assert!(
        current
            .finalize(None, BucketAccess::Permissive, 1024)
            .await
            .is_err()
    );
    assert_eq!(open(&catalog).await.0.size, 8);
    assert!(matches!(
        catalog
            .get_object("test-bucket", "growing", None, Default::default())
            .await,
        Err(ObjectError::ObjectNotFound)
    ));
    drop(snapshot);
    current
        .finalize(None, BucketAccess::Permissive, 1024)
        .await
        .unwrap();
    assert_eq!(
        bytes(
            catalog
                .open_object("test-bucket", "growing", None, Default::default())
                .await
                .unwrap()
                .1
        )
        .await,
        b"abcdefgh"
    );
    drop((writer, current));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_appendable_failed_size_and_publication_keep_acknowledged_state() {
    let catalog = setup(8192, false).await;
    catalog
        .insert_notification(
            "test-bucket",
            crate::notification::NotificationConfig {
                topic: "//pubsub.googleapis.com/projects/local/topics/events".into(),
                payload_format: "JSON_API_V1".into(),
                event_types: Vec::new(),
                custom_attributes: Default::default(),
                object_name_prefix: String::new(),
            },
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    let mut writer = create(&catalog).await;
    writer
        .append(0, Bytes::from_static(b"abcd"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    writer
        .append(4, Bytes::from_static(b"efgh"), 1024)
        .await
        .unwrap();
    catalog.run_object(|connection| {
        connection.execute_batch("CREATE TEMP TRIGGER reject_ram_flush BEFORE UPDATE OF size ON appendable_objects BEGIN SELECT RAISE(ABORT,'injected checkpoint failure'); END;").unwrap(); Ok(())
    }).await.unwrap();
    assert!(writer.flush().await.is_err());
    assert_eq!(bytes(open(&catalog).await.1).await, b"abcd");
    catalog.run_object(|connection| { connection.execute_batch("DROP TRIGGER reject_ram_flush; CREATE TEMP TRIGGER reject_ram_finalize BEFORE INSERT ON objects BEGIN SELECT RAISE(ABORT,'injected publication failure'); END;").unwrap(); Ok(()) }).await.unwrap();
    assert!(
        writer
            .finalize(None, BucketAccess::Permissive, 1024)
            .await
            .is_err()
    );
    assert!(catalog.next_notification().await.unwrap().is_none());
    assert_eq!(bytes(open(&catalog).await.1).await, b"abcdefgh");
    catalog
        .run_object(|connection| {
            connection
                .execute_batch("DROP TRIGGER reject_ram_finalize")
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    let record = writer
        .finalize(None, BucketAccess::Permissive, 1024)
        .await
        .unwrap();
    assert_eq!(record.size, 8);
    assert_eq!(record.generation, writer.record().generation);
    let pending = catalog.next_notification().await.unwrap().unwrap();
    let message: serde_json::Value = serde_json::from_str(&pending.message).unwrap();
    assert_eq!(
        message["messages"][0]["attributes"]["eventType"],
        "OBJECT_FINALIZE"
    );
    catalog.acknowledge_notification(pending.id).await.unwrap();
    assert!(catalog.next_notification().await.unwrap().is_none());
    drop(writer);
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn ram_appendable_cancelled_flush_rolls_back_candidate_and_allows_takeover() {
    let catalog = setup(8192, false).await;
    let mut writer = create(&catalog).await;
    writer
        .append(0, Bytes::from_static(b"abcd"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    let generation = writer.record().generation;
    writer
        .append(4, Bytes::from_static(b"lost"), 1024)
        .await
        .unwrap();
    let id: String = catalog
        .run_object(|connection| {
            connection
                .prepare_cached("SELECT blob_id FROM appendable_objects WHERE name='growing'")
                .unwrap()
                .query_row([], |row| row.get(0))
                .map_err(crate::storage::failure::object_unavailable)
        })
        .await
        .unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let worker = catalog.clone();
    let gate = tokio::spawn(async move {
        worker
            .run_object(move |_| {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Ok(())
            })
            .await
    });
    started.await.unwrap();
    let flush = tokio::spawn(async move { writer.flush().await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let mut reader = catalog.0.memory.as_ref().unwrap().open(&id).unwrap();
            let mut output = Vec::new();
            std::io::Read::read_to_end(&mut reader, &mut output).unwrap();
            if output == b"abcdlost" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    flush.abort();
    assert!(flush.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    gate.await.unwrap().unwrap();
    let mut current = takeover(&catalog, generation).await;
    assert_eq!(current.persisted_size(), 4);
    current
        .append(4, Bytes::from_static(b"safe"), 1024)
        .await
        .unwrap();
    current.flush().await.unwrap();
    assert_eq!(bytes(open(&catalog).await.1).await, b"abcdsafe");
    catalog
        .delete_appendable(
            "test-bucket",
            "growing",
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    drop(current);
    catalog.close().await.unwrap();
}
