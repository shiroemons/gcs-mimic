use super::blob_backend::ReadAccess;
use super::{Catalog, objects};
use crate::storage::blob_id::BlobId;
use crate::{
    hashing::{Hasher, Hashes},
    object::ObjectError,
    storage::failure::object_unavailable,
};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

#[cfg(test)]
use super::fault;

pub(super) fn stage_path(data_dir: &Path, id: &BlobId) -> PathBuf {
    id.staging_path(data_dir)
}

pub(super) enum InputReader {
    Disk(tokio::fs::File),
    Memory(crate::storage::memory::Reader),
}

impl InputReader {
    pub(super) async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, ObjectError> {
        match self {
            Self::Disk(file) => file.read(bytes).await.map_err(object_unavailable),
            Self::Memory(reader) => reader.read(bytes).map_err(object_unavailable),
        }
    }
    pub(super) async fn open(
        catalog: &Catalog,
        id: &BlobId,
        start: u64,
    ) -> Result<Self, ObjectError> {
        catalog.0.backend.open_input(id, start).await
    }

    pub(super) async fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), ObjectError> {
        match self {
            Self::Disk(file) => {
                file.read_exact(bytes).await?;
            }
            Self::Memory(reader) => reader.read_exact(bytes)?,
        }
        Ok(())
    }
}

pub(super) enum Output {
    Disk(tokio::fs::File),
    Memory(crate::storage::memory::Draft),
}

impl Output {
    pub(super) async fn open(
        catalog: &Catalog,
        id: &BlobId,
        offset: u64,
    ) -> Result<Self, ObjectError> {
        Self::open_with_access(catalog, id, offset, ReadAccess::WriteOnly).await
    }

    pub(super) async fn open_readable(
        catalog: &Catalog,
        id: &BlobId,
        offset: u64,
    ) -> Result<Self, ObjectError> {
        Self::open_with_access(catalog, id, offset, ReadAccess::ReadWrite).await
    }

    async fn open_with_access(
        catalog: &Catalog,
        id: &BlobId,
        offset: u64,
        read: ReadAccess,
    ) -> Result<Self, ObjectError> {
        catalog.0.backend.open_output(id, offset, read).await
    }

    pub(super) async fn write(&mut self, bytes: &[u8]) -> Result<(), ObjectError> {
        match self {
            Self::Disk(file) => file.write_all(bytes).await.map_err(object_unavailable),
            Self::Memory(draft) => draft.write(bytes).map_err(object_unavailable),
        }
    }

    pub(super) async fn finish(&mut self, catalog: &Catalog) -> Result<(), ObjectError> {
        if let Self::Disk(file) = self {
            objects::make_durable(
                file,
                #[cfg(test)]
                &catalog.0.data_dir,
            )
            .await?;
        }
        #[cfg(not(test))]
        let _ = catalog;
        Ok(())
    }

    pub(super) fn acknowledge(&mut self, offset: u64) {
        if let Self::Memory(draft) = self {
            draft.acknowledge(offset);
        }
    }

    pub(super) async fn rollback(
        &mut self,
        catalog: &Catalog,
        offset: u64,
    ) -> Result<(), ObjectError> {
        if let Self::Disk(file) = self {
            file.set_len(offset).await?;
            #[cfg(test)]
            fault::truncated(&catalog.0.data_dir, &file.metadata().await?);
        }
        // Tokio retains a failed background write across set_len; flush must observe it before sync.
        self.finish(catalog).await
    }

    pub(super) async fn hashes(&mut self) -> Result<(Hashes, u64), ObjectError> {
        let file = match self {
            Self::Memory(draft) => return draft.hashes().map_err(object_unavailable),
            Self::Disk(file) => file,
        };
        file.seek(io::SeekFrom::Start(0)).await?;
        let mut hasher = Hasher::default();
        let mut length = 0_u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            let n = file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            length += n as u64;
        }
        Ok((hasher.finish(), length))
    }
}

#[cfg(test)]
mod tests;
