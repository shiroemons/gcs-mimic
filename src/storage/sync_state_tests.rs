#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::{
    clock::SystemClock,
    storage::admin::{
        Admin, BatchPublication, BucketDefinition, GenerationCondition, Origin, Publication,
        PublishMode,
    },
    test_support::TestDir,
};
use std::fs;

async fn setup() -> (TestDir, Catalog, Admin, BucketName) {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let admin = catalog.admin(Origin::SyncIngest);
    admin
        .ensure_bucket(
            BucketDefinition::default_for(
                &BucketName::new("test-bucket").unwrap(),
                "project",
                &SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    (dir, catalog, admin, BucketName::new("test-bucket").unwrap())
}

async fn entry(dir: &TestDir, admin: &Admin, name: &str, bytes: &[u8]) -> BatchPublication {
    let source = dir.0.join("source");
    fs::write(&source, bytes).unwrap();
    BatchPublication {
        bucket: BucketName::new("test-bucket").unwrap(),
        name: ObjectName::new(name).unwrap(),
        request: Publication {
            content_type: "text/plain".into(),
            metadata: Default::default(),
            expected: Default::default(),
            condition: None,
            max_bytes: 1024,
            mode: PublishMode::Replace,
        },
        staged: admin
            .stage_from_file(fs::File::open(source).unwrap(), Default::default(), 1024)
            .await
            .unwrap(),
    }
}

fn update(binding: SyncBinding, path: &str, entry: &BatchPublication) -> SyncUpdate {
    SyncUpdate::new(
        binding,
        path.into(),
        SyncFingerprint {
            size: entry.staged.size(),
            mtime_ns: i128::MIN,
            ctime_ns: i128::MAX,
            inode: u64::MAX,
            md5: entry.staged.hashes().md5,
            crc32c: entry.staged.hashes().crc32c,
            racy: true,
        },
    )
    .unwrap()
}

async fn publish(admin: &Admin, entry: BatchPublication) -> Result<ObjectRecord, AdminError> {
    admin.publish_objects(vec![entry]).await?.results.remove(0)
}

#[tokio::test]
async fn sync_state_identity_index_pages_and_exact_fingerprints_survive_reopen() {
    let (dir, catalog, admin, bucket) = setup().await;
    let state = catalog.sync_state();
    let id = state.catalog_id().await.unwrap();
    let binding = state.binding(&bucket).await.unwrap();
    for path in ["test-bucket/z", "test-bucket/a", "test-bucket/m"] {
        let mut item = entry(&dir, &admin, path, b"bytes").await;
        let change = update(binding.clone(), path, &item);
        item.staged = item.staged.with_sync_state(change.clone());
        let record = publish(&admin, item).await.unwrap();
        let stored = state
            .get_path(binding.clone(), path.into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.fingerprint, change.fingerprint);
        assert_eq!(stored.generation.get(), record.generation);
    }
    let page = state.page(binding.clone(), String::new(), 2).await.unwrap();
    assert_eq!(
        page.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        ["test-bucket/a", "test-bucket/m"]
    );
    assert_eq!(
        state
            .page(binding.clone(), page[1].path.clone(), 2)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(state.page(binding.clone(), String::new(), 0).await.is_err());
    assert!(
        state
            .page(binding.clone(), String::new(), 257)
            .await
            .is_err()
    );
    for path in ["", "/a", "a//b", "a/../b", "a/./b", "a\\b", "C:/a", "a\0b"] {
        assert!(
            SyncUpdate::new(binding.clone(), path.into(), page[0].fingerprint.clone()).is_err()
        );
    }
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(reopened.sync_state().catalog_id().await.unwrap(), id);
    assert_eq!(
        reopened
            .sync_state()
            .page(binding.clone(), String::new(), 256)
            .await
            .unwrap()
            .len(),
        3
    );
    let (_, other, _, _) = setup().await;
    assert_ne!(other.sync_state().catalog_id().await.unwrap(), id);
    assert_eq!(
        other.sync_state().page(binding, String::new(), 1).await,
        Err(AdminError::Precondition)
    );
    other.close().await.unwrap();
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn sync_state_publication_skip_rollback_and_conditional_delete_are_atomic() {
    let (dir, catalog, admin, bucket) = setup().await;
    let state = catalog.sync_state();
    let binding = state.binding(&bucket).await.unwrap();
    let name = ObjectName::new("object").unwrap();
    let path = "test-bucket/object";
    let mut item = entry(&dir, &admin, "object", b"old").await;
    let change = update(binding.clone(), path, &item);
    item.staged = item.staged.with_sync_state(change.clone());
    let first = publish(&admin, item).await.unwrap();
    let mut item = entry(&dir, &admin, "object", b"old").await;
    item.request.mode = PublishMode::SkipIfSameHash;
    let mut skipped = update(binding.clone(), path, &item);
    skipped.fingerprint.racy = false;
    item.staged = item.staged.with_sync_state(skipped.clone());
    assert_eq!(
        publish(&admin, item).await.unwrap().generation,
        first.generation
    );
    assert!(
        !state
            .get_path(binding.clone(), path.into())
            .await
            .unwrap()
            .unwrap()
            .fingerprint
            .racy
    );

    catalog.run_admin(|connection| {
        connection.execute_batch("CREATE TEMP TRIGGER reject_sync BEFORE UPDATE ON sync_objects BEGIN SELECT RAISE(ABORT, 'injected sync failure'); END;").map_err(unavailable)
    }).await.unwrap();
    let mut item = entry(&dir, &admin, "object", b"new").await;
    let failed_change = update(binding.clone(), path, &item);
    item.staged = item.staged.with_sync_state(failed_change);
    assert!(publish(&admin, item).await.is_err());
    assert_eq!(
        admin
            .get_object(&bucket, &name, None)
            .await
            .unwrap()
            .generation,
        first.generation
    );
    catalog
        .run_admin(|connection| {
            connection
                .execute_batch("DROP TRIGGER reject_sync")
                .map_err(unavailable)
        })
        .await
        .unwrap();

    let next = publish(&admin, entry(&dir, &admin, "object", b"api").await)
        .await
        .unwrap();
    assert_eq!(
        state
            .get_path(binding.clone(), path.into())
            .await
            .unwrap()
            .unwrap()
            .generation
            .get(),
        first.generation
    );
    assert_eq!(
        state
            .acknowledge(&name, Generation::new(first.generation).unwrap(), skipped)
            .await,
        Err(AdminError::Precondition)
    );
    assert_eq!(
        admin
            .delete_synced_object(
                &bucket,
                &name,
                GenerationCondition::Is(Generation::new(first.generation).unwrap()),
                binding.clone()
            )
            .await,
        Err(AdminError::Precondition)
    );
    let mut current = change;
    let mut hasher = crate::hashing::Hasher::default();
    hasher.update(b"api");
    let hashes = hasher.finish();
    current.fingerprint.md5 = hashes.md5;
    current.fingerprint.crc32c = hashes.crc32c;
    state
        .acknowledge(&name, Generation::new(next.generation).unwrap(), current)
        .await
        .unwrap();
    catalog.run_admin(|connection| connection.execute_batch("CREATE TEMP TRIGGER reject_sync_delete BEFORE DELETE ON sync_objects BEGIN SELECT RAISE(ABORT, 'injected sync delete failure'); END;").map_err(unavailable)).await.unwrap();
    assert!(
        admin
            .delete_synced_object(
                &bucket,
                &name,
                GenerationCondition::Is(Generation::new(next.generation).unwrap()),
                binding.clone()
            )
            .await
            .is_err()
    );
    assert!(admin.get_object(&bucket, &name, None).await.is_ok());
    catalog
        .run_admin(|connection| {
            connection
                .execute_batch("DROP TRIGGER reject_sync_delete")
                .map_err(unavailable)
        })
        .await
        .unwrap();
    admin
        .delete_synced_object(
            &bucket,
            &name,
            GenerationCondition::Is(Generation::new(next.generation).unwrap()),
            binding.clone(),
        )
        .await
        .unwrap();
    assert!(
        state
            .get_path(binding.clone(), path.into())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        admin.get_object(&bucket, &name, None).await,
        Err(AdminError::NotFound)
    );
    catalog
        .delete_bucket("test-bucket", None, None)
        .await
        .unwrap();
    admin
        .ensure_bucket(
            BucketDefinition::default_for(
                &BucketName::new("test-bucket").unwrap(),
                "project",
                &SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        state.page(binding, String::new(), 1).await,
        Err(AdminError::Precondition)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_state_rejects_hash_path_identity_and_hold_conflicts_without_publication() {
    let (dir, catalog, admin, bucket) = setup().await;
    let state = catalog.sync_state();
    let binding = state.binding(&bucket).await.unwrap();
    let mut item = entry(&dir, &admin, "first", b"first").await;
    let change = update(binding.clone(), "test-bucket/shared", &item);
    item.staged = item.staged.with_sync_state(change.clone());
    publish(&admin, item).await.unwrap();

    let mut item = entry(&dir, &admin, "duplicate", b"other").await;
    let duplicate = update(binding.clone(), "test-bucket/shared", &item);
    item.staged = item.staged.with_sync_state(duplicate);
    assert!(publish(&admin, item).await.is_err());
    assert_eq!(
        admin
            .get_object(&bucket, &ObjectName::new("duplicate").unwrap(), None)
            .await,
        Err(AdminError::NotFound)
    );

    let mut item = entry(&dir, &admin, "hash", b"hash").await;
    let mut bad = update(binding.clone(), "test-bucket/hash", &item);
    bad.fingerprint.md5 = [0; 16];
    item.staged = item.staged.with_sync_state(bad);
    assert_eq!(publish(&admin, item).await, Err(AdminError::Checksum));

    let (other_dir, other, other_admin, other_bucket) = setup().await;
    let mut item = entry(&other_dir, &other_admin, "foreign", b"first").await;
    item.staged = item.staged.with_sync_state(change);
    assert_eq!(
        publish(&other_admin, item).await,
        Err(AdminError::Precondition)
    );
    assert!(
        other
            .sync_state()
            .page(
                other.sync_state().binding(&other_bucket).await.unwrap(),
                String::new(),
                1
            )
            .await
            .unwrap()
            .is_empty()
    );
    other.close().await.unwrap();

    let mut item = entry(&dir, &admin, "held", b"held").await;
    item.request.metadata.event_based_hold = Some(true);
    let change = update(binding.clone(), "test-bucket/held", &item);
    item.staged = item.staged.with_sync_state(change);
    let record = publish(&admin, item).await.unwrap();
    assert_eq!(
        admin
            .delete_synced_object(
                &bucket,
                &ObjectName::new("held").unwrap(),
                GenerationCondition::Is(Generation::new(record.generation).unwrap()),
                binding.clone()
            )
            .await,
        Err(AdminError::Hold)
    );
    assert!(
        state
            .get_path(binding, "test-bucket/held".into())
            .await
            .unwrap()
            .is_some()
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_state_batch_path_conflict_isolates_the_failed_element_and_cleans_its_blob() {
    let (dir, catalog, admin, bucket) = setup().await;
    let state = catalog.sync_state();
    let binding = state.binding(&bucket).await.unwrap();
    let mut entries = Vec::new();
    for (name, path) in [
        ("first", "test-bucket/shared"),
        ("duplicate", "test-bucket/shared"),
        ("last", "test-bucket/last"),
    ] {
        let mut item = entry(&dir, &admin, name, name.as_bytes()).await;
        let change = update(binding.clone(), path, &item);
        item.staged = item.staged.with_sync_state(change);
        entries.push(item);
    }
    let batch = admin.publish_objects(entries).await.unwrap();
    assert!(batch.results[0].is_ok());
    assert_eq!(batch.results[1], Err(AdminError::Precondition));
    assert!(batch.results[2].is_ok());
    assert!(batch.unattempted.is_empty());
    assert_eq!(
        state
            .page(binding.clone(), String::new(), 256)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        state
            .get_path(binding.clone(), "test-bucket/shared".into())
            .await
            .unwrap()
            .unwrap()
            .name,
        "first"
    );
    assert_eq!(
        admin
            .get_object(&bucket, &ObjectName::new("duplicate").unwrap(), None)
            .await,
        Err(AdminError::NotFound)
    );
    catalog.close().await.unwrap();
    assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    let blobs: usize = fs::read_dir(dir.0.join("blobs"))
        .unwrap()
        .map(|shard| fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum();
    assert_eq!(blobs, 2);
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        reopened
            .sync_state()
            .page(binding, String::new(), 256)
            .await
            .unwrap()
            .len(),
        2
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn sync_state_schema_migration_rolls_back_retries_and_preserves_objects() {
    let (dir, catalog, admin, bucket) = setup().await;
    let original = publish(&admin, entry(&dir, &admin, "object", b"old").await)
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let mut connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection
        .execute_batch(
            "DROP TABLE sync_objects; DROP TABLE catalog_identity; PRAGMA user_version = 16;",
        )
        .unwrap();
    connection
        .execute_batch("CREATE TABLE sync_objects(collision);")
        .unwrap();
    assert!(migrate(&mut connection).is_err());
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'catalog_identity'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        16
    );
    connection
        .execute_batch("DROP TABLE sync_objects;")
        .unwrap();
    let pages: i64 = connection
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    connection
        .pragma_update(None, "max_page_count", pages)
        .unwrap();
    // Exhaust reusable pages to exercise SQLite's migration failure path.
    connection
        .execute_batch("CREATE TABLE filler (value BLOB);")
        .unwrap();
    while connection
        .execute("INSERT INTO filler VALUES (zeroblob(4096))", [])
        .is_ok()
    {}
    assert!(migrate(&mut connection).is_err());
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        16
    );
    assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('sync_objects','catalog_identity')", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    connection
        .pragma_update(None, "max_page_count", 1_000_000)
        .unwrap();
    connection.execute_batch("DROP TABLE filler").unwrap();
    drop(connection);
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        reopened
            .admin(Origin::SyncIngest)
            .get_object(&bucket, &ObjectName::new("object").unwrap(), None)
            .await
            .unwrap(),
        original
    );
    reopened.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        17
    );
    assert!(super::super::validate_schema_version(17, 16).is_err());
    connection
        .execute("DELETE FROM catalog_identity", [])
        .unwrap();
    drop(connection);
    assert!(Catalog::open(&dir.0).await.is_err());
}
