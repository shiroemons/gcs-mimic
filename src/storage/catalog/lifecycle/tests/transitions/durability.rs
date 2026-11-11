use super::*;

#[tokio::test]
async fn failed_transition_update_rolls_back_the_page_and_retries() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![transition(StorageClass::Coldline, json!({"age":1}))],
    )
    .await;
    let good = put(&catalog, "z-good").await;
    let failed = put(&catalog, "a-fails").await;
    notifications(&catalog).await;
    let fence = catalog.enable_change_journal().await.unwrap();
    clock.advance(DAY);
    catalog.run::<_, CatalogError>(|connection| {
        sql::exec(connection, "CREATE TEMP TRIGGER fail_transition BEFORE UPDATE OF storage_class ON objects WHEN old.name = 'a-fails' BEGIN SELECT RAISE(ABORT, 'injected transition write failure'); END", [])?;
        Ok(())
    }).await.unwrap();
    assert!(catalog.run_lifecycle_now().await.is_err());
    assert_eq!(current(&catalog, "z-good").await, good);
    assert_eq!(current(&catalog, "a-fails").await, failed);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM notification_outbox").await,
        0
    );
    assert_eq!(
        catalog
            .read_changes(Some(fence), None, 256)
            .await
            .unwrap()
            .items
            .len(),
        0
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM lifecycle_quarantine").await,
        0
    );
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec(connection, "DROP TRIGGER fail_transition", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(execute(&catalog).await.transitioned, 2);
    assert_eq!(
        current(&catalog, "a-fails").await.storage_class,
        StorageClass::Coldline
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn failed_transition_commit_preserves_metadata_events_and_journal_for_retry() {
    let data = TestDir::new();
    let (catalog, clock) = disk(&data).await;
    create(
        &catalog,
        vec![transition(StorageClass::Coldline, json!({"age":1}))],
    )
    .await;
    let original = put(&catalog, "object").await;
    let original_blob = blob(&catalog, "object").await;
    notifications(&catalog).await;
    let fence = catalog.enable_change_journal().await.unwrap();
    clock.advance(DAY);
    fault::arm(
        &data.0,
        Step::LifecycleCommit,
        0,
        Fault::Io(io::ErrorKind::Other),
    );
    let error = catalog.maintain_resumable_sessions().await.unwrap_err();
    assert!(std::error::Error::source(&error).is_some());
    assert!(catalog.resumable_maintenance_error().is_some());
    assert_eq!(current(&catalog, "object").await, original);
    assert_eq!(blob(&catalog, "object").await, original_blob);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM notification_outbox").await,
        0
    );
    assert_eq!(
        catalog
            .read_changes(Some(fence), None, 256)
            .await
            .unwrap()
            .items
            .len(),
        0
    );
    counts(
        execute(&catalog).await,
        1,
        0,
        0,
        Some(clock.now() + COALESCE),
    );
    assert_eq!(
        current(&catalog, "object").await.storage_class,
        StorageClass::Coldline
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM notification_outbox").await,
        1
    );
    assert_eq!(
        catalog
            .read_changes(Some(fence), None, 256)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn transition_crash_and_power_loss_restart_converge_without_rewriting_or_duplicate_events() {
    for crash in Crash::ALL {
        for step in [Step::LifecycleCommit, Step::LifecycleCommitted] {
            let data = TestDir::new();
            let snapshot = TestDir::new();
            let (catalog, clock) = disk(&data).await;
            create(
                &catalog,
                vec![transition(StorageClass::Coldline, json!({"age":1}))],
            )
            .await;
            let original = put(&catalog, "object").await;
            let original_blob = blob(&catalog, "object").await;
            notifications(&catalog).await;
            let fence = catalog.enable_change_journal().await.unwrap();
            clock.advance(DAY);
            fault::arm(&data.0, step, 0, crash.fault(snapshot.0.clone()));
            assert_eq!(execute(&catalog).await.transitioned, 1);
            assert_eq!(fault::pending(&data.0), 0);
            catalog.close().await.unwrap();
            let recovered = Catalog::open_with_clock(&snapshot.0, clock.clone())
                .await
                .unwrap();
            recovered.configure_lifecycle(&Default::default());
            assert!(
                recovered
                    .read_changes(Some(fence), None, 256)
                    .await
                    .unwrap()
                    .rescan_required
            );
            let restarted = recovered.enable_change_journal().await.unwrap();
            assert_eq!(
                execute(&recovered).await.transitioned,
                u64::from(step == Step::LifecycleCommit)
            );
            counts(execute(&recovered).await, 0, 0, 0, None);
            let changed = current(&recovered, "object").await;
            assert_eq!(changed.generation, original.generation);
            assert_eq!(changed.metageneration, original.metageneration + 1);
            assert_eq!(changed.storage_class, StorageClass::Coldline);
            assert_eq!(changed.storage_class_updated(), clock.now());
            assert_eq!(blob(&recovered, "object").await, original_blob);
            assert_eq!(
                count(&recovered, "SELECT count(*) FROM pending_blob_deletions").await,
                0
            );
            let page = recovered
                .read_changes(Some(restarted), None, 256)
                .await
                .unwrap();
            // Opening starts a new journal epoch; committed rows survive but need a rescan.
            assert_eq!(page.items.len(), usize::from(step == Step::LifecycleCommit));
            assert_eq!(
                count(
                    &recovered,
                    "SELECT count(*) FROM change_journal WHERE kind = 'metadata'"
                )
                .await,
                1
            );
            let pending = recovered.next_notification().await.unwrap().unwrap();
            let message: Value = serde_json::from_str(&pending.message).unwrap();
            let event = &message["messages"][0];
            assert_eq!(event["attributes"]["eventType"], "OBJECT_METADATA_UPDATE");
            let payload = crate::base64::decode(
                event["data"].as_str().unwrap(),
                crate::base64::Alphabet::Standard,
                crate::base64::Padding::Required,
            )
            .unwrap();
            let payload: Value = serde_json::from_slice(&payload).unwrap();
            assert_eq!(payload["storageClass"], "COLDLINE");
            assert_eq!(payload["generation"], original.generation.to_string());
            assert_eq!(
                payload["metageneration"],
                changed.metageneration.to_string()
            );
            assert_eq!(
                payload["timeStorageClassUpdated"],
                crate::json::timestamp(clock.now()).unwrap()
            );
            recovered
                .acknowledge_notification(pending.id)
                .await
                .unwrap();
            assert!(recovered.next_notification().await.unwrap().is_none());
            recovered.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn malformed_class_timestamp_is_quarantined_while_healthy_transitions_finish() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![transition(StorageClass::Coldline, json!({"age":1}))],
    )
    .await;
    let malformed = put(&catalog, "broken").await;
    put(&catalog, "healthy").await;
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec(
                connection,
                "UPDATE objects SET time_storage_class_updated = 'invalid' WHERE name = 'broken'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    clock.advance(DAY);
    let outcome = execute(&catalog).await;
    assert_eq!(
        (
            outcome.scanned,
            outcome.transitioned,
            outcome.skipped,
            outcome.blocked,
            outcome.deleted,
            outcome.archived,
            outcome.next
        ),
        (2, 1, 1, 0, 0, 0, Some(clock.now() + COALESCE))
    );
    assert_eq!(
        current(&catalog, "healthy").await.storage_class,
        StorageClass::Coldline
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM lifecycle_quarantine").await,
        1
    );
    let repeat = execute(&catalog).await;
    assert_eq!(
        (repeat.transitioned, repeat.skipped, repeat.next),
        (0, 1, None)
    );
    catalog.run::<_, CatalogError>(|connection| {
        sql::exec(connection, "UPDATE objects SET time_storage_class_updated = NULL, metageneration = metageneration + 1 WHERE name = 'broken'", [])?;
        Ok(())
    }).await.unwrap();
    assert_eq!(execute(&catalog).await.transitioned, 1);
    let repaired = current(&catalog, "broken").await;
    assert_eq!(repaired.storage_class, StorageClass::Coldline);
    assert_eq!(repaired.generation, malformed.generation);
    assert_eq!(repaired.metageneration, malformed.metageneration + 2);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM lifecycle_quarantine").await,
        0
    );
    catalog.close().await.unwrap();
}
