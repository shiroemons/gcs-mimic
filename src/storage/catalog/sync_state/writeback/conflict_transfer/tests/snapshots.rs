use super::*;
use crate::{
    hashing::Hasher,
    storage::catalog::fault::{self, Fault, Step},
};

#[derive(serde::Serialize, serde::Deserialize)]
struct SnapshotObservation {
    device: u64,
    inode: u64,
    size: u64,
    modified_ns: i128,
    changed_ns: i128,
    hashes: Option<SnapshotHashes>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SnapshotHashes {
    md5: [u8; 16],
    crc32c: u32,
}

fn restore_tree(snapshot: &std::path::Path, tree: &std::path::Path) {
    let workspace = tree.join(mapping::WORKSPACE);
    for entry in fs::read_dir(&workspace).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    for entry in fs::read_dir(snapshot.join(mapping::WORKSPACE)).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), workspace.join(entry.file_name())).unwrap();
    }
}

fn translated(original: &[u8], file: &std::path::Path) -> Vec<u8> {
    let mut value: serde_json::Value = serde_json::from_slice(original).unwrap();
    let identity = fsutil::stat_identity(&File::open(file).unwrap()).unwrap();
    let body = fs::read(file).unwrap();
    let mut hash = Hasher::default();
    hash.update(&body);
    let hash = hash.finish();
    assert_eq!(value["size"], serde_json::json!(identity.size));
    if !value["hashes"].is_null() {
        assert_eq!(value["hashes"]["md5"], serde_json::json!(hash.md5));
        assert_eq!(value["hashes"]["crc32c"], serde_json::json!(hash.crc32c));
    } else {
        assert_eq!(body, BODY);
    }
    value["device"] = serde_json::json!(identity.device);
    value["inode"] = serde_json::json!(identity.inode);
    value["modified_ns"] = serde_json::json!(identity.modified_ns);
    value["changed_ns"] = serde_json::json!(identity.changed_ns);
    let canonical: SnapshotObservation =
        serde_json::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
    serde_json::to_vec(&canonical).unwrap()
}

async fn translate_snapshot(
    catalog: &Catalog,
    data: &std::path::Path,
    tree: &std::path::Path,
    op: &str,
) {
    let owned = tree.join(mapping::WORKSPACE).join(op);
    let op = op.to_owned();
    let data = data.to_owned();
    catalog
        .run_admin(move |connection| {
            let row = connection
                .prepare_cached(
                    "SELECT id,source,retained FROM sync_conflicts WHERE source_op=?1 AND state!=2",
                )?
                .query_row([&op], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                })
                .optional()?;
            let Some((id, source, retained)) = row else {
                return Ok(());
            };
            let source = if owned.exists() {
                translated(&source, &owned)
            } else {
                source
            };
            let retained =
                retained.map(|bytes| translated(&bytes, &data.join("sync-conflicts").join(&id)));
            connection
                .prepare_cached("UPDATE sync_conflicts SET source=?2,retained=?3 WHERE id=?1")?
                .execute(params![id, source, retained])?;
            let snapshot: Option<Vec<u8>> = connection
                .prepare_cached("SELECT snapshot FROM sync_writeback_ops WHERE id=?1")?
                .query_row([&op], |row| row.get(0))?;
            if let Some(snapshot) = snapshot {
                let mut snapshot: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
                if snapshot["transfer"].is_object() {
                    snapshot["transfer"]["source"] = serde_json::from_slice(&source).unwrap();
                    snapshot["transfer"]["retained"] =
                        serde_json::from_slice(retained.as_ref().unwrap()).unwrap();
                    connection
                        .prepare_cached("UPDATE sync_writeback_ops SET snapshot=?2 WHERE id=?1")?
                        .execute(params![op, serde_json::to_vec(&snapshot).unwrap()])?;
                }
            }
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn protected_transfer_paired_crash_and_power_loss_reopen_catalog_api_and_finish_quota_transfer()
 {
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
        for power_loss in [false, true] {
            let fixture = Fixture::new(4096, 1000).await;
            let item = fixture.protected(BODY).await;
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
            fixture.catalog.inject(
                step,
                0,
                if power_loss {
                    Fault::PowerLoss(data.clone())
                } else {
                    Fault::Snapshot(data.clone())
                },
            );
            fixture.writeback.preserve_protected().await.unwrap();
            assert_eq!(
                (
                    fixture.catalog.pending_faults(),
                    fault::pending(&fixture.tree.0)
                ),
                (0, 0),
                "{step:?}"
            );
            let Fixture {
                data: original_data,
                tree: original_tree,
                catalog,
                lease,
                writeback,
                bucket,
                record,
            } = fixture;
            drop(writeback);
            drop(lease);
            catalog.close().await.unwrap();
            restore_tree(&tree, &original_tree.0);
            let catalog = Catalog::open(&data).await.unwrap();
            translate_snapshot(&catalog, &data, &original_tree.0, item.token.id()).await;
            let lease = catalog
                .sync_state()
                .activate_root(&original_tree.0, Activation::Open)
                .await
                .unwrap();
            let writeback = lease
                .writeback(Limits::new(4096, 1000).unwrap())
                .await
                .unwrap();
            let recovered = Fixture {
                data: original_data,
                tree: original_tree,
                catalog,
                lease,
                writeback,
                bucket,
                record,
            };
            recovered.assert_api().await;
            recovered.writeback.recover_pending().await.unwrap();
            let usage = recovered
                .writeback
                .preserve_protected()
                .await
                .unwrap_or_else(|error| panic!("{step:?} power={power_loss}: {error:?}"));
            assert_eq!(
                (usage.entries, usage.retained_bytes, usage.stop),
                (0, 0, Stop::Ready),
                "{step:?} {power_loss}"
            );
            let page = recovered.page().await;
            assert_eq!(
                (page.usage.entries, page.usage.retained_bytes),
                (1, BODY.len() as u128),
                "{step:?} {power_loss}"
            );
            assert_eq!(
                fs::read(data.join("sync-conflicts").join(&page.items[0].conflict_id)).unwrap(),
                BODY
            );
            recovered.assert_api().await;
            recovered.close().await;
        }
    }
}
