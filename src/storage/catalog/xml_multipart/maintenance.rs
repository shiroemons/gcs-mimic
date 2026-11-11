use super::*;
use crate::storage::catalog::{Work, sql};
use std::sync::atomic::Ordering;

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

pub(super) fn pending(connection: &Connection) -> Result<bool, ObjectError> {
    sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM xml_multipart_deletions)",
        [],
    )
    .map_err(object_unavailable)
}

/// Reads one page of expired uploads through the deadline index, so a maintenance pass costs the
/// uploads that are due rather than every upload.
pub(super) const EXPIRED_PAGE: &str = "SELECT deadline,id FROM xml_multipart_uploads WHERE deadline<=?1 AND (deadline,id)>(?2,?3) ORDER BY deadline,id LIMIT 128";

/// Reads one page of uploads whose bucket incarnation is gone. Nothing indexes an upload by its
/// incarnation, so this walks every upload; only a deleted bucket or an open needs it.
const ORPHANED_PAGE: &str = "SELECT id FROM xml_multipart_uploads u WHERE id>?1 AND NOT EXISTS(SELECT 1 FROM bucket_incarnations b WHERE b.bucket=u.bucket AND b.incarnation=u.incarnation) ORDER BY id LIMIT 128";

/// A place in deadline order. Keyset pages keep memory and each job short however many uploads
/// are due, and let a held upload be stepped over.
struct ExpiredUpload {
    deadline: i64,
    id: String,
}

impl ExpiredUpload {
    const BEFORE_ALL: Self = Self {
        deadline: i64::MIN,
        id: String::new(),
    };
}

fn expired(
    connection: &Connection,
    now: i64,
    after: &ExpiredUpload,
) -> Result<Vec<ExpiredUpload>, CatalogError> {
    connection
        .prepare_cached(EXPIRED_PAGE)
        .and_then(|mut statement| {
            statement
                .query_map(params![now, after.deadline, after.id], |row| {
                    Ok(ExpiredUpload {
                        deadline: row.get(0)?,
                        id: row.get(1)?,
                    })
                })?
                .collect()
        })
        .map_err(super::super::unavailable)
}

fn orphaned(connection: &Connection, after: &str) -> Result<Vec<String>, CatalogError> {
    connection
        .prepare_cached(ORPHANED_PAGE)
        .and_then(|mut statement| statement.query_map([after], |row| row.get(0))?.collect())
        .map_err(super::super::unavailable)
}

fn retire_at_recovery(connection: &Connection, id: &str) -> Result<(), CatalogError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    if let Err(error) = retire(connection, id) {
        connection.execute_batch("ROLLBACK")?;
        return Err(super::super::unavailable(error));
    }
    connection.execute_batch("COMMIT")?;
    Ok(())
}

pub(super) fn recover(
    connection: &Connection,
    data_dir: &Path,
    now: i64,
) -> Result<(), CatalogError> {
    // No request holds an upload yet, so every page retires completely and the next one starts
    // from the beginning again.
    loop {
        let page = expired(connection, now, &ExpiredUpload::BEFORE_ALL)?;
        if page.is_empty() {
            break;
        }
        for upload in &page {
            retire_at_recovery(connection, &upload.id)?;
        }
    }
    loop {
        let page = orphaned(connection, "")?;
        if page.is_empty() {
            break;
        }
        for id in &page {
            retire_at_recovery(connection, id)?;
        }
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
    /// The lifecycle caller holds the whole-upload lease and the retirement transaction.
    pub(in crate::storage::catalog) fn retire_lifecycle_multipart(
        connection: &Connection,
        lease: &resumable::WriterGuard,
    ) -> Result<(), ObjectError> {
        let id = lease.multipart_id().ok_or_else(|| {
            object_unavailable("multipart retirement requires a whole-upload lease")
        })?;
        retire(connection, id)
    }

    /// Journal rows survive failed file work; owned cleanup settles before releasing its slot.
    pub(super) async fn drain_xml_multipart(&self) -> Result<bool, ObjectError> {
        // A part or a completion drains under its own lease; the sweep has none to wait for.
        let activity = self.await_activity(Work::UnderWay).await?;
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
            let pending = if catalog.0.backend.is_memory() {
                let backend = catalog.0.backend.clone();
                catalog
                    .run_upload(move |connection| drain_memory(connection, &backend))
                    .await?
            } else {
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
                catalog.run_upload(|connection| pending(connection)).await?
            };
            if pending {
                catalog.0.maintenance_wake.notify_one();
            }
            Ok(pending)
        })
        .await?
    }

    /// Retires the uploads of one page and counts those it stepped over because a request holds
    /// them. A part still writing keeps its upload here, however few parts remain.
    fn retire_unheld<'a>(
        &self,
        connection: &mut Connection,
        ids: impl Iterator<Item = &'a str>,
    ) -> Result<usize, CatalogError> {
        let mut held = 0;
        for id in ids {
            let _guard =
                match self.claim_writer(WriterKey::Multipart(id.to_owned()), KeyOrigin::Catalog) {
                    Ok(guard) => guard,
                    Err(ClaimError::Busy) => {
                        held += 1;
                        continue;
                    }
                    Err(error @ (ClaimError::Poisoned | ClaimError::Catalog(_))) => {
                        return Err(CatalogError::from(error));
                    }
                };
            let transaction = sql::immediate(connection)?;
            retire(&transaction, id).map_err(super::super::unavailable)?;
            transaction.commit()?;
        }
        Ok(held)
    }

    async fn retire_expired(&self, now: i64) -> Result<(), CatalogError> {
        let mut after = ExpiredUpload::BEFORE_ALL;
        loop {
            let catalog = self.clone();
            let last = self
                .run(move |connection| {
                    let mut page = expired(connection, now, &after)?;
                    catalog
                        .retire_unheld(connection, page.iter().map(|upload| upload.id.as_str()))?;
                    Ok::<_, CatalogError>(page.pop())
                })
                .await?;
            match last {
                Some(last) => after = last,
                None => return Ok(()),
            }
        }
    }

    /// Asks the next maintenance pass to look for uploads whose bucket is gone.
    pub(in crate::storage::catalog) fn schedule_multipart_orphan_scan(&self) {
        self.0.multipart_orphan_scan.store(true, Ordering::Release);
        self.0.maintenance_wake.notify_one();
    }

    /// Once the catalog is open only a deleted bucket leaves orphans behind, and recovery covers
    /// the time before. Walking every upload only then keeps other passes cheap. A missed orphan
    /// costs little: clients cannot reach it, and it still retires at its deadline.
    async fn retire_orphaned(&self) -> Result<(), CatalogError> {
        // Cleared before the scan, so a bucket deleted during it asks for another.
        if !self.0.multipart_orphan_scan.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let scanned = self.scan_for_orphans().await;
        // A held orphan wakes maintenance when its lease drops, and a failed pass runs again;
        // either way the next pass must look again.
        if !matches!(scanned, Ok(0)) {
            self.0.multipart_orphan_scan.store(true, Ordering::Release);
        }
        scanned.map(|_held| ())
    }

    async fn scan_for_orphans(&self) -> Result<usize, CatalogError> {
        let mut after = String::new();
        let mut held = 0;
        loop {
            let catalog = self.clone();
            let (last, skipped) = self
                .run(move |connection| {
                    let mut page = orphaned(connection, &after)?;
                    let skipped =
                        catalog.retire_unheld(connection, page.iter().map(String::as_str))?;
                    Ok::<_, CatalogError>((page.pop(), skipped))
                })
                .await?;
            held += skipped;
            match last {
                Some(last) => after = last,
                None => return Ok(held),
            }
        }
    }

    pub(in crate::storage::catalog) async fn maintain_xml_multipart(
        &self,
        now: i64,
    ) -> Result<Option<SystemTime>, CatalogError> {
        self.retire_expired(now).await?;
        self.retire_orphaned().await?;
        let deadline: Option<i64> = self
            .run(|connection| {
                sql::scalar(
                    connection,
                    "SELECT min(deadline) FROM xml_multipart_uploads",
                    [],
                )
                .map_err(super::super::unavailable)
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
