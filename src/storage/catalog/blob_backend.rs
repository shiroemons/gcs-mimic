#[cfg(test)]
use super::fault::hit_io;
use super::objects::{self, ReaderData, UploadSink};
use super::transfer_io::{CopyOutput, InputReader, Output};
use crate::storage::{
    BlobId,
    memory::{BlobError, Store},
};
use crate::{object::ObjectError, storage::failure::object_unavailable};
use std::{
    fs,
    io::{self, Seek},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(all(test, unix))]
mod durability_tests;

#[derive(Clone, Copy)]
pub(super) enum ReadAccess {
    WriteOnly,
    ReadWrite,
}

#[derive(Clone)]
pub(super) enum BlobBackend {
    Disk(PathBuf),
    Memory(Arc<Store>),
}

impl BlobBackend {
    pub(super) fn reclaim_batch(
        &self,
        ids: Vec<BlobId>,
    ) -> Result<(Vec<BlobId>, Option<ObjectError>), ObjectError> {
        let root = match self {
            Self::Memory(_) => {
                for id in &ids {
                    self.remove_draft(id)?;
                    self.remove(id)?;
                }
                return Ok((ids, None));
            }
            Self::Disk(root) => root,
        };
        let mut shards = std::collections::BTreeMap::<PathBuf, Vec<BlobId>>::new();
        let mut failure = None;
        for id in ids {
            let path = id.blob_path(root);
            let shard = objects::blob_shard(&path)?.to_owned();
            #[cfg(test)]
            let result = hit_io(root, super::fault::Step::ReclaimUnlink)
                .and_then(|()| objects::unlink(&path));
            #[cfg(not(test))]
            let result = objects::unlink(&path);
            match result {
                Ok(()) => shards.entry(shard).or_default().push(id),
                Err(error) => {
                    crate::diagnostics::emit(
                        crate::diagnostics::LogLevel::Warn,
                        crate::diagnostics::Event::BlobReclaimDeferred,
                        serde_json::json!({"message":format!("deferred blob reclaim {id}: {error}")}),
                    );
                    failure = Some(object_unavailable(error));
                }
            }
        }
        let mut removed = Vec::new();
        for (shard, ids) in shards {
            #[cfg(unix)]
            if shard.exists() {
                #[cfg(test)]
                let result = hit_io(root, super::fault::Step::ReclaimSync)
                    .and_then(|()| objects::sync_dir(&shard));
                #[cfg(not(test))]
                let result = objects::sync_dir(&shard);
                if let Err(error) = result {
                    crate::diagnostics::emit(
                        crate::diagnostics::LogLevel::Warn,
                        crate::diagnostics::Event::BlobReclaimDeferred,
                        serde_json::json!({"message":format!("deferred blob reclaim directory: {error}")}),
                    );
                    failure = Some(object_unavailable(error));
                    continue;
                }
            }
            #[cfg(not(unix))]
            let _ = shard;
            removed.extend(ids);
        }
        Ok((removed, failure))
    }

    #[cfg(test)]
    pub(super) fn memory(&self) -> Option<&Arc<Store>> {
        match self {
            Self::Disk(_) => None,
            Self::Memory(store) => Some(store),
        }
    }

    pub(super) fn kind(&self) -> crate::config::Backend {
        match self {
            Self::Disk(_) => crate::config::Backend::Disk,
            Self::Memory(_) => crate::config::Backend::Memory,
        }
    }

    pub(super) fn is_memory(&self) -> bool {
        matches!(self, Self::Memory(_))
    }

    pub(super) fn is_healthy(&self) -> bool {
        match self {
            Self::Disk(_) => true,
            Self::Memory(store) => store.is_healthy(),
        }
    }

    pub(super) fn checkpoint_draft(
        &self,
        id: &BlobId,
    ) -> Result<Option<crate::storage::memory::Checkpoint>, BlobError> {
        match self {
            Self::Disk(_) => Ok(None),
            Self::Memory(store) => store.create_draft_checkpoint(&id.to_hex()).map(Some),
        }
    }

    pub(super) fn retain_memory_ids(
        &self,
        selected: &std::collections::HashSet<&str>,
    ) -> Result<(), BlobError> {
        match self {
            Self::Disk(_) => Ok(()),
            Self::Memory(store) => store.retain_ids(selected),
        }
    }

    pub(super) fn clear_memory(&self) -> Result<(), BlobError> {
        match self {
            Self::Disk(_) => Ok(()),
            Self::Memory(store) => store.clear(),
        }
    }

    pub(super) fn pin(&self, original: &BlobId, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Disk(root) => {
                #[cfg(test)]
                hit_io(root, super::fault::Step::CopyLink)?;
                fs::hard_link(original.blob_path(root), id.staging_path(root))?;
                #[cfg(all(test, unix))]
                super::fault::file_created(&id.staging_path(root));
                Ok(())
            }
            Self::Memory(store) => store.pin(&original.to_hex(), id.to_hex()),
        }
    }

    /// A publication pin has its own ID so recovery can never truncate a shared inode as a draft.
    pub(super) fn pin_staged(&self, source: &BlobId, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Disk(root) => {
                #[cfg(test)]
                hit_io(root, super::fault::Step::CopyLink)?;
                fs::hard_link(source.staging_path(root), id.staging_path(root))?;
                #[cfg(all(test, unix))]
                super::fault::file_created(&id.staging_path(root));
                Ok(())
            }
            Self::Memory(store) => store.pin(&source.to_hex(), id.to_hex()),
        }
    }

    /// RAM pins are already published; a disk pin is a staged link to rename into place.
    pub(super) fn publish_pinned(&self, id: &BlobId) -> Result<(), BlobError> {
        if let Self::Disk(root) = self {
            objects::publish_blob(&id.staging_path(root), &id.blob_path(root))?;
        }
        Ok(())
    }

    pub(super) fn remove(&self, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Disk(root) => objects::unlink(&id.blob_path(root)).map_err(Into::into),
            Self::Memory(store) => store.remove(&id.to_hex()),
        }
    }

    pub(super) fn remove_draft(&self, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Disk(root) => objects::unlink(&id.staging_path(root)).map_err(Into::into),
            Self::Memory(store) => store.remove_draft(&id.to_hex()),
        }
    }

    // RAM pins are immutable aliases; removing a draft would leave their shared bytes retained.
    pub(super) fn remove_pin(&self, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Disk(root) => objects::unlink(&id.staging_path(root)).map_err(Into::into),
            Self::Memory(store) => store.remove(&id.to_hex()),
        }
    }

    // Publication renamed disk drafts, while RAM still owns a separate mutable draft reference.
    pub(super) fn finish_draft(&self, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Disk(_) => Ok(()),
            Self::Memory(_) => self.remove_draft(id),
        }
    }

    pub(super) fn publish_draft(&self, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Disk(root) => objects::publish_blob(&id.staging_path(root), &id.blob_path(root)),
            Self::Memory(store) => store.publish_draft(&id.to_hex()),
        }
    }

    pub(super) async fn publish_resumable(
        &self,
        catalog: &super::Catalog,
        id: BlobId,
    ) -> Result<(), ObjectError> {
        match self {
            Self::Disk(root) => {
                catalog
                    .publish_staged(id.staging_path(root), id.blob_path(root))
                    .await
            }
            Self::Memory(_) => {
                let backend = self.clone();
                catalog
                    .run_upload(move |_| backend.publish_draft(&id).map_err(object_unavailable))
                    .await
            }
        }
    }

    #[cfg(unix)]
    pub(super) fn sync_blob_dir(&self, id: &BlobId) -> io::Result<()> {
        if let Self::Disk(root) = self
            && let Some(shard) = id.blob_path(root).parent()
            && shard.exists()
        {
            #[cfg(test)]
            hit_io(root, super::fault::Step::AdminCleanupSync)?;
            objects::sync_dir(shard)?;
        }
        Ok(())
    }

    pub(super) fn restore_draft(&self, id: &BlobId) -> io::Result<()> {
        if let Self::Disk(root) = self {
            restore_disk_draft(root, id)?;
        }
        Ok(())
    }

    pub(super) fn remove_appendable(&self, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Disk(root) => {
                objects::unlink(&root.join("appendable").join(id.to_hex())).map_err(Into::into)
            }
            Self::Memory(_) => {
                self.remove_draft(id)?;
                self.remove(id)
            }
        }
    }

    pub(super) fn open_appendable(&self, id: &BlobId) -> Result<ReaderData, BlobError> {
        match self {
            Self::Disk(root) => fs::File::open(root.join("appendable").join(id.to_hex()))
                .map(Arc::new)
                .map(ReaderData::Disk)
                .map_err(Into::into),
            Self::Memory(_) => self.open(id),
        }
    }

    pub(super) fn open(&self, id: &BlobId) -> Result<ReaderData, BlobError> {
        match self {
            Self::Disk(root) => fs::File::open(id.blob_path(root))
                .map(Arc::new)
                .map(ReaderData::Disk)
                .map_err(Into::into),
            Self::Memory(store) => store.open(&id.to_hex()).map(ReaderData::Memory),
        }
    }

    pub(super) async fn stage(
        &self,
        id: &BlobId,
        lease: objects::StageLease,
    ) -> Result<UploadSink, crate::object::ObjectError> {
        match self {
            Self::Disk(root) => Ok(UploadSink::Disk(
                objects::create_upload_stage(
                    id.staging_path(root),
                    lease,
                    #[cfg(test)]
                    root.clone(),
                )
                .await?,
            )),
            Self::Memory(store) => Ok(UploadSink::Memory(store.stage()?)),
        }
    }

    pub(super) fn stage_copy_output(
        &self,
        id: &BlobId,
        lease: objects::StageLease,
    ) -> Result<CopyOutput, ObjectError> {
        match self {
            Self::Disk(root) => Ok(Output::Disk(objects::BatchWriter::create(
                id.staging_path(root),
                lease,
                #[cfg(test)]
                root,
            )?)),
            Self::Memory(store) => {
                self.create_draft(id)?;
                Ok(Output::Memory(store.draft(&id.to_hex(), 0)?))
            }
        }
    }

    #[cfg(unix)]
    #[cfg_attr(test, track_caller)]
    pub(super) fn sync_staging_dir(&self) -> io::Result<()> {
        match self {
            Self::Disk(root) => objects::sync_dir(&root.join("staging")),
            Self::Memory(_) => Ok(()),
        }
    }

    pub(super) fn create_draft(&self, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Memory(store) => store.create_draft(id.to_hex()),
            Self::Disk(root) => {
                let stage = id.staging_path(root);
                #[cfg(test)]
                super::fault::record_catalog_thread_io(&stage);
                Ok(fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(stage)
                    .and_then(|file| file.sync_all())?)
            }
        }
    }

    pub(super) fn create_resumable_draft(&self, id: &BlobId) -> Result<(), BlobError> {
        let root = match self {
            Self::Disk(root) => root,
            Self::Memory(store) => return store.create_resumable_draft(id.to_hex()),
        };
        let path = id.staging_path(root);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        #[cfg(all(test, unix))]
        super::fault::file_created(&path);
        if let Err(error) = sync_new_draft(
            &file,
            #[cfg(any(test, unix))]
            root,
        ) {
            drop(file);
            fs::remove_file(path)?;
            return Err(error.into());
        }
        Ok(())
    }

    pub(super) fn remove_new_draft(&self, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Disk(root) => fs::remove_file(id.staging_path(root)).map_err(Into::into),
            Self::Memory(store) => store.remove_draft(&id.to_hex()),
        }
    }

    pub(super) fn remove_draft_durable(&self, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Disk(root) => unlink_synced(
                &id.staging_path(root),
                #[cfg(all(test, unix))]
                root,
            )
            .map_err(Into::into),
            Self::Memory(_) => self.remove_draft(id),
        }
    }

    pub(super) fn remove_durable(&self, id: &BlobId) -> Result<(), BlobError> {
        match self {
            Self::Disk(root) => unlink_synced(
                &id.blob_path(root),
                #[cfg(all(test, unix))]
                root,
            )
            .map_err(Into::into),
            Self::Memory(_) => self.remove(id),
        }
    }

    pub(super) fn open_input(
        &self,
        id: &BlobId,
        start: u64,
    ) -> Result<InputReader, crate::object::ObjectError> {
        match self {
            Self::Memory(store) => {
                let mut reader = store.open(&id.to_hex())?;
                reader.seek(io::SeekFrom::Start(start))?;
                Ok(InputReader::Memory(reader))
            }
            Self::Disk(root) => {
                let mut file = fs::File::open(id.staging_path(root))?;
                file.seek(io::SeekFrom::Start(start))?;
                Ok(InputReader::Disk(file))
            }
        }
    }

    pub(super) async fn open_output(
        &self,
        id: &BlobId,
        offset: u64,
        read: ReadAccess,
        lease: objects::StageLease,
    ) -> Result<Output, crate::object::ObjectError> {
        match self {
            Self::Memory(store) => Ok(Output::Memory(store.draft(&id.to_hex(), offset)?)),
            Self::Disk(root) => {
                let root = root.clone();
                let id = *id;
                crate::task::run_blocking("open-staged-output", move || {
                    open_staged_output(&root, &id, offset, read, lease)
                        .map(|writer| Output::Disk(writer.into()))
                })
                .await?
            }
        }
    }

    pub(super) fn open_copy_output(
        &self,
        id: &BlobId,
        offset: u64,
        lease: objects::StageLease,
    ) -> Result<CopyOutput, ObjectError> {
        match self {
            Self::Memory(store) => Ok(Output::Memory(store.draft(&id.to_hex(), offset)?)),
            Self::Disk(root) => {
                open_staged_output(root, id, offset, ReadAccess::WriteOnly, lease).map(Output::Disk)
            }
        }
    }
}

fn open_staged_output(
    root: &Path,
    id: &BlobId,
    offset: u64,
    read: ReadAccess,
    lease: objects::StageLease,
) -> Result<objects::BatchWriter, ObjectError> {
    let mut file = fs::OpenOptions::new()
        .read(matches!(read, ReadAccess::ReadWrite))
        .write(true)
        .open(id.staging_path(root))?;
    if file.metadata()?.len() < offset {
        return Err(object_unavailable("acknowledged upload prefix is missing"));
    }
    file.set_len(offset)?;
    #[cfg(test)]
    super::fault::truncated(root, &file.metadata()?);
    file.seek(io::SeekFrom::Start(offset))?;
    Ok(objects::BatchWriter::new(
        file,
        lease,
        #[cfg(test)]
        root,
    ))
}

pub(super) fn restore_disk_draft(root: &Path, id: &BlobId) -> io::Result<()> {
    let stage = id.staging_path(root);
    if !stage.try_exists()? {
        fs::rename(id.blob_path(root), stage)?;
    }
    Ok(())
}

/// Makes a new resumable draft durable: the file first, then its entry in the staging directory.
fn sync_new_draft(file: &fs::File, #[cfg(any(test, unix))] root: &Path) -> io::Result<()> {
    #[cfg(test)]
    super::fault::record_catalog_thread_io(root);
    file.sync_all()?;
    #[cfg(test)]
    super::fault::file_synced(root, &file.metadata()?);
    #[cfg(unix)]
    {
        #[cfg(test)]
        hit_io(root, super::fault::Step::SessionCreateSync)?;
        objects::sync_dir(&root.join("staging"))?;
    }
    Ok(())
}

fn unlink_synced(path: &Path, #[cfg(all(test, unix))] data_dir: &Path) -> io::Result<()> {
    objects::unlink(path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && parent.exists()
    {
        #[cfg(test)]
        hit_io(data_dir, super::fault::Step::SessionCleanupSync)?;
        objects::sync_dir(parent)?;
        #[cfg(test)]
        hit_io(data_dir, super::fault::Step::SessionCleanupSynced)?;
    }
    Ok(())
}
