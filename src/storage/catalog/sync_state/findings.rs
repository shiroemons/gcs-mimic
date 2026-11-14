//! Durable observations for names the working tree cannot represent.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::super::{objects, unavailable};
use super::{
    CatalogId, SyncBinding, matching,
    root::{RootError, RootId, RootLease},
};
use crate::storage::catalog::Incarnation;
use crate::storage::catalog::sql;
use crate::{
    bucket::CatalogError,
    storage::admin::{AdminError, Generation, ObjectName},
    sync::mapping::Reason,
};
use rusqlite::params;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub catalog_id: CatalogId,
    pub root_id: RootId,
    pub ordinal: i64,
    pub bucket: String,
    pub name: String,
    pub(crate) incarnation: Incarnation,
    pub generation: Generation,
    pub reason: Reason,
}

sql::text_enum!(pub(crate) Reason, "invalid synchronization finding", {
    InvalidName => "invalid-name", HostRestriction => "host-restriction",
    Reserved => "reserved", Ignored => "ignored", Collision => "collision",
    UnsafeEntry => "unsafe-entry",
});

fn remember_owner(
    transaction: &rusqlite::Transaction<'_>,
    root: RootId,
    bucket: &str,
    name: &str,
) -> Result<(), CatalogError> {
    // One logical finding may survive several bucket incarnations; replacement cannot erase earlier authority.
    sql::exec(
        transaction,
        "INSERT INTO sync_finding_owners(ordinal,incarnation,owner_project) SELECT ordinal,incarnation,owner_project FROM sync_findings WHERE root_id=?1 AND bucket=?2 AND name=?3 ON CONFLICT(ordinal,incarnation) DO UPDATE SET owner_project=excluded.owner_project",
        params![root, bucket, name],
    )?;
    Ok(())
}

impl RootLease {
    /// Conflicts may refer to a deleted API object whose synchronized body is still tracked.
    pub(crate) async fn record_export_conflict(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        generation: Generation,
    ) -> Result<(), RootError> {
        self.revalidate().await?;
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog().run_admin(move |c| {
            let tx = c.transaction()?;
            lease.control_scope().authorize(&tx)?;
            matching(&tx,&binding,&binding.bucket).map_err(unavailable)?;
            let live = objects::lookup(&tx,&binding.bucket,name.as_str()).map_err(unavailable)?.is_some_and(|stored| stored.record.generation == generation.get());
            let tracked: bool = sql::scalar(&tx, "SELECT EXISTS(SELECT 1 FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?4 AND generation=?3)", params![binding.bucket,name.as_str(),generation.get(),binding.incarnation])?;
            let valid = live || tracked;
            if valid {
                sql::exec(&tx, "INSERT INTO sync_findings(root_id,bucket,name,incarnation,generation,reason,owner_project) VALUES(?1,?2,?3,?4,?5,'collision',(SELECT b.project FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?2 AND i.incarnation=?4)) ON CONFLICT(root_id,bucket,name) DO UPDATE SET incarnation=excluded.incarnation,generation=excluded.generation,reason=excluded.reason,owner_project=excluded.owner_project", params![root,binding.bucket,name.as_str(),binding.incarnation,generation.get()])?;
                remember_owner(&tx,root,&binding.bucket,name.as_str())?;
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
        self.revalidate().await?;
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog().run_admin_with_error(move |c| {
            let tx = c.transaction().map_err(unavailable)?;
            lease.control_scope().authorize(&tx)?;
            matching(&tx, &binding, &binding.bucket)?;
            let record = objects::lookup(&tx, &binding.bucket, name.as_str())?
                .ok_or(AdminError::Precondition)?.record;
            if record.generation != generation.get() {
                return Err(AdminError::Precondition);
            }
            sql::exec(&tx, "INSERT INTO sync_findings(root_id,bucket,name,incarnation,generation,reason,owner_project) VALUES(?1,?2,?3,?4,?5,?6,(SELECT b.project FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?2 AND i.incarnation=?4))
                ON CONFLICT(root_id,bucket,name) DO UPDATE SET incarnation=excluded.incarnation,generation=excluded.generation,reason=excluded.reason,owner_project=excluded.owner_project
                WHERE incarnation!=excluded.incarnation OR generation!=excluded.generation OR reason!=excluded.reason OR owner_project IS NOT excluded.owner_project", params![root,binding.bucket,name.as_str(),binding.incarnation,generation.get(),reason]).map_err(unavailable)?;
            remember_owner(&tx,root,&binding.bucket,name.as_str())?;
            tx.commit().map_err(unavailable)?;
            Ok::<_, AdminError>(())
        }).await?;
        Ok(())
    }

    /// Continue after the last ordinal; pages are bounded observations, not frozen snapshots.
    pub async fn findings_page(&self, after: i64, limit: usize) -> Result<Vec<Finding>, RootError> {
        if after < 0 || !(1..=super::MAX_PAGE).contains(&limit) {
            return Err(AdminError::Invalid("invalid finding page".into()).into());
        }
        self.revalidate().await?;
        let lease = self.clone();
        let status = self.status();
        self.catalog().run_admin(move |c| {
            let tx = c.transaction()?;
            lease.control_scope().authorize(&tx)?;
            tx.prepare_cached("SELECT ordinal,bucket,name,incarnation,generation,reason FROM sync_findings INDEXED BY sync_findings_page WHERE root_id=?1 AND ordinal>?2 ORDER BY ordinal LIMIT ?3")?
                .query_map(params![status.root_id,after,limit], |r| {
                    Ok(Finding {catalog_id:status.catalog_id,root_id:status.root_id,ordinal:r.get(0)?,bucket:r.get(1)?,name:r.get(2)?,incarnation:r.get(3)?,generation:Generation::new(r.get(4)?).map_err(|error|sql::conversion_error(4,rusqlite::types::Type::Integer,error))?,reason:r.get(5)?})
                })?.collect::<rusqlite::Result<Vec<_>>>().map_err(unavailable)
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
        self.revalidate().await?;
        let lease = self.clone();
        self.catalog().run_admin(move |c| {
            let _lease = lease;
            sql::exec(c, "DELETE FROM sync_findings WHERE root_id=?1 AND ordinal=?2 AND bucket=?3 AND name=?4 AND incarnation=?5 AND generation=?6 AND reason=?7", params![finding.root_id,finding.ordinal,finding.bucket,finding.name,finding.incarnation,finding.generation.get(),finding.reason])
                .map(|count|count == 1).map_err(unavailable)
        }).await.map_err(RootError::from)
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
