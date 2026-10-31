use super::*;
use crate::{storage::Catalog, test_support::TestDir};

#[tokio::test]
async fn imported_negative_nanoseconds_survive_restart_and_native_metadata_writers() {
    use crate::{acl::AclTarget, bucket::BucketAccess, storage::catalog::acl::AclChange};
    use std::time::UNIX_EPOCH;
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let bucket =
        crate::json::parse_insert(br#"{"name":"time-bucket"}"#, "project", SystemTime::now())
            .unwrap();
    catalog.insert_bucket(bucket).await.unwrap();
    let created = UNIX_EPOCH - Duration::from_nanos(1);
    catalog
        .run(move |connection| {
            let mut record = super::super::select_bucket(connection, "time-bucket")
                .map_err(unavailable)?
                .unwrap();
            store_imported(connection, &mut record, created)
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for writer in ["acl", "notification"] {
        let record = catalog.get_bucket("time-bucket").await.unwrap().unwrap();
        assert_eq!(record.time_created, created);
        if writer == "acl" {
            let resource = crate::json::bucket_resource(
                &record,
                "http://localhost",
                crate::json::Projection::NoAcl,
            )
            .unwrap();
            assert_eq!(resource["timeCreated"], "1969-12-31T23:59:59.999999999Z");
            let timestamp: prost_types::Timestamp = created.into();
            assert_eq!((timestamp.seconds, timestamp.nanos), (-1, 999_999_999));
            catalog
                .change_acl(
                    AclTarget::Bucket("time-bucket".into()),
                    AclChange::Upsert {
                        entity: "user-clock@example.invalid".into(),
                        role: Some("READER".into()),
                    },
                    &BucketAccess::Permissive,
                )
                .await
                .unwrap();
        } else {
            catalog
                .insert_notification(
                    "time-bucket",
                    crate::notification::NotificationConfig {
                        topic: "//pubsub.googleapis.com/projects/project/topics/time".into(),
                        payload_format: "NONE".into(),
                        event_types: Vec::new(),
                        custom_attributes: Default::default(),
                        object_name_prefix: String::new(),
                    },
                    &BucketAccess::Permissive,
                )
                .await
                .unwrap();
        }
        let record = catalog.get_bucket("time-bucket").await.unwrap().unwrap();
        assert_eq!(record.time_created, created);
        assert_eq!(
            record
                .updated
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
                % 1000,
            0
        );
        // Restore a nonzero remainder so each independent writer proves it clears stale precision.
        catalog
            .run(move |connection| {
                let mut record = super::super::select_bucket(connection, "time-bucket")
                    .map_err(unavailable)?
                    .unwrap();
                store_imported(connection, &mut record, created)
            })
            .await
            .unwrap();
    }
    catalog.close().await.unwrap();
}

#[test]
fn partial_migration_rolls_back_the_first_column_and_schema_version() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE buckets(updated_nanos INTEGER); PRAGMA user_version=38;")
        .unwrap();
    assert!(
        super::super::Migration::Statements(SCHEMA)
            .apply(&mut connection)
            .is_err()
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        38
    );
    assert!(!connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('buckets') WHERE name='time_created_nanos')",[],|row|row.get::<_,bool>(0)).unwrap());
}

#[tokio::test]
async fn previous_schema_upgrades_without_changing_microsecond_instants() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let bucket =
        crate::json::parse_insert(br#"{"name":"time-bucket"}"#, "project", SystemTime::now())
            .unwrap();
    let bucket = catalog.insert_bucket(bucket).await.unwrap();
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    super::super::test_support::downgrade_bucket_timestamp_schema(&connection);
    connection.pragma_update(None, "user_version", 38).unwrap();
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog.get_bucket("time-bucket").await.unwrap().unwrap(),
        bucket
    );
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    for invalid in [-1, 1000] {
        assert!(
            connection
                .execute("UPDATE buckets SET time_created_nanos=?1", [invalid])
                .is_err()
        );
    }
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        super::super::CATALOG_SCHEMA_VERSION
    );
}
