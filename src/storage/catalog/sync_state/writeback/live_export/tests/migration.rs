use super::*;

#[tokio::test]
async fn live_export_sqlite_full_preserves_displaced_bytes_until_accounting_can_retry() {
    let fixture = Fixture::new().await;
    let record = fixture.publish("object", b"api").await;
    let item = armed_observation(
        &fixture.writeback,
        &fixture.bucket,
        &ObjectName::new("object").unwrap(),
        Generation::new(record.generation).unwrap(),
        fixture
            .catalog
            .read_changes(None, None, 1)
            .await
            .unwrap()
            .fence,
    )
    .await;
    let path = fixture.tree.0.join(mapping::WORKSPACE).join(&item.token.id);
    fs::write(&path, b"recover after full disk").unwrap();
    fixture.writeback.db(|connection| {
        connection.execute_batch("CREATE TABLE writeback_fill(value BLOB)")?;
        let pages:i64=connection.pragma_query_value(None,"page_count",|row|row.get(0))?;
        connection.pragma_update(None,"max_page_count",pages)?;
        loop {match connection.execute("INSERT INTO writeback_fill VALUES(zeroblob(4096))",[]) {Ok(_)=>{},Err(error)=>{assert!(matches!(error,rusqlite::Error::SqliteFailure(ref code,_) if code.code==rusqlite::ErrorCode::DiskFull));break;}}}
        connection.execute_batch("CREATE TEMP TRIGGER writeback_full BEFORE UPDATE OF phase ON sync_writeback_ops WHEN NEW.phase=2 BEGIN INSERT INTO writeback_fill VALUES(zeroblob(4096)); END;")?;
        Ok(())
    }).await.unwrap();
    assert!(matches!(
        fixture.writeback.recover_pending().await,
        Err(Error::Admin(AdminError::DiskFull))
    ));
    assert_eq!(
        fixture.writeback.page(0, 1).await.unwrap()[0].phase,
        Phase::Armed
    );
    assert_eq!(
        fixture.writeback.status().await.unwrap().stop,
        Stop::RecoveryRequired
    );
    assert_eq!(fs::read(&path).unwrap(), b"recover after full disk");
    fixture
        .writeback
        .db(|connection| {
            connection.pragma_update(None, "max_page_count", 1_000_000)?;
            connection.execute_batch("DROP TRIGGER writeback_full; DROP TABLE writeback_fill")?;
            Ok(())
        })
        .await
        .unwrap();
    let usage = fixture.writeback.recover_pending().await.unwrap();
    assert_eq!(
        (usage.stop, usage.retained_bytes),
        (Stop::ProtectedBody, 23)
    );
    assert_eq!(
        fixture
            .catalog
            .admin(Origin::Api)
            .get_object(&fixture.bucket, &ObjectName::new("object").unwrap(), None)
            .await
            .unwrap(),
        record
    );
    fixture.close().await;
}

#[tokio::test]
async fn live_export_schema_migration_rolls_back_retries_and_preserves_api_seed_root_and_sync() {
    let fixture = Fixture::new().await;
    let id = fixture.catalog.sync_state().catalog_id().await.unwrap();
    let root_id = fixture.root.status().root_id;
    let mut record = fixture.publish("preserved", b"immutable").await;
    let operation = fixture.begin("preserved").await;
    for _ in 0..4 {
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
    }
    fixture.root.complete_baseline().await.unwrap();
    drop(operation);
    let Fixture {
        data,
        tree,
        catalog,
        root,
        writeback,
        bucket,
    } = fixture;
    drop(writeback);
    drop(root);
    catalog.close().await.unwrap();
    let mut connection = Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    crate::storage::catalog::test_support::downgrade_soft_delete_schema(&connection);
    record.owner = None;
    let seed: i64 = connection
        .query_row("SELECT complete FROM startup_seed", [], |row| row.get(0))
        .unwrap();
    connection.execute_batch("ALTER TABLE buckets DROP COLUMN retention_policy; ALTER TABLE buckets DROP COLUMN lifecycle; ALTER TABLE buckets DROP COLUMN uniform_bucket_level_access_locked_time; ALTER TABLE objects DROP COLUMN temporary_hold; ALTER TABLE objects DROP COLUMN custom_time; DROP TABLE sync_findings; DROP TABLE sync_writeback_ops; DROP TABLE sync_writeback_roots; PRAGMA user_version=19; CREATE TABLE sync_writeback_ops(collision)").unwrap();
    assert!(
        crate::storage::catalog::Migration::Statements(SCHEMA)
            .apply(&mut connection)
            .is_err()
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        19
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='sync_writeback_roots'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    connection
        .execute_batch("DROP TABLE sync_writeback_ops; CREATE TABLE writeback_fill(value BLOB)")
        .unwrap();
    let pages: i64 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    connection
        .pragma_update(None, "max_page_count", pages)
        .unwrap();
    loop {
        match connection.execute("INSERT INTO writeback_fill VALUES(zeroblob(4096))", []) {
            Ok(_) => {}
            Err(error) => {
                assert!(
                    matches!(error,rusqlite::Error::SqliteFailure(ref code,_) if code.code==rusqlite::ErrorCode::DiskFull)
                );
                break;
            }
        }
    }
    assert!(
        crate::storage::catalog::Migration::Statements(SCHEMA)
            .apply(&mut connection)
            .is_err()
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        19
    );
    connection
        .pragma_update(None, "max_page_count", 1_000_000)
        .unwrap();
    connection
        .execute_batch("DROP TABLE writeback_fill")
        .unwrap();
    {
        let transaction = connection.transaction().unwrap();
        transaction.execute_batch(SCHEMA).unwrap();
        transaction.rollback().unwrap();
    }
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        19
    );
    drop(connection);
    let catalog = Catalog::open(&data.0).await.unwrap();
    assert_eq!(catalog.sync_state().catalog_id().await.unwrap(), id);
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, super::super::super::super::root::Activation::Open)
        .await
        .unwrap();
    assert_eq!(root.status().root_id, root_id);
    assert_eq!(
        root.status().baseline,
        super::super::super::super::root::Baseline::Complete
    );
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    assert_eq!(
        catalog
            .sync_state()
            .get_path(binding, "test-bucket/preserved".into())
            .await
            .unwrap()
            .unwrap()
            .generation
            .get(),
        record.generation
    );
    let (actual, reader) = catalog
        .admin(Origin::SyncIngest)
        .open_object(&bucket, &ObjectName::new("preserved").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(actual, record);
    drop(reader);
    assert_eq!(
        catalog
            .run_admin(|connection| connection
                .query_row("SELECT complete FROM startup_seed", [], |row| row
                    .get::<_, i64>(0))
                .map_err(unavailable))
            .await
            .unwrap(),
        seed
    );
    drop(root);
    catalog.close().await.unwrap();
}
