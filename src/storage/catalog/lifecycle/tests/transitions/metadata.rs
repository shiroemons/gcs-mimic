use super::*;

#[tokio::test]
async fn newly_configured_rules_choose_current_candidates_without_replaying_old_deadlines() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![]).await;
    put(&catalog, "object").await;
    clock.advance(DAY * 60);
    rules(
        &catalog,
        vec![
            transition(StorageClass::Coldline, json!({"age":30})),
            transition(StorageClass::Nearline, json!({"age":10})),
            delete(json!({"age":20,"matchesStorageClass":["NEARLINE"]})),
        ],
    )
    .await;
    let outcome = execute(&catalog).await;
    assert_eq!((outcome.transitioned, outcome.deleted), (1, 0));
    assert_eq!(
        current(&catalog, "object").await.storage_class,
        StorageClass::Coldline
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn newly_configured_rules_preserve_the_observed_noncurrent_time() {
    let (catalog, clock) = versioned(vec![]).await;
    let original = put(&catalog, "object").await;
    clock.advance(DAY * 40);
    put(&catalog, "object").await;
    let archived_at = clock.now();
    clock.advance(DAY * 15);
    rules(
        &catalog,
        vec![
            delete(json!({"isLive":true,"age":30})),
            delete(json!({"daysSinceNoncurrentTime":20})),
        ],
    )
    .await;
    let outcome = execute(&catalog).await;
    assert_eq!(
        (outcome.archived, outcome.deleted, outcome.next),
        (0, 0, Some(original.time_created + DAY * 60))
    );
    assert_eq!(
        selected(&catalog, original.generation).await.time_deleted,
        Some(archived_at)
    );
    clock.advance(DAY * 5);
    assert_eq!(execute(&catalog).await.deleted, 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn live_only_rules_never_match_existing_noncurrent_versions_even_after_versioning_is_suspended()
 {
    for suspended in [false, true] {
        let (catalog, clock) = versioned(vec![]).await;
        let original = put(&catalog, "object").await;
        clock.advance(DAY * 40);
        put(&catalog, "object").await;
        let archived = selected(&catalog, original.generation).await;
        clock.advance(DAY);
        catalog
            .patch_bucket(
                "bucket",
                BucketMetadataUpdate {
                    versioning_enabled: suspended.then_some(false),
                    lifecycle: Some(vec![
                        delete(json!({"isLive":true,"age":30})),
                        transition(StorageClass::Nearline, json!({"isLive":true,"age":30})),
                    ]),
                    ..Default::default()
                },
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert_eq!(execute(&catalog).await.applied(), 0);
        assert_eq!(selected(&catalog, original.generation).await, archived);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_noncurrent_transition_cannot_enable_a_historical_live_only_rule() {
    let (catalog, clock) = versioned(vec![
        transition(StorageClass::Nearline, json!({"isLive":false})),
        transition(
            StorageClass::Coldline,
            json!({"isLive":true,"age":10,"matchesStorageClass":["NEARLINE"]}),
        ),
        delete(json!({"isLive":false,"matchesStorageClass":["COLDLINE"]})),
    ])
    .await;
    let original = put(&catalog, "object").await;
    clock.advance(DAY * 20);
    put(&catalog, "object").await;
    clock.advance(DAY * 25);
    let outcome = execute(&catalog).await;
    assert_eq!((outcome.transitioned, outcome.deleted), (1, 0));
    assert_eq!(
        selected(&catalog, original.generation).await.storage_class,
        StorageClass::Nearline
    );
    assert_eq!(execute(&catalog).await.applied(), 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn an_overwritten_retained_version_stops_matching_a_blocked_live_delete() {
    for with_class_delete in [false, true] {
        let (catalog, clock) = memory().await;
        let mut record = bucket("bucket", "project");
        record.versioning_enabled = true;
        record.object_retention_enabled = true;
        record.lifecycle = vec![
            delete(json!({"isLive":true})),
            transition(StorageClass::Nearline, json!({"age":10})),
        ];
        if with_class_delete {
            record
                .lifecycle
                .push(delete(json!({"age":20,"matchesStorageClass":["STANDARD"]})));
        }
        catalog.insert_bucket(record).await.unwrap();
        let mut upload = permissive_upload("bucket", "object", 100);
        upload.metadata.retention = Some(Some(ObjectRetention {
            mode: RetentionMode::Unlocked,
            retain_until: (clock.now() + DAY * 20).into(),
        }));
        let original = catalog
            .put_object(upload, body(vec![Bytes::from_static(b"data")]))
            .await
            .unwrap();
        for day in 0..=21 {
            clock.set(original.time_created + DAY * day);
            if day == 5 {
                put(&catalog, "object").await;
            }
            let outcome = candidate(
                &catalog,
                version("bucket", "object", original.generation),
                incarnation(&catalog).await,
            )
            .await;
            assert_eq!(outcome.blocked, u64::from(day < 5), "day={day}");
            assert_eq!(outcome.transitioned, u64::from(day == 10), "day={day}");
            assert_eq!((outcome.archived, outcome.deleted), (0, 0), "day={day}");
        }
        assert_eq!(
            selected(&catalog, original.generation).await.storage_class,
            StorageClass::Nearline
        );
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn releasing_a_hold_preserves_custom_time_and_only_class_dependent_delete_yields() {
    for gated in [false, true] {
        let (catalog, clock) = memory().await;
        let mut condition = json!({"daysSinceCustomTime":5});
        if gated {
            condition["matchesStorageClass"] = json!(["STANDARD"]);
        }
        create(
            &catalog,
            vec![
                delete(condition),
                transition(StorageClass::Nearline, json!({"age":8})),
            ],
        )
        .await;
        let mut upload = permissive_upload("bucket", "object", 100);
        upload.metadata.custom_time = Some(Some("1970-01-02T00:00:00Z".into()));
        upload.metadata.temporary_hold = Some(true);
        let original = catalog
            .put_object(upload, body(vec![Bytes::from_static(b"data")]))
            .await
            .unwrap();
        clock.advance(DAY * 5);
        let blocked = execute(&catalog).await;
        assert_eq!(blocked.blocked, 1);
        assert_eq!(
            blocked.next,
            gated.then_some(original.time_created + DAY * 8)
        );
        clock.advance(DAY * 3);
        let due = execute(&catalog).await;
        assert_eq!(
            (due.transitioned, due.blocked),
            (u64::from(gated), u64::from(!gated))
        );
        clock.advance(DAY);
        catalog
            .update_object_metadata(
                live("bucket", "object"),
                Default::default(),
                ObjectMetadataUpdate {
                    temporary_hold: Some(false),
                    ..Default::default()
                },
                |_| Ok(()),
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert_eq!(
            current(&catalog, "object").await.custom_time,
            original.custom_time
        );
        let released = execute(&catalog).await;
        assert_eq!(
            (released.transitioned, released.deleted, released.blocked),
            (0, u64::from(!gated), 0)
        );
        assert_eq!(exists(&catalog, live("bucket", "object")).await, gated);
        if gated {
            assert_eq!(
                current(&catalog, "object").await.storage_class,
                StorageClass::Nearline
            );
        }
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn backdated_custom_time_is_effective_immediately_on_upload_or_patch() {
    for patch in [false, true] {
        for gated in [false, true] {
            let (catalog, clock) = memory().await;
            let mut condition = json!({"daysSinceCustomTime":90});
            if gated {
                condition["matchesStorageClass"] = json!(["STANDARD"]);
            }
            create(
                &catalog,
                vec![
                    transition(StorageClass::Nearline, json!({"daysSinceCustomTime":30})),
                    delete(condition),
                ],
            )
            .await;
            clock.advance(DAY * 200);
            let timestamp = "1970-01-10T00:00:00Z";
            let mut upload = permissive_upload("bucket", "object", 100);
            upload.metadata.custom_time = (!patch).then(|| Some(timestamp.into()));
            catalog
                .put_object(upload, body(vec![Bytes::from_static(b"data")]))
                .await
                .unwrap();
            if patch {
                catalog
                    .update_object_metadata(
                        live("bucket", "object"),
                        Default::default(),
                        ObjectMetadataUpdate {
                            custom_time: Some(Some(timestamp.into())),
                            ..Default::default()
                        },
                        |_| Ok(()),
                        PERMISSIVE,
                    )
                    .await
                    .unwrap();
            }
            let outcome = execute(&catalog).await;
            assert_eq!(
                (outcome.transitioned, outcome.deleted),
                (u64::from(gated), u64::from(!gated))
            );
            assert_eq!(exists(&catalog, live("bucket", "object")).await, gated);
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn a_delayed_archive_starts_noncurrent_age_at_the_physical_pass_time() {
    let (catalog, clock) = versioned(vec![
        delete(json!({"isLive":true,"age":10})),
        delete(json!({"daysSinceNoncurrentTime":10})),
    ])
    .await;
    let original = put(&catalog, "object").await;
    clock.advance(DAY * 30);
    let archived_at = clock.now();
    let first = execute(&catalog).await;
    assert_eq!((first.archived, first.deleted), (1, 0));
    assert_eq!(
        selected(&catalog, original.generation).await.time_deleted,
        Some(archived_at)
    );
    let next = execute(&catalog).await;
    assert_eq!(
        (next.applied(), next.next),
        (0, Some(archived_at + DAY * 10))
    );
    clock.advance(DAY * 10);
    assert_eq!(execute(&catalog).await.deleted, 1);
    catalog.close().await.unwrap();
}
