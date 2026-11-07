use super::durability::SHARD_CREATION;
#[cfg(unix)]
use super::durability::cleanup_failed_shard;
use super::list::{ListSource, UpperBound, list_page_within, list_seek_sql};
use super::publication::expiration_after;
use super::*;
use crate::storage::test_fixtures::{PERMISSIVE, bucket, live, permissive_upload};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

#[tokio::test]
async fn bucket_settings_migrate_supported_catalogs() {
    use crate::storage::catalog::test_support as common;
    use crate::storage::catalog_tests::bucket;
    use crate::storage::test_fixtures::{assert_current_schema, downgraded};
    use crate::test_support::TestDir;
    for version in [4, 5, 6, 7] {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let expected = catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        let path = dir.0.join("catalog.sqlite3");
        let objects_sql = downgraded(&dir, &catalog, |connection| {
            let objects_sql: String = connection
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE name = 'objects'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            connection.execute_batch("ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold;").unwrap();
            common::downgrade_object_schema(connection);
            if version < 7 {
                connection
                    .execute_batch("DROP TABLE resumable_sessions; DROP TABLE bucket_incarnations;")
                    .unwrap();
            }
            if version < 6 {
                connection
                    .execute_batch("DROP TABLE bucket_policies; DROP TABLE policy_revision;")
                    .unwrap();
            }
            if version < 5 {
                connection
                    .execute_batch("DROP TABLE startup_seed;")
                    .unwrap();
            }
            connection
                .pragma_update(None, "user_version", version)
                .unwrap();
            objects_sql
        }).await;
        let catalog = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(
            catalog.get_bucket("bucket", PERMISSIVE).await.unwrap(),
            Some(expected),
            "version {version}"
        );
        for (index, (versioning, hold)) in
            [(false, false), (false, true), (true, false), (true, true)]
                .into_iter()
                .enumerate()
        {
            let mut record = bucket(&format!("settings-{index}"), "project");
            record.versioning_enabled = versioning;
            record.default_event_based_hold = hold;
            catalog.insert_bucket(record).await.unwrap();
        }
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        for (index, settings) in [(false, false), (false, true), (true, false), (true, true)]
            .into_iter()
            .enumerate()
        {
            let record = catalog
                .get_bucket(&format!("settings-{index}"), PERMISSIVE)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (record.versioning_enabled, record.default_event_based_hold),
                settings
            );
        }
        catalog.close().await.unwrap();
        let mut connection = Connection::open(path).unwrap();
        assert_current_schema(&connection);
        assert_eq!(
            connection
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE name = 'objects'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            objects_sql
        );
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "UPDATE buckets SET versioning_enabled = 1 WHERE name = 'bucket'",
                [],
            )
            .unwrap();
        let settings = BucketPublication::load(&transaction, "bucket").unwrap();
        assert_eq!(
            (settings.versioning, settings.default_event_hold),
            (Versioning::Enabled, false)
        );
        assert!(matches!(
            BucketPublication::load(&transaction, "missing"),
            Err(ObjectError::BucketNotFound)
        ));
        assert!(
            transaction
                .execute("UPDATE buckets SET default_event_based_hold = 2", [])
                .is_err()
        );
        assert!(
            transaction
                .execute("UPDATE buckets SET versioning_enabled = NULL", [])
                .is_err()
        );
        transaction.rollback().unwrap();
    }
}

#[cfg(unix)]
#[test]
fn shard_cleanup_keeps_the_original_sync_failure() {
    for occupied in [false, true] {
        let data = crate::test_support::TestDir::new();
        let shard = data.0.join("shard");
        fs::create_dir(&shard).unwrap();
        if occupied {
            fs::write(shard.join("concurrent"), b"bytes").unwrap();
        }
        let error = cleanup_failed_shard(
            &shard,
            io::Error::new(io::ErrorKind::StorageFull, "shard parent sync failed"),
        );
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert_eq!(error.to_string(), "shard parent sync failed");
        assert_eq!(shard.exists(), occupied);
    }
}

#[cfg(unix)]
#[test]
fn publish_refuses_to_replace_an_existing_blob() {
    let data = crate::test_support::TestDir::new();
    fs::create_dir_all(data.0.join("staging")).unwrap();
    let id = BlobId::generate().unwrap();
    let stage = id.staging_path(&data.0);
    let destination = id.blob_path(&data.0);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&stage, b"replacement").unwrap();
    fs::write(&destination, b"live bytes").unwrap();
    assert_eq!(
        publish_blob(&stage, &destination).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&stage).unwrap(), b"replacement");
    assert_eq!(fs::read(&destination).unwrap(), b"live bytes");
}

#[tokio::test]
async fn glob_listing_pages_through_non_matching_rows() {
    for shape in ["items", "prefixes", "versions"] {
        let data = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        create_bucket(&catalog, "project").await;
        catalog
            .run(|connection| {
                sql::exec(
                    connection,
                    "UPDATE buckets SET versioning_enabled = 1 WHERE name = 'bucket'",
                    [],
                )?;
                Ok::<_, ObjectError>(())
            })
            .await
            .unwrap();
        let mut created = Vec::new();
        for index in 0..10 {
            let name = if index == 9 {
                "z".into()
            } else if shape == "versions" {
                "a".into()
            } else {
                format!("a{index:02}")
            };
            let name = if shape == "prefixes" {
                format!("{name}/file")
            } else {
                name
            };
            created.push(put(&catalog, &name, b"body").await);
        }
        let mut query = ObjectListQuery {
            bucket: "bucket".into(),
            delimiter: if shape == "prefixes" {
                "/".into()
            } else {
                String::new()
            },
            versions: shape == "versions",
            match_glob: Some(crate::match_glob::Glob::parse("z**").unwrap()),
            ..Default::default()
        };
        let control_query = query.clone();
        let control = catalog
            .run(move |connection| {
                list_page_within(connection, &control_query, 1000, 0, usize::MAX)
            })
            .await
            .unwrap();
        assert_eq!(control.items.len() + control.prefixes.len(), 1, "{shape}");
        let mut items = Vec::new();
        let mut prefixes = Vec::new();
        let mut pages = 0;
        loop {
            let page_query = query.clone();
            let page = catalog
                .run(move |connection| list_page_within(connection, &page_query, 1000, 0, 3))
                .await
                .unwrap();
            if pages == 0 {
                assert!(page.items.is_empty() && page.prefixes.is_empty(), "{shape}");
                let cursor = page.next.as_ref().unwrap();
                assert_eq!(
                    cursor.name,
                    if shape == "prefixes" {
                        "a02/"
                    } else {
                        &created[2].name
                    }
                );
                assert_eq!(
                    cursor.generation,
                    (shape == "versions").then_some(created[2].generation)
                );
            }
            items.extend(page.items);
            prefixes.extend(page.prefixes);
            pages += 1;
            assert!(pages <= 4, "continuation must advance: {shape}");
            query.cursor = page.next;
            if query.cursor.is_none() {
                break;
            }
        }
        assert_eq!(pages, 4, "{shape}");
        assert_eq!(items, control.items, "{shape}");
        assert_eq!(prefixes, control.prefixes, "{shape}");
        query.match_glob = None;
        query.cursor = None;
        let page = catalog
            .run(move |connection| list_page_within(connection, &query, 1, 0, 3))
            .await
            .unwrap();
        assert_eq!(page.items.len() + page.prefixes.len(), 1, "{shape}");
        assert!(page.next.is_some(), "{shape}");
        catalog.close().await.unwrap();
    }
}

#[test]
fn acl_update_errors_keep_client_input_and_authorization_status() {
    for (error, expected) in [
        (
            CatalogError::InvalidMetadata("invalid".into()),
            ObjectError::InvalidFixedMetadata("invalid".into()),
        ),
        (
            CatalogError::UnsupportedMetadata("unsupported".into()),
            ObjectError::UnsupportedMetadata("unsupported".into()),
        ),
        (
            CatalogError::Metadata(crate::metadata_error::MetadataError::AutoclassDisabled),
            ObjectError::Metadata(crate::metadata_error::MetadataError::AutoclassDisabled),
        ),
        (CatalogError::Forbidden, ObjectError::Forbidden),
        (
            CatalogError::PublicAccessPrevention,
            ObjectError::PublicAccessPrevention,
        ),
    ] {
        assert_eq!(acl_update_error(error), expected);
    }
    assert!(matches!(
        acl_update_error(CatalogError::NotFound),
        ObjectError::Unavailable(_)
    ));
}

#[tokio::test]
async fn retention_expiration_matches_between_publication_and_reads() {
    for period in [0, 1, 3_155_760_000] {
        let data = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        create_bucket(&catalog, "project").await;
        catalog.run(move |connection| {
            sql::exec(connection,
                "UPDATE buckets SET retention_policy = json_object('retention_period', ?1, 'is_locked', json('false')) WHERE name = 'bucket'", [period])?;
            Ok::<_, ObjectError>(())
        }).await.unwrap();
        let published = put(&catalog, "object", b"body").await;
        let expected = Some(expiration_after(published.time_created, period).unwrap());
        assert_eq!(published.retention_expiration_time, expected);
        let loaded = catalog
            .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        assert_eq!(loaded.retention_expiration_time, expected);
        let page = catalog
            .list_objects(
                ObjectListQuery {
                    bucket: "bucket".into(),
                    ..Default::default()
                },
                1000,
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].retention_expiration_time, expected);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn publication_settings_reject_missing_buckets_and_corrupt_autoclass() {
    let data = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    create_bucket(&catalog, "project").await;
    catalog
        .run(|connection| {
            assert!(matches!(
                BucketPublication::load(connection, "missing"),
                Err(ObjectError::BucketNotFound)
            ));
            sql::exec(
                connection,
                "UPDATE buckets SET autoclass = 'broken' WHERE name = 'bucket'",
                [],
            )?;
            let error = BucketPublication::load(connection, "bucket").err().unwrap();
            assert!(matches!(error, ObjectError::Corrupt(_)));
            assert!(crate::error::causes(&error).any(|cause| cause.is::<serde_json::Error>()));
            Ok::<_, ObjectError>(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn staging_recovery_keeps_entry_context_and_the_io_failure_kind() {
    let data = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    catalog.close().await.unwrap();
    let stage = data.0.join("staging").join("abandoned");
    fs::write(&stage, b"body").unwrap();
    fault::arm(
        &data.0,
        Step::RecoveryStagingUnlink,
        0,
        fault::Fault::Io(io::ErrorKind::PermissionDenied),
    );
    let error = Catalog::open(&data.0).await.err().unwrap();
    assert!(
        matches!(&error, CatalogError::Io(cause) if cause.kind() == io::ErrorKind::PermissionDenied)
    );
    assert!(error.to_string().contains("staging entry"));
    assert!(error.to_string().contains("abandoned"));
    assert!(crate::error::causes(&error).any(|error| error.downcast_ref::<io::Error>().is_some()));
    assert!(stage.is_file());
    assert_eq!(fault::pending(&data.0), 0);
    let recovered = Catalog::open(&data.0).await.unwrap();
    assert!(!stage.exists());
    recovered.close().await.unwrap();
}

#[tokio::test]
async fn replayed_upload_keeps_its_record_when_discard_fails() {
    let data = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    create_bucket(&catalog, "project").await;
    let mut first = None;
    for replay in [false, true] {
        let mut permit = catalog.admit_object_upload().unwrap();
        catalog
            .check_upload_replay(
                "bucket",
                "object",
                Default::default(),
                PERMISSIVE,
                super::super::upload_replay::ReplayInvocation {
                    token: Some("retry"),
                    request: "media-upload",
                },
                &mut permit,
            )
            .await
            .unwrap();
        if replay {
            fault::arm(&data.0, Step::DiscardProbe, 0, fault::Fault::Busy);
        }
        let record = catalog
            .put_object_prechecked(
                permissive_upload("bucket", "object", 1000),
                body(b"body"),
                permit,
            )
            .await
            .unwrap();
        if let Some(first) = &first {
            assert_eq!(&record, first);
        }
        first = Some(record);
    }
    assert_eq!(fault::pending(&data.0), 0);
    assert_eq!(fault::catalog_thread_io(&data.0), 0);
    assert_eq!(catalog.0.uploads.lock().unwrap().active, 0);
    catalog.close().await.unwrap();
    let recovered = Catalog::open(&data.0).await.unwrap();
    assert_eq!(
        fs::read_dir(data.0.join("blobs"))
            .unwrap()
            .map(|shard| fs::read_dir(shard.unwrap().path()).unwrap().count())
            .sum::<usize>(),
        1
    );
    recovered.close().await.unwrap();
}

#[tokio::test]
async fn skip_mode_publishes_changed_bytes_without_a_discard_job() {
    let data = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    create_bucket(&catalog, "project").await;
    let original = put(&catalog, "object", b"old").await;
    fault::arm(&data.0, Step::DiscardProbe, 0, fault::Fault::Busy);
    let record = catalog
        .put_object_with_mode(
            permissive_upload("bucket", "object", 1000),
            body(b"new"),
            catalog.admit_object_upload().unwrap(),
            super::super::admin::PublishMode::SkipIfSameHash,
            PublishIdentity::Assigned,
            OwnerAssignment::Request,
        )
        .await
        .unwrap();
    assert_ne!(record.generation, original.generation);
    assert_eq!(fault::pending(&data.0), 1);
    assert!(fault::hit(&data.0, Step::DiscardProbe).is_err());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn invalid_body_keeps_its_error_when_discard_fails() {
    let data = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    create_bucket(&catalog, "project").await;
    for too_large in [false, true] {
        let mut upload = permissive_upload("bucket", "object", if too_large { 1 } else { 1000 });
        if !too_large {
            upload.expected.md5 = Some([0; 16]);
        }
        fault::arm(&data.0, Step::DiscardProbe, 0, fault::Fault::Busy);
        let error = catalog
            .put_object_prechecked(
                upload,
                body(b"body"),
                catalog.admit_object_upload().unwrap(),
            )
            .await
            .unwrap_err();
        assert!(if too_large {
            error == ObjectError::TooLarge
        } else {
            matches!(error, ObjectError::HashMismatch(_))
        });
        assert_eq!(fault::pending(&data.0), 0);
    }
    assert_eq!(catalog.0.uploads.lock().unwrap().active, 0);
    catalog.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn upload_cleanup_failure_preserves_success_and_precondition_error() {
    let data = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    create_bucket(&catalog, "project").await;
    let original = put(&catalog, "object", b"body").await;
    fault::arm(
        &data.0,
        Step::DiscardProbe,
        0,
        fault::Fault::Io(io::ErrorKind::Other),
    );
    let result = catalog
        .put_object_with_mode(
            permissive_upload("bucket", "object", 1000),
            body(b"body"),
            catalog.admit_object_upload().unwrap(),
            super::super::admin::PublishMode::SkipIfSameHash,
            PublishIdentity::Assigned,
            OwnerAssignment::Request,
        )
        .await
        .unwrap();
    assert_eq!(result, original);
    assert_eq!(fault::pending(&data.0), 0);
    let mut upload = permissive_upload("bucket", "object", 1000);
    upload.conditions.if_generation_match = Some(0);
    fault::arm(
        &data.0,
        Step::DiscardProbe,
        0,
        fault::Fault::Io(io::ErrorKind::Other),
    );
    assert_eq!(
        catalog
            .put_object_prechecked(upload, body(b"new"), catalog.admit_object_upload().unwrap())
            .await,
        Err(ObjectError::PreconditionFailed)
    );
    assert_eq!(fault::pending(&data.0), 0);
    for step in [Step::BodyWrite, Step::BodyFlush, Step::BodySync] {
        fault::arm(
            &data.0,
            step,
            0,
            fault::Fault::Io(io::ErrorKind::StorageFull),
        );
        fault::arm(
            &data.0,
            Step::DiscardProbe,
            0,
            fault::Fault::Io(io::ErrorKind::PermissionDenied),
        );
        let error = catalog
            .put_object_prechecked(
                permissive_upload("bucket", "object", 1000),
                body(b"new"),
                catalog.admit_object_upload().unwrap(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, ObjectError::Io(ref cause) if cause.kind() == io::ErrorKind::StorageFull),
            "{step:?}: {error}"
        );
        assert_eq!(fault::pending(&data.0), 0, "{step:?}");
    }
    assert_eq!(
        catalog
            .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap(),
        original
    );
    assert_eq!(fault::catalog_thread_io(&data.0), 0);
    catalog.close().await.unwrap();
    let recovered = Catalog::open(&data.0).await.unwrap();
    assert_eq!(fs::read_dir(data.0.join("staging")).unwrap().count(), 0);
    recovered.close().await.unwrap();
}

#[tokio::test]
async fn upload_publication_reads_the_clock_once() {
    struct CountingClock(AtomicUsize);
    impl Clock for CountingClock {
        fn now(&self) -> std::time::SystemTime {
            UNIX_EPOCH
                + std::time::Duration::from_secs(1 + self.0.fetch_add(1, Ordering::Relaxed) as u64)
        }
    }
    let data = crate::test_support::TestDir::new();
    let clock = Arc::new(CountingClock(AtomicUsize::new(0)));
    let catalog = Catalog::open_with_clock(&data.0, clock.clone())
        .await
        .unwrap();
    create_bucket(&catalog, "project").await;
    clock.0.store(0, Ordering::Relaxed);
    let record = catalog
        .put_object_prechecked(
            permissive_upload("bucket", "object", 1000),
            body(b"body"),
            catalog.admit_object_upload().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(clock.0.load(Ordering::Relaxed), 1);
    assert_eq!(
        record.time_created,
        UNIX_EPOCH + std::time::Duration::from_secs(1)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn poisoned_upload_counter_remains_usable() {
    let data = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let other = catalog.clone();
    let _ = std::thread::spawn(move || {
        let _guard = other.0.uploads.lock().unwrap();
        panic!("injected upload counter poison");
    })
    .join();
    drop(catalog.admit_object_upload().unwrap());
    assert!(catalog.is_healthy());
    catalog.close().await.unwrap();
}

/// # Safety
/// `counter` must point to a live, aligned AtomicUsize whenever SQLite can invoke this callback.
#[expect(
    unsafe_code,
    reason = "Test-only SQLite callbacks avoid enabling the hooks feature in the production dependency; callers keep the counter live."
)]
unsafe extern "C" fn count_progress(counter: *mut std::ffi::c_void) -> std::ffi::c_int {
    // SAFETY: The caller guarantees a live, aligned AtomicUsize whenever SQLite can call us.
    let counter = unsafe { &*counter.cast::<AtomicUsize>() };
    i32::from(counter.fetch_add(1, Ordering::Relaxed) >= 100)
}

#[test]
fn publishing_recovers_after_a_shard_creator_panicked() {
    let data = crate::test_support::TestDir::new();
    let _ = std::thread::spawn(|| {
        let _creation = SHARD_CREATION.lock();
        panic!("injected shard creation panic");
    })
    .join();
    let blob = crate::storage::blob_id::BlobId::generate().unwrap();
    let stage = blob.staging_path(&data.0);
    let published = blob.blob_path(&data.0);
    fs::create_dir_all(stage.parent().unwrap()).unwrap();
    fs::create_dir_all(published.parent().unwrap().parent().unwrap()).unwrap();
    fs::write(&stage, b"bytes").unwrap();
    publish_blob(&stage, &published).unwrap();
    assert_eq!(fs::read(&published).unwrap(), b"bytes");
    assert!(!SHARD_CREATION.is_poisoned());
}

#[test]
fn versioning_migration_copies_named_columns_from_permuted_legacy_table() {
    let mut connection = Connection::open_in_memory().unwrap();
    // Catalog validation rejects altered schemas; exercise migration mapping independently.
    let legacy = SCHEMA.replace(
        "bucket TEXT NOT NULL COLLATE BINARY,\n            name TEXT NOT NULL COLLATE BINARY,",
        "name TEXT NOT NULL COLLATE BINARY,\n            bucket TEXT NOT NULL COLLATE BINARY,",
    );
    assert_ne!(legacy, SCHEMA);
    connection.execute_batch(&legacy).unwrap();
    connection.execute_batch(METADATA_SCHEMA).unwrap();
    let columns = "bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata";
    connection.execute_batch(&format!(
        "INSERT INTO objects ({columns}) VALUES ('bucket','object',7,3,'text/plain',5,zeroblob(16),123,'NEARLINE',1234,5678,'opaque-blob','identity','inline','en','private','{{\"team\":\"storage\"}}');"
    )).unwrap();
    let selected = format!("SELECT {columns} FROM objects");
    let read_values = |row: &Row<'_>| {
        (0..17)
            .map(|index| row.get::<_, rusqlite::types::Value>(index))
            .collect::<rusqlite::Result<Vec<_>>>()
    };
    let before = connection.query_row(&selected, [], read_values).unwrap();
    migrate_versioning(&mut connection).unwrap();
    assert_eq!(
        connection.query_row(&selected, [], read_values).unwrap(),
        before
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT time_deleted, event_based_hold FROM objects",
                [],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, bool>(1)?))
            )
            .unwrap(),
        (None, false)
    );
}

/// A file holding `size` distinct bytes, and the reader over it with a read-ahead done.
fn read_ahead_fixture(size: usize) -> (crate::test_support::TestDir, Vec<u8>, ObjectReader) {
    let dir = crate::test_support::TestDir::new();
    let path = dir.0.join("blob");
    let contents: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();
    fs::write(&path, &contents).unwrap();
    let mut reader = ObjectReader::bounded_for_test(File::open(&path).unwrap(), size as u64);
    reader.read_ahead();
    (dir, contents, reader)
}

async fn drain(reader: &mut ObjectReader) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

#[test]
fn a_read_ahead_holds_a_small_object_whole() {
    let (_dir, contents, mut reader) = read_ahead_fixture(100 * 1024);
    assert_eq!(reader.next_frame_at_hand().unwrap(), contents);
    assert!(reader.is_finished());
    assert!(reader.next_frame_at_hand().is_none());
}

#[test]
fn an_empty_object_is_finished_without_reading() {
    let (_dir, _contents, reader) = read_ahead_fixture(0);
    assert!(reader.head.is_none());
    assert!(reader.is_finished());
}

#[tokio::test]
async fn a_read_ahead_starts_a_large_object_and_streaming_continues_it() {
    let (_dir, contents, mut reader) = read_ahead_fixture(3 * 1024 * 1024);
    assert_eq!(reader.read_ahead_len(), Some(1024 * 1024));
    assert_eq!(drain(&mut reader).await, contents);
}

/// A read-ahead is sized for a frame, which must not turn into a chunk over the chunk limit.
#[tokio::test]
async fn a_chunk_splits_a_larger_read_ahead_and_keeps_the_rest() {
    let (_dir, contents, mut reader) = read_ahead_fixture(100 * 1024);
    let first = reader.next_chunk().await.unwrap().unwrap();
    assert_eq!(first, contents[..CHUNK_SIZE]);
    assert_eq!(drain(&mut reader).await, contents[CHUNK_SIZE..]);
}

/// gRPC sizes its messages by this limit, unlike an HTTP frame.
#[tokio::test]
async fn a_chunk_stays_small_and_a_frame_may_be_larger() {
    let dir = crate::test_support::TestDir::new();
    let path = dir.0.join("blob");
    let size = 3 * 1024 * 1024;
    fs::write(&path, vec![7; size]).unwrap();
    let open = || ObjectReader::bounded_for_test(File::open(&path).unwrap(), size as u64);
    let mut reader = open();
    assert_eq!(
        reader.next_chunk().await.unwrap().unwrap().len(),
        CHUNK_SIZE
    );
    let mut reader = open();
    assert_eq!(
        reader.next_frame().await.unwrap().unwrap().len(),
        1024 * 1024
    );
}

/// Large frames only pay off while more than one of them remains.
#[tokio::test]
async fn the_last_megabyte_goes_out_in_small_frames() {
    let (_dir, contents, mut reader) = read_ahead_fixture(1024 * 1024 + 7);
    let mut sizes = Vec::new();
    let mut bytes = Vec::new();
    while let Some(frame) = reader.next_frame().await.unwrap() {
        sizes.push(frame.len());
        bytes.extend_from_slice(&frame);
    }
    assert_eq!(bytes, contents);
    assert_eq!(sizes[0], 1024 * 1024);
    assert_eq!(sizes[1..], [7]);
    let (_dir, _contents, mut reader) = read_ahead_fixture(1024 * 1024);
    assert_eq!(
        reader.next_frame().await.unwrap().unwrap().len(),
        256 * 1024
    );
}

#[tokio::test]
async fn a_frame_not_at_hand_leaves_the_reader_where_it_was() {
    let (_dir, contents, mut reader) = read_ahead_fixture(1024);
    reader.set_range(10, 20).await.unwrap();
    // Whether the page cache answered depends on the platform; the bytes must not.
    let at_hand = reader.next_frame_at_hand().unwrap_or_default();
    assert_eq!(at_hand, contents[10..10 + at_hand.len()]);
    assert_eq!(drain(&mut reader).await, contents[10 + at_hand.len()..30]);
}

#[tokio::test]
async fn peeking_keeps_a_read_ahead_and_a_range_discards_it() {
    let (_dir, contents, mut reader) = read_ahead_fixture(1024);
    assert_eq!(reader.peek_prefix(2).await.unwrap(), contents[..2]);
    assert!(reader.head.is_some());
    reader.set_range(10, 20).await.unwrap();
    assert!(reader.head.is_none());
    assert_eq!(drain(&mut reader).await, contents[10..30]);
}

#[tokio::test]
async fn a_read_ahead_is_not_lost_when_the_file_is_handed_over() {
    use std::io::Read;

    let (_dir, contents, reader) = read_ahead_fixture(1024);
    let mut bytes = Vec::new();
    reader.into_reader().await.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, contents);
    let (_dir, contents, reader) = read_ahead_fixture(1024);
    let mut bytes = Vec::new();
    reader
        .into_file()
        .await
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, contents);
}

/// A short file must not pass for a whole object: the missing end surfaces as a read failure
/// on the streaming path, after the headers that promised it.
#[tokio::test]
async fn a_truncated_blob_is_not_reported_as_whole() {
    let dir = crate::test_support::TestDir::new();
    let path = dir.0.join("blob");
    fs::write(&path, b"short").unwrap();
    let mut reader = ObjectReader::bounded_for_test(File::open(&path).unwrap(), 10);
    reader.read_ahead();
    assert_eq!(reader.read_ahead_len(), Some(5));
    assert!(!reader.is_finished());
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"short"[..]);
    assert_eq!(
        reader.next_chunk().await.unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[tokio::test]
async fn object_range_reader_seeks_sparse_blob_and_bounds_every_chunk() {
    use std::io::{Seek, Write};

    struct SparseBlob(PathBuf);
    impl Drop for SparseBlob {
        fn drop(&mut self) {
            fs::remove_file(&self.0).unwrap();
        }
    }
    let path = temp_catalog_dir("range-sparse");
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let _blob = SparseBlob(path);
    let size = 64_u64 * 1024 * 1024;
    file.set_len(size).unwrap();
    file.seek(io::SeekFrom::Start(size - 3)).unwrap();
    file.write_all(b"end").unwrap();
    let mut reader = ObjectReader {
        file: ReaderData::Disk(Arc::new(file)),
        remaining: size,
        position: size - 3,
        head: None,
        publicly_readable: false,
    };
    let offset = size - CHUNK_SIZE as u64 - 3;
    reader
        .set_range(offset, CHUNK_SIZE as u64 + 3)
        .await
        .unwrap();
    let chunk = reader.next_chunk().await.unwrap().unwrap();
    assert_eq!(chunk.len(), CHUNK_SIZE);
    assert!(chunk.iter().all(|byte| *byte == 0));
    assert_eq!(reader.peek_prefix(2).await.unwrap(), b"en"[..]);
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"end"[..]);
    assert!(reader.next_chunk().await.unwrap().is_none());
    reader.set_range(size - 3, 2).await.unwrap();
    assert_eq!(reader.peek_prefix(1).await.unwrap(), b"e"[..]);
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"en"[..]);
    assert!(reader.next_chunk().await.unwrap().is_none());
    reader.set_range(size, 1).await.unwrap();
    assert_eq!(
        reader.next_chunk().await.unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    drop(reader);
}

#[tokio::test]
async fn upload_reports_a_failed_final_write_instead_of_a_short_blob() {
    struct Blob(PathBuf);
    impl Drop for Blob {
        fn drop(&mut self) {
            fs::remove_file(&self.0).unwrap();
        }
    }
    let blob = Blob(temp_catalog_dir("final-write"));
    let context = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&context.0).await.unwrap();
    fs::write(&blob.0, b"").unwrap();
    // tokio completes a write in the background after write_all returns, so a
    // read-only handle makes exactly that final, unreported write fail.
    let mut file = UploadSink::Disk(tokio::fs::File::open(&blob.0).await.unwrap());
    let body = crate::storage::test_fixtures::body(vec![Bytes::from_static(b"tail")]);
    let (_cancel, cancellation) = oneshot::channel();
    let result = write_stream(
        &mut file,
        ExpectedHashes::default(),
        1024,
        body,
        cancellation,
        &catalog,
    )
    .await;
    assert!(
        matches!(
            result,
            Err(ObjectError::Unavailable(_) | ObjectError::Io(_))
        ),
        "{result:?}"
    );
    assert_eq!(fs::metadata(&blob.0).unwrap().len(), 0);
    catalog.close().await.unwrap();
}

#[test]
fn object_list_seeks_past_large_prefixes_with_bounded_sql_work() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(super::super::BUCKET_SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::BUCKET_RETENTION_SCHEMA)
        .unwrap();
    connection.execute_batch(SCHEMA).unwrap();
    connection.execute_batch("WITH RECURSIVE entries(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM entries WHERE n < 20000)
        INSERT INTO objects SELECT 'bucket', printf('a/%05d', n), n, 1, 'text/plain', 0, zeroblob(16), 0, 'STANDARD', 0, 0, printf('%032x', n) FROM entries;
        INSERT INTO objects VALUES('bucket', 'z', 20001, 1, 'text/plain', 0, zeroblob(16), 0, 'STANDARD', 0, 0, '00000000000000000000000000004e21');").unwrap();
    connection.execute_batch(METADATA_SCHEMA).unwrap();
    connection.execute_batch(VERSIONING_SCHEMA).unwrap();
    connection
        .execute_batch(super::super::derived::SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::OBJECT_HOLDS_CUSTOM_TIME_SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::SOFT_DELETE_SCHEMA)
        .unwrap();
    connection.execute_batch(retention::SCHEMA).unwrap();
    connection
        .execute_batch(super::super::customer_keys::SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::md5_availability::SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::object_owner::SCHEMA)
        .unwrap();
    connection
        .execute_batch(retention::SUB_MICRO_SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::object_hold::SCHEMA)
        .unwrap();
    let counter = AtomicUsize::new(0);
    // Interrupt after 10,000 VM instructions so a row scan cannot masquerade as a seek.
    #[expect(
        unsafe_code,
        reason = "Install the synchronous test-only progress callback while its counter is live."
    )]
    // SAFETY: This live connection invokes the callback synchronously with the live counter.
    unsafe {
        rusqlite::ffi::sqlite3_progress_handler(
            connection.handle(),
            100,
            Some(count_progress),
            std::ptr::from_ref(&counter).cast_mut().cast(),
        );
    }
    let mut query = ObjectListQuery {
        bucket: "bucket".into(),
        delimiter: "/".into(),
        ..Default::default()
    };
    let page = list_page(&connection, &query, 1).unwrap();
    assert_eq!(page.prefixes, ["a/"]);
    assert!(page.items.is_empty());
    query.cursor = page.next;
    let page = list_page(&connection, &query, 1).unwrap();
    assert_eq!(page.items[0].name, "z");
    assert!(page.next.is_none());
    query.cursor = None;
    query.versions = true;
    let page = list_page(&connection, &query, 1).unwrap();
    assert_eq!(page.prefixes, ["a/"]);
    query.cursor = page.next;
    let page = list_page(&connection, &query, 1).unwrap();
    assert_eq!(page.items[0].name, "z");
    assert!(page.next.is_none());
    #[expect(
        unsafe_code,
        reason = "Unregister the test-only progress callback before its counter is dropped."
    )]
    // SAFETY: Unregister on the live connection before the callback's counter is dropped.
    unsafe {
        rusqlite::ffi::sqlite3_progress_handler(connection.handle(), 0, None, std::ptr::null_mut());
    }
    assert!(counter.load(Ordering::Relaxed) < 100);
}

#[test]
fn object_list_skips_noncurrent_rows_with_bounded_sql_work() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(super::super::BUCKET_SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::BUCKET_RETENTION_SCHEMA)
        .unwrap();
    connection.execute_batch(SCHEMA).unwrap();
    connection.execute_batch(METADATA_SCHEMA).unwrap();
    connection.execute_batch(VERSIONING_SCHEMA).unwrap();
    connection
        .execute_batch(super::super::derived::SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::OBJECT_HOLDS_CUSTOM_TIME_SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::SOFT_DELETE_SCHEMA)
        .unwrap();
    connection.execute_batch(retention::SCHEMA).unwrap();
    connection
        .execute_batch(super::super::customer_keys::SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::md5_availability::SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::object_owner::SCHEMA)
        .unwrap();
    connection
        .execute_batch(retention::SUB_MICRO_SCHEMA)
        .unwrap();
    connection
        .execute_batch(super::super::object_hold::SCHEMA)
        .unwrap();
    connection.execute_batch("WITH RECURSIVE entries(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM entries WHERE n < 20000)
        INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,time_deleted)
        SELECT 'bucket', CASE WHEN n <= 10000 THEN 'a/history' ELSE printf('y/%05d',n) END, n,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',n),1 FROM entries;
        INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id)
        VALUES ('bucket','c',20001,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',20001)),
        ('bucket','d/file',20002,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',20002)),
        ('bucket','z',20003,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',20003));").unwrap();
    for source in [ListSource::Live, ListSource::Versions] {
        for start in [SeekStart::Inclusive, SeekStart::Exclusive] {
            for upper in [UpperBound::Open, UpperBound::Bounded] {
                let sql = format!("EXPLAIN QUERY PLAN {}", list_seek_sql(source, start, upper));
                let mut statement = connection.prepare(&sql).unwrap();
                let read = |row: &Row<'_>| row.get::<_, String>(3);
                let plan: Vec<String> = if matches!(upper, UpperBound::Bounded) {
                    statement
                        .query_map(params!["bucket", "", "zz"], read)
                        .unwrap()
                        .collect::<Result<_, _>>()
                        .unwrap()
                } else {
                    statement
                        .query_map(params!["bucket", ""], read)
                        .unwrap()
                        .collect::<Result<_, _>>()
                        .unwrap()
                };
                assert!(
                    plan.iter().any(|step| step.starts_with("SEARCH objects")),
                    "{plan:?}"
                );
                assert!(
                    plan.iter().all(|step| !step.contains("TEMP B-TREE")),
                    "{plan:?}"
                );
                if matches!(source, ListSource::Live) {
                    assert!(
                        plan.iter()
                            .any(|step| step.contains("USING INDEX objects_live")),
                        "{plan:?}"
                    );
                }
            }
        }
    }
    let counter = AtomicUsize::new(0);
    // One budget covers multiple pages and history-only prefixes, independent of history size.
    #[expect(
        unsafe_code,
        reason = "Install the synchronous test-only progress callback while its counter is live."
    )]
    // SAFETY: This live connection invokes the callback synchronously with the live counter.
    unsafe {
        rusqlite::ffi::sqlite3_progress_handler(
            connection.handle(),
            100,
            Some(count_progress),
            std::ptr::from_ref(&counter).cast_mut().cast(),
        );
    }
    for delimiter in ["", "/"] {
        let mut query = ObjectListQuery {
            bucket: "bucket".into(),
            delimiter: delimiter.into(),
            ..Default::default()
        };
        for expected in ["c", if delimiter.is_empty() { "d/file" } else { "d/" }, "z"] {
            let page = list_page(&connection, &query, 1).unwrap();
            let names: Vec<_> = page
                .items
                .iter()
                .map(|record| record.name.as_str())
                .chain(page.prefixes.iter().map(String::as_str))
                .collect();
            assert_eq!(names, [expected]);
            query.cursor = page.next;
        }
        assert!(query.cursor.is_none());
    }
    let mut query = ObjectListQuery {
        bucket: "bucket".into(),
        delimiter: "/".into(),
        versions: true,
        ..Default::default()
    };
    for expected in ["a/", "c", "d/", "y/", "z"] {
        let page = list_page(&connection, &query, 1).unwrap();
        let names: Vec<_> = page
            .items
            .iter()
            .map(|record| record.name.as_str())
            .chain(page.prefixes.iter().map(String::as_str))
            .collect();
        assert_eq!(names, [expected]);
        query.cursor = page.next;
    }
    query.delimiter.clear();
    query.prefix = "a/".into();
    query.cursor = Some(ObjectListCursor {
        name: "a/history".into(),
        is_prefix: false,
        generation: Some(9999),
    });
    let page = list_page(&connection, &query, 1).unwrap();
    assert_eq!(page.items[0].generation, 10000);
    assert!(page.next.is_none());
    #[expect(
        unsafe_code,
        reason = "Unregister the test-only progress callback before its counter is dropped."
    )]
    // SAFETY: Unregister on the live connection before the callback's counter is dropped.
    unsafe {
        rusqlite::ffi::sqlite3_progress_handler(connection.handle(), 0, None, std::ptr::null_mut());
    }
    assert!(counter.load(Ordering::Relaxed) < 100);
}

#[test]
fn metadata_migration_rolls_back_all_columns_and_version_after_late_failure() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(SCHEMA).unwrap();
    connection.execute_batch(GENERATION_SCHEMA).unwrap();
    connection.execute_batch("INSERT INTO object_generation VALUES (1,7);
        INSERT INTO objects VALUES ('bucket','object',7,1,'text/plain',3,zeroblob(16),0,'STANDARD',0,0,'a0000000000000000000000000000000');
        ALTER TABLE objects ADD COLUMN metadata TEXT;
        PRAGMA user_version = 3;").unwrap();
    let before: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='objects'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // The final ALTER fails after four successful statements, exercising transaction rollback.
    assert!(migrate_metadata(&mut connection).is_err());
    let after: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='objects'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        3
    );
    let data: (i64, String, i64) = connection
        .query_row(
            "SELECT generation,content_type,size FROM objects",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(data, (7, "text/plain".into(), 3));
}

#[tokio::test]
async fn prepared_upload_skips_advisory_repeat_but_rechecks_publication() {
    let directory = std::env::temp_dir().join(format!(
        "gcs-mimic-prechecked-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let catalog = Catalog::open(&directory).await.unwrap();
    for checksum_mismatch in [true, false] {
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        let conditions = ObjectConditions::default();
        let permit = catalog
            .prepare_object_upload("bucket", "object", conditions, PERMISSIVE)
            .await
            .unwrap();
        catalog
            .delete_bucket("bucket", Default::default(), PERMISSIVE)
            .await
            .unwrap();
        let body = crate::storage::test_fixtures::body(vec![Bytes::from_static(b"new")]);
        let expected = ExpectedHashes {
            md5: checksum_mismatch.then_some([0; 16]),
            crc32c: None,
        };
        let error = catalog
            .put_object_prechecked(
                ObjectUpload {
                    bucket: "bucket",
                    name: "object",
                    content_type: "text/plain".into(),
                    expected,
                    conditions,
                    max_bytes: 1000,
                    metadata: ObjectMetadataUpdate::default(),
                    access: BucketAccess::Permissive,
                },
                body,
                permit,
            )
            .await
            .unwrap_err();
        if checksum_mismatch {
            assert!(matches!(error, ObjectError::HashMismatch(_)), "{error:?}");
        } else {
            assert_eq!(error, ObjectError::BucketNotFound);
        }
        assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
    }
    catalog.close().await.unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn forbidden_overwrite_is_rechecked_atomically_after_a_concurrent_create() {
    let directory = std::env::temp_dir().join(format!(
        "gcs-mimic-no-overwrite-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let catalog = Catalog::open(&directory).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    let upload = |allow_overwrite, bytes: &'static [u8]| {
        let catalog = catalog.clone();
        async move {
            let permit = catalog
                .prepare_object_upload(
                    "bucket",
                    "object",
                    ObjectConditions::default(),
                    &project_access(allow_overwrite),
                )
                .await?;
            let body = crate::storage::test_fixtures::body(vec![Bytes::from_static(bytes)]);
            Ok::<_, ObjectError>((permit, body))
        }
    };
    // The advisory check passes while the object is absent; another writer then creates it.
    let (permit, body) = upload(false, b"late").await.unwrap();
    let (winner_permit, winner_body) = upload(true, b"winner").await.unwrap();
    let publish = |body, permit, allow_overwrite| {
        catalog.put_object_prechecked(
            ObjectUpload {
                bucket: "bucket",
                name: "object",
                content_type: "text/plain".into(),
                expected: ExpectedHashes::default(),
                conditions: ObjectConditions::default(),
                max_bytes: 1000,
                metadata: ObjectMetadataUpdate::default(),
                access: project_access(allow_overwrite),
            },
            body,
            permit,
        )
    };
    let winner = publish(winner_body, winner_permit, true).await.unwrap();
    assert_eq!(
        publish(body, permit, false).await.unwrap_err(),
        ObjectError::OverwriteForbidden
    );
    let current = catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(current, winner);
    assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
    // A failing precondition takes precedence: nothing would have been replaced.
    let conditions = ObjectConditions {
        if_generation_match: Some(0),
        ..Default::default()
    };
    let mismatch = ObjectConditions {
        if_generation_match: Some(winner.generation + 1),
        ..Default::default()
    };
    for (conditions, error) in [
        (conditions, ObjectError::PreconditionFailed),
        (mismatch, ObjectError::PreconditionFailed),
        (Default::default(), ObjectError::OverwriteForbidden),
    ] {
        assert_eq!(
            catalog
                .check_upload("bucket", "object", conditions, &project_access(false))
                .await
                .unwrap_err(),
            error
        );
    }
    assert!(
        catalog
            .check_upload(
                "bucket",
                "fresh",
                Default::default(),
                &project_access(false)
            )
            .await
            .is_ok()
    );
    catalog.close().await.unwrap();
    fs::remove_dir_all(directory).unwrap();
}

fn project_access(overwrite: bool) -> BucketAccess {
    policy_access("project", Some("project"), overwrite)
}

fn policy_access(project: &str, observed: Option<&str>, overwrite: bool) -> BucketAccess {
    use crate::auth::{Auth, Registry};
    let roles = if overwrite {
        vec!["roles/storage.admin"]
    } else {
        vec!["roles/storage.objectViewer", "roles/storage.objectCreator"]
    };
    let auth = Auth::strict(
        Registry::parse(
            serde_json::json!({"identities":[{
                "id":"local", "tokens":["token"], "project_roles":{project:roles}
            }]})
            .to_string()
            .as_bytes(),
        )
        .unwrap(),
    );
    let identity = auth.principals().unwrap().first().unwrap();
    BucketAccess::Policy {
        identity: Some(identity.clone()),
        permission: crate::auth::Permission::ObjectsCreate,
        project: observed.map(str::to_owned),
    }
}

fn temp_catalog_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "gcs-mimic-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

async fn create_bucket(catalog: &Catalog, project: &str) {
    catalog
        .insert_bucket(bucket("bucket", project))
        .await
        .unwrap();
}

fn body(bytes: &'static [u8]) -> mpsc::Receiver<Result<UploadChunk, io::Error>> {
    crate::storage::test_fixtures::body(vec![Bytes::from_static(bytes)])
}

async fn put(catalog: &Catalog, name: &str, bytes: &'static [u8]) -> ObjectRecord {
    catalog
        .put_object(permissive_upload("bucket", name, 1000), body(bytes))
        .await
        .unwrap()
}

async fn publish(
    catalog: &Catalog,
    name: &str,
    permit: UploadGuard,
    access: BucketAccess,
) -> Result<ObjectRecord, ObjectError> {
    catalog
        .put_object_prechecked(
            ObjectUpload {
                bucket: "bucket",
                name,
                content_type: "text/plain".into(),
                expected: ExpectedHashes::default(),
                conditions: ObjectConditions::default(),
                max_bytes: 1000,
                metadata: ObjectMetadataUpdate::default(),
                access,
            },
            body(b"late"),
            permit,
        )
        .await
}

/// Recreate "bucket" under `project` holding one object named "object".
async fn recreate_bucket(catalog: &Catalog, project: &str) -> ObjectRecord {
    for name in ["object", "fresh"] {
        let _ = catalog
            .delete_object(live("bucket", name), Default::default(), PERMISSIVE)
            .await;
    }
    let _ = catalog
        .delete_bucket("bucket", Default::default(), PERMISSIVE)
        .await;
    create_bucket(catalog, project).await;
    put(catalog, "object", b"other project").await
}

/// Every bucket and object operation must fail with `Forbidden` and change nothing.
async fn assert_all_forbidden(catalog: &Catalog, access: &BucketAccess, current: &ObjectRecord) {
    let conditions = ObjectConditions::default();
    assert_eq!(
        catalog
            .check_upload("bucket", "object", conditions, access)
            .await,
        Err(ObjectError::Forbidden)
    );
    assert_eq!(
        catalog
            .get_object(live("bucket", "object"), conditions, access)
            .await,
        Err(ObjectError::Forbidden)
    );
    assert_eq!(
        catalog
            .open_object(live("bucket", "object"), conditions, access)
            .await
            .map(|(record, _)| record),
        Err(ObjectError::Forbidden)
    );
    let query = ObjectListQuery {
        bucket: "bucket".into(),
        ..Default::default()
    };
    assert_eq!(
        catalog
            .list_objects(query, 10, access)
            .await
            .map(|page| page.items),
        Err(ObjectError::Forbidden)
    );
    let update = ObjectMetadataUpdate {
        content_type: Some(Some("application/json".into())),
        ..Default::default()
    };
    assert_eq!(
        catalog
            .update_object_metadata(
                live("bucket", "object"),
                conditions,
                update,
                |_| Ok(()),
                access,
            )
            .await,
        Err(ObjectError::Forbidden)
    );
    assert_eq!(
        catalog
            .delete_object(live("bucket", "object"), conditions, access)
            .await,
        Err(ObjectError::Forbidden)
    );
    assert_eq!(
        catalog.get_bucket("bucket", access).await,
        Err(CatalogError::Forbidden)
    );
    // Authorization precedes the non-empty check, so a non-empty bucket also reports 403.
    assert_eq!(
        catalog
            .delete_bucket("bucket", Default::default(), access)
            .await,
        Err(CatalogError::Forbidden)
    );
    assert_eq!(
        catalog
            .get_object(live("bucket", "object"), conditions, PERMISSIVE)
            .await
            .as_ref(),
        Ok(current)
    );
    assert!(
        catalog
            .get_bucket("bucket", PERMISSIVE)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn pap_is_rechecked_after_upload_admission_and_before_reads() {
    use crate::auth::{Binding, OBJECTS_CREATE, OBJECTS_GET, Policy};
    for parent in [false, true] {
        let directory = temp_catalog_dir("pap-recheck");
        let catalog = Catalog::open(&directory).await.unwrap();
        create_bucket(&catalog, "project").await;
        let current = put(&catalog, "object", b"original").await;
        let policy = Policy {
            bindings: vec![Binding {
                role: "roles/storage.admin".into(),
                members: vec!["allUsers".into()],
            }],
            etag: None,
        };
        let stored = catalog
            .set_policy("bucket", policy, PERMISSIVE)
            .await
            .unwrap();
        let read = catalog
            .bucket_access("bucket", None, OBJECTS_GET)
            .await
            .unwrap();
        let create = catalog
            .bucket_access("bucket", None, OBJECTS_CREATE)
            .await
            .unwrap();
        let permit = catalog
            .prepare_object_upload("bucket", "fresh", Default::default(), &create)
            .await
            .unwrap();
        if parent {
            catalog
                .configure_public_access_prevention(&["project".into(), "project".into()])
                .await
                .unwrap();
        } else {
            catalog
                .patch_bucket(
                    "bucket",
                    crate::bucket::BucketMetadataUpdate {
                        public_access_prevention_enforced: Some(true),
                        ..Default::default()
                    },
                    Default::default(),
                    PERMISSIVE,
                )
                .await
                .unwrap();
        }
        assert_all_forbidden(&catalog, &read, &current).await;
        assert_eq!(
            publish(&catalog, "fresh", permit, create).await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(
            catalog.get_policy("bucket", PERMISSIVE).await.unwrap(),
            stored
        );
        assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(directory).unwrap();
    }
}

#[tokio::test]
async fn object_acl_revocation_rechecks_admitted_reads_and_keeps_opened_snapshot() {
    use crate::{
        acl::AclTarget,
        auth::{Auth, OBJECTS_GET, Registry},
        storage::catalog::acl::AclChange,
    };
    for memory in [false, true] {
        let directory = temp_catalog_dir("revoked-object-acl");
        let catalog = if memory {
            Catalog::open_memory_for_server(
                1024 * 1024,
                4 * 1024 * 1024,
                Arc::new(crate::clock::SystemClock),
                crate::storage::admin::LocalAdmin::Disabled,
            )
            .await
            .unwrap()
        } else {
            Catalog::open(&directory).await.unwrap()
        };
        create_bucket(&catalog, "project").await;
        let current = put(&catalog, "object", b"selected snapshot").await;
        let auth = Auth::strict(
            Registry::parse(br#"{"identities":[{"id":"reader","tokens":["token"]}]}"#).unwrap(),
        );
        let mut headers = hyper::HeaderMap::new();
        headers.insert("authorization", "Bearer token".parse().unwrap());
        let identity = auth
            .authenticate(&headers, None)
            .await
            .unwrap()
            .unwrap()
            .principal()
            .unwrap();
        let access = catalog
            .bucket_access("bucket", Some(identity), OBJECTS_GET)
            .await
            .unwrap();
        assert_eq!(
            catalog
                .get_object(live("bucket", "object"), Default::default(), &access)
                .await,
            Err(ObjectError::Forbidden)
        );
        let target = AclTarget::Object {
            bucket: "bucket".into(),
            name: "object".into(),
            generation: Some(current.generation),
        };
        catalog
            .change_acl(
                target.clone(),
                AclChange::Upsert {
                    entity: "user-reader".into(),
                    role: Some(crate::acl::AclRole::Reader),
                },
                PERMISSIVE,
            )
            .await
            .unwrap();
        let (_, reader) = catalog
            .open_object(live("bucket", "object"), Default::default(), &access)
            .await
            .unwrap();
        let revoke = catalog.change_acl(
            target,
            AclChange::Delete {
                entity: "user-reader".into(),
            },
            PERMISSIVE,
        );
        let read = catalog.get_object(live("bucket", "object"), Default::default(), &access);
        let (revoked, concurrent) = tokio::join!(revoke, read);
        revoked.unwrap();
        assert!(
            concurrent
                .as_ref()
                .is_ok_and(|record| record.generation == current.generation)
                || concurrent == Err(ObjectError::Forbidden)
        );
        assert_eq!(
            catalog
                .get_object(
                    live("bucket", "object"),
                    ObjectConditions {
                        if_generation_match: Some(0),
                        ..Default::default()
                    },
                    &access
                )
                .await,
            Err(ObjectError::Forbidden)
        );
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut reader.into_reader().await, &mut bytes).unwrap();
        assert_eq!(bytes, b"selected snapshot");
        catalog.close().await.unwrap();
        if !memory {
            fs::remove_dir_all(directory).unwrap();
        }
    }
}

#[tokio::test]
async fn revoked_bucket_binding_is_rechecked_inside_operations() {
    use crate::auth::{
        Auth, BUCKETS_GET_IAM, BUCKETS_SET_IAM, Binding, OBJECTS_CREATE, OBJECTS_GET, Policy,
        Registry,
    };
    for public in [false, true] {
        let directory = temp_catalog_dir("revoked-policy");
        let catalog = Catalog::open(&directory).await.unwrap();
        create_bucket(&catalog, "project").await;
        let current = put(&catalog, "object", b"original").await;
        let auth = Auth::strict(
            Registry::parse(br#"{"identities":[{"id":"local", "tokens":["token"]}]}"#).unwrap(),
        );
        let mut headers = hyper::HeaderMap::new();
        headers.insert("authorization", "Bearer token".parse().unwrap());
        let identity = auth
            .authenticate(&headers, None)
            .await
            .unwrap()
            .unwrap()
            .principal()
            .unwrap();
        let identity = if public { None } else { Some(identity) };
        let granted = Policy {
            bindings: vec![Binding {
                role: "roles/storage.admin".into(),
                members: vec![if public { "allUsers" } else { "user:local" }.into()],
            }],
            etag: None,
        };
        catalog
            .set_policy("bucket", granted, PERMISSIVE)
            .await
            .unwrap();
        let read = catalog
            .bucket_access("bucket", identity, OBJECTS_GET)
            .await
            .unwrap();
        let create = catalog
            .bucket_access("bucket", identity, OBJECTS_CREATE)
            .await
            .unwrap();
        let get_iam = catalog
            .bucket_access("bucket", identity, BUCKETS_GET_IAM)
            .await
            .unwrap();
        let set_iam = catalog
            .bucket_access("bucket", identity, BUCKETS_SET_IAM)
            .await
            .unwrap();
        let permit = catalog
            .prepare_object_upload("bucket", "fresh", Default::default(), &create)
            .await
            .unwrap();
        // These accesses passed advisory authorization; transaction rechecks must observe
        // revocation before selecting a blob or publishing an upload.
        catalog
            .set_policy(
                "bucket",
                Policy {
                    bindings: vec![],
                    etag: None,
                },
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert_all_forbidden(&catalog, &read, &current).await;
        assert_eq!(
            catalog.get_policy("bucket", &get_iam).await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog
                .set_policy(
                    "bucket",
                    Policy {
                        bindings: vec![],
                        etag: None
                    },
                    &set_iam
                )
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            publish(&catalog, "fresh", permit, create).await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(directory).unwrap();
    }
}

#[tokio::test]
async fn operations_recheck_the_authorized_project_after_bucket_recreation() {
    let directory = temp_catalog_dir("recreated-bucket");
    let catalog = Catalog::open(&directory).await.unwrap();
    create_bucket(&catalog, "project").await;
    let access = project_access(true);
    // The advisory check passes in the authorized project; the bucket is then deleted and
    // recreated under another project before the body completes.
    let permit = catalog
        .prepare_object_upload("bucket", "object", Default::default(), &access)
        .await
        .unwrap();
    let current = recreate_bucket(&catalog, "other-project").await;
    assert_eq!(
        publish(&catalog, "object", permit, access.clone()).await,
        Err(ObjectError::Forbidden)
    );
    let permit = catalog.admit_object_upload().unwrap();
    assert_eq!(
        publish(&catalog, "fresh", permit, access.clone()).await,
        Err(ObjectError::Forbidden)
    );
    assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
    assert_all_forbidden(&catalog, &access, &current).await;
    let page = catalog
        .list_objects(
            ObjectListQuery {
                bucket: "bucket".into(),
                ..Default::default()
            },
            10,
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(page.items, vec![current.clone()]);
    // An access authorized against the bucket's actual project still succeeds.
    let actual = policy_access("other-project", Some("other-project"), false);
    assert_eq!(
        catalog
            .get_object(live("bucket", "object"), Default::default(), &actual)
            .await,
        Ok(current)
    );
    catalog.close().await.unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn missing_bucket_access_reauthorizes_against_the_actual_project() {
    let directory = temp_catalog_dir("missing-bucket-access");
    let catalog = Catalog::open(&directory).await.unwrap();
    let BucketAccess::Policy {
        identity,
        permission,
        ..
    } = policy_access("project", None, false)
    else {
        unreachable!()
    };
    let access = catalog
        .bucket_access("bucket", identity.as_ref(), permission)
        .await
        .unwrap();
    let conditions = ObjectConditions::default();
    // Still missing: the caller holds the permission somewhere, so it learns 404.
    assert_eq!(
        catalog
            .check_upload("bucket", "object", conditions, &access)
            .await,
        Err(ObjectError::BucketNotFound)
    );
    assert_eq!(
        catalog
            .get_object(live("bucket", "object"), conditions, &access)
            .await,
        Err(ObjectError::BucketNotFound)
    );
    assert_eq!(
        catalog
            .delete_object(live("bucket", "object"), conditions, &access)
            .await,
        Err(ObjectError::BucketNotFound)
    );
    assert_eq!(catalog.get_bucket("bucket", &access).await, Ok(None));
    assert_eq!(
        catalog
            .delete_bucket("bucket", Default::default(), &access)
            .await,
        Err(CatalogError::NotFound)
    );
    // Created later in a project where the caller lacks the permission.
    create_bucket(&catalog, "project").await;
    let permit = catalog
        .prepare_object_upload("bucket", "object", conditions, &access)
        .await
        .unwrap();
    let current = recreate_bucket(&catalog, "other-project").await;
    assert_eq!(
        publish(&catalog, "object", permit, access.clone()).await,
        Err(ObjectError::Forbidden)
    );
    assert_all_forbidden(&catalog, &access, &current).await;
    // Created later in a permitted project: overwriting still needs storage.objects.delete
    // there and never defaults to allowed.
    recreate_bucket(&catalog, "other-project").await;
    let current = recreate_bucket(&catalog, "project").await;
    let permit = catalog.admit_object_upload().unwrap();
    assert_eq!(
        publish(&catalog, "object", permit, access.clone()).await,
        Err(ObjectError::OverwriteForbidden)
    );
    assert_eq!(
        catalog
            .get_object(live("bucket", "object"), conditions, &access)
            .await,
        Ok(current)
    );
    let permit = catalog.admit_object_upload().unwrap();
    assert!(publish(&catalog, "fresh", permit, access).await.is_ok());
    let deleter = policy_access("project", None, true);
    let permit = catalog.admit_object_upload().unwrap();
    assert!(publish(&catalog, "object", permit, deleter).await.is_ok());
    assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
    catalog.close().await.unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn failed_v1_migration_never_commits_an_unreopenable_v2_catalog() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(super::super::BUCKET_SCHEMA)
        .unwrap();
    // Force the second DDL to fail after the object tables and v2 marker were written.
    connection.execute_batch(GENERATION_SCHEMA).unwrap();
    assert!(migrate(&mut connection).is_err());
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let objects: i64 = connection.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('objects', 'pending_blob_deletions')", [], |row| row.get(0)).unwrap();
    assert_eq!(objects, 0);
}
