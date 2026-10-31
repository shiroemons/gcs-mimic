use super::*;
use crate::{
    object::{ExpectedHashes, ObjectConditions, ObjectError, UploadChunk},
    storage::admin::{AdminError, BucketDefinition, BucketName},
};
use bytes::Bytes;
use tokio::sync::mpsc;

mod appendable_tests;
mod resumable_tests;
mod xml_multipart_tests;

async fn setup(budget: u64, versioning: bool) -> Catalog {
    setup_with_clock(budget, versioning, Arc::new(SystemClock)).await
}

async fn setup_with_clock(
    budget: u64,
    versioning: bool,
    clock: Arc<dyn crate::clock::Clock>,
) -> Catalog {
    let catalog = Catalog::open_memory_for_server(
        budget,
        1024 * 1024,
        clock.clone(),
        admin::LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    let name = BucketName::new("test-bucket").unwrap();
    let mut record = BucketDefinition::default_for(&name, "local", &*clock)
        .unwrap()
        .into_record();
    record.versioning_enabled = versioning;
    catalog
        .admin(admin::Origin::Seed)
        .ensure_bucket(BucketDefinition::new(record).unwrap())
        .await
        .unwrap();
    catalog
}

fn body(complete: bool) -> mpsc::Receiver<Result<UploadChunk, std::io::Error>> {
    let (sender, receiver) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(b"data"))))
        .unwrap();
    if complete {
        sender.try_send(Ok(UploadChunk::End)).unwrap();
    }
    receiver
}

async fn put(
    catalog: &Catalog,
    condition: ObjectConditions,
    complete: bool,
) -> Result<crate::object::ObjectRecord, ObjectError> {
    catalog
        .put_object(
            "test-bucket",
            "value",
            "text/plain".into(),
            ExpectedHashes::default(),
            condition,
            1024,
            body(complete),
        )
        .await
}

async fn ram_transfer(catalog: &Catalog) -> derived::DerivedTransfer {
    let source = catalog
        .resolve_derived_source(
            "test-bucket",
            "value",
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    catalog
        .begin_derived(vec![source.whole()], 1024)
        .await
        .unwrap()
}

fn ram_destination(name: &str) -> derived::DerivedDestination {
    derived::DerivedDestination {
        bucket: "test-bucket".into(),
        name: name.into(),
        content_type: "text/plain".into(),
        ..derived::DerivedDestination::new(BucketAccess::Permissive)
    }
}

#[tokio::test]
async fn ram_derived_pins_survive_source_delete_and_checkpoints_share_hash_and_publication() {
    let catalog = setup(4096, false).await;
    let source = put(&catalog, Default::default(), true).await.unwrap();
    let transfer = ram_transfer(&catalog).await;
    assert_eq!(
        catalog
            .advance_derived(&transfer.id, 2)
            .await
            .unwrap()
            .offset,
        2
    );
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
    assert_eq!(
        catalog
            .advance_derived(&transfer.id, 1)
            .await
            .unwrap()
            .offset,
        3
    );
    catalog
        .advance_derived(&transfer.id, u64::MAX)
        .await
        .unwrap();
    let result = catalog
        .publish_derived(&transfer.id, ram_destination("copy"), |_, _| Ok(()))
        .await
        .unwrap();
    assert_eq!(
        (result.size, result.md5, result.crc32c),
        (source.size, source.md5, source.crc32c)
    );
    let (_, mut reader) = catalog
        .open_object("test-bucket", "copy", None, Default::default())
        .await
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    assert_eq!(bytes, b"data");
    assert_eq!(
        catalog.get_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    catalog
        .run_object(|connection| {
            let count: i64 = connection
                .prepare_cached("SELECT count(*) FROM derived_inputs")
                .unwrap()
                .query_row([], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 0);
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_derived_capacity_and_sql_failure_preserve_acknowledged_offset_and_old_destination() {
    let catalog = setup(8192, true).await;
    let original = put(&catalog, Default::default(), true).await.unwrap();
    let transfer = ram_transfer(&catalog).await;
    catalog.advance_derived(&transfer.id, 2).await.unwrap();
    let memory = catalog.0.memory.as_ref().unwrap();
    let mut blocker = memory.stage().unwrap();
    while blocker.write(&[0; 128]).is_ok() {}
    let mut reservations = Vec::new();
    while let Ok(stage) = memory.stage() {
        reservations.push(stage);
    }
    assert!(
        catalog
            .advance_derived(&transfer.id, u64::MAX)
            .await
            .is_err()
    );
    assert_eq!(catalog.get_derived(&transfer.id).await.unwrap().offset, 2);
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, Default::default())
            .await
            .unwrap(),
        original
    );
    drop(blocker);
    drop(reservations);
    catalog
        .advance_derived(&transfer.id, u64::MAX)
        .await
        .unwrap();
    let mut destination = ram_destination("value");
    destination.conditions.if_generation_match = Some(original.generation + 1);
    assert!(matches!(
        catalog
            .publish_derived(&transfer.id, destination, |_, _| Ok(()))
            .await,
        Err(ObjectError::PreconditionFailed)
    ));
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, Default::default())
            .await
            .unwrap(),
        original
    );
    assert_eq!(
        catalog.get_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    let transfer = ram_transfer(&catalog).await;
    catalog.advance_derived(&transfer.id, 2).await.unwrap();
    catalog.run_object(|connection| {
        connection.execute_batch("CREATE TEMP TRIGGER reject_derived_offset BEFORE UPDATE OF offset ON derived_transfers BEGIN SELECT RAISE(ABORT, 'injected SQL checkpoint failure'); END;").unwrap();
        Ok(())
    }).await.unwrap();
    assert!(
        catalog
            .advance_derived(&transfer.id, u64::MAX)
            .await
            .is_err()
    );
    assert_eq!(catalog.get_derived(&transfer.id).await.unwrap().offset, 2);
    catalog
        .run_object(|connection| {
            connection
                .execute_batch("DROP TRIGGER reject_derived_offset")
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    catalog
        .advance_derived(&transfer.id, u64::MAX)
        .await
        .unwrap();
    let copy = catalog
        .publish_derived(&transfer.id, ram_destination("copy"), |_, _| Ok(()))
        .await
        .unwrap();
    assert_eq!((copy.md5, copy.crc32c), (original.md5, original.crc32c));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_derived_publication_rolls_back_callbacks_and_serializes_destination_conditions() {
    let catalog = setup(8192, true).await;
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
    let original = put(&catalog, Default::default(), true).await.unwrap();
    let initial = catalog.next_notification().await.unwrap().unwrap();
    catalog.acknowledge_notification(initial.id).await.unwrap();
    let first = ram_transfer(&catalog).await;
    catalog.advance_derived(&first.id, u64::MAX).await.unwrap();
    let source_generation = original.generation;
    assert!(
        catalog
            .publish_derived(&first.id, ram_destination("value"), move |connection, _| {
                Catalog::delete_derived_source_in_transaction(
                    connection,
                    "test-bucket",
                    "value",
                    source_generation,
                    Default::default(),
                    &BucketAccess::Permissive,
                    std::time::SystemTime::now(),
                )?;
                Err(crate::storage::failure::object_unavailable(
                    std::io::Error::from(std::io::ErrorKind::StorageFull),
                ))
            })
            .await
            .is_err()
    );
    assert_eq!(catalog.get_derived(&first.id).await.unwrap().offset, 4);
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, Default::default())
            .await
            .unwrap(),
        original
    );
    assert!(catalog.next_notification().await.unwrap().is_none());
    let second = ram_transfer(&catalog).await;
    catalog.advance_derived(&second.id, u64::MAX).await.unwrap();
    let mut destination = ram_destination("value");
    destination.conditions.if_generation_match = Some(original.generation);
    let (left, right) = tokio::join!(
        catalog.publish_derived(&first.id, destination.clone(), |_, _| Ok(())),
        catalog.publish_derived(&second.id, destination, |_, _| Ok(()))
    );
    assert_ne!(left.is_ok(), right.is_ok());
    assert_eq!(
        left.err().or(right.err()),
        Some(ObjectError::PreconditionFailed)
    );
    let retained = catalog
        .get_object(
            "test-bucket",
            "value",
            Some(original.generation),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        (retained.generation, retained.md5, retained.crc32c),
        (original.generation, original.md5, original.crc32c)
    );
    assert!(retained.time_deleted.is_some());
    let mut events = std::collections::BTreeSet::new();
    while let Some(pending) = catalog.next_notification().await.unwrap() {
        let message: serde_json::Value = serde_json::from_str(&pending.message).unwrap();
        events.insert(
            message["messages"][0]["attributes"]["eventType"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        catalog.acknowledge_notification(pending.id).await.unwrap();
    }
    assert_eq!(
        events,
        std::collections::BTreeSet::from(["OBJECT_ARCHIVE".into(), "OBJECT_FINALIZE".into()])
    );
    assert_eq!(
        catalog.get_derived(&first.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    assert_eq!(
        catalog.get_derived(&second.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_derived_cancel_expiry_and_abandoned_begin_reclaim_their_owned_state() {
    let catalog = setup(4096, false).await;
    put(&catalog, Default::default(), true).await.unwrap();
    let transfer = ram_transfer(&catalog).await;
    catalog.advance_derived(&transfer.id, 2).await.unwrap();
    catalog.cancel_derived(&transfer.id).await.unwrap();
    assert_eq!(
        catalog.cancel_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    let transfer = ram_transfer(&catalog).await;
    catalog
        .maintain_derived_transfers_at(i64::MAX)
        .await
        .unwrap();
    assert_eq!(
        catalog.get_derived(&transfer.id).await,
        Err(ObjectError::DerivedTransferNotFound)
    );
    let source = catalog
        .resolve_derived_source(
            "test-bucket",
            "value",
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    let worker = catalog.clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let request = tokio::spawn(async move {
        worker
            .begin_derived_with_binding(
                vec![source.whole()],
                4,
                derived::TransferKind::Ephemeral,
                move |_, transfer| {
                    entered.send(transfer.id.clone()).unwrap();
                    blocked.recv().unwrap();
                    Ok(())
                },
            )
            .await
    });
    let id = started.await.unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if catalog.get_derived(&id).await == Err(ObjectError::DerivedTransferNotFound) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let transfer = ram_transfer(&catalog).await;
    catalog
        .advance_derived(&transfer.id, u64::MAX)
        .await
        .unwrap();
    catalog
        .publish_derived(&transfer.id, ram_destination("copy"), |_, _| Ok(()))
        .await
        .unwrap();
    catalog.close().await.unwrap();
    assert!(!catalog.is_healthy());
    assert!(catalog.advance_derived(&id, 1).await.is_err());
}

#[tokio::test]
async fn ram_history_and_readers_hold_capacity_until_the_last_byte_reference_drops() {
    let catalog = setup(520, true).await;
    let first = put(&catalog, ObjectConditions::default(), true)
        .await
        .unwrap();
    put(&catalog, ObjectConditions::default(), true)
        .await
        .unwrap();
    let (_, mut reader) = catalog
        .open_object(
            "test-bucket",
            "value",
            Some(first.generation),
            ObjectConditions::default(),
        )
        .await
        .unwrap();
    let response_chunk = reader.next_chunk().await.unwrap().unwrap();
    catalog
        .delete_object_authorized(
            "test-bucket",
            "value",
            Some(first.generation),
            ObjectConditions::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    drop(reader);
    assert!(
        put(&catalog, ObjectConditions::default(), true)
            .await
            .is_err()
    );
    drop(response_chunk);
    put(&catalog, ObjectConditions::default(), true)
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_incomplete_bodies_and_competing_conditions_preserve_committed_state() {
    let catalog = setup(200_000, false).await;
    let first = put(&catalog, ObjectConditions::default(), true)
        .await
        .unwrap();
    assert!(matches!(
        put(&catalog, ObjectConditions::default(), false).await,
        Err(ObjectError::BodyAborted(_))
    ));
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, ObjectConditions::default())
            .await
            .unwrap(),
        first
    );
    let condition = ObjectConditions {
        if_generation_match: Some(first.generation),
        ..Default::default()
    };
    let (left, right) = tokio::join!(
        put(&catalog, condition, true),
        put(&catalog, condition, true)
    );
    assert_ne!(left.is_ok(), right.is_ok());
    assert!(matches!(
        left.err().or(right.err()),
        Some(ObjectError::PreconditionFailed)
    ));
    let (_, reader) = catalog
        .open_object("test-bucket", "value", None, ObjectConditions::default())
        .await
        .unwrap();
    assert_eq!(
        reader.into_file().await.unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_catalog_pages_are_bounded_without_limiting_another_catalog() {
    let catalog = setup(1024, false).await;
    let original = put(&catalog, ObjectConditions::default(), true)
        .await
        .unwrap();
    let admin = catalog.admin(admin::Origin::Seed);
    exhaust_catalog_pages(&admin).await;
    assert!(catalog.get_bucket("test-bucket").await.unwrap().is_some());
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, ObjectConditions::default())
            .await
            .unwrap(),
        original
    );
    catalog
        .delete_object_authorized(
            "test-bucket",
            "value",
            None,
            ObjectConditions::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    admin.reset().await.unwrap();
    let name = BucketName::new("after-reset").unwrap();
    admin
        .ensure_bucket(BucketDefinition::default_for(&name, "local", &SystemClock).unwrap())
        .await
        .unwrap();
    let other = setup(1024, false).await;
    put(&other, ObjectConditions::default(), true)
        .await
        .unwrap();
    other.close().await.unwrap();
    catalog.close().await.unwrap();
}

async fn exhaust_catalog_pages(admin: &admin::Admin) {
    let mut exhausted = false;
    for index in 0..200 {
        let name = BucketName::new(format!("bucket-{index:04}")).unwrap();
        let mut record = BucketDefinition::default_for(&name, "local", &SystemClock)
            .unwrap()
            .into_record();
        record.labels = (0..64)
            .map(|index| (format!("label-{index:02}"), "v".repeat(63)))
            .collect();
        match admin
            .ensure_bucket(BucketDefinition::new(record).unwrap())
            .await
        {
            Ok(_) => {}
            Err(AdminError::DiskFull) => {
                exhausted = true;
                break;
            }
            Err(error) => panic!("unexpected capacity error: {error}"),
        }
    }
    assert!(exhausted, "SQLite page budget did not stop catalog growth");
}

#[tokio::test]
async fn ram_full_catalog_resets_many_objects_without_growing_a_deletion_journal() {
    let catalog = setup(1024 * 1024, false).await;
    let admin = catalog.admin(admin::Origin::Seed);
    let bucket = BucketName::new("test-bucket").unwrap();
    for index in 0..200 {
        admin
            .publish_object(
                &bucket,
                &admin::ObjectName::new(format!("object-{index:04}")).unwrap(),
                admin::Publication {
                    content_type: "text/plain".into(),
                    expected: ExpectedHashes::default(),
                    condition: None,
                    metadata: Default::default(),
                    max_bytes: 1024,
                    mode: admin::PublishMode::Replace,
                },
                body(true),
            )
            .await
            .unwrap();
    }
    let (_, mut reader) = catalog
        .open_object(
            "test-bucket",
            "object-0000",
            None,
            ObjectConditions::default(),
        )
        .await
        .unwrap();
    exhaust_catalog_pages(&admin).await;
    admin.reset().await.unwrap();
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"data"[..]);
    assert!(catalog.get_bucket("test-bucket").await.unwrap().is_none());
    admin
        .ensure_bucket(BucketDefinition::default_for(&bucket, "local", &SystemClock).unwrap())
        .await
        .unwrap();
    put(&catalog, ObjectConditions::default(), true)
        .await
        .unwrap();
    drop(reader);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_default_budgets_accept_one_thousand_small_objects_and_report_database_pages() {
    let config = crate::config::Config::default();
    let catalog = Catalog::open_memory_for_server(
        config.memory_max_bytes,
        config.memory_catalog_max_bytes,
        Arc::new(SystemClock),
        admin::LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    let admin = catalog.admin(admin::Origin::Seed);
    let bucket = BucketName::new("test-bucket").unwrap();
    admin
        .ensure_bucket(BucketDefinition::default_for(&bucket, "local", &SystemClock).unwrap())
        .await
        .unwrap();
    for index in 0..1000 {
        let name = admin::ObjectName::new(format!("object-{index:04}")).unwrap();
        let metadata = crate::object::ObjectMetadataUpdate {
            metadata: Some(Some(std::collections::BTreeMap::from([(
                "fixture".into(),
                Some("v".repeat(if index == 999 { 8185 } else { 128 })),
            )]))),
            ..Default::default()
        };
        let (sender, body) = mpsc::channel(2);
        sender
            .try_send(Ok(UploadChunk::Data(Bytes::from(vec![b'x'; 1024]))))
            .unwrap();
        sender.try_send(Ok(UploadChunk::End)).unwrap();
        drop(sender);
        admin
            .publish_object(
                &bucket,
                &name,
                admin::Publication {
                    content_type: "text/plain".into(),
                    expected: ExpectedHashes::default(),
                    condition: None,
                    metadata,
                    max_bytes: 1024,
                    mode: admin::PublishMode::Replace,
                },
                body,
            )
            .await
            .unwrap();
    }
    let pages = catalog
        .run(|connection| {
            let main: i64 = connection
                .pragma_query_value(None, "page_count", |row| row.get(0))
                .map_err(unavailable)?;
            let limit: i64 = connection
                .pragma_query_value(None, "max_page_count", |row| row.get(0))
                .map_err(unavailable)?;
            let page_size: i64 = connection
                .pragma_query_value(None, "page_size", |row| row.get(0))
                .map_err(unavailable)?;
            let temp: i64 = connection
                .pragma_query_value(Some("temp"), "page_count", |row| row.get(0))
                .map_err(unavailable)?;
            let filenames = connection
                .prepare_cached("PRAGMA database_list")
                .map_err(unavailable)?
                .query_map([], |row| row.get::<_, String>(2))
                .map_err(unavailable)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(unavailable)?;
            assert!(filenames.iter().all(String::is_empty));
            Ok((main, limit, page_size, temp))
        })
        .await
        .unwrap();
    assert!(pages.0 <= pages.1);
    println!(
        "RAM capacity: objects=1000 body_bytes_each=1024 metadata_value_bytes=128 (one=8185); main_pages={} page_size={} main_bytes={} main_max_pages={} temp_pages={}",
        pages.0,
        pages.2,
        pages.0 * pages.2,
        pages.1,
        pages.3
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_last_catalog_drop_wakes_and_releases_an_idle_notification_publisher() {
    let catalog = setup(1024, false).await;
    let wake = Arc::downgrade(&catalog.0.notification_wake);
    let (release, held) = std::sync::mpsc::channel();
    let (entered, running) = tokio::sync::oneshot::channel();
    {
        let blocked = catalog.run(move |_| {
            entered.send(()).unwrap();
            held.recv().unwrap();
            Ok(())
        });
        tokio::pin!(blocked);
        tokio::select! {
            result = &mut blocked => panic!("catalog gate completed early: {result:?}"),
            result = running => result.unwrap(),
        }
        catalog
            .start_notification_publisher(
                crate::notification::Endpoint::parse("127.0.0.1:1").unwrap(),
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while catalog.0.notification_attempts.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        release.send(()).unwrap();
        blocked.await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while Arc::strong_count(&catalog.0) != 1
            || catalog.0.notification_attempts.available_permits() != 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(catalog);
    tokio::time::timeout(Duration::from_secs(5), async {
        while wake.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn ram_pragmas_keep_main_and_temp_page_limits_and_memory_journaling() {
    let catalog = setup(1024, false).await;
    catalog
        .run(|connection| {
            for schema in [None, Some("temp")] {
                let pages: i64 = connection
                    .pragma_query_value(schema, "max_page_count", |row| row.get(0))
                    .map_err(unavailable)?;
                assert_eq!(pages, 256);
            }
            let mode: String = connection
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .map_err(unavailable)?;
            assert_eq!(mode, "memory");
            let temp: i64 = connection
                .pragma_query_value(None, "temp_store", |row| row.get(0))
                .map_err(unavailable)?;
            assert_eq!(temp, 2);
            let files = connection
                .prepare_cached("PRAGMA database_list")
                .map_err(unavailable)?
                .query_map([], |row| row.get::<_, String>(2))
                .map_err(unavailable)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(unavailable)?;
            assert!(files.iter().all(String::is_empty));
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_restore_shares_retained_bytes_and_preserves_deleted_generation() {
    struct Time;
    impl crate::clock::Clock for Time {
        fn now(&self) -> std::time::SystemTime {
            std::time::UNIX_EPOCH + std::time::Duration::new(1_000_000, 123_456_789)
        }
    }
    let catalog = Catalog::open_memory_for_server(
        520,
        1024 * 1024,
        Arc::new(Time),
        admin::LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    let mut record = BucketDefinition::default_for(&bucket, "local", &SystemClock)
        .unwrap()
        .into_record();
    record.soft_delete_policy = Some(crate::bucket::SoftDeletePolicy {
        retention_duration_seconds: 604800,
        effective_time: SystemClock.now(),
    });
    catalog
        .admin(admin::Origin::Seed)
        .ensure_bucket(BucketDefinition::new(record).unwrap())
        .await
        .unwrap();
    let first = put(&catalog, ObjectConditions::default(), true)
        .await
        .unwrap();
    let (_, mut reader) = catalog
        .open_object("test-bucket", "value", None, ObjectConditions::default())
        .await
        .unwrap();
    let id: String = catalog
        .run(|connection| {
            connection
                .prepare_cached(
                    "SELECT blob_id FROM objects WHERE bucket='test-bucket' AND name='value'",
                )
                .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
                .map_err(unavailable)
        })
        .await
        .unwrap();
    let blob = super::super::blob_id::BlobId::parse(&id).unwrap();
    let paths = [
        blob.staging_path(Path::new("")),
        blob.blob_path(Path::new("")),
    ];
    assert!(paths.iter().all(|path| !path.exists()));
    catalog
        .delete_object_authorized(
            "test-bucket",
            "value",
            Some(first.generation),
            ObjectConditions::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    let before = catalog
        .get_soft_deleted_object_authorized(
            "test-bucket",
            "value",
            first.generation,
            ObjectConditions::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    let restored = catalog
        .restore_object_authorized(
            "test-bucket",
            "value",
            first.generation,
            ObjectConditions::default(),
            &BucketAccess::Permissive,
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert!(restored.generation > first.generation);
    assert_eq!(restored.metageneration, 1);
    assert_eq!(
        restored
            .time_created
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos(),
        123_456_000
    );
    assert!(restored.soft_delete_time.is_none());
    assert!(restored.hard_delete_time.is_none());
    assert_eq!(restored.md5, first.md5);
    assert_eq!(restored.crc32c, first.crc32c);
    assert_eq!(restored.metadata, first.metadata);
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, Default::default())
            .await
            .unwrap(),
        restored
    );
    assert_eq!(
        catalog
            .get_soft_deleted_object_authorized(
                "test-bucket",
                "value",
                first.generation,
                ObjectConditions::default(),
                &BucketAccess::Permissive
            )
            .await
            .unwrap(),
        before
    );
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"data"[..]);
    assert!(paths.iter().all(|path| !path.exists()));
    let pending: i64 = catalog
        .run(|connection| {
            connection
                .prepare_cached("SELECT count(*) FROM pending_blob_deletions")
                .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
                .map_err(unavailable)
        })
        .await
        .unwrap();
    assert_eq!(pending, 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_restore_quota_and_failed_sql_keep_source_and_retry_capacity() {
    for budget in [400, 520] {
        let catalog = setup(budget, false).await;
        catalog
            .run_object(|connection| {
                let policy = crate::bucket::SoftDeletePolicy {
                    retention_duration_seconds: 604800,
                    effective_time: SystemClock.now(),
                };
                connection
                    .prepare_cached("UPDATE buckets SET soft_delete_policy=?1")
                    .unwrap()
                    .execute([serde_json::to_string(&policy).unwrap()])
                    .unwrap();
                Ok(())
            })
            .await
            .unwrap();
        let first = put(&catalog, Default::default(), true).await.unwrap();
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
        if budget == 520 {
            catalog.run_object(|connection| {
                connection.execute_batch("CREATE TEMP TRIGGER reject_ram_restore BEFORE INSERT ON objects BEGIN SELECT RAISE(ABORT,'injected restore failure'); END;").unwrap();
                Ok(())
            }).await.unwrap();
        }
        assert!(
            catalog
                .restore_object_authorized(
                    "test-bucket",
                    "value",
                    first.generation,
                    Default::default(),
                    &BucketAccess::Permissive,
                    &BucketAccess::Permissive
                )
                .await
                .is_err()
        );
        assert!(matches!(
            catalog
                .get_object("test-bucket", "value", None, Default::default())
                .await,
            Err(ObjectError::ObjectNotFound)
        ));
        let retained = catalog
            .get_soft_deleted_object_authorized(
                "test-bucket",
                "value",
                first.generation,
                Default::default(),
                &BucketAccess::Permissive,
            )
            .await
            .unwrap();
        assert_eq!(retained.md5, first.md5);
        let pending: i64 = catalog
            .run_object(|connection| {
                connection
                    .prepare_cached("SELECT count(*) FROM pending_blob_deletions")
                    .unwrap()
                    .query_row([], |row| row.get(0))
                    .map_err(crate::storage::failure::object_unavailable)
            })
            .await
            .unwrap();
        assert_eq!(pending, 0);
        if budget == 520 {
            catalog
                .run_object(|connection| {
                    connection
                        .execute_batch("DROP TRIGGER reject_ram_restore")
                        .unwrap();
                    Ok(())
                })
                .await
                .unwrap();
            let restored = catalog
                .restore_object_authorized(
                    "test-bucket",
                    "value",
                    first.generation,
                    Default::default(),
                    &BucketAccess::Permissive,
                    &BucketAccess::Permissive,
                )
                .await
                .unwrap();
            assert!(restored.generation > first.generation);
            assert!(matches!(
                catalog
                    .restore_object_authorized(
                        "test-bucket",
                        "value",
                        first.generation,
                        ObjectConditions {
                            if_generation_match: Some(0),
                            ..Default::default()
                        },
                        &BucketAccess::Permissive,
                        &BucketAccess::Permissive
                    )
                    .await,
                Err(ObjectError::PreconditionFailed)
            ));
            assert_eq!(
                catalog
                    .get_object("test-bucket", "value", None, Default::default())
                    .await
                    .unwrap(),
                restored
            );
        }
        catalog.close().await.unwrap();
    }
}
