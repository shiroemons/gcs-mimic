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
/// counts in `mapping_findings`.
#[tokio::test]
async fn findings_count_by_what_each_does_to_a_wait() {
    let (_data, _tree, catalog, root) = fixture().await;
    let id = root.status().root_id;
    catalog
        .run_admin(move |connection| {
            for (name, reason) in [
                ("edited", "export-conflict"),
                ("deleted", "export-conflict"),
                ("a", "collision"),
                (r"dir\file", "host-restriction"),
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
    let counted = |effect| {
        listed
            .iter()
            .filter(|finding| finding.reason.effect() == effect)
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
        (5, 2, 2)
    );
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
