use super::*;

mod current_time;
mod durability;
mod metadata;
mod migration;
mod missed_passes;
mod timestamps;

const MICROSECOND: Duration = Duration::from_micros(1);

async fn versioned(configured: Vec<Value>) -> (Catalog, Arc<ManualClock>) {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.versioning = crate::bucket::Versioning::Enabled;
    record.lifecycle = crate::bucket::lifecycle::LifecycleRules::for_test(configured).into();
    catalog.insert_bucket(record).await.unwrap();
    (catalog, clock)
}

async fn selected(catalog: &Catalog, generation: i64) -> ObjectRecord {
    catalog
        .get_object(
            version("bucket", "object", generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap()
}

fn transition(class: StorageClass, condition: Value) -> Value {
    json!({"action":{"type":"SetStorageClass","storageClass":class},"condition":condition})
}

async fn current(catalog: &Catalog, name: &str) -> ObjectRecord {
    catalog
        .get_object(live("bucket", name), Default::default(), PERMISSIVE)
        .await
        .unwrap()
}

async fn put_data(catalog: &Catalog, name: &str, data: &'static [u8]) -> ObjectRecord {
    catalog
        .put_object(
            permissive_upload("bucket", name, 100),
            body(vec![Bytes::from_static(data)]),
        )
        .await
        .unwrap()
}

async fn put_class(catalog: &Catalog, name: &str, class: StorageClass) -> ObjectRecord {
    let mut upload = permissive_upload("bucket", name, 100);
    upload.metadata.storage_class = Some(class);
    catalog
        .put_object(upload, body(vec![Bytes::from_static(b"data")]))
        .await
        .unwrap()
}

fn counts(
    outcome: LifecycleCounts,
    transitioned: u64,
    deleted: u64,
    blocked: u64,
    next: Option<SystemTime>,
) {
    assert_eq!(
        (
            outcome.scanned,
            outcome.transitioned,
            outcome.deleted,
            outcome.archived,
            outcome.blocked,
            outcome.skipped,
            outcome.next
        ),
        (1, transitioned, deleted, 0, blocked, 0, next),
    );
}

async fn blob(catalog: &Catalog, name: &str) -> String {
    let name = name.to_owned();
    catalog
        .run::<_, CatalogError>(move |connection| {
            Ok(sql::scalar(
                connection,
                "SELECT blob_id FROM objects WHERE bucket = 'bucket' AND name = ?1",
                [name],
            )?)
        })
        .await
        .unwrap()
}

/// Each legacy class lives in its own location type, so a multi-region bucket covers
/// MULTI_REGIONAL and a regional one REGIONAL, each with the classes every location holds.
#[tokio::test]
async fn every_supported_transition_and_inert_pair_obey_exact_age_deadlines() {
    use StorageClass::{
        Archive, Coldline, DurableReducedAvailability, MultiRegional, Nearline, Regional, Standard,
    };
    let supported = [
        (DurableReducedAvailability, MultiRegional),
        (DurableReducedAvailability, Regional),
        (DurableReducedAvailability, Nearline),
        (DurableReducedAvailability, Coldline),
        (DurableReducedAvailability, Archive),
        (Standard, Nearline),
        (Standard, Coldline),
        (Standard, Archive),
        (MultiRegional, Nearline),
        (MultiRegional, Coldline),
        (MultiRegional, Archive),
        (Regional, Nearline),
        (Regional, Coldline),
        (Regional, Archive),
        (Nearline, Coldline),
        (Nearline, Archive),
        (Coldline, Archive),
    ];
    for (location, legacy) in [("US", MultiRegional), ("US-CENTRAL1", Regional)] {
        let classes = [
            Standard,
            legacy,
            DurableReducedAvailability,
            Nearline,
            Coldline,
            Archive,
        ];
        for (from, to) in classes
            .into_iter()
            .flat_map(|from| classes.map(|to| (from, to)))
        {
            let (catalog, clock) = memory().await;
            let mut record = bucket("bucket", "project");
            record.location = location.parse().unwrap();
            record.lifecycle =
                crate::bucket::lifecycle::LifecycleRules::for_test(vec![transition(
                    to,
                    json!({"age":1}),
                )])
                .into();
            catalog.insert_bucket(record).await.unwrap();
            let original = put_class(&catalog, "object", from).await;
            let original_blob = blob(&catalog, "object").await;
            assert_eq!(original.storage_class_updated(), original.time_created);
            let valid = supported.contains(&(from, to));
            let due = original.time_created + DAY;
            clock.set(due - MICROSECOND);
            counts(execute(&catalog).await, 0, 0, 0, valid.then_some(due));
            assert_eq!(current(&catalog, "object").await, original);
            clock.set(due);
            counts(
                execute(&catalog).await,
                u64::from(valid),
                0,
                0,
                valid.then_some(due + COALESCE),
            );
            let changed = current(&catalog, "object").await;
            assert_eq!(changed.storage_class, if valid { to } else { from });
            assert_eq!(changed.generation, original.generation);
            assert_eq!(
                changed.metageneration.get(),
                original.metageneration.get() + i64::from(valid)
            );
            assert_eq!(changed.time_created, original.time_created);
            assert_eq!(changed.updated, if valid { due } else { original.updated });
            assert_eq!(
                changed.storage_class_updated(),
                if valid { due } else { original.time_created }
            );
            assert_eq!(blob(&catalog, "object").await, original_blob);
            clock.set(due + MICROSECOND);
            counts(execute(&catalog).await, 0, 0, 0, None);
            assert_eq!(current(&catalog, "object").await, changed);
            catalog.close().await.unwrap();
        }
    }
}

/// A transition to a class the bucket's location cannot hold is refused when configured, as
/// Cloud Storage refused it (recorded 2026-11-17). A rule an earlier release stored that way
/// stays inert.
#[tokio::test]
async fn location_incompatible_transitions_are_refused_and_stored_ones_stay_inert() {
    for (location, target, fits) in [
        ("US-CENTRAL1", StorageClass::MultiRegional, false),
        ("US", StorageClass::MultiRegional, true),
        ("NAM4", StorageClass::MultiRegional, true),
        ("US-CENTRAL1", StorageClass::Regional, true),
        ("US", StorageClass::Regional, false),
        ("NAM4", StorageClass::Regional, false),
    ] {
        let (catalog, clock) = memory().await;
        let mut record = bucket("bucket", "project");
        record.location = location.parse().unwrap();
        catalog.insert_bucket(record).await.unwrap();
        let original =
            put_class(&catalog, "object", StorageClass::DurableReducedAvailability).await;
        let configured = vec![transition(target, json!({"age":1}))];
        let patched = catalog
            .patch_bucket(
                "bucket",
                BucketMetadataUpdate {
                    lifecycle: Some(crate::bucket::lifecycle::LifecycleRules::for_test(
                        configured.clone(),
                    )),
                    ..Default::default()
                },
                Default::default(),
                PERMISSIVE,
            )
            .await;
        if fits {
            patched.unwrap();
        } else {
            assert!(
                matches!(
                    patched,
                    Err(CatalogError::Metadata(
                        crate::metadata_error::MetadataError::ClassLocation(_)
                    ))
                ),
                "{location} {target:?}"
            );
            store_rules(&catalog, &configured).await;
        }
        clock.advance(DAY);
        counts(
            execute(&catalog).await,
            u64::from(fits),
            0,
            0,
            fits.then_some(clock.now() + COALESCE),
        );
        assert_eq!(
            current(&catalog, "object").await.storage_class,
            if fits { target } else { original.storage_class }
        );
        // A stored incompatible transition must not hide a valid colder target or its deadline.
        if !fits {
            store_rules(
                &catalog,
                &[
                    transition(target, json!({"age":1})),
                    transition(StorageClass::Coldline, json!({"age":2})),
                ],
            )
            .await;
            counts(
                execute(&catalog).await,
                0,
                0,
                0,
                Some(original.time_created + 2 * DAY),
            );
            clock.advance(DAY);
            assert_eq!(execute(&catalog).await.transitioned, 1);
            assert_eq!(
                current(&catalog, "object").await.storage_class,
                StorageClass::Coldline
            );
        }
        catalog.close().await.unwrap();
    }
}

/// Writes `rules` past today's validation, as an earlier release could have stored them.
async fn store_rules(catalog: &Catalog, rules: &[Value]) {
    let encoded = Value::Array(rules.to_vec()).to_string();
    catalog
        .run::<_, CatalogError>(move |connection| {
            sql::exec_on_writer(
                connection,
                "UPDATE buckets SET lifecycle = ?1 WHERE name = 'bucket'",
                [encoded],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// A location type that no release wrote cannot tell which transitions the bucket allows, so
/// the scheduler quarantines every rule of the bucket, deletions included, until the row is
/// repaired, rather than running only some of them.
#[tokio::test]
async fn an_unreadable_stored_location_type_quarantines_the_bucket_rules() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.location = "US-CENTRAL1".parse().unwrap();
    record.lifecycle = crate::bucket::lifecycle::LifecycleRules::for_test(vec![
        transition(StorageClass::Coldline, json!({"age":1})),
        delete(json!({"age":1})),
    ])
    .into();
    catalog.insert_bucket(record).await.unwrap();
    let original = put_class(&catalog, "object", StorageClass::Standard).await;
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec_on_writer(
                connection,
                "UPDATE buckets SET location_type = 'unknown' WHERE name = 'bucket'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    clock.advance(DAY);
    let outcome = execute(&catalog).await;
    assert_eq!(
        (outcome.scanned, outcome.transitioned, outcome.deleted),
        (0, 0, 0)
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM lifecycle_quarantine").await,
        1
    );
    assert_eq!(current(&catalog, "object").await, original);
    assert!(matches!(
        catalog.get_bucket("bucket", PERMISSIVE).await,
        Err(CatalogError::Corrupt(_))
    ));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn autoclass_rejects_transition_configuration_without_changing_objects_or_rules() {
    for incompatible in [
        transition(StorageClass::Coldline, json!({"age":1})),
        delete(json!({"age":1,"matchesStorageClass":["STANDARD"]})),
    ] {
        let (catalog, clock) = memory().await;
        let mut record = bucket("bucket", "project");
        record.autoclass = Some(crate::bucket::autoclass::Autoclass {
            toggle_time: clock.now(),
            state: crate::bucket::autoclass::AutoclassState::Enabled {
                terminal_storage_class: crate::bucket::autoclass::TerminalStorageClass::Nearline,
                terminal_storage_class_update_time: clock.now(),
            },
        });
        record.lifecycle =
            crate::bucket::lifecycle::LifecycleRules::for_test(vec![incompatible.clone()]).into();
        assert!(catalog.insert_bucket(record.clone()).await.is_err());
        assert!(
            catalog
                .get_bucket("bucket", PERMISSIVE)
                .await
                .unwrap()
                .is_none()
        );
        record.lifecycle = Default::default();
        catalog.insert_bucket(record).await.unwrap();
        let original = put(&catalog, "object").await;
        assert!(
            catalog
                .patch_bucket(
                    "bucket",
                    BucketMetadataUpdate {
                        lifecycle: Some(crate::bucket::lifecycle::LifecycleRules::for_test(vec![
                            incompatible
                        ])),
                        ..Default::default()
                    },
                    Default::default(),
                    PERMISSIVE
                )
                .await
                .is_err()
        );
        clock.advance(DAY);
        let outcome = execute(&catalog).await;
        assert_eq!(
            (outcome.scanned, outcome.applied(), outcome.next),
            (0, 0, None)
        );
        assert_eq!(current(&catalog, "object").await, original);
        assert!(
            catalog
                .get_bucket("bucket", PERMISSIVE)
                .await
                .unwrap()
                .unwrap()
                .lifecycle
                .is_empty()
        );
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn delete_precedence_and_coldest_target_do_not_depend_on_rule_order() {
    for deleting in [false, true] {
        for reversed in [false, true] {
            let (catalog, clock) = memory().await;
            let mut configured = vec![
                transition(StorageClass::Nearline, json!({"age":1})),
                transition(StorageClass::Coldline, json!({"age":1})),
            ];
            if deleting {
                configured.push(delete(json!({"age":1})));
            }
            if reversed {
                configured.reverse();
            }
            create(&catalog, configured).await;
            let original = put(&catalog, "object").await;
            clock.advance(DAY);
            counts(
                execute(&catalog).await,
                u64::from(!deleting),
                u64::from(deleting),
                0,
                Some(clock.now() + COALESCE),
            );
            if deleting {
                assert!(!exists(&catalog, live("bucket", "object")).await);
            } else {
                let changed = current(&catalog, "object").await;
                assert_eq!(changed.storage_class, StorageClass::Coldline);
                assert_eq!(
                    changed.metageneration.get(),
                    original.metageneration.get() + 1
                );
            }
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn transitions_chain_into_class_dependent_delete_in_one_pass_or_at_a_later_deadline() {
    for delete_age in [1, 2] {
        let (catalog, clock) = memory().await;
        create(
            &catalog,
            vec![
                transition(
                    StorageClass::Nearline,
                    json!({"age":1,"matchesStorageClass":["STANDARD"]}),
                ),
                transition(
                    StorageClass::Coldline,
                    json!({"age":1,"matchesStorageClass":["NEARLINE"]}),
                ),
                delete(json!({"age":delete_age,"matchesStorageClass":["COLDLINE"]})),
            ],
        )
        .await;
        let original = put(&catalog, "object").await;
        let due = original.time_created + DAY;
        clock.set(due - MICROSECOND);
        counts(execute(&catalog).await, 0, 0, 0, Some(due));
        assert_eq!(current(&catalog, "object").await, original);
        clock.set(due);
        counts(
            execute(&catalog).await,
            2,
            u64::from(delete_age == 1),
            0,
            Some(due + COALESCE),
        );
        if delete_age == 2 {
            let changed = current(&catalog, "object").await;
            assert_eq!(changed.storage_class, StorageClass::Coldline);
            assert_eq!(
                changed.metageneration.get(),
                original.metageneration.get() + 2
            );
            let delete_due = original.time_created + DAY * 2;
            clock.set(delete_due - MICROSECOND);
            counts(execute(&catalog).await, 0, 0, 0, Some(delete_due));
            assert_eq!(current(&catalog, "object").await, changed);
            clock.set(delete_due);
            counts(
                execute(&catalog).await,
                0,
                1,
                0,
                Some(delete_due + COALESCE),
            );
        }
        assert!(!exists(&catalog, live("bucket", "object")).await);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn class_dependent_delete_without_a_transition_uses_the_stored_class() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![delete(json!({"age":1,"matchesStorageClass":["STANDARD"]}))],
    )
    .await;
    let original = put(&catalog, "object").await;
    let due = original.time_created + DAY;
    clock.set(due - MICROSECOND);
    counts(execute(&catalog).await, 0, 0, 0, Some(due));
    assert_eq!(current(&catalog, "object").await, original);
    clock.set(due);
    counts(execute(&catalog).await, 0, 1, 0, Some(due + COALESCE));
    assert!(!exists(&catalog, live("bucket", "object")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn holds_allow_transitions_but_a_matching_blocked_delete_suppresses_them() {
    for event_hold in [false, true] {
        for deleting in [false, true] {
            let (catalog, clock) = memory().await;
            let mut configured = vec![transition(StorageClass::Coldline, json!({"age":1}))];
            if deleting {
                configured.push(delete(json!({"age":1})));
            }
            create(&catalog, configured).await;
            let mut upload = permissive_upload("bucket", "object", 100);
            upload.metadata = ObjectMetadataUpdate {
                temporary_hold: Some(!event_hold),
                event_based_hold: Some(event_hold),
                ..Default::default()
            };
            let original = catalog
                .put_object(upload, body(vec![Bytes::from_static(b"data")]))
                .await
                .unwrap();
            let due = original.time_created + DAY;
            clock.set(due - MICROSECOND);
            counts(execute(&catalog).await, 0, 0, 0, Some(due));
            assert_eq!(current(&catalog, "object").await, original);
            clock.set(due);
            counts(
                execute(&catalog).await,
                u64::from(!deleting),
                0,
                u64::from(deleting),
                (!deleting).then_some(due + COALESCE),
            );
            let changed = current(&catalog, "object").await;
            assert_eq!(
                changed.storage_class,
                if deleting {
                    StorageClass::Standard
                } else {
                    StorageClass::Coldline
                }
            );
            assert_eq!(changed.event_based_hold, original.event_based_hold);
            assert_eq!(changed.temporary_hold, original.temporary_hold);
            if deleting {
                assert_eq!(changed, original);
                catalog
                    .update_object_metadata(
                        live("bucket", "object"),
                        Default::default(),
                        ObjectMetadataUpdate {
                            temporary_hold: Some(false),
                            event_based_hold: Some(false),
                            ..Default::default()
                        },
                        |_| Ok(()),
                        PERMISSIVE,
                    )
                    .await
                    .unwrap();
                counts(execute(&catalog).await, 0, 1, 0, Some(due + COALESCE));
                assert!(!exists(&catalog, live("bucket", "object")).await);
            } else {
                counts(execute(&catalog).await, 0, 0, 0, None);
            }
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn retention_allows_transitions_and_delays_a_matching_delete_to_exact_expiry() {
    for object_retention in [false, true] {
        for deleting in [false, true] {
            let (catalog, clock) = memory().await;
            let until = clock.now() + 2 * DAY;
            let mut record = bucket("bucket", "project");
            record.object_retention_enabled = object_retention;
            if !object_retention {
                record.retention_policy = Some(crate::bucket::RetentionPolicy {
                    retention_period: 2 * DAY.as_secs(),
                    effective_time: clock.now(),
                    is_locked: true,
                });
            }
            let mut configured = vec![transition(StorageClass::Coldline, json!({"age":1}))];
            if deleting {
                configured.push(delete(json!({"age":1})));
            }
            record.lifecycle =
                crate::bucket::lifecycle::LifecycleRules::for_test(configured).into();
            catalog.insert_bucket(record).await.unwrap();
            let mut upload = permissive_upload("bucket", "object", 100);
            if object_retention {
                upload.metadata.retention = Patch::Set(ObjectRetention {
                    mode: RetentionMode::Locked,
                    retain_until: until.into(),
                });
            }
            let original = catalog
                .put_object(upload, body(vec![Bytes::from_static(b"data")]))
                .await
                .unwrap();
            let due = original.time_created + DAY;
            clock.set(due - MICROSECOND);
            counts(execute(&catalog).await, 0, 0, 0, Some(due));
            assert_eq!(current(&catalog, "object").await, original);
            clock.set(due);
            counts(
                execute(&catalog).await,
                u64::from(!deleting),
                0,
                u64::from(deleting),
                Some(if deleting { until } else { due + COALESCE }),
            );
            let changed = current(&catalog, "object").await;
            assert_eq!(
                changed.storage_class,
                if deleting {
                    StorageClass::Standard
                } else {
                    StorageClass::Coldline
                }
            );
            assert_eq!(changed.retention, original.retention);
            assert_eq!(
                changed.retention_expiration_time,
                original.retention_expiration_time
            );
            // A subsequent class-dependent Delete still obeys the same protection deadline.
            rules(
                &catalog,
                vec![delete(
                    json!({"age":1,"matchesStorageClass":[changed.storage_class]}),
                )],
            )
            .await;
            clock.set(until - MICROSECOND);
            counts(execute(&catalog).await, 0, 0, 1, Some(until));
            assert_eq!(current(&catalog, "object").await, changed);
            clock.set(until);
            counts(execute(&catalog).await, 0, 1, 0, Some(until + COALESCE));
            assert!(!exists(&catalog, live("bucket", "object")).await);
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn a_class_dependent_delete_yields_to_further_transitions_during_retention() {
    let (catalog, clock) = memory().await;
    let until = clock.now() + 2 * DAY;
    let mut record = bucket("bucket", "project");
    record.retention_policy = Some(crate::bucket::RetentionPolicy {
        retention_period: 2 * DAY.as_secs(),
        effective_time: clock.now(),
        is_locked: true,
    });
    record.lifecycle = crate::bucket::lifecycle::LifecycleRules::for_test(vec![
        transition(
            StorageClass::Nearline,
            json!({"age":1,"matchesStorageClass":["STANDARD"]}),
        ),
        transition(
            StorageClass::Coldline,
            json!({"age":1,"matchesStorageClass":["NEARLINE"]}),
        ),
        delete(json!({"age":1,"matchesStorageClass":["NEARLINE"]})),
    ])
    .into();
    catalog.insert_bucket(record).await.unwrap();
    let original = put(&catalog, "object").await;
    clock.advance(DAY);
    counts(
        execute(&catalog).await,
        2,
        0,
        0,
        Some(clock.now() + COALESCE),
    );
    let changed = current(&catalog, "object").await;
    assert_eq!(changed.storage_class, StorageClass::Coldline);
    assert_eq!(
        changed.metageneration.get(),
        original.metageneration.get() + 2
    );
    counts(execute(&catalog).await, 0, 0, 0, None);
    clock.set(until);
    counts(execute(&catalog).await, 0, 0, 0, None);
    assert_eq!(current(&catalog, "object").await, changed);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn retention_blocks_delete_matching_a_transitioned_class_until_expiry() {
    let (catalog, clock) = memory().await;
    let until = clock.now() + 5 * DAY;
    let mut record = bucket("bucket", "project");
    record.retention_policy = Some(crate::bucket::RetentionPolicy {
        retention_period: 5 * DAY.as_secs(),
        effective_time: clock.now(),
        is_locked: true,
    });
    record.lifecycle = crate::bucket::lifecycle::LifecycleRules::for_test(vec![
        delete(json!({"age":1,"matchesStorageClass":["STANDARD","NEARLINE"]})),
        transition(StorageClass::Nearline, json!({"age":2})),
    ])
    .into();
    catalog.insert_bucket(record).await.unwrap();
    let original = put(&catalog, "object").await;
    clock.advance(2 * DAY);
    counts(
        execute(&catalog).await,
        1,
        0,
        1,
        Some(clock.now() + COALESCE),
    );
    let changed = current(&catalog, "object").await;
    assert_eq!(changed.storage_class, StorageClass::Nearline);
    assert_eq!(
        changed.metageneration.get(),
        original.metageneration.get() + 1
    );
    // Consume the mutation's dirty wake separately from the protection deadline.
    counts(execute(&catalog).await, 0, 0, 1, Some(until));
    clock.set(until - MICROSECOND);
    counts(execute(&catalog).await, 0, 0, 1, Some(until));
    assert_eq!(current(&catalog, "object").await, changed);
    clock.set(until);
    counts(execute(&catalog).await, 0, 1, 0, Some(until + COALESCE));
    assert!(!exists(&catalog, live("bucket", "object")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_hold_blocks_delete_after_transition_and_waits_for_the_next_transition() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![
            delete(json!({"age":1,"matchesStorageClass":["STANDARD","NEARLINE"]})),
            transition(StorageClass::Nearline, json!({"age":2})),
            transition(StorageClass::Coldline, json!({"age":3})),
        ],
    )
    .await;
    let mut upload = permissive_upload("bucket", "object", 100);
    upload.metadata.temporary_hold = Some(true);
    let original = catalog
        .put_object(upload, body(vec![Bytes::from_static(b"data")]))
        .await
        .unwrap();
    let due = original.time_created + 3 * DAY;
    clock.advance(2 * DAY);
    counts(
        execute(&catalog).await,
        1,
        0,
        1,
        Some(clock.now() + COALESCE),
    );
    assert_eq!(
        current(&catalog, "object").await.storage_class,
        StorageClass::Nearline
    );
    counts(execute(&catalog).await, 0, 0, 1, Some(due));
    clock.set(due - MICROSECOND);
    counts(execute(&catalog).await, 0, 0, 1, Some(due));
    clock.set(due);
    counts(execute(&catalog).await, 1, 0, 0, Some(due + COALESCE));
    let changed = current(&catalog, "object").await;
    assert_eq!(changed.storage_class, StorageClass::Coldline);
    assert_eq!(changed.temporary_hold, original.temporary_hold);
    counts(execute(&catalog).await, 0, 0, 0, None);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn transitions_update_live_and_noncurrent_generations_but_exclude_soft_deleted_versions() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.versioning = crate::bucket::Versioning::Enabled;
    record.soft_delete_policy = Some(SoftDeletePolicy {
        retention_duration_seconds: 604800,
        effective_time: clock.now(),
    });
    record.lifecycle = crate::bucket::lifecycle::LifecycleRules::for_test(vec![transition(
        StorageClass::Coldline,
        json!({"age":1}),
    )])
    .into();
    catalog.insert_bucket(record).await.unwrap();
    let removed = put(&catalog, "object").await;
    let old = put(&catalog, "object").await;
    let live_record = put(&catalog, "object").await;
    catalog
        .delete_object(
            version("bucket", "object", removed.generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let target = ObjectTarget::new("bucket", "object", removed.generation.get());
    let soft_deleted = catalog
        .get_soft_deleted_object(target.clone(), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    clock.advance(DAY);
    let outcome = execute(&catalog).await;
    assert_eq!(
        (
            outcome.scanned,
            outcome.transitioned,
            outcome.deleted,
            outcome.archived,
            outcome.blocked,
            outcome.skipped,
            outcome.next
        ),
        (2, 2, 0, 0, 0, 0, Some(clock.now() + COALESCE))
    );
    for original in [&old, &live_record] {
        let changed = catalog
            .get_object(
                version("bucket", "object", original.generation),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert_eq!(changed.storage_class, StorageClass::Coldline);
        assert_eq!(
            changed.metageneration.get(),
            original.metageneration.get() + 1
        );
        assert_eq!(
            changed.time_deleted.is_some(),
            original.generation == old.generation
        );
    }
    assert_eq!(
        catalog
            .get_soft_deleted_object(target, Default::default(), PERMISSIVE)
            .await
            .unwrap(),
        soft_deleted
    );
    assert_eq!(
        current(&catalog, "object").await.generation,
        live_record.generation
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn transition_then_versioned_delete_archives_once_and_deletes_on_the_next_pass() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.versioning = crate::bucket::Versioning::Enabled;
    record.lifecycle = crate::bucket::lifecycle::LifecycleRules::for_test(vec![
        transition(StorageClass::Coldline, json!({"age":1})),
        delete(json!({"age":1,"matchesStorageClass":["COLDLINE"]})),
    ])
    .into();
    catalog.insert_bucket(record).await.unwrap();
    let original = put(&catalog, "object").await;
    clock.advance(DAY);
    let first = execute(&catalog).await;
    assert_eq!(
        (
            first.scanned,
            first.transitioned,
            first.archived,
            first.deleted,
            first.next
        ),
        (1, 1, 1, 0, Some(clock.now() + COALESCE))
    );
    assert!(!exists(&catalog, live("bucket", "object")).await);
    let archived = catalog
        .get_object(
            version("bucket", "object", original.generation),
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(archived.storage_class, StorageClass::Coldline);
    assert_eq!(archived.time_deleted, Some(clock.now()));
    counts(
        execute(&catalog).await,
        0,
        1,
        0,
        Some(clock.now() + COALESCE),
    );
    assert!(!exists(&catalog, version("bucket", "object", original.generation)).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn each_chained_action_counts_toward_the_pass_limit_and_resumes_next_pass() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![
            transition(
                StorageClass::Nearline,
                json!({"age":1,"matchesStorageClass":["STANDARD"]}),
            ),
            transition(
                StorageClass::Coldline,
                json!({"age":1,"matchesStorageClass":["NEARLINE"]}),
            ),
            delete(json!({"age":1,"matchesStorageClass":["COLDLINE"]})),
        ],
    )
    .await;
    for index in 0..342 {
        put(&catalog, &format!("object-{index:03}")).await;
    }
    clock.advance(DAY);
    let first = execute(&catalog).await;
    assert_eq!(
        (
            first.scanned,
            first.transitioned,
            first.deleted,
            first.archived,
            first.blocked,
            first.skipped,
            first.next
        ),
        (342, 683, 341, 0, 0, 0, Some(clock.now()))
    );
    assert_eq!(first.applied(), ACTION_LIMIT);
    let remaining = current(&catalog, "object-000").await;
    assert_eq!(remaining.storage_class, StorageClass::Nearline);
    assert_eq!(remaining.metageneration.get(), 2);
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 1);
    counts(
        execute(&catalog).await,
        1,
        1,
        0,
        Some(clock.now() + COALESCE),
    );
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 0);
    catalog.close().await.unwrap();
}

/// The sweep hands each version's creation, custom-time, noncurrent and size facts to transitions
/// exactly as it does to Delete; prefixes keep every rule to its own objects.
#[tokio::test]
async fn transitions_use_each_versions_creation_custom_time_noncurrent_and_size_facts() {
    let coldline = |prefix: &str, condition: Value| {
        let mut condition = condition;
        condition["matchesPrefix"] = json!([prefix]);
        transition(StorageClass::Coldline, condition)
    };
    let (catalog, clock) = versioned(vec![
        coldline("created/", json!({"createdBefore":"1970-01-03"})),
        coldline("size/", json!({"sizeAboveBytes":4})),
        coldline("custom/", json!({"daysSinceCustomTime":1})),
        coldline("noncurrent/", json!({"daysSinceNoncurrentTime":1})),
    ])
    .await;
    let start = clock.now();
    put_data(&catalog, "created/object", b"data").await;
    put_data(&catalog, "size/large", b"large").await;
    put_data(&catalog, "size/exact", b"data").await;
    put_data(&catalog, "custom/unset", b"data").await;
    put_data(&catalog, "custom/dated", b"data").await;
    catalog
        .update_object_metadata(
            live("bucket", "custom/dated"),
            Default::default(),
            ObjectMetadataUpdate {
                custom_time: Patch::Set("1970-01-02T00:00:00Z".into()),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let archived = put_data(&catalog, "noncurrent/object", b"data").await;
    let live_version = put_data(&catalog, "noncurrent/object", b"data").await;

    // Calendar and size conditions match at once; day counts wait for their deadline.
    assert_eq!(execute(&catalog).await.transitioned, 2);
    for (name, expected) in [
        ("created/object", StorageClass::Coldline),
        ("size/large", StorageClass::Coldline),
        ("size/exact", StorageClass::Standard),
    ] {
        assert_eq!(
            current(&catalog, name).await.storage_class,
            expected,
            "{name}"
        );
    }
    let due = start + DAY;
    clock.set(due - MICROSECOND);
    assert_eq!(execute(&catalog).await.transitioned, 0);
    assert_eq!(
        current(&catalog, "custom/dated").await.storage_class,
        StorageClass::Standard
    );
    clock.set(due);
    assert_eq!(execute(&catalog).await.transitioned, 2);
    for (target, expected) in [
        (live("bucket", "custom/dated"), StorageClass::Coldline),
        (live("bucket", "custom/unset"), StorageClass::Standard),
        (
            version("bucket", "noncurrent/object", archived.generation),
            StorageClass::Coldline,
        ),
        (
            version("bucket", "noncurrent/object", live_version.generation),
            StorageClass::Standard,
        ),
    ] {
        let found = catalog
            .get_object(target, Default::default(), PERMISSIVE)
            .await
            .unwrap();
        assert_eq!(found.storage_class, expected, "{}", found.name);
    }
    clock.advance(10 * DAY);
    assert_eq!(execute(&catalog).await.transitioned, 0);
    catalog.close().await.unwrap();
}
