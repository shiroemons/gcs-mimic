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
use tokio::io::{AsyncReadExt, AsyncSeekExt};

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
    Disk(objects::StagedFile),
    Memory(crate::storage::memory::Draft),
}

impl Output {
    pub(super) async fn open(
        catalog: &Catalog,
        id: &BlobId,
        offset: u64,
        lease: objects::StageLease,
    ) -> Result<Self, ObjectError> {
        catalog
            .0
            .backend
            .open_output(id, offset, ReadAccess::WriteOnly, lease)
            .await
    }

    pub(super) async fn open_readable(
        catalog: &Catalog,
        id: &BlobId,
        offset: u64,
        lease: objects::StageLease,
    ) -> Result<Self, ObjectError> {
        catalog
            .0
            .backend
            .open_output(id, offset, ReadAccess::ReadWrite, lease)
            .await
    }

    pub(super) async fn write(&mut self, bytes: &[u8]) -> Result<(), ObjectError> {
        match self {
            Self::Disk(staged) => staged.write(bytes).await,
            Self::Memory(draft) => draft.write(bytes).map_err(object_unavailable),
        }
    }

    pub(super) async fn settle_result<T, E: From<ObjectError>>(
        &mut self,
        result: Result<T, E>,
    ) -> Result<T, E> {
        match self {
            Self::Disk(staged) => staged.settle_result(result).await,
            Self::Memory(_) => result,
        }
    }

    pub(super) async fn finish(&mut self) -> Result<(), ObjectError> {
        match self {
            Self::Disk(staged) => staged.make_durable().await,
            Self::Memory(_) => Ok(()),
        }
    }

    pub(super) fn acknowledge(&mut self, offset: u64) {
        if let Self::Memory(draft) = self {
            draft.acknowledge(offset);
        }
    }

    pub(super) async fn rollback(&mut self, offset: u64) -> Result<(), ObjectError> {
        if let Self::Disk(staged) = self {
            staged.rollback(offset).await?;
        }
        Ok(())
    }

    pub(super) async fn into_hashes(self) -> Result<(Hashes, u64), ObjectError> {
        let mut file = match self {
            Self::Memory(draft) => return draft.hashes().map_err(object_unavailable),
            Self::Disk(staged) => tokio::fs::File::from_std(staged.into_file()?),
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
            hasher.update(
                buffer
                    .get(..n)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?,
            );
            length += n as u64;
        }
        Ok((hasher.finish(), length))
    }
}

#[cfg(test)]
mod tests;
