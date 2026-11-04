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

fn drain_memory(
    connection: &Connection,
    backend: &super::super::BlobBackend,
) -> Result<bool, ObjectError> {
    drain_with(connection, |id| {
        let referenced: bool = sql::scalar(
            connection,
            "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id=?1 UNION ALL SELECT 1 FROM xml_multipart_parts WHERE blob_id=?1 UNION ALL SELECT 1 FROM derived_transfers WHERE id=?1)",
            [id],
        )?;
        if referenced {
            return Err(object_unavailable(
                "multipart deletion journal references retained bytes",
            ));
        }
        backend.remove_draft(id)?;
        backend.remove(id)?;
        Ok(())
    })
}

fn reclaim_disk(data_dir: &Path, ids: &[BlobId]) -> Result<(), ObjectError> {
    for id in ids {
        #[cfg(test)]
        super::super::fault::hit(data_dir, super::super::fault::Step::MultipartReclaim)
            .map_err(object_unavailable)?;
        objects::unlink(&id.staging_path(data_dir))?;
    }
    #[cfg(unix)]
    {
        #[cfg(test)]
        super::super::fault::hit(data_dir, super::super::fault::Step::MultipartReclaimSync)
            .map_err(object_unavailable)?;
        objects::sync_dir(&data_dir.join("staging"))?;
    }
    Ok(())
}

fn drain_with(
    connection: &Connection,
    mut reclaim: impl FnMut(&BlobId) -> Result<(), ObjectError>,
) -> Result<bool, ObjectError> {
    for id in pending_ids(connection)? {
        reclaim(&id)?;
        forget(connection, &[id])?;
    }
    pending(connection)
}

fn pending_ids(connection: &Connection) -> Result<Vec<BlobId>, ObjectError> {
    connection
        .prepare_cached("SELECT blob_id FROM xml_multipart_deletions ORDER BY blob_id LIMIT 32")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, BlobId>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(object_unavailable)
}

fn forget(connection: &Connection, ids: &[BlobId]) -> Result<(), ObjectError> {
    for id in ids {
        sql::exec(
            connection,
            "DELETE FROM xml_multipart_deletions WHERE blob_id=?1",
            [id],
        )?;
    }
    Ok(())
}

fn pending(connection: &Connection) -> Result<bool, ObjectError> {
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
    data_dir: &Path,
    now: i64,
) -> Result<(), CatalogError> {
    for id in due(connection, now)? {
        connection.execute_batch("BEGIN IMMEDIATE")?;
        if let Err(error) = retire(connection, &id) {
            connection.execute_batch("ROLLBACK")?;
            return Err(super::super::unavailable(error));
        }
        connection.execute_batch("COMMIT")?;
    }
    loop {
        let ids = pending_ids(connection).map_err(super::super::unavailable)?;
        if ids.is_empty() {
            break;
        }
        reclaim_disk(data_dir, &ids).map_err(super::super::unavailable)?;
        forget(connection, &ids).map_err(super::super::unavailable)?;
    }
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
    /// Journal rows survive failed file work; owned cleanup settles before releasing its slot.
    pub(super) async fn drain_xml_multipart(&self) -> Result<bool, ObjectError> {
        let activity = self.activity()?;
        let permit = self
            .0
            .reclamation
            .clone()
            .acquire_owned()
            .await
            .map_err(object_unavailable)?;
        let catalog = self.clone();
        crate::task::spawn_supervised("multipart-reclamation", async move {
            let (_activity, _permit) = (activity, permit);
            if catalog.0.backend.is_memory() {
                let backend = catalog.0.backend.clone();
                return catalog
                    .run_upload(move |connection| drain_memory(connection, &backend))
                    .await;
            }
            let ids = catalog
                .run_upload(|connection| pending_ids(connection))
                .await?;
            if !ids.is_empty() {
                let data_dir = catalog.0.data_dir.clone();
                let ids = crate::task::run_blocking("multipart-reclamation", move || {
                    reclaim_disk(&data_dir, &ids)?;
                    Ok::<_, ObjectError>(ids)
                })
                .await??;
                catalog
                    .run_upload(move |connection| {
                        #[cfg(test)]
                        super::super::fault::hit(
                            &super::super::fault::database_dir(connection),
                            super::super::fault::Step::MultipartReclaimForget,
                        )
                        .map_err(object_unavailable)?;
                        let transaction = sql::immediate(connection)?;
                        forget(&transaction, &ids)?;
                        transaction.commit().map_err(object_unavailable)
                    })
                    .await?;
            }
            catalog.run_upload(|connection| pending(connection)).await
        })
        .await?
    }

    pub(in crate::storage::catalog) async fn maintain_xml_multipart(
        &self,
        now: i64,
    ) -> Result<Option<SystemTime>, CatalogError> {
        let catalog = self.clone();
        let deadline = self
            .run(move |connection| {
                for id in due(connection, now)? {
                    let _guard = match catalog
                        .claim_writer(WriterKey::Multipart(id.clone()), KeyOrigin::Catalog)
                    {
                        Ok(guard) => guard,
                        Err(ClaimError::Busy) => continue,
                        Err(error) => return Err(CatalogError::from(error)),
                    };
                    let transaction = sql::immediate(connection)?;
                    retire(&transaction, &id).map_err(super::super::unavailable)?;
                    transaction.commit()?;
                }
                let deadline: Option<i64> = sql::scalar(
                    connection,
                    "SELECT min(deadline) FROM xml_multipart_uploads",
                    [],
                )?;
                Ok(deadline)
            })
            .await?;
        if self
            .drain_xml_multipart()
            .await
            .map_err(super::super::unavailable)?
        {
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
    }
}
