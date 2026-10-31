use super::*;
use crate::storage::catalog::fault::{Fault, Step};

fn restore_tree(snapshot: &std::path::Path, tree: &std::path::Path) {
    let workspace = tree.join(mapping::WORKSPACE);
    for entry in fs::read_dir(&workspace).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    for entry in fs::read_dir(snapshot.join(mapping::WORKSPACE)).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), workspace.join(entry.file_name())).unwrap();
    }
    let target = tree.join("test-bucket/object");
    if snapshot.join("test-bucket/object").exists() {
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(snapshot.join("test-bucket/object"), target).unwrap();
    } else if target.exists() {
        fs::remove_file(target).unwrap();
    }
}

#[tokio::test]
async fn recovery_failures_keep_intents_and_retry_without_double_quota_release() {
    for step in [
        Step::SyncRecoveryUnlink,
        Step::SyncRecoveryUnlinked,
        Step::SyncRecoveryParentSync,
        Step::SyncRecoveryParentDurable,
        Step::SyncRecoveryComplete,
        Step::SyncRecoveryCompleted,
    ] {
        let fixture = Fixture::new().await;
        let item = fixture.prepare().await;
        let owned = fixture
            .tree
            .0
            .join(mapping::WORKSPACE)
            .join(item.token.id());
        fs::write(&owned, b"short").unwrap();
        fixture
            .catalog
            .inject(step, 0, Fault::Io(io::ErrorKind::StorageFull));
        assert!(
            fixture.writeback.recover_pending().await.is_err(),
            "{step:?}"
        );
        assert_eq!(fixture.catalog.pending_faults(), 0);
        if step != Step::SyncRecoveryCompleted {
            assert_eq!(
                fixture.writeback.status().await.unwrap().entries,
                1,
                "{step:?}"
            );
        }
        if step == Step::SyncRecoveryUnlink {
            assert_eq!(fs::read(&owned).unwrap(), b"short");
        }
        let fixture = fixture.restart().await;
        let usage = fixture.writeback.recover_pending().await.unwrap();
        assert_eq!(
            (
                usage.entries,
                usage.reserved_bytes,
                usage.retained_bytes,
                usage.stop
            ),
            (0, 0, 0, Stop::Ready),
            "{step:?}"
        );
        assert_eq!(
            fixture.writeback.recover_pending().await.unwrap().entries,
            0
        );
        fixture.api().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn recovery_paired_crash_and_power_loss_snapshots_finish_only_owned_cancellation() {
    for step in [
        Step::SyncRecoveryUnlink,
        Step::SyncRecoveryUnlinked,
        Step::SyncRecoveryParentSync,
        Step::SyncRecoveryParentDurable,
        Step::SyncRecoveryComplete,
        Step::SyncRecoveryCompleted,
    ] {
        for power_loss in [false, true] {
            let fixture = Fixture::new().await;
            let item = fixture.prepare().await;
            fs::write(
                fixture
                    .tree
                    .0
                    .join(mapping::WORKSPACE)
                    .join(item.token.id()),
                b"short",
            )
            .unwrap();
            fs::write(
                fixture.tree.0.join(mapping::WORKSPACE).join("unrelated"),
                b"keep",
            )
            .unwrap();
            fs::create_dir(fixture.tree.0.join("test-bucket")).unwrap();
            fs::write(fixture.tree.0.join("test-bucket/object"), b"external").unwrap();
            let snapshots = TestDir::new();
            let data = snapshots.0.join("catalog");
            let tree = snapshots.0.join("tree");
            fault::arm(
                &fixture.tree.0,
                step,
                0,
                if power_loss {
                    Fault::PowerLoss(tree.clone())
                } else {
                    Fault::Snapshot(tree.clone())
                },
            );
            fixture
                .catalog
                .inject(step, 0, Fault::Snapshot(data.clone()));
            assert_eq!(
                fixture.writeback.recover_pending().await.unwrap().stop,
                Stop::Ready
            );
            assert_eq!(fixture.catalog.pending_faults(), 0);
            assert_eq!(fault::pending(&fixture.tree.0), 0);
            let Fixture {
                data: original_data,
                tree: original_tree,
                catalog,
                lease,
                writeback,
                record,
                ..
            } = fixture;
            drop(writeback);
            drop(lease);
            catalog.close().await.unwrap();
            restore_tree(&tree, &original_tree.0);
            let recovered = Catalog::open(&data).await.unwrap();
            assert_api(&recovered, &record).await;
            let lease = recovered
                .sync_state()
                .activate_root(
                    &original_tree.0,
                    super::super::super::super::root::Activation::Open,
                )
                .await
                .unwrap();
            let writeback = lease.writeback(limits()).await.unwrap();
            let usage = writeback.recover_pending().await.unwrap();
            assert_eq!(
                (usage.entries, usage.reserved_bytes, usage.stop),
                (0, 0, Stop::Ready),
                "{step:?} {power_loss}"
            );
            assert!(
                !original_tree
                    .0
                    .join(mapping::WORKSPACE)
                    .join(item.token.id())
                    .exists()
            );
            assert_eq!(
                fs::read(original_tree.0.join(mapping::WORKSPACE).join("unrelated")).unwrap(),
                b"keep"
            );
            assert_eq!(
                fs::read(original_tree.0.join("test-bucket/object")).unwrap(),
                b"external"
            );
            assert_api(&recovered, &record).await;
            drop(writeback);
            drop(lease);
            recovered.close().await.unwrap();
            drop(original_data);
        }
    }
}

#[tokio::test]
async fn live_copy_snapshots_recover_from_synced_copy_install_and_sql_ack() {
    for step in [
        Step::SyncCopySync,
        Step::SyncCopyDurable,
        Step::SyncCopyInstalled,
        Step::SyncCopyCommit,
    ] {
        for power_loss in [false, true] {
            let fixture = Fixture::new().await;
            fs::create_dir(fixture.tree.0.join("test-bucket")).unwrap();
            let snapshots = TestDir::new();
            let data = snapshots.0.join("catalog");
            let tree = snapshots.0.join("tree");
            fault::arm(
                &fixture.tree.0,
                step,
                0,
                if power_loss {
                    Fault::PowerLoss(tree.clone())
                } else {
                    Fault::Snapshot(tree.clone())
                },
            );
            fixture
                .catalog
                .inject(step, 0, Fault::Snapshot(data.clone()));
            let operation = fixture.live().await;
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
            drop(operation);
            assert_eq!(fixture.catalog.pending_faults(), 0);
            assert_eq!(fault::pending(&fixture.tree.0), 0);
            let Fixture {
                data: original_data,
                tree: original_tree,
                catalog,
                lease,
                writeback,
                record,
                ..
            } = fixture;
            drop(writeback);
            drop(lease);
            catalog.close().await.unwrap();
            restore_tree(&tree, &original_tree.0);
            let recovered = Catalog::open(&data).await.unwrap();
            assert_api(&recovered, &record).await;
            let lease = recovered
                .sync_state()
                .activate_root(
                    &original_tree.0,
                    super::super::super::super::root::Activation::Open,
                )
                .await
                .unwrap();
            let writeback = lease.writeback(limits()).await.unwrap();
            assert_eq!(
                writeback.recover_pending().await.unwrap().stop,
                Stop::Ready,
                "{step:?} {power_loss}"
            );
            if matches!(step, Step::SyncCopyInstalled | Step::SyncCopyCommit) {
                assert_eq!(
                    fs::read(original_tree.0.join("test-bucket/object")).unwrap(),
                    API
                );
            } else {
                assert!(!original_tree.0.join("test-bucket/object").exists());
            }
            assert_api(&recovered, &record).await;
            drop(writeback);
            drop(lease);
            recovered.close().await.unwrap();
            drop(original_data);
        }
    }
}

#[tokio::test]
async fn protection_snapshots_keep_full_charge_until_confirmed_discard_and_rescan() {
    use crate::storage::catalog::sync_state::{
        authority::ControlAuthority,
        conflicts::{Action, Confirmation, ResolveRequest, ResolveTarget},
    };
    for power_loss in [false, true] {
        let fixture = Fixture::new().await;
        let item = fixture
            .writeback
            .arm(fixture.prepare().await.token)
            .await
            .unwrap();
        fs::write(
            fixture
                .tree
                .0
                .join(mapping::WORKSPACE)
                .join(item.token.id()),
            b"external\0\xff",
        )
        .unwrap();
        let snapshots = TestDir::new();
        let data = snapshots.0.join("catalog");
        let tree = snapshots.0.join("tree");
        let step = Step::SyncRecoveryProtected;
        fault::arm(
            &fixture.tree.0,
            step,
            0,
            if power_loss {
                Fault::PowerLoss(tree.clone())
            } else {
                Fault::Snapshot(tree.clone())
            },
        );
        fixture
            .catalog
            .inject(step, 0, Fault::Snapshot(data.clone()));
        assert_eq!(
            fixture.writeback.recover_pending().await.unwrap().stop,
            Stop::ProtectedBody
        );
        assert_eq!(fixture.catalog.pending_faults(), 0);
        assert_eq!(fault::pending(&fixture.tree.0), 0);
        let Fixture {
            data: original_data,
            tree: original_tree,
            catalog,
            lease,
            writeback,
            record,
            ..
        } = fixture;
        drop(writeback);
        drop(lease);
        catalog.close().await.unwrap();
        restore_tree(&tree, &original_tree.0);
        let recovered = Catalog::open(&data).await.unwrap();
        assert_api(&recovered, &record).await;
        let lease = recovered
            .sync_state()
            .activate_root(
                &original_tree.0,
                super::super::super::super::root::Activation::Open,
            )
            .await
            .unwrap();
        let writeback = lease.writeback(limits()).await.unwrap();
        let usage = writeback.recover_pending().await.unwrap();
        assert_eq!(
            (usage.retained_bytes, usage.entries, usage.stop),
            (10, 1, Stop::ProtectedBody)
        );
        assert_eq!(
            fs::read(
                original_tree
                    .0
                    .join(mapping::WORKSPACE)
                    .join(item.token.id())
            )
            .unwrap(),
            b"external\0\xff"
        );
        let inspected = lease
            .recovery_page(0, 128, ControlAuthority::Permissive)
            .await
            .unwrap()
            .pop()
            .unwrap();
        lease
            .resolve_conflict(
                ResolveRequest {
                    operation_id: "explicit-discard".into(),
                    action: Action::Discard,
                    target: ResolveTarget::Recovery {
                        recovery_id: item.token.id.clone(),
                        revision: inspected.recovery_revision,
                        target_identity: inspected.target_identity,
                        confirmation: Some(Confirmation::BackupVerified),
                    },
                },
                ControlAuthority::Permissive,
                4096,
            )
            .await
            .unwrap();
        let usage = writeback.recover_pending().await.unwrap();
        assert_eq!(
            (usage.retained_bytes, usage.entries, usage.stop),
            (0, 0, Stop::Ready)
        );
        assert_api(&recovered, &record).await;
        drop(writeback);
        drop(lease);
        recovered.close().await.unwrap();
        drop(original_data);
    }
}
