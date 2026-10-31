#[test]
fn threshold_is_root_wide_strictly_above_half_and_has_an_absolute_floor() {
    use super::{Guard, guarded};
    for (n, d, expected) in [
        (0, 0, false),
        (99, 99, false),
        (200, 100, false),
        (199, 100, true),
        (1000, 500, false),
        (1000, 501, true),
        (u64::MAX, u64::MAX, true),
    ] {
        assert_eq!(guarded(Guard::Enabled, n, d), expected, "N={n} D={d}");
        assert!(!guarded(Guard::Disabled, n, d));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn deletion_membership_progress_rolls_back_with_object_failure_and_restart_invalidates_revision()
 {
    use super::*;
    use crate::{
        config::Config,
        storage::{
            Catalog,
            admin::{
                BucketName, Generation, GenerationCondition, ObjectName, Origin, Publication,
                PublishMode,
            },
        },
        test_support::TestDir,
    };
    use std::{fs, time::SystemTime};
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    catalog
        .insert_bucket(
            crate::json::parse_insert(br#"{"name":"test-bucket"}"#, "project", SystemTime::now())
                .unwrap(),
        )
        .await
        .unwrap();
    fs::write(data.0.join("source"), b"body").unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("object").unwrap();
    let record = catalog
        .admin(Origin::Api)
        .publish_from_file(
            &bucket,
            &name,
            Publication {
                content_type: "text/plain".into(),
                metadata: Default::default(),
                expected: Default::default(),
                condition: None,
                max_bytes: 1024,
                mode: PublishMode::Replace,
            },
            fs::File::open(data.0.join("source")).unwrap(),
        )
        .await
        .unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    assert!(
        crate::sync::reconcile(&catalog, &root, &config)
            .await
            .unwrap()
            .baseline_complete
    );
    fs::remove_file(tree.0.join("test-bucket/object")).unwrap();
    let inventory = root
        .begin_deletion_inventory(Guard::Disabled)
        .await
        .unwrap();
    let page = root
        .deletion_inventory_page(inventory.clone(), String::new())
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    root.record_deletion_candidates(inventory.clone(), 1, page)
        .await
        .unwrap();
    let set = root.finish_deletion_inventory(inventory).await.unwrap();
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    let binding = root.deletion_binding(binding, set.revision.clone(), 1);
    catalog.run_admin(|connection|connection.execute_batch("CREATE TEMP TRIGGER fail_delete BEFORE DELETE ON objects BEGIN SELECT RAISE(ABORT,'injected delete failure'); END;").map_err(unavailable)).await.unwrap();
    assert!(
        catalog
            .admin(Origin::SyncIngest)
            .delete_synced_object(
                &bucket,
                &name,
                GenerationCondition::Is(Generation::new(record.generation).unwrap()),
                binding.clone()
            )
            .await
            .is_err()
    );
    assert_eq!(
        root.deletion_set().await.unwrap().unwrap().completed_count,
        0
    );
    assert!(
        catalog
            .sync_state()
            .get_path(binding.clone(), "test-bucket/object".into())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await
            .unwrap()
            .generation,
        record.generation
    );
    catalog
        .run_admin(|connection| {
            connection
                .execute_batch("DROP TRIGGER fail_delete;")
                .map_err(unavailable)
        })
        .await
        .unwrap();
    catalog
        .admin(Origin::SyncIngest)
        .delete_synced_object(
            &bucket,
            &name,
            GenerationCondition::Is(Generation::new(record.generation).unwrap()),
            binding,
        )
        .await
        .unwrap();
    assert_eq!(
        root.deletion_set().await.unwrap().unwrap().completed_count,
        1
    );
    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, super::super::root::Activation::Open)
        .await
        .unwrap();
    assert!(root.deletion_page(set.revision, 0, 256).await.is_err());
    drop(root);
    catalog.close().await.unwrap();
}
