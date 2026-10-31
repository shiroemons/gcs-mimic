use super::*;
use crate::storage::catalog::fault::{Fault, Step};

#[tokio::test]
async fn recovery_restart_invalidates_unused_revision_and_replays_accepted_discard() {
    for step in [
        Step::ConflictAccept,
        Step::ConflictAccepted,
        Step::ConflictUnlink,
        Step::ConflictUnlinked,
        Step::ConflictParentSync,
        Step::ConflictComplete,
        Step::ConflictCompleted,
    ] {
        let fixture = Fixture::new().await;
        let (writeback, id) = protected_workspace(&fixture, 10).await;
        let inspected = fixture
            .lease
            .recovery_page(0, 128, ControlAuthority::Permissive)
            .await
            .unwrap()
            .pop()
            .unwrap();
        let request = ResolveRequest {
            operation_id: "workspace-discard".into(),
            action: Action::Discard,
            target: ResolveTarget::Recovery {
                recovery_id: id.clone(),
                revision: inspected.recovery_revision,
                target_identity: inspected.target_identity,
                confirmation: Some(Confirmation::BackupVerified),
            },
        };
        fixture
            .catalog
            .inject(step, 0, Fault::Io(io::ErrorKind::StorageFull));
        assert!(
            fixture
                .lease
                .resolve_conflict(request.clone(), ControlAuthority::Permissive, 4096)
                .await
                .is_err()
        );
        assert_eq!(fixture.catalog.pending_faults(), 0);
        if step != Step::ConflictCompleted {
            assert_eq!(writeback.status().await.unwrap().entries, 1);
        }
        if matches!(
            step,
            Step::ConflictAccept | Step::ConflictAccepted | Step::ConflictUnlink
        ) {
            assert_eq!(
                fs::metadata(
                    fixture
                        .tree
                        .0
                        .join(crate::sync::mapping::WORKSPACE)
                        .join(&id)
                )
                .unwrap()
                .len(),
                10
            );
        }
        let Fixture {
            data,
            tree,
            catalog,
            lease,
            bucket,
            winner,
        } = fixture;
        drop(writeback);
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
        if step == Step::ConflictAccept {
            assert!(matches!(
                fixture
                    .lease
                    .resolve_conflict(request.clone(), ControlAuthority::Permissive, 4096)
                    .await,
                Err(Error::RevisionMismatch)
            ));
            let fresh = fixture
                .lease
                .recovery_page(0, 128, ControlAuthority::Permissive)
                .await
                .unwrap()
                .pop()
                .unwrap();
            let mut fresh_request = request.clone();
            if let ResolveTarget::Recovery {
                revision,
                target_identity,
                ..
            } = &mut fresh_request.target
            {
                *revision = fresh.recovery_revision;
                *target_identity = fresh.target_identity;
            }
            assert!(
                fixture
                    .lease
                    .resolve_conflict(fresh_request, ControlAuthority::Permissive, 4096)
                    .await
                    .unwrap()
                    .completed
            );
        } else {
            assert!(
                fixture
                    .lease
                    .resolve_conflict(request.clone(), ControlAuthority::Permissive, 4096)
                    .await
                    .unwrap()
                    .completed
            );
            assert!(
                fixture
                    .lease
                    .resolve_conflict(request, ControlAuthority::Permissive, 4096)
                    .await
                    .unwrap()
                    .completed
            );
        }
        let writeback = fixture
            .lease
            .writeback(writeback::Limits::new(MAX_BYTES, MAX_ENTRIES).unwrap())
            .await
            .unwrap();
        assert_eq!(writeback.status().await.unwrap().entries, 0);
        assert!(
            !fixture
                .tree
                .0
                .join(crate::sync::mapping::WORKSPACE)
                .join(id)
                .exists()
        );
        assert_eq!(fixture.api_bytes().await, b"api winner");
        drop(writeback);
        fixture.close().await;
    }
}

#[tokio::test]
async fn recovery_foreign_authority_and_changed_body_leave_the_stop_and_bytes() {
    let fixture = Fixture::new().await;
    let (writeback, id) = protected_workspace(&fixture, 10).await;
    let item = fixture
        .lease
        .recovery_page(0, 128, strict_authority(&["seed", "owner"]))
        .await
        .unwrap()
        .pop()
        .unwrap();
    let request = ResolveRequest {
        operation_id: "protected".into(),
        action: Action::Discard,
        target: ResolveTarget::Recovery {
            recovery_id: id.clone(),
            revision: item.recovery_revision,
            target_identity: item.target_identity,
            confirmation: Some(Confirmation::BackupVerified),
        },
    };
    assert!(matches!(
        fixture
            .lease
            .resolve_conflict(request.clone(), strict_authority(&["seed"]), 4096)
            .await,
        Err(Error::Admin(AdminError::Forbidden))
    ));
    let path = fixture
        .tree
        .0
        .join(crate::sync::mapping::WORKSPACE)
        .join(id);
    fs::write(&path, b"changed external").unwrap();
    assert!(matches!(
        fixture
            .lease
            .resolve_conflict(request.clone(), ControlAuthority::Permissive, 4096)
            .await,
        Err(Error::TargetChanged)
    ));
    assert_eq!(fs::read(path).unwrap(), b"changed external");
    assert_eq!(writeback.status().await.unwrap().entries, 1);
    let fresh = fixture
        .lease
        .recovery_page(0, 128, ControlAuthority::Permissive)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(fresh.retained_size, 16);
    assert!(matches!(
        fixture
            .lease
            .resolve_conflict(request, ControlAuthority::Permissive, 4096)
            .await,
        Err(Error::TargetChanged)
    ));
    let confirmed = ResolveRequest {
        operation_id: "reinspected".into(),
        action: Action::Discard,
        target: ResolveTarget::Recovery {
            recovery_id: fresh.recovery_id,
            revision: fresh.recovery_revision,
            target_identity: fresh.target_identity,
            confirmation: Some(Confirmation::BackupVerified),
        },
    };
    assert!(
        fixture
            .lease
            .resolve_conflict(confirmed, ControlAuthority::Permissive, 4096)
            .await
            .unwrap()
            .completed
    );
    assert_eq!(writeback.status().await.unwrap().entries, 0);
    assert_eq!(fixture.api_bytes().await, b"api winner");
    drop(writeback);
    fixture.close().await;
}

#[tokio::test]
async fn conflict_exact_entry_capacity_counts_zero_length_reservations() {
    let fixture = Fixture::new().await;
    let captured = fixture.capture().await;
    let id = captured.id.clone();
    fixture.lease.conflict_db(move|connection| {
        connection.prepare_cached("WITH RECURSIVE copies(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM copies WHERE n<999) INSERT INTO sync_conflicts(id,root_id,revision,owner,bucket,incarnation,name,generation,state,size,source) SELECT lower(hex(randomblob(16))),c.root_id,c.revision,c.owner,c.bucket,c.incarnation,c.name,c.generation,0,0,c.source FROM copies JOIN sync_conflicts c ON c.id=?1")?.execute([id])?;Ok(())
    }).await.unwrap();
    assert_eq!(fixture.page().await.usage.entries, 1000);
    let empty = File::create(fixture.tree.0.join("empty")).unwrap();
    assert!(matches!(
        fixture
            .lease
            .capture_conflict(fixture.request().await, empty)
            .await,
        Err(Error::Capacity)
    ));
    assert_eq!(fixture.page().await.usage.entries, 1000);
    assert_eq!(fixture.api_bytes().await, b"api winner");
    fixture.close().await;
}

#[tokio::test]
async fn recovery_paired_tree_and_catalog_snapshots_replay_only_the_accepted_discard() {
    for step in [
        Step::ConflictAccepted,
        Step::ConflictUnlink,
        Step::ConflictUnlinked,
        Step::ConflictParentSync,
        Step::ConflictComplete,
        Step::ConflictCompleted,
    ] {
        for power_loss in [false, true] {
            let fixture = Fixture::new().await;
            let (writeback, id) = protected_workspace(&fixture, 10).await;
            let item = fixture
                .lease
                .recovery_page(0, 128, ControlAuthority::Permissive)
                .await
                .unwrap()
                .pop()
                .unwrap();
            let request = ResolveRequest {
                operation_id: "paired-discard".into(),
                action: Action::Discard,
                target: ResolveTarget::Recovery {
                    recovery_id: id.clone(),
                    revision: item.recovery_revision,
                    target_identity: item.target_identity,
                    confirmation: Some(Confirmation::BackupVerified),
                },
            };
            let snapshots = TestDir::new();
            let data_snapshot = snapshots.0.join("data");
            let tree_snapshot = snapshots.0.join("tree");
            crate::storage::catalog::fault::arm(
                &fixture.tree.0,
                step,
                0,
                if power_loss {
                    Fault::PowerLoss(tree_snapshot.clone())
                } else {
                    Fault::Snapshot(tree_snapshot.clone())
                },
            );
            fixture
                .catalog
                .inject(step, 0, Fault::Snapshot(data_snapshot.clone()));
            assert!(
                fixture
                    .lease
                    .resolve_conflict(request.clone(), ControlAuthority::Permissive, 4096)
                    .await
                    .unwrap()
                    .completed
            );
            assert_eq!(fixture.catalog.pending_faults(), 0);
            assert_eq!(crate::storage::catalog::fault::pending(&fixture.tree.0), 0);
            let workspace = fixture.tree.0.join(crate::sync::mapping::WORKSPACE);
            let saved = tree_snapshot
                .join(crate::sync::mapping::WORKSPACE)
                .join(&id);
            if power_loss && matches!(step, Step::ConflictUnlinked | Step::ConflictParentSync) {
                assert_eq!(fs::read(&saved).unwrap(), [0; 10]);
            }
            let Fixture {
                data: _data,
                tree,
                catalog,
                lease,
                bucket,
                winner,
            } = fixture;
            drop(writeback);
            drop(lease);
            catalog.close().await.unwrap();
            for entry in fs::read_dir(&workspace).unwrap() {
                fs::remove_file(entry.unwrap().path()).unwrap();
            }
            if saved.exists() {
                fs::copy(&saved, workspace.join(&id)).unwrap();
            }
            let catalog = Catalog::open(&data_snapshot).await.unwrap();
            if saved.exists() {
                // Translate copied test inodes without weakening the production identity fence.
                let file = File::open(workspace.join(&id)).unwrap();
                assert_eq!(fs::read(workspace.join(&id)).unwrap(), [0; 10]);
                let observation = observe(&file, HashMode::Metadata)
                    .unwrap()
                    .encode()
                    .unwrap();
                let selected = id.clone();
                catalog
                    .run_admin(move |connection| {
                        connection
                            .prepare_cached(
                                "UPDATE sync_recovery_workspaces SET observation=?2 WHERE id=?1",
                            )
                            .and_then(|mut statement| {
                                statement.execute(params![selected, observation])
                            })
                            .map(|_| ())
                            .map_err(unavailable)
                    })
                    .await
                    .unwrap();
            }
            let root = catalog
                .sync_state()
                .activate_root(&tree.0, Activation::Open)
                .await
                .unwrap();
            let (_, reader) = catalog
                .admin(Origin::Api)
                .open_object(&bucket, &ObjectName::new("object.txt").unwrap(), None)
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
            assert_eq!(
                catalog
                    .admin(Origin::Api)
                    .get_object(&bucket, &ObjectName::new("object.txt").unwrap(), None)
                    .await
                    .unwrap(),
                winner
            );
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
            let writeback = root
                .writeback(writeback::Limits::new(MAX_BYTES, MAX_ENTRIES).unwrap())
                .await
                .unwrap();
            assert_eq!(writeback.status().await.unwrap().entries, 0);
            assert!(!workspace.join(&id).exists());
            assert_eq!(
                writeback.resume().await.unwrap().stop,
                writeback::Stop::Ready
            );
            drop(writeback);
            drop(root);
            catalog.close().await.unwrap();
        }
    }
}
