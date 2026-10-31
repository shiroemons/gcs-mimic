use super::super::SyncRecord;
use super::*;

#[cfg(test)]
pub(super) mod faults;
#[cfg(test)]
use crate::storage::catalog::fault::Step;

#[cfg(test)]
static FAILURE: std::sync::Mutex<Vec<(RootId, &'static str, usize)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn inject_failure(root: RootId, phase: &'static str, count: usize) {
    FAILURE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((root, phase, count));
}

#[cfg(test)]
fn failure(root: RootId, phase: &str) -> Result<(), Error> {
    let mut failure = FAILURE
        .lock()
        .map_err(|_| AdminError::Backend("live-export failure injection poisoned".into()))?;
    if let Some((_, _, count)) = failure
        .iter_mut()
        .find(|(selected, step, count)| *selected == root && *step == phase && *count > 0)
    {
        *count -= 1;
        return Err(AdminError::DiskFull.into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveResult {
    Pending,
    Complete,
    Conflict,
    Obsolete,
}

#[derive(Clone)]
pub(crate) struct LiveExport(Arc<tokio::sync::Mutex<LiveOperation>>);

pub(crate) enum LiveSource {
    Observe,
    ApiCopy(File),
    ProtectedRestore(File),
    ProtectedObservation,
    Preserved {
        source: Option<File>,
        conflict: Box<super::super::conflicts::CapturedConflict>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyPurpose {
    Ordinary,
    RestoreAbsent,
    ObserveProtected,
    Preserved,
}

struct LiveOperation {
    item: Pending,
    record: Option<crate::object::ObjectRecord>,
    source: Option<File>,
    tracked: Option<SyncRecord>,
    staged: Option<fsutil::FileIdentity>,
    copied: bool,
    installed: bool,
    observing: bool,
    durable: Option<Option<SyncFingerprint>>,
    cancelling: Option<LiveResult>,
    purpose: CopyPurpose,
    captured: Option<super::super::conflicts::CapturedConflict>,
    source_device: Option<u64>,
}

fn matches_tracked(file: &File, tracked: &SyncRecord) -> Result<bool, Error> {
    let before = fsutil::stat_identity(file)?;
    let fp = &tracked.fingerprint;
    if before.inode != fp.inode || before.size != fp.size || before.modified_ns != fp.mtime_ns {
        return Ok(false);
    }
    // Permission repair changes ctime; stable hashes still prove the tracked body.
    Ok(
        content_matches(file, fp.size, fp.md5, fp.crc32c)?
            && before == fsutil::stat_identity(file)?,
    )
}

fn content_matches(file: &File, size: u64, md5: [u8; 16], crc32c: u32) -> Result<bool, Error> {
    let before = fsutil::stat_identity(file)?;
    if before.size != size {
        return Ok(false);
    }
    let mut hash = Hasher::default();
    let mut buffer = [0; 65_536];
    let mut offset = 0;
    while offset < size {
        let count = fsutil::read_at(
            file,
            &mut buffer[..(size - offset).min(65_536) as usize],
            offset,
        )?;
        if count == 0 {
            return Ok(false);
        }
        hash.update(&buffer[..count]);
        offset += count as u64;
    }
    let hashes = hash.finish();
    Ok(before == fsutil::stat_identity(file)? && hashes.md5 == md5 && hashes.crc32c == crc32c)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IdentityStage {
    BeforeExchange,
    Displaced,
}
fn matches_source(
    file: &File,
    expected: &SyncRecord,
    device: Option<u64>,
    stage: IdentityStage,
) -> Result<bool, Error> {
    if let Some(device) = device {
        let identity = fsutil::stat_identity(file)?;
        // Rename changes ctime; the pre-exchange capture must still match every original observation.
        if identity.device != device
            || (stage == IdentityStage::BeforeExchange
                && identity.changed_ns != expected.fingerprint.ctime_ns)
        {
            return Ok(false);
        }
    }
    matches_tracked(file, expected)
}

fn api_original(
    c: &Transaction<'_>,
    item: &Pending,
    purpose: CopyPurpose,
    captured: Option<&super::super::conflicts::CapturedConflict>,
) -> Result<(), Error> {
    validate_capture(c, item, purpose, captured)?;
    if item.kind == Kind::Publish {
        return match purpose {
            CopyPurpose::Ordinary => original(c, item),
            CopyPurpose::RestoreAbsent | CopyPurpose::Preserved | CopyPurpose::ObserveProtected => {
                original_with_protection(c, item, OriginalProtection::ReadOnlyCopy)
            }
        };
    }
    if matches!(
        purpose,
        CopyPurpose::RestoreAbsent | CopyPurpose::ObserveProtected
    ) {
        return Err(stale());
    }
    let binding = SyncBinding {
        catalog: item.token.catalog,
        bucket: item.bucket.clone(),
        incarnation: item.incarnation.clone(),
        deletion: None,
        control: None,
    };
    super::super::matching(c, &binding, &item.bucket)?;
    if objects::lookup(c, &item.bucket, &item.name)?.is_some() {
        return Err(stale());
    }
    let valid: bool = c.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?3 AND generation=?4)")?.query_row(params![item.bucket,item.name,item.incarnation,item.generation.get()], |r| r.get(0))?;
    if !valid {
        return Err(stale());
    }
    Ok(())
}

fn validate_capture(
    tx: &Transaction<'_>,
    item: &Pending,
    purpose: CopyPurpose,
    captured: Option<&super::super::conflicts::CapturedConflict>,
) -> Result<(), Error> {
    if purpose == CopyPurpose::Preserved {
        let binding = SyncBinding {
            catalog: item.token.catalog,
            bucket: item.bucket.clone(),
            incarnation: item.incarnation.clone(),
            deletion: None,
            control: None,
        };
        captured.ok_or_else(stale)?.validate(
            tx,
            item.token.root.as_bytes(),
            &binding,
            &ObjectName::new(item.name.clone())?,
            if item.kind == Kind::Publish {
                Some(item.generation)
            } else {
                None
            },
        )?;
    }
    Ok(())
}

/// Installs the staged copy at the target path (or moves a removed file into the workspace)
/// unless an earlier attempt already did; `Ok(false)` reports an external change.
fn install_target(
    root: &File,
    workspace: &File,
    selected: &Pending,
    expected: Option<&SyncRecord>,
    staged: Option<fsutil::FileIdentity>,
    purpose: CopyPurpose,
    source_device: Option<u64>,
) -> Result<bool, Error> {
    let current = target(root, &selected.path)?;
    let directory = parent(root, &selected.path, ParentSync::Skip)?.ok_or_else(stale)?;
    let name = selected.path.rsplit('/').next().ok_or_else(stale)?;
    let retained = entry(workspace, &selected.token.id)?;
    let already_installed = match staged {
        Some(staged) => current
            .as_ref()
            .map(fsutil::stat_identity)
            .transpose()?
            .is_some_and(|f| f.device == staged.device && f.inode == staged.inode),
        None => retained.is_some(),
    };
    if !already_installed {
        if purpose == CopyPurpose::RestoreAbsent && current.is_some() {
            return Ok(false);
        }
        if let Some(staged) = staged {
            let copy = retained.as_ref().ok_or(Error::Protected)?;
            if fsutil::stat_identity(copy)? != staged {
                return Err(Error::Protected);
            }
        }
        let identity = match (&current, expected) {
            (None, _) => None,
            (Some(file), Some(expected))
                if matches_source(
                    file,
                    expected,
                    source_device,
                    IdentityStage::BeforeExchange,
                )? =>
            {
                Some(fsutil::stat_identity(file)?)
            }
            _ => return Ok(false),
        };
        #[cfg(test)]
        failure(selected.token.root, "install")?;
        let outcome = if selected.kind == Kind::Remove && current.is_none() {
            fsutil::ReplaceOutcome::Created
        } else if selected.kind == Kind::Publish {
            fsutil::durable_replace_between(
                workspace,
                selected.token.id.as_ref(),
                &directory,
                name.as_ref(),
                identity.as_ref(),
            )?
        } else {
            fsutil::durable_replace_between(
                &directory,
                name.as_ref(),
                workspace,
                selected.token.id.as_ref(),
                None,
            )?
        };
        if matches!(outcome, fsutil::ReplaceOutcome::Conflict { .. }) {
            return Err(Error::Protected);
        }
    }
    #[cfg(test)]
    failure(selected.token.root, "installed")?;
    if let Some(file) = entry(workspace, &selected.token.id)? {
        let matches = match expected {
            Some(expected) => {
                matches_source(&file, expected, source_device, IdentityStage::Displaced)?
            }
            None => false,
        };
        if !matches {
            return Err(Error::Protected);
        }
        #[cfg(test)]
        failure(selected.token.root, "displaced-unlink")?;
        fsutil::unlink_file_at(workspace, selected.token.id.as_ref())?;
    }
    #[cfg(test)]
    failure(selected.token.root, "displaced-parent-sync")?;
    directory.sync_all()?;
    workspace.sync_all()?;
    root.sync_all()?;
    Ok(true)
}

impl Writeback {
    /// A missing source observes an identical live body, or removes an API-deleted tracked body.
    pub(crate) async fn begin_live_export(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        generation: Generation,
        tracked: Option<SyncRecord>,
        fence: JournalFence,
        source: LiveSource,
    ) -> Result<LiveExport, Error> {
        let this = self.clone();
        self.serial(async move {
            let (purpose,source,captured)=match source {
                LiveSource::Observe=>(CopyPurpose::Ordinary,None,None),
                LiveSource::ApiCopy(file)=>(CopyPurpose::Ordinary,Some(file),None),
                LiveSource::ProtectedRestore(file)=>(CopyPurpose::RestoreAbsent,Some(file),None),
                LiveSource::ProtectedObservation=>(CopyPurpose::ObserveProtected,None,None),
                LiveSource::Preserved {source,conflict}=>(CopyPurpose::Preserved,source,Some(*conflict)),
            };
            if let Some(proof)=&captured {
                proof.verify_owned(&this.lease).await.map_err(AdminError::backend)?;
            }
            let bucket = BucketName::new(binding.bucket.clone())?;
            let path = match mapping::name_to_path(&bucket, &name) { PathMapping::File(p) => p.to_str().ok_or_else(stale)?.to_owned(), _ => return Err(stale()) };
            let (catalog, root) = this.identities();
            let has_source = source.is_some();
            let selected = tracked.clone();
            let proof_token=captured.clone();
            let (item,record,proof) = this.db(move |c| {
                #[cfg(test)]
                failure(root,"prepare")?;
                let tx = c.transaction()?;
                check_fence(&tx, fence)?;
                super::super::matching(&tx,&binding,bucket.as_str())?;
                let mut usage = status(&tx, root)?;
                if usage.stop != Stop::Ready { return Err(Error::RecoveryRequired); }
                let (owner, incarnation): (String,String) = tx.prepare_cached("SELECT b.project,i.incarnation FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?1")?.query_row([bucket.as_str()], |r| Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or_else(stale)?;
                let stored = tx.prepare_cached(super::super::SELECT_BY_NAME)?.query_row(params![bucket.as_str(),name.as_str(),incarnation],super::super::read_record).optional()?;
                if stored != selected { return Err(stale()); }
                let record = objects::lookup(&tx,bucket.as_str(),name.as_str())?.map(|(record,_)| record);
                let expected_generation=record.as_ref().map(|record|Generation::new(record.generation)).transpose()?;
                let proof=proof_token.as_ref().map(|token|token.validate(&tx,root.as_bytes(),&binding,&name,expected_generation)).transpose()?;
                let kind = if has_source || record.is_some() { Kind::Publish } else { Kind::Remove };
                let (size,md5,crc32c) = if kind == Kind::Publish {
                    let record = record.as_ref().ok_or_else(stale)?;
                    (record.size,record.md5,record.crc32c)
                } else {
                    let record = selected.as_ref().ok_or_else(stale)?;
                    (record.fingerprint.size,record.fingerprint.md5,record.fingerprint.crc32c)
                };
                // A registered exact-body copy already owns its full retained charge.
                let reservation = if proof.is_some() { 0 } else { selected.as_ref().map_or(0,|s|s.fingerprint.size) };
                let captured_snapshot=proof_token.as_ref().map(|token|token.durable_snapshot(&tx).map_err(AdminError::backend)).transpose()?;
                if if proof.is_some() {usage.over_limit()} else {usage.would_exceed(reservation)} { return Err(Error::Capacity); }
                let id = BlobId::generate().map_err(AdminError::backend)?.to_hex();
                tx.prepare_cached("INSERT INTO sync_writeback_ops(id,root_id,owner,bucket,incarnation,name,path,generation,kind,phase,revision,fence_sequence,fence_epoch,reservation,size,md5,crc32c) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,1,?10,?11,?12,?13,?14,?15)")?.execute(params![id,root.as_bytes(),owner,bucket.as_str(),incarnation,name.as_str(),path,generation.get(),kind as i64,fence.sequence.0 as i64,fence.revision,reservation.to_be_bytes(),size as i64,md5,crc32c])?;
                if let Some(snapshot)=captured_snapshot {
                    tx.prepare_cached("UPDATE sync_writeback_ops SET snapshot=?2 WHERE id=?1")?.execute(params![id,snapshot])?;
                }
                let item = load(&tx,catalog,root,&id)?;
                api_original(&tx,&item,purpose,proof_token.as_ref())?;
                usage.entries += 1;
                usage.reserved_bytes += u128::from(reservation);
                usage.recovery = true;
                save_status(&tx,root,&usage)?;
                tx.commit()?;
                Ok((item,record,proof))
            }).await?;
            let source_device=proof.as_ref().map(|proof|proof.device);
            let tracked=proof.map_or(tracked,|proof|Some(SyncRecord {name:item.name.clone(),path:item.path.clone(),generation:item.generation,fingerprint:proof.fingerprint}));
            let observing = item.kind == Kind::Publish && source.is_none();
            Ok(LiveExport(Arc::new(tokio::sync::Mutex::new(LiveOperation { item, record, source, tracked, staged: None, copied: observing, installed: false, observing, durable: None, cancelling: None,purpose,captured,source_device }))))
        }).await
    }

    /// Each call owns a complete file/SQL transition; callers may cancel only between calls.
    pub(crate) async fn advance_live_export(
        &self,
        operation: LiveExport,
    ) -> Result<LiveResult, Error> {
        let this = self.clone();
        self.serial(async move {
            let mut operation = operation.0.lock().await;
            let operation = &mut *operation;
            this.token(&operation.item.token)?;
            if operation.item.phase == Phase::Protected {
                return Err(Error::RecoveryRequired);
            }
            let selected = operation.item.token.clone();
            this.db(move |connection| {
                let current = checked(connection, &selected)?;
                let usage = status(connection, current.token.root)?;
                if current.phase == Phase::Protected
                    || usage.io_failure
                    || usage.protected_entries > 0
                    || usage.over_limit()
                {
                    return Err(Error::RecoveryRequired);
                }
                Ok(())
            })
            .await?;
            if let Some(result) = operation.cancelling {
                this.cancel_live_copy(&operation.item.clone()).await?;
                return Ok(result);
            }
            if !operation.copied {
                this.copy_live(operation).await?;
                return Ok(LiveResult::Pending);
            }
            if !operation.installed {
                return this.install_live(operation).await;
            }
            if operation.durable.is_none() {
                return this.record_live_durable(operation).await;
            }
            this.acknowledge_live(operation).await
        })
        .await
    }

    /// Copies the API body into a private workspace entry and makes it durable.
    async fn copy_live(&self, operation: &mut LiveOperation) -> Result<(), Error> {
        #[cfg(test)]
        let faults = self.fault_paths();
        let source = operation.source.as_ref().map(File::try_clone).transpose()?;
        let selected = operation.item.clone();
        operation.staged = self
            .fs(move |_, workspace| {
                if let Some(source) = source {
                    #[cfg(test)]
                    failure(selected.token.root, "copy")?;
                    // Only this unarmed operation can own this private, possibly partial API copy.
                    if entry(workspace, &selected.token.id)?.is_some() {
                        fsutil::unlink_file_at(workspace, selected.token.id.as_ref())?;
                    }
                    #[cfg(test)]
                    faults.hit(Step::SyncCopyWrite)?;
                    fsutil::clone_or_copy_into(&source, workspace, selected.token.id.as_ref())?;
                    let file = entry(workspace, &selected.token.id)?.ok_or_else(stale)?;
                    if !content_matches(&file, selected.size, selected.md5, selected.crc32c)? {
                        return Err(AdminError::Checksum.into());
                    }
                    #[cfg(test)]
                    faults.hit(Step::SyncCopySync)?;
                    file.sync_all()?;
                    workspace.sync_all()?;
                    #[cfg(test)]
                    {
                        faults.synced(&file)?;
                        faults.hit(Step::SyncCopyDurable)?;
                    }
                    Ok(Some(fsutil::stat_identity(&file)?))
                } else {
                    Ok(None)
                }
            })
            .await?;
        operation.copied = true;
        Ok(())
    }

    /// Arms a prepared operation, then installs (or, when observing, checks) the target file.
    async fn install_live(&self, operation: &mut LiveOperation) -> Result<LiveResult, Error> {
        if let Some(proof) = &operation.captured {
            proof
                .verify_owned(&self.lease)
                .await
                .map_err(AdminError::backend)?;
        }
        let purpose = operation.purpose;
        let captured = operation.captured.clone();
        let mut item = operation.item.clone();
        let selected = item.clone();
        if item.phase == Phase::Prepared {
            let armed = self
                .db(move |c| {
                    #[cfg(test)]
                    failure(selected.token.root, "arm")?;
                    let tx = c.transaction()?;
                    api_original(&tx, &selected, purpose, captured.as_ref())?;
                    checked(&tx, &selected.token)?;
                    tx.prepare_cached(
                        "UPDATE sync_writeback_ops SET phase=1,revision=revision+1 WHERE id=?1",
                    )?
                    .execute([&selected.token.id])?;
                    let armed = load(
                        &tx,
                        selected.token.catalog,
                        selected.token.root,
                        &selected.token.id,
                    )?;
                    tx.commit()?;
                    Ok(armed)
                })
                .await;
            match armed {
                Ok(armed) => {
                    item = armed;
                    operation.item = item.clone();
                }
                Err(Error::Admin(AdminError::Precondition | AdminError::NotFound)) => {
                    operation.cancelling = Some(LiveResult::Obsolete);
                    self.cancel_live_copy(&item).await?;
                    return Ok(LiveResult::Obsolete);
                }
                Err(error) => return Err(error),
            }
        }
        let selected = item.clone();
        let expected = operation.tracked.clone();
        let source_device = operation.source_device;
        let staged = operation.staged;
        let observing = operation.observing;
        #[cfg(test)]
        let faults = self.fault_paths();
        let installed = self
            .fs(move |root, workspace| {
                if observing {
                    return match target(root, &selected.path)? {
                        Some(file) => {
                            content_matches(&file, selected.size, selected.md5, selected.crc32c)
                        }
                        None => Ok(false),
                    };
                }
                let installed = install_target(
                    root,
                    workspace,
                    &selected,
                    expected.as_ref(),
                    staged,
                    purpose,
                    source_device,
                )?;
                #[cfg(test)]
                if installed {
                    faults.installed(&selected);
                    faults.hit(Step::SyncCopyInstalled)?;
                }
                Ok(installed)
            })
            .await;
        match installed {
            Ok(false) => {
                operation.cancelling = Some(LiveResult::Conflict);
                self.cancel_live_copy(&item).await?;
                return Ok(LiveResult::Conflict);
            }
            Ok(true) => operation.installed = true,
            Err(Error::Protected) => {
                operation.item = self.inspect_inner(item.token.clone()).await?;
                return Err(Error::Protected);
            }
            Err(error) => {
                let selected = item.clone();
                let staged = operation.staged;
                let displaced = self.displaced_live(selected, staged).await?;
                if displaced {
                    let pending = self.inspect_inner(item.token.clone()).await?;
                    operation.item = pending.clone();
                    self.io_failure(pending.token, 1).await?;
                    // Recovery owns displaced bytes; an automatic retry must not unlink them.
                    return Err(AdminError::backend(error).into());
                }
                return Err(error);
            }
        }
        Ok(LiveResult::Pending)
    }

    /// Determines whether a failed installation has already displaced the target.
    async fn displaced_live(
        &self,
        item: Pending,
        staged: Option<fsutil::FileIdentity>,
    ) -> Result<bool, Error> {
        self.fs(move |root, workspace| {
            if let Some(staged) = staged {
                let retained = entry(workspace, &item.token.id)?
                    .as_ref()
                    .map(fsutil::stat_identity)
                    .transpose()?;
                if retained.is_some_and(|retained| {
                    retained.device != staged.device || retained.inode != staged.inode
                }) {
                    return Ok(true);
                }
                let current = target(root, &item.path)?
                    .as_ref()
                    .map(fsutil::stat_identity)
                    .transpose()?;
                Ok(current.is_some_and(|current| {
                    current.device == staged.device && current.inode == staged.inode
                }))
            } else {
                Ok(item.kind == Kind::Remove && entry(workspace, &item.token.id)?.is_some())
            }
        })
        .await
    }

    /// Records the installed file's fingerprint durably before acknowledging it.
    async fn record_live_durable(
        &self,
        operation: &mut LiveOperation,
    ) -> Result<LiveResult, Error> {
        if let Some(proof) = &operation.captured {
            proof
                .verify_owned(&self.lease)
                .await
                .map_err(AdminError::backend)?;
        }
        let item = operation.item.clone();
        #[cfg(test)]
        failure(item.token.root, "durable")?;
        let selected = item.clone();
        let fp = match self
            .fs(move |root, workspace| fingerprint(root, workspace, &selected))
            .await
        {
            Ok(fp) => fp,
            Err(Error::Admin(AdminError::Precondition | AdminError::NotFound))
                if operation.observing =>
            {
                operation.cancelling = Some(LiveResult::Conflict);
                self.cancel_live_copy(&item).await?;
                return Ok(LiveResult::Conflict);
            }
            Err(error) => return Err(error),
        };
        let snapshot = fp
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()
            .map_err(AdminError::backend)?;
        let selected = item.clone();
        let captured = operation.captured.clone();
        let purpose = operation.purpose;
        let durable_fingerprint = fp.clone();
        operation.item = self.db(move |c| {
            let tx = c.transaction()?;
            checked(&tx,&selected.token)?;
            validate_capture(&tx,&selected,purpose,captured.as_ref())?;
            let snapshot=if let Some(proof)=&captured {Some(proof.durable_fingerprint(&tx,durable_fingerprint).map_err(AdminError::backend)?)} else {snapshot};
            tx.prepare_cached("UPDATE sync_writeback_ops SET phase=3,revision=revision+1,snapshot=?2 WHERE id=?1")?.execute(params![selected.token.id,snapshot])?;
            let durable = load(&tx,selected.token.catalog,selected.token.root,&selected.token.id)?;
            tx.commit()?;
            Ok(durable)
        }).await?;
        operation.durable = Some(fp);
        Ok(LiveResult::Pending)
    }

    /// Verifies the durable fingerprint still holds, then tracks it and finishes the operation.
    async fn acknowledge_live(&self, operation: &mut LiveOperation) -> Result<LiveResult, Error> {
        if let Some(proof) = &operation.captured {
            proof
                .verify_owned(&self.lease)
                .await
                .map_err(AdminError::backend)?;
        }
        let purpose = operation.purpose;
        let captured = operation.captured.clone();
        let item = operation.item.clone();
        let fp = operation.durable.clone().ok_or_else(stale)?;
        let selected = item.clone();
        let expected = fp.clone();
        let verified = self
            .fs(move |root, workspace| {
                if fingerprint(root, workspace, &selected)? != expected {
                    return Err(stale());
                }
                Ok(())
            })
            .await;
        match verified {
            Ok(()) => {}
            Err(Error::Admin(AdminError::Precondition | AdminError::NotFound))
                if operation.observing =>
            {
                operation.cancelling = Some(LiveResult::Conflict);
                self.cancel_live_copy(&item).await?;
                return Ok(LiveResult::Conflict);
            }
            Err(error) => return Err(error),
        }
        let selected = item.clone();
        let record = operation.record.clone();
        let scope = self.lease.control_scope();
        let result=self.db(move |c| {
            #[cfg(test)]
            failure(selected.token.root,"ack")?;
            let tx = c.transaction()?;
            let current = checked(&tx,&selected.token)?;
            let valid = match api_original(&tx,&current,purpose,captured.as_ref()) {
                Ok(()) => true,
                Err(Error::Admin(AdminError::Precondition | AdminError::NotFound)) => false,
                Err(error) => return Err(error),
            };
            let binding = SyncBinding { catalog: current.token.catalog, bucket: current.bucket.clone(), incarnation: current.incarnation.clone(), deletion:None,control:Some(scope) };
            let same_binding = match super::super::matching(&tx,&binding,&current.bucket) {
                Ok(()) => true,
                Err(ObjectError::PreconditionFailed) => false,
                Err(error) => return Err(error.into()),
            };
            if same_binding {
                if let Some(fp) = fp {
                    let update = SyncUpdate::new(binding,current.path.clone(),fp)?;
                    // Track the bytes actually installed even if a newer API generation won meanwhile.
                    super::super::record_in_transaction(&tx,&update,record.as_ref().ok_or_else(stale)?)?;
                } else {
                    tx.prepare_cached("DELETE FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?3 AND generation=?4")?.execute(params![current.bucket,current.name,current.incarnation,current.generation.get()])?;
                    if valid {
                        tx.prepare_cached("DELETE FROM sync_findings WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4 AND generation=?5")?.execute(params![current.token.root.as_bytes(),current.bucket,current.name,current.incarnation,current.generation.get()])?;
                    }
                }
            }
            finish_live(&tx,&current)?;
            tx.commit()?;
            Ok(if valid { LiveResult::Complete } else { LiveResult::Obsolete })
        }).await?;
        #[cfg(test)]
        self.fault_paths().hit(Step::SyncCopyCommit)?;
        Ok(result)
    }

    async fn cancel_live_copy(&self, item: &Pending) -> Result<(), Error> {
        let selected = item.clone();
        self.fs(move |_, workspace| {
            if selected.kind == Kind::Publish && entry(workspace, &selected.token.id)?.is_some() {
                fsutil::unlink_file_at(workspace, selected.token.id.as_ref())?;
                workspace.sync_all()?;
            }
            Ok(())
        })
        .await?;
        let selected = item.clone();
        self.db(move |c| {
            #[cfg(test)]
            failure(selected.token.root, "cancel")?;
            let tx = c.transaction()?;
            finish_live(&tx, &selected)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

pub(super) fn finish_live(tx: &Transaction<'_>, item: &Pending) -> Result<(), Error> {
    let mut usage = status(tx, item.token.root)?;
    usage.entries = usage.entries.checked_sub(1).ok_or_else(stale)?;
    usage.reserved_bytes = usage
        .reserved_bytes
        .checked_sub(u128::from(item.reserved_bytes))
        .ok_or_else(stale)?;
    usage.recovery = usage.entries != 0;
    save_status(tx, item.token.root, &usage)?;
    tx.prepare_cached("DELETE FROM sync_writeback_ops WHERE id=?1")?
        .execute([&item.token.id])?;
    Ok(())
}
