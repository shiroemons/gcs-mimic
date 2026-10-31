use super::*;

pub(super) fn retire(connection: &Connection, id: &str) -> Result<(), ObjectError> {
    connection.prepare_cached("INSERT OR IGNORE INTO xml_multipart_deletions SELECT blob_id FROM xml_multipart_parts WHERE upload_id=?1")
        .and_then(|mut s|s.execute([id])).map_err(object_unavailable)?;
    connection
        .prepare_cached("DELETE FROM xml_multipart_uploads WHERE id=?1")
        .and_then(|mut s| s.execute([id]))
        .map_err(object_unavailable)?;
    Ok(())
}

/// A journal entry survives a failed unlink or directory sync and is retried by maintenance.
pub(super) fn drain(connection: &Connection, data_dir: &Path) -> Result<bool, ObjectError> {
    drain_storage(connection, data_dir, None)
}

pub(super) fn drain_storage(
    connection: &Connection,
    data_dir: &Path,
    memory: Option<&crate::storage::memory::Store>,
) -> Result<bool, ObjectError> {
    let ids = connection
        .prepare_cached("SELECT blob_id FROM xml_multipart_deletions ORDER BY blob_id LIMIT 32")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(object_unavailable)?;
    for id in ids {
        let blob = super::super::super::blob_id::BlobId::parse(&id)
            .ok_or_else(|| object_unavailable("invalid multipart blob ID"))?;
        if let Some(memory) = memory {
            let referenced: bool = connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id=?1 UNION ALL SELECT 1 FROM xml_multipart_parts WHERE blob_id=?1 UNION ALL SELECT 1 FROM derived_transfers WHERE id=?1)").and_then(|mut s| s.query_row([&id], |row| row.get(0))).map_err(object_unavailable)?;
            if referenced {
                return Err(object_unavailable(
                    "multipart deletion journal references retained bytes",
                ));
            }
            memory.remove_draft(&id).map_err(object_unavailable)?;
            memory.remove(&id).map_err(object_unavailable)?;
        } else {
            #[cfg(test)]
            super::super::fault::hit(data_dir, super::super::fault::Step::MultipartReclaim)
                .map_err(object_unavailable)?;
            objects::unlink(&blob.staging_path(data_dir)).map_err(object_unavailable)?;
            #[cfg(unix)]
            {
                #[cfg(test)]
                super::super::fault::hit(data_dir, super::super::fault::Step::MultipartReclaimSync)
                    .map_err(object_unavailable)?;
                objects::sync_dir(&data_dir.join("staging")).map_err(object_unavailable)?;
            }
        }
        connection
            .prepare_cached("DELETE FROM xml_multipart_deletions WHERE blob_id=?1")
            .and_then(|mut s| s.execute([id]))
            .map_err(object_unavailable)?;
    }
    connection
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM xml_multipart_deletions)")
        .and_then(|mut s| s.query_row([], |r| r.get(0)))
        .map_err(object_unavailable)
}

fn due(connection: &Connection, now: i64) -> Result<Vec<String>, CatalogError> {
    connection.prepare_cached("SELECT id FROM xml_multipart_uploads u WHERE deadline<=?1 OR NOT EXISTS(SELECT 1 FROM bucket_incarnations b WHERE b.bucket=u.bucket AND b.incarnation=u.incarnation) ORDER BY id LIMIT 128")
        .and_then(|mut s|s.query_map([now],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()).map_err(super::super::unavailable)
}

pub(super) fn recover(
    connection: &Connection,
    data_dir: &Path,
    now: i64,
) -> Result<(), CatalogError> {
    for id in due(connection, now)? {
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(super::super::unavailable)?;
        if let Err(error) = retire(connection, &id) {
            connection
                .execute_batch("ROLLBACK")
                .map_err(super::super::unavailable)?;
            return Err(super::super::unavailable(error));
        }
        connection
            .execute_batch("COMMIT")
            .map_err(super::super::unavailable)?;
    }
    while drain(connection, data_dir).map_err(super::super::unavailable)? {}
    let mut after = String::new();
    loop {
        let part:Option<(String,i64)>=connection.prepare_cached("SELECT blob_id,size FROM xml_multipart_parts WHERE blob_id>?1 ORDER BY blob_id LIMIT 1")
            .and_then(|mut s|s.query_row([&after],|r|Ok((r.get(0)?,r.get(1)?)))).optional().map_err(super::super::unavailable)?;
        let Some((id, size)) = part else { break };
        let blob = super::super::super::blob_id::BlobId::parse(&id)
            .ok_or_else(|| super::super::unavailable("invalid multipart blob ID"))?;
        let metadata = std::fs::symlink_metadata(blob.staging_path(data_dir))
            .map_err(super::super::unavailable)?;
        if !metadata.is_file() || metadata.len() != size as u64 {
            return Err(super::super::unavailable(
                "multipart part is missing or truncated",
            ));
        }
        after = id;
    }
    Ok(())
}

impl Catalog {
    pub(in crate::storage::catalog) async fn maintain_xml_multipart(
        &self,
        now: i64,
    ) -> Result<Option<SystemTime>, CatalogError> {
        let catalog = self.clone();
        self.run(move |connection| {
            for id in due(connection, now)? {
                let guard_id = format!("multipart:{id}");
                let activity = catalog.activity()?;
                if !catalog
                    .0
                    .session_writers
                    .lock()
                    .map_err(super::super::unavailable)?
                    .insert(guard_id.clone())
                {
                    continue;
                }
                let _guard = resumable::WriterGuard {
                    catalog: catalog.clone(),
                    id: guard_id,
                    _activity: activity,
                };
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(super::super::unavailable)?;
                retire(&transaction, &id).map_err(super::super::unavailable)?;
                transaction.commit().map_err(super::super::unavailable)?;
            }
            let pending =
                drain_storage(connection, &catalog.0.data_dir, catalog.0.memory.as_deref())
                    .map_err(super::super::unavailable)?;
            let deadline: Option<i64> = connection
                .prepare_cached("SELECT min(deadline) FROM xml_multipart_uploads")
                .and_then(|mut s| s.query_row([], |r| r.get(0)))
                .map_err(super::super::unavailable)?;
            if pending {
                return system_time_millis(now)
                    .map(Some)
                    .map_err(super::super::unavailable);
            }
            // Busy expired uploads wake the scheduler when their owned operation releases its lease.
            deadline
                .filter(|value| *value > now)
                .map(system_time_millis)
                .transpose()
                .map_err(super::super::unavailable)
        })
        .await
    }
}
