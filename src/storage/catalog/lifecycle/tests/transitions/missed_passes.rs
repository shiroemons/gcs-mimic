use super::*;

#[tokio::test]
async fn a_delayed_pass_can_delete_an_old_version_after_removing_its_transition_evidence() {
    for on_time in [true, false] {
        let (catalog, clock) = versioned(vec![
            transition(
                StorageClass::Nearline,
                json!({"age":15,"numNewerVersions":2}),
            ),
            delete(json!({"daysSinceNoncurrentTime":20,"matchesStorageClass":["STANDARD"]})),
        ])
        .await;
        let first = put(&catalog, "object").await;
        let created = first.time_created;
        clock.set(created + DAY * 5);
        let second = put(&catalog, "object").await;
        clock.set(created + DAY * 10);
        let third = put(&catalog, "object").await;
        assert_eq!(execute(&catalog).await.applied(), 0);
        // All client mutations precede the stop; only the maintenance schedule differs.
        if on_time {
            for day in 11..=49 {
                clock.set(created + DAY * day);
                execute(&catalog).await;
            }
        }
        clock.set(created + DAY * 50);
        let outcome = execute(&catalog).await;
        assert_eq!(
            (outcome.transitioned, outcome.deleted),
            (0, if on_time { 0 } else { 2 })
        );
        assert_eq!(
            exists(&catalog, version("bucket", "object", first.generation)).await,
            on_time
        );
        if on_time {
            assert_eq!(
                selected(&catalog, first.generation).await.storage_class,
                StorageClass::Nearline
            );
        }
        assert!(!exists(&catalog, version("bucket", "object", second.generation)).await);
        assert_eq!(
            selected(&catalog, third.generation).await.storage_class,
            StorageClass::Standard
        );
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn an_overwrite_during_a_stop_can_remove_a_live_only_transition_opportunity() {
    for on_time in [true, false] {
        let (catalog, clock) = versioned(vec![
            transition(StorageClass::Nearline, json!({"age":30,"isLive":true})),
            delete(json!({"daysSinceNoncurrentTime":1,"matchesStorageClass":["STANDARD"]})),
        ])
        .await;
        let first = put(&catalog, "object").await;
        clock.advance(DAY * 30);
        if on_time {
            assert_eq!(execute(&catalog).await.transitioned, 1);
        }
        clock.advance(DAY * 10);
        put(&catalog, "object").await;
        clock.advance(DAY * 5);
        let outcome = execute(&catalog).await;
        assert_eq!(
            (outcome.transitioned, outcome.deleted),
            (0, u64::from(!on_time))
        );
        assert_eq!(
            exists(&catalog, version("bucket", "object", first.generation)).await,
            on_time
        );
        if on_time {
            assert_eq!(
                selected(&catalog, first.generation).await.storage_class,
                StorageClass::Nearline
            );
        }
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_delayed_coldest_transition_can_take_a_class_path_that_enables_delete() {
    for on_time in [true, false] {
        let (catalog, clock) = memory().await;
        create(
            &catalog,
            vec![
                transition(
                    StorageClass::Nearline,
                    json!({"age":10,"matchesStorageClass":["STANDARD"]}),
                ),
                transition(
                    StorageClass::Coldline,
                    json!({"age":20,"matchesStorageClass":["STANDARD"]}),
                ),
                delete(json!({"age":30,"matchesStorageClass":["COLDLINE"]})),
            ],
        )
        .await;
        let original = put(&catalog, "object").await;
        if on_time {
            for day in 0..30 {
                clock.set(original.time_created + DAY * day);
                execute(&catalog).await;
            }
        }
        clock.set(original.time_created + DAY * 30);
        let outcome = execute(&catalog).await;
        assert_eq!(
            (outcome.transitioned, outcome.deleted),
            if on_time { (0, 0) } else { (1, 1) }
        );
        assert_eq!(exists(&catalog, live("bucket", "object")).await, on_time);
        if on_time {
            assert_eq!(
                current(&catalog, "object").await.storage_class,
                StorageClass::Nearline
            );
        }
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_delayed_pass_can_keep_a_version_that_an_earlier_class_delete_would_remove() {
    for on_time in [true, false] {
        let (catalog, clock) = memory().await;
        create(
            &catalog,
            vec![
                transition(StorageClass::Nearline, json!({"age":30})),
                transition(
                    StorageClass::Coldline,
                    json!({"age":90,"matchesStorageClass":["NEARLINE"]}),
                ),
                delete(json!({"age":60,"matchesStorageClass":["NEARLINE"]})),
            ],
        )
        .await;
        let original = put(&catalog, "object").await;
        if on_time {
            for day in 0..100 {
                clock.set(original.time_created + DAY * day);
                execute(&catalog).await;
            }
        }
        clock.set(original.time_created + DAY * 100);
        let outcome = execute(&catalog).await;
        assert_eq!(
            (outcome.transitioned, outcome.deleted),
            if on_time { (0, 0) } else { (2, 0) }
        );
        assert_eq!(exists(&catalog, live("bucket", "object")).await, !on_time);
        if !on_time {
            assert_eq!(
                current(&catalog, "object").await.storage_class,
                StorageClass::Coldline
            );
        }
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn descending_scans_remove_successors_before_testing_an_older_delete_threshold() {
    for on_time in [true, false] {
        let (catalog, clock) = versioned(vec![
            delete(json!({"numNewerVersions":2,"age":30})),
            delete(json!({"isLive":false,"matchesStorageClass":["COLDLINE"],"daysSinceNoncurrentTime":1})),
        ]).await;
        let first = put(&catalog, "object").await;
        clock.advance(DAY * 5);
        let second = put_class(&catalog, "object", StorageClass::Coldline).await;
        clock.advance(DAY * 5);
        let third = put(&catalog, "object").await;
        assert_eq!(execute(&catalog).await.applied(), 0);
        if on_time {
            for day in 11..50 {
                clock.set(first.time_created + DAY * day);
                execute(&catalog).await;
            }
        }
        clock.set(first.time_created + DAY * 50);
        assert_eq!(execute(&catalog).await.deleted, u64::from(!on_time));
        assert_eq!(
            selected(&catalog, first.generation).await.storage_class,
            StorageClass::Standard
        );
        assert!(!exists(&catalog, version("bucket", "object", second.generation)).await);
        assert_eq!(current(&catalog, "object").await, third);
        catalog.close().await.unwrap();
    }
}
