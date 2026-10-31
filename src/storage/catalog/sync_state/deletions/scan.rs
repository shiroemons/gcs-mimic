use super::*;

#[derive(Clone)]
pub(crate) struct Inventory {
    pub revision: String,
    root: [u8; 16],
    fence: JournalFence,
}

impl RootLease {
    pub(crate) async fn begin_deletion_inventory(
        &self,
        guard: Guard,
    ) -> Result<Inventory, RootError> {
        self.file_job(|_| Ok(())).await?;
        let root = self.status().root_id.as_bytes();
        let scope = self.control_scope();
        Ok(self
            .catalog()
            .run_admin(move |connection| {
                let transaction = connection.transaction().map_err(unavailable)?;
                scope.authorize(&transaction)?;
                let complete: bool = transaction
                    .prepare_cached("SELECT complete FROM sync_roots WHERE root_id=?1")
                    .and_then(|mut statement| statement.query_row([root], |row| row.get(0)))
                    .map_err(unavailable)?;
                if !complete {
                    return Err(CatalogError::PreconditionFailed);
                }
                let current = fence(&transaction)?;
                let revision = new_revision()?;
                let scan_revision = new_revision()?;
                transaction
                    .prepare_cached("DELETE FROM sync_deletion_sets WHERE root_id=?1")
                    .and_then(|mut statement| statement.execute([root]))
                    .map_err(unavailable)?;
                transaction
                    .prepare_cached(
                        "INSERT INTO sync_deletion_sets VALUES(?1,?2,?3,?4,?5,?4,0,0,?6,0,0,0,0)",
                    )
                    .and_then(|mut statement| {
                        statement.execute(params![
                            root,
                            revision,
                            scan_revision,
                            current.sequence.0 as i64,
                            current.revision,
                            matches!(guard, Guard::Enabled)
                        ])
                    })
                    .map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
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
            let tx=connection.transaction().map_err(unavailable)?;scope.authorize(&tx)?;
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
            let transaction=connection.transaction().map_err(unavailable)?;
            scope.authorize(&transaction)?;
            if fence(&transaction)?!=inventory.fence {return Err(CatalogError::PreconditionFailed)}
            let ordinal:i64=transaction.prepare_cached("SELECT candidate_count FROM sync_deletion_sets WHERE root_id=?1 AND revision=?2 AND complete=0")
                .and_then(|mut statement|statement.query_row(params![inventory.root,inventory.revision],|row|row.get(0))).map_err(unavailable)?;
            for (index,candidate) in candidates.iter().enumerate() {
                transaction.prepare_cached("INSERT INTO sync_deletion_members VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0)")
                    .and_then(|mut statement|statement.execute(params![inventory.root,ordinal+index as i64+1,candidate.bucket,candidate.name,candidate.owner_project,candidate.bucket_incarnation,candidate.generation,candidate.path])).map_err(unavailable)?;
            }
            transaction.prepare_cached("UPDATE sync_deletion_sets SET baseline_count=baseline_count+?3,candidate_count=candidate_count+?4 WHERE root_id=?1 AND revision=?2 AND complete=0")
                .and_then(|mut statement|statement.execute(params![inventory.root,inventory.revision,baseline as i64,candidates.len() as i64])).map_err(unavailable)?;
            transaction.commit().map_err(unavailable)?;
            Ok(())
        }).await?)
    }

    pub(crate) async fn finish_deletion_inventory(
        &self,
        inventory: Inventory,
    ) -> Result<DeletionSet, RootError> {
        self.file_job(|_| Ok(())).await?;
        let scope = self.control_scope();
        Ok(self.catalog().run_admin(move |connection| {
            let transaction=connection.transaction().map_err(unavailable)?;
            scope.authorize(&transaction)?;
            if fence(&transaction)?!=inventory.fence {return Err(CatalogError::PreconditionFailed)}
            let (baseline,candidates,enabled):(i64,i64,bool)=transaction.prepare_cached("SELECT baseline_count,candidate_count,guard_enabled FROM sync_deletion_sets WHERE root_id=?1 AND revision=?2 AND complete=0")
                .and_then(|mut statement|statement.query_row(params![inventory.root,inventory.revision],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))).map_err(unavailable)?;
            let guard=guarded(Guard::from(enabled),baseline as u64,candidates as u64);
            transaction.prepare_cached("UPDATE sync_deletion_sets SET complete=1,valid=1,guarded=?3,authorization=?4 WHERE root_id=?1 AND revision=?2")
                .and_then(|mut statement|statement.execute(params![inventory.root,inventory.revision,guard,i64::from(!guard)])).map_err(unavailable)?;
            let snapshot=read_set(&transaction,inventory.root)?.ok_or(CatalogError::PreconditionFailed)?;
            transaction.commit().map_err(unavailable)?;
            Ok(snapshot)
        }).await?)
    }
}
