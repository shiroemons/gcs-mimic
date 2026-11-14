use super::*;
use crate::{
    bucket::autoclass::AutoclassUpdate,
    object::ComposeSource,
    storage::{
        DerivedDestination, SourceDisposal,
        catalog::{
            MoveAccess, MoveNames,
            rewrite::{RewriteBinding, RewriteStart},
        },
    },
};

#[tokio::test]
async fn transitions_never_backdate_creation_or_a_newer_metadata_update() {
    for metadata_update in [false, true] {
        let (catalog, clock) = memory().await;
        create(
            &catalog,
            vec![transition(StorageClass::Nearline, json!({"isLive":true}))],
        )
        .await;
        let pass_time = clock.now();
        if !metadata_update {
            clock.advance(Duration::from_micros(50));
        }
        put(&catalog, "object").await;
        if metadata_update {
            clock.advance(Duration::from_micros(50));
            catalog
                .update_object_metadata(
                    live("bucket", "object"),
                    Default::default(),
                    ObjectMetadataUpdate::default(),
                    |_| Ok(()),
                    PERMISSIVE,
                )
                .await
                .unwrap();
        }
        let before = current(&catalog, "object").await;
        let incarnation = incarnation(&catalog).await;
        catalog
            .run::<_, CatalogError>(move |connection| {
                sweep_page(
                    connection,
                    "bucket",
                    incarnation,
                    None,
                    pass_time,
                    ACTION_LIMIT,
                    BucketRules::Absent,
                )
                .map_err(unavailable)
            })
            .await
            .unwrap();
        let changed = current(&catalog, "object").await;
        assert_eq!(changed.storage_class, StorageClass::Nearline);
        assert_eq!(changed.metageneration, before.metageneration + 1);
        assert_eq!(changed.updated, before.updated);
        assert_eq!(changed.storage_class_updated(), before.updated);
        assert!(changed.updated >= changed.time_created);
        assert!(changed.storage_class_updated() >= changed.time_created);
        assert_eq!(execute(&catalog).await.transitioned, 0);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn every_new_generation_starts_its_storage_class_time_at_creation() {
    let (catalog, clock) = memory().await;
    let mut bucket = bucket("bucket", "project");
    bucket.soft_delete_policy = Some(SoftDeletePolicy {
        retention_duration_seconds: 604800,
        effective_time: clock.now(),
    });
    bucket.lifecycle = vec![transition(StorageClass::Nearline, json!({"age":1}))];
    catalog.insert_bucket(bucket).await.unwrap();
    put(&catalog, "source").await;
    put(&catalog, "restore-source").await;
    clock.advance(DAY);
    assert_eq!(execute(&catalog).await.transitioned, 2);
    let transitioned = current(&catalog, "source").await;
    rules(&catalog, vec![]).await;
    for route in ["upload", "copy", "rewrite", "compose", "move", "restore"] {
        clock.advance(Duration::from_secs(1));
        let destination = DerivedDestination {
            bucket: "bucket".into(),
            name: route.into(),
            content_type: "text/plain".into(),
            ..DerivedDestination::new(BucketAccess::Permissive)
        };
        let created = match route {
            "upload" => put(&catalog, route).await,
            "copy" => {
                let source = catalog
                    .resolve_derived_source(
                        live("bucket", "source"),
                        Default::default(),
                        PERMISSIVE,
                    )
                    .await
                    .unwrap();
                let (activity, _updates) = tokio::sync::watch::channel(Default::default());
                crate::server::copy::execute(
                    &catalog,
                    crate::server::copy::CopyJob {
                        source,
                        source_access: BucketAccess::Permissive,
                        destination,
                        source_location: None,
                        replay: None,
                    },
                    crate::server::copy::CopyBudget {
                        config: &Default::default(),
                        deadline: crate::time_limit::TimeLimit::DISABLED.deadline(),
                        activity: &activity,
                    },
                    None,
                )
                .await
                .unwrap()
            }
            "rewrite" => {
                let source = catalog
                    .resolve_derived_source(
                        live("bucket", "source"),
                        Default::default(),
                        PERMISSIVE,
                    )
                    .await
                    .unwrap();
                let start = RewriteStart {
                    binding: RewriteBinding {
                        source_bucket: "bucket".into(),
                        source_object: "source".into(),
                        source_generation: transitioned.generation,
                        destination_bucket: "bucket".into(),
                        destination_object: route.into(),
                        parameters: Default::default(),
                        metadata: None,
                    },
                    destination,
                    source_conditions: Default::default(),
                    source_metageneration: transitioned.metageneration,
                    source_access: BucketAccess::Permissive,
                    source_key_sha256: None,
                    budget: 1_048_576,
                };
                let (token, mut owner) = catalog
                    .begin_rewrite_session(vec![source.whole()], u64::MAX, start)
                    .await
                    .unwrap();
                owner.disarm();
                let result = catalog
                    .continue_rewrite(
                        &token.to_string(),
                        BucketAccess::Permissive,
                        BucketAccess::Permissive,
                        |record| Ok(json!({"generation":record.generation})),
                    )
                    .await
                    .unwrap();
                assert!(result.resource.is_some());
                current(&catalog, route).await
            }
            "compose" => catalog
                .compose_object(
                    destination,
                    vec![ComposeSource {
                        name: "source".into(),
                        generation: None,
                        if_generation_match: None,
                    }],
                    SourceDisposal::Keep,
                    100,
                )
                .await
                .unwrap(),
            "move" => catalog
                .move_object(
                    MoveNames {
                        bucket: "bucket".into(),
                        source: "source".into(),
                        destination: route.into(),
                    },
                    Default::default(),
                    MoveAccess {
                        source_read: PERMISSIVE,
                        source_delete: PERMISSIVE,
                        destination_create: PERMISSIVE,
                    },
                )
                .await
                .unwrap(),
            "restore" => {
                let original = current(&catalog, "restore-source").await;
                catalog
                    .delete_object(
                        live("bucket", "restore-source"),
                        Default::default(),
                        PERMISSIVE,
                    )
                    .await
                    .unwrap();
                catalog
                    .restore_object(
                        ObjectTarget::new("bucket", "restore-source", original.generation),
                        Default::default(),
                        PERMISSIVE_RESTORE,
                    )
                    .await
                    .unwrap()
            }
            _ => unreachable!("known publication route"),
        };
        assert_eq!(created.time_created, clock.now(), "{route}");
        assert_eq!(
            created.storage_class_updated(),
            created.time_created,
            "{route}"
        );
        assert!(
            created.storage_class_updated() > transitioned.storage_class_updated(),
            "{route}"
        );
        assert_eq!(
            current(&catalog, &created.name)
                .await
                .storage_class_updated(),
            created.time_created,
            "{route}"
        );
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn enabling_autoclass_updates_the_class_time_only_for_changed_live_and_noncurrent_versions() {
    let (catalog, clock) = memory().await;
    let mut bucket = bucket("bucket", "project");
    bucket.versioning_enabled = true;
    bucket.soft_delete_policy = Some(SoftDeletePolicy {
        retention_duration_seconds: 604800,
        effective_time: clock.now(),
    });
    bucket.lifecycle = vec![transition(StorageClass::Nearline, json!({"age":1}))];
    catalog.insert_bucket(bucket).await.unwrap();
    let noncurrent = put(&catalog, "versions").await;
    put(&catalog, "versions").await;
    let deleted = put(&catalog, "deleted").await;
    clock.advance(DAY);
    assert_eq!(execute(&catalog).await.transitioned, 3);
    catalog
        .delete_object(
            version("bucket", "deleted", deleted.generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let standard = put(&catalog, "standard").await;
    clock.advance(Duration::from_secs(1));
    catalog
        .patch_bucket(
            "bucket",
            BucketMetadataUpdate {
                lifecycle: Some(vec![]),
                autoclass: Some(AutoclassUpdate {
                    enabled: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    for target in [
        live("bucket", "versions"),
        version("bucket", "versions", noncurrent.generation),
    ] {
        let changed = catalog
            .get_object(target, Default::default(), PERMISSIVE)
            .await
            .unwrap();
        assert_eq!(changed.storage_class, StorageClass::Standard);
        assert_eq!(changed.storage_class_updated(), clock.now());
    }
    assert_eq!(
        current(&catalog, "standard").await.storage_class_updated(),
        standard.time_created
    );
    let stored = catalog.run::<_, CatalogError>(move |connection| {
        sql::row(connection, "SELECT storage_class, time_storage_class_updated FROM objects WHERE name = 'deleted'", [], |row| Ok((row.get::<_, StorageClass>(0)?, row.get::<_, i64>(1)?))).map_err(unavailable)
    }).await.unwrap();
    assert_eq!(
        stored,
        (
            StorageClass::Nearline,
            crate::storage::catalog::unix_micros(deleted.time_created + DAY).unwrap()
        )
    );
    catalog.close().await.unwrap();
}
