#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::{clock::SystemClock, test_support::TestDir};
use std::{
    fs,
    time::{Duration, UNIX_EPOCH},
};

fn binding(count: i64) -> ImportBinding {
    ImportBinding {
        source: [1; 32],
        fingerprint: [2; 32],
        expected_elements: count,
    }
}
fn element(ordinal: i64) -> ImportElement {
    ImportElement {
        ordinal,
        fingerprint: [ordinal as u8; 32],
        inode: Some(u64::MAX),
        ctime_ns: Some(i128::MIN),
    }
}
async fn setup(count: i64) -> (TestDir, Catalog, Admin, ImportBinding) {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let admin = catalog.admin(Origin::Import);
    let source = binding(count);
    catalog
        .import_journal()
        .begin(source.clone())
        .await
        .unwrap();
    (dir, catalog, admin, source)
}
fn definition() -> BucketDefinition {
    BucketDefinition::default_for(
        &BucketName::new("test-bucket").unwrap(),
        "original-project",
        &SystemClock,
    )
    .unwrap()
}
async fn bucket(admin: &Admin, source: &ImportBinding) {
    admin
        .publish_import_bucket(source.clone(), 0, element(1), definition())
        .await
        .unwrap();
}
async fn entry(
    dir: &TestDir,
    admin: &Admin,
    ordinal: i64,
    name: &str,
    bytes: &[u8],
) -> ImportedPublication {
    let path = dir.0.join("source");
    fs::write(&path, bytes).unwrap();
    let staged = admin
        .stage_from_file(fs::File::open(path).unwrap(), Default::default(), 1_000_000)
        .await
        .unwrap();
    let hashes = staged.hashes();
    ImportedPublication {
        publication: BatchPublication {
            bucket: BucketName::new("test-bucket").unwrap(),
            name: ObjectName::new(name).unwrap(),
            request: Publication {
                content_type: "text/plain".into(),
                metadata: Default::default(),
                expected: crate::object::ExpectedHashes {
                    md5: Some(hashes.md5),
                    crc32c: Some(hashes.crc32c),
                },
                condition: Some(GenerationCondition::Absent),
                max_bytes: 1_000_000,
                mode: PublishMode::Replace,
            },
            staged,
        },
        identity: ImportedIdentity::new(
            Generation::new(900 + ordinal).unwrap(),
            UNIX_EPOCH + Duration::from_micros(123_456),
            UNIX_EPOCH + Duration::from_micros(234_567),
        )
        .unwrap(),
        element: element(ordinal),
    }
}

#[tokio::test]
async fn import_publication_preserves_identity_settings_hold_and_atomic_prefix() {
    let (dir, catalog, admin, source) = setup(2).await;
    let mut record = definition().0;
    record.time_created = UNIX_EPOCH + Duration::from_micros(123_456);
    record.updated = UNIX_EPOCH + Duration::from_micros(234_567);
    record.default_event_based_hold = true;
    record.labels.insert("source".into(), "kept".into());
    admin
        .publish_import_bucket(
            source.clone(),
            0,
            element(1),
            BucketDefinition::new(record.clone()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        admin
            .get_bucket(&BucketName::new("test-bucket").unwrap())
            .await
            .unwrap(),
        Some(record)
    );
    let item = entry(&dir, &admin, 2, "object", b"imported").await;
    let result = admin
        .publish_import_objects(source.clone(), 1, vec![item])
        .await
        .unwrap();
    let object = result.results[0].as_ref().unwrap();
    assert_eq!(object.generation, 902);
    assert_eq!(
        object.time_created,
        UNIX_EPOCH + Duration::from_micros(123_456)
    );
    assert_eq!(object.updated, UNIX_EPOCH + Duration::from_micros(234_567));
    assert!(!object.event_based_hold);
    assert_eq!(
        catalog
            .import_journal()
            .progress()
            .await
            .unwrap()
            .unwrap()
            .cursor,
        2
    );
    assert_eq!(
        catalog
            .import_journal()
            .page(source.clone(), 0, 256)
            .await
            .unwrap(),
        vec![element(1), element(2)]
    );
    catalog.import_journal().finish(source).await.unwrap();
    let normal = super::super::super::admin::tests::request(None);
    let normal = admin
        .publish_object(
            &BucketName::new("test-bucket").unwrap(),
            &ObjectName::new("normal").unwrap(),
            normal,
            super::super::super::admin::tests::body(b"normal"),
        )
        .await
        .unwrap();
    assert!(normal.generation > 902 && normal.event_based_hold);
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert!(!reopened.import_journal().in_progress().await.unwrap());
    assert!(reopened.admin(Origin::Seed).seed_complete().await.unwrap());
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn import_publication_keeps_independent_successes_and_terminal_failure_after_restart() {
    let (dir, catalog, admin, source) = setup(4).await;
    bucket(&admin, &source).await;
    let a = entry(&dir, &admin, 2, "a", b"a").await;
    let mut b = entry(&dir, &admin, 3, "b", b"b").await;
    b.publication.request.expected.md5 = Some([0; 16]);
    let c = entry(&dir, &admin, 4, "c", b"c").await;
    let result = admin
        .publish_import_objects(source.clone(), 1, vec![a, b, c])
        .await
        .unwrap();
    assert!(result.results[0].is_ok());
    assert_eq!(result.results[1], Err(AdminError::Checksum));
    assert!(result.results[2].is_ok());
    let progress = catalog.import_journal().progress().await.unwrap().unwrap();
    assert_eq!(progress.cursor, 2);
    assert_eq!(progress.failure.as_deref(), Some("element_rejected"));
    assert_eq!(
        catalog.import_journal().begin(source.clone()).await,
        Err(AdminError::Precondition)
    );
    assert_eq!(
        catalog.import_journal().finish(source.clone()).await,
        Err(AdminError::Precondition)
    );
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        reopened.import_journal().progress().await.unwrap(),
        Some(progress)
    );
    assert!(reopened.import_journal().in_progress().await.unwrap());
    assert_eq!(
        reopened.import_journal().begin(source).await,
        Err(AdminError::Precondition)
    );
    for name in ["a", "c"] {
        assert!(
            reopened
                .admin(Origin::Import)
                .get_object(
                    &BucketName::new("test-bucket").unwrap(),
                    &ObjectName::new(name).unwrap(),
                    None
                )
                .await
                .is_ok()
        );
    }
    let blobs: usize = fs::read_dir(dir.0.join("blobs"))
        .unwrap()
        .map(|shard| fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum();
    assert_eq!(blobs, 2);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn import_publication_disk_full_rolls_back_records_observations_and_cursor() {
    let (dir, catalog, admin, source) = setup(3).await;
    bucket(&admin, &source).await;
    let a = entry(&dir, &admin, 2, "a", b"a").await;
    let mut b = entry(&dir, &admin, 3, "b", b"b").await;
    b.publication.request.metadata.metadata = Some(Some(std::collections::BTreeMap::from([(
        "large".into(),
        Some("x".repeat(7000)),
    )])));
    catalog
        .run_upload(|connection| {
            let pages: i64 = connection
                .pragma_query_value(None, "page_count", |r| r.get(0))
                .map_err(objects::object_unavailable)?;
            connection
                .pragma_update(None, "max_page_count", pages)
                .map_err(objects::object_unavailable)
        })
        .await
        .unwrap();
    assert_eq!(
        admin
            .publish_import_objects(source.clone(), 1, vec![a, b])
            .await
            .err(),
        Some(AdminError::DiskFull)
    );
    assert_eq!(
        catalog
            .import_journal()
            .progress()
            .await
            .unwrap()
            .unwrap()
            .cursor,
        1
    );
    assert_eq!(
        catalog.import_journal().page(source, 0, 256).await.unwrap(),
        vec![element(1)]
    );
    assert_eq!(
        admin
            .get_object(
                &BucketName::new("test-bucket").unwrap(),
                &ObjectName::new("a").unwrap(),
                None
            )
            .await,
        Err(AdminError::NotFound)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn import_publication_progress_rejects_foreign_project_records_and_failure_is_bounded() {
    let (dir, catalog, admin, source) = setup(1).await;
    let extra = BucketDefinition::default_for(
        &BucketName::new("extra-bucket").unwrap(),
        "other-project",
        &SystemClock,
    )
    .unwrap();
    admin.ensure_bucket(extra).await.unwrap();
    assert_eq!(
        catalog.import_journal().progress().await,
        Err(AdminError::Precondition)
    );
    assert_eq!(
        catalog.import_journal().begin(source.clone()).await,
        Err(AdminError::Precondition)
    );
    catalog
        .import_journal()
        .fail(source.clone(), "失敗".repeat(1000))
        .await
        .unwrap();
    let state = catalog.import_journal().progress().await.unwrap().unwrap();
    assert_eq!(state.failure.unwrap().chars().count(), 256);
    assert_eq!(
        catalog.import_journal().finish(source.clone()).await,
        Err(AdminError::Precondition)
    );
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        reopened.import_journal().begin(source).await,
        Err(AdminError::Precondition)
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn import_publication_cancelled_caller_drains_commit_and_busy_keeps_prefix() {
    let (dir, catalog, admin, source) = setup(2).await;
    bucket(&admin, &source).await;
    let item = entry(&dir, &admin, 2, "object", b"retained").await;
    let slots = catalog
        .0
        .publication_requests
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    assert!(matches!(
        admin
            .publish_import_objects(source.clone(), 1, vec![item])
            .await,
        Err(AdminError::Busy { .. })
    ));
    drop(slots);
    assert_eq!(
        catalog
            .import_journal()
            .progress()
            .await
            .unwrap()
            .unwrap()
            .cursor,
        1
    );
    let item = entry(&dir, &admin, 2, "object", b"retained").await;
    let execution = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let worker = admin.clone();
    let source_copy = source.clone();
    let publishing = tokio::spawn(async move {
        worker
            .publish_import_objects(source_copy, 1, vec![item])
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if fs::read_dir(dir.0.join("blobs")).unwrap().any(|shard| {
                fs::read_dir(shard.unwrap().path())
                    .unwrap()
                    .next()
                    .is_some()
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    publishing.abort();
    assert!(matches!(publishing.await, Err(error) if error.is_cancelled()));
    let worker = catalog.clone();
    let close = tokio::spawn(async move { worker.close().await });
    tokio::task::yield_now().await;
    assert!(!close.is_finished());
    assert!(Catalog::open(&dir.0).await.is_err());
    drop(execution);
    close.await.unwrap().unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        reopened
            .import_journal()
            .progress()
            .await
            .unwrap()
            .unwrap()
            .cursor,
        2
    );
    let (_, mut reader) = reopened
        .admin(Origin::Import)
        .open_object(
            &BucketName::new("test-bucket").unwrap(),
            &ObjectName::new("object").unwrap(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"retained"[..]);
    assert!(reader.next_chunk().await.unwrap().is_none());
    assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn import_publication_failure_schema_rolls_back_and_reopens_preserved_state() {
    let (dir, catalog, admin, source) = setup(2).await;
    bucket(&admin, &source).await;
    let id = catalog.sync_state().catalog_id().await.unwrap();
    catalog.close().await.unwrap();
    let mut connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection
        .execute_batch("DROP TABLE sync_findings; DROP TABLE sync_writeback_ops; DROP TABLE sync_writeback_roots; DROP TABLE sync_roots; ALTER TABLE import_journal DROP COLUMN failure; PRAGMA user_version = 17;")
        .unwrap();
    {
        let transaction = connection.transaction().unwrap();
        transaction
            .execute_batch(import_journal::FAILURE_SCHEMA)
            .unwrap();
        transaction.rollback().unwrap();
    }
    super::super::super::validate_schema(&connection, 17).unwrap();
    drop(connection);
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(reopened.sync_state().catalog_id().await.unwrap(), id);
    let progress = reopened.import_journal().progress().await.unwrap().unwrap();
    assert_eq!(progress.binding, source);
    assert_eq!(progress.cursor, 1);
    assert!(progress.failure.is_none());
    reopened
        .import_journal()
        .fail(source, "source_changed")
        .await
        .unwrap();
    reopened.close().await.unwrap();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    super::super::super::validate_schema(&connection, 21).unwrap();
    assert!(super::super::super::validate_schema_version(18, 17).is_err());
}
