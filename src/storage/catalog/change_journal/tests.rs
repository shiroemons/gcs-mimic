use super::*;
use crate::storage::test_fixtures::{PERMISSIVE, body, live, publication};
use crate::{
    clock::SystemClock,
    object::ObjectMetadataUpdate,
    storage::admin::{BucketDefinition, BucketName, ObjectName},
    test_support::TestDir,
};
use bytes::Bytes;

mod observation;

#[test]
fn journal_sequences_preserve_sqlite_integer_bounds() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    for value in [0, u64::try_from(i64::MAX).unwrap()] {
        let sequence = JournalSeq(value);
        assert_eq!(
            connection
                .query_row("SELECT ?1", [sequence], |row| row.get::<_, JournalSeq>(0))
                .unwrap(),
            sequence
        );
    }
    assert!(matches!(
        connection.query_row("SELECT -1", [], |row| row.get::<_, JournalSeq>(0)),
        Err(rusqlite::Error::IntegralValueOutOfRange(0, -1))
    ));
    assert!(
        connection
            .query_row("SELECT ?1", [JournalSeq(u64::MAX)], |row| row
                .get::<_, JournalSeq>(0))
            .is_err()
    );
}

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

#[tokio::test]
async fn journal_records_atomic_origins_mutations_and_stable_pages() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .admin(Origin::Seed)
        .ensure_bucket(bucket("test-bucket"))
        .await
        .unwrap();
    publish(&catalog, Origin::Seed, "disabled").await;
    assert_eq!(
        catalog
            .read_changes(None, None, 256)
            .await
            .unwrap()
            .fence
            .sequence,
        JournalSeq(0)
    );
    let baseline = catalog.enable_change_journal().await.unwrap();
    assert!(
        catalog
            .read_changes(None, None, 1)
            .await
            .unwrap()
            .rescan_required
    );
    for origin in [
        Origin::Api,
        Origin::Seed,
        Origin::BucketInit,
        Origin::SyncIngest,
        Origin::Import,
    ] {
        publish(&catalog, origin, "object").await;
    }
    catalog
        .update_object_metadata(
            live("test-bucket", "object"),
            Default::default(),
            ObjectMetadataUpdate::default(),
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    catalog
        .delete_object(
            live("test-bucket", "object"),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let first = catalog.read_changes(Some(baseline), None, 2).await.unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.has_more);
    let fence = first.fence;
    publish(&catalog, Origin::Api, "later").await;
    let second = catalog
        .read_changes(Some(first.next), Some(fence), 256)
        .await
        .unwrap();
    assert_eq!(second.items.len(), 5);
    assert!(!second.has_more);
    assert_eq!(second.items.last().unwrap().kind, ChangeKind::Delete);
    assert_eq!(second.items[3].kind, ChangeKind::Metadata);
    let all = catalog
        .read_changes(Some(baseline), Some(fence), 256)
        .await
        .unwrap();
    assert_eq!(
        all.items[..5].iter().map(|c| c.origin).collect::<Vec<_>>(),
        [
            Origin::Api,
            Origin::Seed,
            Origin::BucketInit,
            Origin::SyncIngest,
            Origin::Import
        ]
    );
    assert!(all.items.iter().all(|c| c.project == "project"
        && c.incarnation.to_hex().len() == 32
        && c.generation.is_some()));
    let before = catalog
        .read_changes(Some(fence), None, 256)
        .await
        .unwrap()
        .fence;
    catalog.run::<_, CatalogError>(|connection| {
        connection.execute_batch("CREATE TEMP TRIGGER reject_change BEFORE INSERT ON change_journal BEGIN SELECT RAISE(ABORT, 'injected journal failure'); END;").map_err(unavailable)
    }).await.unwrap();
    assert!(
        catalog
            .admin(Origin::Seed)
            .ensure_bucket(bucket("rolled-back"))
            .await
            .is_err()
    );
    assert!(
        catalog
            .get_bucket("rolled-back", PERMISSIVE)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        catalog
            .read_changes(Some(before), None, 256)
            .await
            .unwrap()
            .fence,
        before
    );
    assert!(catalog.read_changes(Some(before), None, 0).await.is_err());
    assert!(catalog.read_changes(Some(before), None, 257).await.is_err());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn journal_retention_activation_restart_and_pap_require_rescan() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .admin(Origin::Seed)
        .ensure_bucket(bucket("test-bucket"))
        .await
        .unwrap();
    let baseline = catalog.enable_change_journal().await.unwrap();
    catalog
        .run::<_, CatalogError>(|connection| {
            let tx = connection.transaction()?;
            // Real retention boundary with fixed metadata, without staging 65,537 blobs.
            for _ in 0..=MAX_RECORDS {
                append(
                    &tx,
                    Origin::Api,
                    ChangeKind::BucketUpdate,
                    "test-bucket",
                    None,
                )?;
            }
            tx.commit().map_err(unavailable)
        })
        .await
        .unwrap();
    let page = catalog
        .read_changes(Some(baseline), None, 256)
        .await
        .unwrap();
    assert!(page.rescan_required);
    assert_eq!(page.fence.sequence, JournalSeq(MAX_RECORDS as u64 + 1));
    let mut cursor = baseline;
    cursor.sequence = JournalSeq(1);
    assert_eq!(
        catalog
            .read_changes(Some(cursor), None, 256)
            .await
            .unwrap()
            .items
            .len(),
        256
    );
    catalog
        .run::<_, CatalogError>(|connection| {
            validate(connection)?;
            Ok(())
        })
        .await
        .unwrap();
    let latest = page.fence;
    catalog
        .configure_public_access_prevention(&["project".into()])
        .await
        .unwrap();
    assert!(
        catalog
            .read_changes(Some(latest), None, 1)
            .await
            .unwrap()
            .rescan_required
    );
    let pap = catalog.read_changes(None, None, 1).await.unwrap().fence;
    catalog
        .configure_public_access_prevention(&["project".into(), "project".into()])
        .await
        .unwrap();
    assert_eq!(
        catalog
            .read_changes(Some(pap), None, 1)
            .await
            .unwrap()
            .fence,
        pap
    );
    catalog.disable_change_journal().await.unwrap();
    catalog
        .admin(Origin::Import)
        .ensure_bucket(bucket("unlogged-bucket"))
        .await
        .unwrap();
    let enabled = catalog.enable_change_journal().await.unwrap();
    assert_eq!(enabled.sequence, pap.sequence);
    assert!(
        catalog
            .read_changes(Some(pap), None, 1)
            .await
            .unwrap()
            .rescan_required
    );
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert!(
        catalog
            .read_changes(Some(enabled), None, 1)
            .await
            .unwrap()
            .rescan_required
    );
    let restarted = catalog.enable_change_journal().await.unwrap();
    catalog
        .configure_public_access_prevention(&[])
        .await
        .unwrap();
    assert!(
        catalog
            .read_changes(Some(restarted), None, 1)
            .await
            .unwrap()
            .rescan_required
    );
    catalog.close().await.unwrap();
}

#[test]
fn journal_migration_is_atomic_and_inventory_cannot_hide_extra_tables() {
    assert!(<ChangeKind as FromSql>::column_result(ValueRef::Text(b"unknown")).is_err());
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
}

#[tokio::test]
async fn journal_bucket_iam_compose_and_resumable_share_mutation_transactions() {
    use super::super::{DerivedDestination, ResumableChunk, ResumableOptions};
    use crate::{bucket::BucketMetadataUpdate, object::ComposeSource};
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let baseline = catalog.enable_change_journal().await.unwrap();
    catalog
        .admin(Origin::BucketInit)
        .ensure_bucket(bucket("test-bucket"))
        .await
        .unwrap();
    catalog
        .admin(Origin::Import)
        .ensure_bucket(bucket("empty-bucket"))
        .await
        .unwrap();
    catalog
        .patch_bucket(
            "empty-bucket",
            BucketMetadataUpdate {
                versioning_enabled: Some(true),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let policy = catalog
        .get_policy("empty-bucket", PERMISSIVE)
        .await
        .unwrap();
    catalog
        .set_policy("empty-bucket", policy, PERMISSIVE)
        .await
        .unwrap();
    catalog
        .delete_bucket("empty-bucket", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    publish(&catalog, Origin::Seed, "source").await;
    let destination = DerivedDestination {
        bucket: "test-bucket".into(),
        name: "composed".into(),
        content_type: "text/plain".into(),
        ..DerivedDestination::new(BucketAccess::Permissive)
    };
    catalog
        .compose_object(
            destination,
            vec![ComposeSource {
                name: "source".into(),
                generation: None,
                if_generation_match: None,
            }],
            crate::storage::SourceDisposal::Delete,
            100,
        )
        .await
        .unwrap();
    let options = ResumableOptions::new(BucketAccess::Permissive);
    let session = catalog
        .create_resumable_session("test-bucket", "resumable", options.clone(), Some(4))
        .await
        .unwrap();
    let lease = catalog
        .lock_resumable_session(&session.id, "test-bucket")
        .await
        .unwrap();
    catalog
        .write_resumable_session(
            lease,
            ResumableChunk {
                start: 0,
                length: 4,
                total: Some(4),
                finalize: true,
                open_end: false,
                options,
                catalog_wait: tokio::sync::watch::channel(false).0,
            },
            body(vec![Bytes::from_static(b"body")]),
            catalog.admit_object_upload().unwrap(),
            |_| Ok(serde_json::json!({})),
        )
        .await
        .unwrap();
    let page = catalog
        .read_changes(Some(baseline), None, 256)
        .await
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|c| c.kind.as_str())
            .collect::<Vec<_>>(),
        [
            "bucket-create",
            "bucket-create",
            "bucket-update",
            "iam",
            "bucket-delete",
            "publish",
            "publish",
            "delete",
            "publish"
        ]
    );
    assert_eq!(page.items[0].origin, Origin::BucketInit);
    assert_eq!(page.items[1].origin, Origin::Import);
    assert_eq!(page.items[4].incarnation, page.items[1].incarnation);
    assert_eq!(page.items[7].object.as_deref(), Some("source"));
    assert_eq!(page.items[8].object.as_deref(), Some("resumable"));
    catalog
        .run::<_, CatalogError>(|connection| {
            for index in 0..40 {
                connection.execute_batch(&format!("CREATE TABLE zz_extra_{index}(value);"))?;
            }
            assert!(super::super::validate_schema(connection, 16).is_err());
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
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
        .lock_resumable_session(&session.id, "test-bucket")
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
