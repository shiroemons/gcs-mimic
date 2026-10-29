use super::*;

/// Private provenance for a prepared API copy, never for a displaced external body.
#[derive(Clone)]
pub struct Unpublished {
    token: Token,
    staged: Option<fsutil::FileIdentity>,
    target: Option<fsutil::FileIdentity>,
}

/// Whether the prepared body was already installed at the target path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Installation {
    Installed,
    Absent,
}

fn same_inode(left: fsutil::FileIdentity, right: fsutil::FileIdentity) -> bool {
    left.device == right.device && left.inode == right.inode
}

fn matches_body(file: &File, item: &Pending) -> Result<bool, Error> {
    let before = fsutil::stat_identity(file)?;
    if before.size != item.size {
        return Ok(false);
    }
    let mut hash = Hasher::default();
    let mut buffer = [0; 65_536];
    let mut offset = 0;
    while offset < item.size {
        let count = fsutil::read_at(
            file,
            &mut buffer[..(item.size - offset).min(65_536) as usize],
            offset,
        )?;
        if count == 0 {
            return Ok(false);
        }
        hash.update(&buffer[..count]);
        offset += count as u64;
    }
    let hashes = hash.finish();
    Ok(before == fsutil::stat_identity(file)?
        && hashes.md5 == item.md5
        && hashes.crc32c == item.crc32c)
}

impl Writeback {
    /// Stages only immutable API bytes before arming; publication still uses the existing file job.
    pub async fn prepare_publish_copy(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        generation: Generation,
        fence: JournalFence,
        source: Option<File>,
    ) -> Result<(Pending, Unpublished), Error> {
        let (this, bucket, name) = (self.clone(), bucket.clone(), name.clone());
        self.serial(async move {
            let item = this
                .prepare_inner(&bucket, &name, generation, Kind::Publish, fence)
                .await?;
            let selected = item.clone();
            let (staged, target) = this
                .fs(move |root, workspace| {
                    let target = target(root, &selected.path)?
                        .as_ref()
                        .map(fsutil::stat_identity)
                        .transpose()?;
                    let staged = if let Some(source) = source {
                        fsutil::clone_or_copy_into(
                            &source,
                            workspace,
                            selected.workspace_entry.name().as_ref(),
                        )?;
                        let copy =
                            entry(workspace, selected.workspace_entry.name())?.ok_or_else(stale)?;
                        if !matches_body(&copy, &selected)? {
                            return Err(AdminError::Checksum.into());
                        }
                        copy.sync_all()?;
                        workspace.sync_all()?;
                        Some(fsutil::stat_identity(&copy)?)
                    } else {
                        if entry(workspace, selected.workspace_entry.name())?.is_some() {
                            return Err(Error::Protected);
                        }
                        None
                    };
                    Ok((staged, target))
                })
                .await?;
            let proof = Unpublished {
                token: item.token.clone(),
                staged,
                target,
            };
            Ok((item, proof))
        })
        .await
    }

    /// Cancels only an unpublished intent with private copy provenance; the target is untouched.
    pub async fn abandon_unpublished(&self, proof: Unpublished) -> Result<(), Error> {
        self.token(&proof.token)?;
        let this = self.clone();
        self.serial(async move {
            let token = proof.token.clone();
            let item = this
                .db(move |c| load(c, token.catalog, token.root, &token.id))
                .await?;
            if item.kind != Kind::Publish
                || item.retained_bytes.is_some()
                || !matches!(item.phase, Phase::Prepared | Phase::Armed | Phase::Durable)
            {
                return Err(Error::Protected);
            }
            let selected = item.clone();
            this.fs(move |root, workspace| {
                let installed = target(root, &selected.path)?;
                let retained = entry(workspace, selected.workspace_entry.name())?;
                match proof.staged {
                    Some(expected) => {
                        if installed
                            .as_ref()
                            .map(fsutil::stat_identity)
                            .transpose()?
                            .is_some_and(|now| same_inode(expected, now))
                        {
                            return Err(Error::Protected);
                        }
                        let file = retained.ok_or(Error::Protected)?;
                        if fsutil::stat_identity(&file)? != expected
                            || !matches_body(&file, &selected)?
                        {
                            return Err(Error::Protected);
                        }
                        fsutil::unlink_file_at(
                            workspace,
                            selected.workspace_entry.name().as_ref(),
                        )?;
                    }
                    None => {
                        if retained.is_some() {
                            return Err(Error::Protected);
                        }
                        if let Some(file) = installed {
                            let now = fsutil::stat_identity(&file)?;
                            if !proof
                                .target
                                .is_some_and(|original| same_inode(original, now))
                                && matches_body(&file, &selected)?
                            {
                                return Err(Error::Protected);
                            }
                        }
                    }
                }
                workspace.sync_all()?;
                root.sync_all()?;
                Ok(())
            })
            .await?;
            this.finish_without_progress(item, Installation::Absent)
                .await
        })
        .await
    }

    /// Retires an obsolete but durably installed API copy, without claiming synchronized progress.
    pub async fn finish_obsolete_publish(&self, proof: Unpublished) -> Result<(), Error> {
        self.token(&proof.token)?;
        let this = self.clone();
        self.serial(async move {
            let token = proof.token.clone();
            let item = this
                .db(move |c| load(c, token.catalog, token.root, &token.id))
                .await?;
            if item.kind != Kind::Publish
                || item.retained_bytes.is_some()
                || !matches!(item.phase, Phase::Armed | Phase::Durable)
            {
                return Err(Error::Protected);
            }
            let expected = proof.staged.ok_or(Error::Protected)?;
            let selected = item.clone();
            this.fs(move |root, workspace| {
                if entry(workspace, selected.workspace_entry.name())?.is_some() {
                    return Err(Error::Protected);
                }
                let file = target(root, &selected.path)?.ok_or(Error::Protected)?;
                if !same_inode(expected, fsutil::stat_identity(&file)?)
                    || !matches_body(&file, &selected)?
                {
                    return Err(Error::Protected);
                }
                file.sync_all()?;
                parent(root, &selected.path, ParentSync::Sync)?;
                workspace.sync_all()?;
                root.sync_all()?;
                Ok(())
            })
            .await?;
            this.finish_without_progress(item, Installation::Installed)
                .await
        })
        .await
    }

    async fn finish_without_progress(
        &self,
        item: Pending,
        installation: Installation,
    ) -> Result<(), Error> {
        self.db(move |c| {
            let tx = c.transaction()?;
            let current = checked(&tx, &item.token)?;
            if current.retained_bytes.is_some() { return Err(Error::Protected); }
            if installation == Installation::Installed {
                match original(&tx, &current) {
                    Err(Error::Admin(AdminError::Precondition | AdminError::NotFound)) => {},
                    Ok(()) => return Err(stale()),
                    Err(error) => return Err(error),
                }
            }
            let mut usage = status(&tx, item.token.root)?;
            usage.entries = usage.entries.checked_sub(1).ok_or_else(stale)?;
            usage.reserved_bytes = usage.reserved_bytes.checked_sub(u128::from(current.reserved_bytes)).ok_or_else(stale)?;
            usage.recovery = usage.entries != 0;
            save_status(&tx, item.token.root, &usage)?;
            if installation == Installation::Installed {
                tx.execute("UPDATE sync_writeback_ops SET phase=4,revision=revision+1,io_failure=0 WHERE id=?1", [&item.token.id])?;
                tx.execute("DELETE FROM sync_writeback_ops WHERE phase=4 AND ordinal <= (SELECT coalesce(max(ordinal),0)-65536 FROM sync_writeback_ops) AND ordinal IN (SELECT ordinal FROM sync_writeback_ops WHERE phase=4 ORDER BY ordinal LIMIT 256)", [])?;
            } else {
                tx.execute("DELETE FROM sync_writeback_ops WHERE id=?1", [&item.token.id])?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }
}
