use super::*;
use crate::storage::catalog::{bucket_soft_delete, soft_delete};
use std::sync::atomic::Ordering;

/// An earliest deadline doubles as the due flag and the scheduler's next wake if the sweep sleeps.
#[derive(Debug)]
pub(super) struct DueWork {
    pub(super) derived: Option<SystemTime>,
    pub(super) resumable: Option<SystemTime>,
    pub(super) rewrite: Option<SystemTime>,
    pub(super) multipart: Option<SystemTime>,
    pub(super) buckets: Option<SystemTime>,
    pub(super) objects: Option<SystemTime>,
    pub(super) journal: bool,
}

impl DueWork {
    pub(super) fn all(now: SystemTime) -> Self {
        Self {
            derived: Some(now),
            resumable: Some(now),
            rewrite: Some(now),
            multipart: Some(now),
            buckets: Some(now),
            objects: Some(now),
            journal: true,
        }
    }
}

impl Catalog {
    pub(super) async fn maintenance_probe(&self, now: SystemTime) -> Result<DueWork, CatalogError> {
        let orphans = self
            .0
            .maintenance
            .multipart_orphan_scan
            .load(Ordering::Acquire);
        self.run_read(move |connection| {
            let snapshot = sql::read_snapshot(connection)?;
            let (derived, resumable, rewrite, multipart, cancelled, parts) = sql::row(
                &snapshot,
                "SELECT (SELECT min(deadline) FROM derived_transfers),
                        (SELECT min(deadline) FROM resumable_sessions),
                        (SELECT min(deadline) FROM rewrite_sessions),
                        (SELECT min(deadline) FROM xml_multipart_uploads),
                        EXISTS(SELECT 1 FROM pending_blob_deletions p CROSS JOIN resumable_sessions s
                               ON s.blob_id = p.blob_id WHERE s.state = 'CANCELLED'),
                        EXISTS(SELECT 1 FROM xml_multipart_deletions)",
                [], |row| {
                    let deadline = |column| row.get::<_, Option<UnixMillis>>(column)?.map(UnixMillis::system_time).transpose();
                    Ok((deadline(0)?, deadline(1)?, deadline(2)?, deadline(3)?, row.get::<_, bool>(4)?, row.get::<_, bool>(5)?))
                },
            )?;
            Ok(DueWork {
                derived,
                resumable: if cancelled { Some(now) } else { resumable },
                rewrite,
                multipart: if parts || orphans { Some(now) } else { multipart },
                buckets: bucket_soft_delete::earliest_hard_delete(&snapshot)?.map(UnixMicros::system_time).transpose()?,
                objects: object_expiry(&snapshot)?,
                journal: journal_due(&snapshot)?,
            })
        }).await
    }
}

pub(super) fn object_expiry(connection: &Connection) -> Result<Option<SystemTime>, CatalogError> {
    Ok(soft_delete::earliest_expiry(connection)?
        .map(|(_, time)| time.system_time())
        .transpose()?)
}

pub(super) fn journal_due(connection: &Connection) -> Result<bool, CatalogError> {
    // Cancelled sessions own their journal markers until their staging cleanup succeeds.
    // Conflicts with live objects must still be reported by reclamation.
    Ok(sql::scalar(connection, "SELECT EXISTS(SELECT 1 FROM pending_blob_deletions p
        WHERE NOT EXISTS(SELECT 1 FROM resumable_sessions s WHERE s.blob_id=p.blob_id AND s.state!='COMPLETE')
           OR EXISTS(SELECT 1 FROM objects o WHERE o.blob_id=p.blob_id))", [])?)
}
