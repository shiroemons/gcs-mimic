#![expect(
    clippy::unwrap_used,
    reason = "Invalid test fixtures and failed operations must fail at their call sites"
)]

use super::*;
use crate::{
    storage::{admin::Origin, sync_state::root::Activation},
    test_support::TestDir,
};
use std::fs;

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
    (data, tree, catalog, root, config)
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
            "test-bucket",
            "0000",
            None,
            Default::default(),
            crate::object::ObjectMetadataUpdate {
                temporary_hold: Some(true),
                ..Default::default()
            },
            |_| Ok(()),
        )
        .await
        .unwrap();
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let set = inventory(&root, &config, &summary, &stopping)
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
    assert!(protected.temporary_hold);
    let observation = root.observation().await.unwrap();
    assert_eq!(observation.path_stops, 1);
    assert_eq!(
        root.observation_page(observation.revision, (0, 0), 256)
            .await
            .unwrap()[0]
            .reason,
        "ProtectedDeletion"
    );
    catalog
        .update_object_metadata(
            "test-bucket",
            "0000",
            None,
            Default::default(),
            crate::object::ObjectMetadataUpdate {
                temporary_hold: Some(false),
                ..Default::default()
            },
            |_| Ok(()),
        )
        .await
        .unwrap();
    catalog
        .patch_bucket(
            "test-bucket",
            crate::bucket::BucketMetadataUpdate {
                versioning_enabled: Some(true),
                ..Default::default()
            },
            None,
            None,
            &crate::bucket::BucketAccess::Permissive,
        )
        .await
        .unwrap();
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    let set = inventory(&root, &config, &summary, &stopping)
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
    assert!(
        inventory(&root, &config, &summary, &stopping)
            .await
            .is_err()
    );
    assert_eq!(fs::read(outside.0.join("preserved")).unwrap(), b"outside");
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    fs::remove_file(tree.0.join(mapping::MARKER)).unwrap();
    assert!(
        inventory(&root, &config, &summary, &stopping)
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
    let set = inventory(&root, &config, &summary, &stopping)
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
        .deletion_page(set.revision.clone(), 0, 256)
        .await
        .unwrap();
    let second = root
        .deletion_page(set.revision.clone(), first.last().unwrap().ordinal, 256)
        .await
        .unwrap();
    assert_eq!((first.len(), second.len()), (256, 44));
    assert_eq!(first[0].name, "0000");
    assert_eq!(second.last().unwrap().name, "0299");
    let approved = root.approve_deletions(set.revision.clone()).await.unwrap();
    assert!(root.approve_deletions(set.revision.clone()).await.is_err());
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
        inventory(&root, &config, &summary, &stopping)
            .await
            .unwrap()
            .is_none()
    );
    summary.deferred = 0;
    let set = inventory(&root, &config, &summary, &stopping)
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
    assert!(root.deletion_page(set.revision, 0, 256).await.is_err());
    fs::remove_file(tree.0.join("test-bucket/0000")).unwrap();
    let set = inventory(&root, &config, &summary, &stopping)
        .await
        .unwrap()
        .unwrap();
    catalog
        .patch_bucket(
            "test-bucket",
            crate::bucket::BucketMetadataUpdate::default(),
            None,
            None,
            &crate::bucket::BucketAccess::Permissive,
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
