use super::*;
use crate::sync::mapping::WORKSPACE;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Recovery {
    pub recovery_id: String,
    pub recovery_revision: String,
    pub target_identity: String,
    pub owner_project: String,
    pub bucket: String,
    pub bucket_incarnation: String,
    #[serde(rename = "object")]
    pub name: String,
    pub retained_size: u64,
    pub hashes: Option<Hashes>,
    pub phase: &'static str,
    pub retained_location: &'static str,
    pub over_limit: bool,
    pub remedy: &'static str,
    #[serde(skip)]
    pub ordinal: i64,
}

#[derive(Clone)]
struct Workspace {
    id: String,
    owner: String,
    bucket: String,
    incarnation: String,
    name: String,
    ordinal: i64,
    device: [u8; 8],
    inode: [u8; 8],
}

fn directory(root: &File, selected: &Workspace) -> Result<File, Error> {
    let directory =
        fsutil::directory_at(root, WORKSPACE.as_ref(), fsutil::DirectoryOpen::Existing)?;
    let identity = fsutil::stat_identity(&directory)?;
    if identity.device.to_be_bytes() != selected.device
        || identity.inode.to_be_bytes() != selected.inode
    {
        return Err(Error::TargetChanged);
    }
    Ok(directory)
}

fn workspace(connection: &Connection, root: [u8; 16], id: &str) -> Result<Workspace, Error> {
    connection.prepare_cached("SELECT w.id,w.owner,w.bucket,w.incarnation,w.name,w.ordinal,r.workspace_device,r.workspace_inode FROM sync_writeback_ops w JOIN sync_writeback_roots r ON r.root_id=w.root_id WHERE w.root_id=?1 AND w.id=?2 AND w.phase=2")?
        .query_row(params![root,id],|row|Ok(Workspace {id:row.get(0)?,owner:row.get(1)?,bucket:row.get(2)?,incarnation:row.get(3)?,name:row.get(4)?,ordinal:row.get(5)?,device:row.get(6)?,inode:row.get(7)?})).optional()?.ok_or(Error::RevisionMismatch)
}

impl RootLease {
    /// Refresh only a bounded inspected page; a sparse over-limit body is never read to completion.
    pub(crate) async fn recovery_page(
        &self,
        after: i64,
        limit: usize,
        authority: ControlAuthority,
    ) -> Result<Vec<super::Recovery>, Error> {
        if after < 0 || !(1..=128).contains(&limit) {
            return Err(Error::InvalidResolution);
        }
        let lease = self.with_authority(authority.clone());
        self.conflict_serial(async move { lease.recoveries_inner(after, limit, authority).await })
            .await
    }

    async fn recoveries_inner(
        &self,
        after: i64,
        limit: usize,
        authority: ControlAuthority,
    ) -> Result<Vec<Recovery>, Error> {
        let root = self.status().root_id.as_bytes();
        let auth = authority.clone();
        let pending=self.conflict_db(move|connection| {
            let tx=connection.transaction()?;auth.authorize(&tx,root)?;
            let ids=tx.prepare_cached("SELECT id FROM sync_writeback_ops WHERE root_id=?1 AND phase=2 AND ordinal>?2 ORDER BY ordinal LIMIT ?3")?
                .query_map(params![root,after,limit as i64],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            ids.iter().map(|id|workspace(&tx,root,id)).collect::<Result<Vec<_>,Error>>()
        }).await?;
        let mut results = Vec::with_capacity(pending.len());
        let mut page_bytes = 0;
        for selected in pending {
            let item = selected.clone();
            let observation = self
                .conflict_files(move |root| {
                    let workspace = directory(root, &item)?;
                    match fsutil::open_regular_at(&workspace, item.id.as_ref()) {
                        Ok(file) => observe(&file, HashMode::Metadata).map(Some),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                        Err(error) => Err(error.into()),
                    }
                })
                .await?;
            let auth = authority.clone();
            let item=self.conflict_db(move|connection| {
                let tx=connection.transaction()?;auth.authorize(&tx,root)?;workspace(&tx,root,&selected.id)?;
                let stored=tx.prepare_cached("SELECT revision,target_identity,observation,phase FROM sync_recovery_workspaces WHERE root_id=?1 AND id=?2")?
                    .query_row(params![root,selected.id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Vec<u8>>(2)?,row.get::<_,i64>(3)?))).optional()?;
                let accepted:bool=tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_conflict_operations WHERE root_id=?1 AND target_id=?2 AND target_kind=1 AND done=0)")?.query_row(params![root,selected.id],|row|row.get(0))?;
                let (revision,target_identity,observed,phase)=match (observation,stored) {
                    (None,Some((revision,identity,bytes,phase))) if accepted=>(revision,identity,Observation::decode(&bytes)?,phase),
                    (None,_)=>return Err(Error::TargetChanged),
                    (Some(observed),Some((revision,identity,bytes,phase))) if Observation::decode(&bytes)?==observed=>(revision,identity,observed,phase),
                    (Some(observed),_)=>{
                        let revision=opaque_id()?;let identity=opaque_id()?;
                        tx.prepare_cached("UPDATE sync_conflict_operations SET done=1,reclaimed=1,error_code='target_changed' WHERE root_id=?1 AND target_kind=1 AND target_id=?2 AND done=0")?.execute(params![root,selected.id])?;
                        tx.prepare_cached("INSERT INTO sync_recovery_workspaces(id,root_id,revision,target_identity,observation,phase) VALUES(?1,?2,?3,?4,?5,0) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,target_identity=excluded.target_identity,observation=excluded.observation,phase=0")?
                            .execute(params![selected.id,root,revision,identity,observed.encode()?])?;
                        (revision,identity,observed,0)
                    }
                };
                account_workspace(&tx,root,&selected.id,observed.size)?;
                let usage=list::total_usage(&tx,root)?;
                tx.commit()?;
                Ok(Recovery {recovery_id:selected.id,recovery_revision:revision,target_identity,owner_project:selected.owner,bucket:selected.bucket,bucket_incarnation:selected.incarnation,name:selected.name,
                    retained_size:observed.size,hashes:observed.hashes.map(|h|Hashes{md5:h.md5,crc32c:h.crc32c}),phase:if phase==0 {"protected"} else {"discard_accepted"},retained_location:"workspace",over_limit:usage.over_limit,
                    remedy:"Stop external writers and the server; copy the protected workspace entry outside both roots and verify it. Resume, inspect again, then confirm backup_verified or discard_without_backup for this exact identity.",ordinal:selected.ordinal})
            }).await?;
            let bytes = serde_json::to_vec(&item)
                .map_err(AdminError::backend)?
                .len();
            if page_bytes + bytes > 240_000 {
                break;
            }
            page_bytes += bytes;
            results.push(item);
        }
        Ok(results)
    }

    pub(super) async fn resolve_recovery(
        &self,
        request: ResolveRequest,
        authority: ControlAuthority,
    ) -> Result<ResolveOutcome, Error> {
        let ResolveTarget::Recovery {
            recovery_id,
            revision,
            target_identity,
            confirmation,
        } = &request.target
        else {
            return Err(Error::InvalidResolution);
        };
        if request.action != Action::Discard
            || !valid_id(recovery_id)
            || !valid_id(revision)
            || !valid_id(target_identity)
        {
            return Err(Error::InvalidResolution);
        }
        if confirmation.is_none() {
            return Err(Error::ConfirmationRequired);
        }
        let root = self.status().root_id.as_bytes();
        let req = request.clone();
        let auth = authority.clone();
        let id = recovery_id.clone();
        let rev = revision.clone();
        let target = target_identity.clone();
        let admitted=self.conflict_db(move|connection| {
            let tx=connection.transaction()?;auth.authorize(&tx,root)?;
            let prior=resolve::recorded_operation(&tx,root,&req,&auth)?;
            if prior.is_some_and(|(done,reclaimed,_)|done && reclaimed) {return Ok(None)}
            let selected=workspace(&tx,root,&id)?;
            let row=tx.prepare_cached("SELECT revision,target_identity,observation FROM sync_recovery_workspaces WHERE root_id=?1 AND id=?2")?
                .query_row(params![root,id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Vec<u8>>(2)?))).optional()?.ok_or(Error::RevisionMismatch)?;
            if row.1!=target {return Err(Error::TargetChanged)}
            if prior.is_none() {
                if row.0!=rev {return Err(Error::RevisionMismatch)}
                resolve::accept_recovery(&tx,root,&req,&id,&selected.owner)?;
                tx.prepare_cached("UPDATE sync_recovery_workspaces SET phase=1 WHERE root_id=?1 AND id=?2")?.execute(params![root,id])?;
            }
            let observation=Observation::decode(&row.2)?;
            tx.commit()?;Ok(Some((selected,observation)))
        }).await?;
        let Some((selected, observation)) = admitted else {
            return Ok(ResolveOutcome {
                operation_id: request.operation_id,
                completed: true,
                result_generation: None,
            });
        };
        #[cfg(test)]
        self.conflict_boundary(fault::Step::ConflictAccepted)
            .await?;
        let req = request.clone();
        let auth = authority.clone();
        self.conflict_db(move |connection| {
            let tx = connection.transaction()?;
            auth.authorize(&tx, root)?;
            resolve::recorded_operation(&tx, root, &req, &auth)?.ok_or(Error::OperationIdReused)?;
            Ok(())
        })
        .await?;
        let data = data_path(self);
        let entry = selected.clone();
        #[cfg(test)]
        let parent = self.fault_tree().join(WORKSPACE);
        #[cfg(not(test))]
        let parent = data.join(WORKSPACE);
        self.conflict_files(move |root| {
            let workspace = directory(root, &entry)?;
            resolve::checked_unlink(&workspace, &entry.id, Some(&observation), &data, &parent)
        })
        .await?;
        #[cfg(test)]
        self.conflict_boundary(fault::Step::ConflictComplete)
            .await?;
        let req = request.clone();
        let auth = authority.clone();
        self.conflict_db(move|connection| {
            let tx=connection.transaction()?;auth.authorize(&tx,root)?;
            resolve::recorded_operation(&tx,root,&req,&auth)?.ok_or(Error::OperationIdReused)?;
            release_workspace(&tx,root,&selected.id)?;
            tx.prepare_cached("UPDATE sync_recovery_workspaces SET phase=2 WHERE root_id=?1 AND id=?2")?.execute(params![root,selected.id])?;
            tx.prepare_cached("UPDATE sync_conflict_operations SET done=1,reclaimed=1 WHERE root_id=?1 AND operation_id=?2")?.execute(params![root,req.operation_id])?;
            resolve::settle_other_operations(&tx,root,&req,&selected.id,1)?;
            changed(&tx,root)?;tx.commit()?;Ok(())
        }).await?;
        #[cfg(test)]
        self.conflict_boundary(fault::Step::ConflictCompleted)
            .await?;
        Ok(ResolveOutcome {
            operation_id: request.operation_id,
            completed: true,
            result_generation: None,
        })
    }
}

pub(super) fn account_workspace(
    connection: &Connection,
    root: [u8; 16],
    id: &str,
    size: u64,
) -> Result<(), Error> {
    let old: Option<[u8; 8]> = connection
        .prepare_cached(
            "SELECT retained FROM sync_writeback_ops WHERE root_id=?1 AND id=?2 AND phase=2",
        )?
        .query_row(params![root, id], |row| row.get(0))?;
    if old.map(u64::from_be_bytes) == Some(size) {
        return Ok(());
    }
    let retained: [u8; 16] = connection
        .prepare_cached("SELECT retained FROM sync_writeback_roots WHERE root_id=?1")?
        .query_row([root], |row| row.get(0))?;
    let total = u128::from_be_bytes(retained)
        .checked_sub(u128::from(
            old.map(u64::from_be_bytes).ok_or(Error::TargetChanged)?,
        ))
        .ok_or(Error::TargetChanged)?
        + u128::from(size);
    connection
        .prepare_cached("UPDATE sync_writeback_roots SET retained=?2,recovery=1 WHERE root_id=?1")?
        .execute(params![root, total.to_be_bytes()])?;
    connection.prepare_cached("UPDATE sync_writeback_ops SET retained=?3,revision=revision+1 WHERE root_id=?1 AND id=?2")?.execute(params![root,id,size.to_be_bytes()])?;
    Ok(())
}

pub(super) fn release_workspace(
    connection: &Connection,
    root: [u8; 16],
    id: &str,
) -> Result<(), Error> {
    let retained: Option<[u8; 8]> = connection
        .prepare_cached(
            "SELECT retained FROM sync_writeback_ops WHERE root_id=?1 AND id=?2 AND phase=2",
        )?
        .query_row(params![root, id], |row| row.get(0))?;
    let total: [u8; 16] = connection
        .prepare_cached("SELECT retained FROM sync_writeback_roots WHERE root_id=?1")?
        .query_row([root], |row| row.get(0))?;
    let remaining = u128::from_be_bytes(total)
        .checked_sub(u128::from(
            retained
                .map(u64::from_be_bytes)
                .ok_or(Error::TargetChanged)?,
        ))
        .ok_or(Error::TargetChanged)?;
    connection.prepare_cached("UPDATE sync_writeback_roots SET retained=?2,entries=entries-1,protected=protected-1,recovery=1 WHERE root_id=?1 AND entries>0 AND protected>0")?.execute(params![root,remaining.to_be_bytes()])?;
    connection.prepare_cached("UPDATE sync_writeback_ops SET phase=4,retained=NULL,revision=revision+1,io_failure=0 WHERE root_id=?1 AND id=?2 AND phase=2")?.execute(params![root,id])?;
    Ok(())
}
