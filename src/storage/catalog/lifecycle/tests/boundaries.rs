use super::*;

const MICROSECOND: Duration = Duration::from_micros(1);

fn assert_counts(
    outcome: LifecycleCounts,
    scanned: u64,
    deleted: u64,
    archived: u64,
    next: Option<SystemTime>,
) {
    assert_eq!(
        (
            outcome.scanned,
            outcome.deleted,
            outcome.archived,
            outcome.blocked,
            outcome.skipped,
            outcome.next,
        ),
        (scanned, deleted, archived, 0, 0, next)
    );
}

fn time(timestamp: &str) -> SystemTime {
    let (seconds, nanos) = crate::object::custom_time_key(timestamp).unwrap();
    crate::clock::system_time_from_unix_seconds(seconds).unwrap()
        + Duration::from_nanos(u64::from(nanos))
}

#[tokio::test]
async fn overflowing_days_never_delete_or_schedule_catalog_work() {
    for field in ["age", "daysSinceCustomTime", "daysSinceNoncurrentTime"] {
        for days in [i64::from(i32::MAX), i64::MAX / 86_400_000_000] {
            let (catalog, clock) = memory().await;
            clock.set(time("2026-01-10T10:00:00.123456Z"));
            let mut record = bucket("bucket", "project");
            record.versioning_enabled = true;
            catalog.insert_bucket(record).await.unwrap();
            let original = catalog
                .put_object(
                    crate::storage::ObjectUpload {
                        metadata: ObjectMetadataUpdate {
                            custom_time: Some(Some("2026-01-10T10:00:00.123456Z".into())),
                            ..Default::default()
                        },
                        ..permissive_upload("bucket", "object", 100)
                    },
                    body(vec![Bytes::from_static(b"data")]),
                )
                .await
                .unwrap();
            put(&catalog, "object").await;
            rules(&catalog, vec![delete(json!({field:days}))]).await;
            clock.advance(DAY);
            assert_counts(execute(&catalog).await, 2, 0, 0, None);
            assert!(exists(&catalog, version("bucket", "object", original.generation)).await);
            assert!(exists(&catalog, live("bucket", "object")).await);
            assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn creation_dates_at_extremes_and_leap_midnight_are_strict_in_the_sweep() {
    for (date, created, deleted) in [
        ("0001-01-01", "1970-01-02T00:00:00Z", 0),
        ("1900-03-01", "1970-01-02T00:00:00Z", 0),
        ("1969-12-31", "1970-01-02T00:00:00Z", 0),
        ("2000-02-29", "2000-02-29T00:00:00Z", 1),
        ("9999-12-31", "9999-12-31T00:00:00Z", 1),
    ] {
        let (catalog, clock) = memory().await;
        create(&catalog, vec![delete(json!({"createdBefore":date}))]).await;
        let midnight = time(created);
        clock.set(midnight - MICROSECOND);
        put(&catalog, "before").await;
        clock.set(midnight);
        put(&catalog, "equal").await;
        assert_counts(
            execute(&catalog).await,
            2,
            deleted,
            0,
            (deleted > 0).then_some(midnight + COALESCE),
        );
        assert_eq!(
            exists(&catalog, live("bucket", "before")).await,
            deleted == 0
        );
        assert!(exists(&catalog, live("bucket", "equal")).await);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn combined_time_conditions_use_the_latest_deadline_and_rules_use_the_earliest() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.versioning_enabled = true;
    catalog.insert_bucket(record).await.unwrap();
    let original = catalog
        .put_object(
            crate::storage::ObjectUpload {
                metadata: ObjectMetadataUpdate {
                    custom_time: Some(Some("1970-01-03T00:00:00Z".into())),
                    ..Default::default()
                },
                ..permissive_upload("bucket", "object", 100)
            },
            body(vec![Bytes::from_static(b"data")]),
        )
        .await
        .unwrap();
    clock.advance(DAY);
    put(&catalog, "object").await;
    rules(
        &catalog,
        vec![
            delete(json!({"age":1,"daysSinceCustomTime":2,"daysSinceNoncurrentTime":2})),
            delete(json!({"age":5})),
        ],
    )
    .await;
    let due = clock.now() + DAY * 2;
    assert_counts(execute(&catalog).await, 2, 0, 0, Some(due));
    clock.set(due - MICROSECOND);
    assert_counts(execute(&catalog).await, 2, 0, 0, Some(due));
    assert!(exists(&catalog, version("bucket", "object", original.generation)).await);
    assert!(exists(&catalog, live("bucket", "object")).await);
    clock.set(due);
    assert_counts(execute(&catalog).await, 2, 1, 0, Some(due + COALESCE));
    assert!(!exists(&catalog, version("bucket", "object", original.generation)).await);
    assert!(exists(&catalog, live("bucket", "object")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn zero_age_at_creation_midnight_waits_until_the_following_midnight() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![delete(json!({"age":0}))]).await;
    put(&catalog, "object").await;
    let due = clock.now() + DAY;
    assert_counts(execute(&catalog).await, 1, 0, 0, Some(due));
    clock.set(due - MICROSECOND);
    assert_counts(execute(&catalog).await, 1, 0, 0, Some(due));
    assert!(exists(&catalog, live("bucket", "object")).await);
    clock.set(due);
    assert_counts(execute(&catalog).await, 1, 1, 0, Some(due + COALESCE));
    assert!(!exists(&catalog, live("bucket", "object")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn zero_custom_days_and_past_custom_time_delete_at_the_exact_catalog_deadline() {
    for (days, custom) in [
        (0, "1970-01-05T12:00:00.000123Z"),
        (1, "1970-01-04T12:00:00.000123Z"),
    ] {
        let (catalog, clock) = memory().await;
        clock.set(UNIX_EPOCH + DAY * 4);
        create(&catalog, vec![delete(json!({"daysSinceCustomTime":days}))]).await;
        catalog
            .put_object(
                crate::storage::ObjectUpload {
                    metadata: ObjectMetadataUpdate {
                        custom_time: Some(Some(custom.into())),
                        ..Default::default()
                    },
                    ..permissive_upload("bucket", "object", 100)
                },
                body(vec![Bytes::from_static(b"data")]),
            )
            .await
            .unwrap();
        let due = time("1970-01-05T12:00:00.000123Z");
        assert_counts(execute(&catalog).await, 1, 0, 0, Some(due));
        clock.set(due - MICROSECOND);
        assert_counts(execute(&catalog).await, 1, 0, 0, Some(due));
        assert!(exists(&catalog, live("bucket", "object")).await);
        clock.set(due);
        assert_counts(execute(&catalog).await, 1, 1, 0, Some(due + COALESCE));
        assert!(!exists(&catalog, live("bucket", "object")).await);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn custom_time_one_microsecond_before_the_date_matches_but_midnight_does_not() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![delete(json!({"customTimeBefore":"1970-01-03"}))],
    )
    .await;
    for (name, custom) in [
        ("before", "1970-01-02T23:59:59.999999Z"),
        ("equal", "1970-01-03T00:00:00Z"),
    ] {
        catalog
            .put_object(
                crate::storage::ObjectUpload {
                    metadata: ObjectMetadataUpdate {
                        custom_time: Some(Some(custom.into())),
                        ..Default::default()
                    },
                    ..permissive_upload("bucket", name, 100)
                },
                body(vec![Bytes::from_static(b"data")]),
            )
            .await
            .unwrap();
    }
    assert_counts(
        execute(&catalog).await,
        2,
        1,
        0,
        Some(clock.now() + COALESCE),
    );
    assert!(!exists(&catalog, live("bucket", "before")).await);
    assert!(exists(&catalog, live("bucket", "equal")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn zero_noncurrent_days_delete_on_the_pass_after_archiving() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.versioning_enabled = true;
    record.lifecycle = vec![
        delete(json!({"age":1,"isLive":true})),
        delete(json!({"daysSinceNoncurrentTime":0})),
    ];
    catalog.insert_bucket(record).await.unwrap();
    let object = put(&catalog, "object").await;
    let due = clock.now() + DAY;
    clock.set(due - MICROSECOND);
    assert_counts(execute(&catalog).await, 1, 0, 0, Some(due));
    assert!(exists(&catalog, live("bucket", "object")).await);
    clock.set(due);
    assert_counts(execute(&catalog).await, 1, 0, 1, Some(due + COALESCE));
    assert!(!exists(&catalog, live("bucket", "object")).await);
    let target = version("bucket", "object", object.generation);
    assert!(exists(&catalog, target.clone()).await);
    assert_counts(execute(&catalog).await, 1, 1, 0, Some(due + COALESCE));
    assert!(!exists(&catalog, target).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn noncurrent_date_midnight_excludes_equality_but_includes_one_microsecond_before() {
    let (catalog, clock) = memory().await;
    let mut record = bucket("bucket", "project");
    record.versioning_enabled = true;
    catalog.insert_bucket(record).await.unwrap();
    let before = put(&catalog, "before").await;
    let equal = put(&catalog, "equal").await;
    let midnight = UNIX_EPOCH + DAY * 2;
    clock.set(midnight - MICROSECOND);
    put(&catalog, "before").await;
    clock.set(midnight);
    put(&catalog, "equal").await;
    rules(
        &catalog,
        vec![delete(json!({"noncurrentTimeBefore":"1970-01-03"}))],
    )
    .await;
    assert_counts(execute(&catalog).await, 4, 1, 0, Some(midnight + COALESCE));
    assert!(!exists(&catalog, version("bucket", "before", before.generation)).await);
    assert!(exists(&catalog, version("bucket", "equal", equal.generation)).await);
    assert!(exists(&catalog, live("bucket", "before")).await);
    assert!(exists(&catalog, live("bucket", "equal")).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn newer_version_thresholds_include_live_versions_and_work_without_a_live_version() {
    for with_live in [false, true] {
        for minimum in 0..=3 {
            let (catalog, clock) = memory().await;
            let mut record = bucket("bucket", "project");
            record.versioning_enabled = true;
            catalog.insert_bucket(record).await.unwrap();
            let mut generations = Vec::new();
            for _ in 0..3 {
                generations.push(put(&catalog, "object").await.generation);
            }
            if !with_live {
                catalog
                    .delete_object(live("bucket", "object"), Default::default(), PERMISSIVE)
                    .await
                    .unwrap();
            }
            rules(
                &catalog,
                vec![delete(json!({"isLive":false,"numNewerVersions":minimum}))],
            )
            .await;
            let deleted = (0..3)
                .filter(|index| (!with_live || *index < 2) && 2 - index >= minimum)
                .count() as u64;
            assert_counts(
                execute(&catalog).await,
                3,
                deleted,
                0,
                (deleted > 0).then_some(clock.now() + COALESCE),
            );
            for (index, generation) in generations.into_iter().enumerate() {
                let removed = (!with_live || index < 2) && 2 - index >= minimum;
                assert_eq!(
                    exists(&catalog, version("bucket", "object", generation)).await,
                    !removed
                );
            }
            assert_eq!(exists(&catalog, live("bucket", "object")).await, with_live);
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn stored_body_sizes_use_strict_thresholds_at_equality_and_each_adjacent_byte() {
    for (field, matching) in [("sizeAboveBytes", 5), ("sizeBelowBytes", 3)] {
        let (catalog, clock) = memory().await;
        create(&catalog, vec![delete(json!({field:"4"}))]).await;
        for size in [3, 4, 5] {
            catalog
                .put_object(
                    permissive_upload("bucket", &size.to_string(), 100),
                    body(vec![Bytes::from(vec![b'x'; size])]),
                )
                .await
                .unwrap();
        }
        assert_counts(
            execute(&catalog).await,
            3,
            1,
            0,
            Some(clock.now() + COALESCE),
        );
        for size in [3, 4, 5] {
            assert_eq!(
                exists(&catalog, live("bucket", &size.to_string())).await,
                size != matching
            );
        }
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn small_bodies_compare_against_thresholds_at_and_near_five_tebibytes() {
    for threshold in [5 * 1024_u64.pow(4) - 1, 5 * 1024_u64.pow(4)] {
        for field in ["sizeAboveBytes", "sizeBelowBytes"] {
            let (catalog, clock) = memory().await;
            create(&catalog, vec![delete(json!({field:threshold.to_string()}))]).await;
            put(&catalog, "object").await;
            let deleted = u64::from(field == "sizeBelowBytes");
            assert_counts(
                execute(&catalog).await,
                1,
                deleted,
                0,
                (deleted > 0).then_some(clock.now() + COALESCE),
            );
            assert_eq!(
                exists(&catalog, live("bucket", "object")).await,
                deleted == 0
            );
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn catalog_sizes_around_five_tebibytes_use_small_bodies_and_strict_comparisons() {
    let limit = 5 * 1024_i64.pow(4);
    for (field, matching) in [("sizeAboveBytes", limit + 1), ("sizeBelowBytes", limit - 1)] {
        let data = TestDir::new();
        let (catalog, clock) = disk(&data).await;
        create(&catalog, vec![delete(json!({field:limit.to_string()}))]).await;
        for size in [limit - 1, limit, limit + 1] {
            put(&catalog, &size.to_string()).await;
            // Only the recorded size is synthetic; no large upload or body allocation occurs.
            catalog
                .run::<_, CatalogError>(move |connection| {
                    sql::exec(
                        connection,
                        "UPDATE objects SET size = ?1 WHERE bucket = 'bucket' AND name = ?2",
                        params![size, size.to_string()],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        assert_counts(
            execute(&catalog).await,
            3,
            1,
            0,
            Some(clock.now() + COALESCE),
        );
        for size in [limit - 1, limit, limit + 1] {
            assert_eq!(
                exists(&catalog, live("bucket", &size.to_string())).await,
                size != matching
            );
        }
        catalog.close().await.unwrap();
    }
}
