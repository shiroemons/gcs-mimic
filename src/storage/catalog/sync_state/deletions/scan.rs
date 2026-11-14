use super::*;
use crate::storage::catalog::sql;

#[derive(Clone)]
pub(crate) struct Inventory {
    pub revision: String,
    root: RootId,
    fence: JournalFence,
}

impl RootLease {
    pub(crate) async fn begin_deletion_inventory(
        &self,
        guard: Guard,
    ) -> Result<Inventory, RootError> {
        self.revalidate().await?;
        let root = self.status().root_id;
        let scope = self.control_scope();
        Ok(self
            .catalog()
            .run_admin(move |connection| {
                let transaction = connection.transaction()?;
                scope.authorize(&transaction)?;
                let complete: bool = sql::scalar(
                    &transaction,
                    "SELECT complete FROM sync_roots WHERE root_id=?1",
                    [root],
                )?;
                if !complete {
                    return Err(CatalogError::PreconditionFailed);
                }
                let current = fence(&transaction)?;
                let revision = new_revision()?;
                let scan_revision = new_revision()?;
                sql::exec(
                    &transaction,
                    "DELETE FROM sync_deletion_sets WHERE root_id=?1",
                    [root],
                )?;
                sql::exec(
                    &transaction,
                    "INSERT INTO sync_deletion_sets VALUES(?1,?2,?3,?4,?5,?4,0,0,?6,0,0,0,0)",
                    params![
                        root,
                        revision,
                        scan_revision,
                        current.sequence,
                        current.revision,
                        matches!(guard, Guard::Enabled)
                    ],
                )?;
                transaction.commit()?;
                Ok(Inventory {
                    revision,
                    root,
                    fence: current,
                })
            })
            .await?)
    }

    pub(crate) async fn deletion_inventory_page(
        &self,
        inventory: Inventory,
        after: String,
    ) -> Result<Vec<Candidate>, RootError> {
        let scope = self.control_scope();
        Ok(self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;scope.authorize(&tx)?;
            if fence(&tx)?!=inventory.fence {return Err(CatalogError::PreconditionFailed)}
            tx.prepare_cached("SELECT s.bucket,s.name,b.project,s.incarnation,s.generation,s.path,0 FROM sync_objects s JOIN bucket_incarnations i ON i.bucket=s.bucket AND i.incarnation=s.incarnation JOIN buckets b ON b.name=s.bucket JOIN objects o ON o.bucket=s.bucket AND o.name=s.name AND o.generation=s.generation AND o.time_deleted IS NULL WHERE s.path>?1 AND NOT EXISTS(SELECT 1 FROM sync_findings f WHERE f.root_id=?2 AND f.bucket=s.bucket AND f.name=s.name) ORDER BY s.path LIMIT 256")
                .and_then(|mut statement|statement.query_map(params![after,inventory.root],read_candidate)?.collect()).map_err(unavailable)
        }).await?)
    }

    pub(crate) async fn record_deletion_candidates(
        &self,
        inventory: Inventory,
        baseline: u64,
        candidates: Vec<Candidate>,
    ) -> Result<(), RootError> {
        if candidates.len() > 256 || baseline > 256 || baseline < candidates.len() as u64 {
            return Err(AdminError::Invalid("invalid deletion inventory page".into()).into());
        }
        let scope = self.control_scope();
        Ok(self.catalog().run_admin(move |connection| {
            let transaction=connection.transaction()?;
            scope.authorize(&transaction)?;
            if fence(&transaction)?!=inventory.fence {return Err(CatalogError::PreconditionFailed)}
            let ordinal:u64=sql::scalar(&transaction, "SELECT candidate_count FROM sync_deletion_sets WHERE root_id=?1 AND revision=?2 AND complete=0", params![inventory.root,inventory.revision])?;
            for (index,candidate) in candidates.iter().enumerate() {
                sql::exec(&transaction, "INSERT INTO sync_deletion_members VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0)", params![inventory.root,ordinal+u64::try_from(index).map_err(unavailable)?+1,candidate.bucket,candidate.name,candidate.owner_project,candidate.bucket_incarnation,candidate.generation,candidate.path])?;
            }
            sql::exec(&transaction, "UPDATE sync_deletion_sets SET baseline_count=baseline_count+?3,candidate_count=candidate_count+?4 WHERE root_id=?1 AND revision=?2 AND complete=0", params![inventory.root,inventory.revision,baseline,candidates.len()])?;
            transaction.commit()?;
            Ok(())
        }).await?)
    }

    pub(crate) async fn finish_deletion_inventory(
        &self,
        inventory: Inventory,
    ) -> Result<DeletionSet, RootError> {
        self.revalidate().await?;
        let scope = self.control_scope();
        Ok(self.catalog().run_admin(move |connection| {
            let transaction=connection.transaction()?;
            scope.authorize(&transaction)?;
            if fence(&transaction)?!=inventory.fence {return Err(CatalogError::PreconditionFailed)}
            let (baseline,candidates,enabled):(u64,u64,bool)=sql::row(&transaction, "SELECT baseline_count,candidate_count,guard_enabled FROM sync_deletion_sets WHERE root_id=?1 AND revision=?2 AND complete=0", params![inventory.root,inventory.revision],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
            let automatic=history::automatic_count(&transaction,inventory.root)?;
            let guard=guarded(Guard::from(enabled),baseline+automatic,candidates+automatic);
            sql::exec(&transaction, "UPDATE sync_deletion_sets SET complete=1,valid=1,guarded=?3,authorization=?4 WHERE root_id=?1 AND revision=?2", params![inventory.root,inventory.revision,guard,i64::from(!guard)])?;
            let snapshot=read_set(&transaction,inventory.root)?.ok_or(CatalogError::PreconditionFailed)?;
            transaction.commit()?;
            Ok(snapshot)
        }).await?)
    }
}
