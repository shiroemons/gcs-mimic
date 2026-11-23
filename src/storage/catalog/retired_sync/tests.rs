use super::*;
use crate::{
    clock::SystemClock,
    storage::{
        Catalog,
        admin::LocalAdmin,
        catalog::{
            CATALOG_SCHEMA_VERSION, STEPS, initialize_catalog, schema::Migration, test_support,
            validate_schema, validate_schema_version,
        },
        test_fixtures::{PERMISSIVE, body, bucket, live, permissive_upload},
    },
    test_support::TestDir,
};
use bytes::Bytes;
use rusqlite::params;
use std::sync::Arc;

const ROOT: [u8; 16] = [1; 16];

fn user_version(connection: &Connection) -> i64 {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

/// A new in-memory catalog upgraded through every step up to `version`.
fn catalog_at(version: i64) -> Connection {
    let mut connection = Connection::open_in_memory().unwrap();
    initialize_catalog(&mut connection).unwrap();
    for step in STEPS.iter().filter(|step| step.version <= version) {
        if user_version(&connection) < step.version
            && let Some(migration) = &step.migration
        {
            migration.apply(&mut connection).unwrap();
        }
    }
    assert_eq!(user_version(&connection), version);
    connection
}

fn count(connection: &Connection, query: &str) -> i64 {
    connection.query_row(query, [], |row| row.get(0)).unwrap()
}

/// Every retired table and the rows it holds.
fn retired_rows(connection: &Connection) -> Vec<(&'static str, i64)> {
    TABLES
        .iter()
        .map(|table| {
            (
                *table,
                count(connection, &format!("SELECT count(*) FROM {table}")),
            )
        })
        .collect()
}

/// A root that tracks `bucket/object` and holds one finding for each incarnation given.
fn record_root(connection: &Connection, bucket: &str, incarnations: &[&str]) {
    connection
        .execute(
            "INSERT INTO sync_roots VALUES(?1,?2,?3,?4,1)",
            params![ROOT, b"root", [0u8; 8], [1u8; 8]],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sync_buckets VALUES(?1,?2,?3)",
            params![ROOT, bucket, incarnations[0]],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sync_objects VALUES(?1,'object',?2,'bucket/object',1,4,?3,?3,?4,?5,0,0)",
            params![bucket, incarnations[0], [0u8; 16], [0u8; 8], [0u8; 16]],
        )
        .unwrap();
    for (index, incarnation) in incarnations.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO sync_findings(root_id,bucket,name,incarnation,generation,reason) VALUES(?1,?2,?3,?4,1,'invalid-name')",
                params![ROOT, bucket, format!("finding//{index}"), incarnation],
            )
            .unwrap();
    }
}

#[tokio::test]
async fn the_identity_step_commits_whole_and_a_catalog_keeps_exactly_one_identity() {
    let mut connection = catalog_at(16);
    let identity = STEPS.iter().find(|step| step.version == 17).unwrap();
    let migration = identity.migration.as_ref().unwrap();
    connection
        .execute_batch("CREATE TABLE sync_objects(collision);")
        .unwrap();
    assert!(migration.apply(&mut connection).is_err());
    assert_eq!(user_version(&connection), 16);
    assert_eq!(
        count(
            &connection,
            "SELECT count(*) FROM sqlite_schema WHERE name = 'catalog_identity'"
        ),
        0
    );
    connection
        .execute_batch("DROP TABLE sync_objects;")
        .unwrap();
    migration.apply(&mut connection).unwrap();
    validate_schema(&connection, 17).unwrap();
    assert!(validate_schema_version(17, 16).is_err());
    connection
        .execute("DELETE FROM catalog_identity", [])
        .unwrap();
    assert!(validate_identity(&connection).is_err());

    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let id = catalog.identity().await.unwrap();
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(reopened.identity().await.unwrap(), id);
    reopened.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection
        .execute("DELETE FROM catalog_identity", [])
        .unwrap();
    drop(connection);
    assert!(Catalog::open(&dir.0).await.is_err());
}

/// A data directory that synchronized before schema 40 upgrades with its rows: each finding
/// gains the owner of the bucket incarnation it names, or none when that incarnation is gone,
/// and each root gains its observation and its deletion history.
#[tokio::test]
async fn a_synchronized_catalog_from_before_schema_40_upgrades_with_its_rows() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "owner"))
        .await
        .unwrap();
    let mut object = catalog
        .put_object(
            permissive_upload("bucket", "object", 100),
            body(vec![Bytes::from_static(b"data")]),
        )
        .await
        .unwrap();
    // Schema 39 predates object owners, so the downgrade below drops the one recorded here.
    object.owner = None;
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let incarnation: String = connection
        .query_row(
            "SELECT incarnation FROM bucket_incarnations WHERE bucket = 'bucket'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let gone = "0".repeat(32);
    record_root(&connection, "bucket", &[&incarnation, &gone]);
    test_support::downgrade_deletion_schema(&connection);
    connection.pragma_update(None, "user_version", 39).unwrap();
    drop(connection);

    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        reopened
            .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap(),
        object
    );
    reopened.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(user_version(&connection), CATALOG_SCHEMA_VERSION);
    let owners = |query: &str| {
        connection
            .prepare(query)
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let expected = vec![(incarnation, Some("owner".to_owned())), (gone, None)];
    assert_eq!(
        owners("SELECT incarnation, owner_project FROM sync_findings ORDER BY ordinal"),
        expected
    );
    assert_eq!(
        owners("SELECT incarnation, owner_project FROM sync_finding_owners ORDER BY ordinal"),
        expected
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT revision, paused_reason IS NULL AND scan_stop IS NULL FROM sync_observations WHERE root_id = ?1",
                [ROOT],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
            )
            .unwrap(),
        (0, true)
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT automatic_count FROM sync_deletion_history WHERE root_id = ?1",
                [ROOT],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn the_deletion_history_step_commits_whole_and_clears_the_count_only_at_a_new_baseline() {
    let mut connection = catalog_at(49);
    connection
        .execute(
            "INSERT INTO sync_roots VALUES(?1,?2,?3,?4,1)",
            params![ROOT, b"root", [0u8; 8], [1u8; 8]],
        )
        .unwrap();
    validate_schema(&connection, 49).unwrap();
    connection
        .execute_batch("CREATE TABLE sync_deletion_history(collision)")
        .unwrap();
    assert!(
        Migration::Statements(DELETION_HISTORY_SCHEMA)
            .apply(&mut connection)
            .is_err()
    );
    assert_eq!(user_version(&connection), 49);
    connection
        .execute_batch("DROP TABLE sync_deletion_history")
        .unwrap();
    Migration::Statements(DELETION_HISTORY_SCHEMA)
        .apply(&mut connection)
        .unwrap();
    validate_schema(&connection, 50).unwrap();
    let automatic = |connection: &Connection| {
        count(
            connection,
            "SELECT automatic_count FROM sync_deletion_history",
        )
    };
    assert_eq!(automatic(&connection), 0);
    connection
        .execute("UPDATE sync_deletion_history SET automatic_count=12", [])
        .unwrap();
    connection
        .execute("UPDATE sync_roots SET complete=1", [])
        .unwrap();
    assert_eq!(automatic(&connection), 12);
    connection
        .execute("UPDATE sync_roots SET complete=0", [])
        .unwrap();
    connection
        .execute("UPDATE sync_roots SET complete=1", [])
        .unwrap();
    assert_eq!(automatic(&connection), 0);
    assert!(validate_schema_version(50, 49).is_err());
}

/// Nothing at runtime reads or writes the retired tables: object writes, bucket deletion and a
/// reset leave every row a released catalog holds there, and the catalog still opens.
#[tokio::test]
async fn api_writes_bucket_deletion_and_reset_leave_retired_rows_alone() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "owner"))
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let incarnation: String = connection
        .query_row(
            "SELECT incarnation FROM bucket_incarnations WHERE bucket = 'bucket'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    record_root(&connection, "bucket", &[&incarnation]);
    let before = retired_rows(&connection);
    assert!(before.iter().any(|(_, rows)| *rows > 0));
    drop(connection);

    let catalog = Catalog::open_for_server(&dir.0, Arc::new(SystemClock), LocalAdmin::Enabled)
        .await
        .unwrap();
    for data in [b"data".as_slice(), b"replaced"] {
        catalog
            .put_object(
                permissive_upload("bucket", "object", 100),
                body(vec![Bytes::copy_from_slice(data)]),
            )
            .await
            .unwrap();
    }
    catalog
        .delete_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    catalog
        .delete_bucket("bucket", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    catalog.local_admin().unwrap().reset().await.unwrap();
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(retired_rows(&connection), before);
    drop(connection);
    Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
}
