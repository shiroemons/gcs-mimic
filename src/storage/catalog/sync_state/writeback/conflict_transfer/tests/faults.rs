use super::*;
use crate::storage::catalog::fault::{Fault, Step};

#[tokio::test]
async fn protected_transfer_crosses_keyset_pages_without_skipping_completed_rows() {
    let fixture = Fixture::new(4096, 1000).await;
    let original = fixture.protected(BODY).await;
    let mut entries = Vec::new();
    for index in 1..=256 {
        let id = format!("{index:032x}");
        fs::write(fixture.tree.0.join(mapping::WORKSPACE).join(&id), b"").unwrap();
        entries.push((id, format!("item-{index}")));
    }
    fixture.writeback.db(move |connection| {
        let tx=connection.transaction()?;
        for (id,name) in entries {
            let path=format!("test-bucket/{name}");
            tx.prepare_cached("INSERT INTO sync_writeback_ops(id,root_id,owner,bucket,incarnation,name,path,generation,kind,phase,revision,fence_sequence,fence_epoch,reservation,retained,size,md5,crc32c) SELECT ?1,root_id,owner,bucket,incarnation,?2,?3,generation,kind,2,1,fence_sequence,fence_epoch,?4,?4,size,md5,crc32c FROM sync_writeback_ops WHERE id=?5")?.execute(params![id,name,path,0u64.to_be_bytes(),original.token.id])?;
        }
        tx.prepare_cached("UPDATE sync_writeback_roots SET entries=257,protected=257 WHERE root_id=?1")?.execute([original.token.root.as_bytes()])?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    assert_eq!(
        fixture.writeback.preserve_protected().await.unwrap().stop,
        Stop::Ready
    );
    assert!(fixture.writeback.page(0, 256).await.unwrap().is_empty());
    let first = fixture
        .lease
        .conflict_page(None, 0, 256, ControlAuthority::Permissive)
        .await
        .unwrap();
    assert_eq!(
        (
            first.items.len(),
            first.usage.entries,
            first.usage.retained_bytes
        ),
        (256, 257, BODY.len() as u128)
    );
    let second = fixture
        .lease
        .conflict_page(
            Some(first.revision),
            first.next_after.unwrap(),
            256,
            ControlAuthority::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    fixture.assert_api().await;
    fixture.close().await;
}

#[tokio::test]
async fn protected_transfer_restarts_each_copy_and_unlink_failure_without_losing_a_body() {
    for step in [
        Step::ConflictReserved,
        Step::ConflictWrite,
        Step::ConflictFlush,
        Step::ConflictFileSync,
        Step::ConflictDirectorySync,
        Step::ConflictRegister,
        Step::ConflictRegistered,
        Step::TransferAccepted,
        Step::ConflictUnlink,
        Step::ConflictUnlinked,
        Step::ConflictParentSync,
        Step::TransferComplete,
        Step::TransferCompleted,
    ] {
        let fixture = Fixture::new(4096, 1000).await;
        fixture.protected(BODY).await;
        fixture
            .catalog
            .inject(step, 0, Fault::Io(io::ErrorKind::StorageFull));
        assert!(
            fixture.writeback.preserve_protected().await.is_err(),
            "{step:?}"
        );
        assert_eq!(fixture.catalog.pending_faults(), 0, "{step:?}");
        fixture.assert_api().await;
        let fixture = fixture.restart().await;
        fixture.writeback.recover_pending().await.unwrap();
        let usage = fixture.writeback.preserve_protected().await.unwrap();
        assert_eq!(
            (usage.entries, usage.retained_bytes, usage.stop),
            (0, 0, Stop::Ready),
            "{step:?}"
        );
        let page = fixture.page().await;
        assert_eq!(
            (
                page.items.len(),
                page.usage.entries,
                page.usage.retained_bytes
            ),
            (1, 1, BODY.len() as u128),
            "{step:?}"
        );
        assert_eq!(
            fs::read(
                fixture
                    .data
                    .0
                    .join("sync-conflicts")
                    .join(&page.items[0].conflict_id)
            )
            .unwrap(),
            BODY
        );
        fixture.assert_api().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn protected_transfer_retains_original_owner_incarnation_and_generation_after_recreation() {
    for recreate in [false, true] {
        let fixture = Fixture::new(4096, 1000).await;
        let item = fixture.protected(BODY).await;
        fixture
            .catalog
            .admin(Origin::Api)
            .delete_object(
                &fixture.bucket,
                &ObjectName::new("object").unwrap(),
                crate::storage::admin::GenerationCondition::Is(item.generation),
            )
            .await
            .unwrap();
        fixture
            .catalog
            .delete_bucket(fixture.bucket.as_str(), None, None)
            .await
            .unwrap();
        if recreate {
            fixture
                .catalog
                .admin(Origin::BucketInit)
                .ensure_bucket(
                    BucketDefinition::default_for(&fixture.bucket, "other-owner", &SystemClock)
                        .unwrap(),
                )
                .await
                .unwrap();
        }
        let fixture = fixture.restart().await;
        fixture.writeback.recover_pending().await.unwrap();
        fixture.writeback.preserve_protected().await.unwrap();
        let page = fixture.page().await;
        assert_eq!(page.items[0].owner_project, "owner");
        assert_eq!(page.items[0].bucket_incarnation, item.incarnation);
        assert_eq!(page.items[0].generation, Some(item.generation.get()));
        assert_eq!(
            fs::read(
                fixture
                    .data
                    .0
                    .join("sync-conflicts")
                    .join(&page.items[0].conflict_id)
            )
            .unwrap(),
            BODY
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn protected_transfer_refuses_missing_proof_or_changed_owned_copy() {
    for change_copy in [false, true] {
        let fixture = Fixture::new(4096, 1000).await;
        let item = fixture.protected(BODY).await;
        if change_copy {
            fixture
                .catalog
                .inject(Step::TransferAccepted, 0, Fault::Io(io::ErrorKind::Other));
            assert!(fixture.writeback.preserve_protected().await.is_err());
            let id = &fixture.page().await.items[0].conflict_id;
            fs::remove_file(fixture.data.0.join("sync-conflicts").join(id)).unwrap();
        } else {
            fs::remove_file(
                fixture
                    .tree
                    .0
                    .join(mapping::WORKSPACE)
                    .join(item.token.id()),
            )
            .unwrap();
        }
        assert!(fixture.writeback.preserve_protected().await.is_err());
        assert_eq!(fixture.writeback.status().await.unwrap().entries, 1);
        if change_copy {
            assert_eq!(
                fs::read(
                    fixture
                        .tree
                        .0
                        .join(mapping::WORKSPACE)
                        .join(item.token.id())
                )
                .unwrap(),
                BODY
            );
        }
        fixture.assert_api().await;
        fixture.close().await;
    }
}
