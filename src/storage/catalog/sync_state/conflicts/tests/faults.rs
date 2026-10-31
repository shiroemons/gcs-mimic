use super::*;
use crate::storage::catalog::fault::{Fault, Step};

#[tokio::test]
async fn conflict_capture_write_flush_sync_failures_preserve_input_and_winner() {
    for step in [
        Step::ConflictWrite,
        Step::ConflictFlush,
        Step::ConflictFileSync,
        Step::ConflictDirectorySync,
        Step::ConflictRegister,
    ] {
        let fixture = Fixture::new().await;
        fixture
            .catalog
            .inject(step, 0, Fault::Io(io::ErrorKind::StorageFull));
        assert!(
            fixture
                .lease
                .capture_conflict(
                    fixture.request().await,
                    File::open(fixture.tree.0.join("external")).unwrap()
                )
                .await
                .is_err()
        );
        assert_eq!(fixture.catalog.pending_faults(), 0);
        assert_eq!(
            fs::read(fixture.tree.0.join("external")).unwrap(),
            b"external\0\xff"
        );
        assert_eq!(fixture.api_bytes().await, b"api winner");
        let page = fixture.page().await;
        if step == Step::ConflictRegister {
            assert_eq!(page.usage.entries, 1);
            assert!(
                fixture
                    .data
                    .0
                    .join("sync-conflicts")
                    .join(&page.items[0].conflict_id)
                    .exists()
            );
        } else {
            assert_eq!(page.usage.entries, 0);
        }
        fixture.close().await;
    }
}

#[tokio::test]
async fn conflict_discard_unlink_and_commit_failures_replay_after_restart() {
    for step in [
        Step::ConflictAccepted,
        Step::ConflictUnlink,
        Step::ConflictUnlinked,
        Step::ConflictParentSync,
        Step::ConflictComplete,
    ] {
        let fixture = Fixture::new().await;
        let captured = fixture.capture().await;
        fixture
            .catalog
            .inject(step, 0, Fault::Io(io::ErrorKind::StorageFull));
        assert!(
            fixture
                .resolve(&captured, Action::Discard, "restart-discard")
                .await
                .is_err()
        );
        assert_eq!(fixture.catalog.pending_faults(), 0);
        assert_eq!(fixture.page().await.usage.entries, 1);
        assert_eq!(fixture.api_bytes().await, b"api winner");
        let Fixture {
            data,
            tree,
            catalog,
            lease,
            bucket,
            winner,
        } = fixture;
        drop(lease);
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let lease = catalog
            .sync_state()
            .activate_root(&tree.0, Activation::Open)
            .await
            .unwrap();
        let fixture = Fixture {
            data,
            tree,
            catalog,
            lease,
            bucket,
            winner,
        };
        assert!(
            fixture
                .resolve(&captured, Action::Discard, "restart-discard")
                .await
                .unwrap()
                .completed
        );
        assert_eq!(fixture.page().await.usage.entries, 0);
        assert_eq!(fixture.api_bytes().await, b"api winner");
        fixture.close().await;
    }
}

#[tokio::test]
async fn conflict_restore_ambiguous_commit_returns_recorded_generation_after_restart() {
    let fixture = Fixture::new().await;
    let captured = fixture.capture().await;
    fixture.catalog.inject(
        Step::ConflictRestorePublished,
        0,
        Fault::Io(io::ErrorKind::Other),
    );
    let _ambiguous = fixture
        .resolve(&captured, Action::Restore, "ambiguous")
        .await;
    assert_eq!(fixture.catalog.pending_faults(), 0);
    let record = fixture
        .catalog
        .admin(Origin::Api)
        .get_object(
            &fixture.bucket,
            &ObjectName::new("object.txt").unwrap(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(fixture.api_bytes().await, b"external\0\xff");
    let Fixture {
        data,
        tree,
        catalog,
        lease,
        bucket,
        winner,
    } = fixture;
    drop(lease);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    let fixture = Fixture {
        data,
        tree,
        catalog,
        lease,
        bucket,
        winner,
    };
    let result = fixture
        .resolve(&captured, Action::Restore, "ambiguous")
        .await
        .unwrap();
    assert_eq!(result.result_generation, Some(record.generation));
    fixture.close().await;
}

#[tokio::test]
async fn conflict_crash_and_power_loss_snapshots_keep_owned_bytes_or_authorized_intent() {
    for step in [
        Step::ConflictReserved,
        Step::ConflictWrite,
        Step::ConflictFlush,
        Step::ConflictFileSync,
        Step::ConflictDirectorySync,
        Step::ConflictRegister,
        Step::ConflictRegistered,
        Step::ConflictAccepted,
        Step::ConflictUnlink,
        Step::ConflictUnlinked,
        Step::ConflictParentSync,
        Step::ConflictComplete,
        Step::ConflictCompleted,
    ] {
        for power_loss in [false, true] {
            let fixture = Fixture::new().await;
            let discard = matches!(
                step,
                Step::ConflictAccepted
                    | Step::ConflictUnlink
                    | Step::ConflictUnlinked
                    | Step::ConflictParentSync
                    | Step::ConflictComplete
                    | Step::ConflictCompleted
            );
            let captured = if discard {
                Some(fixture.capture().await)
            } else {
                None
            };
            let snapshots = TestDir::new();
            let snapshot = snapshots.0.join("data");
            fixture.catalog.inject(
                step,
                0,
                if power_loss {
                    Fault::PowerLoss(snapshot.clone())
                } else {
                    Fault::Snapshot(snapshot.clone())
                },
            );
            if let Some(captured) = &captured {
                fixture
                    .resolve(captured, Action::Discard, "snapshot")
                    .await
                    .unwrap();
            } else {
                fixture.capture().await;
            }
            assert_eq!(fixture.catalog.pending_faults(), 0, "{step:?}");
            let recovered = Catalog::open(&snapshot).await.unwrap();
            let root = fixture.lease.status().root_id.as_bytes();
            let state=recovered.run_admin(move|connection| {
                let row=connection.prepare_cached("SELECT id,state,retained FROM sync_conflicts WHERE root_id=?1")?.query_row([root],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,Option<Vec<u8>>>(2)?)))?;
                let accepted:bool=connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_conflict_operations WHERE root_id=?1)")?.query_row([root],|row|row.get(0))?;
                Ok((row,accepted))
            }).await.unwrap();
            let file = snapshot.join("sync-conflicts").join(&state.0.0);
            if state.0.1 == 1 && !state.1 {
                assert_eq!(
                    fs::read(&file).unwrap(),
                    b"external\0\xff",
                    "{step:?} power={power_loss}"
                );
            }
            if state.0.1 == 2 {
                assert!(state.1);
                assert!(!file.exists());
            }
            if matches!(step, Step::ConflictUnlinked | Step::ConflictParentSync) && power_loss {
                assert!(
                    file.exists(),
                    "missing parent fsync must restore the test inode"
                );
            }
            let (_, reader) = recovered
                .admin(Origin::Api)
                .open_object(
                    &fixture.bucket,
                    &ObjectName::new("object.txt").unwrap(),
                    None,
                )
                .await
                .unwrap();
            let mut bytes = Vec::new();
            reader
                .into_file()
                .await
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes, b"api winner");
            let record = recovered
                .admin(Origin::Api)
                .get_object(
                    &fixture.bucket,
                    &ObjectName::new("object.txt").unwrap(),
                    None,
                )
                .await
                .unwrap();
            assert_eq!(record, fixture.winner);
            if file.exists() && state.0.2.is_some() {
                // Copy-based snapshots assign new host inodes; translate only after verifying the modeled bytes.
                let original = Observation::decode(state.0.2.as_ref().unwrap()).unwrap();
                let observed = observe(&File::open(&file).unwrap(), HashMode::Body).unwrap();
                assert_eq!(original.size, observed.size);
                assert_eq!(original.hashes, observed.hashes);
                let identity = observed.encode().unwrap();
                let id = state.0.0.clone();
                recovered
                    .run_admin(move |connection| {
                        connection
                            .prepare_cached("UPDATE sync_conflicts SET retained=?2 WHERE id=?1")
                            .and_then(|mut statement| statement.execute(params![id, identity]))
                            .map(|_| ())
                            .map_err(unavailable)
                    })
                    .await
                    .unwrap();
            }
            let Fixture {
                data: _original_data,
                tree,
                catalog,
                lease,
                bucket: _,
                winner: _,
            } = fixture;
            drop(lease);
            catalog.close().await.unwrap();
            let root = recovered
                .sync_state()
                .activate_root(&tree.0, Activation::Open)
                .await
                .unwrap();
            let page = root
                .conflict_page(None, 0, 256, ControlAuthority::Permissive)
                .await
                .unwrap();
            if state.0.1 != 2 {
                assert_eq!(page.usage.entries, 1);
            }
            let request = if state.1 {
                let captured = captured.as_ref().unwrap();
                ResolveRequest {
                    operation_id: "snapshot".into(),
                    action: Action::Discard,
                    target: ResolveTarget::Conflict {
                        conflict_id: captured.id.clone(),
                        revision: captured.revision.clone(),
                        expected_generation: Some(record.generation),
                    },
                }
            } else {
                ResolveRequest {
                    operation_id: "fresh-recovery".into(),
                    action: Action::Discard,
                    target: ResolveTarget::Conflict {
                        conflict_id: state.0.0,
                        revision: page.items[0].revision.clone(),
                        expected_generation: None,
                    },
                }
            };
            assert!(
                root.resolve_conflict(request.clone(), ControlAuthority::Permissive, 4096)
                    .await
                    .unwrap()
                    .completed
            );
            assert!(
                root.resolve_conflict(request, ControlAuthority::Permissive, 4096)
                    .await
                    .unwrap()
                    .completed
            );
            assert_eq!(
                root.conflict_page(None, 0, 256, ControlAuthority::Permissive)
                    .await
                    .unwrap()
                    .usage
                    .entries,
                0
            );
            drop(root);
            recovered.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn conflict_restore_transaction_failure_keeps_winner_and_retry_publishes_once() {
    let fixture = Fixture::new().await;
    let captured = fixture.capture().await;
    fixture.catalog.inject(
        Step::ConflictRestoreCommit,
        0,
        Fault::Io(io::ErrorKind::StorageFull),
    );
    assert!(
        fixture
            .resolve(&captured, Action::Restore, "rollback")
            .await
            .is_err()
    );
    assert_eq!(fixture.catalog.pending_faults(), 0);
    assert_eq!(fixture.api_bytes().await, b"api winner");
    assert_eq!(fixture.page().await.usage.retained_bytes, 10);
    let result = fixture
        .resolve(&captured, Action::Restore, "rollback")
        .await
        .unwrap();
    assert_eq!(
        fixture
            .resolve(&captured, Action::Restore, "rollback")
            .await
            .unwrap()
            .result_generation,
        result.result_generation
    );
    assert_eq!(fixture.api_bytes().await, b"external\0\xff");
    fixture.close().await;
}
