use super::super::xml_multipart::{CompletedPart, Error, Options, Part, PartUpload, Target};
use super::*;

async fn begin(catalog: &Catalog) -> Target {
    catalog
        .begin_xml_multipart(
            "test-bucket",
            "value",
            Options {
                content_type: "text/plain".into(),
                metadata: Default::default(),
            },
            BucketAccess::Permissive,
        )
        .await
        .unwrap()
        .target
}

async fn part(
    catalog: &Catalog,
    target: &Target,
    bytes: &'static [u8],
    eof: bool,
) -> Result<Part, Error> {
    let lease = catalog
        .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
        .await?;
    let (sender, body) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(bytes))))
        .unwrap();
    if eof {
        sender.try_send(Ok(UploadChunk::End)).unwrap();
    }
    drop(sender);
    catalog
        .put_xml_part(
            lease,
            PartUpload {
                number: 1,
                expected: Default::default(),
                max_bytes: 1024,
            },
            body,
        )
        .await
}

async fn complete(
    catalog: &Catalog,
    target: &Target,
    etag: &str,
) -> Result<crate::object::ObjectRecord, Error> {
    let lease = catalog
        .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
        .await?;
    catalog
        .complete_xml_multipart(
            lease,
            vec![CompletedPart {
                number: 1,
                etag: etag.into(),
            }],
            1024,
        )
        .await
}

async fn bytes(catalog: &Catalog) -> Vec<u8> {
    let (_, mut reader) = catalog
        .open_object("test-bucket", "value", None, Default::default())
        .await
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

async fn reclaimed(catalog: &Catalog) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let empty = catalog
                .run_object(|connection| {
                    connection
                        .prepare_cached("SELECT NOT EXISTS(SELECT 1 FROM derived_transfers)")
                        .unwrap()
                        .query_row([], |row| row.get::<_, bool>(0))
                        .map_err(crate::storage::failure::object_unavailable)
                })
                .await
                .unwrap();
            if empty {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn ram_xml_multipart_part_eof_sql_failure_and_replacement_keep_selected_bytes() {
    let catalog = setup(1100, false).await;
    let original = put(&catalog, Default::default(), true).await.unwrap();
    let target = begin(&catalog).await;
    let first = part(&catalog, &target, b"first", true).await.unwrap();
    assert!(part(&catalog, &target, b"aborted", false).await.is_err());
    catalog.run_object(|connection| {
        connection.execute_batch("CREATE TEMP TRIGGER reject_ram_part BEFORE INSERT ON xml_multipart_parts BEGIN SELECT RAISE(ABORT, 'injected part checkpoint failure'); END;").unwrap();
        Ok(())
    }).await.unwrap();
    assert!(part(&catalog, &target, b"rejected", true).await.is_err());
    catalog
        .run_object(|connection| {
            connection
                .execute_batch("DROP TRIGGER reject_ram_part")
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    let (_, listed, _) = catalog
        .list_xml_parts(target.clone(), BucketAccess::Permissive, 0, 100)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!((&listed[0].etag, listed[0].size), (&first.etag, 5));
    let lease = catalog
        .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
        .await
        .unwrap();
    assert!(part(&catalog, &target, b"race", true).await.is_err());
    drop(lease);
    let replacement = part(&catalog, &target, b"other", true).await.unwrap();
    assert_ne!(replacement.etag, first.etag);
    assert!(matches!(
        complete(&catalog, &target, &first.etag).await,
        Err(Error::InvalidPart)
    ));
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, Default::default())
            .await
            .unwrap(),
        original
    );
    let record = complete(&catalog, &target, &replacement.etag)
        .await
        .unwrap();
    assert!(!record.has_md5());
    assert_eq!(record.component_count, None);
    assert_eq!(bytes(&catalog).await, b"other");
    assert!(matches!(
        complete(&catalog, &target, &replacement.etag).await,
        Err(Error::NoSuchUpload)
    ));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_xml_multipart_quota_preserves_parts_and_old_destination_for_retry() {
    let catalog = setup(900, false).await;
    let original = put(&catalog, Default::default(), true).await.unwrap();
    let target = begin(&catalog).await;
    let first = part(&catalog, &target, b"first", true).await.unwrap();
    let mut reservations = Vec::new();
    while let Ok(stage) = catalog.0.memory.as_ref().unwrap().stage() {
        reservations.push(stage);
    }
    assert!(part(&catalog, &target, b"new part", true).await.is_err());
    drop(reservations);
    assert!(complete(&catalog, &target, &first.etag).await.is_err());
    reclaimed(&catalog).await;
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, Default::default())
            .await
            .unwrap(),
        original
    );
    let (_, listed, _) = catalog
        .list_xml_parts(target.clone(), BucketAccess::Permissive, 0, 100)
        .await
        .unwrap();
    assert_eq!(listed[0].etag, first.etag);
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
    complete(&catalog, &target, &first.etag).await.unwrap();
    assert_eq!(bytes(&catalog).await, b"first");
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_xml_multipart_abort_expiry_and_reset_release_owned_hidden_parts() {
    let catalog = setup(8192, false).await;
    let target = begin(&catalog).await;
    part(&catalog, &target, b"hidden", true).await.unwrap();
    let lease = catalog
        .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
        .await
        .unwrap();
    assert!(matches!(
        catalog.admin(admin::Origin::Seed).reset().await,
        Err(AdminError::Busy { .. })
    ));
    catalog.abort_xml_multipart(lease).await.unwrap();
    assert!(matches!(
        catalog
            .lease_xml_multipart(target, BucketAccess::Permissive)
            .await,
        Err(Error::NoSuchUpload)
    ));
    let expired = begin(&catalog).await;
    part(&catalog, &expired, b"expired", true).await.unwrap();
    catalog.maintain_xml_multipart(i64::MAX).await.unwrap();
    assert!(matches!(
        catalog
            .lease_xml_multipart(expired, BucketAccess::Permissive)
            .await,
        Err(Error::NoSuchUpload)
    ));
    let reset = begin(&catalog).await;
    part(&catalog, &reset, b"reset", true).await.unwrap();
    catalog.admin(admin::Origin::Seed).reset().await.unwrap();
    assert!(
        catalog
            .lease_xml_multipart(reset, BucketAccess::Permissive)
            .await
            .is_err()
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_xml_multipart_publication_failure_rolls_back_session_parts_and_outbox() {
    let catalog = setup(8192, true).await;
    let original = put(&catalog, Default::default(), true).await.unwrap();
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
    let target = begin(&catalog).await;
    let selected = part(&catalog, &target, b"assembled", true).await.unwrap();
    assert!(catalog.next_notification().await.unwrap().is_none());
    catalog.run_object(|connection| {
        connection.execute_batch("CREATE TEMP TRIGGER reject_ram_multipart BEFORE INSERT ON objects BEGIN SELECT RAISE(ABORT, 'injected completion failure'); END;").unwrap();
        Ok(())
    }).await.unwrap();
    assert!(complete(&catalog, &target, &selected.etag).await.is_err());
    reclaimed(&catalog).await;
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, Default::default())
            .await
            .unwrap(),
        original
    );
    assert!(catalog.next_notification().await.unwrap().is_none());
    let (_, listed, _) = catalog
        .list_xml_parts(target.clone(), BucketAccess::Permissive, 0, 100)
        .await
        .unwrap();
    assert_eq!(listed[0].etag, selected.etag);
    catalog
        .run_object(|connection| {
            connection
                .execute_batch("DROP TRIGGER reject_ram_multipart")
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    complete(&catalog, &target, &selected.etag).await.unwrap();
    let mut events = Vec::new();
    while let Some(pending) = catalog.next_notification().await.unwrap() {
        let message: serde_json::Value = serde_json::from_str(&pending.message).unwrap();
        events.push(
            message["messages"][0]["attributes"]["eventType"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        catalog.acknowledge_notification(pending.id).await.unwrap();
    }
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["OBJECT_ARCHIVE".into(), "OBJECT_FINALIZE".into()])
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_xml_multipart_close_wakes_stalled_parts_without_publishing_them() {
    let catalog = setup(200_000, false).await;
    let target = begin(&catalog).await;
    let lease = catalog
        .lease_xml_multipart(target, BucketAccess::Permissive)
        .await
        .unwrap();
    let (sender, body) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(b"pending"))))
        .unwrap();
    let worker = catalog.clone();
    let request = tokio::spawn(async move {
        worker
            .put_xml_part(
                lease,
                PartUpload {
                    number: 1,
                    expected: Default::default(),
                    max_bytes: 1024,
                },
                body,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while sender.capacity() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), catalog.close())
        .await
        .unwrap()
        .unwrap();
    assert!(request.await.unwrap().is_err());
    assert!(sender.is_closed());
}

#[tokio::test(flavor = "current_thread")]
async fn ram_xml_multipart_queued_part_checkpoint_rechecks_deadline_without_replacing_bytes() {
    struct Time(std::sync::Mutex<std::time::SystemTime>);
    impl crate::clock::Clock for Time {
        fn now(&self) -> std::time::SystemTime {
            *self.0.lock().unwrap()
        }
    }
    let clock = Arc::new(Time(std::sync::Mutex::new(
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000),
    )));
    let catalog = setup_with_clock(8192, false, clock.clone()).await;
    let original = put(&catalog, Default::default(), true).await.unwrap();
    let target = begin(&catalog).await;
    let old = part(&catalog, &target, b"first", true).await.unwrap();
    let upload = target.id.clone();
    let identity = catalog.run_object(move |connection| {
        connection.prepare_cached("SELECT blob_id,size,md5,crc32c FROM xml_multipart_parts WHERE upload_id=?1 AND number=1").unwrap()
            .query_row([upload], |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,Vec<u8>>(2)?,row.get::<_,u32>(3)?))).map_err(crate::storage::failure::object_unavailable)
    }).await.unwrap();
    let lease = catalog
        .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
        .await
        .unwrap();
    let (sender, body) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(
            b"expired replacement",
        ))))
        .unwrap();
    let worker = catalog.clone();
    let request = tokio::spawn(async move {
        worker
            .put_xml_part(
                lease,
                PartUpload {
                    number: 1,
                    expected: Default::default(),
                    max_bytes: 1024,
                },
                body,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while sender.capacity() != 2 {
            tokio::task::yield_now().await;
        }
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
    sender.send(Ok(UploadChunk::End)).await.unwrap();
    // Current-thread ordering runs EOF through the SQL wait before this receiver-close wake.
    tokio::time::timeout(std::time::Duration::from_secs(2), sender.closed())
        .await
        .unwrap();
    *clock.0.lock().unwrap() += std::time::Duration::from_secs(604800);
    release.send(()).unwrap();
    gate.await.unwrap().unwrap();
    assert!(matches!(request.await.unwrap(), Err(Error::NoSuchUpload)));
    let upload = target.id;
    let retained = catalog.run_object(move |connection| {
        connection.prepare_cached("SELECT blob_id,size,md5,crc32c FROM xml_multipart_parts WHERE upload_id=?1 AND number=1").unwrap()
            .query_row([upload], |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,Vec<u8>>(2)?,row.get::<_,u32>(3)?))).map_err(crate::storage::failure::object_unavailable)
    }).await.unwrap();
    assert_eq!(retained, identity);
    assert_eq!(old.size, 5);
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, Default::default())
            .await
            .unwrap(),
        original
    );
    catalog.close().await.unwrap();
}
