use super::*;
use std::io::Write;

fn store(data: &std::path::Path) -> Result<File, Error> {
    let directory = fsutil::open_directory_nofollow(data)?;
    #[cfg(all(test, unix))]
    let existed = data.join("sync-conflicts").try_exists()?;
    let store = fsutil::directory_at(
        &directory,
        "sync-conflicts".as_ref(),
        fsutil::DirectoryOpen::CreateIfMissing,
    )?;
    #[cfg(all(test, unix))]
    if !existed {
        super::super::super::fault::dir_created(&data.join("sync-conflicts"));
    }
    store.sync_all()?;
    directory.sync_all()?;
    #[cfg(all(test, unix))]
    {
        super::super::super::fault::dir_synced(&data.join("sync-conflicts"));
        super::super::super::fault::dir_synced(data);
    }
    Ok(store)
}

pub(super) fn open_store(data: &std::path::Path) -> Result<File, Error> {
    let directory = fsutil::open_directory_nofollow(data)?;
    fsutil::directory_at(
        &directory,
        "sync-conflicts".as_ref(),
        fsutil::DirectoryOpen::Existing,
    )
    .map_err(Error::from)
}

fn copy(
    data: &std::path::Path,
    id: &str,
    source: &File,
    before: &Observation,
    #[cfg(test)] tree: &std::path::Path,
) -> Result<(Observation, Observation), Error> {
    let directory = store(data)?;
    let mut target = fsutil::create_regular_at(&directory, id.as_ref())?;
    #[cfg(all(test, unix))]
    super::super::super::fault::file_created(&data.join("sync-conflicts").join(id));
    let mut hasher = Hasher::default();
    let mut offset = 0;
    let mut buffer = [0; 65_536];
    while offset < before.size {
        let count = fsutil::read_at(
            source,
            &mut buffer[..(before.size - offset).min(65_536) as usize],
            offset,
        )?;
        if count == 0 {
            return Err(Error::TargetChanged);
        }
        #[cfg(test)]
        boundary(data, tree, fault::Step::ConflictWrite)?;
        target.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        offset += count as u64;
    }
    if !before.matches(fsutil::stat_identity(source)?) {
        return Err(Error::TargetChanged);
    }
    let hashes = hasher.finish();
    #[cfg(test)]
    boundary(data, tree, fault::Step::ConflictFlush)?;
    target.flush()?;
    let mut permissions = target.metadata()?.permissions();
    permissions.set_readonly(true);
    target.set_permissions(permissions)?;
    #[cfg(test)]
    boundary(data, tree, fault::Step::ConflictFileSync)?;
    target.sync_all()?;
    #[cfg(test)]
    super::super::super::fault::file_synced(data, &target.metadata()?);
    #[cfg(test)]
    boundary(data, tree, fault::Step::ConflictDirectorySync)?;
    directory.sync_all()?;
    #[cfg(all(test, unix))]
    super::super::super::fault::dir_synced(&data.join("sync-conflicts"));
    let retained = Observation::new(fsutil::stat_identity(&target)?, Some(hashes));
    let mut original = before.clone();
    original.hashes = retained.hashes.clone();
    Ok((original, retained))
}

impl RootLease {
    pub(crate) async fn capture_path(
        &self,
        request: CaptureRequest,
    ) -> Result<CapturedConflict, Error> {
        if request.source_op.is_some() {
            return Err(Error::InvalidResolution);
        }
        let bucket = crate::storage::admin::BucketName::new(request.binding.bucket.clone())?;
        let crate::sync::mapping::PathMapping::File(path) =
            crate::sync::mapping::name_to_path(&bucket, &request.name)
        else {
            return Err(Error::InvalidResolution);
        };
        let path = path.to_str().ok_or(Error::InvalidResolution)?.to_owned();
        let lease = self.clone();
        self.conflict_serial(async move {
            let source = lease
                .conflict_files(move |root| {
                    let (parents, name) = path.rsplit_once('/').ok_or(Error::InvalidResolution)?;
                    let mut directory = root.try_clone()?;
                    for part in parents.split('/') {
                        directory = fsutil::directory_at(
                            &directory,
                            part.as_ref(),
                            fsutil::DirectoryOpen::Existing,
                        )?;
                    }
                    fsutil::open_regular_at(&directory, name.as_ref()).map_err(Error::from)
                })
                .await?;
            if let Some(captured) = lease.reuse_path_capture(&request, &source).await? {
                return Ok(captured);
            }
            lease.capture_inner(request, source).await
        })
        .await
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn capture_conflict(
        &self,
        request: CaptureRequest,
        source: File,
    ) -> Result<CapturedConflict, Error> {
        let lease = self.clone();
        self.conflict_serial(async move { lease.capture_inner(request, source).await })
            .await
    }

    async fn reuse_path_capture(
        &self,
        request: &CaptureRequest,
        source: &File,
    ) -> Result<Option<CapturedConflict>, Error> {
        let root = self.status().root_id.as_bytes();
        let selected = request.clone();
        let exists = self.conflict_db(move |connection| {
            Ok(connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_conflicts WHERE root_id=?1 AND bucket=?2 AND incarnation=?3 AND name=?4 AND state=1 AND source_op IS NULL)")?
                .query_row(params![root,selected.binding.bucket,selected.binding.incarnation,selected.name.as_str()], |row| row.get::<_,bool>(0))?)
        }).await?;
        if !exists {
            return Ok(None);
        }
        let selected = source.try_clone()?;
        let original = self
            .conflict_files(move |_| {
                if fsutil::stat_identity(&selected)?.size > MAX_BYTES {
                    return Err(Error::Capacity);
                }
                observe(&selected, HashMode::Body)
            })
            .await?;
        let selected = request.clone();
        let source_blob = original.encode()?;
        let candidate = self.conflict_db(move |connection| {
            let owner = check_original(connection, &selected.binding, &selected.name, selected.expected_generation)?;
            connection.prepare_cached("SELECT id,revision,retained FROM sync_conflicts WHERE root_id=?1 AND bucket=?2 AND incarnation=?3 AND name=?4 AND owner=?5 AND state=1 AND source_op IS NULL AND source=?6 ORDER BY ordinal DESC LIMIT 1")?
                .query_row(params![root,selected.binding.bucket,selected.binding.incarnation,selected.name.as_str(),owner,source_blob], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Vec<u8>>(2)?)))
                .optional().map_err(Error::from)
        }).await?;
        let Some((id, revision, retained_blob)) = candidate else {
            return Ok(None);
        };
        let retained = Observation::decode(&retained_blob)?;
        if retained.size != original.size || retained.hashes != original.hashes {
            return Ok(None);
        }
        let data = data_path(self);
        let selected_id = id.clone();
        let expected = retained.clone();
        let selected = source.try_clone()?;
        let expected_source = original.clone();
        let valid = self
            .conflict_files(move |_| {
                let store = open_store(&data)?;
                let file = match fsutil::open_regular_at(&store, selected_id.as_ref()) {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                    Err(error) => return Err(error.into()),
                };
                if !file.metadata()?.permissions().readonly()
                    || !expected.matches(fsutil::stat_identity(&file)?)
                {
                    return Ok(false);
                }
                Ok(observe(&file, HashMode::Body)? == expected
                    && expected_source.matches(fsutil::stat_identity(&selected)?))
            })
            .await?;
        if !valid {
            return Ok(None);
        }
        let selected = request.clone();
        let selected_id = id.clone();
        let selected_revision = revision.clone();
        let source_blob = original.encode()?;
        self.conflict_db(move |connection| {
            let tx = connection.transaction()?;
            let owner = check_original(&tx, &selected.binding, &selected.name, selected.expected_generation)?;
            let valid:bool = tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_conflicts WHERE root_id=?1 AND id=?2 AND revision=?3 AND owner=?4 AND state=1 AND source=?5 AND retained=?6)")?
                .query_row(params![root,selected_id,selected_revision,owner,source_blob,retained_blob], |row| row.get(0))?;
            if !valid { return Err(Error::TargetChanged); }
            Ok(())
        }).await?;
        let hashes = original.hashes.ok_or(Error::TargetChanged)?;
        Ok(Some(CapturedConflict {
            root,
            id,
            revision,
            binding: request.binding.clone(),
            name: request.name.clone(),
            expected_generation: request.expected_generation,
            fingerprint: SyncFingerprint {
                size: original.size,
                mtime_ns: original.modified_ns,
                ctime_ns: original.changed_ns,
                inode: original.inode,
                md5: hashes.md5,
                crc32c: hashes.crc32c,
                racy: true,
            },
            device: original.device,
        }))
    }
    pub(super) async fn capture_inner(
        &self,
        request: CaptureRequest,
        source: File,
    ) -> Result<CapturedConflict, Error> {
        if request.source_op.is_some()
            && let Some(captured) = self.reuse_workspace_capture(&request, &source).await?
        {
            return Ok(captured);
        }
        let (source, before) = self
            .conflict_files(move |_| {
                let before = observe(&source, HashMode::Metadata)?;
                Ok((source, before))
            })
            .await?;
        if before.size > MAX_BYTES {
            return Err(Error::Capacity);
        }
        let root = self.status().root_id.as_bytes();
        let id = opaque_id()?;
        let revision = opaque_id()?;
        let selected = request.clone();
        let reserved_id = id.clone();
        let reserved_revision = revision.clone();
        let observed = before.encode()?;
        let size = before.size;
        self.conflict_db(move|connection| {
            let tx=connection.transaction()?;
            let owner=if let Some(op)=&selected.source_op {
                recovery::account_workspace(&tx,root,op,size)?;
                tx.prepare_cached("SELECT owner FROM sync_writeback_ops WHERE root_id=?1 AND id=?2 AND bucket=?3 AND incarnation=?4 AND name=?5 AND generation=?6 AND phase=2")?
                    .query_row(params![root,op,selected.binding.bucket,selected.binding.incarnation,selected.name.as_str(),selected.expected_generation.map(Generation::get)],|row|row.get::<_,String>(0)).optional()?.ok_or(Error::TargetChanged)?
            } else {check_original(&tx,&selected.binding,&selected.name,selected.expected_generation)?};
            let usage=list::total_usage(&tx,root)?;
            let limits:Option<(u64,u64)>=tx.prepare_cached("SELECT max_bytes,max_entries FROM sync_writeback_roots WHERE root_id=?1")?.query_row([root],|row|Ok((row.get::<_,i64>(0)? as u64,row.get::<_,i64>(1)? as u64))).optional()?;
            let (max_bytes,max_entries)=limits.unwrap_or((MAX_BYTES,MAX_ENTRIES));
            if usage.over_limit || usage.reserved_bytes+usage.retained_bytes+u128::from(size)>u128::from(max_bytes.min(MAX_BYTES)) || usage.entries>=max_entries.min(MAX_ENTRIES) {return Err(Error::Capacity)}
            tx.prepare_cached("INSERT INTO sync_conflicts(id,root_id,revision,owner,bucket,incarnation,name,generation,state,size,source_op,source) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0,?9,?10,?11)")?
                .execute(params![reserved_id,root,reserved_revision,owner,selected.binding.bucket,selected.binding.incarnation,selected.name.as_str(),selected.expected_generation.map(Generation::get),size as i64,selected.source_op,observed])?;
            changed(&tx,root)?;tx.commit()?;Ok(())
        }).await?;
        #[cfg(test)]
        {
            self.conflict_boundary(fault::Step::ConflictReserved)
                .await?;
        }
        let data = data_path(self);
        #[cfg(test)]
        let tree = self.fault_tree();
        let copy_id = id.clone();
        let result = self
            .conflict_files(move |_| {
                copy(
                    &data,
                    &copy_id,
                    &source,
                    &before,
                    #[cfg(test)]
                    &tree,
                )
            })
            .await;
        let (original, retained) = match result {
            Ok(value) => value,
            Err(error) => {
                self.cleanup_capture(id).await?;
                return Err(error);
            }
        };
        let fingerprint = SyncFingerprint {
            size: original.size,
            mtime_ns: original.modified_ns,
            ctime_ns: original.changed_ns,
            inode: original.inode,
            md5: original.hashes.as_ref().ok_or(Error::TargetChanged)?.md5,
            crc32c: original.hashes.as_ref().ok_or(Error::TargetChanged)?.crc32c,
            racy: true,
        };
        let registered = id.clone();
        let source_blob = original.encode()?;
        let retained_blob = retained.encode()?;
        #[cfg(test)]
        let tree = self.fault_tree();
        self.conflict_db(move |connection| {
            let tx = connection.transaction()?;
            #[cfg(test)]
            fault::hit(&tree, fault::Step::ConflictRegister)?;
            #[cfg(test)]
            fault::hit(
                &super::super::super::fault::database_dir(&tx),
                fault::Step::ConflictRegister,
            )?;
            tx.prepare_cached(
                "UPDATE sync_conflicts SET state=1,source=?2,retained=?3 WHERE id=?1 AND state=0",
            )?
            .execute(params![registered, source_blob, retained_blob])?;
            changed(&tx, root)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        #[cfg(test)]
        {
            self.conflict_boundary(fault::Step::ConflictRegistered)
                .await?;
        }
        Ok(CapturedConflict {
            root,
            id,
            revision,
            binding: request.binding,
            name: request.name,
            expected_generation: request.expected_generation,
            fingerprint,
            device: original.device,
        })
    }
    pub(super) async fn cleanup_capture(&self, id: String) -> Result<(), Error> {
        let data = data_path(self);
        let selected = id.clone();
        self.conflict_files(move |_| {
            let store = match open_store(&data) {
                Ok(store) => store,
                Err(Error::Admin(AdminError::Io(io::ErrorKind::NotFound))) => return Ok(()),
                Err(error) => return Err(error),
            };
            match fsutil::unlink_file_at(&store, selected.as_ref()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            store.sync_all()?;
            Ok(())
        })
        .await?;
        let root = self.status().root_id.as_bytes();
        self.conflict_db(move |connection| {
            let tx = connection.transaction()?;
            tx.prepare_cached("DELETE FROM sync_conflicts WHERE id=?1 AND state=0")?
                .execute([id])?;
            changed(&tx, root)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
fn boundary(
    data: &std::path::Path,
    tree: &std::path::Path,
    step: fault::Step,
) -> Result<(), Error> {
    fault::hit(tree, step)?;
    fault::hit(data, step)
}
