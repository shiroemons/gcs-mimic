use super::*;

#[tokio::test]
async fn upgrading_stored_rules_after_their_deadlines_transitions_before_old_class_delete() {
    let data = TestDir::new();
    let (catalog, clock) = disk(&data).await;
    create(
        &catalog,
        vec![
            transition(StorageClass::Nearline, json!({"age":30})),
            delete(json!({"age":60,"matchesStorageClass":["STANDARD"]})),
        ],
    )
    .await;
    let original = put(&catalog, "object").await;
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    crate::storage::catalog::test_support::downgrade_storage_class_updated_schema(&connection);
    connection.pragma_update(None, "user_version", 52).unwrap();
    drop(connection);
    clock.advance(DAY * 100);
    let catalog = Catalog::open_with_clock(&data.0, clock.clone())
        .await
        .unwrap();
    catalog.configure_lifecycle(&Default::default());
    let outcome = execute(&catalog).await;
    assert_eq!((outcome.transitioned, outcome.deleted), (1, 0));
    let changed = current(&catalog, "object").await;
    assert_eq!(changed.storage_class, StorageClass::Nearline);
    assert_eq!(changed.generation, original.generation);
    assert_eq!(changed.storage_class_updated(), clock.now());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn schema_52_migrates_null_class_times_and_executes_stored_rules() {
    let data = TestDir::new();
    let (catalog, clock) = disk(&data).await;
    create(
        &catalog,
        vec![transition(StorageClass::Coldline, json!({"age":1}))],
    )
    .await;
    let original = put(&catalog, "object").await;
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    crate::storage::catalog::test_support::downgrade_storage_class_updated_schema(&connection);
    connection.pragma_update(None, "user_version", 52).unwrap();
    drop(connection);
    let catalog = Catalog::open_with_clock(&data.0, clock.clone())
        .await
        .unwrap();
    catalog.configure_lifecycle(&Default::default());
    assert_eq!(current(&catalog, "object").await, original);
    let mut serialized = serde_json::to_value(&original).unwrap();
    serialized
        .as_object_mut()
        .unwrap()
        .remove("time_storage_class_updated");
    let legacy: ObjectRecord = serde_json::from_value(serialized).unwrap();
    assert_eq!(legacy.storage_class_updated(), original.time_created);
    assert_eq!(
        current(&catalog, "object").await.storage_class_updated(),
        original.time_created
    );
    counts(
        execute(&catalog).await,
        0,
        0,
        0,
        Some(original.time_created + DAY),
    );
    clock.advance(DAY);
    assert_eq!(execute(&catalog).await.transitioned, 1);
    let changed = current(&catalog, "object").await;
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        53
    );
    assert!(matches!(
        crate::storage::catalog::schema::validate_schema_version(53, 52),
        Err(CatalogError::NewerSchema {
            version: 53,
            supported: 52
        })
    ));
    drop(connection);
    let catalog = Catalog::open_with_clock(&data.0, clock).await.unwrap();
    assert_eq!(current(&catalog, "object").await, changed);
    catalog.close().await.unwrap();
}
