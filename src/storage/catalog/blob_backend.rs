use super::objects::{self, ReaderData, UploadSink};
use super::transfer_io::{InputReader, Output};
use crate::storage::{BlobId, memory::Store};
use crate::{object::ObjectError, storage::failure::object_unavailable};
use std::{
    fs,
    io::{self, Seek},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::AsyncSeekExt;

#[cfg(all(test, unix))]
mod durability_tests;

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
            match objects::unlink(&path) {
                Ok(()) => shards.entry(shard).or_default().push(id),
                Err(error) => {
                    crate::diagnostics::emit(
                        crate::diagnostics::LogLevel::Warn,
                        "blob_reclaim_deferred",
                        serde_json::json!({"message":format!("deferred blob reclaim {id}: {error}")}),
                    );
                    failure = Some(object_unavailable(error));
                }
            }
        }
        let mut removed = Vec::new();
        for (shard, ids) in shards {
            #[cfg(unix)]
            if shard.exists()
                && let Err(error) = objects::sync_dir(&shard)
            {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    "blob_reclaim_deferred",
                    serde_json::json!({"message":format!("deferred blob reclaim directory: {error}")}),
                );
                failure = Some(object_unavailable(error));
                continue;
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

    pub(super) fn discard_unreferenced(
        &self,
        connection: &rusqlite::Connection,
        id: &BlobId,
    ) -> Result<(), crate::object::ObjectError> {
        if !objects::blob_referenced(connection, id)? {
            match self {
                Self::Disk(root) => objects::reclaim(connection, root, id)?,
                Self::Memory(_) => self.remove(id)?,
            }
        }
        Ok(())
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
    ) -> io::Result<Option<crate::storage::memory::Checkpoint>> {
        match self {
            Self::Disk(_) => Ok(None),
            Self::Memory(store) => store.create_draft_checkpoint(&id.to_hex()).map(Some),
        }
    }

    pub(super) fn retain_memory_ids(
        &self,
        selected: &std::collections::HashSet<&str>,
    ) -> io::Result<()> {
        match self {
            Self::Disk(_) => Ok(()),
            Self::Memory(store) => store.retain_ids(selected),
        }
    }

    pub(super) fn clear_memory(&self) -> io::Result<()> {
        match self {
            Self::Disk(_) => Ok(()),
            Self::Memory(store) => store.clear(),
        }
    }

    pub(super) fn pin(&self, original: &BlobId, id: &BlobId) -> io::Result<()> {
        match self {
            Self::Disk(root) => fs::hard_link(original.blob_path(root), id.staging_path(root)),
            Self::Memory(store) => store.pin(&original.to_hex(), id.to_hex()),
        }
    }

    pub(super) fn publish_pin(&self, original: &BlobId, id: &BlobId) -> io::Result<()> {
        self.pin(original, id)?;
        if let Self::Disk(_) = self {
            self.publish_draft(id)?;
        }
        Ok(())
    }

    pub(super) fn remove(&self, id: &BlobId) -> io::Result<()> {
        match self {
            Self::Disk(root) => objects::unlink(&id.blob_path(root)),
            Self::Memory(store) => store.remove(&id.to_hex()),
        }
    }

    pub(super) fn remove_draft(&self, id: &BlobId) -> io::Result<()> {
        match self {
            Self::Disk(root) => objects::unlink(&id.staging_path(root)),
            Self::Memory(store) => store.remove_draft(&id.to_hex()),
        }
    }

    // RAM pins are immutable aliases; removing a draft would leave their shared bytes retained.
    pub(super) fn remove_pin(&self, id: &BlobId) -> io::Result<()> {
        match self {
            Self::Disk(root) => objects::unlink(&id.staging_path(root)),
            Self::Memory(store) => store.remove(&id.to_hex()),
        }
    }

    // Publication renamed disk drafts, while RAM still owns a separate mutable draft reference.
    pub(super) fn finish_draft(&self, id: &BlobId) -> io::Result<()> {
        match self {
            Self::Disk(_) => Ok(()),
            Self::Memory(_) => self.remove_draft(id),
        }
    }

    pub(super) fn publish_draft(&self, id: &BlobId) -> io::Result<()> {
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

    pub(super) fn remove_appendable(&self, id: &BlobId) -> io::Result<()> {
        match self {
            Self::Disk(root) => objects::unlink(&root.join("appendable").join(id.to_hex())),
            Self::Memory(_) => {
                self.remove_draft(id)?;
                self.remove(id)
            }
        }
    }

    pub(super) fn open_appendable(&self, id: &BlobId) -> io::Result<ReaderData> {
        match self {
            Self::Disk(root) => fs::File::open(root.join("appendable").join(id.to_hex()))
                .map(tokio::fs::File::from_std)
                .map(ReaderData::Disk),
            Self::Memory(_) => self.open(id),
        }
    }

    pub(super) fn open(&self, id: &BlobId) -> io::Result<ReaderData> {
        match self {
            Self::Disk(root) => fs::File::open(id.blob_path(root))
                .map(tokio::fs::File::from_std)
                .map(ReaderData::Disk),
            Self::Memory(store) => store.open(&id.to_hex()).map(ReaderData::Memory),
        }
    }

    pub(super) async fn stage(
        &self,
        id: &BlobId,
    ) -> Result<UploadSink, crate::object::ObjectError> {
        match self {
            Self::Disk(root) => {
                objects::create_upload_stage(id.staging_path(root), id.blob_path(root))
                    .await
                    .map(tokio::fs::File::from_std)
                    .map(UploadSink::Disk)
            }
            Self::Memory(store) => Ok(UploadSink::Memory(store.stage()?)),
        }
    }

    pub(super) async fn stage_output(
        &self,
        id: &BlobId,
    ) -> Result<Output, crate::object::ObjectError> {
        match self {
            Self::Disk(root) => Ok(Output::Disk(tokio::fs::File::from_std(
                objects::create_upload_stage(id.staging_path(root), id.blob_path(root)).await?,
            ))),
            Self::Memory(_) => {
                self.create_draft(id)?;
                self.open_output(id, 0, ReadAccess::WriteOnly).await
            }
        }
    }

    #[cfg(unix)]
    pub(super) fn sync_staging_dir(&self) -> io::Result<()> {
        match self {
            Self::Disk(root) => objects::sync_dir(&root.join("staging")),
            Self::Memory(_) => Ok(()),
        }
    }

    pub(super) fn create_draft(&self, id: &BlobId) -> io::Result<()> {
        match self {
            Self::Memory(store) => store.create_draft(id.to_hex()),
            Self::Disk(root) => fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(id.staging_path(root))
                .and_then(|file| file.sync_all()),
        }
    }

    pub(super) fn create_resumable_draft(&self, id: &BlobId) -> io::Result<()> {
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
        let synced = (|| -> io::Result<()> {
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
        })();
        if let Err(error) = synced {
            drop(file);
            fs::remove_file(path)?;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn remove_new_draft(&self, id: &BlobId) -> io::Result<()> {
        match self {
            Self::Disk(root) => fs::remove_file(id.staging_path(root)),
            Self::Memory(store) => store.remove_draft(&id.to_hex()),
        }
    }

    pub(super) fn remove_draft_durable(&self, id: &BlobId) -> io::Result<()> {
        match self {
            Self::Disk(root) => unlink_synced(
                &id.staging_path(root),
                #[cfg(all(test, unix))]
                root,
            ),
            Self::Memory(_) => self.remove_draft(id),
        }
    }

    pub(super) fn remove_durable(&self, id: &BlobId) -> io::Result<()> {
        match self {
            Self::Disk(root) => unlink_synced(
                &id.blob_path(root),
                #[cfg(all(test, unix))]
                root,
            ),
            Self::Memory(_) => self.remove(id),
        }
    }

    pub(super) async fn open_input(
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
                let mut file = tokio::fs::File::open(id.staging_path(root)).await?;
                file.seek(io::SeekFrom::Start(start)).await?;
                Ok(InputReader::Disk(file))
            }
        }
    }

    pub(super) async fn open_output(
        &self,
        id: &BlobId,
        offset: u64,
        read: ReadAccess,
    ) -> Result<Output, crate::object::ObjectError> {
        match self {
            Self::Memory(store) => Ok(Output::Memory(store.draft(&id.to_hex(), offset)?)),
            Self::Disk(root) => {
                let mut file = tokio::fs::OpenOptions::new()
                    .read(matches!(read, ReadAccess::ReadWrite))
                    .write(true)
                    .open(id.staging_path(root))
                    .await?;
                if file.metadata().await?.len() < offset {
                    return Err(crate::storage::failure::object_unavailable(
                        "acknowledged upload prefix is missing",
                    ));
                }
                file.set_len(offset).await?;
                #[cfg(test)]
                super::fault::truncated(root, &file.metadata().await?);
                file.seek(io::SeekFrom::Start(offset)).await?;
                Ok(Output::Disk(file))
            }
        }
    }
}

pub(super) fn restore_disk_draft(root: &Path, id: &BlobId) -> io::Result<()> {
    let stage = id.staging_path(root);
    if !stage.try_exists()? {
        fs::rename(id.blob_path(root), stage)?;
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

#[cfg(all(test, unix))]
fn hit_io(data_dir: &Path, step: super::fault::Step) -> io::Result<()> {
    super::fault::hit(data_dir, step).map_err(|error| match error {
        super::fault::Injected::Io(error) => error,
        super::fault::Injected::Sqlite(error) => io::Error::other(error),
    })
}
