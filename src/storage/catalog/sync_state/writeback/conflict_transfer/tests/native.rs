use super::*;

#[tokio::test]
async fn native_cleanup_failure_charges_the_actual_displaced_body_and_stops_new_exports() {
    for phase in ["installed", "displaced-unlink", "displaced-parent-sync"] {
        let fixture = Fixture::new(BODY.len() as u64, 1).await;
        let (operation, id) = fixture.preserved().await;
        assert_eq!(
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .unwrap(),
            LiveResult::Pending
        );
        live_export::inject_failure(fixture.lease.status().root_id, phase, 1);
        assert!(
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .is_err(),
            "{phase}"
        );
        for _ in 0..2 {
            assert!(
                fixture
                    .writeback
                    .advance_live_export(operation.clone())
                    .await
                    .is_err(),
                "{phase}"
            );
        }
        let usage = fixture.writeback.status().await.unwrap();
        let page = fixture.page().await;
        assert_eq!(
            fs::read(fixture.data.0.join("sync-conflicts").join(id)).unwrap(),
            BODY
        );
        let pending = fixture.writeback.page(0, 1).await.unwrap().pop().unwrap();
        if phase == "displaced-parent-sync" {
            assert_eq!(usage.stop, Stop::IoFailure);
            assert_eq!((usage.retained_bytes, page.usage.entries), (0, 1));
        } else {
            assert_eq!(usage.stop, Stop::IoFailure);
            assert!(usage.over_limit());
            assert_eq!(
                (
                    usage.retained_bytes,
                    page.usage.entries,
                    page.usage.retained_bytes
                ),
                (BODY.len() as u128, 2, BODY.len() as u128 * 2)
            );
            assert_eq!(pending.phase, Phase::Protected);
            assert_eq!(
                fs::read(
                    fixture
                        .tree
                        .0
                        .join(mapping::WORKSPACE)
                        .join(pending.token.id())
                )
                .unwrap(),
                BODY
            );
        }
        assert!(
            fixture
                .writeback
                .begin_live_export(
                    fixture
                        .catalog
                        .sync_state()
                        .binding(&fixture.bucket)
                        .await
                        .unwrap(),
                    ObjectName::new("object").unwrap(),
                    Generation::new(fixture.record.generation).unwrap(),
                    None,
                    fixture
                        .catalog
                        .read_changes(None, None, 1)
                        .await
                        .unwrap()
                        .fence,
                    LiveSource::Observe
                )
                .await
                .is_err()
        );
        fixture.assert_api().await;
        drop(operation);
        let fixture = fixture.restart().await;
        fixture.writeback.recover_pending().await.unwrap();
        if phase == "displaced-parent-sync" {
            assert_eq!(
                fixture.writeback.preserve_protected().await.unwrap().stop,
                Stop::Ready
            );
        } else {
            assert_eq!(
                fixture.writeback.preserve_protected().await.unwrap().stop,
                Stop::ConflictCapacity
            );
            let pending = fixture.writeback.page(0, 1).await.unwrap().pop().unwrap();
            fs::write(
                fixture
                    .tree
                    .0
                    .join(mapping::WORKSPACE)
                    .join(pending.token.id()),
                b"new larger actual displaced body",
            )
            .unwrap();
            fixture.writeback.recover_pending().await.unwrap();
            assert_eq!(
                fixture.writeback.status().await.unwrap().retained_bytes,
                b"new larger actual displaced body".len() as u128
            );
        }
        fixture.assert_api().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn preserved_native_proof_survives_restart_without_charging_another_retained_entry() {
    let fixture = Fixture::new(BODY.len() as u64, 1).await;
    let (operation, id) = fixture.preserved().await;
    assert_eq!(
        (
            fixture.page().await.usage.entries,
            fixture.page().await.usage.retained_bytes
        ),
        (1, BODY.len() as u128)
    );
    drop(operation);
    let fixture = fixture.restart().await;
    assert_eq!(
        fixture.writeback.recover_pending().await.unwrap().stop,
        Stop::Ready
    );
    let (operation, reused) = fixture.preserved().await;
    assert_eq!(id, reused);
    loop {
        if fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap()
            != LiveResult::Pending
        {
            break;
        }
    }
    assert_eq!(
        (
            fixture.page().await.usage.entries,
            fixture.page().await.usage.retained_bytes
        ),
        (1, BODY.len() as u128)
    );
    fixture.assert_api().await;
    drop(operation);
    fixture.close().await;
}

#[tokio::test]
async fn preserved_native_export_never_removes_a_later_external_body_or_an_unproven_copy() {
    for missing_copy in [false, true] {
        let fixture = Fixture::new(BODY.len() as u64, 1).await;
        let (operation, id) = fixture.preserved().await;
        assert_eq!(
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .unwrap(),
            LiveResult::Pending
        );
        if missing_copy {
            fs::remove_file(fixture.data.0.join("sync-conflicts").join(id)).unwrap();
        } else {
            fs::write(
                fixture.tree.0.join("test-bucket/object"),
                b"later\0external",
            )
            .unwrap();
        }
        let result = fixture
            .writeback
            .advance_live_export(operation.clone())
            .await;
        if missing_copy {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap(), LiveResult::Conflict);
        }
        assert_eq!(
            fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
            if missing_copy {
                BODY
            } else {
                b"later\0external"
            }
        );
        fixture.assert_api().await;
        drop(operation);
        fixture.close().await;
    }
}

#[tokio::test]
async fn protected_zero_byte_transfer_reserves_a_second_physical_entry() {
    let fixture = Fixture::new(4096, 1).await;
    fixture.protected(b"").await;
    assert_eq!(
        fixture.writeback.preserve_protected().await.unwrap().stop,
        Stop::ProtectedBody
    );
    assert!(fixture.page().await.items.is_empty());
    assert_eq!(fixture.writeback.status().await.unwrap().entries, 1);
    fixture.close().await;
}
