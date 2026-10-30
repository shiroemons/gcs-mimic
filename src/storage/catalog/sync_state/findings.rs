//! Durable observations for names the working tree cannot represent.
#![deny(clippy::unwrap_used, clippy::expect_used)]
// xtask-lint: allow-file(text::uncached_statement) -- findings are recorded only for the rare names a working tree cannot represent

use super::super::{objects, unavailable};
use super::{
    CatalogId, SyncBinding, matching,
    root::{RootError, RootId, RootLease},
};
use crate::{
    bucket::CatalogError,
    storage::admin::{AdminError, Generation, ObjectName},
    sync::mapping::Reason,
};
use rusqlite::{Connection, params};

pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE sync_findings (
    ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
    root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    reason TEXT NOT NULL CHECK(reason IN ('invalid-name','host-restriction','reserved','ignored','collision','unsafe-entry')),
    UNIQUE(root_id,bucket,name)
);
CREATE INDEX sync_findings_page ON sync_findings(root_id,ordinal);
PRAGMA user_version = 21;";

pub(in crate::storage) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let tx = connection.transaction().map_err(unavailable)?;
    tx.execute_batch(SCHEMA).map_err(unavailable)?;
    tx.commit().map_err(unavailable)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub catalog_id: CatalogId,
    pub root_id: RootId,
    pub ordinal: i64,
    pub bucket: String,
    pub name: String,
    pub incarnation: String,
    pub generation: Generation,
    pub reason: Reason,
}

fn reason_text(reason: Reason) -> &'static str {
    match reason {
        Reason::InvalidName => "invalid-name",
        Reason::HostRestriction => "host-restriction",
        Reason::Reserved => "reserved",
        Reason::Ignored => "ignored",
        Reason::Collision => "collision",
        Reason::UnsafeEntry => "unsafe-entry",
    }
}

fn read_reason(reason: &str) -> rusqlite::Result<Reason> {
    match reason {
        "invalid-name" => Ok(Reason::InvalidName),
        "host-restriction" => Ok(Reason::HostRestriction),
        "reserved" => Ok(Reason::Reserved),
        "ignored" => Ok(Reason::Ignored),
        "collision" => Ok(Reason::Collision),
        "unsafe-entry" => Ok(Reason::UnsafeEntry),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

impl RootLease {
    /// Conflicts may refer to a deleted API object whose synchronized body is still tracked.
    pub(crate) async fn record_export_conflict(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        generation: Generation,
    ) -> Result<(), RootError> {
        self.file_job(|_| Ok(())).await?;
        let lease = self.clone();
        let root = self.status().root_id.as_bytes();
        self.catalog().run_admin(move |c| {
            let _lease = lease;
            let tx = c.transaction().map_err(unavailable)?;
            matching(&tx,&binding,&binding.bucket).map_err(unavailable)?;
            let live = objects::lookup(&tx,&binding.bucket,name.as_str()).map_err(unavailable)?.is_some_and(|(record,_)| record.generation == generation.get());
            let tracked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?4 AND generation=?3)",params![binding.bucket,name.as_str(),generation.get(),binding.incarnation],|r| r.get(0)).map_err(unavailable)?;
            let valid = live || tracked;
            if valid {
                tx.execute("INSERT INTO sync_findings(root_id,bucket,name,incarnation,generation,reason) VALUES(?1,?2,?3,?4,?5,'collision') ON CONFLICT(root_id,bucket,name) DO UPDATE SET incarnation=excluded.incarnation,generation=excluded.generation,reason=excluded.reason",params![root,binding.bucket,name.as_str(),binding.incarnation,generation.get()]).map_err(unavailable)?;
            }
            tx.commit().map_err(unavailable)
        }).await?;
        Ok(())
    }

    /// Records only the selected live incarnation/generation; no filesystem path is required.
    pub(crate) async fn record_finding(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        generation: Generation,
        reason: Reason,
    ) -> Result<(), RootError> {
        self.file_job(|_| Ok(())).await?;
        let lease = self.clone();
        let root = self.status().root_id.as_bytes();
        self.catalog().run_admin(move |c| {
            let _lease = lease;
            let tx = c.transaction().map_err(unavailable)?;
            let result = (|| -> Result<(), AdminError> {
                matching(&tx, &binding, &binding.bucket)?;
                let record = objects::lookup(&tx, &binding.bucket, name.as_str())?
                    .ok_or(AdminError::Precondition)?.0;
                if record.generation != generation.get() {
                    return Err(AdminError::Precondition);
                }
                tx.execute("INSERT INTO sync_findings(root_id,bucket,name,incarnation,generation,reason) VALUES(?1,?2,?3,?4,?5,?6)
                    ON CONFLICT(root_id,bucket,name) DO UPDATE SET incarnation=excluded.incarnation,generation=excluded.generation,reason=excluded.reason
                    WHERE incarnation!=excluded.incarnation OR generation!=excluded.generation OR reason!=excluded.reason",
                    params![root,binding.bucket,name.as_str(),binding.incarnation,generation.get(),reason_text(reason)]).map_err(unavailable)?;
                Ok(())
            })();
            if result.is_ok() { tx.commit().map_err(unavailable)?; }
            Ok(result)
        }).await??;
        Ok(())
    }

    /// Continue after the last ordinal; pages are bounded observations, not frozen snapshots.
    pub async fn findings_page(&self, after: i64, limit: usize) -> Result<Vec<Finding>, RootError> {
        if after < 0 || !(1..=super::MAX_PAGE).contains(&limit) {
            return Err(AdminError::Invalid("invalid finding page".into()).into());
        }
        self.file_job(|_| Ok(())).await?;
        let lease = self.clone();
        let status = self.status();
        self.catalog().run_admin(move |c| {
            let _lease = lease;
            c.prepare_cached("SELECT ordinal,bucket,name,incarnation,generation,reason FROM sync_findings INDEXED BY sync_findings_page WHERE root_id=?1 AND ordinal>?2 ORDER BY ordinal LIMIT ?3")
                .map_err(unavailable)?.query_map(params![status.root_id.as_bytes(),after,limit as i64], |r| {
                    Ok(Finding {catalog_id:status.catalog_id,root_id:status.root_id,ordinal:r.get(0)?,bucket:r.get(1)?,name:r.get(2)?,incarnation:r.get(3)?,generation:Generation::new(r.get(4)?).map_err(|_|rusqlite::Error::InvalidQuery)?,reason:read_reason(&r.get::<_,String>(5)?)?})
                }).map_err(unavailable)?.collect::<rusqlite::Result<Vec<_>>>().map_err(unavailable)
        }).await.map_err(RootError::from)
    }

    /// Removal remains possible after deletion, but cannot erase a newer incarnation or observation.
    #[cfg(all(test, unix))]
    pub(crate) async fn remove_finding(&self, finding: Finding) -> Result<bool, RootError> {
        if finding.catalog_id != self.status().catalog_id
            || finding.root_id != self.status().root_id
        {
            return Err(AdminError::Precondition.into());
        }
        self.file_job(|_| Ok(())).await?;
        let lease = self.clone();
        self.catalog().run_admin(move |c| {
            let _lease = lease;
            c.execute("DELETE FROM sync_findings WHERE root_id=?1 AND ordinal=?2 AND bucket=?3 AND name=?4 AND incarnation=?5 AND generation=?6 AND reason=?7",
                params![finding.root_id.as_bytes(),finding.ordinal,finding.bucket,finding.name,finding.incarnation,finding.generation.get(),reason_text(finding.reason)])
                .map(|count|count == 1).map_err(unavailable)
        }).await.map_err(RootError::from)
    }
}

#[cfg(all(test, unix))]
mod tests;
