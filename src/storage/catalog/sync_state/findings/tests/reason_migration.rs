//! Schema 58 rebuilds the finding and path-stop tables while foreign keys stay on.
use super::*;
use crate::storage::catalog::{
    CATALOG_SCHEMA_VERSION, Migration, test_support::downgrade_finding_reason_schema,
    validate_schema, validate_schema_version,
};
use crate::storage::sync_state::{
    observation::{FindingCategory, ObservationCursor, PathStop},
    root::Activation,
    writeback::Limits,
};

/// Projects that only retained synchronization state still names once the bucket belongs to
/// `new-owner`.
const RETAINED_OWNERS: [&str; 3] = ["old-owner", "writeback-owner", "conflict-owner"];

type FindingRow = (i64, String, String, i64, String, Option<String>);
type OwnerRow = (i64, String, Option<String>);
type StopRow = (i64, String, String, i64, String);

/// Everything schema 58 rebuilds or must leave alone, in a comparable form.
#[derive(Debug, PartialEq)]
struct Rows {
    findings: Vec<FindingRow>,
    owners: Vec<OwnerRow>,
    stops: Vec<StopRow>,
    /// Pending write-back operations, conflicts, deletion sets and deletion members.
    retained: [i64; 4],
    sequences: Vec<(String, i64)>,
    revision: i64,
}

fn rows(connection: &Connection) -> Rows {
    fn all<T>(
        connection: &Connection,
        query: &str,
        read: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Vec<T> {
        connection
            .prepare(query)
            .unwrap()
            .query_map([], read)
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
    Rows {
        findings: all(
            connection,
            "SELECT ordinal,name,incarnation,generation,reason,owner_project FROM sync_findings ORDER BY ordinal",
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        ),
        owners: all(
            connection,
            "SELECT ordinal,incarnation,owner_project FROM sync_finding_owners ORDER BY ordinal,incarnation",
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ),
        stops: all(
            connection,
            "SELECT ordinal,name,owner_project,generation,reason FROM sync_path_stops ORDER BY ordinal",
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        ),
        retained: connection
            .query_row(
                "SELECT (SELECT count(*) FROM sync_writeback_ops WHERE phase!=4),(SELECT count(*) FROM sync_conflicts WHERE state!=2),(SELECT count(*) FROM sync_deletion_sets),(SELECT count(*) FROM sync_deletion_members WHERE done=0)",
                [],
                |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?]),
            )
            .unwrap(),
        sequences: all(
            connection,
            "SELECT name,seq FROM sqlite_sequence WHERE name IN ('sync_findings','sync_path_stops') ORDER BY name",
            |row| Ok((row.get(0)?, row.get(1)?)),
        ),
        revision: connection
            .query_row("SELECT revision FROM sync_observations", [], |row| {
                row.get(0)
            })
            .unwrap(),
    }
}

fn user_version(connection: &Connection) -> i64 {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

/// What schema 58 makes of `before`: every `collision` row becomes an unclassified collision.
fn migrated(mut before: Rows) -> Rows {
    for finding in &mut before.findings {
        if finding.4 == "collision" {
            finding.4 = "unclassified-collision".into();
        }
    }
    before
}

/// A schema 57 catalog with findings owned by two bucket incarnations, an export conflict and
/// a name collision (both `collision` there), an ignored name, all three path stops, pending
/// write-back work, a retained conflict and a deletion set. The highest finding and path-stop
/// ordinals were removed, so only `sqlite_sequence` remembers them.
async fn schema_57_fixture() -> (TestDir, TestDir) {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    let admin = catalog.admin(Origin::Api);
    admin
        .ensure_bucket(BucketDefinition::default_for(&bucket, "old-owner", &SystemClock).unwrap())
        .await
        .unwrap();
    let reused = ObjectName::new("reused").unwrap();
    let first = publish(&catalog, &data, &bucket, reused.as_str()).await;
    lease
        .record_finding(
            catalog.sync_state().binding(&bucket).await.unwrap(),
            reused.clone(),
            first,
            Reason::InvalidName,
        )
        .await
        .unwrap();
    admin
        .delete_object(&bucket, &reused, GenerationCondition::Is(first))
        .await
        .unwrap();
    catalog
        .delete_bucket(bucket.as_str(), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    admin
        .ensure_bucket(BucketDefinition::default_for(&bucket, "new-owner", &SystemClock).unwrap())
        .await
        .unwrap();
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    let second = publish(&catalog, &data, &bucket, reused.as_str()).await;
    lease
        .record_finding(binding.clone(), reused, second, Reason::InvalidName)
        .await
        .unwrap();
    for (name, reason) in [
        ("edited", None),
        ("collision", Some(Reason::Collision)),
        ("notes.swp", Some(Reason::Ignored)),
        ("removed", Some(Reason::Reserved)),
    ] {
        let generation = publish(&catalog, &data, &bucket, name).await;
        let name = ObjectName::new(name).unwrap();
        match reason {
            Some(reason) => lease
                .record_finding(binding.clone(), name, generation, reason)
                .await
                .unwrap(),
            None => lease
                .record_export_conflict(binding.clone(), name, generation)
                .await
                .unwrap(),
        }
    }
    let removed = lease.findings_page(0, 256).await.unwrap().pop().unwrap();
    assert_eq!(removed.name, "removed");
    assert!(lease.remove_finding(removed).await.unwrap());
    for (name, stop) in [
        ("protected", PathStop::ProtectedDeletion),
        ("changing", PathStop::SourceChanging),
        ("failing", PathStop::IoFailure),
        ("cleared", PathStop::IoFailure),
    ] {
        lease
            .record_path_stop(binding.clone(), ObjectName::new(name).unwrap(), stop)
            .await
            .unwrap();
    }
    lease
        .clear_path_stops(
            binding,
            ObjectName::new("cleared").unwrap(),
            &[PathStop::IoFailure],
        )
        .await
        .unwrap();
    drop(
        lease
            .writeback(Limits::new(1024, 10).unwrap())
            .await
            .unwrap(),
    );
    let root = lease.status().root_id;
    catalog
        .run_admin(move |connection| {
            for statement in [
                "INSERT INTO sync_writeback_ops(ordinal,id,root_id,owner,bucket,incarnation,name,path,generation,kind,phase,revision,fence_sequence,fence_epoch,reservation,size,md5,crc32c) VALUES(1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',?1,'writeback-owner','test-bucket','00000000000000000000000000000000','pending','test-bucket/pending',1,0,0,1,0,0,zeroblob(8),0,zeroblob(16),0)",
                "INSERT INTO sync_conflicts(ordinal,id,root_id,revision,owner,bucket,incarnation,name,generation,state,size,source) VALUES(1,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',?1,'cccccccccccccccccccccccccccccccc','conflict-owner','test-bucket','00000000000000000000000000000000','retained',1,1,0,x'')",
                "INSERT INTO sync_deletion_sets(root_id,revision,scan_revision,sequence,epoch,execution_sequence,baseline_count,candidate_count,guard_enabled,guarded,complete,valid,authorization) VALUES(?1,'dddddddddddddddddddddddddddddddd','eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',0,0,0,1,1,1,1,1,1,0)",
                "INSERT INTO sync_deletion_members(root_id,ordinal,bucket,name,owner_project,incarnation,generation,path,done) VALUES(?1,1,'test-bucket','gone','deletion-owner','00000000000000000000000000000000',1,'test-bucket/gone',0)",
            ] {
                connection.execute(statement, [root]).map_err(unavailable)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    drop(lease);
    catalog.close().await.unwrap();
    let connection = Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    downgrade_finding_reason_schema(&connection);
    connection.pragma_update(None, "user_version", 57).unwrap();
    validate_schema(&connection, 57).unwrap();
    (data, tree)
}

#[tokio::test]
async fn schema_58_keeps_owners_ordinals_and_retained_work_and_marks_collisions_unclassified() {
    let (data, tree) = schema_57_fixture().await;
    let connection = Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    let before = rows(&connection);
    assert_eq!(
        before
            .findings
            .iter()
            .map(|finding| (finding.1.as_str(), finding.4.as_str()))
            .collect::<Vec<_>>(),
        [
            ("reused", "invalid-name"),
            ("edited", "collision"),
            ("collision", "collision"),
            ("notes.swp", "ignored"),
        ]
    );
    assert_eq!(before.owners.len(), 5);
    assert_eq!(
        before
            .stops
            .iter()
            .map(|stop| stop.4.as_str())
            .collect::<Vec<_>>(),
        ["ProtectedDeletion", "SourceChanging", "IoFailure"]
    );
    let finding_sequence = before.sequences[0].1;
    let stop_sequence = before.sequences[1].1;
    assert!(finding_sequence > before.findings.last().unwrap().0);
    assert!(stop_sequence > before.stops.last().unwrap().0);
    drop(connection);

    let catalog = Catalog::open(&data.0).await.unwrap();
    let after = catalog
        .run_admin(|connection| {
            assert_eq!(user_version(connection), 58);
            let broken: i64 = connection
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(broken, 0);
            Ok(rows(connection))
        })
        .await
        .unwrap();
    // Recovery at open marks the pending write-back and invalidates the deletion set, which
    // advances the observation revision; the migration alone leaves it, as the rollback test
    // shows by migrating without opening.
    assert_eq!(before.revision + 2, after.revision);
    assert_eq!(
        after,
        Rows {
            revision: after.revision,
            ..migrated(before)
        }
    );

    let lease = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    let page = lease.findings_page(0, 256).await.unwrap();
    assert_eq!(
        page.iter()
            .map(|finding| (finding.ordinal, finding.reason))
            .collect::<Vec<_>>(),
        after
            .findings
            .iter()
            .zip([
                FindingKind::Unmapped(Reason::InvalidName),
                FindingKind::UnclassifiedCollision,
                FindingKind::UnclassifiedCollision,
                FindingKind::Unmapped(Reason::Ignored),
            ])
            .map(|(finding, kind)| (finding.0, kind))
            .collect::<Vec<_>>()
    );
    // The status findings view reads the rebuilt rows through the same codecs.
    let items = lease
        .observation_page(
            lease.observation().await.unwrap().revision,
            ObservationCursor::START,
            PageLimit::try_from(256).unwrap(),
        )
        .await
        .unwrap();
    let reasons = |category| {
        items
            .iter()
            .filter(|item| item.category == category)
            .map(|item| item.reason.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        reasons(FindingCategory::Mapping),
        [
            "invalid-name",
            "unclassified-collision",
            "unclassified-collision",
            "ignored"
        ]
    );
    assert_eq!(
        reasons(FindingCategory::PathStop),
        ["ProtectedDeletion", "SourceChanging", "IoFailure"]
    );
    let all_owners = [
        "seed",
        "new-owner",
        RETAINED_OWNERS[0],
        RETAINED_OWNERS[1],
        RETAINED_OWNERS[2],
    ];
    lease
        .authorize_controls("seed".into(), Some(control_identity(&all_owners).await))
        .await
        .unwrap();
    for missing in RETAINED_OWNERS {
        let projects: Vec<&str> = all_owners
            .into_iter()
            .filter(|project| *project != missing)
            .collect();
        assert!(
            matches!(
                lease
                    .authorize_controls("seed".into(), Some(control_identity(&projects).await))
                    .await,
                Err(RootError::Admin(AdminError::Forbidden))
            ),
            "{missing}"
        );
    }

    let bucket = BucketName::new("test-bucket").unwrap();
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    let fresh = ObjectName::new("fresh").unwrap();
    let generation = publish(&catalog, &data, &bucket, fresh.as_str()).await;
    let start = revisions(&lease).await;
    lease
        .record_finding(binding.clone(), fresh.clone(), generation, Reason::Reserved)
        .await
        .unwrap();
    let recorded = revisions(&lease).await;
    assert_eq!(recorded.0, start.0 + 1);
    assert!(recorded.1 > start.1);
    lease
        .record_path_stop(binding, fresh, PathStop::IoFailure)
        .await
        .unwrap();
    assert!(lease.observation().await.unwrap().revision > recorded.1);
    let (new_finding, new_stop) = catalog
        .run_admin(|connection| {
            Ok(connection
                .query_row(
                    "SELECT (SELECT max(ordinal) FROM sync_findings),(SELECT max(ordinal) FROM sync_path_stops)",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap())
        })
        .await
        .unwrap();
    assert_eq!(new_finding, finding_sequence + 1);
    assert_eq!(new_stop, stop_sequence + 1);
    drop(lease);
    catalog.close().await.unwrap();

    // The next open validates the stored catalog against schema 58.
    Catalog::open(&data.0).await.unwrap().close().await.unwrap();
    assert!(matches!(
        validate_schema_version(CATALOG_SCHEMA_VERSION, 57),
        Err(CatalogError::NewerSchema {
            version: 58,
            supported: 57
        })
    ));
}

#[tokio::test]
async fn schema_58_migration_rolls_back_failed_attempts_and_then_upgrades() {
    let (data, _tree) = schema_57_fixture().await;
    let mut connection = Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    let before = rows(&connection);
    let expected = migrated(rows(&connection));
    let attempt = |connection: &mut Connection| {
        Migration::Function(super::super::migrate_reasons).apply(connection)
    };
    for injection in [
        // A statement fails after both tables were dropped and rebuilt.
        "CREATE TEMP TRIGGER injected AFTER INSERT ON sync_finding_owners BEGIN SELECT RAISE(ABORT,'injected owner restore failure'); END;",
        // Every statement succeeds, but an owner does not come back.
        "CREATE TEMP TRIGGER injected AFTER INSERT ON sync_finding_owners WHEN NEW.owner_project='old-owner' BEGIN DELETE FROM sync_finding_owners WHERE ordinal=NEW.ordinal AND incarnation=NEW.incarnation; END;",
    ] {
        connection.execute_batch(injection).unwrap();
        assert!(attempt(&mut connection).is_err(), "{injection}");
        assert_eq!(user_version(&connection), 57);
        validate_schema(&connection, 57).unwrap();
        assert_eq!(rows(&connection), before);
        connection
            .execute_batch("DROP TRIGGER temp.injected;")
            .unwrap();
    }
    // With foreign keys off, an owner whose finding is gone is restored as it was; the check
    // after the rebuild refuses it.
    connection
        .execute_batch("PRAGMA foreign_keys=OFF; INSERT INTO sync_finding_owners VALUES(999,'00000000000000000000000000000000','orphan');")
        .unwrap();
    let error = attempt(&mut connection).unwrap_err();
    assert!(
        error.to_string().contains("without its parent row"),
        "{error}"
    );
    assert_eq!(user_version(&connection), 57);
    validate_schema(&connection, 57).unwrap();
    connection
        .execute_batch("DELETE FROM sync_finding_owners WHERE ordinal=999; PRAGMA foreign_keys=ON;")
        .unwrap();
    assert_eq!(rows(&connection), before);

    attempt(&mut connection).unwrap();
    assert_eq!(user_version(&connection), 58);
    validate_schema(&connection, 58).unwrap();
    assert_eq!(rows(&connection), expected);
    let leftovers: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_temp_schema WHERE type='table'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0);
}
