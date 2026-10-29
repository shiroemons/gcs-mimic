#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::{
    clock::SystemClock,
    storage::admin::{BucketDefinition, BucketName, Origin},
    test_support::TestDir,
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
        fingerprint: [3; 32],
        inode: Some(u64::MAX),
        ctime_ns: Some(i128::MIN),
    }
}

async fn commit(
    catalog: &Catalog,
    binding: ImportBinding,
    cursor: i64,
    elements: Vec<ImportElement>,
) -> Result<(), AdminError> {
    catalog
        .run_admin(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            record_in_transaction(&transaction, &binding, cursor, &elements)?;
            for element in &elements {
                let input =
                    serde_json::json!({"name": format!("import-element-{}", element.ordinal)})
                        .to_string();
                let mut record = crate::json::parse_insert(
                    input.as_bytes(),
                    "project",
                    std::time::SystemTime::now(),
                )
                .unwrap();
                super::super::insert_bucket_in_transaction(&transaction, &mut record)?;
            }
            transaction.commit().map_err(unavailable)
        })
        .await
}

#[tokio::test]
async fn import_journal_binding_pages_prefix_and_reopen_are_bounded() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let journal = catalog.import_journal();
    let source = binding(257);
    assert!(!journal.in_progress().await.unwrap());
    assert_eq!(journal.begin(source.clone()).await.unwrap().cursor, 0);
    for foreign in [
        ImportBinding {
            source: [9; 32],
            ..source.clone()
        },
        ImportBinding {
            fingerprint: [9; 32],
            ..source.clone()
        },
        binding(256),
    ] {
        assert_eq!(journal.begin(foreign).await, Err(AdminError::Precondition));
    }
    assert!(journal.begin(binding(-1)).await.is_err());
    assert!(journal.page(source.clone(), 0, 0).await.is_err());
    assert!(journal.page(source.clone(), 0, 257).await.is_err());
    assert!(
        commit(
            &catalog,
            source.clone(),
            0,
            (1..=257).map(element).collect()
        )
        .await
        .is_err()
    );
    assert!(
        commit(&catalog, source.clone(), 0, vec![element(1), element(3)])
            .await
            .is_err()
    );
    assert!(
        journal
            .page(source.clone(), 0, 256)
            .await
            .unwrap()
            .is_empty()
    );
    commit(
        &catalog,
        source.clone(),
        0,
        (1..=256).map(element).collect(),
    )
    .await
    .unwrap();
    assert_eq!(journal.begin(source.clone()).await.unwrap().cursor, 256);
    assert_eq!(
        commit(&catalog, source.clone(), 0, vec![element(1)]).await,
        Err(AdminError::Precondition)
    );
    assert_eq!(
        journal.finish(source.clone()).await,
        Err(AdminError::Precondition)
    );
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let journal = catalog.import_journal();
    assert_eq!(
        journal.page(source.clone(), 0, 256).await.unwrap(),
        (1..=256).map(element).collect::<Vec<_>>()
    );
    assert!(
        journal
            .page(source.clone(), 256, 256)
            .await
            .unwrap()
            .is_empty()
    );
    commit(&catalog, source.clone(), 256, vec![element(257)])
        .await
        .unwrap();
    assert_eq!(
        journal.page(source.clone(), 256, 256).await.unwrap(),
        vec![element(257)]
    );
    journal.finish(source.clone()).await.unwrap();
    assert!(!journal.in_progress().await.unwrap());
    assert_eq!(journal.begin(source).await, Err(AdminError::Precondition));
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert!(!catalog.import_journal().in_progress().await.unwrap());
    assert!(catalog.admin(Origin::Seed).seed_complete().await.unwrap());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn import_journal_revalidation_is_atomic_without_advancing_progress() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let source = binding(2);
    let journal = catalog.import_journal();
    journal.begin(source.clone()).await.unwrap();
    commit(&catalog, source.clone(), 0, vec![element(1)])
        .await
        .unwrap();
    let before = element(1);
    let after = ImportElement {
        inode: Some(7),
        ctime_ns: Some(i128::MAX),
        ..before.clone()
    };
    let invalid = ImportElement {
        fingerprint: [9; 32],
        ..after.clone()
    };
    assert!(
        journal
            .revalidate(source.clone(), before.clone(), invalid)
            .await
            .is_err()
    );
    catalog.run_admin(|connection| connection.execute_batch("CREATE TRIGGER fail_observation BEFORE UPDATE ON import_observations BEGIN SELECT RAISE(ABORT, 'injected'); END;").map_err(unavailable)).await.unwrap();
    assert!(
        journal
            .revalidate(source.clone(), before.clone(), after.clone())
            .await
            .is_err()
    );
    assert_eq!(
        journal.page(source.clone(), 0, 1).await.unwrap(),
        vec![before.clone()]
    );
    catalog
        .run_admin(|connection| {
            connection
                .execute_batch("DROP TRIGGER fail_observation;")
                .map_err(unavailable)
        })
        .await
        .unwrap();
    journal
        .revalidate(source.clone(), before.clone(), after.clone())
        .await
        .unwrap();
    assert_eq!(
        journal
            .revalidate(source.clone(), before, after.clone())
            .await,
        Err(AdminError::Precondition)
    );
    assert_eq!(journal.begin(source.clone()).await.unwrap().cursor, 1);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog
            .import_journal()
            .page(source.clone(), 0, 1)
            .await
            .unwrap(),
        vec![after.clone()]
    );
    catalog
        .import_journal()
        .revalidate(source.clone(), after.clone(), after)
        .await
        .unwrap();
    assert_eq!(
        catalog.import_journal().begin(source).await.unwrap().cursor,
        1
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn import_journal_completion_and_seed_rollback_together() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let journal = catalog.import_journal();
    journal.begin(binding(0)).await.unwrap();
    catalog.run_admin(|connection| connection.execute_batch("CREATE TRIGGER fail_seed BEFORE UPDATE ON startup_seed BEGIN SELECT RAISE(ABORT, 'injected'); END;").map_err(unavailable)).await.unwrap();
    assert!(journal.finish(binding(0)).await.is_err());
    assert!(journal.in_progress().await.unwrap());
    assert!(!catalog.admin(Origin::Seed).seed_complete().await.unwrap());
    catalog
        .run_admin(|connection| {
            connection
                .execute_batch("DROP TRIGGER fail_seed;")
                .map_err(unavailable)
        })
        .await
        .unwrap();
    journal.finish(binding(0)).await.unwrap();
    assert!(catalog.admin(Origin::Seed).seed_complete().await.unwrap());
    catalog.close().await.unwrap();

    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .admin(Origin::BucketInit)
        .ensure_bucket(
            BucketDefinition::default_for(
                &BucketName::new("existing-bucket").unwrap(),
                "project",
                &SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        catalog.import_journal().begin(binding(0)).await,
        Err(AdminError::Precondition)
    );
    catalog.close().await.unwrap();
}

#[test]
fn import_journal_migration_rolls_back_and_retries_without_seed_changes() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA user_version = 15; CREATE TABLE import_observations(collision);")
        .unwrap();
    assert!(migrate(&mut connection).is_err());
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        15
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'import_journal'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    connection.execute_batch("DROP TABLE import_observations; CREATE TABLE startup_seed(singleton, complete); INSERT INTO startup_seed VALUES(1, 0);").unwrap();
    migrate(&mut connection).unwrap();
    validate(&connection).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        16
    );
    assert_eq!(
        connection
            .query_row("SELECT complete FROM startup_seed", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(super::super::validate_schema_version(16, 15).is_err());
}

#[tokio::test]
async fn import_journal_upgrade_preserves_native_state_and_stopped_snapshot() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .admin(Origin::BucketInit)
        .ensure_bucket(
            BucketDefinition::default_for(
                &BucketName::new("existing-bucket").unwrap(),
                "project",
                &SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let before = catalog
        .admin(Origin::Seed)
        .get_bucket(&BucketName::new("existing-bucket").unwrap())
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let path = dir.0.join("catalog.sqlite3");
    let connection = Connection::open(&path).unwrap();
    crate::storage::catalog::test_support::downgrade_soft_delete_schema(&connection);
    connection
        .execute_batch(
            "ALTER TABLE buckets DROP COLUMN retention_policy; ALTER TABLE buckets DROP COLUMN lifecycle; ALTER TABLE buckets DROP COLUMN uniform_bucket_level_access_locked_time; ALTER TABLE objects DROP COLUMN temporary_hold; ALTER TABLE objects DROP COLUMN custom_time; DROP TABLE sync_findings; DROP TABLE sync_writeback_ops; DROP TABLE sync_writeback_roots; DROP TABLE sync_roots; DROP TABLE sync_objects; DROP TABLE catalog_identity; DROP TABLE import_observations; DROP TABLE import_journal; PRAGMA user_version = 15;",
        )
        .unwrap();
    super::super::validate_schema(&connection, 15).unwrap();
    drop(connection);
    let backup = TestDir::new();
    std::fs::create_dir_all(&backup.0).unwrap();
    std::fs::copy(&path, backup.0.join("catalog.sqlite3")).unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog
            .admin(Origin::Seed)
            .get_bucket(&BucketName::new("existing-bucket").unwrap())
            .await
            .unwrap(),
        before
    );
    assert!(!catalog.admin(Origin::Seed).seed_complete().await.unwrap());
    catalog.close().await.unwrap();
    let connection = Connection::open(&path).unwrap();
    super::super::validate_schema(&connection, 31).unwrap();
    assert!(super::super::validate_schema(&connection, 15).is_err());
    let snapshot = Connection::open(backup.0.join("catalog.sqlite3")).unwrap();
    super::super::validate_schema(&snapshot, 15).unwrap();
}

#[test]
fn import_journal_crash_child() {
    let Some(path) = std::env::var_os("GCS_MIMIC_IMPORT_CRASH_CATALOG") else {
        return;
    };
    let mut connection = Connection::open(path).unwrap();
    let transaction = connection.transaction().unwrap();
    transaction
        .execute(
            "UPDATE import_observations SET inode = ?1 WHERE ordinal = 1",
            [9_u64.to_be_bytes()],
        )
        .unwrap();
    if std::env::var_os("GCS_MIMIC_IMPORT_CRASH_COMMITTED").is_some() {
        transaction.commit().unwrap();
    }
    std::process::exit(0);
}

#[tokio::test]
async fn import_journal_observation_crashes_preserve_committed_baselines() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let source = binding(2);
    catalog
        .import_journal()
        .begin(source.clone())
        .await
        .unwrap();
    commit(&catalog, source.clone(), 0, vec![element(1)])
        .await
        .unwrap();
    catalog.close().await.unwrap();
    for committed in [false, true] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "storage::catalog::import_journal::tests::import_journal_crash_child",
            ])
            .env(
                "GCS_MIMIC_IMPORT_CRASH_CATALOG",
                dir.0.join("catalog.sqlite3"),
            );
        if committed {
            command.env("GCS_MIMIC_IMPORT_CRASH_COMMITTED", "1");
        }
        assert!(command.status().unwrap().success());
        let catalog = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(
            catalog
                .import_journal()
                .page(source.clone(), 0, 1)
                .await
                .unwrap()[0]
                .inode,
            Some(if committed { 9 } else { u64::MAX })
        );
        assert_eq!(
            catalog
                .import_journal()
                .begin(source.clone())
                .await
                .unwrap()
                .cursor,
            1
        );
        assert!(catalog.import_journal().in_progress().await.unwrap());
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn import_journal_progress_hook_rolls_back_with_its_owning_transaction() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog.import_journal().begin(binding(1)).await.unwrap();
    catalog
        .run_admin(|connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            record_in_transaction(&transaction, &binding(1), 0, &[element(1)])?;
            transaction
                .execute("UPDATE startup_seed SET complete = 1", [])
                .map_err(unavailable)?;
            transaction.rollback().map_err(unavailable)
        })
        .await
        .unwrap();
    assert!(
        catalog
            .import_journal()
            .page(binding(1), 0, 1)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!catalog.admin(Origin::Seed).seed_complete().await.unwrap());
    assert_eq!(
        catalog
            .import_journal()
            .begin(binding(1))
            .await
            .unwrap()
            .cursor,
        0
    );
    catalog.close().await.unwrap();
}
