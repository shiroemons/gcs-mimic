use super::*;
use crate::storage::catalog::{BucketAccess, Catalog};
use crate::storage::test_fixtures::{PERMISSIVE, body, live, publication};
use crate::{
    clock::SystemClock,
    storage::admin::{BucketDefinition, BucketName, ObjectName, Origin},
    test_support::TestDir,
};
use bytes::Bytes;

fn bucket(name: &str) -> BucketDefinition {
    BucketDefinition::default_for(&BucketName::new(name).unwrap(), "project", &SystemClock).unwrap()
}

async fn publish(catalog: &Catalog, origin: Origin, name: &str) {
    catalog
        .admin(origin)
        .publish_object(
            &BucketName::new("test-bucket").unwrap(),
            &ObjectName::new(name).unwrap(),
            publication(100),
            body(vec![Bytes::from_static(b"body")]),
        )
        .await
        .unwrap();
}

#[test]
fn journal_migration_is_atomic_and_inventory_cannot_hide_extra_tables() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA user_version = 14; CREATE TABLE change_journal(sequence);")
        .unwrap();
    assert!(
        crate::storage::catalog::Migration::Statements(SCHEMA)
            .apply(&mut connection)
            .is_err()
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        14
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'change_journal_state'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    connection
        .execute_batch("DROP TABLE change_journal;")
        .unwrap();
    crate::storage::catalog::Migration::Statements(SCHEMA)
        .apply(&mut connection)
        .unwrap();
    validate(&connection).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        15
    );
    assert!(super::super::validate_schema_version(15, 14).is_err());
    // A current catalog with one table more than its version names fails validation.
    let mut current = Connection::open_in_memory().unwrap();
    super::super::initialize_catalog(&mut current).unwrap();
    super::super::upgrade(&mut current, 5, None).unwrap();
    let version = super::super::CATALOG_SCHEMA_VERSION;
    super::super::validate_schema(&current, version).unwrap();
    current
        .execute_batch("CREATE TABLE zz_extra(value);")
        .unwrap();
    assert!(super::super::validate_schema(&current, version).is_err());
}

#[tokio::test]
async fn journal_schema_upgrade_preserves_objects_policies_and_active_sessions() {
    use super::super::ResumableOptions;
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .admin(Origin::Seed)
        .ensure_bucket(bucket("test-bucket"))
        .await
        .unwrap();
    publish(&catalog, Origin::Seed, "preserved").await;
    let target = live("test-bucket", "preserved");
    let mut before = catalog
        .get_object(target.clone(), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    before.owner = None;
    let policy = catalog.get_policy("test-bucket", PERMISSIVE).await.unwrap();
    let session = catalog
        .create_resumable_session(
            "test-bucket",
            "pending",
            ResumableOptions::new(BucketAccess::Permissive),
            Some(4),
        )
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let path = dir.0.join("catalog.sqlite3");
    let connection = Connection::open(&path).unwrap();
    crate::storage::catalog::test_support::downgrade_soft_delete_schema(&connection);
    connection
        .execute_batch(
            "ALTER TABLE buckets DROP COLUMN retention_policy; ALTER TABLE buckets DROP COLUMN lifecycle; ALTER TABLE buckets DROP COLUMN uniform_bucket_level_access_locked_time; ALTER TABLE objects DROP COLUMN temporary_hold; ALTER TABLE objects DROP COLUMN custom_time; DROP TABLE sync_findings; DROP TABLE sync_writeback_ops; DROP TABLE sync_writeback_roots; DROP TABLE sync_roots; DROP TABLE sync_objects; DROP TABLE catalog_identity; DROP TABLE import_observations; DROP TABLE import_journal; DROP TABLE change_journal; DROP TABLE change_journal_state; PRAGMA user_version = 14;",
        )
        .unwrap();
    super::super::validate_schema(&connection, 14).unwrap();
    drop(connection);
    let backup = TestDir::new();
    std::fs::copy(&path, backup.0.join("catalog.sqlite3")).unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog
            .get_object(target, Default::default(), PERMISSIVE)
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        catalog.get_policy("test-bucket", PERMISSIVE).await.unwrap(),
        policy
    );
    let lease = catalog
        .lock_resumable_session(&session.id.to_string(), "test-bucket")
        .await
        .unwrap();
    drop(lease);
    catalog.close().await.unwrap();
    let connection = Connection::open(&path).unwrap();
    super::super::validate_schema(&connection, super::super::CATALOG_SCHEMA_VERSION).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        super::super::CATALOG_SCHEMA_VERSION
    );
    assert!(super::super::validate_schema(&connection, 14).is_err());
    let snapshot = Connection::open(backup.0.join("catalog.sqlite3")).unwrap();
    super::super::validate_schema(&snapshot, 14).unwrap();
}
