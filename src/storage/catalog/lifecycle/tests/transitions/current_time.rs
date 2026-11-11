use super::*;
use crate::config::{Config, LifecycleExecution};

fn old_class_delete_rules() -> Vec<Value> {
    vec![
        transition(StorageClass::Nearline, json!({"age":30})),
        delete(json!({"age":60,"matchesStorageClass":["STANDARD"]})),
    ]
}

#[tokio::test]
async fn a_late_first_pass_transitions_before_an_old_class_delete() {
    for day in [60, 90, 100] {
        for reverse in [false, true] {
            let (catalog, clock) = memory().await;
            let mut configured = old_class_delete_rules();
            if reverse {
                configured.reverse();
            }
            create(&catalog, configured).await;
            let original = put(&catalog, "object").await;
            clock.advance(DAY * day);
            let outcome = execute(&catalog).await;
            assert_eq!((outcome.transitioned, outcome.deleted), (1, 0));
            let changed = current(&catalog, "object").await;
            assert_eq!(changed.generation, original.generation);
            assert_eq!(changed.storage_class, StorageClass::Nearline);
            assert_eq!(changed.storage_class_updated(), clock.now());
            assert_eq!(execute(&catalog).await.applied(), 0);
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn enabling_or_replacing_rules_evaluates_current_class_candidates() {
    for mode in ["disabled", "sync-default", "replace"] {
        let (catalog, clock) = memory().await;
        create(&catalog, old_class_delete_rules()).await;
        let original = put(&catalog, "object").await;
        let tree = TestDir::new();
        let config = Config {
            sync_dir: (mode == "sync-default").then(|| tree.0.clone()),
            lifecycle_execution: (mode == "disabled").then_some(LifecycleExecution::Disabled),
            ..Default::default()
        };
        catalog.configure_lifecycle(&config);
        clock.advance(DAY * 100);
        if mode == "replace" {
            rules(
                &catalog,
                old_class_delete_rules().into_iter().rev().collect(),
            )
            .await;
        } else {
            assert!(matches!(
                catalog.run_lifecycle_now().await.unwrap(),
                LifecycleOutcome::Disabled
            ));
            assert_eq!(current(&catalog, "object").await, original);
            catalog.configure_lifecycle(&Config {
                lifecycle_execution: Some(LifecycleExecution::Enabled),
                ..config
            });
        }
        let outcome = execute(&catalog).await;
        assert_eq!((outcome.transitioned, outcome.deleted), (1, 0), "{mode}");
        assert_eq!(
            current(&catalog, "object").await.storage_class,
            StorageClass::Nearline
        );
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_class_dependent_delete_yields_only_to_an_applicable_transition() {
    for (target, age, expected) in [
        (StorageClass::Standard, 30, (0, 1)),
        (StorageClass::MultiRegional, 30, (0, 1)),
        (StorageClass::Nearline, 100, (0, 1)),
        (StorageClass::Nearline, 60, (1, 0)),
    ] {
        let (catalog, clock) = memory().await;
        create(
            &catalog,
            vec![
                transition(target, json!({"age":age})),
                delete(json!({"age":60,"matchesStorageClass":["STANDARD"]})),
            ],
        )
        .await;
        put(&catalog, "object").await;
        clock.advance(DAY * 60);
        let outcome = execute(&catalog).await;
        assert_eq!((outcome.transitioned, outcome.deleted), expected);
        assert_eq!(
            exists(&catalog, live("bucket", "object")).await,
            expected.1 == 0
        );
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn an_independent_delete_wins_even_when_a_class_dependent_delete_also_matches() {
    for reverse in [false, true] {
        let (catalog, clock) = memory().await;
        let mut configured = old_class_delete_rules();
        configured.push(delete(json!({"age":60})));
        if reverse {
            configured.reverse();
        }
        create(&catalog, configured).await;
        put(&catalog, "object").await;
        clock.advance(DAY * 100);
        let outcome = execute(&catalog).await;
        assert_eq!((outcome.transitioned, outcome.deleted), (0, 1));
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_delete_matching_the_resulting_class_still_runs_in_the_same_transaction() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![
            transition(StorageClass::Nearline, json!({"age":30})),
            delete(json!({"age":60,"matchesStorageClass":["STANDARD","NEARLINE"]})),
        ],
    )
    .await;
    put(&catalog, "object").await;
    clock.advance(DAY * 100);
    let outcome = execute(&catalog).await;
    assert_eq!((outcome.transitioned, outcome.deleted), (1, 1));
    assert!(!exists(&catalog, live("bucket", "object")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_tiny_action_budget_stops_mid_chain_and_resumes_from_the_stored_class() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![
            transition(
                StorageClass::Nearline,
                json!({"age":30,"matchesStorageClass":["STANDARD"]}),
            ),
            transition(
                StorageClass::Coldline,
                json!({"age":60,"matchesStorageClass":["NEARLINE"]}),
            ),
            delete(json!({"age":90,"matchesStorageClass":["COLDLINE"]})),
        ],
    )
    .await;
    let first = put(&catalog, "object-a").await;
    put(&catalog, "object-b").await;
    clock.advance(DAY * 100);
    let now = clock.now();
    let incarnation = incarnation(&catalog).await;
    let outcome = catalog
        .run::<_, CatalogError>(move |connection| {
            sweep_page(
                connection,
                "bucket",
                &incarnation,
                None,
                now,
                1,
                BucketRules::Absent,
            )
            .map(|page| page.outcome)
            .map_err(unavailable)
        })
        .await
        .unwrap();
    assert_eq!(
        (
            outcome.scanned,
            outcome.transitioned,
            outcome.deleted,
            outcome.next
        ),
        (1, 1, 0, Some(now))
    );
    assert_eq!(current(&catalog, "object-a").await, first);
    assert_eq!(
        current(&catalog, "object-b").await.storage_class,
        StorageClass::Nearline
    );
    let resumed = execute(&catalog).await;
    assert_eq!((resumed.transitioned, resumed.deleted), (3, 2));
    assert_eq!(execute(&catalog).await.applied(), 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn protected_class_deletes_schedule_a_future_transition_before_retention_expires() {
    for gated in [false, true] {
        let (catalog, clock) = memory().await;
        let mut record = bucket("bucket", "project");
        record.retention_policy = Some(crate::bucket::RetentionPolicy {
            retention_period: 3 * DAY.as_secs(),
            effective_time: clock.now(),
            is_locked: true,
        });
        let mut condition = json!({"age":1});
        if gated {
            condition["matchesStorageClass"] = json!(["STANDARD"]);
        }
        record.lifecycle = vec![
            delete(condition),
            transition(StorageClass::Nearline, json!({"age":2})),
        ];
        catalog.insert_bucket(record).await.unwrap();
        let original = put(&catalog, "object").await;
        clock.advance(DAY);
        let blocked = execute(&catalog).await;
        assert_eq!(blocked.blocked, 1);
        assert_eq!(
            blocked.next,
            Some(original.time_created + DAY * if gated { 2 } else { 3 })
        );
        clock.advance(DAY);
        let due = execute(&catalog).await;
        assert_eq!(
            (due.transitioned, due.blocked),
            (u64::from(gated), u64::from(!gated))
        );
        clock.advance(DAY);
        assert_eq!(execute(&catalog).await.deleted, u64::from(!gated));
        assert_eq!(exists(&catalog, live("bucket", "object")).await, gated);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn removing_an_unapplied_transition_leaves_only_current_class_delete_rules() {
    let (catalog, clock) = memory().await;
    create(&catalog, old_class_delete_rules()).await;
    put(&catalog, "object").await;
    clock.advance(DAY * 90);
    rules(
        &catalog,
        vec![delete(json!({"age":60,"matchesStorageClass":["STANDARD"]}))],
    )
    .await;
    let outcome = execute(&catalog).await;
    assert_eq!((outcome.transitioned, outcome.deleted), (0, 1));
    assert!(!exists(&catalog, live("bucket", "object")).await);
    catalog.close().await.unwrap();
}
