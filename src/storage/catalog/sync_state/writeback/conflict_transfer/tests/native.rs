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
            InstallOutcome::Pending
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
            assert_eq!(usage.stop(), Stop::IoFailure);
            assert_eq!((usage.retained_bytes, page.usage.entries), (0, 1));
        } else {
            assert_eq!(usage.stop(), Stop::IoFailure);
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
                        .join(pending.token.id().to_string())
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
                    fixture.catalog.current_fence().await.unwrap(),
                    LiveSource::Observe
                )
                .await
                .is_err()
        );
        fixture.assert_api().await;
        drop(operation);
        let fixture = fixture.restart(Journal::Enabled).await;
        fixture.writeback.recover_pending().await.unwrap();
        if phase == "displaced-parent-sync" {
            assert_eq!(
                fixture.writeback.preserve_protected().await.unwrap().stop(),
                Stop::Ready
            );
        } else {
            assert_eq!(
                fixture.writeback.preserve_protected().await.unwrap().stop(),
                Stop::ConflictCapacity
            );
            let pending = fixture.writeback.page(0, 1).await.unwrap().pop().unwrap();
            fs::write(
                fixture
                    .tree
                    .0
                    .join(mapping::WORKSPACE)
                    .join(pending.token.id().to_string()),
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
    let fixture = fixture.restart(Journal::Enabled).await;
    assert_eq!(
        fixture.writeback.recover_pending().await.unwrap().stop(),
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
            != InstallOutcome::Pending
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
            InstallOutcome::Pending
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
            assert_eq!(result.unwrap(), InstallOutcome::Conflict);
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

/// A preserved target whose permissions change after its capture, which changes its ctime,
/// still holds the captured bytes, and their hashes show it: the export over it completes
/// instead of reporting the same body as another conflict, and the conflict store keeps the
/// captured copy.
#[tokio::test]
async fn preserved_native_export_replaces_a_captured_target_whose_permissions_changed() {
    use std::os::unix::fs::PermissionsExt;
    replaces_a_captured_target_after(|path| {
        let before = fsutil::FileIdentity::from(&fs::metadata(path).unwrap());
        let mode = fs::metadata(path).unwrap().permissions().mode() ^ 0o040;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        let after = fsutil::FileIdentity::from(&fs::metadata(path).unwrap());
        assert_ne!(before.changed_ns, after.changed_ns);
    })
    .await;
}

/// A preserved target whose modification time alone moves after its capture still holds the
/// captured bytes: the export over it completes, as over one whose permissions changed, and the
/// conflict store keeps the captured copy.
#[tokio::test]
async fn preserved_native_export_replaces_a_captured_target_whose_mtime_alone_moved() {
    replaces_a_captured_target_after(|path| {
        let before = fs::metadata(path).unwrap().modified().unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(before + std::time::Duration::from_secs(1))
            .unwrap();
        assert_ne!(fs::metadata(path).unwrap().modified().unwrap(), before);
    })
    .await;
}

/// Runs `change`, which keeps the bytes, on a preserved target after its capture, and checks that
/// the export over it completes and that the conflict store keeps the captured copy.
async fn replaces_a_captured_target_after(change: impl FnOnce(&std::path::Path)) {
    let fixture = Fixture::new(BODY.len() as u64, 1).await;
    let (operation, id) = fixture.preserved().await;
    let path = fixture.tree.0.join("test-bucket/object");
    change(&path);
    assert_eq!(fs::read(&path).unwrap(), BODY);
    let mut result = InstallOutcome::Pending;
    for _ in 0..8 {
        result = fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
        if result != InstallOutcome::Pending {
            break;
        }
    }
    assert_eq!(result, InstallOutcome::Complete);
    assert_eq!(fs::read(&path).unwrap(), b"api winner");
    assert_eq!(
        fs::read(fixture.data.0.join("sync-conflicts").join(id)).unwrap(),
        BODY
    );
    assert_eq!(
        (
            fixture.writeback.status().await.unwrap().stop(),
            fixture.page().await.usage.entries
        ),
        (Stop::Ready, 1)
    );
    fixture.assert_api().await;
    drop(operation);
    fixture.close().await;
}

#[tokio::test]
async fn protected_zero_byte_transfer_reserves_a_second_physical_entry() {
    let fixture = Fixture::new(4096, 1).await;
    fixture.protected(b"").await;
    assert_eq!(
        fixture.writeback.preserve_protected().await.unwrap().stop(),
        Stop::ProtectedBody
    );
    assert!(fixture.page().await.items.is_empty());
    assert_eq!(fixture.writeback.status().await.unwrap().entries, 1);
    fixture.close().await;
}
