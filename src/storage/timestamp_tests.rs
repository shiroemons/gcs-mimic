use super::*;
use crate::{
    json::{self, Projection},
    object::{
        ComposeSource, ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate,
        UploadChunk,
    },
};
use bytes::Bytes;

fn migration_fixture() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE buckets(time_created INTEGER, updated INTEGER);
         CREATE TABLE objects(time_created INTEGER, updated INTEGER, time_deleted INTEGER);
         PRAGMA user_version = 13;",
        )
        .unwrap();
    for millis in [
        -9_223_372_036_854_775i64,
        -1001,
        -1,
        0,
        1234,
        9_223_372_036_854_775,
    ] {
        connection
            .execute("INSERT INTO buckets VALUES (?1, ?1)", [millis])
            .unwrap();
        connection
            .execute("INSERT INTO objects VALUES (?1, ?1, ?1)", [millis])
            .unwrap();
    }
    connection
        .execute("INSERT INTO objects VALUES (0, 0, NULL)", [])
        .unwrap();
    connection
}

#[test]
fn timestamp_migration_scales_extrema_and_rolls_back_after_partial_update() {
    let mut connection = migration_fixture();
    connection.execute_batch("CREATE TEMP TRIGGER fail_timestamp_update BEFORE UPDATE ON objects BEGIN SELECT RAISE(ABORT, 'injected timestamp migration failure'); END;").unwrap();
    let before = timestamp_rows(&connection);
    let error = migrate_timestamp_precision(&mut connection).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected timestamp migration failure")
    );
    assert_eq!(timestamp_rows(&connection), before);
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        13
    );
    connection
        .execute_batch("DROP TRIGGER fail_timestamp_update;")
        .unwrap();
    migrate_timestamp_precision(&mut connection).unwrap();
    let after = timestamp_rows(&connection);
    assert_eq!(
        after,
        before
            .into_iter()
            .map(|row| row
                .into_iter()
                .map(|n| n.map(|n| n * 1000))
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        14
    );
    assert!(
        validate_schema_version(14, 13)
            .unwrap_err()
            .to_string()
            .contains("supported version is 13")
    );
    assert!(validate_schema_version(15, 14).is_err());
}

fn timestamp_rows(connection: &Connection) -> Vec<Vec<Option<i64>>> {
    [
        "SELECT time_created, updated, NULL FROM buckets ORDER BY rowid",
        "SELECT time_created, updated, time_deleted FROM objects ORDER BY rowid",
    ]
    .into_iter()
    .flat_map(|sql| {
        connection
            .prepare(sql)
            .unwrap()
            .query_map([], |row| Ok(vec![row.get(0)?, row.get(1)?, row.get(2)?]))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    })
    .collect()
}

#[test]
fn timestamp_migration_rejects_overflow_without_changing_version_or_values() {
    for invalid in [
        i64::MIN,
        -9_223_372_036_854_776,
        9_223_372_036_854_776,
        i64::MAX,
    ] {
        let mut connection = migration_fixture();
        connection
            .execute("UPDATE objects SET updated = ?1 WHERE rowid = 1", [invalid])
            .unwrap();
        let before = timestamp_rows(&connection);
        assert!(
            migrate_timestamp_precision(&mut connection)
                .unwrap_err()
                .to_string()
                .contains("objects timestamps")
        );
        assert_eq!(timestamp_rows(&connection), before);
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            13
        );
    }
}

struct FixedClock(Mutex<SystemTime>);
impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().unwrap()
    }
}

#[test]
fn timestamp_fixture_downgrade_floors_signed_values_and_deletion_times() {
    let connection = migration_fixture();
    let before = timestamp_rows(&connection);
    connection
        .execute_batch(OBJECT_HOLDS_CUSTOM_TIME_SCHEMA)
        .unwrap();
    connection
        .execute_batch(BUCKET_UNIFORM_ACCESS_SCHEMA)
        .unwrap();
    super::test_support::downgrade_timestamp_precision(&connection);
    assert_eq!(
        timestamp_rows(&connection),
        before
            .into_iter()
            .map(|row| row
                .into_iter()
                .map(|value| value.map(|value| value.div_euclid(1000)))
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );
    for nanos in [-1_001_001i64, -1, 0, 1_001_001] {
        let time = if nanos < 0 {
            UNIX_EPOCH - Duration::from_nanos(nanos.unsigned_abs())
        } else {
            UNIX_EPOCH + Duration::from_nanos(nanos as u64)
        };
        assert_eq!(
            super::test_support::floor_to_millis(time),
            system_time_millis(unix_millis(time).unwrap()).unwrap()
        );
    }
}

fn body() -> tokio::sync::mpsc::Receiver<Result<UploadChunk, std::io::Error>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(b"x"))))
        .unwrap();
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    receiver
}

#[tokio::test]
async fn timestamp_writes_floor_microseconds_across_publication_routes() {
    for (index, nanos) in [0i64, 123_456_000, 123_456_789].into_iter().enumerate() {
        let now = if nanos < 0 {
            UNIX_EPOCH - Duration::from_nanos(nanos.unsigned_abs())
        } else {
            UNIX_EPOCH + Duration::from_nanos(nanos as u64)
        };
        let micros = nanos.div_euclid(1000);
        let expected = system_time_micros(micros).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "gcs-mimic-timestamp-writes-{}-{index}",
            std::process::id()
        ));
        let catalog = Catalog::open_with_clock(&dir, Arc::new(FixedClock(Mutex::new(now))))
            .await
            .unwrap();
        let mut bucket =
            json::parse_insert(br#"{"name":"timestamp-bucket"}"#, "project", now).unwrap();
        bucket.versioning_enabled = true;
        let bucket = catalog.insert_bucket(bucket).await.unwrap();
        assert_eq!(bucket.time_created, expected);
        let patched = catalog
            .patch_bucket(
                "timestamp-bucket",
                BucketMetadataUpdate::default(),
                None,
                None,
                &BucketAccess::Permissive,
            )
            .await
            .unwrap();
        assert_eq!(patched.updated, expected);
        assert_eq!(
            json::timestamp(expected).unwrap(),
            json::timestamp(now).unwrap()
        );
        let object = catalog
            .put_object(
                "timestamp-bucket",
                "media",
                "text/plain".into(),
                ExpectedHashes::default(),
                ObjectConditions::default(),
                1,
                body(),
            )
            .await
            .unwrap();
        assert_eq!(object.time_created, expected);
        let updated = catalog
            .update_object_metadata(
                "timestamp-bucket",
                "media",
                None,
                ObjectConditions::default(),
                ObjectMetadataUpdate::default(),
                |_| Ok(()),
            )
            .await
            .unwrap();
        assert_eq!(updated.updated, expected);
        let source = catalog
            .resolve_derived_source(
                "timestamp-bucket",
                "media",
                None,
                ObjectConditions::default(),
                &BucketAccess::Permissive,
            )
            .await
            .unwrap();
        let transfer = catalog
            .begin_derived(vec![source.whole()], 1)
            .await
            .unwrap();
        assert_eq!(
            transfer.created,
            system_time_millis(nanos.div_euclid(1_000_000)).unwrap()
        );
        catalog.advance_derived(&transfer.id, 1).await.unwrap();
        let destination = |name: &str| DerivedDestination {
            bucket: "timestamp-bucket".into(),
            name: name.into(),
            content_type: "text/plain".into(),
            ..crate::storage::DerivedDestination::new(crate::bucket::BucketAccess::Permissive)
        };
        let derived = catalog
            .publish_derived(&transfer.id, destination("derived"), |_, _| Ok(()))
            .await
            .unwrap();
        assert_eq!(derived.time_created, expected);
        let composed = catalog
            .compose_object(
                destination("composed"),
                vec![ComposeSource {
                    name: "media".into(),
                    generation: None,
                    if_generation_match: None,
                }],
                false,
                1,
            )
            .await
            .unwrap();
        assert_eq!(composed.updated, expected);
        let options = ResumableOptions {
            content_type: "text/plain".into(),
            ..crate::storage::ResumableOptions::new(crate::bucket::BucketAccess::Permissive)
        };
        let session = catalog
            .create_resumable_session("timestamp-bucket", "resumable", options.clone(), Some(1))
            .await
            .unwrap();
        assert_eq!(session.created, transfer.created);
        assert_eq!(
            session.deadline.duration_since(session.created).unwrap(),
            Duration::from_secs(604800)
        );
        let lease = catalog
            .lock_resumable_session(&session.id, "timestamp-bucket")
            .await
            .unwrap();
        catalog
            .write_resumable_session(
                lease,
                ResumableChunk {
                    start: 0,
                    length: 1,
                    total: Some(1),
                    finalize: true,
                    options,
                    catalog_wait: tokio::sync::watch::channel(false).0,
                },
                body(),
                catalog.admit_object_upload().unwrap(),
                |_| Ok(serde_json::json!({})),
            )
            .await
            .unwrap();
        catalog
            .delete_object(
                "timestamp-bucket",
                "media",
                None,
                ObjectConditions::default(),
            )
            .await
            .unwrap();
        let retired = catalog
            .get_object(
                "timestamp-bucket",
                "media",
                Some(object.generation),
                ObjectConditions::default(),
            )
            .await
            .unwrap();
        assert_eq!(retired.time_deleted, Some(expected));
        catalog
            .run(move |connection| {
                assert_eq!(
                    connection
                        .query_row(
                            "SELECT count(*) FROM objects WHERE time_created = ?1 AND updated = ?1",
                            [micros],
                            |row| row.get::<_, i64>(0)
                        )
                        .unwrap(),
                    4
                );
                assert_eq!(
                    connection
                        .query_row("SELECT time_created FROM buckets", [], |row| row
                            .get::<_, i64>(0))
                        .unwrap(),
                    micros
                );
                Ok(())
            })
            .await
            .unwrap();
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&dir).await.unwrap();
        assert_eq!(
            catalog
                .get_object(
                    "timestamp-bucket",
                    "resumable",
                    None,
                    ObjectConditions::default()
                )
                .await
                .unwrap()
                .time_created,
            expected
        );
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn timestamp_pre_epoch_publication_remains_rejected_and_metadata_floors() {
    let dir = std::env::temp_dir().join(format!(
        "gcs-mimic-timestamp-pre-epoch-{}",
        std::process::id()
    ));
    let clock = Arc::new(FixedClock(Mutex::new(UNIX_EPOCH)));
    let catalog = Catalog::open_with_clock(&dir, clock.clone()).await.unwrap();
    let mut bucket = json::parse_insert(
        br#"{"name":"timestamp-bucket"}"#,
        "project",
        UNIX_EPOCH - Duration::from_nanos(123_456_789),
    )
    .unwrap();
    bucket.versioning_enabled = true;
    let bucket = catalog.insert_bucket(bucket).await.unwrap();
    assert_eq!(bucket.time_created, system_time_micros(-123_457).unwrap());
    let object = catalog
        .put_object(
            "timestamp-bucket",
            "source",
            "text/plain".into(),
            ExpectedHashes::default(),
            ObjectConditions::default(),
            1,
            body(),
        )
        .await
        .unwrap();
    for nanos in [100, 123_456_789] {
        let source = catalog
            .resolve_derived_source(
                "timestamp-bucket",
                "source",
                None,
                ObjectConditions::default(),
                &BucketAccess::Permissive,
            )
            .await
            .unwrap();
        let transfer = catalog
            .begin_derived(vec![source.whole()], 1)
            .await
            .unwrap();
        catalog.advance_derived(&transfer.id, 1).await.unwrap();
        let options = ResumableOptions {
            content_type: "text/plain".into(),
            ..crate::storage::ResumableOptions::new(crate::bucket::BucketAccess::Permissive)
        };
        let session = catalog
            .create_resumable_session("timestamp-bucket", "resumable", options.clone(), Some(1))
            .await
            .unwrap();
        let lease = catalog
            .lock_resumable_session(&session.id, "timestamp-bucket")
            .await
            .unwrap();
        *clock.0.lock().unwrap() = UNIX_EPOCH - Duration::from_nanos(nanos);
        assert!(matches!(
            catalog
                .put_object(
                    "timestamp-bucket",
                    "media",
                    "text/plain".into(),
                    ExpectedHashes::default(),
                    ObjectConditions::default(),
                    1,
                    body()
                )
                .await,
            Err(ObjectError::Unavailable(_))
        ));
        assert!(matches!(
            catalog
                .publish_derived(
                    &transfer.id,
                    DerivedDestination {
                        bucket: "timestamp-bucket".into(),
                        name: "derived".into(),
                        content_type: "text/plain".into(),
                        ..crate::storage::DerivedDestination::new(
                            crate::bucket::BucketAccess::Permissive
                        )
                    },
                    |_, _| Ok(())
                )
                .await,
            Err(ObjectError::Unavailable(_))
        ));
        assert!(matches!(
            catalog
                .write_resumable_session(
                    lease,
                    ResumableChunk {
                        start: 0,
                        length: 1,
                        total: Some(1),
                        finalize: true,
                        options,
                        catalog_wait: tokio::sync::watch::channel(false).0
                    },
                    body(),
                    catalog.admit_object_upload().unwrap(),
                    |_| Ok(serde_json::json!({}))
                )
                .await,
            Err(ResumableError::Object(ObjectError::Unavailable(_)))
        ));
        let expected = system_time_micros((-(nanos as i64)).div_euclid(1000)).unwrap();
        let patched = catalog
            .patch_bucket(
                "timestamp-bucket",
                BucketMetadataUpdate::default(),
                None,
                None,
                &BucketAccess::Permissive,
            )
            .await
            .unwrap();
        assert_eq!(patched.updated, expected);
        let updated = catalog
            .update_object_metadata(
                "timestamp-bucket",
                "source",
                None,
                ObjectConditions::default(),
                ObjectMetadataUpdate::default(),
                |_| Ok(()),
            )
            .await
            .unwrap();
        assert_eq!(updated.updated, expected);
        assert_eq!(
            json::timestamp(updated.updated).unwrap(),
            json::timestamp(clock.now()).unwrap()
        );
        *clock.0.lock().unwrap() = UNIX_EPOCH;
        catalog.cancel_derived(&transfer.id).await.unwrap();
        catalog
            .cancel_resumable_session(&session.id, "timestamp-bucket")
            .await
            .unwrap();
    }
    assert_eq!(
        catalog
            .get_object(
                "timestamp-bucket",
                "source",
                None,
                ObjectConditions::default()
            )
            .await
            .unwrap()
            .generation,
        object.generation
    );
    catalog.close().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn timestamp_migration_preserves_resources_etags_permissions_and_sessions() {
    let dir = std::env::temp_dir().join(format!(
        "gcs-mimic-timestamp-upgrade-{}",
        std::process::id()
    ));
    let clock = Arc::new(FixedClock(Mutex::new(
        UNIX_EPOCH + Duration::from_millis(1234),
    )));
    let catalog = Catalog::open_with_clock(&dir, clock.clone()).await.unwrap();
    for millis in [
        -62_167_219_200_000i64,
        -1001,
        -1,
        0,
        1234,
        253_402_300_799_999,
    ] {
        // Windows SystemTime starts in 1601, so year 0 cannot be built there.
        let now = system_time_millis(millis);
        assert!(now.is_ok() || cfg!(windows), "{millis}");
        let Ok(now) = now else {
            continue;
        };
        let bucket = json::parse_insert(br#"{"name":"timestamp-bucket"}"#, "project", now).unwrap();
        let mut bucket = bucket;
        bucket.name = format!("timestamp-{}", millis.unsigned_abs());
        if millis < 0 {
            bucket.name.push_str("-negative");
        }
        catalog.insert_bucket(bucket).await.unwrap();
    }
    catalog
        .put_object(
            "timestamp-0",
            "object",
            "text/plain".into(),
            ExpectedHashes::default(),
            ObjectConditions::default(),
            1,
            body(),
        )
        .await
        .unwrap();
    let session = catalog
        .create_resumable_session(
            "timestamp-0",
            "pending",
            ResumableOptions::new(crate::bucket::BucketAccess::Permissive),
            Some(1),
        )
        .await
        .unwrap();
    let policy = catalog
        .get_policy("timestamp-0", &BucketAccess::Permissive)
        .await
        .unwrap();
    let before = resources(&catalog).await;
    catalog.close().await.unwrap();
    let mut connection = Connection::open(dir.join("catalog.sqlite3")).unwrap();
    super::test_support::downgrade_timestamp_precision(&connection);
    connection.pragma_update(None, "user_version", 13).unwrap();
    connection.execute_batch("CREATE TEMP TRIGGER fail_timestamp_update BEFORE UPDATE ON objects BEGIN SELECT RAISE(ABORT, 'injected timestamp migration failure'); END;").unwrap();
    assert!(
        migrate_timestamp_precision(&mut connection)
            .unwrap_err()
            .to_string()
            .contains("injected timestamp migration failure")
    );
    let observer = Connection::open(dir.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        observer
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        13
    );
    assert_eq!(
        observer
            .query_row(
                "SELECT time_created FROM buckets WHERE name='timestamp-0'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        observer
            .query_row(
                "SELECT time_created FROM buckets WHERE name='timestamp-1001-negative'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        -1001
    );
    assert_eq!(
        observer
            .query_row("SELECT time_created, updated FROM objects", [], |row| Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?
            )))
            .unwrap(),
        (1234, 1234)
    );
    drop(observer);
    connection
        .execute_batch("DROP TRIGGER fail_timestamp_update;")
        .unwrap();
    drop(connection);
    let catalog = Catalog::open_with_clock(&dir, clock).await.unwrap();
    assert_eq!(resources(&catalog).await, before);
    assert_eq!(
        catalog
            .get_policy("timestamp-0", &BucketAccess::Permissive)
            .await
            .unwrap(),
        policy
    );
    assert_eq!(
        catalog
            .lock_resumable_session(&session.id, "timestamp-0")
            .await
            .unwrap()
            .session,
        session
    );
    catalog.close().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

async fn resources(catalog: &Catalog) -> Vec<Vec<u8>> {
    let mut result = Vec::new();
    let buckets = catalog
        .list_buckets("project", None, None, 100)
        .await
        .unwrap();
    for bucket in buckets {
        result.push(
            serde_json::to_vec(
                &json::bucket_resource(&bucket, "http://localhost", Projection::NoAcl).unwrap(),
            )
            .unwrap(),
        );
    }
    let object = catalog
        .get_object("timestamp-0", "object", None, ObjectConditions::default())
        .await
        .unwrap();
    result.push(
        serde_json::to_vec(
            &json::object_resource(&object, "http://localhost", Projection::NoAcl).unwrap(),
        )
        .unwrap(),
    );
    result
}
