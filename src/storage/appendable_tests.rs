use super::*;
use crate::test_support::TestDir;

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
    super::super::validate_schema(&connection, 34).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        34
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
    writer.mark_finalizing(true).await.unwrap();
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
