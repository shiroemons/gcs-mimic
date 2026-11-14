use super::*;
use crate::storage::admin::{BucketName, GenerationCondition, Origin, Publication, PublishMode};
use crate::storage::catalog::sql;

#[cfg(test)]
mod codec_tests;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Restore,
    Discard,
}
sql::integer_enum!(Action, { Restore => 0, Discard => 1 });
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Confirmation {
    BackupVerified,
    DiscardWithoutBackup,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum ResolveTarget {
    Conflict(ConflictTarget),
    Recovery(RecoveryTarget),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ConflictTarget {
    pub conflict_id: String,
    pub revision: String,
    pub expected_generation: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct RecoveryTarget {
    pub recovery_id: String,
    pub revision: String,
    pub target_identity: String,
    pub confirmation: Confirmation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
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
    root: RootId,
    operation_id: String,
    authority: ControlAuthority,
    target_id: OpaqueId,
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
        .map_err(ObjectError::from)?;
    let owner = check_original(
        tx,
        &binding.binding,
        &binding.name,
        binding.expected_generation,
    )
    .map_err(restore_check_error)?;
    if owner != binding.owner {
        return Err(ObjectError::PreconditionFailed);
    }
    active_restore(tx, binding)
}

/// A storage failure while checking the original stays a storage failure, so the batch stops
/// with 503 instead of reporting a changed target and moving on to the next element.
fn restore_check_error(error: Error) -> ObjectError {
    match error {
        Error::Admin(AdminError::Forbidden) => ObjectError::Forbidden,
        Error::Admin(AdminError::Backend(cause)) => ObjectError::Unavailable(cause),
        Error::Admin(AdminError::Io(cause) | AdminError::DiskFull(cause)) => ObjectError::Io(cause),
        Error::Admin(AdminError::Corrupt(cause)) => ObjectError::Corrupt(cause),
        _ => ObjectError::PreconditionFailed,
    }
}

fn active_restore(tx: &Transaction<'_>, binding: &ResolutionBinding) -> Result<(), ObjectError> {
    let active: bool = sql::scalar(
        tx,
        "SELECT EXISTS(SELECT 1 FROM sync_conflict_operations o JOIN sync_conflicts c ON c.id=o.target_id AND c.root_id=o.root_id WHERE o.root_id=?1 AND o.operation_id=?2 AND o.target_id=?3 AND o.target_kind=0 AND o.action=0 AND o.done=0 AND c.state=1)",
        params![binding.root, binding.operation_id, binding.target_id],
    )?;
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
        .map_err(ObjectError::from)?;
    super::super::matching(tx, &binding.binding, &record.bucket)?;
    active_restore(tx, binding)?;
    if record.bucket != binding.binding.bucket || record.name != binding.name.as_str() {
        return Err(ObjectError::PreconditionFailed);
    }
    sql::exec(
        tx,
        "UPDATE sync_conflict_operations SET done=1,result_generation=?3 WHERE root_id=?1 AND operation_id=?2 AND done=0",
        params![binding.root, binding.operation_id, record.generation],
    )?;
    #[cfg(test)]
    super::super::super::fault::hit(
        &super::super::super::fault::database_dir(tx),
        fault::Step::ConflictRestoreCommit,
    )
    .map_err(objects::object_unavailable)?;
    Ok(())
}

struct Selected {
    id: OpaqueId,
    owner: String,
    bucket: String,
    incarnation: Incarnation,
    name: String,
    observation: Option<Observation>,
    done: bool,
    reclaimed: bool,
    generation: Option<i64>,
}

/// What a resolution operation targets, as stored in `sync_conflict_operations.target_kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TargetKind {
    Conflict,
    Recovery,
}
sql::integer_enum!(TargetKind, { Conflict => 0, Recovery => 1 });

/// What an earlier request with the same operation ID has done so far.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct RecordedOperation {
    pub(super) done: bool,
    pub(super) reclaimed: bool,
    pub(super) result_generation: Option<i64>,
}

/// A `sync_conflict_operations` row, found by its operation ID.
struct StoredOperation {
    root: RootId,
    payload: Vec<u8>,
    done: bool,
    reclaimed: bool,
    result_generation: Option<i64>,
    owner: String,
    error_code: Option<String>,
}

impl StoredOperation {
    const SELECT: &str = "SELECT root_id,payload,done,reclaimed,result_generation,owner,error_code FROM sync_conflict_operations WHERE operation_id=?1";

    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            root: row.get(0)?,
            payload: row.get(1)?,
            done: row.get(2)?,
            reclaimed: row.get(3)?,
            result_generation: row.get(4)?,
            owner: row.get(5)?,
            error_code: row.get(6)?,
        })
    }
}

/// A `sync_conflicts` row a resolution selects.
struct StoredConflict {
    revision: OpaqueId,
    owner: String,
    bucket: String,
    incarnation: Incarnation,
    name: String,
    retained: Option<Vec<u8>>,
    state: ConflictState,
}

impl StoredConflict {
    const SELECT: &str = "SELECT revision,owner,bucket,incarnation,name,retained,state FROM sync_conflicts WHERE root_id=?1 AND id=?2";

    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            revision: row.get(0)?,
            owner: row.get(1)?,
            bucket: row.get(2)?,
            incarnation: row.get(3)?,
            name: row.get(4)?,
            retained: row.get(5)?,
            state: row.get(6)?,
        })
    }
}

fn operation(
    connection: &Connection,
    root: RootId,
    request: &ResolveRequest,
    authority: &ControlAuthority,
) -> Result<Option<RecordedOperation>, Error> {
    let existing = connection
        .prepare_cached(StoredOperation::SELECT)?
        .query_row([&request.operation_id], StoredOperation::read)
        .optional()?;
    let payload = serde_json::to_vec(request).map_err(AdminError::backend)?;
    let Some(existing) = existing else {
        return Ok(None);
    };
    authority.require_project(&existing.owner)?;
    if existing.root != root || existing.payload != payload {
        return Err(Error::OperationIdReused);
    }
    if existing.error_code.is_some() {
        return Err(Error::TargetChanged);
    }
    Ok(Some(RecordedOperation {
        done: existing.done,
        reclaimed: existing.reclaimed,
        result_generation: existing.result_generation,
    }))
}

fn accept(
    connection: &Connection,
    root: RootId,
    request: &ResolveRequest,
    id: OpaqueId,
    kind: TargetKind,
    owner: &str,
) -> Result<(), Error> {
    #[cfg(test)]
    fault::hit(
        &super::super::super::fault::database_dir(connection),
        fault::Step::ConflictAccept,
    )?;
    connection.prepare_cached("INSERT INTO sync_conflict_operations(operation_id,root_id,target_id,target_kind,owner,payload,action,done) VALUES(?1,?2,?3,?4,?5,?6,?7,0)")?
        .execute(params![request.operation_id,root,id,kind,owner,serde_json::to_vec(request).map_err(AdminError::backend)?,request.action])?;
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
                ResolveTarget::Conflict(_) => {
                    lease
                        .resolve_registered(request, authority, max_bytes)
                        .await
                }
                ResolveTarget::Recovery(_) => lease.resolve_recovery(request, authority).await,
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
        let ResolveTarget::Conflict(ConflictTarget {
            conflict_id,
            revision,
            expected_generation,
        }) = &request.target
        else {
            return Err(Error::InvalidResolution);
        };
        let (Ok(target), Ok(inspected)) = (conflict_id.parse::<OpaqueId>(), revision.parse())
        else {
            return Err(Error::InvalidResolution);
        };
        if *expected_generation < 0 {
            return Err(Error::InvalidResolution);
        }
        let root = self.status().root_id;
        let req = request.clone();
        let auth = authority.clone();
        let selected = self
            .conflict_db(move |connection| {
                let tx = connection.transaction()?;
                auth.authorize(&tx, root)?;
                let prior = operation(&tx, root, &req, &auth)?;
                let row = tx
                    .prepare_cached(StoredConflict::SELECT)?
                    .query_row(params![root, target], StoredConflict::read)
                    .optional()?
                    .ok_or(Error::RevisionMismatch)?;
                if prior.is_none() {
                    if row.revision != inspected
                        || row.state == ConflictState::Discarded
                        || (req.action == Action::Restore && row.state != ConflictState::Registered)
                    {
                        return Err(Error::RevisionMismatch);
                    }
                    if req.action == Action::Restore {
                        let ResolveTarget::Conflict(ConflictTarget {
                            expected_generation,
                            ..
                        }) = req.target
                        else {
                            return Err(Error::InvalidResolution);
                        };
                        let expected = (expected_generation != 0)
                            .then(|| Generation::new(expected_generation))
                            .transpose()?;
                        let binding = SyncBinding::new(
                            super::super::catalog_id(&tx)?,
                            row.bucket.clone(),
                            row.incarnation,
                        );
                        if check_original(
                            &tx,
                            &binding,
                            &ObjectName::new(row.name.clone())?,
                            expected,
                        )? != row.owner
                        {
                            return Err(Error::TargetChanged);
                        }
                    }
                    accept(&tx, root, &req, target, TargetKind::Conflict, &row.owner)?;
                }
                let prior = prior.unwrap_or_default();
                let observation = row
                    .retained
                    .as_deref()
                    .map(Observation::decode)
                    .transpose()?;
                tx.commit()?;
                Ok(Selected {
                    id: target,
                    owner: row.owner,
                    bucket: row.bucket,
                    incarnation: row.incarnation,
                    name: row.name,
                    observation,
                    done: prior.done,
                    reclaimed: prior.reclaimed,
                    generation: prior.result_generation,
                })
            })
            .await?;
        #[cfg(test)]
        self.conflict_boundary(fault::Step::ConflictAccepted)
            .await?;
        let generation = if request.action == Action::Restore && !selected.done {
            Some(
                self.restore_copy(&request, &authority, &selected, max_bytes)
                    .await?,
            )
        } else {
            selected.generation
        };
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
        let id = selected.id;
        let source = self
            .conflict_files(move |_| {
                let directory = capture::open_store(&data)?;
                let file = fsutil::open_regular_at(&directory, id.to_hex().as_ref())?;
                if observe(&file, HashMode::Body)? != observation {
                    return Err(Error::TargetChanged);
                }
                Ok(file)
            })
            .await?;
        let ResolveTarget::Conflict(ConflictTarget {
            expected_generation,
            ..
        }) = request.target
        else {
            return Err(Error::InvalidResolution);
        };
        let generation = (expected_generation != 0)
            .then(|| Generation::new(expected_generation))
            .transpose()?;
        let bucket = BucketName::new(selected.bucket.clone())?;
        let name = ObjectName::new(selected.name.clone())?;
        let binding = ResolutionBinding {
            root: self.status().root_id,
            operation_id: request.operation_id.clone(),
            authority: authority.clone(),
            target_id: selected.id,
            binding: SyncBinding::new(
                self.status().catalog_id,
                selected.bucket.clone(),
                selected.incarnation,
            ),
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
        let root = self.status().root_id;
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
        let id = selected.id;
        let mut observation = selected.observation.clone();
        if observation.is_none() {
            let selected_data = data.clone();
            observation = self
                .conflict_files(move |_| {
                    let directory = match capture::open_store(&selected_data) {
                        Ok(directory) => directory,
                        Err(Error::Admin(AdminError::Io(cause)))
                            if cause.kind() == io::ErrorKind::NotFound =>
                        {
                            return Ok(None);
                        }
                        Err(error) => return Err(error),
                    };
                    match fsutil::open_regular_at(&directory, id.to_hex().as_ref()) {
                        Ok(file) => observe(&file, HashMode::Body).map(Some),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                        Err(error) => Err(error.into()),
                    }
                })
                .await?;
            if let Some(observation) = &observation {
                let blob = observation.encode()?;
                self.conflict_db(move|connection| {connection.prepare_cached("UPDATE sync_conflicts SET retained=?2 WHERE id=?1 AND state=0 AND retained IS NULL")?.execute(params![id,blob])?;Ok(())}).await?;
            }
        }
        self.conflict_files(move |_| {
            let directory = match capture::open_store(&data) {
                Ok(directory) => directory,
                Err(Error::Admin(AdminError::Io(cause)))
                    if cause.kind() == io::ErrorKind::NotFound =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            checked_unlink(
                &directory,
                id,
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
        let target = selected.id;
        self.conflict_db(move|connection| {
            let tx=connection.transaction()?;auth.authorize(&tx,root)?;
            operation(&tx,root,&req,&auth)?.ok_or(Error::OperationIdReused)?;
            tx.prepare_cached("UPDATE sync_conflicts SET state=2 WHERE root_id=?1 AND id=?2")?.execute(params![root,target])?;
            tx.prepare_cached("UPDATE sync_conflict_operations SET done=1,reclaimed=1 WHERE root_id=?1 AND operation_id=?2")?.execute(params![root,req.operation_id])?;
            settle_other_operations(&tx,root,&req,target,TargetKind::Conflict)?;
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
    id: OpaqueId,
    expected: Option<&Observation>,
    _data: &std::path::Path,
    _parent: &std::path::Path,
) -> Result<(), Error> {
    let name = id.to_hex();
    match fsutil::open_regular_at(directory, name.as_ref()) {
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
            let again = fsutil::open_regular_at(directory, name.as_ref())?;
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
            super::super::super::fault::unlinked(&_parent.join(&name), &file)?;
            fsutil::unlink_file_at(directory, name.as_ref())?;
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
    root: RootId,
    request: &ResolveRequest,
    authority: &ControlAuthority,
) -> Result<Option<RecordedOperation>, Error> {
    operation(connection, root, request, authority)
}
pub(super) fn accept_recovery(
    connection: &Connection,
    root: RootId,
    request: &ResolveRequest,
    id: OpaqueId,
    owner: &str,
) -> Result<(), Error> {
    accept(connection, root, request, id, TargetKind::Recovery, owner)
}

pub(super) fn settle_other_operations(
    connection: &Connection,
    root: RootId,
    request: &ResolveRequest,
    id: OpaqueId,
    kind: TargetKind,
) -> Result<(), Error> {
    connection.prepare_cached("UPDATE sync_conflict_operations SET error_code=CASE WHEN done=0 THEN 'target_resolved' ELSE error_code END,done=1,reclaimed=1 WHERE root_id=?1 AND target_id=?2 AND target_kind=?3 AND operation_id!=?4 AND reclaimed=0")?
        .execute(params![root,id,kind,request.operation_id])?;
    Ok(())
}
