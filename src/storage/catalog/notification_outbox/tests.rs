use super::*;
use crate::{
    bucket::BucketAccess,
    notification::NotificationConfig,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord, UploadChunk},
    test_support::TestDir,
};
use bytes::Bytes;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::UNIX_EPOCH};

async fn setup(dir: &TestDir, versioning: bool) -> Catalog {
    setup_with_clock(
        dir,
        versioning,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .await
}

async fn setup_with_clock(
    dir: &TestDir,
    versioning: bool,
    clock: std::sync::Arc<dyn crate::clock::Clock>,
) -> Catalog {
    let catalog = Catalog::open_for_server(&dir.0, clock, super::super::admin::LocalAdmin::Enabled)
        .await
        .unwrap();
    let bucket = crate::json::parse_insert(
        json!({"name":"test-bucket", "versioning":{"enabled":versioning}})
            .to_string()
            .as_bytes(),
        "local-project",
        UNIX_EPOCH,
    )
    .unwrap();
    catalog.insert_bucket(bucket).await.unwrap();
    catalog
        .insert_notification(
            "test-bucket",
            NotificationConfig {
                topic: "//pubsub.googleapis.com/projects/p/topics/topic".into(),
                payload_format: "JSON_API_V1".into(),
                event_types: vec![],
                custom_attributes: BTreeMap::from([("team".into(), "test".into())]),
                object_name_prefix: "prefix/".into(),
            },
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    catalog
}

async fn put(
    catalog: &Catalog,
    name: &str,
    conditions: ObjectConditions,
) -> Result<ObjectRecord, ObjectError> {
    let (sender, body) = tokio::sync::mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(Bytes::from_static(b"body"))))
        .await
        .unwrap();
    sender.send(Ok(UploadChunk::End)).await.unwrap();
    drop(sender);
    catalog
        .put_object(
            "test-bucket",
            name,
            "text/plain".into(),
            ExpectedHashes::default(),
            conditions,
            10,
            body,
        )
        .await
}

async fn drain(catalog: &Catalog) -> Vec<Value> {
    let mut messages = vec![];
    while let Some(pending) = catalog.next_notification().await.unwrap() {
        messages
            .push(serde_json::from_str::<Value>(&pending.message).unwrap()["messages"][0].clone());
        catalog.acknowledge_notification(pending.id).await.unwrap();
    }
    messages
}

async fn initialization_put(
    admin: &crate::storage::admin::Admin,
    condition: Option<crate::storage::admin::GenerationCondition>,
    expected: ExpectedHashes,
) -> Result<ObjectRecord, crate::storage::admin::AdminError> {
    let (sender, body) = tokio::sync::mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(Bytes::from_static(b"body"))))
        .await
        .unwrap();
    sender.send(Ok(UploadChunk::End)).await.unwrap();
    drop(sender);
    admin
        .publish_object(
            &crate::storage::admin::BucketName::new("test-bucket").unwrap(),
            &crate::storage::admin::ObjectName::new("prefix/item").unwrap(),
            crate::storage::admin::Publication {
                content_type: "text/plain".into(),
                metadata: Default::default(),
                expected,
                condition,
                max_bytes: 10,
                mode: crate::storage::admin::PublishMode::Replace,
            },
            body,
        )
        .await
}

#[tokio::test]
async fn initial_publications_suppress_global_and_json_events_without_changing_runtime_writes() {
    use crate::{
        clock::SystemClock,
        notification::NotificationMode,
        storage::admin::{AdminError, GenerationCondition, LocalAdmin, Origin},
    };
    for versioning in [false, true] {
        for memory in [false, true] {
            let dir = TestDir::new();
            let catalog = if memory {
                let catalog = Catalog::open_memory_for_server(
                    1024 * 1024,
                    4 * 1024 * 1024,
                    std::sync::Arc::new(SystemClock),
                    LocalAdmin::Enabled,
                )
                .await
                .unwrap();
                catalog
                    .insert_bucket(
                        crate::json::parse_insert(
                            json!({"name":"test-bucket","versioning":{"enabled":versioning}})
                                .to_string()
                                .as_bytes(),
                            "local-project",
                            UNIX_EPOCH,
                        )
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                catalog
                    .insert_notification(
                        "test-bucket",
                        NotificationConfig {
                            topic: "//pubsub.googleapis.com/projects/p/topics/topic".into(),
                            payload_format: "JSON_API_V1".into(),
                            event_types: vec![],
                            custom_attributes: BTreeMap::new(),
                            object_name_prefix: "prefix/".into(),
                        },
                        &BucketAccess::Permissive,
                    )
                    .await
                    .unwrap();
                catalog
            } else {
                setup(&dir, versioning).await
            };
            catalog
                .configure_notifications(&Runtime {
                    endpoint: None,
                    global: Some(GlobalConfig {
                        bucket: "test-bucket".into(),
                        notification: NotificationConfig {
                            topic: "//pubsub.googleapis.com/projects/p/topics/topic".into(),
                            payload_format: "JSON_API_V1".into(),
                            event_types: vec![],
                            custom_attributes: BTreeMap::new(),
                            object_name_prefix: "prefix/".into(),
                        },
                    }),
                })
                .await
                .unwrap();
            let seed_directory = TestDir::new();
            std::fs::create_dir_all(seed_directory.0.join("test-bucket/prefix")).unwrap();
            std::fs::write(
                seed_directory.0.join("test-bucket/prefix/seed"),
                b"seed bytes",
            )
            .unwrap();
            let config = crate::config::Config {
                backend: if memory {
                    crate::config::Backend::Memory
                } else {
                    crate::config::Backend::Disk
                },
                data_dir: dir.0.clone(),
                seed_dir: Some(seed_directory.0.clone()),
                ..Default::default()
            };
            crate::seed::import(&catalog, &config, &SystemClock)
                .await
                .unwrap();
            assert!(
                drain(&catalog).await.is_empty(),
                "startup seed emitted notifications"
            );
            let seed = catalog.admin(Origin::Seed);
            let first = initialization_put(&seed, None, Default::default())
                .await
                .unwrap();
            assert!(drain(&catalog).await.is_empty());
            let runtime = catalog.local_admin().unwrap();
            let initial = runtime
                .clone()
                .with_notification_mode(NotificationMode::Suppress);
            let second = initialization_put(&initial, None, Default::default())
                .await
                .unwrap();
            assert!(second.generation > first.generation);
            assert!(
                drain(&catalog).await.is_empty(),
                "initial overwrite emitted an archive/delete/finalize event"
            );
            let third = initialization_put(&runtime, None, Default::default())
                .await
                .unwrap();
            let messages = drain(&catalog).await;
            let retirement = if versioning {
                "OBJECT_ARCHIVE"
            } else {
                "OBJECT_DELETE"
            };
            let types: Vec<_> = messages
                .iter()
                .map(|message| message["attributes"]["eventType"].as_str().unwrap())
                .collect();
            assert_eq!(
                types,
                [retirement, retirement, "OBJECT_FINALIZE", "OBJECT_FINALIZE"]
            );
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| message["attributes"].get("notificationConfig").is_some())
                    .count(),
                2
            );
            assert_eq!(
                messages[0]["attributes"]["objectGeneration"],
                second.generation.to_string()
            );
            assert_eq!(
                messages[2]["attributes"]["objectGeneration"],
                third.generation.to_string()
            );
            for admin in [&runtime, &initial] {
                assert_eq!(
                    initialization_put(
                        admin,
                        Some(GenerationCondition::Absent),
                        Default::default()
                    )
                    .await,
                    Err(AdminError::Precondition)
                );
                assert_eq!(
                    initialization_put(
                        admin,
                        None,
                        ExpectedHashes {
                            md5: Some([0; 16]),
                            ..Default::default()
                        }
                    )
                    .await,
                    Err(AdminError::Checksum)
                );
            }
            assert!(drain(&catalog).await.is_empty());
            assert_eq!(
                catalog
                    .get_object("test-bucket", "prefix/item", None, Default::default())
                    .await
                    .unwrap()
                    .generation,
                third.generation
            );
            put(&catalog, "prefix/item", Default::default())
                .await
                .unwrap();
            assert_eq!(
                drain(&catalog).await.len(),
                4,
                "ordinary API writes lost their notifications"
            );
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn finalize_archive_delete_snapshots_survive_restart_and_failed_mutations_emit_nothing() {
    let dir = TestDir::new();
    let catalog = setup(&dir, true).await;
    put(&catalog, "ignored", Default::default()).await.unwrap();
    assert!(catalog.next_notification().await.unwrap().is_none());
    let first = put(&catalog, "prefix/item", Default::default())
        .await
        .unwrap();
    assert_eq!(
        put(
            &catalog,
            "prefix/item",
            ObjectConditions {
                if_generation_match: Some(0),
                ..Default::default()
            }
        )
        .await,
        Err(ObjectError::PreconditionFailed)
    );
    let second = put(&catalog, "prefix/item", Default::default())
        .await
        .unwrap();
    catalog
        .delete_object("test-bucket", "prefix/item", None, Default::default())
        .await
        .unwrap();
    catalog
        .delete_object(
            "test-bucket",
            "prefix/item",
            Some(first.generation),
            Default::default(),
        )
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let messages = drain(&catalog).await;
    let types: Vec<_> = messages
        .iter()
        .map(|message| message["attributes"]["eventType"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "OBJECT_FINALIZE",
            "OBJECT_ARCHIVE",
            "OBJECT_FINALIZE",
            "OBJECT_ARCHIVE",
            "OBJECT_DELETE"
        ]
    );
    assert_eq!(
        messages[1]["attributes"]["overwrittenByGeneration"],
        second.generation.to_string()
    );
    assert_eq!(
        messages[2]["attributes"]["overwroteGeneration"],
        first.generation.to_string()
    );
    assert_eq!(
        messages[0]["attributes"]["notificationConfig"],
        "projects/_/buckets/test-bucket/notificationConfigs/1"
    );
    assert!(
        messages
            .iter()
            .all(|message| message["attributes"]["team"] == "test")
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn full_outbox_rolls_back_the_object_mutation() {
    let dir = TestDir::new();
    let catalog = setup(&dir, false).await;
    catalog.run(|connection| {
        let transaction = connection.transaction().map_err(unavailable)?;
        for id in 1..=4096 {
            transaction.prepare_cached("INSERT INTO notification_outbox VALUES (?1, '//pubsub.googleapis.com/projects/p/topics/topic', '{\"messages\":[]}', 0, 0)").and_then(|mut statement| statement.execute([id])).map_err(unavailable)?;
        }
        transaction.prepare_cached("UPDATE notification_outbox_state SET last_id = 4096").and_then(|mut statement| statement.execute([])).map_err(unavailable)?;
        transaction.commit().map_err(unavailable)
    }).await.unwrap();
    assert!(matches!(
        put(&catalog, "prefix/item", Default::default()).await,
        Err(ObjectError::Unavailable(_))
    ));
    assert_eq!(
        catalog
            .get_object("test-bucket", "prefix/item", None, Default::default())
            .await,
        Err(ObjectError::ObjectNotFound)
    );
    let (generation, acl_rows): (i64, i64) = catalog.run(|connection| {
        connection.prepare_cached("SELECT last_generation, (SELECT count(*) FROM acl_object_entries) FROM object_generation")
            .and_then(|mut statement| statement.query_row([], |row| Ok((row.get(0)?, row.get(1)?)))).map_err(unavailable)
    }).await.unwrap();
    assert_eq!((generation, acl_rows), (0, 0));
    catalog.acknowledge_notification(1).await.unwrap();
    put(&catalog, "prefix/item", Default::default())
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn metadata_payload_filters_and_deleted_configuration_use_transaction_time_snapshots() {
    let dir = TestDir::new();
    let catalog = setup(&dir, false).await;
    let first = put(&catalog, "prefix/item", Default::default())
        .await
        .unwrap();
    drain(&catalog).await;
    let config = NotificationConfig {
        topic: "//pubsub.googleapis.com/projects/p/topics/metadata".into(),
        payload_format: "NONE".into(),
        event_types: vec!["OBJECT_METADATA_UPDATE".into()],
        custom_attributes: BTreeMap::from([
            ("eventType".into(), "forged".into()),
            ("payloadFormat".into(), "forged".into()),
        ]),
        object_name_prefix: "prefix/".into(),
    };
    catalog
        .insert_notification("test-bucket", config, &BucketAccess::Permissive)
        .await
        .unwrap();
    let update = crate::object::ObjectMetadataUpdate {
        content_type: Some(Some("application/test".into())),
        ..Default::default()
    };
    let changed = catalog
        .update_object_metadata(
            "test-bucket",
            "prefix/item",
            None,
            Default::default(),
            update.clone(),
            |_| Ok(()),
        )
        .await
        .unwrap();
    assert_eq!(changed.generation, first.generation);
    assert_eq!(changed.metageneration, 2);
    assert!(
        catalog
            .update_object_metadata(
                "test-bucket",
                "prefix/item",
                None,
                ObjectConditions {
                    if_metageneration_match: Some(1),
                    ..Default::default()
                },
                update,
                |_| Ok(())
            )
            .await
            .is_err()
    );
    assert!(
        catalog
            .delete_notification("test-bucket", "1", &BucketAccess::Permissive)
            .await
            .unwrap()
    );
    assert!(
        catalog
            .delete_notification("test-bucket", "2", &BucketAccess::Permissive)
            .await
            .unwrap()
    );
    let messages = drain(&catalog).await;
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .all(|message| message["attributes"]["eventType"] == "OBJECT_METADATA_UPDATE")
    );
    let mut expected =
        crate::json::object_resource(&changed, "", crate::json::Projection::NoAcl).unwrap();
    expected.as_object_mut().unwrap().remove("selfLink");
    expected.as_object_mut().unwrap().remove("mediaLink");
    assert_eq!(
        messages[0]["data"],
        crate::json::base64(expected.to_string().as_bytes())
    );
    assert!(messages[1].get("data").is_none());
    assert_eq!(messages[1]["attributes"]["payloadFormat"], "NONE");
    put(&catalog, "prefix/item", Default::default())
        .await
        .unwrap();
    assert!(catalog.next_notification().await.unwrap().is_none());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn publication_failures_and_crash_snapshots_keep_objects_and_events_atomic() {
    use super::super::fault::{self, Fault, Step};
    for step in [
        Step::BodyWrite,
        Step::BodyFlush,
        Step::BodySync,
        Step::BeforeCommit,
    ] {
        let dir = TestDir::new();
        let catalog = setup(&dir, false).await;
        fault::arm(&dir.0, step, 0, Fault::Io(std::io::ErrorKind::Other));
        assert!(
            put(&catalog, "prefix/item", Default::default())
                .await
                .is_err(),
            "{step:?}"
        );
        assert!(catalog.next_notification().await.unwrap().is_none());
        catalog.close().await.unwrap();
        let reopened = Catalog::open(&dir.0).await.unwrap();
        assert!(reopened.next_notification().await.unwrap().is_none());
        assert_eq!(
            reopened
                .get_object("test-bucket", "prefix/item", None, Default::default())
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        reopened.close().await.unwrap();
    }
    for step in [Step::BeforeCommit, Step::AfterCommit] {
        let dir = TestDir::new();
        let snapshot = TestDir::new();
        let catalog = setup(&dir, false).await;
        fault::arm(&dir.0, step, 0, Fault::Snapshot(snapshot.0.clone()));
        put(&catalog, "prefix/item", Default::default())
            .await
            .unwrap();
        assert_eq!(fault::pending(&dir.0), 0);
        catalog.close().await.unwrap();
        let recovered = Catalog::open(&snapshot.0).await.unwrap();
        assert_eq!(
            recovered
                .get_object("test-bucket", "prefix/item", None, Default::default())
                .await
                .is_ok(),
            step == Step::AfterCommit
        );
        assert_eq!(
            recovered.next_notification().await.unwrap().is_some(),
            step == Step::AfterCommit
        );
        recovered.close().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn power_loss_keeps_publication_and_outbox_in_the_same_commit() {
    use super::super::fault::{self, Fault, Step};
    for step in [
        Step::BeforePublish,
        Step::AfterPublish,
        Step::BeforeCommit,
        Step::AfterCommit,
    ] {
        let dir = TestDir::new();
        let snapshot = TestDir::new();
        let catalog = setup(&dir, false).await;
        fault::arm(&dir.0, step, 0, Fault::PowerLoss(snapshot.0.clone()));
        put(&catalog, "prefix/item", Default::default())
            .await
            .unwrap();
        assert_eq!(fault::pending(&dir.0), 0);
        catalog.close().await.unwrap();
        let recovered = Catalog::open(&snapshot.0).await.unwrap();
        let committed = step == Step::AfterCommit;
        assert_eq!(
            recovered
                .get_object("test-bucket", "prefix/item", None, Default::default())
                .await
                .is_ok(),
            committed,
            "{step:?}"
        );
        assert_eq!(
            recovered.next_notification().await.unwrap().is_some(),
            committed,
            "{step:?}"
        );
        recovered.close().await.unwrap();
    }
}

async fn accept_publish(listener: &tokio::net::TcpListener) -> (tokio::net::TcpStream, String) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let (socket, _) = listener.accept().await.unwrap();
    let mut reader = BufReader::new(socket);
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    assert!((1..=131072).contains(&length));
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await.unwrap();
    (reader.into_inner(), String::from_utf8(bytes).unwrap())
}

async fn wait_for_pending(catalog: &Catalog, check: impl Fn(&Option<Pending>) -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if check(&catalog.next_notification().await.unwrap()) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn missing_acknowledgement_retries_the_same_snapshot_after_restart() {
    use tokio::io::AsyncWriteExt;
    let dir = TestDir::new();
    let catalog = setup(&dir, false).await;
    put(&catalog, "prefix/item", Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        crate::notification::Endpoint::parse(&listener.local_addr().unwrap().to_string()).unwrap();
    catalog
        .start_notification_publisher(endpoint.clone())
        .unwrap();
    let (socket, first) =
        tokio::time::timeout(std::time::Duration::from_secs(3), accept_publish(&listener))
            .await
            .unwrap();
    drop(socket);
    wait_for_pending(&catalog, |pending| {
        pending
            .as_ref()
            .is_some_and(|pending| pending.attempts == 1)
    })
    .await;
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog.next_notification().await.unwrap().unwrap().attempts,
        1
    );
    catalog.start_notification_publisher(endpoint).unwrap();
    let (mut socket, second) =
        tokio::time::timeout(std::time::Duration::from_secs(3), accept_publish(&listener))
            .await
            .unwrap();
    assert_eq!(first, second);
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 23\r\n\r\n{\"messageIds\":[\"done\"]}")
        .await
        .unwrap();
    drop(socket);
    wait_for_pending(&catalog, Option::is_none).await;
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn reset_is_busy_during_network_attempt_and_clears_retry_payloads_afterward() {
    use tokio::io::AsyncWriteExt;
    struct ControlledClock(std::sync::atomic::AtomicU64);
    impl crate::clock::Clock for ControlledClock {
        fn now(&self) -> std::time::SystemTime {
            UNIX_EPOCH
                + std::time::Duration::from_micros(self.0.load(std::sync::atomic::Ordering::SeqCst))
        }
    }
    let dir = TestDir::new();
    let clock = std::sync::Arc::new(ControlledClock(std::sync::atomic::AtomicU64::new(
        42_000_000,
    )));
    let catalog = setup_with_clock(&dir, false, clock.clone()).await;
    put(&catalog, "prefix/item", Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        crate::notification::Endpoint::parse(&listener.local_addr().unwrap().to_string()).unwrap();
    catalog.start_notification_publisher(endpoint).unwrap();
    let (mut socket, _) =
        tokio::time::timeout(std::time::Duration::from_secs(3), accept_publish(&listener))
            .await
            .unwrap();
    let admin = catalog.local_admin().unwrap();
    assert!(matches!(
        admin.reset().await,
        Err(super::super::admin::AdminError::Busy { .. })
    ));
    socket
        .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 2\r\n\r\n{}")
        .await
        .unwrap();
    drop(socket);
    wait_for_pending(&catalog, |pending| {
        pending
            .as_ref()
            .is_some_and(|pending| pending.attempts == 1)
    })
    .await;
    // SQL retry visibility precedes lease destruction. Join the actual attempt,
    // then fence completed SQL jobs before checking reset admission without polling.
    let attempt = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        catalog.0.notification_attempts.clone().acquire_owned(),
    )
    .await
    .unwrap()
    .unwrap();
    let completed = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        catalog.0.activity.clone().write_owned(),
    )
    .await
    .unwrap();
    drop(completed);
    tokio::time::timeout(std::time::Duration::from_secs(3), admin.reset())
        .await
        .unwrap()
        .unwrap();
    drop(attempt);
    assert!(catalog.next_notification().await.unwrap().is_none());
    // Release the attempt fence and advance beyond backoff before observing the
    // listener; a blocked publisher must not make the no-resend assertion pass.
    clock
        .0
        .store(42_200_000, std::sync::atomic::Ordering::SeqCst);
    catalog.0.notification_wake.notify_one();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(150), listener.accept())
            .await
            .is_err()
    );
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert!(reopened.next_notification().await.unwrap().is_none());
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn byte_budget_and_count_budget_both_rollback_the_mutation() {
    let dir = TestDir::new();
    let catalog = setup(&dir, false).await;
    catalog
        .run(|connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            let message = json!({"padding": "x".repeat(130_000)}).to_string();
            let topic = "//pubsub.googleapis.com/projects/p/topics/topic";
            let size = message.len() + topic.len();
            let count = 16_777_216 / size;
            for id in 1..=count {
                transaction
                    .prepare_cached("INSERT INTO notification_outbox VALUES (?1, ?2, ?3, 0, 0)")
                    .and_then(|mut statement| statement.execute(params![id as i64, topic, message]))
                    .map_err(unavailable)?;
            }
            // Leave less capacity than even the smallest notification, without exceeding a row bound.
            let remainder = 16_777_216 - count * size;
            let last = json!({"padding": "x".repeat(remainder - topic.len() - 14)}).to_string();
            transaction
                .prepare_cached("INSERT INTO notification_outbox VALUES (?1, ?2, ?3, 0, 0)")
                .and_then(|mut statement| statement.execute(params![count as i64 + 1, topic, last]))
                .map_err(unavailable)?;
            transaction
                .prepare_cached("UPDATE notification_outbox_state SET last_id = ?1")
                .and_then(|mut statement| statement.execute([count as i64 + 1]))
                .map_err(unavailable)?;
            transaction.commit().map_err(unavailable)
        })
        .await
        .unwrap();
    let result = put(&catalog, "prefix/item", Default::default()).await;
    let error = result.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("notification outbox limit reached")
    );
    assert_eq!(
        catalog
            .get_object("test-bucket", "prefix/item", None, Default::default())
            .await,
        Err(ObjectError::ObjectNotFound)
    );
    catalog.acknowledge_notification(1).await.unwrap();
    put(&catalog, "prefix/item", Default::default())
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn retry_deadline_allows_other_messages_and_close_waits_for_the_active_attempt() {
    use tokio::io::AsyncWriteExt;
    let dir = TestDir::new();
    let catalog = setup(&dir, false).await;
    put(&catalog, "prefix/first", Default::default())
        .await
        .unwrap();
    put(&catalog, "prefix/second", Default::default())
        .await
        .unwrap();
    let first = catalog.next_notification().await.unwrap().unwrap();
    catalog
        .retry_notification(
            first.id,
            unix_micros(std::time::SystemTime::now()).unwrap() + 30_000_000,
        )
        .await
        .unwrap();
    let second = catalog.next_notification().await.unwrap().unwrap();
    assert_ne!(first.id, second.id);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    catalog
        .start_notification_publisher(
            crate::notification::Endpoint::parse(&listener.local_addr().unwrap().to_string())
                .unwrap(),
        )
        .unwrap();
    let (mut socket, message) =
        tokio::time::timeout(std::time::Duration::from_secs(3), accept_publish(&listener))
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&message).unwrap()["messages"][0]["attributes"]["objectId"],
        "prefix/second"
    );
    {
        let closing = catalog.close();
        tokio::pin!(closing);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut closing)
                .await
                .is_err()
        );
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 23\r\n\r\n{\"messageIds\":[\"done\"]}")
            .await
            .unwrap();
        drop(socket);
        tokio::time::timeout(std::time::Duration::from_secs(3), &mut closing)
            .await
            .unwrap()
            .unwrap();
    }
    let reopened = Catalog::open(&dir.0).await.unwrap();
    // Closing admission during an unacknowledged attempt retains both snapshots for replay.
    assert_eq!(drain(&reopened).await.len(), 2);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn schema_migration_preserves_configs_and_inconsistent_accounting_fails_read_only() {
    let dir = TestDir::new();
    let catalog = setup(&dir, false).await;
    catalog.close().await.unwrap();
    let path = dir.0.join("catalog.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    super::super::test_support::downgrade_bucket_timestamp_schema(&connection);
    connection.execute_batch("DROP TABLE notification_outbox; DROP TABLE notification_outbox_state; PRAGMA user_version = 37;").unwrap();
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog
            .list_notifications("test-bucket", &BucketAccess::Permissive)
            .await
            .unwrap()
            .len(),
        1
    );
    put(&catalog, "prefix/migrated", Default::default())
        .await
        .unwrap();
    assert!(catalog.next_notification().await.unwrap().is_some());
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute("UPDATE notification_outbox_state SET pending_count = 0", [])
        .unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    assert!(Catalog::open(&dir.0).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
