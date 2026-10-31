//! An accepted, verified copy is the only authority to release a protected workspace.

use super::*;
use crate::sync::mapping::WORKSPACE;

type StoredWorkspace = (
    String,
    String,
    String,
    String,
    i64,
    [u8; 8],
    [u8; 8],
    Option<Vec<u8>>,
);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedProof {
    id: String,
    revision: String,
    source: Observation,
    retained: Observation,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    transfer: Option<OwnedProof>,
    captured: Option<OwnedProof>,
    durable: Option<SyncFingerprint>,
}

fn snapshot(bytes: &[u8]) -> Result<Option<Snapshot>, Error> {
    if let Ok(snapshot) = serde_json::from_slice::<Snapshot>(bytes) {
        return Ok(Some(snapshot));
    }
    // Direct decoding preserves i128 timestamps; serde's untagged buffer cannot represent them.
    serde_json::from_slice::<SyncFingerprint>(bytes)
        .map(|_| None)
        .map_err(|error| AdminError::backend(error).into())
}

#[derive(Clone)]
struct Transfer {
    id: String,
    owner: String,
    request: CaptureRequest,
    accepted: Option<OwnedProof>,
    device: [u8; 8],
    inode: [u8; 8],
}

fn directory(root: &File, selected: &Transfer) -> Result<File, Error> {
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

fn validate(
    connection: &Connection,
    root: [u8; 16],
    item: &Transfer,
    proof: &OwnedProof,
) -> Result<(), Error> {
    let valid: bool = connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_conflicts WHERE root_id=?1 AND id=?2 AND revision=?3 AND state=1 AND source_op=?4 AND owner=?5 AND bucket=?6 AND incarnation=?7 AND name=?8 AND generation=?9 AND source=?10 AND retained=?11)")?
        .query_row(params![root,proof.id,proof.revision,item.id,item.owner,item.request.binding.bucket,item.request.binding.incarnation,item.request.name.as_str(),item.request.expected_generation.map(Generation::get),proof.source.encode()?,proof.retained.encode()?], |row|row.get(0))?;
    if !valid {
        return Err(Error::TargetChanged);
    }
    Ok(())
}

fn verify_copy(data: &std::path::Path, proof: &OwnedProof) -> Result<(), Error> {
    let store = capture::open_store(data)?;
    let file = fsutil::open_regular_at(&store, proof.id.as_ref())?;
    if !file.metadata()?.permissions().readonly()
        || !proof.retained.matches(fsutil::stat_identity(&file)?)
        || observe(&file, HashMode::Body)? != proof.retained
        || proof.source.size != proof.retained.size
        || proof.source.hashes.is_none()
        || proof.source.hashes != proof.retained.hashes
    {
        return Err(Error::TargetChanged);
    }
    Ok(())
}

impl CapturedConflict {
    pub(in crate::storage) fn durable_snapshot(
        &self,
        connection: &Connection,
    ) -> Result<Vec<u8>, Error> {
        let proof = owned_proof(connection, &self.id)?;
        if proof.revision != self.revision {
            return Err(Error::TargetChanged);
        }
        serde_json::to_vec(&Snapshot {
            transfer: None,
            captured: Some(proof),
            durable: None,
        })
        .map_err(|error| AdminError::backend(error).into())
    }

    pub(in crate::storage) async fn verify_owned(&self, lease: &RootLease) -> Result<(), Error> {
        let id = self.id.clone();
        let proof = lease
            .conflict_db(move |connection| owned_proof(connection, &id))
            .await?;
        if proof.revision != self.revision {
            return Err(Error::TargetChanged);
        }
        let data = data_path(lease);
        lease
            .conflict_files(move |_| verify_copy(&data, &proof))
            .await
    }

    pub(in crate::storage) fn durable_fingerprint(
        &self,
        connection: &Connection,
        fingerprint: Option<SyncFingerprint>,
    ) -> Result<Vec<u8>, Error> {
        let proof = owned_proof(connection, &self.id)?;
        serde_json::to_vec(&Snapshot {
            transfer: None,
            captured: Some(proof),
            durable: fingerprint,
        })
        .map_err(|error| AdminError::backend(error).into())
    }
}

fn owned_proof(connection: &Connection, id: &str) -> Result<OwnedProof, Error> {
    let (revision, source, retained): (String, Vec<u8>, Vec<u8>) = connection
        .prepare_cached(
            "SELECT revision,source,retained FROM sync_conflicts WHERE id=?1 AND state=1",
        )?
        .query_row([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    Ok(OwnedProof {
        id: id.to_owned(),
        revision,
        source: Observation::decode(&source)?,
        retained: Observation::decode(&retained)?,
    })
}

pub(in crate::storage) fn captured_entries(
    connection: &Connection,
    root: [u8; 16],
) -> Result<u64, Error> {
    let mut statement = connection.prepare_cached("SELECT snapshot,owner,bucket,incarnation,name FROM sync_writeback_ops WHERE root_id=?1 AND phase IN(0,1,3) AND snapshot IS NOT NULL")?;
    let mut rows = statement.query([root])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let snapshot: Vec<u8> = row.get(0)?;
        let Some(Snapshot {
            captured: Some(proof),
            transfer: None,
            ..
        }) = self::snapshot(&snapshot)?
        else {
            continue;
        };
        let valid:bool = connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_conflicts WHERE root_id=?1 AND id=?2 AND revision=?3 AND state=1 AND owner=?4 AND bucket=?5 AND incarnation=?6 AND name=?7 AND source=?8 AND retained=?9)")?
            .query_row(params![root,proof.id,proof.revision,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,proof.source.encode()?,proof.retained.encode()?], |row|row.get(0))?;
        count += u64::from(valid);
    }
    Ok(count)
}

impl RootLease {
    pub(in crate::storage::catalog::sync_state) async fn accepted_transfer(
        &self,
        id: &str,
    ) -> Result<bool, Error> {
        let item = self.transfer_context(id.to_owned()).await?;
        let selected = item.clone();
        let absent = self
            .conflict_files(move |root| {
                let workspace = directory(root, &selected)?;
                match fsutil::open_regular_at(&workspace, selected.id.as_ref()) {
                    Ok(_) => Ok(false),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
                    Err(error) => Err(error.into()),
                }
            })
            .await?;
        if !absent {
            return Ok(false);
        }
        let Some(proof) = item.accepted.clone() else {
            return Ok(false);
        };
        let root = self.status().root_id.as_bytes();
        let copy = proof.clone();
        self.conflict_db(move |connection| validate(connection, root, &item, &copy))
            .await?;
        let data = data_path(self);
        self.conflict_files(move |_| verify_copy(&data, &proof))
            .await?;
        Ok(true)
    }

    pub(super) async fn reuse_workspace_capture(
        &self,
        request: &CaptureRequest,
        source: &File,
    ) -> Result<Option<CapturedConflict>, Error> {
        let root = self.status().root_id.as_bytes();
        let op = request.source_op.clone().ok_or(Error::InvalidResolution)?;
        let row = self.conflict_db(move |connection| {
            connection.prepare_cached("SELECT id,state,source FROM sync_conflicts WHERE root_id=?1 AND source_op=?2 AND state!=2")?
                .query_row(params![root,op], |row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,Vec<u8>>(2)?))).optional().map_err(Error::from)
        }).await?;
        let Some((id, state, original)) = row else {
            return Ok(None);
        };
        let original = Observation::decode(&original)?;
        let file = source.try_clone()?;
        let observed = self
            .conflict_files(move |_| {
                if fsutil::stat_identity(&file)?.size > MAX_BYTES {
                    return Err(Error::Capacity);
                }
                observe(&file, HashMode::Body)
            })
            .await?;
        if !original.matches(FileIdentity {
            device: observed.device,
            inode: observed.inode,
            size: observed.size,
            modified_ns: observed.modified_ns,
            changed_ns: observed.changed_ns,
        }) || original
            .hashes
            .as_ref()
            .is_some_and(|hashes| Some(hashes) != observed.hashes.as_ref())
        {
            // A new actual body must not replace or erase the older retained copy.
            self.conflict_db(move |connection| {
                connection
                    .prepare_cached(
                        "UPDATE sync_conflicts SET source_op=NULL WHERE root_id=?1 AND id=?2",
                    )?
                    .execute(params![root, id])?;
                Ok(())
            })
            .await?;
            return Ok(None);
        }
        if state == 0
            && !self
                .resume_workspace_copy(id.clone(), observed.clone())
                .await?
        {
            self.cleanup_capture(id).await?;
            return Ok(None);
        }
        let proof = self
            .conflict_db(move |connection| owned_proof(connection, &id))
            .await?;
        let context = self
            .transfer_context(request.source_op.clone().ok_or(Error::InvalidResolution)?)
            .await?;
        let copy = proof.clone();
        self.conflict_db(move |connection| validate(connection, root, &context, &copy))
            .await?;
        let data = data_path(self);
        let copy = proof.clone();
        self.conflict_files(move |_| verify_copy(&data, &copy))
            .await?;
        let hashes = observed.hashes.ok_or(Error::TargetChanged)?;
        Ok(Some(CapturedConflict {
            root,
            id: proof.id,
            revision: proof.revision,
            binding: request.binding.clone(),
            name: request.name.clone(),
            expected_generation: request.expected_generation,
            fingerprint: SyncFingerprint {
                size: observed.size,
                mtime_ns: observed.modified_ns,
                ctime_ns: observed.changed_ns,
                inode: observed.inode,
                md5: hashes.md5,
                crc32c: hashes.crc32c,
                racy: true,
            },
            device: observed.device,
        }))
    }

    async fn resume_workspace_copy(
        &self,
        id: String,
        observed: Observation,
    ) -> Result<bool, Error> {
        let data = data_path(self);
        let copy_id = id.clone();
        let expected = observed.clone();
        let retained = self
            .conflict_files(move |_| {
                let store = match capture::open_store(&data) {
                    Ok(store) => store,
                    Err(Error::Admin(AdminError::Io(io::ErrorKind::NotFound))) => {
                        return Ok(None);
                    }
                    Err(error) => return Err(error),
                };
                let file = match fsutil::open_regular_at(&store, copy_id.as_ref()) {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error.into()),
                };
                if !file.metadata()?.permissions().readonly()
                    || file.metadata()?.len() != expected.size
                {
                    return Ok(None);
                }
                let retained = observe(&file, HashMode::Body)?;
                if retained.hashes != expected.hashes {
                    return Ok(None);
                }
                #[cfg(test)]
                fault::hit(&data, fault::Step::ConflictFileSync)?;
                file.sync_all()?;
                #[cfg(test)]
                super::super::super::fault::file_synced(&data, &file.metadata()?);
                #[cfg(test)]
                fault::hit(&data, fault::Step::ConflictDirectorySync)?;
                store.sync_all()?;
                #[cfg(all(test, unix))]
                super::super::super::fault::dir_synced(&data.join("sync-conflicts"));
                Ok(Some(retained))
            })
            .await?;
        let Some(retained) = retained else {
            return Ok(false);
        };
        let selected = id.clone();
        let source_blob = observed.encode()?;
        let retained_blob = retained.encode()?;
        self.conflict_db(move |connection| {
                #[cfg(test)]
                fault::hit(&super::super::super::fault::database_dir(connection),fault::Step::ConflictRegister)?;
                connection.prepare_cached("UPDATE sync_conflicts SET state=1,source=?2,retained=?3 WHERE id=?1 AND state=0")?.execute(params![selected,source_blob,retained_blob])?;
                Ok(())
            }).await?;
        #[cfg(test)]
        self.conflict_boundary(fault::Step::ConflictRegistered)
            .await?;
        Ok(true)
    }

    /// The caller holds the shared execution permit through this entire owned transition.
    pub(in crate::storage::catalog::sync_state) async fn transfer_protected(
        &self,
        id: &str,
    ) -> Result<(), Error> {
        let selected = self.transfer_context(id.to_owned()).await?;
        let probe = self.transfer_context(id.to_owned()).await?;
        let source = self
            .conflict_files(move |root| {
                let workspace = directory(root, &probe)?;
                match fsutil::open_regular_at(&workspace, probe.id.as_ref()) {
                    Ok(file) => Ok(Some(file)),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error.into()),
                }
            })
            .await?;
        let proof = if let Some(source) = source {
            self.transfer_capture(&selected, source).await?
        } else {
            selected.accepted.clone().ok_or(Error::TargetChanged)?
        };
        self.finish_transfer(selected, proof).await
    }

    pub(in crate::storage::catalog::sync_state) async fn refresh_transfer_charge(
        &self,
        id: &str,
    ) -> Result<(), Error> {
        let selected = self.transfer_context(id.to_owned()).await?;
        let id = selected.id.clone();
        let size = self
            .conflict_files(move |root| {
                let workspace = directory(root, &selected)?;
                match fsutil::open_regular_at(&workspace, selected.id.as_ref()) {
                    Ok(file) => Ok(Some(fsutil::stat_identity(&file)?.size)),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error.into()),
                }
            })
            .await?;
        if let Some(size) = size {
            let root = self.status().root_id.as_bytes();
            self.conflict_db(move |connection| {
                let tx = connection.transaction()?;
                recovery::account_workspace(&tx, root, &id, size)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        }
        Ok(())
    }

    async fn transfer_context(&self, id: String) -> Result<Transfer, Error> {
        let root = self.status().root_id.as_bytes();
        let catalog = self.catalog().sync_state().catalog_id().await?;
        self.conflict_db(move |connection| {
            let (owner,bucket,incarnation,name,generation,device,inode,snapshot): StoredWorkspace = connection.prepare_cached("SELECT w.owner,w.bucket,w.incarnation,w.name,w.generation,r.workspace_device,r.workspace_inode,w.snapshot FROM sync_writeback_ops w JOIN sync_writeback_roots r ON r.root_id=w.root_id WHERE w.root_id=?1 AND w.id=?2 AND w.phase=2")?
                .query_row(params![root,id], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)))?;
            let accepted = snapshot.as_deref().map(self::snapshot).transpose()?.flatten().and_then(|snapshot|snapshot.transfer);
            Ok(Transfer { id:id.clone(),owner,request:CaptureRequest { binding:SyncBinding {catalog,bucket,incarnation,deletion:None,control:None},name:ObjectName::new(name)?,expected_generation:Some(Generation::new(generation)?),source_op:Some(id)},accepted,device,inode })
        }).await
    }

    async fn transfer_capture(&self, item: &Transfer, source: File) -> Result<OwnedProof, Error> {
        let selected = source.try_clone()?;
        let before = self
            .conflict_files(move |_| observe(&selected, HashMode::Metadata))
            .await?;
        let root = self.status().root_id.as_bytes();
        let id = item.id.clone();
        let size = before.size;
        self.conflict_db(move |connection| {
            let tx = connection.transaction()?;
            recovery::account_workspace(&tx, root, &id, size)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        if before.size > MAX_BYTES {
            return Err(Error::Capacity);
        }
        let captured = self.capture_inner(item.request.clone(), source).await?;
        let id = captured.id.clone();
        self.conflict_db(move |connection| owned_proof(connection, &id))
            .await
    }

    async fn finish_transfer(&self, item: Transfer, proof: OwnedProof) -> Result<(), Error> {
        let data = data_path(self);
        let copy = proof.clone();
        self.conflict_files(move |_| verify_copy(&data, &copy))
            .await?;
        let root = self.status().root_id.as_bytes();
        let proof_blob = serde_json::to_vec(&Snapshot {
            transfer: Some(proof.clone()),
            captured: None,
            durable: None,
        })
        .map_err(AdminError::backend)?;
        let selected = self.transfer_context(item.id.clone()).await?;
        let copy = proof.clone();
        let encoded = proof_blob.clone();
        self.conflict_db(move |connection| {
            let tx = connection.transaction()?;
            validate(&tx,root,&selected,&copy)?;
            let count = tx.prepare_cached("UPDATE sync_writeback_ops SET snapshot=?3 WHERE root_id=?1 AND id=?2 AND phase=2")?.execute(params![root,selected.id,encoded])?;
            if count != 1 { return Err(Error::TargetChanged); }
            tx.commit()?;
            Ok(())
        }).await?;
        #[cfg(test)]
        self.transfer_boundary(super::super::super::fault::Step::TransferAccepted)
            .await?;
        let selected = self.transfer_context(item.id.clone()).await?;
        let data = data_path(self);
        #[cfg(test)]
        let parent = self.fault_tree().join(WORKSPACE);
        #[cfg(not(test))]
        let parent = data.join(WORKSPACE);
        let expected = proof.source.clone();
        self.conflict_files(move |root| {
            let workspace = directory(root, &selected)?;
            resolve::checked_unlink(&workspace, &selected.id, Some(&expected), &data, &parent)?;
            root.sync_all()?;
            Ok(())
        })
        .await?;
        #[cfg(test)]
        self.transfer_boundary(super::super::super::fault::Step::TransferComplete)
            .await?;
        self.conflict_db(move |connection| {
            let tx = connection.transaction()?;
            validate(&tx,root,&item,&proof)?;
            let current:Option<Vec<u8>> = tx.prepare_cached("SELECT snapshot FROM sync_writeback_ops WHERE root_id=?1 AND id=?2 AND phase=2")?.query_row(params![root,item.id],|row|row.get(0))?;
            if current.as_ref() != Some(&proof_blob) { return Err(Error::TargetChanged); }
            recovery::release_workspace(&tx,root,&item.id)?;
            changed(&tx,root)?;
            tx.commit()?;
            Ok(())
        }).await?;
        #[cfg(test)]
        self.transfer_boundary(super::super::super::fault::Step::TransferCompleted)
            .await?;
        Ok(())
    }

    #[cfg(test)]
    async fn transfer_boundary(&self, step: super::super::super::fault::Step) -> Result<(), Error> {
        let tree = self.fault_tree();
        let data = data_path(self);
        self.conflict_files(move |_| {
            super::super::super::fault::hit(&tree, step).map_err(objects::object_unavailable)?;
            super::super::super::fault::hit(&data, step).map_err(objects::object_unavailable)?;
            Ok(())
        })
        .await
    }
}
