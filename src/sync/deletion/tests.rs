#![expect(
    clippy::unwrap_used,
    reason = "Invalid test fixtures and failed operations must fail at their call sites"
)]

use super::*;
use crate::storage::admin::{BucketName, ObjectName};
use crate::storage::sync_state::deletions::Refusal;
use crate::storage::test_fixtures::{PERMISSIVE, live};
use crate::sync::Reconciliation;
use crate::{
    storage::{admin::Origin, sync_state::root::Activation},
    test_support::TestDir,
};
use std::fs;

mod awaiting;

async fn fixture(count: usize) -> (TestDir, TestDir, Catalog, RootLease, Config) {
    let data = TestDir::new();
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for index in 0..count {
        fs::write(tree.0.join(format!("test-bucket/{index:04}")), b"immutable").unwrap();
    }
    let catalog = Catalog::open(&data.0).await.unwrap();
    catalog.enable_change_journal().await.unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    let root = crate::sync::test_support::probed(root).await;
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let scanned = crate::sync::ingest::rescan(&catalog, &root, &config, &stopping)
        .await
        .unwrap();
    assert_eq!(scanned.ingested, count as u64);
    root.complete_baseline().await.unwrap();
    // A started mirror inventories once with every file in place. That first inventory is the
    // start which a bucket emptied while the server was stopped is told apart from, so the tests
    // below delete files from a mirror that is already running.
    let started = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let first = inventory(&root, &config, started.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.candidate_count, 0);
    (data, tree, catalog, root, config)
}

#[tokio::test]
async fn cumulative_deletions_require_approval_after_split_scans_rescan_and_restart() {
    let (data, tree, catalog, root, config) = fixture(100).await;
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    for index in 0..12 {
        fs::remove_file(tree.0.join(format!("test-bucket/{index:04}"))).unwrap();
    }
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert!(!set.guarded);
    assert_eq!(execute(&catalog, &root, set, &stopping).await.unwrap(), 12);
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 12);
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.candidate_count, 88);
    assert!(set.guarded);
    root.reset_empty_deletion_history().await.unwrap();
    root.complete_baseline().await.unwrap();
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 12);
    assert_eq!(execute(&catalog, &root, set, &stopping).await.unwrap(), 0);
    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    catalog.enable_change_journal().await.unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    let root = crate::sync::test_support::probed(root).await;
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert!(set.guarded);
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 12);
    let approved = root.approve_deletions(set.revision).await.unwrap();
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 0);
    assert_eq!(
        execute(&catalog, &root, approved, &stopping).await.unwrap(),
        88
    );
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 0);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn zero_candidate_inventory_can_clear_automatic_deletion_history() {
    let (_data, tree, catalog, root, config) = fixture(1).await;
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(execute(&catalog, &root, set, &stopping).await.unwrap(), 1);
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 1);
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.candidate_count, 0);
    root.reset_empty_deletion_history().await.unwrap();
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 0);
    drop(root);
    catalog.close().await.unwrap();
}

/// The log names the first deleted paths in set order and counts the rest, so a deletion the
/// user did not expect, such as one made while the server was stopped, can be traced to its
/// files without the log growing with the set.
#[tokio::test]
async fn applied_deletions_name_the_first_paths_and_count_the_rest() {
    let (_data, tree, catalog, root, config) = fixture(100).await;
    for index in 0..12 {
        fs::remove_file(tree.0.join(format!("test-bucket/{index:04}"))).unwrap();
    }
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    let revision = set.revision;
    let applied = execute_with(&catalog, &root, set, Interrupt::new(&stopping))
        .await
        .unwrap();
    assert_eq!((applied.paths.count(), applied.revision), (12, revision));
    assert_eq!(
        applied.paths.names,
        (0..Named::<String>::LIMIT)
            .map(|index| format!("test-bucket/{index:04}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(applied.paths.omitted, 2);
    drop(root);
    catalog.close().await.unwrap();
}

/// An inventory gathers the paths it could not observe into one bounded warning instead of one
/// for each path on every pass, and keeps each path out of the set as before.
#[tokio::test]
async fn an_inventory_names_the_first_unobservable_paths_and_counts_the_rest() {
    let (_data, tree, catalog, root, _config) = fixture(12).await;
    for index in 0..12 {
        let path = tree.0.join(format!("test-bucket/{index:04}"));
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
    }
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let mut inventory = root
        .begin_deletion_inventory(Guard::Enabled)
        .await
        .unwrap()
        .expect_started();
    let mut unobserved = Unobserved::default();
    let counted = count_candidates(
        &root,
        &mut inventory,
        Interrupt::new(&stopping),
        &mut unobserved,
    )
    .await
    .unwrap();
    assert_eq!(counted, Some(Confirmed::Same));
    assert_eq!(unobserved.count, 12);
    assert_eq!(
        unobserved
            .first
            .iter()
            .map(|path| (path.path.as_str(), path.reason.as_str()))
            .collect::<Vec<_>>(),
        (0..Unobserved::NAMED)
            .map(|index| format!("test-bucket/{index:04}"))
            .collect::<Vec<_>>()
            .iter()
            .map(|path| (path.as_str(), Unobservable::EntryReplaced.as_str()))
            .collect::<Vec<_>>()
    );
    let set = root
        .finish_deletion_inventory(inventory)
        .await
        .unwrap()
        .expect_set();
    assert_eq!(set.candidate_count, 0);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn expired_wait_neither_replaces_a_deletion_revision_nor_deletes_a_candidate() {
    let (_data, tree, catalog, root, config) = fixture(1).await;
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    let deadline = Interrupt {
        stopping: &stopping,
        deadline: Some(tokio::time::Instant::now()),
    };
    assert!(matches!(
        inventory_with(&root, &config, summary.gate(), deadline)
            .await
            .unwrap(),
        InventoryOutcome::Interrupted
    ));
    assert_eq!(
        root.deletion_set().await.unwrap().unwrap().revision,
        set.revision
    );
    assert_eq!(
        execute_with(&catalog, &root, set, deadline)
            .await
            .unwrap()
            .paths
            .count(),
        0
    );
    assert!(
        catalog
            .admin(Origin::Api)
            .get_object(
                &BucketName::new("test-bucket").unwrap(),
                &ObjectName::new("0000").unwrap(),
                None
            )
            .await
            .is_ok()
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn protected_external_deletion_restores_bytes_and_versioned_removal_keeps_history() {
    let (_data, tree, catalog, root, config) = fixture(1).await;
    let bucket = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("0000").unwrap();
    let original = catalog
        .admin(Origin::Api)
        .get_object(&bucket, &name, None)
        .await
        .unwrap();
    catalog
        .update_object_metadata(
            live("test-bucket", "0000"),
            Default::default(),
            crate::object::ObjectMetadataUpdate {
                temporary_hold: Some(true),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(execute(&catalog, &root, set, &stopping).await.unwrap(), 0);
    assert_eq!(
        fs::read(tree.0.join("test-bucket/0000")).unwrap(),
        b"immutable"
    );
    let protected = catalog
        .admin(Origin::Api)
        .get_object(&bucket, &name, None)
        .await
        .unwrap();
    assert_eq!(protected.generation, original.generation);
    assert_eq!(protected.temporary_hold.as_bool(), Some(true));
    let observation = root.observation().await.unwrap();
    assert_eq!(observation.path_stops, 1);
    assert_eq!(
        root.observation_page(
            observation.revision,
            crate::storage::sync_state::observation::ObservationCursor::START,
            PageLimit::try_from(256).unwrap()
        )
        .await
        .unwrap()[0]
            .reason
            .as_str(),
        "ProtectedDeletion"
    );
    catalog
        .update_object_metadata(
            live("test-bucket", "0000"),
            Default::default(),
            crate::object::ObjectMetadataUpdate {
                temporary_hold: Some(false),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    catalog
        .patch_bucket(
            "test-bucket",
            crate::bucket::BucketMetadataUpdate {
                versioning: Some(crate::bucket::VersioningUpdate::Set(
                    crate::bucket::Versioning::Enabled,
                )),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(execute(&catalog, &root, set, &stopping).await.unwrap(), 1);
    assert_eq!(root.observation().await.unwrap().path_stops, 0);
    assert!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await
            .is_err()
    );
    let archived = catalog
        .admin(Origin::Api)
        .get_object(
            &bucket,
            &name,
            Some(crate::storage::admin::Generation::new(original.generation).unwrap()),
        )
        .await
        .unwrap();
    assert!(archived.time_deleted.is_some());
    assert_eq!(archived.md5, original.md5);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn unsafe_source_and_missing_marker_never_produce_a_delete() {
    let (_data, tree, catalog, root, config) = fixture(1).await;
    let outside = TestDir::new();
    fs::write(outside.0.join("preserved"), b"outside").unwrap();
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    std::os::unix::fs::symlink(outside.0.join("preserved"), tree.0.join("test-bucket/0000"))
        .unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    // The link tells nothing about the tracked file, so it is no candidate; it stops only its
    // own path, and the inventory itself completes.
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((set.baseline_count, set.candidate_count), (1, 0));
    assert_eq!(execute(&catalog, &root, set, &stopping).await.unwrap(), 0);
    let observation = root.observation().await.unwrap();
    assert_eq!(observation.path_stops, 1);
    assert_eq!(
        root.observation_page(
            observation.revision,
            crate::storage::sync_state::observation::ObservationCursor::START,
            PageLimit::try_from(256).unwrap()
        )
        .await
        .unwrap()[0]
            .reason
            .as_str(),
        "EntryReplaced"
    );
    assert_eq!(fs::read(outside.0.join("preserved")).unwrap(), b"outside");
    assert!(
        fs::symlink_metadata(tree.0.join("test-bucket/0000"))
            .unwrap()
            .is_symlink()
    );
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    fs::remove_file(tree.0.join(mapping::MARKER)).unwrap();
    assert!(
        inventory(&root, &config, summary.gate(), &stopping)
            .await
            .is_err()
    );
    assert!(
        catalog
            .admin(Origin::Api)
            .get_object(
                &BucketName::new("test-bucket").unwrap(),
                &ObjectName::new("0000").unwrap(),
                None
            )
            .await
            .is_ok()
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn frozen_root_inventory_pages_and_one_shot_approval_consume_exact_members() {
    let (_data, tree, catalog, root, config) = fixture(300).await;
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((set.baseline_count, set.candidate_count), (300, 300));
    assert!(set.guarded);
    assert_eq!(
        execute(&catalog, &root, set.clone(), &stopping)
            .await
            .unwrap(),
        0
    );
    let first = root
        .deletion_page(set.revision, Ordinal::START, PageLimit::LARGEST)
        .await
        .unwrap();
    let second = root
        .deletion_page(
            set.revision,
            first.last().unwrap().ordinal,
            PageLimit::LARGEST,
        )
        .await
        .unwrap();
    assert_eq!((first.len(), second.len()), (256, 44));
    assert_eq!(first[0].name.as_str(), "0000");
    assert_eq!(second.last().unwrap().name.as_str(), "0299");
    let approved = root.approve_deletions(set.revision).await.unwrap();
    assert!(root.approve_deletions(set.revision).await.is_err());
    assert_eq!(
        execute(&catalog, &root, approved, &stopping).await.unwrap(),
        300
    );
    assert!(tree.0.join("test-bucket").is_dir());
    assert_eq!(
        root.deletion_set().await.unwrap().unwrap().completed_count,
        300
    );
    assert!(
        catalog
            .admin(Origin::Api)
            .get_bucket(&BucketName::new("test-bucket").unwrap())
            .await
            .unwrap()
            .is_some()
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn incomplete_scan_recreated_file_and_api_change_never_authorize_an_old_delete() {
    let (_data, tree, catalog, root, config) = fixture(1).await;
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let mut summary = Reconciliation {
        baseline_complete: true,
        deferred: 1,
        ..Default::default()
    };
    assert!(
        inventory(&root, &config, summary.gate(), &stopping)
            .await
            .unwrap()
            .is_none()
    );
    summary.deferred = 0;
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert!(!set.guarded);
    fs::write(tree.0.join("test-bucket/0000"), b"recreated externally").unwrap();
    assert_eq!(
        execute(&catalog, &root, set.clone(), &stopping)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        fs::read(tree.0.join("test-bucket/0000")).unwrap(),
        b"recreated externally"
    );
    assert!(
        root.deletion_page(set.revision, Ordinal::START, PageLimit::LARGEST)
            .await
            .is_err()
    );
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    catalog
        .patch_bucket(
            "test-bucket",
            crate::bucket::BucketMetadataUpdate {
                versioning: Some(crate::bucket::VersioningUpdate::Set(
                    crate::bucket::Versioning::Enabled,
                )),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(execute(&catalog, &root, set, &stopping).await.unwrap(), 0);
    assert!(
        catalog
            .admin(Origin::Api)
            .get_object(
                &BucketName::new("test-bucket").unwrap(),
                &ObjectName::new("0000").unwrap(),
                None
            )
            .await
            .is_ok()
    );
    drop(root);
    catalog.close().await.unwrap();
}

async fn object_exists(catalog: &Catalog, name: &str) -> bool {
    match catalog
        .admin(Origin::Api)
        .get_object(
            &BucketName::new("test-bucket").unwrap(),
            &ObjectName::new(name).unwrap(),
            None,
        )
        .await
    {
        Ok(_) => true,
        Err(AdminError::NotFound) => false,
        Err(error) => panic!("{error}"),
    }
}

/// A tracked path the tree cannot show keeps its own object and nothing else: the file missing
/// beside it is still deleted, and the inventory completes rather than failing the pass.
#[tokio::test]
async fn an_unobservable_tracked_path_keeps_its_object_without_holding_back_other_deletions() {
    use std::os::unix::fs::PermissionsExt;
    let (_data, tree, catalog, root, config) = fixture(2).await;
    let sealed = tree.0.join("test-bucket/sealed");
    fs::create_dir(&sealed).unwrap();
    fs::write(sealed.join("inside"), b"inside").unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    crate::sync::ingest::rescan(&catalog, &root, &config, &stopping)
        .await
        .unwrap();
    assert!(object_exists(&catalog, "sealed/inside").await);
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).unwrap();
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let inventoried = inventory(&root, &config, summary.gate(), &stopping).await;
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o700)).unwrap();
    let set = inventoried.unwrap().unwrap();
    assert_eq!((set.baseline_count, set.candidate_count), (3, 1));
    assert_eq!(execute(&catalog, &root, set, &stopping).await.unwrap(), 1);
    assert!(!object_exists(&catalog, "0000").await);
    assert!(object_exists(&catalog, "0001").await);
    assert!(object_exists(&catalog, "sealed/inside").await);
    assert_eq!(fs::read(sealed.join("inside")).unwrap(), b"inside");
    drop(root);
    catalog.close().await.unwrap();
}

/// An inventory that finds the members of the root's unapproved set again confirms that set,
/// even after something invalidated it, so a revision being inspected stays readable. It never
/// confirms an approved set, whose approval would then count twice, nor a set whose members
/// changed, nor one from before a restart.
#[tokio::test]
async fn an_identical_inventory_keeps_an_unapproved_revision_and_never_reuses_an_approval() {
    let (data, tree, catalog, root, config) = fixture(200).await;
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let first = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert!(first.guarded);
    let observed = root.observation().await.unwrap().revision;
    let again = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.revision, first.revision);
    assert_eq!(root.observation().await.unwrap().revision, observed);

    root.invalidate_deletions().await.unwrap();
    assert!(
        root.deletion_page(first.revision, Ordinal::START, PageLimit::LARGEST)
            .await
            .is_err()
    );
    let confirmed = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(confirmed.revision, first.revision);
    assert_eq!(confirmed.candidate_count, 200);
    assert_eq!(
        root.deletion_page(first.revision, Ordinal::START, PageLimit::LARGEST)
            .await
            .unwrap()
            .len(),
        200
    );

    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/0000"), b"immutable").unwrap();
    let changed = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(changed.revision, first.revision);
    assert_eq!(changed.candidate_count, 199);
    assert!(root.approve_deletions(first.revision).await.is_err());

    root.approve_deletions(changed.revision).await.unwrap();
    root.invalidate_deletions().await.unwrap();
    let after_approval = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(after_approval.revision, changed.revision);
    assert_eq!(
        after_approval.authorization,
        DeletionAuthorization::Withheld
    );
    assert!(root.approve_deletions(changed.revision).await.is_err());

    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    catalog.enable_change_journal().await.unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    let root = crate::sync::test_support::probed(root).await;
    let restarted = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(restarted.revision, after_approval.revision);
    assert_eq!(restarted.candidate_count, 199);
    assert!(
        root.deletion_page(after_approval.revision, Ordinal::START, PageLimit::LARGEST)
            .await
            .is_err()
    );
    for name in ["0000", "0001", "0199"] {
        assert!(object_exists(&catalog, name).await);
    }
    drop(root);
    catalog.close().await.unwrap();
}

/// An API write since the set was taken moves the journal past it, which keeps the set from
/// being approved until an inventory looks again. That inventory keeps the revision while the
/// members are the same, as after a metadata write that leaves every generation as it was, and
/// replaces the set once a member changed, here deleted through the API, so the approval the
/// earlier revision named never applies to other members.
#[tokio::test]
async fn an_api_write_keeps_the_revision_only_while_the_members_stay_the_same() {
    let (_data, tree, catalog, root, config) = fixture(2).await;
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let first = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first.candidate_count, first.guarded), (2, true));

    catalog
        .update_object_metadata(
            live("test-bucket", "0000"),
            Default::default(),
            crate::object::ObjectMetadataUpdate {
                content_type: crate::object::Patch::Set("text/plain".into()),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert!(root.approve_deletions(first.revision).await.is_err());
    let again = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.revision, first.revision);
    assert_eq!(again.candidate_count, 2);

    let bucket = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("0001").unwrap();
    let admin = catalog.admin(Origin::Api);
    let generation = admin
        .get_object(&bucket, &name, None)
        .await
        .unwrap()
        .generation;
    admin
        .delete_object(
            &bucket,
            &name,
            GenerationCondition::Is(generation.try_into().unwrap()),
        )
        .await
        .unwrap();
    let changed = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(changed.revision, first.revision);
    assert_eq!(changed.candidate_count, 1);
    assert!(root.approve_deletions(first.revision).await.is_err());
    assert!(object_exists(&catalog, "0000").await);
    drop(root);
    catalog.close().await.unwrap();
}

/// A decision that finds its set no longer waiting reads what became of the set, which the
/// refusal names: decided already, and how; replaced by the set that waits now; still waiting
/// under its revision, which only an API write racing the decision leaves; or no set waiting.
#[tokio::test]
async fn a_refused_decision_reads_what_became_of_its_set() {
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    for decided in [Refusal::Approved, Refusal::Rejected] {
        let (_data, tree, catalog, root, config) = fixture(2).await;
        fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
        let set = inventory(&root, &config, summary.gate(), &stopping)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(root.refusal(set.revision).await.unwrap(), Refusal::Moved);
        if decided == Refusal::Approved {
            root.approve_deletions(set.revision).await.unwrap();
        } else {
            root.reject_deletions(set.revision).await.unwrap();
        }
        assert_eq!(root.refusal(set.revision).await.unwrap(), decided);
        drop(root);
        catalog.close().await.unwrap();
    }

    let (_data, tree, catalog, root, config) = fixture(2).await;
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    let first = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("0001").unwrap();
    let admin = catalog.admin(Origin::Api);
    let generation = admin
        .get_object(&bucket, &name, None)
        .await
        .unwrap()
        .generation;
    admin
        .delete_object(
            &bucket,
            &name,
            GenerationCondition::Is(generation.try_into().unwrap()),
        )
        .await
        .unwrap();
    let changed = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(changed.revision, first.revision);
    assert_eq!(
        root.refusal(first.revision).await.unwrap(),
        Refusal::Replaced {
            revision: changed.revision,
            missing: changed.candidate_count,
            tracked: changed.baseline_count,
        }
    );
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/0000"), b"immutable").unwrap();
    inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap();
    for revision in [first.revision, changed.revision] {
        assert_eq!(root.refusal(revision).await.unwrap(), Refusal::Settled);
    }
    drop(root);
    catalog.close().await.unwrap();
}

/// A bucket's whole directory gone takes its files to approval however few they are, because a
/// missing mount looks the same; approving deletes them. The same files deleted inside a
/// directory that is still there are deleted at once, and a disabled guard approves nothing.
#[tokio::test]
async fn a_bucket_directory_gone_as_a_whole_needs_approval_whatever_its_size() {
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let (_stop, stopping) = tokio::sync::watch::channel(false);

    let (_data, tree, catalog, root, config) = fixture(2).await;
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((set.baseline_count, set.candidate_count), (2, 2));
    assert!(set.guarded);
    assert_eq!(execute(&catalog, &root, set, &stopping).await.unwrap(), 0);
    assert!(object_exists(&catalog, "0000").await);
    assert!(object_exists(&catalog, "0001").await);
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert!(set.guarded);
    root.approve_deletions(set.revision).await.unwrap();
    let approved = root.deletion_set().await.unwrap().unwrap();
    assert_eq!(
        execute(&catalog, &root, approved, &stopping).await.unwrap(),
        2
    );
    assert!(!object_exists(&catalog, "0000").await);
    assert!(!object_exists(&catalog, "0001").await);
    drop(root);
    catalog.close().await.unwrap();

    let (_data, tree, catalog, root, config) = fixture(2).await;
    for name in ["0000", "0001"] {
        fs::remove_file(tree.0.join("test-bucket").join(name)).unwrap();
    }
    let set = inventory(&root, &config, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert!(!set.guarded);
    assert_eq!(execute(&catalog, &root, set, &stopping).await.unwrap(), 2);
    drop(root);
    catalog.close().await.unwrap();

    let (_data, tree, catalog, root, config) = fixture(2).await;
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    let unguarded = Config {
        sync_delete_guard: false,
        ..config
    };
    let set = inventory(&root, &unguarded, summary.gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert!(!set.guarded);
    assert_eq!(execute(&catalog, &root, set, &stopping).await.unwrap(), 2);
    drop(root);
    catalog.close().await.unwrap();
}

/// The warning on a bucket whose tracked files all went names a failed mount before anything
/// else, in the words every decision remedy uses, and only then the rejection, restoring by hand
/// and last the approval; it never offers a mount and a rejection as alternatives, and it says
/// what the inventory saw rather than when the files went.
#[test]
fn a_bucket_gone_warning_names_the_mount_before_any_decision() {
    for gone in [BucketGone::Missing, BucketGone::Emptied] {
        let (_, reason, remedy) = bucket_gone_report(gone);
        let at = |step: &str| {
            remedy
                .find(step)
                .unwrap_or_else(|| panic!("{step}: {remedy}"))
        };
        let steps = [
            at("Inspect the pending deletion set"),
            at(MOUNT_FIRST),
            at("went by mistake, reject the set"),
            at("back yourself and wait again"),
            at("on purpose, approve the set"),
        ];
        assert!(steps.is_sorted(), "{remedy}");
        assert!(!remedy.contains("or reject"), "{remedy}");
        assert!(!reason.contains("stopped"), "{reason}");
    }
}
