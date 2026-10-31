use super::*;
use crate::storage::admin::{BucketName, GenerationCondition, Origin, Publication, PublishMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Restore,
    Discard,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Confirmation {
    BackupVerified,
    DiscardWithoutBackup,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ResolveTarget {
    Conflict {
        conflict_id: String,
        revision: String,
        expected_generation: Option<i64>,
    },
    Recovery {
        recovery_id: String,
        revision: String,
        target_identity: String,
        confirmation: Option<Confirmation>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ResolveRequest {
    pub operation_id: String,
    pub action: Action,
    pub target: ResolveTarget,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ResolveOutcome {
    pub operation_id: String,
    pub completed: bool,
    pub result_generation: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolutionBinding {
    root: [u8; 16],
    operation_id: String,
    authority: ControlAuthority,
    target_id: String,
    binding: SyncBinding,
    name: ObjectName,
    expected_generation: Option<Generation>,
    owner: String,
}

pub(crate) fn validate_restore(
    tx: &Transaction<'_>,
    binding: &ResolutionBinding,
) -> Result<(), ObjectError> {
    binding
        .authority
        .authorize(tx, binding.root)
        .map_err(authority_error)?;
    let owner = check_original(
        tx,
        &binding.binding,
        &binding.name,
        binding.expected_generation,
    )
    .map_err(|error| match error {
        Error::Admin(AdminError::Forbidden) => ObjectError::Forbidden,
        Error::Admin(AdminError::Backend(cause)) => ObjectError::Unavailable(cause),
        _ => ObjectError::PreconditionFailed,
    })?;
    if owner != binding.owner {
        return Err(ObjectError::PreconditionFailed);
    }
    active_restore(tx, binding)
}

fn active_restore(tx: &Transaction<'_>, binding: &ResolutionBinding) -> Result<(), ObjectError> {
    let active:bool=tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_conflict_operations o JOIN sync_conflicts c ON c.id=o.target_id AND c.root_id=o.root_id WHERE o.root_id=?1 AND o.operation_id=?2 AND o.target_id=?3 AND o.target_kind=0 AND o.action=0 AND o.done=0 AND c.state=1)")
        .and_then(|mut statement|statement.query_row(params![binding.root,binding.operation_id,binding.target_id],|row|row.get(0))).map_err(objects::object_unavailable)?;
    if !active {
        return Err(ObjectError::PreconditionFailed);
    }
    Ok(())
}

pub(crate) fn complete_restore(
    tx: &Transaction<'_>,
    binding: &ResolutionBinding,
    record: &ObjectRecord,
) -> Result<(), ObjectError> {
    binding
        .authority
        .authorize(tx, binding.root)
        .map_err(authority_error)?;
    super::super::matching(tx, &binding.binding, &record.bucket)?;
    active_restore(tx, binding)?;
    if record.bucket != binding.binding.bucket || record.name != binding.name.as_str() {
        return Err(ObjectError::PreconditionFailed);
    }
    tx.prepare_cached("UPDATE sync_conflict_operations SET done=1,result_generation=?3 WHERE root_id=?1 AND operation_id=?2 AND done=0")
        .and_then(|mut statement|statement.execute(params![binding.root,binding.operation_id,record.generation])).map_err(objects::object_unavailable)?;
    #[cfg(test)]
    super::super::super::fault::hit(
        &super::super::super::fault::database_dir(tx),
        fault::Step::ConflictRestoreCommit,
    )
    .map_err(objects::object_unavailable)?;
    Ok(())
}

fn authority_error(error: CatalogError) -> ObjectError {
    match error {
        CatalogError::Forbidden => ObjectError::Forbidden,
        error => objects::object_unavailable(error),
    }
}

struct Selected {
    id: String,
    owner: String,
    bucket: String,
    incarnation: String,
    name: String,
    observation: Option<Observation>,
    done: bool,
    reclaimed: bool,
    generation: Option<i64>,
}

fn operation(
    connection: &Connection,
    root: [u8; 16],
    request: &ResolveRequest,
    authority: &ControlAuthority,
) -> Result<Option<(bool, bool, Option<i64>)>, Error> {
    let existing=connection.prepare_cached("SELECT root_id,payload,done,reclaimed,result_generation,owner,error_code FROM sync_conflict_operations WHERE operation_id=?1")?
        .query_row([&request.operation_id],|row|Ok((row.get::<_,[u8;16]>(0)?,row.get::<_,Vec<u8>>(1)?,row.get::<_,bool>(2)?,row.get::<_,bool>(3)?,row.get::<_,Option<i64>>(4)?,row.get::<_,String>(5)?,row.get::<_,Option<String>>(6)?))).optional()?;
    let payload = serde_json::to_vec(request).map_err(AdminError::backend)?;
    match existing {
        None => Ok(None),
        Some((r, p, d, c, g, owner, error)) => {
            authority.require_project(&owner)?;
            if r != root || p != payload {
                return Err(Error::OperationIdReused);
            }
            if error.is_some() {
                return Err(Error::TargetChanged);
            }
            Ok(Some((d, c, g)))
        }
    }
}

fn accept(
    connection: &Connection,
    root: [u8; 16],
    request: &ResolveRequest,
    id: &str,
    kind: i64,
    owner: &str,
) -> Result<(), Error> {
    #[cfg(test)]
    fault::hit(
        &super::super::super::fault::database_dir(connection),
        fault::Step::ConflictAccept,
    )?;
    connection.prepare_cached("INSERT INTO sync_conflict_operations(operation_id,root_id,target_id,target_kind,owner,payload,action,done) VALUES(?1,?2,?3,?4,?5,?6,?7,0)")?
        .execute(params![request.operation_id,root,id,kind,owner,serde_json::to_vec(request).map_err(AdminError::backend)?,i64::from(request.action==Action::Discard)])?;
    Ok(())
}

impl RootLease {
    pub(crate) async fn resolve_conflict(
        &self,
        request: ResolveRequest,
        authority: ControlAuthority,
        max_bytes: u64,
    ) -> Result<ResolveOutcome, Error> {
        if request.operation_id.is_empty()
            || request.operation_id.len() > 128
            || !request
                .operation_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Error::InvalidResolution);
        }
        let lease = self.with_authority(authority.clone());
        self.conflict_serial(async move {
            match request.target {
                ResolveTarget::Conflict { .. } => {
                    lease
                        .resolve_registered(request, authority, max_bytes)
                        .await
                }
                ResolveTarget::Recovery { .. } => lease.resolve_recovery(request, authority).await,
            }
        })
        .await
    }

    async fn resolve_registered(
        &self,
        request: ResolveRequest,
        authority: ControlAuthority,
        max_bytes: u64,
    ) -> Result<ResolveOutcome, Error> {
        let ResolveTarget::Conflict {
            conflict_id,
            revision,
            expected_generation,
        } = &request.target
        else {
            return Err(Error::InvalidResolution);
        };
        if !valid_id(conflict_id)
            || !valid_id(revision)
            || expected_generation.is_some_and(|generation| generation < 0)
        {
            return Err(Error::InvalidResolution);
        }
        let root = self.status().root_id.as_bytes();
        let target = conflict_id.clone();
        let inspected = revision.clone();
        let req = request.clone();
        let auth = authority.clone();
        let selected=self.conflict_db(move|connection| {
            let tx=connection.transaction()?;auth.authorize(&tx,root)?;
            let prior=operation(&tx,root,&req,&auth)?;
            let row=tx.prepare_cached("SELECT revision,owner,bucket,incarnation,name,retained,state FROM sync_conflicts WHERE root_id=?1 AND id=?2")?
                .query_row(params![root,target],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,Option<Vec<u8>>>(5)?,row.get::<_,i64>(6)?))).optional()?.ok_or(Error::RevisionMismatch)?;
            if prior.is_none() {
                if row.0!=inspected || row.6==2 || (req.action==Action::Restore && row.6!=1) {return Err(Error::RevisionMismatch)}
                if req.action==Action::Restore {
                    let ResolveTarget::Conflict {expected_generation,..}=req.target else {return Err(Error::InvalidResolution)};
                    let expected=expected_generation.filter(|g|*g!=0).map(Generation::new).transpose()?;
                    let binding=SyncBinding {catalog:super::super::catalog_id(&tx)?,bucket:row.2.clone(),incarnation:row.3.clone(),deletion:None,control:None};
                    if check_original(&tx,&binding,&ObjectName::new(row.4.clone())?,expected)?!=row.1 {return Err(Error::TargetChanged)}
                }
                accept(&tx,root,&req,&target,0,&row.1)?;
            }
            let (done,reclaimed,generation)=prior.unwrap_or((false,false,None));
            let observation=row.5.as_deref().map(Observation::decode).transpose()?;
            tx.commit()?;Ok(Selected {id:target,owner:row.1,bucket:row.2,incarnation:row.3,name:row.4,observation,done,reclaimed,generation})
        }).await?;
        #[cfg(test)]
        self.conflict_boundary(fault::Step::ConflictAccepted)
            .await?;
        let mut generation = selected.generation;
        if request.action == Action::Restore && !selected.done {
            generation = Some(
                self.restore_copy(&request, &authority, &selected, max_bytes)
                    .await?,
            );
        }
        if !selected.reclaimed {
            self.discard_registered(&request, &authority, &selected)
                .await?;
        }
        Ok(ResolveOutcome {
            operation_id: request.operation_id,
            completed: true,
            result_generation: generation,
        })
    }

    async fn restore_copy(
        &self,
        request: &ResolveRequest,
        authority: &ControlAuthority,
        selected: &Selected,
        max_bytes: u64,
    ) -> Result<i64, Error> {
        let observation = selected.observation.clone().ok_or(Error::TargetChanged)?;
        let hashes = observation.hashes.as_ref().ok_or(Error::TargetChanged)?;
        let expected = crate::object::ExpectedHashes {
            md5: Some(hashes.md5),
            crc32c: Some(hashes.crc32c),
        };
        let data = data_path(self);
        let id = selected.id.clone();
        let source = self
            .conflict_files(move |_| {
                let directory = capture::open_store(&data)?;
                let file = fsutil::open_regular_at(&directory, id.as_ref())?;
                if observe(&file, HashMode::Body)? != observation {
                    return Err(Error::TargetChanged);
                }
                Ok(file)
            })
            .await?;
        let ResolveTarget::Conflict {
            expected_generation,
            ..
        } = request.target
        else {
            return Err(Error::InvalidResolution);
        };
        let generation = expected_generation
            .filter(|g| *g != 0)
            .map(Generation::new)
            .transpose()?;
        let bucket = BucketName::new(selected.bucket.clone())?;
        let name = ObjectName::new(selected.name.clone())?;
        let binding = ResolutionBinding {
            root: self.status().root_id.as_bytes(),
            operation_id: request.operation_id.clone(),
            authority: authority.clone(),
            target_id: selected.id.clone(),
            binding: SyncBinding {
                catalog: self.status().catalog_id,
                bucket: selected.bucket.clone(),
                incarnation: selected.incarnation.clone(),
                deletion: None,
                control: None,
            },
            name: name.clone(),
            expected_generation: generation,
            owner: selected.owner.clone(),
        };
        // Operator restores must reach the tree; they are not echoes of a tree ingest.
        let admin = self.catalog().admin(Origin::Api).with_sync_authority(
            super::super::authority::ControlScope::new(self.status().root_id, authority.clone()),
        );
        let staged = admin
            .stage_from_file(source, expected, max_bytes)
            .await?
            .with_sync_resolution(binding);
        let mut results = admin
            .publish_objects(vec![crate::storage::catalog::admin::BatchPublication {
                bucket,
                name,
                request: Publication {
                    content_type: crate::mime::SeedContentType::Extension
                        .for_name(&selected.name)
                        .into(),
                    metadata: Default::default(),
                    expected,
                    condition: Some(
                        generation.map_or(GenerationCondition::Absent, GenerationCondition::Is),
                    ),
                    max_bytes,
                    mode: PublishMode::InheritFromReplaced,
                },
                staged,
            }])
            .await?;
        let result = results.results.pop().ok_or(Error::TargetChanged)??;
        #[cfg(test)]
        self.conflict_boundary(fault::Step::ConflictRestorePublished)
            .await?;
        Ok(result.generation)
    }

    async fn discard_registered(
        &self,
        request: &ResolveRequest,
        authority: &ControlAuthority,
        selected: &Selected,
    ) -> Result<(), Error> {
        let root = self.status().root_id.as_bytes();
        let req = request.clone();
        let auth = authority.clone();
        self.conflict_db(move |connection| {
            let tx = connection.transaction()?;
            auth.authorize(&tx, root)?;
            operation(&tx, root, &req, &auth)?.ok_or(Error::OperationIdReused)?;
            Ok(())
        })
        .await?;
        let data = data_path(self);
        let id = selected.id.clone();
        let mut observation = selected.observation.clone();
        if observation.is_none() {
            let selected_data = data.clone();
            let selected_id = id.clone();
            observation = self
                .conflict_files(move |_| {
                    let directory = match capture::open_store(&selected_data) {
                        Ok(directory) => directory,
                        Err(Error::Admin(AdminError::Io(io::ErrorKind::NotFound))) => {
                            return Ok(None);
                        }
                        Err(error) => return Err(error),
                    };
                    match fsutil::open_regular_at(&directory, selected_id.as_ref()) {
                        Ok(file) => observe(&file, HashMode::Body).map(Some),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                        Err(error) => Err(error.into()),
                    }
                })
                .await?;
            if let Some(observation) = &observation {
                let blob = observation.encode()?;
                let selected_id = id.clone();
                self.conflict_db(move|connection| {connection.prepare_cached("UPDATE sync_conflicts SET retained=?2 WHERE id=?1 AND state=0 AND retained IS NULL")?.execute(params![selected_id,blob])?;Ok(())}).await?;
            }
        }
        self.conflict_files(move |_| {
            let directory = match capture::open_store(&data) {
                Ok(directory) => directory,
                Err(Error::Admin(AdminError::Io(io::ErrorKind::NotFound))) => return Ok(()),
                Err(error) => return Err(error),
            };
            checked_unlink(
                &directory,
                &id,
                observation.as_ref(),
                &data,
                &data.join("sync-conflicts"),
            )
        })
        .await?;
        #[cfg(test)]
        self.conflict_boundary(fault::Step::ConflictComplete)
            .await?;
        let req = request.clone();
        let auth = authority.clone();
        let target = selected.id.clone();
        self.conflict_db(move|connection| {
            let tx=connection.transaction()?;auth.authorize(&tx,root)?;
            operation(&tx,root,&req,&auth)?.ok_or(Error::OperationIdReused)?;
            tx.prepare_cached("UPDATE sync_conflicts SET state=2 WHERE root_id=?1 AND id=?2")?.execute(params![root,target])?;
            tx.prepare_cached("UPDATE sync_conflict_operations SET done=1,reclaimed=1 WHERE root_id=?1 AND operation_id=?2")?.execute(params![root,req.operation_id])?;
            settle_other_operations(&tx,root,&req,&target,0)?;
            changed(&tx,root)?;tx.commit()?;Ok(())
        }).await?;
        #[cfg(test)]
        self.conflict_boundary(fault::Step::ConflictCompleted)
            .await?;
        Ok(())
    }
}

pub(super) fn checked_unlink(
    directory: &File,
    id: &str,
    expected: Option<&Observation>,
    _data: &std::path::Path,
    _parent: &std::path::Path,
) -> Result<(), Error> {
    match fsutil::open_regular_at(directory, id.as_ref()) {
        Ok(file) => {
            if let Some(expected) = expected {
                if observe(
                    &file,
                    if expected.hashes.is_some() {
                        HashMode::Body
                    } else {
                        HashMode::Metadata
                    },
                )? != *expected
                {
                    return Err(Error::TargetChanged);
                }
            } else {
                return Err(Error::TargetChanged);
            }
            let again = fsutil::open_regular_at(directory, id.as_ref())?;
            if fsutil::stat_identity(&again)? != fsutil::stat_identity(&file)? {
                return Err(Error::TargetChanged);
            }
            #[cfg(test)]
            {
                fault::hit(
                    _parent.parent().ok_or(Error::TargetChanged)?,
                    fault::Step::ConflictUnlink,
                )?;
                fault::hit(_data, fault::Step::ConflictUnlink)?;
            }
            #[cfg(all(test, unix))]
            super::super::super::fault::unlinked(&_parent.join(id), &file)?;
            fsutil::unlink_file_at(directory, id.as_ref())?;
            #[cfg(test)]
            {
                fault::hit(
                    _parent.parent().ok_or(Error::TargetChanged)?,
                    fault::Step::ConflictUnlinked,
                )?;
                fault::hit(_data, fault::Step::ConflictUnlinked)?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    #[cfg(test)]
    {
        fault::hit(
            _parent.parent().ok_or(Error::TargetChanged)?,
            fault::Step::ConflictParentSync,
        )?;
        fault::hit(_data, fault::Step::ConflictParentSync)?;
    }
    directory.sync_all()?;
    #[cfg(all(test, unix))]
    super::super::super::fault::dir_synced(_parent);
    Ok(())
}

pub(super) fn recorded_operation(
    connection: &Connection,
    root: [u8; 16],
    request: &ResolveRequest,
    authority: &ControlAuthority,
) -> Result<Option<(bool, bool, Option<i64>)>, Error> {
    operation(connection, root, request, authority)
}
pub(super) fn accept_recovery(
    connection: &Connection,
    root: [u8; 16],
    request: &ResolveRequest,
    id: &str,
    owner: &str,
) -> Result<(), Error> {
    accept(connection, root, request, id, 1, owner)
}

pub(super) fn settle_other_operations(
    connection: &Connection,
    root: [u8; 16],
    request: &ResolveRequest,
    id: &str,
    kind: i64,
) -> Result<(), Error> {
    connection.prepare_cached("UPDATE sync_conflict_operations SET error_code=CASE WHEN done=0 THEN 'target_resolved' ELSE error_code END,done=1,reclaimed=1 WHERE root_id=?1 AND target_id=?2 AND target_kind=?3 AND operation_id!=?4 AND reclaimed=0")?
        .execute(params![root,id,kind,request.operation_id])?;
    Ok(())
}
