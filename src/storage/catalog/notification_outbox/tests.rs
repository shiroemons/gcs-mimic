use super::*;
use crate::storage::ObjectUpload;
use crate::storage::catalog::sql;
use crate::storage::test_fixtures::{
    PERMISSIVE, body, live, permissive_upload, publication, version,
};
use crate::{
    notification::NotificationConfig,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord},
    test_support::{TestDir, eventually_async},
};
use bytes::Bytes;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::UNIX_EPOCH};

#[derive(Clone, Copy)]
enum VersionHistory {
    Preserve,
    Discard,
}

async fn setup(dir: &TestDir, history: VersionHistory) -> Catalog {
    setup_with_clock(dir, history, std::sync::Arc::new(crate::clock::SystemClock)).await
}

async fn setup_with_clock(
    dir: &TestDir,
    history: VersionHistory,
    clock: std::sync::Arc<dyn crate::clock::Clock>,
) -> Catalog {
    let catalog = Catalog::open_for_server(&dir.0, clock, super::super::admin::LocalAdmin::Enabled)
        .await
        .unwrap();
    let versioning = matches!(history, VersionHistory::Preserve);
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
                topic: "//pubsub.googleapis.com/projects/p/topics/topic"
                    .parse()
                    .unwrap(),
                payload_format: crate::notification::PayloadFormat::JsonApiV1,
                event_types: crate::notification::EventTypes::default(),
                custom_attributes: BTreeMap::from([("team".into(), "test".into())]),
                object_name_prefix: "prefix/".into(),
            },
            PERMISSIVE,
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
    let body = body(vec![Bytes::from_static(b"body")]);
    catalog
        .put_object(
            ObjectUpload {
                conditions,
                ..permissive_upload("test-bucket", name, 10)
            },
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
    let body = body(vec![Bytes::from_static(b"body")]);
    admin
        .publish_object(
            &crate::storage::admin::BucketName::new("test-bucket").unwrap(),
            &crate::storage::admin::ObjectName::new("prefix/item").unwrap(),
            crate::storage::admin::Publication {
                expected,
                condition,
                ..publication(10)
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
    for history in [VersionHistory::Discard, VersionHistory::Preserve] {
        let versioning = matches!(history, VersionHistory::Preserve);
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
                            topic: "//pubsub.googleapis.com/projects/p/topics/topic"
                                .parse()
                                .unwrap(),
                            payload_format: crate::notification::PayloadFormat::JsonApiV1,
                            event_types: crate::notification::EventTypes::default(),
                            custom_attributes: BTreeMap::new(),
                            object_name_prefix: "prefix/".into(),
                        },
                        PERMISSIVE,
                    )
                    .await
                    .unwrap();
                catalog
            } else {
                setup(&dir, history).await
            };
            catalog
                .configure_notifications(&Runtime {
                    endpoint: None,
                    global: Some(GlobalConfig {
                        bucket: "test-bucket".into(),
                        notification: NotificationConfig {
                            topic: "//pubsub.googleapis.com/projects/p/topics/topic"
                                .parse()
                                .unwrap(),
                            payload_format: crate::notification::PayloadFormat::JsonApiV1,
                            event_types: crate::notification::EventTypes::default(),
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
                    .get_object(
                        live("test-bucket", "prefix/item"),
                        Default::default(),
                        PERMISSIVE
                    )
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
    let catalog = setup(&dir, VersionHistory::Preserve).await;
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
        .delete_object(
            live("test-bucket", "prefix/item"),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    catalog
        .delete_object(
            version("test-bucket", "prefix/item", first.generation),
            Default::default(),
            PERMISSIVE,
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

const TOPIC: &str = "//pubsub.googleapis.com/projects/p/topics/topic";
const MAX_MESSAGES: i64 = 4096;
const MAX_BYTES: usize = 16_777_216;

/// Fills the outbox with `count` messages named `old-1` onward, as if Pub/Sub had been down.
async fn fill_outbox(catalog: &Catalog, count: i64) {
    catalog
        .run(move |connection| {
            let transaction = connection.transaction()?;
            sql::exec(
                &transaction,
                "WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM ids WHERE n < ?1) INSERT INTO notification_outbox SELECT n, ?2, json_object('messages', json_array(json_object('attributes', json_object('objectId', 'old-' || n)))), 0, 0 FROM ids",
                params![count, TOPIC],
            )?;
            sql::exec(
                &transaction,
                "UPDATE notification_outbox_state SET last_id = ?1",
                [count],
            )?;
            transaction.commit().map_err(unavailable)
        })
        .await
        .unwrap();
}

/// How many messages are pending, and the oldest one's ID.
async fn pending(catalog: &Catalog) -> (i64, Option<i64>) {
    catalog
        .run(|connection| {
            sql::row(
                connection,
                "SELECT count(*), min(id) FROM notification_outbox",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(unavailable)
        })
        .await
        .unwrap()
}

fn object_id(message: &Value) -> &str {
    message["attributes"]["objectId"].as_str().unwrap()
}

#[tokio::test]
async fn writes_past_the_limit_succeed_oldest_first_and_the_newest_are_delivered() {
    let dir = TestDir::new();
    let catalog = setup(&dir, VersionHistory::Discard).await;
    fill_outbox(&catalog, MAX_MESSAGES).await;
    for name in ["prefix/first", "prefix/second"] {
        put(&catalog, name, Default::default()).await.unwrap();
    }
    assert_eq!(pending(&catalog).await, (MAX_MESSAGES, Some(3)));
    // The accounting that a restart validates stays exact through evictions.
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let delivered = drain(&catalog).await;
    assert_eq!(delivered.len(), usize::try_from(MAX_MESSAGES).unwrap());
    assert_eq!(object_id(&delivered[0]), "old-3");
    let newest: Vec<_> = delivered[delivered.len() - 2..]
        .iter()
        .map(object_id)
        .collect();
    assert_eq!(newest, ["prefix/first", "prefix/second"]);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_rejected_write_keeps_the_messages_it_would_have_evicted() {
    let dir = TestDir::new();
    let catalog = setup(&dir, VersionHistory::Discard).await;
    fill_outbox(&catalog, MAX_MESSAGES).await;
    catalog
        .run::<_, CatalogError>(|connection| {
            connection.execute_batch("CREATE TEMP TRIGGER reject_notification BEFORE INSERT ON notification_outbox BEGIN SELECT RAISE(ABORT, 'notification rejected'); END;")?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        put(&catalog, "prefix/item", Default::default())
            .await
            .is_err()
    );
    assert_eq!(pending(&catalog).await, (MAX_MESSAGES, Some(1)));
    assert_eq!(
        catalog
            .get_object(
                live("test-bucket", "prefix/item"),
                Default::default(),
                PERMISSIVE
            )
            .await,
        Err(ObjectError::ObjectNotFound)
    );
    let (generation, acl_rows): (i64, i64) = catalog.run(|connection| {
        sql::row(connection, "SELECT last_generation, (SELECT count(*) FROM acl_object_entries) FROM object_generation", [], |row| Ok((row.get(0)?, row.get(1)?))).map_err(unavailable)
    }).await.unwrap();
    // Buckets use their own allocator, so the failed upload leaves the object allocator unused.
    assert_eq!((generation, acl_rows), (0, 0));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn evicting_the_message_in_flight_does_not_disturb_the_publisher() {
    use tokio::io::AsyncWriteExt;
    let dir = TestDir::new();
    let catalog = setup(&dir, VersionHistory::Discard).await;
    fill_outbox(&catalog, MAX_MESSAGES).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    catalog
        .start_notification_publisher(
            crate::notification::Endpoint::parse(&listener.local_addr().unwrap().to_string())
                .unwrap(),
        )
        .unwrap();
    let timeout = std::time::Duration::from_secs(3);
    let (mut socket, message) = tokio::time::timeout(timeout, accept_publish(&listener))
        .await
        .unwrap();
    let sent: Value = serde_json::from_str(&message).unwrap();
    assert_eq!(object_id(&sent["messages"][0]), "old-1");
    // The write evicts the very row the publisher holds an open attempt for.
    put(&catalog, "prefix/item", Default::default())
        .await
        .unwrap();
    assert_eq!(pending(&catalog).await, (MAX_MESSAGES, Some(2)));
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 23\r\n\r\n{\"messageIds\":[\"done\"]}")
        .await
        .unwrap();
    drop(socket);
    let (socket, message) = tokio::time::timeout(timeout, accept_publish(&listener))
        .await
        .unwrap();
    let sent: Value = serde_json::from_str(&message).unwrap();
    assert_eq!(object_id(&sent["messages"][0]), "old-2");
    drop((socket, listener));
    catalog.close().await.unwrap();
}

#[test]
fn eviction_warnings_are_spaced_out_and_report_everything_since_the_last() {
    const INTERVAL: i64 = 60_000_000;
    let mut log = EvictionLog {
        warned_at: None,
        unreported: 0,
    };
    assert_eq!(log.record(1, 0), Some(1));
    assert_eq!(log.record(2, 1), None);
    assert_eq!(log.record(3, INTERVAL - 1), None);
    assert_eq!(log.record(4, INTERVAL), Some(9));
    assert_eq!(log.record(1, INTERVAL + 1), None);
    // A clock set back does not silence warnings until it catches up.
    assert_eq!(log.record(1, 0), Some(2));
}

#[tokio::test]
async fn metadata_payload_filters_and_deleted_configuration_use_transaction_time_snapshots() {
    let dir = TestDir::new();
    let catalog = setup(&dir, VersionHistory::Discard).await;
    let first = put(&catalog, "prefix/item", Default::default())
        .await
        .unwrap();
    drain(&catalog).await;
    let config = NotificationConfig {
        topic: "//pubsub.googleapis.com/projects/p/topics/metadata"
            .parse()
            .unwrap(),
        payload_format: crate::notification::PayloadFormat::None,
        event_types: [crate::notification::Event::MetadataUpdate]
            .into_iter()
            .collect(),
        custom_attributes: BTreeMap::from([
            ("eventType".into(), "forged".into()),
            ("payloadFormat".into(), "forged".into()),
        ]),
        object_name_prefix: "prefix/".into(),
    };
    catalog
        .insert_notification("test-bucket", config, PERMISSIVE)
        .await
        .unwrap();
    let update = crate::object::ObjectMetadataUpdate {
        content_type: Some(Some("application/test".into())),
        ..Default::default()
    };
    let changed = catalog
        .update_object_metadata(
            live("test-bucket", "prefix/item"),
            Default::default(),
            update.clone(),
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(changed.generation, first.generation);
    assert_eq!(changed.metageneration, 2);
    assert!(
        catalog
            .update_object_metadata(
                live("test-bucket", "prefix/item"),
                ObjectConditions {
                    if_metageneration_match: Some(1),
                    ..Default::default()
                },
                update,
                |_| Ok(()),
                PERMISSIVE
            )
            .await
            .is_err()
    );
    assert!(
        catalog
            .delete_notification("test-bucket", "1", PERMISSIVE)
            .await
            .unwrap()
    );
    assert!(
        catalog
            .delete_notification("test-bucket", "2", PERMISSIVE)
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
    expected.as_object_mut().unwrap().shift_remove("selfLink");
    expected.as_object_mut().unwrap().shift_remove("mediaLink");
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
    let target = live("test-bucket", "prefix/item");
    for step in [
        Step::BodyWrite,
        Step::BodyFlush,
        Step::BodySync,
        Step::BeforeCommit,
    ] {
        let dir = TestDir::new();
        let catalog = setup(&dir, VersionHistory::Discard).await;
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
                .get_object(target.clone(), Default::default(), PERMISSIVE)
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        reopened.close().await.unwrap();
    }
    for step in [Step::BeforeCommit, Step::AfterCommit] {
        let dir = TestDir::new();
        let snapshot = TestDir::new();
        let catalog = setup(&dir, VersionHistory::Discard).await;
        fault::arm(&dir.0, step, 0, Fault::Snapshot(snapshot.0.clone()));
        put(&catalog, "prefix/item", Default::default())
            .await
            .unwrap();
        assert_eq!(fault::pending(&dir.0), 0);
        catalog.close().await.unwrap();
        let recovered = Catalog::open(&snapshot.0).await.unwrap();
        assert_eq!(
            recovered
                .get_object(target.clone(), Default::default(), PERMISSIVE)
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
        let catalog = setup(&dir, VersionHistory::Discard).await;
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
                .get_object(
                    live("test-bucket", "prefix/item"),
                    Default::default(),
                    PERMISSIVE
                )
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
    eventually_async("the expected pending notification", || async {
        check(&catalog.next_notification().await.unwrap())
    })
    .await;
}

#[tokio::test]
async fn missing_acknowledgement_retries_the_same_snapshot_after_restart() {
    use tokio::io::AsyncWriteExt;
    let dir = TestDir::new();
    // The retry after a failed attempt is due 100 ms later. A frozen clock keeps it pending, so
    // a slow poll cannot miss `attempts == 1` while further failed retries keep raising it.
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
        UNIX_EPOCH + std::time::Duration::from_micros(42_000_000),
    ));
    let catalog = setup_with_clock(&dir, VersionHistory::Discard, clock.clone()).await;
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
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    assert_eq!(
        catalog.next_notification().await.unwrap().unwrap().attempts,
        1
    );
    clock.set(UNIX_EPOCH + std::time::Duration::from_micros(42_200_000));
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
    let dir = TestDir::new();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
        UNIX_EPOCH + std::time::Duration::from_micros(42_000_000),
    ));
    let catalog = setup_with_clock(&dir, VersionHistory::Discard, clock.clone()).await;
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
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        catalog.0.activity.drained(),
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), admin.reset())
        .await
        .unwrap()
        .unwrap();
    drop(attempt);
    assert!(catalog.next_notification().await.unwrap().is_none());
    // Release the attempt fence and advance beyond backoff before observing the
    // listener; a blocked publisher must not make the no-resend assertion pass.
    clock.set(UNIX_EPOCH + std::time::Duration::from_micros(42_200_000));
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
async fn the_byte_budget_evicts_as_many_messages_as_the_new_one_needs() {
    let dir = TestDir::new();
    let catalog = setup(&dir, VersionHistory::Discard).await;
    catalog
        .run(|connection| {
            let transaction = connection.transaction()?;
            let insert = |id: i64, message: &str| {
                sql::exec(
                    &transaction,
                    "INSERT INTO notification_outbox VALUES (?1, ?2, ?3, 0, 0)",
                    params![id, TOPIC, message],
                )
            };
            // The two oldest are too small to free enough room for a real notification.
            insert(1, "{}")?;
            insert(2, "{}")?;
            let large = json!({"padding": "x".repeat(120_000)}).to_string();
            let tiny = 2 * (TOPIC.len() + 2);
            let count = (MAX_BYTES - tiny) / (large.len() + TOPIC.len());
            let mut id = 2;
            for _ in 0..count {
                id += 1;
                insert(id, &large)?;
            }
            // Exactly exhaust the budget, without exceeding a row bound.
            let remainder = MAX_BYTES - tiny - count * (large.len() + TOPIC.len());
            id += 1;
            insert(
                id,
                &json!({"padding": "x".repeat(remainder - TOPIC.len() - 14)}).to_string(),
            )?;
            sql::exec(
                &transaction,
                "UPDATE notification_outbox_state SET last_id = ?1",
                [id],
            )?;
            transaction.commit().map_err(unavailable)
        })
        .await
        .unwrap();
    let before = pending(&catalog).await.0;
    put(&catalog, "prefix/item", Default::default())
        .await
        .unwrap();
    // Both small messages and one large one gave way, and the new message took one slot.
    assert_eq!(pending(&catalog).await, (before - 3 + 1, Some(4)));
    catalog.close().await.unwrap();
    // The byte accounting a restart validates stays exact through evictions.
    Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
}

#[tokio::test]
async fn retry_deadline_allows_other_messages_and_close_waits_for_the_active_attempt() {
    use tokio::io::AsyncWriteExt;
    let dir = TestDir::new();
    let catalog = setup(&dir, VersionHistory::Discard).await;
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
    let catalog = setup(&dir, VersionHistory::Discard).await;
    catalog.close().await.unwrap();
    let path = dir.0.join("catalog.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    super::super::test_support::downgrade_bucket_timestamp_schema(&connection);
    connection.execute_batch("DROP TABLE notification_outbox; DROP TABLE notification_outbox_state; PRAGMA user_version = 37;").unwrap();
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog
            .list_notifications("test-bucket", PERMISSIVE)
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
