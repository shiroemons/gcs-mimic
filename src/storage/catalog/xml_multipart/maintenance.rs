use super::*;
use crate::storage::catalog::sql;

pub(super) fn retire(connection: &Connection, id: &str) -> Result<(), ObjectError> {
    sql::exec(
        connection,
        "INSERT OR IGNORE INTO xml_multipart_deletions SELECT blob_id FROM xml_multipart_parts WHERE upload_id=?1",
        [id],
    )?;
    sql::exec(
        connection,
        "DELETE FROM xml_multipart_uploads WHERE id=?1",
        [id],
    )?;
    Ok(())
}

/// A journal entry survives a failed unlink or directory sync and is retried by maintenance.
pub(super) fn drain_storage(
    connection: &Connection,
    backend: &super::super::BlobBackend,
) -> Result<bool, ObjectError> {
    let ids = connection
        .prepare_cached("SELECT blob_id FROM xml_multipart_deletions ORDER BY blob_id LIMIT 32")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, BlobId>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })?;
    for id in ids {
        match backend {
            super::super::BlobBackend::Memory(_) => {
                let referenced: bool = sql::scalar(
                    connection,
                    "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id=?1 UNION ALL SELECT 1 FROM xml_multipart_parts WHERE blob_id=?1 UNION ALL SELECT 1 FROM derived_transfers WHERE id=?1)",
                    [&id],
                )?;
                if referenced {
                    return Err(object_unavailable(
                        "multipart deletion journal references retained bytes",
                    ));
                }
                backend.remove_draft(&id)?;
                backend.remove(&id)?;
            }
            super::super::BlobBackend::Disk(data_dir) => {
                #[cfg(test)]
                super::super::fault::hit(data_dir, super::super::fault::Step::MultipartReclaim)
                    .map_err(object_unavailable)?;
                backend.remove_draft(&id)?;
                #[cfg(unix)]
                {
                    #[cfg(test)]
                    super::super::fault::hit(
                        data_dir,
                        super::super::fault::Step::MultipartReclaimSync,
                    )
                    .map_err(object_unavailable)?;
                    objects::sync_dir(&data_dir.join("staging"))?;
                }
            }
        }
        sql::exec(
            connection,
            "DELETE FROM xml_multipart_deletions WHERE blob_id=?1",
            [id],
        )?;
    }
    sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM xml_multipart_deletions)",
        [],
    )
    .map_err(object_unavailable)
}

fn due(connection: &Connection, now: i64) -> Result<Vec<String>, CatalogError> {
    connection.prepare_cached("SELECT id FROM xml_multipart_uploads u WHERE deadline<=?1 OR NOT EXISTS(SELECT 1 FROM bucket_incarnations b WHERE b.bucket=u.bucket AND b.incarnation=u.incarnation) ORDER BY id LIMIT 128")
        .and_then(|mut s|s.query_map([now],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()).map_err(super::super::unavailable)
}

pub(super) fn recover(
    connection: &Connection,
    backend: &super::super::BlobBackend,
    now: i64,
) -> Result<(), CatalogError> {
    let super::super::BlobBackend::Disk(data_dir) = backend else {
        return Ok(());
    };
    for id in due(connection, now)? {
        connection.execute_batch("BEGIN IMMEDIATE")?;
        if let Err(error) = retire(connection, &id) {
            connection.execute_batch("ROLLBACK")?;
            return Err(super::super::unavailable(error));
        }
        connection.execute_batch("COMMIT")?;
    }
    while drain_storage(connection, backend).map_err(super::super::unavailable)? {}
    let mut after = String::new();
    loop {
        let part:Option<(BlobId,u64)>=sql::row(connection, "SELECT blob_id,size FROM xml_multipart_parts WHERE blob_id>?1 ORDER BY blob_id LIMIT 1", [&after],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((id, size)) = part else { break };
        let metadata = std::fs::symlink_metadata(id.staging_path(data_dir))?;
        if !metadata.is_file() || metadata.len() != size {
            return Err(super::super::unavailable(
                "multipart part is missing or truncated",
            ));
        }
        after = id.to_hex();
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
                let transaction = sql::immediate(connection)?;
                retire(&transaction, &id).map_err(super::super::unavailable)?;
                transaction.commit()?;
            }
            let pending =
                drain_storage(connection, &catalog.0.backend).map_err(super::super::unavailable)?;
            let deadline: Option<i64> = sql::scalar(
                connection,
                "SELECT min(deadline) FROM xml_multipart_uploads",
                [],
            )?;
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
