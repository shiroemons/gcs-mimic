use super::*;
use crate::storage::test_fixtures::PERMISSIVE;
use crate::{
    clock::SystemClock,
    storage::{
        Catalog,
        admin::{
            BucketDefinition, BucketName, GenerationCondition, Origin, Publication, PublishMode,
        },
    },
    test_support::TestDir,
};
use rusqlite::Connection;
use std::fs;

async fn control_identity(projects: &[&str]) -> crate::auth::Identity {
    let projects: std::collections::BTreeMap<_, _> = projects
        .iter()
        .map(|project| (*project, vec!["roles/storage.admin"]))
        .collect();
    let registry=crate::auth::Registry::parse(&serde_json::to_vec(&serde_json::json!({"identities":[{"id":"admin","tokens":["control-token"],"project_roles":projects}]})).unwrap()).unwrap();
    let auth = crate::auth::Auth::strict(registry);
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        "authorization",
        hyper::header::HeaderValue::from_static("Bearer control-token"),
    );
    auth.authenticate_request(
        &headers,
        None,
        &SystemClock,
        crate::auth::ListenerRole::Public,
    )
    .await
    .unwrap()
    .unwrap()
    .principal()
    .unwrap()
    .clone()
}

async fn publish(catalog: &Catalog, data: &TestDir, bucket: &BucketName, name: &str) -> Generation {
    let admin = catalog.admin(Origin::SyncIngest);
    let source = data.0.join("source");
    fs::write(&source, b"finding body").unwrap();
    let staged = admin
        .stage_from_file(fs::File::open(source).unwrap(), Default::default(), 1024)
        .await
        .unwrap();
    let record = admin
        .publish_objects(vec![crate::storage::admin::BatchPublication {
            bucket: bucket.clone(),
            name: ObjectName::new(name).unwrap(),
            staged,
            request: Publication {
                content_type: "text/plain".into(),
                metadata: Default::default(),
                expected: Default::default(),
                condition: None,
                max_bytes: 1024,
                mode: PublishMode::Replace,
            },
        }])
        .await
        .unwrap()
        .results
        .remove(0)
        .unwrap();
    Generation::new(record.generation).unwrap()
}

#[tokio::test]
async fn sync_findings_idempotent_pages_conditional_removal_and_restart() {
    let data = TestDir::new();
    let root = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    let admin = catalog.admin(Origin::SyncIngest);
    admin
        .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
        .await
        .unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    for index in 0..258 {
        let name = ObjectName::new(format!("nested/{index:03}.swp")).unwrap();
        let generation = publish(&catalog, &data, &bucket, name.as_str()).await;
        lease
            .record_finding(binding.clone(), name.clone(), generation, Reason::Ignored)
            .await
            .unwrap();
        lease
            .record_finding(binding.clone(), name, generation, Reason::Ignored)
            .await
            .unwrap();
    }
    let page = lease.findings_page(0, 256).await.unwrap();
    assert_eq!(page.len(), 256);
    assert_eq!(
        lease
            .findings_page(page.last().unwrap().ordinal, 256)
            .await
            .unwrap()
            .len(),
        2
    );
    for (after, limit) in [(-1, 1), (0, 0), (0, 257)] {
        assert!(lease.findings_page(after, limit).await.is_err());
    }
    let old = page[0].clone();
    let name = ObjectName::new(old.name.clone()).unwrap();
    let new_generation = publish(&catalog, &data, &bucket, &old.name).await;
    assert!(
        lease
            .record_finding(
                binding.clone(),
                name.clone(),
                old.generation,
                Reason::Ignored
            )
            .await
            .is_err()
    );
    lease
        .record_finding(
            binding.clone(),
            name.clone(),
            new_generation,
            Reason::Collision,
        )
        .await
        .unwrap();
    assert!(!lease.remove_finding(old).await.unwrap());
    let current = lease.findings_page(0, 1).await.unwrap().remove(0);
    admin
        .delete_object(&bucket, &name, GenerationCondition::Is(new_generation))
        .await
        .unwrap();
    assert!(lease.remove_finding(current.clone()).await.unwrap());
    assert!(!lease.remove_finding(current).await.unwrap());
    let expected = lease.findings_page(0, 256).await.unwrap();
    drop(lease);
    drop(admin);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    assert_eq!(lease.findings_page(0, 256).await.unwrap(), expected);
    let other_root = TestDir::new();
    let other = catalog
        .sync_state()
        .activate_root(&other_root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    assert!(other.findings_page(0, 256).await.unwrap().is_empty());
    assert!(other.remove_finding(expected[0].clone()).await.is_err());
    drop(other);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_findings_reject_foreign_bindings_and_stale_incarnations() {
    let data = TestDir::new();
    let root = TestDir::new();
    let foreign_data = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let foreign = Catalog::open(&foreign_data.0).await.unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    for c in [&catalog, &foreign] {
        c.admin(Origin::SyncIngest)
            .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
    }
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    let name = ObjectName::new("a//b").unwrap();
    let generation = publish(&catalog, &data, &bucket, name.as_str()).await;
    let foreign_binding = foreign.sync_state().binding(&bucket).await.unwrap();
    assert!(
        lease
            .record_finding(
                foreign_binding,
                name.clone(),
                generation,
                Reason::InvalidName
            )
            .await
            .is_err()
    );
    for reason in [
        Reason::InvalidName,
        Reason::HostRestriction,
        Reason::Reserved,
        Reason::Ignored,
        Reason::Collision,
        Reason::UnsafeEntry,
    ] {
        lease
            .record_finding(binding.clone(), name.clone(), generation, reason)
            .await
            .unwrap();
        assert_eq!(
            lease.findings_page(0, 1).await.unwrap()[0].reason,
            FindingKind::Unmapped(reason)
        );
    }
    let old = lease.findings_page(0, 1).await.unwrap().remove(0);
    let seed_and_current = control_identity(&["seed", "new-owner"]).await;
    let all_owners = control_identity(&["seed", "owner", "new-owner"]).await;
    assert!(matches!(
        lease
            .authorize_controls("seed".into(), Some(seed_and_current.clone()))
            .await,
        Err(RootError::Admin(AdminError::Forbidden))
    ));
    catalog
        .admin(Origin::SyncIngest)
        .delete_object(&bucket, &name, GenerationCondition::Is(generation))
        .await
        .unwrap();
    catalog
        .delete_bucket(bucket.as_str(), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert!(matches!(
        lease
            .authorize_controls("seed".into(), Some(seed_and_current.clone()))
            .await,
        Err(RootError::Admin(AdminError::Forbidden))
    ));
    lease
        .authorize_controls("seed".into(), Some(all_owners.clone()))
        .await
        .unwrap();
    catalog
        .admin(Origin::SyncIngest)
        .ensure_bucket(BucketDefinition::default_for(&bucket, "new-owner", &SystemClock).unwrap())
        .await
        .unwrap();
    let new_generation = publish(&catalog, &data, &bucket, name.as_str()).await;
    assert!(
        lease
            .record_finding(binding, name.clone(), new_generation, Reason::InvalidName)
            .await
            .is_err()
    );
    let new_binding = catalog.sync_state().binding(&bucket).await.unwrap();
    lease
        .record_finding(new_binding, name, new_generation, Reason::InvalidName)
        .await
        .unwrap();
    assert!(matches!(
        lease
            .authorize_controls("seed".into(), Some(seed_and_current.clone()))
            .await,
        Err(RootError::Admin(AdminError::Forbidden))
    ));
    lease
        .authorize_controls("seed".into(), Some(all_owners.clone()))
        .await
        .unwrap();
    assert!(!lease.remove_finding(old.clone()).await.unwrap());
    let current = lease.findings_page(0, 1).await.unwrap().remove(0);
    assert_ne!(old.incarnation, current.incarnation);
    drop(lease);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    assert!(matches!(
        lease
            .authorize_controls("seed".into(), Some(seed_and_current.clone()))
            .await,
        Err(RootError::Admin(AdminError::Forbidden))
    ));
    lease
        .authorize_controls("seed".into(), Some(all_owners.clone()))
        .await
        .unwrap();
    catalog
        .run_admin(|connection| {
            connection
                .execute(
                    "UPDATE sync_finding_owners SET owner_project=NULL WHERE owner_project='owner'",
                    [],
                )
                .map_err(unavailable)?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        lease
            .authorize_controls("seed".into(), Some(all_owners))
            .await,
        Err(RootError::Admin(AdminError::Forbidden))
    ));
    assert!(lease.remove_finding(current).await.unwrap());
    lease
        .authorize_controls("seed".into(), Some(seed_and_current))
        .await
        .unwrap();
    drop(lease);
    foreign.close().await.unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn finding_owner_migration_backfills_only_proven_incarnations_and_rolls_back_atomically() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&tree.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    for name in ["current-bucket", "deleted-bucket", "reused-bucket"] {
        let bucket = BucketName::new(name).unwrap();
        catalog
            .admin(Origin::Api)
            .ensure_bucket(
                BucketDefinition::default_for(&bucket, "old-owner", &SystemClock).unwrap(),
            )
            .await
            .unwrap();
        let generation = publish(&catalog, &data, &bucket, "a//b").await;
        lease
            .record_finding(
                catalog.sync_state().binding(&bucket).await.unwrap(),
                ObjectName::new("a//b").unwrap(),
                generation,
                Reason::InvalidName,
            )
            .await
            .unwrap();
        if name != "current-bucket" {
            catalog
                .admin(Origin::Api)
                .delete_object(
                    &bucket,
                    &ObjectName::new("a//b").unwrap(),
                    GenerationCondition::Is(generation),
                )
                .await
                .unwrap();
            catalog
                .delete_bucket(name, Default::default(), PERMISSIVE)
                .await
                .unwrap();
            if name == "reused-bucket" {
                catalog
                    .admin(Origin::Api)
                    .ensure_bucket(
                        BucketDefinition::default_for(&bucket, "new-owner", &SystemClock).unwrap(),
                    )
                    .await
                    .unwrap();
            }
        }
    }
    drop(lease);
    catalog.close().await.unwrap();
    let mut connection = Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    crate::storage::catalog::test_support::downgrade_deletion_schema(&connection);
    connection.pragma_update(None, "user_version", 39).unwrap();
    connection.execute_batch("CREATE TEMP TRIGGER fail_owner_migration BEFORE UPDATE ON sync_findings BEGIN SELECT RAISE(ABORT,'injected owner migration failure'); END;").unwrap();
    assert!(
        crate::storage::catalog::Migration::Statements(super::super::deletions::SCHEMA)
            .apply(&mut connection)
            .is_err()
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        39
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM sync_findings", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert!(!connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('sync_findings') WHERE name='owner_project')",[],|row|row.get::<_,bool>(0)).unwrap());
    connection
        .execute_batch("DROP TRIGGER fail_owner_migration;")
        .unwrap();
    drop(connection);
    let catalog = Catalog::open(&data.0).await.unwrap();
    let owners = catalog
        .run_admin(|connection| {
            connection
                .prepare_cached("SELECT bucket,owner_project FROM sync_findings ORDER BY bucket")
                .map_err(unavailable)?
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map_err(unavailable)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(unavailable)
        })
        .await
        .unwrap();
    assert_eq!(
        owners,
        vec![
            ("current-bucket".into(), Some("old-owner".into())),
            ("deleted-bucket".into(), None),
            ("reused-bucket".into(), None)
        ]
    );
    let lease = catalog
        .sync_state()
        .activate_root(&tree.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    assert!(matches!(
        lease
            .authorize_controls(
                "seed".into(),
                Some(control_identity(&["seed", "old-owner", "new-owner"]).await)
            )
            .await,
        Err(RootError::Admin(AdminError::Forbidden))
    ));
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_findings_migration_rolls_back_and_preserves_catalog_root_and_writeback() {
    let data = TestDir::new();
    let root = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let id = catalog.sync_state().catalog_id().await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let root_id = lease.status().root_id;
    let wb = lease
        .writeback(super::super::writeback::Limits::new(1024, 10).unwrap())
        .await
        .unwrap();
    drop(wb);
    drop(lease);
    catalog.close().await.unwrap();
    let mut c = Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    crate::storage::catalog::test_support::downgrade_soft_delete_schema(&c);
    c.execute_batch(
        "ALTER TABLE buckets DROP COLUMN retention_policy; ALTER TABLE buckets DROP COLUMN lifecycle; ALTER TABLE buckets DROP COLUMN uniform_bucket_level_access_locked_time; ALTER TABLE objects DROP COLUMN temporary_hold; ALTER TABLE objects DROP COLUMN custom_time; DROP TABLE sync_findings; PRAGMA user_version=20; CREATE TABLE sync_findings(collision)",
    )
    .unwrap();
    assert!(
        crate::storage::catalog::Migration::Statements(SCHEMA)
            .apply(&mut c)
            .is_err()
    );
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        20
    );
    c.execute_batch("DROP TABLE sync_findings").unwrap();
    c.execute_batch("CREATE TABLE finding_fill(value BLOB)")
        .unwrap();
    let pages: i64 = c
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    c.pragma_update(None, "max_page_count", pages).unwrap();
    loop {
        match c.execute("INSERT INTO finding_fill VALUES(zeroblob(4096))", []) {
            Ok(_) => {}
            Err(e) => {
                assert!(
                    matches!(e,rusqlite::Error::SqliteFailure(ref e,_) if e.code==rusqlite::ErrorCode::DiskFull)
                );
                break;
            }
        }
    }
    assert!(
        crate::storage::catalog::Migration::Statements(SCHEMA)
            .apply(&mut c)
            .is_err()
    );
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        20
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='sync_findings'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    c.pragma_update(None, "max_page_count", 1_000_000).unwrap();
    c.execute_batch("DROP TABLE finding_fill").unwrap();
    {
        let tx = c.transaction().unwrap();
        tx.execute_batch(SCHEMA).unwrap();
        tx.rollback().unwrap();
    }
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        20
    );
    drop(c);
    let catalog = Catalog::open(&data.0).await.unwrap();
    assert_eq!(catalog.sync_state().catalog_id().await.unwrap(), id);
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    assert_eq!(lease.status().root_id, root_id);
    let wb = lease
        .writeback(super::super::writeback::Limits::new(1024, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(wb.status().await.unwrap().entries, 0);
    assert!(lease.findings_page(0, 256).await.unwrap().is_empty());
    drop(wb);
    drop(lease);
    catalog.close().await.unwrap();
}

mod reason_migration;

/// The findings revision and the observation revision of one root.
async fn revisions(lease: &RootLease) -> (i64, i64) {
    (
        lease
            .mapping_findings_revision_for(lease.authority())
            .await
            .unwrap(),
        lease.observation().await.unwrap().revision,
    )
}

#[tokio::test]
async fn mapping_findings_revision_moves_with_findings_only_and_not_for_an_identical_record() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    catalog
        .admin(Origin::Api)
        .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
        .await
        .unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&tree.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    let name = ObjectName::new("object").unwrap();
    let generation = publish(&catalog, &data, &bucket, name.as_str()).await;
    let start = revisions(&lease).await;

    // A tracked file changing is not a finding change.
    catalog
        .run_admin(|connection| {
            connection
                .execute_batch("INSERT INTO sync_objects VALUES('test-bucket','tracked','00000000000000000000000000000000','test-bucket/tracked',1,0,zeroblob(16),zeroblob(16),zeroblob(8),zeroblob(16),0,0)")
                .map_err(unavailable)
        })
        .await
        .unwrap();
    let tracked = revisions(&lease).await;
    assert_eq!(tracked.0, start.0);
    assert!(tracked.1 > start.1);

    let record = |reason| lease.record_finding(binding.clone(), name.clone(), generation, reason);
    record(Reason::InvalidName).await.unwrap();
    let recorded = revisions(&lease).await;
    assert_eq!(recorded.0, tracked.0 + 1);
    record(Reason::InvalidName).await.unwrap();
    assert_eq!(revisions(&lease).await, recorded);
    record(Reason::Reserved).await.unwrap();
    let changed = revisions(&lease).await;
    assert_eq!(changed.0, recorded.0 + 1);

    lease
        .record_export_conflict(binding.clone(), name.clone(), generation)
        .await
        .unwrap();
    let conflicted = revisions(&lease).await;
    assert_eq!(conflicted.0, changed.0 + 1);
    lease
        .record_export_conflict(binding.clone(), name.clone(), generation)
        .await
        .unwrap();
    assert_eq!(revisions(&lease).await, conflicted);

    let finding = lease.findings_page(0, 1).await.unwrap().remove(0);
    assert_eq!(finding.reason, FindingKind::ExportConflict);
    assert!(lease.remove_finding(finding).await.unwrap());
    let removed = revisions(&lease).await;
    assert_eq!(removed.0, conflicted.0 + 1);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn name_findings_keep_an_export_conflict_of_the_same_incarnation() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    catalog
        .admin(Origin::Api)
        .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
        .await
        .unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&tree.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    let name = ObjectName::new("object").unwrap();
    let generation = publish(&catalog, &data, &bucket, name.as_str()).await;
    lease
        .record_finding(binding.clone(), name.clone(), generation, Reason::Collision)
        .await
        .unwrap();
    lease
        .record_export_conflict(binding.clone(), name.clone(), generation)
        .await
        .unwrap();
    let conflicted = revisions(&lease).await;
    let reason = |lease: &RootLease| {
        let lease = lease.clone();
        async move { lease.findings_page(0, 1).await.unwrap()[0].reason }
    };
    assert_eq!(reason(&lease).await, FindingKind::ExportConflict);

    // The same generation's name finding leaves the conflict and writes nothing.
    for unmapped in [Reason::Collision, Reason::UnsafeEntry, Reason::Ignored] {
        lease
            .record_finding(binding.clone(), name.clone(), generation, unmapped)
            .await
            .unwrap();
        assert_eq!(reason(&lease).await, FindingKind::ExportConflict);
        assert_eq!(revisions(&lease).await, conflicted);
    }

    // A newer generation of the same incarnation does not settle the edit the conflict holds,
    // and a name finding only reports, so the conflict stays as recorded.
    let newer = publish(&catalog, &data, &bucket, name.as_str()).await;
    lease
        .record_finding(binding.clone(), name.clone(), newer, Reason::Collision)
        .await
        .unwrap();
    let finding = lease.findings_page(0, 1).await.unwrap().remove(0);
    assert_eq!(
        (finding.reason, finding.generation),
        (FindingKind::ExportConflict, generation)
    );
    assert_eq!(revisions(&lease).await, conflicted);
    drop(lease);
    catalog.close().await.unwrap();
}

/// A collision carried over from schema 57 gives way to whichever finding observes the object
/// again on the same generation. Once an export conflict is recorded, in either order, a name
/// finding no longer replaces it.
#[tokio::test]
async fn an_unclassified_collision_gives_way_to_the_finding_that_classifies_it() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    catalog
        .admin(Origin::Api)
        .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
        .await
        .unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&tree.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    let name = ObjectName::new("object").unwrap();
    let generation = publish(&catalog, &data, &bucket, name.as_str()).await;
    let reason = || {
        let lease = lease.clone();
        async move { lease.findings_page(0, 1).await.unwrap()[0].reason }
    };
    let collision =
        || lease.record_finding(binding.clone(), name.clone(), generation, Reason::Collision);
    let conflict = || lease.record_export_conflict(binding.clone(), name.clone(), generation);
    collision().await.unwrap();
    let unclassified = async || {
        assert!(
            lease
                .unclassify_finding("test-bucket", "object")
                .await
                .unwrap()
        );
        assert_eq!(reason().await, FindingKind::UnclassifiedCollision);
    };

    unclassified().await;
    collision().await.unwrap();
    assert_eq!(reason().await, FindingKind::Unmapped(Reason::Collision));

    unclassified().await;
    conflict().await.unwrap();
    assert_eq!(reason().await, FindingKind::ExportConflict);
    collision().await.unwrap();
    assert_eq!(reason().await, FindingKind::ExportConflict);

    // A name finding first, then the export conflict the next ingestion records again.
    unclassified().await;
    collision().await.unwrap();
    conflict().await.unwrap();
    assert_eq!(reason().await, FindingKind::ExportConflict);
    drop(lease);
    catalog.close().await.unwrap();
}

#[test]
fn finding_effects_block_export_conflicts_report_names_and_skip_ignored_names() {
    assert_eq!(FindingKind::ExportConflict.effect(), FindingEffect::Blocks);
    assert_eq!(
        FindingKind::UnclassifiedCollision.effect(),
        FindingEffect::Blocks
    );
    assert_eq!(
        FindingKind::Unmapped(Reason::Ignored).effect(),
        FindingEffect::Silent
    );
    for reason in [
        Reason::InvalidName,
        Reason::HostRestriction,
        Reason::Reserved,
        Reason::Collision,
        Reason::UnsafeEntry,
    ] {
        assert_eq!(
            FindingKind::Unmapped(reason).effect(),
            FindingEffect::Reports,
            "{reason:?}"
        );
    }
}
