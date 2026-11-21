use super::*;
use crate::{
    storage::{Catalog, sync_state::root::Activation},
    test_support::TestDir,
};

#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails at the catalog or root operation that could not complete"
)]
async fn fixture() -> (TestDir, TestDir, Catalog, RootLease) {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    (data, tree, catalog, root)
}

#[tokio::test]
async fn observation_revision_rejects_changed_pages_and_overflow_rolls_back() {
    let (_data, _tree, catalog, root) = fixture().await;
    let initial = root.observation().await.unwrap();
    assert_eq!(initial.mapping_findings, 0);
    root.observation_pause(Some(PauseReason::ObserverFailure))
        .await
        .unwrap();
    assert!(matches!(
        root.observation_page(
            initial.revision,
            ObservationCursor::START,
            PageLimit::try_from(1).unwrap()
        )
        .await,
        Err(RootError::Admin(AdminError::Precondition))
    ));
    let paused = root.observation().await.unwrap();
    let items = root
        .observation_page(
            paused.revision,
            ObservationCursor::START,
            PageLimit::try_from(1).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].category, FindingCategory::RootStop);
    assert_eq!(items[0].reason.as_str(), "observer-failure");
    root.observation_pause(None).await.unwrap();
    let cleared = root.observation().await.unwrap();
    assert!(cleared.revision > paused.revision);
    assert!(
        root.observation_page(
            cleared.revision,
            ObservationCursor::START,
            PageLimit::try_from(256).unwrap()
        )
        .await
        .unwrap()
        .is_empty()
    );
    catalog
        .run_admin(|connection| {
            connection
                .execute(
                    "UPDATE sync_observations SET revision=9223372036854775806",
                    [],
                )
                .map(|_| ())
                .map_err(unavailable)
        })
        .await
        .unwrap();
    assert!(
        root.observation_pause(Some(PauseReason::ObserverFailure))
            .await
            .is_err()
    );
    assert!(root.observation().await.unwrap().paused_reason.is_none());
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn mapping_updates_deletes_and_restart_keep_a_monotonic_revision() {
    let (data, tree, catalog, root) = fixture().await;
    let id = root.status().root_id;
    catalog.run_admin(move |connection|connection.execute("INSERT INTO sync_findings(root_id,bucket,name,incarnation,generation,reason,owner_project) VALUES(?1,'bucket','name','00000000000000000000000000000000',1,'collision','project')",[id]).map(|_|()).map_err(unavailable)).await.unwrap();
    let first = root.observation().await.unwrap();
    assert_eq!(first.mapping_findings, 1);
    catalog
        .run_admin(|connection| {
            connection
                .execute("UPDATE sync_findings SET reason='unsafe-entry'", [])
                .map(|_| ())
                .map_err(unavailable)
        })
        .await
        .unwrap();
    assert!(
        root.observation_page(
            first.revision,
            ObservationCursor::START,
            PageLimit::try_from(256).unwrap()
        )
        .await
        .is_err()
    );
    let updated = root.observation().await.unwrap();
    assert_eq!(
        root.observation_page(
            updated.revision,
            ObservationCursor::START,
            PageLimit::try_from(256).unwrap()
        )
        .await
        .unwrap()[0]
            .reason
            .as_str(),
        "unsafe-entry"
    );
    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    assert_eq!(root.observation().await.unwrap().revision, updated.revision);
    catalog
        .run_admin(|connection| {
            connection
                .execute("DELETE FROM sync_findings", [])
                .map(|_| ())
                .map_err(unavailable)
        })
        .await
        .unwrap();
    assert!(root.observation().await.unwrap().revision > updated.revision);
    drop(root);
    catalog.close().await.unwrap();
}

/// The summary counts each stored finding by what its kind does to a wait, as the Rust kind
/// decides it, so the statement and `FindingKind::effect` cannot disagree. Every finding still
/// counts in `mapping_findings`. A name the tree cannot hold counts only while the API holds
/// the object, since a deletion the mirror has not replayed yet leaves its finding behind; an
/// export conflict counts whether or not its object remains.
#[tokio::test]
async fn findings_count_by_what_each_does_to_a_wait() {
    use crate::storage::{
        admin::{ObjectName, Origin},
        test_fixtures::{body, publication, seed_bucket},
    };
    let (_data, _tree, catalog, root) = fixture().await;
    seed_bucket(&catalog).await;
    let bucket = crate::storage::admin::BucketName::new("bucket").unwrap();
    for name in ["a", r"dir\file"] {
        catalog
            .admin(Origin::Api)
            .publish_object(
                &bucket,
                &ObjectName::new(name).unwrap(),
                publication(1024),
                body(vec![bytes::Bytes::from_static(b"live")]),
            )
            .await
            .unwrap();
    }
    let id = root.status().root_id;
    catalog
        .run_admin(move |connection| {
            for (name, reason) in [
                ("edited", "export-conflict"),
                ("deleted", "export-conflict"),
                ("a", "collision"),
                (r"dir\file", "host-restriction"),
                ("gone", "host-restriction"),
                ("x/.DS_Store", "ignored"),
            ] {
                connection
                    .execute(
                        "INSERT INTO sync_findings(root_id,bucket,name,incarnation,generation,reason,owner_project) VALUES(?1,'bucket',?2,'00000000000000000000000000000000',1,?3,'project')",
                        params![id, name, reason],
                    )
                    .map_err(unavailable)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let summary = root.observation().await.unwrap();
    let listed = root.findings_page(0, 256).await.unwrap();
    // The object of `gone` was never there, as after a deletion the mirror has not replayed.
    let counted = |effect| {
        listed
            .iter()
            .filter(|finding| finding.reason.effect() == effect)
            .filter(|finding| effect != FindingEffect::Reports || finding.name.as_str() != "gone")
            .count() as u64
    };
    assert_eq!(
        (
            summary.mapping_findings,
            summary.blocking_findings,
            summary.unmapped_findings
        ),
        (
            listed.len() as u64,
            counted(FindingEffect::Blocks),
            counted(FindingEffect::Reports)
        )
    );
    assert_eq!(
        (
            summary.mapping_findings,
            summary.blocking_findings,
            summary.unmapped_findings
        ),
        (6, 2, 2)
    );
    drop(root);
    catalog.close().await.unwrap();
}

/// A failed export of an object whose finding each ingestion observes again stops its path
/// once: recording the failure again keeps the stop and moves nothing, the tree scan leaves the
/// stop to the finding's observation while it clears another object's, and the observation that
/// no longer fails clears it, writing nothing when there is none.
#[tokio::test]
async fn an_export_failure_of_an_object_with_a_finding_stops_its_path_once() {
    use crate::storage::{
        admin::{Generation, ObjectName, Origin},
        sync_state::Ordinal,
        test_fixtures::{body, publication, seed_bucket},
    };
    let (_data, _tree, catalog, root) = fixture().await;
    seed_bucket(&catalog).await;
    let bucket = crate::storage::admin::BucketName::new("bucket").unwrap();
    let (held, other) = (ObjectName::new("a").unwrap(), ObjectName::new("b").unwrap());
    let record = catalog
        .admin(Origin::Api)
        .publish_object(
            &bucket,
            &held,
            publication(1024),
            body(vec![bytes::Bytes::from_static(b"live")]),
        )
        .await
        .unwrap();
    let binding = root.sync_binding(&bucket).await.unwrap();
    let generation = Generation::new(record.generation).unwrap();
    root.record_finding(
        binding.clone(),
        held.clone(),
        generation,
        crate::sync::mapping::Obstruction::Collision.into(),
    )
    .await
    .unwrap();
    let stopped = || async {
        let listed = root.first_path_stops(8).await.unwrap();
        let transient = root.transient_stop_page(Ordinal::START).await.unwrap();
        let transient: Vec<_> = transient.into_iter().map(|stop| stop.name).collect();
        (listed, transient)
    };
    let revision = || async { root.observation().await.unwrap().revision };
    let failed = |name: &ObjectName| root.record_export_failure(binding.clone(), name.clone());

    assert_eq!(failed(&held).await.unwrap(), Transition::New);
    root.record_path_stop(binding.clone(), other.clone(), PathStop::IoFailure)
        .await
        .unwrap();
    let recorded = revision().await;
    assert_eq!(failed(&held).await.unwrap(), Transition::Kept);
    assert_eq!(revision().await, recorded);
    let stop = |name: &str| ListedStop {
        path: format!("bucket/{name}"),
        reason: PathStop::IoFailure,
    };
    assert_eq!(stopped().await, (vec![stop("a"), stop("b")], vec![other]));

    root.clear_export_failure(binding.clone(), held.clone())
        .await
        .unwrap();
    let cleared = revision().await;
    assert_ne!(cleared, recorded);
    assert_eq!(stopped().await.0, [stop("b")]);
    root.clear_export_failure(binding.clone(), held.clone())
        .await
        .unwrap();
    assert_eq!(revision().await, cleared);
    drop(root);
    catalog.close().await.unwrap();
}

/// The observation revision is the `findings_revision` a client pages the findings view by, so
/// it stays where it is while nothing that view lists changes. A late directory notification
/// invalidates the deletion set, and the next inventory confirms or replaces it; a full pass
/// completes the baseline again; tracked files change on every synchronized save. None of that
/// is in the view, and a page a wait had just named must stay readable through it. A scan that
/// records the stop already recorded changes nothing either, and one that records another does.
#[tokio::test]
async fn the_findings_revision_moves_only_with_what_the_findings_view_lists() {
    let (_data, _tree, catalog, root) = fixture().await;
    crate::storage::test_fixtures::seed_bucket(&catalog).await;
    root.observation_scan(Some(ScanStop::SourceChanging))
        .await
        .unwrap();
    let start = root.observation().await.unwrap().revision;
    let unchanged = |step: &str| {
        let root = root.clone();
        let step = step.to_owned();
        async move {
            let now = root.observation().await.unwrap().revision;
            assert_eq!(now, start, "{step}");
            assert!(
                root.observation_page(
                    start,
                    ObservationCursor::START,
                    PageLimit::try_from(256).unwrap()
                )
                .await
                .is_ok(),
                "{step}"
            );
        }
    };
    let id = root.status().root_id;
    let execute = |statement: &'static str| {
        catalog.run_admin(move |connection| {
            connection
                .execute(statement, [id])
                .map(|_| ())
                .map_err(unavailable)
        })
    };
    let track = |statement: &'static str| {
        catalog
            .run_admin(move |connection| connection.execute_batch(statement).map_err(unavailable))
    };

    root.observation_scan(Some(ScanStop::SourceChanging))
        .await
        .unwrap();
    unchanged("the same scan stop").await;
    root.complete_baseline().await.unwrap();
    root.complete_baseline().await.unwrap();
    unchanged("baseline completion").await;
    execute("INSERT INTO sync_deletion_sets(root_id,revision,scan_revision,sequence,epoch,execution_sequence,baseline_count,candidate_count,guard_enabled,guarded,complete,valid,authorization) VALUES(?1,'dddddddddddddddddddddddddddddddd','eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',0,0,0,1,1,1,1,1,1,0)").await.unwrap();
    execute("INSERT INTO sync_deletion_members(root_id,ordinal,bucket,name,owner_project,incarnation,generation,path,done) VALUES(?1,1,'bucket','gone','project','00000000000000000000000000000000',1,'bucket/gone',0)").await.unwrap();
    unchanged("a new deletion set").await;
    root.invalidate_deletions().await.unwrap();
    unchanged("an invalidated deletion set").await;
    execute("UPDATE sync_deletion_sets SET valid=1 WHERE root_id=?1 AND valid=0")
        .await
        .unwrap();
    execute("UPDATE sync_deletion_members SET done=1 WHERE root_id=?1")
        .await
        .unwrap();
    execute("DELETE FROM sync_deletion_sets WHERE root_id=?1")
        .await
        .unwrap();
    unchanged("a confirmed, completed and replaced deletion set").await;
    track("INSERT INTO sync_objects VALUES('bucket','tracked','00000000000000000000000000000000','bucket/tracked',1,0,zeroblob(16),zeroblob(16),zeroblob(8),zeroblob(16),0,0); UPDATE sync_objects SET generation=2; DELETE FROM sync_objects;").await.unwrap();
    unchanged("a tracked file").await;

    root.observation_scan(None).await.unwrap();
    let cleared = root.observation().await.unwrap().revision;
    assert!(cleared > start);
    execute("INSERT INTO sync_findings(root_id,bucket,name,incarnation,generation,reason,owner_project) VALUES(?1,'bucket','name','00000000000000000000000000000000',1,'collision','project')").await.unwrap();
    assert!(root.observation().await.unwrap().revision > cleared);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn observation_schema_migrates_existing_roots_and_resets_with_catalog() {
    let (data, tree, catalog, root) = fixture().await;
    drop(root);
    catalog
        .run_admin(|connection| {
            super::super::super::test_support::downgrade_observation_schema(connection);
            connection.pragma_update(None, "user_version", 40).unwrap();
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    assert_eq!(root.observation().await.unwrap().revision, 0);
    drop(root);
    catalog
        .admin(crate::storage::admin::Origin::Api)
        .reset()
        .await
        .unwrap();
    catalog.close().await.unwrap();
    Catalog::open(&data.0).await.unwrap().close().await.unwrap();
}

/// Schema 59 only drops triggers: a schema 58 catalog keeps its revision, and afterwards a
/// deletion set, the baseline flag and a tracked file change it no more, while a finding still
/// does.
#[tokio::test]
async fn schema_59_keeps_the_revision_and_stops_following_what_the_findings_view_omits() {
    use crate::storage::catalog::{
        test_support::downgrade_observation_revision_schema, validate_schema,
    };
    let (data, tree, catalog, root) = fixture().await;
    root.observation_pause(Some(PauseReason::ObserverFailure))
        .await
        .unwrap();
    let before = root.observation().await.unwrap().revision;
    drop(root);
    catalog
        .run_admin(|connection| {
            downgrade_observation_revision_schema(connection);
            connection.pragma_update(None, "user_version", 58).unwrap();
            validate_schema(connection, 58).unwrap();
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();

    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    assert_eq!(root.observation().await.unwrap().revision, before);
    root.complete_baseline().await.unwrap();
    let id = root.status().root_id;
    catalog
        .run_admin(move |connection| {
            connection
                .execute("INSERT INTO sync_deletion_sets(root_id,revision,scan_revision,sequence,epoch,execution_sequence,baseline_count,candidate_count,guard_enabled,guarded,complete,valid,authorization) VALUES(?1,'dddddddddddddddddddddddddddddddd','eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',0,0,0,1,1,1,1,1,1,0)", [id])
                .map_err(unavailable)?;
            connection
                .execute_batch("INSERT INTO sync_objects VALUES('bucket','tracked','00000000000000000000000000000000','bucket/tracked',1,0,zeroblob(16),zeroblob(16),zeroblob(8),zeroblob(16),0,0);")
                .map_err(unavailable)
        })
        .await
        .unwrap();
    assert_eq!(root.observation().await.unwrap().revision, before);
    root.observation_pause(None).await.unwrap();
    assert!(root.observation().await.unwrap().revision > before);
    drop(root);
    catalog.close().await.unwrap();
}
