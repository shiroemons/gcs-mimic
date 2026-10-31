use super::*;
use crate::test_support::TestDir;

mod owner_tests;

async fn catalog_with_bucket(dir: &TestDir) -> Catalog {
    let catalog = Catalog::open(&dir.0).await.unwrap();
    insert_bucket(&catalog).await;
    catalog
}

async fn insert_bucket(catalog: &Catalog) {
    let definition =
        crate::json::parse_insert(br#"{"name":"append"}"#, "project", SystemTime::now()).unwrap();
    catalog.insert_bucket(definition).await.unwrap();
}

fn options() -> ResumableOptions {
    let mut options = ResumableOptions::new(BucketAccess::Permissive);
    options.content_type = "text/plain".into();
    options
}

#[tokio::test]
async fn ram_and_disk_cancelled_takeover_keeps_slot_registered_until_truncation_finishes() {
    for backend in [crate::config::Backend::Disk, crate::config::Backend::Memory] {
        let dir = TestDir::new();
        let data_dir = dir.0.join("state");
        let catalog = match backend {
            crate::config::Backend::Disk => Catalog::open(&data_dir).await.unwrap(),
            crate::config::Backend::Memory => Catalog::open_backend(
                data_dir.clone(),
                Arc::new(crate::clock::SystemClock),
                super::super::admin::LocalAdmin::Enabled,
                Some((8192, 1024 * 1024)),
            )
            .await
            .unwrap(),
        };
        insert_bucket(&catalog).await;
        let mut writer = catalog
            .create_appendable("append", "takeover", options())
            .await
            .unwrap();
        writer
            .append(0, Bytes::from_static(b"kept"), 1024)
            .await
            .unwrap();
        writer.flush().await.unwrap();
        let generation = writer.record().generation;
        drop(writer);
        let (entered, release) = arm_write_pause(&data_dir);
        let worker = catalog.clone();
        let request = tokio::spawn(async move { take_over(&worker, "takeover", generation).await });
        entered.await.unwrap();
        request.abort();
        let Err(error) = request.await else {
            panic!("cancelled takeover completed");
        };
        assert!(error.is_cancelled());
        let worker = catalog.clone();
        let mut successor =
            tokio::spawn(async move { take_over(&worker, "takeover", generation).await });
        let blocked = tokio::time::timeout(std::time::Duration::from_millis(50), &mut successor)
            .await
            .is_err();
        release.send(()).unwrap();
        assert!(blocked, "successor must wait for the cancelled truncation");
        let mut current = successor.await.unwrap();
        current
            .append(4, Bytes::from_static(b"next"), 1024)
            .await
            .unwrap();
        current.flush().await.unwrap();
        assert_eq!(persisted_content(&catalog, "takeover").await, b"keptnext");
        drop(current);
        catalog.close().await.unwrap();
        if backend == crate::config::Backend::Memory {
            assert!(!data_dir.exists());
        }
    }
}

#[tokio::test]
async fn ram_appendable_cancelled_finalizer_finishes_owned_publication_and_reclaims_draft() {
    type Gate = (
        tokio::sync::oneshot::Sender<()>,
        std::sync::mpsc::Receiver<()>,
    );
    struct Time(Mutex<Option<Gate>>);
    impl crate::clock::Clock for Time {
        fn now(&self) -> SystemTime {
            if let Some((entered, release)) = self.0.lock().unwrap().take() {
                entered.send(()).unwrap();
                release.recv().unwrap();
            }
            SystemTime::now()
        }
    }
    let clock = Arc::new(Time(Mutex::new(None)));
    let catalog = Catalog::open_memory_for_server(
        1024,
        1024 * 1024,
        clock.clone(),
        super::super::admin::LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    insert_bucket(&catalog).await;
    let mut writer = catalog
        .create_appendable("append", "cancelled", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"body"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    let id = writer.blob_id.clone();
    let generation = writer.record().generation;
    let (_, mut reader) = catalog
        .open_appendable(
            "append",
            "cancelled",
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    *clock.0.lock().unwrap() = Some((entered, blocked));
    let request =
        tokio::spawn(async move { writer.finalize(None, BucketAccess::Permissive, 1024).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), started)
        .await
        .unwrap()
        .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    let published = catalog
        .get_object("append", "cancelled", None, Default::default())
        .await
        .unwrap();
    assert_eq!((published.generation, published.size), (generation, 4));
    catalog.reclaim_pending().await.unwrap();
    let memory = catalog.0.memory.as_ref().unwrap();
    assert!(memory.open(&id).is_err());
    assert!(memory.draft(&id, 0).is_err());
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"body"[..]);
    assert!(reader.next_chunk().await.unwrap().is_none());
    assert!(matches!(
        catalog
            .take_over_appendable(
                "append",
                "cancelled",
                generation,
                None,
                Default::default(),
                BucketAccess::Permissive
            )
            .await,
        Err(AppendError::Finalized)
    ));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_appendable_cancelled_creations_leave_no_unreferenced_blob_budget() {
    let catalog = Catalog::open_memory_for_server(
        600,
        1024 * 1024,
        Arc::new(crate::clock::SystemClock),
        super::super::admin::LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    insert_bucket(&catalog).await;
    let slot = SlotHandle::new(&catalog, "append", "cancelled");
    let guard = slot.lock().await;
    for _ in 0..16 {
        let worker = catalog.clone();
        let request = tokio::spawn(async move {
            worker
                .create_appendable("append", "cancelled", options())
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while slots().lock().unwrap().get(&slot.key).unwrap().holders != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        request.abort();
        let Err(error) = request.await else {
            panic!("cancelled creation completed");
        };
        assert!(error.is_cancelled());
        assert_eq!(slots().lock().unwrap().get(&slot.key).unwrap().holders, 1);
    }
    drop(guard);
    drop(slot);
    let rows: i64 = catalog
        .run_upload(|connection| {
            connection
                .prepare_cached("SELECT count(*) FROM appendable_objects")
                .unwrap()
                .query_row([], |row| row.get(0))
                .map_err(object_unavailable)
        })
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let mut writer = catalog
        .create_appendable("append", "usable", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"data"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    assert_eq!(persisted_content(&catalog, "usable").await, b"data");
    drop(writer);
    catalog.close().await.unwrap();
}

/// Finalization uses shared publication, so the bucket's default object ACL is inherited and
/// Autoclass resolves the class; the unfinalized view already reports the same class.
/// Retention and customer-supplied keys are rejected rather than published silently.
#[tokio::test]
async fn finalization_inherits_default_acl_and_autoclass_class() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let definition = crate::json::parse_insert(
        br#"{"name":"auto","autoclass":{"enabled":true}}"#,
        "project",
        SystemTime::now(),
    )
    .unwrap();
    catalog.insert_bucket(definition).await.unwrap();
    catalog
        .run_upload(|connection| {
            connection
                .execute(
                    "INSERT INTO acl_bucket_entries VALUES ('auto', 'defaultObject', '[{\"entity\":\"allUsers\",\"role\":\"READER\"}]')",
                    [],
                )
                .map_err(object_unavailable)?;
            Ok(())
        })
        .await
        .unwrap();
    // Autoclass places new objects in STANDARD whatever class the writer requests.
    let mut requested = options();
    requested.metadata.storage_class = Some("NEARLINE".into());
    let mut writer = catalog
        .create_appendable("auto", "classed", requested)
        .await
        .unwrap();
    assert_eq!(writer.record().storage_class, "STANDARD");
    writer
        .append(0, Bytes::from_static(b"x"), 1024)
        .await
        .unwrap();
    let record = writer
        .finalize(None, BucketAccess::Permissive, 1024)
        .await
        .unwrap();
    assert_eq!(record.storage_class, "STANDARD");
    let generation = record.generation;
    let inherited: String = catalog
        .run_upload(move |connection| {
            connection
                .query_row(
                    "SELECT entries FROM acl_object_entries WHERE bucket = 'auto' AND name = 'classed' AND generation = ?1",
                    [generation],
                    |row| row.get(0),
                )
                .map_err(object_unavailable)
        })
        .await
        .unwrap();
    assert!(inherited.contains("allUsers"));

    let mut encrypted = options();
    encrypted.metadata.customer_key_sha256 = Some([7; 32]);
    assert!(matches!(
        catalog
            .create_appendable("auto", "encrypted", encrypted)
            .await
            .err(),
        Some(AppendError::Object(ObjectError::UnsupportedMetadata(_)))
    ));
}

/// A schema 33 catalog gains the appendable table; its existing objects are unchanged.
#[tokio::test]
async fn schema_33_catalog_migrates_to_appendable_objects() {
    let dir = TestDir::new();
    let catalog = catalog_with_bucket(&dir).await;
    let mut writer = catalog
        .create_appendable("append", "before", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"kept"), 1024)
        .await
        .unwrap();
    writer
        .finalize(None, BucketAccess::Permissive, 1024)
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let path = dir.0.join("catalog.sqlite3");
    let connection = Connection::open(&path).unwrap();
    super::super::test_support::downgrade_appendable_schema(&connection);
    connection.pragma_update(None, "user_version", 33).unwrap();
    super::super::validate_schema(&connection, 33).unwrap();
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog
            .get_object_authorized(
                "append",
                "before",
                None,
                Default::default(),
                &BucketAccess::Permissive
            )
            .await
            .unwrap()
            .size,
        4
    );
    assert_eq!(
        open(&catalog, "missing").await.err(),
        Some(ObjectError::ObjectNotFound)
    );
    catalog.close().await.unwrap();
    let connection = Connection::open(&path).unwrap();
    super::super::validate_schema(&connection, super::super::CATALOG_SCHEMA_VERSION).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        super::super::CATALOG_SCHEMA_VERSION
    );
}

async fn open(catalog: &Catalog, name: &str) -> Result<ObjectRecord, ObjectError> {
    catalog
        .open_appendable(
            "append",
            name,
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .map(|(record, _)| record)
}

/// A crash after finalization's publication commit leaves the appendable row behind. The
/// published generation wins, and the leftover row and file are removed by the next creation.
#[tokio::test]
async fn appendable_row_left_by_crash_after_publication_is_stale() {
    let dir = TestDir::new();
    let catalog = catalog_with_bucket(&dir).await;
    let mut writer = catalog
        .create_appendable("append", "crash", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"abc"), 1024)
        .await
        .unwrap();
    let published = writer
        .finalize(None, BucketAccess::Permissive, 1024)
        .await
        .unwrap();
    assert_eq!(published.size, 3);
    assert!(!writer.path.exists());

    // Recreate the state between the publication commit and the row removal.
    let leftover = BlobId::generate().unwrap().to_hex();
    let leftover_path = dir.0.join("appendable").join(&leftover);
    fs::write(&leftover_path, b"abc").unwrap();
    let orphan_path = dir
        .0
        .join("appendable")
        .join(BlobId::generate().unwrap().to_hex());
    fs::write(&orphan_path, b"never committed").unwrap();
    let (id, generation) = (leftover.clone(), published.generation);
    catalog
        .run_upload(move |connection| {
            let incarnation = incarnation(connection, "append")?.unwrap();
            let encoded = encode_options(&options()).to_string();
            connection
                .execute(
                    "INSERT INTO appendable_objects VALUES ('append', 'crash', ?1, ?2, ?3, ?4, 3, 0, 1, 1, ?5, 1)",
                    params![generation, incarnation, id, encoded, "0".repeat(32)],
                )
                .map_err(object_unavailable)?;
            Ok(())
        })
        .await
        .unwrap();
    // At runtime the published generation already wins.
    assert_eq!(
        open(&catalog, "crash").await.err(),
        Some(ObjectError::ObjectNotFound)
    );
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    // Recovery retires the row before a delete could make the leftover look unpublished.
    assert!(!leftover_path.exists());
    assert!(!orphan_path.exists());
    assert_eq!(
        catalog
            .take_over_appendable(
                "append",
                "crash",
                generation,
                None,
                Default::default(),
                BucketAccess::Permissive,
            )
            .await
            .err(),
        Some(AppendError::Finalized)
    );
    catalog
        .delete_object_authorized(
            "append",
            "crash",
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(
        open(&catalog, "crash").await.err(),
        Some(ObjectError::ObjectNotFound)
    );
    let writer = catalog
        .create_appendable("append", "crash", options())
        .await
        .unwrap();
    assert!(writer.record().generation > generation);
}

/// A crash before the publication commit leaves a `finalizing` row whose generation was never
/// published; recovery makes it appendable again with its persisted bytes.
#[tokio::test]
async fn interrupted_finalization_before_publication_stays_appendable() {
    let dir = TestDir::new();
    let catalog = catalog_with_bucket(&dir).await;
    let mut writer = catalog
        .create_appendable("append", "pending", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"kept"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    writer.mark_finalizing(Finalizing::Started).await.unwrap();
    let generation = writer.record().generation;
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(open(&catalog, "pending").await.unwrap().size, 4);
    let mut writer = catalog
        .take_over_appendable(
            "append",
            "pending",
            generation,
            None,
            Default::default(),
            BucketAccess::Permissive,
        )
        .await
        .unwrap();
    let record = writer
        .finalize(
            Some(crc32c::crc32c(b"kept")),
            BucketAccess::Permissive,
            1024,
        )
        .await
        .unwrap();
    assert_eq!((record.generation, record.size), (generation, 4));
}

/// Publication of the reserved generation removes the appendable row in its own commit, so a
/// finalization that stops right after that commit cannot leave a row that would look
/// unfinalized again, with its old bytes, once the published generation is deleted.
#[tokio::test]
async fn publishing_a_reserved_generation_retires_its_row_atomically() {
    let dir = TestDir::new();
    let catalog = catalog_with_bucket(&dir).await;
    let mut writer = catalog
        .create_appendable("append", "atomic", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"old"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    let record = writer.record().clone();
    drop(writer);
    let identity = super::super::admin::ImportedIdentity::new(
        super::super::admin::Generation::new(record.generation).unwrap(),
        record.time_created,
        record.updated,
    )
    .unwrap();
    // Publish exactly as finalization does, without the steps that follow its commit.
    let (tx, rx) = mpsc::channel(2);
    tx.send(Ok(UploadChunk::Data(Bytes::from_static(b"old"))))
        .await
        .unwrap();
    tx.send(Ok(UploadChunk::End)).await.unwrap();
    let published = catalog
        .put_object_with_mode(
            ObjectUpload {
                bucket: "append",
                name: "atomic",
                content_type: record.content_type.clone(),
                expected: Default::default(),
                conditions: ObjectConditions {
                    if_generation_match: Some(0),
                    ..Default::default()
                },
                max_bytes: 1024,
                metadata: Default::default(),
                access: BucketAccess::Permissive,
            },
            rx,
            catalog.admit_object_upload().unwrap(),
            super::super::admin::PublishMode::Replace,
            Some(identity),
            super::super::objects::OwnerAssignment::Preserve(record.owner.clone()),
        )
        .await
        .unwrap();
    assert_eq!(published.generation, record.generation);
    let rows: i64 = catalog
        .run_upload(|connection| {
            connection
                .query_row("SELECT COUNT(*) FROM appendable_objects", [], |row| {
                    row.get(0)
                })
                .map_err(object_unavailable)
        })
        .await
        .unwrap();
    assert_eq!(rows, 0);
    catalog
        .delete_object_authorized(
            "append",
            "atomic",
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(
        open(&catalog, "atomic").await.err(),
        Some(ObjectError::ObjectNotFound)
    );
}

/// The writer registry tracks only objects with a live in-process user, so it does not grow
/// with the number of objects ever appended, while a takeover still shares the slot that
/// supersedes the earlier writer.
#[tokio::test]
async fn writer_slots_are_released_with_their_last_holder() {
    let dir = TestDir::new();
    let catalog = catalog_with_bucket(&dir).await;
    for index in 0..64 {
        let name = format!("object-{index}");
        let mut writer = catalog
            .create_appendable("append", &name, options())
            .await
            .unwrap();
        writer
            .append(0, Bytes::from_static(b"abc"), 1024)
            .await
            .unwrap();
        if index % 2 == 0 {
            writer
                .finalize(None, BucketAccess::Permissive, 1024)
                .await
                .unwrap();
        } else {
            // An abandoned stream drops its writer without finalizing.
            writer.flush().await.unwrap();
        }
        drop(writer);
        assert_eq!(slot_count(&dir.0), 0);
    }

    let mut first = catalog
        .create_appendable("append", "shared", options())
        .await
        .unwrap();
    let generation = first.record().generation;
    let mut second = catalog
        .take_over_appendable(
            "append",
            "shared",
            generation,
            None,
            Default::default(),
            BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(slot_count(&dir.0), 1);
    assert_eq!(
        first.append(0, Bytes::from_static(b"x"), 1024).await,
        Err(AppendError::Superseded)
    );
    drop(first);
    assert_eq!(slot_count(&dir.0), 1);
    second
        .append(0, Bytes::from_static(b"y"), 1024)
        .await
        .unwrap();
    assert_eq!(second.flush().await.unwrap(), 1);
    drop(second);
    assert_eq!(slot_count(&dir.0), 0);
}

/// Rows belong to one bucket incarnation, so a recreated bucket never resurrects them.
#[tokio::test]
async fn appendable_objects_do_not_survive_bucket_recreation() {
    let dir = TestDir::new();
    let catalog = catalog_with_bucket(&dir).await;
    let mut writer = catalog
        .create_appendable("append", "old", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"old"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    assert_eq!(open(&catalog, "old").await.unwrap().size, 3);
    catalog.delete_bucket("append", None, None).await.unwrap();
    insert_bucket(&catalog).await;
    assert_eq!(
        open(&catalog, "old").await.err(),
        Some(ObjectError::ObjectNotFound)
    );
    // The process-local writer still owns its file; only the durable size update is refused.
    assert_eq!(
        writer.append(3, Bytes::from_static(b"x"), 1024).await,
        Ok(())
    );
    assert_eq!(writer.flush().await, Err(AppendError::Superseded));
    assert_eq!(
        catalog
            .create_appendable("append", "old", options())
            .await
            .unwrap()
            .persisted_size(),
        0
    );
}

#[tokio::test]
async fn appendable_size_limit_and_reader_bound_by_persisted_size() {
    let dir = TestDir::new();
    let catalog = catalog_with_bucket(&dir).await;
    let mut writer = catalog
        .create_appendable("append", "bounded", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"1234"), 8)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    writer
        .append(4, Bytes::from_static(b"5678"), 8)
        .await
        .unwrap();
    assert_eq!(
        writer.append(8, Bytes::from_static(b"9"), 8).await,
        Err(AppendError::Object(ObjectError::TooLarge))
    );
    let (record, mut reader) = catalog
        .open_appendable(
            "append",
            "bounded",
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(record.size, 4);
    let mut content = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        content.extend_from_slice(&chunk);
    }
    assert_eq!(content, b"1234");
    assert_eq!(
        catalog
            .open_appendable(
                "append",
                "bounded",
                Some(record.generation + 1),
                Default::default(),
                &BucketAccess::Permissive,
            )
            .await
            .err(),
        Some(ObjectError::ObjectNotFound)
    );
    assert_eq!(
        catalog
            .open_appendable(
                "append",
                "bounded",
                None,
                ObjectConditions {
                    if_generation_not_match: Some(record.generation),
                    ..Default::default()
                },
                &BucketAccess::Permissive,
            )
            .await
            .err(),
        Some(ObjectError::NotModified)
    );
}

async fn persisted_content(catalog: &Catalog, name: &str) -> Vec<u8> {
    let (_, mut reader) = catalog
        .open_appendable(
            "append",
            name,
            None,
            Default::default(),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    let mut content = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        content.extend_from_slice(&chunk);
    }
    content
}

async fn take_over(catalog: &Catalog, name: &str, generation: i64) -> AppendWriter {
    catalog
        .take_over_appendable(
            "append",
            name,
            generation,
            None,
            Default::default(),
            BucketAccess::Permissive,
        )
        .await
        .unwrap()
}

/// A dropped append whose blocking write is still running must keep the slot until the write
/// ends. Otherwise a takeover truncates the file, the new writer commits bytes, and the late
/// write through the old descriptor overwrites them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_append_cannot_overwrite_bytes_committed_after_a_takeover() {
    let dir = TestDir::new();
    let catalog = catalog_with_bucket(&dir).await;
    let mut writer = catalog
        .create_appendable("append", "dropped", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"kept"), 1024)
        .await
        .unwrap();
    assert_eq!(writer.flush().await, Ok(4));
    let generation = writer.record().generation;
    let (entered, release) = arm_write_pause(&catalog.0.data_dir);
    {
        let append = writer.append(4, Bytes::from_static(b"LATE"), 1024);
        tokio::pin!(append);
        tokio::select! {
            result = &mut append => panic!("the paused write finished: {result:?}"),
            entered = entered => entered.unwrap(),
        }
        // Dropping the future here leaves its blocking write in flight.
    }
    let taker = catalog.clone();
    let mut takeover = tokio::spawn(async move {
        let mut next = take_over(&taker, "dropped", generation).await;
        next.append(4, Bytes::from_static(b"next"), 1024)
            .await
            .unwrap();
        next.flush().await.unwrap()
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), &mut takeover)
            .await
            .is_err(),
        "the takeover waits for the in-flight write"
    );
    release.send(()).unwrap();
    assert_eq!(takeover.await.unwrap(), 8);
    assert_eq!(persisted_content(&catalog, "dropped").await, b"keptnext");
    drop(writer);
}

#[tokio::test]
async fn failed_append_write_is_not_received_and_can_be_retried() {
    use super::super::fault::{Fault, Step};
    let dir = TestDir::new();
    let catalog = catalog_with_bucket(&dir).await;
    let mut writer = catalog
        .create_appendable("append", "write", options())
        .await
        .unwrap();
    catalog.inject(Step::AppendWrite, 0, Fault::Io(io::ErrorKind::StorageFull));
    assert!(matches!(
        writer.append(0, Bytes::from_static(b"abc"), 1024).await,
        Err(AppendError::Object(_))
    ));
    assert_eq!(catalog.pending_faults(), 0);
    assert_eq!(writer.flush().await, Ok(0), "nothing was received");
    writer
        .append(0, Bytes::from_static(b"abc"), 1024)
        .await
        .unwrap();
    assert_eq!(writer.flush().await, Ok(3));
    assert_eq!(persisted_content(&catalog, "write").await, b"abc");
}

/// A failed sync may already have lost the unflushed bytes, and a retried sync can succeed
/// without writing them, so the writer must receive them again instead of publishing them.
#[tokio::test]
async fn failed_append_sync_is_never_published_by_a_retried_flush() {
    use super::super::fault::{Fault, Step};
    let dir = TestDir::new();
    let catalog = catalog_with_bucket(&dir).await;
    let mut writer = catalog
        .create_appendable("append", "sync", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"one"), 1024)
        .await
        .unwrap();
    assert_eq!(writer.flush().await, Ok(3));
    writer
        .append(3, Bytes::from_static(b"two"), 1024)
        .await
        .unwrap();
    catalog.inject(Step::AppendSync, 0, Fault::Io(io::ErrorKind::Other));
    assert!(matches!(writer.flush().await, Err(AppendError::Object(_))));
    assert_eq!(writer.persisted_size(), 3);
    assert_eq!(
        writer.flush().await,
        Ok(3),
        "the failed bytes stay unacknowledged"
    );
    assert_eq!(
        writer.append(6, Bytes::from_static(b"more"), 1024).await,
        Err(AppendError::OffsetGap { expected: 3 })
    );
    writer
        .append(3, Bytes::from_static(b"two"), 1024)
        .await
        .unwrap();
    assert_eq!(writer.flush().await, Ok(6));
    assert_eq!(persisted_content(&catalog, "sync").await, b"onetwo");
    let record = writer
        .finalize(
            Some(crc32c::crc32c(b"onetwo")),
            BucketAccess::Permissive,
            1024,
        )
        .await
        .unwrap();
    assert_eq!(record.size, 6);
}

/// A crash before the size update commits leaves only the previously flushed prefix; the next
/// writer truncates the unacknowledged bytes and continues from there.
#[tokio::test]
async fn append_crash_at_each_durable_step_recovers_the_flushed_prefix() {
    use super::super::fault::{Fault, Step};
    let crashes = TestDir::new();
    for (index, step) in [Step::AppendWrite, Step::AppendSync, Step::AppendCommit]
        .into_iter()
        .enumerate()
    {
        let dir = TestDir::new();
        let catalog = catalog_with_bucket(&dir).await;
        let mut writer = catalog
            .create_appendable("append", "crash", options())
            .await
            .unwrap();
        writer
            .append(0, Bytes::from_static(b"one"), 1024)
            .await
            .unwrap();
        writer.flush().await.unwrap();
        let generation = writer.record().generation;
        let snapshot = crashes.0.join(index.to_string());
        catalog.inject(step, 0, Fault::Snapshot(snapshot.clone()));
        writer
            .append(3, Bytes::from_static(b"two"), 1024)
            .await
            .unwrap();
        assert_eq!(writer.flush().await, Ok(6), "{step:?}");
        assert_eq!(catalog.pending_faults(), 0, "{step:?}");
        drop(writer);
        catalog.close().await.unwrap();

        let recovered = Catalog::open(&snapshot).await.unwrap();
        assert_eq!(open(&recovered, "crash").await.unwrap().size, 3, "{step:?}");
        assert_eq!(
            persisted_content(&recovered, "crash").await,
            b"one",
            "{step:?}"
        );
        let mut writer = take_over(&recovered, "crash", generation).await;
        assert_eq!(writer.persisted_size(), 3, "{step:?}");
        writer
            .append(3, Bytes::from_static(b"TWO"), 1024)
            .await
            .unwrap();
        assert_eq!(writer.flush().await, Ok(6), "{step:?}");
        assert_eq!(
            persisted_content(&recovered, "crash").await,
            b"oneTWO",
            "{step:?}"
        );
        recovered.close().await.unwrap();
    }
}

/// Finalization publishes through the ordinary write path: a failed publication keeps the
/// generation appendable, a crash before its commit recovers it as appendable, and a crash
/// after the commit recovers it as finalized.
#[tokio::test]
async fn finalize_failure_and_crashes_recover_appendable_or_finalized() {
    use super::super::fault::{Fault, Step};
    let crashes = TestDir::new();
    let dir = TestDir::new();
    let catalog = catalog_with_bucket(&dir).await;
    let mut writer = catalog
        .create_appendable("append", "final", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"data"), 1024)
        .await
        .unwrap();
    let generation = writer.record().generation;
    catalog.inject(Step::BodySync, 0, Fault::Io(io::ErrorKind::Other));
    assert!(
        writer
            .finalize(None, BucketAccess::Permissive, 1024)
            .await
            .is_err()
    );
    assert_eq!(catalog.pending_faults(), 0);
    assert_eq!(open(&catalog, "final").await.unwrap().size, 4);
    drop(writer);

    let before = crashes.0.join("before");
    let after = crashes.0.join("after");
    catalog.inject(Step::BeforeCommit, 0, Fault::Snapshot(before.clone()));
    catalog.inject(Step::AfterCommit, 0, Fault::Snapshot(after.clone()));
    let mut writer = take_over(&catalog, "final", generation).await;
    let record = writer
        .finalize(
            Some(crc32c::crc32c(b"data")),
            BucketAccess::Permissive,
            1024,
        )
        .await
        .unwrap();
    assert_eq!((record.generation, record.size), (generation, 4));
    assert_eq!(catalog.pending_faults(), 0);
    drop(writer);
    catalog.close().await.unwrap();

    let recovered = Catalog::open(&before).await.unwrap();
    assert_eq!(open(&recovered, "final").await.unwrap().size, 4);
    assert_eq!(persisted_content(&recovered, "final").await, b"data");
    recovered.close().await.unwrap();

    let recovered = Catalog::open(&after).await.unwrap();
    assert_eq!(
        open(&recovered, "final").await.err(),
        Some(ObjectError::ObjectNotFound)
    );
    assert_eq!(
        recovered
            .take_over_appendable(
                "append",
                "final",
                generation,
                None,
                Default::default(),
                BucketAccess::Permissive,
            )
            .await
            .err(),
        Some(AppendError::Finalized)
    );
    recovered.close().await.unwrap();
}
