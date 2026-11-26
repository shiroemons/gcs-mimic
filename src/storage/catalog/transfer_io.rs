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

pub(super) fn stage_path(data_dir: &Path, id: &BlobId) -> PathBuf {
    id.staging_path(data_dir)
}

pub(super) enum InputReader {
    Disk(std::fs::File),
    Memory(crate::storage::memory::Reader),
}

impl InputReader {
    pub(super) fn short_derived_input(&self) -> ObjectError {
        // Keep the disk and memory readers' existing read_exact diagnostics.
        let message = match self {
            Self::Disk(_) => "early eof",
            Self::Memory(_) => "failed to fill whole buffer",
        };
        io::Error::new(io::ErrorKind::UnexpectedEof, message).into()
    }
}

impl Read for InputReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Disk(file) => file.read(bytes),
            Self::Memory(reader) => reader.read(bytes),
        }
    }
}

pub(super) enum Output<W = objects::StagedFile> {
    Disk(W),
    Memory(crate::storage::memory::Draft),
}

pub(super) type CopyOutput = Output<objects::BatchWriter>;

impl<W> Output<W> {
    pub(super) fn acknowledge(&mut self, offset: u64) {
        if let Self::Memory(draft) = self {
            draft.acknowledge(offset);
        }
    }
}

impl CopyOutput {
    pub(super) fn write(&mut self, bytes: &[u8]) -> Result<(), ObjectError> {
        match self {
            Self::Disk(writer) => writer.write(bytes),
            Self::Memory(draft) => draft.write(bytes).map_err(object_unavailable),
        }
    }

    pub(super) fn make_durable(&mut self) -> Result<(), ObjectError> {
        if let Self::Disk(writer) = self {
            writer.make_durable()?;
        }
        Ok(())
    }
}

impl Catalog {
    /// The caller holds the output's exclusive lease for the whole blocking job.
    pub(super) fn copy_range(
        &self,
        input: &mut InputReader,
        mut remaining: u64,
        buffer: &mut [u8],
        mut consume: impl FnMut(&[u8]) -> Result<(), ObjectError>,
        on_eof: impl FnOnce(&InputReader) -> ObjectError,
    ) -> Result<(), ObjectError> {
        while remaining != 0 {
            if self.0.admission.slots.is_closed() {
                return Err(object_unavailable("catalog is closing"));
            }
            let length =
                usize::try_from(remaining.min(buffer.len() as u64)).map_err(object_unavailable)?;
            let window = buffer
                .get_mut(..length)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
            let n = match input.read(window) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if n == 0 {
                return Err(on_eof(input));
            }
            consume(
                window
                    .get(..n)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?,
            )?;
            remaining -= n as u64;
        }
        Ok(())
    }
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
            .storage
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
            .storage
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

    pub(super) async fn rollback(&mut self, offset: u64) -> Result<objects::Rollback, ObjectError> {
        match self {
            Self::Disk(staged) => staged.rollback(offset).await,
            Self::Memory(_) => Ok(objects::Rollback::Deferred),
        }
    }

    pub(super) async fn into_hashes(self) -> Result<(Hashes, u64), ObjectError> {
        let staged = match self {
            Self::Memory(draft) => return draft.hashes().map_err(object_unavailable),
            Self::Disk(staged) => staged,
        };
        crate::task::run_blocking("resumable-rehash", move || {
            let file = staged.file()?;
            objects::with_copy_buffer(|buffer| {
                let mut hasher = Hasher::default();
                let mut offset = 0;
                loop {
                    let n = match crate::fsutil::read_at(file, buffer, offset) {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        result => result?,
                    };
                    if n == 0 {
                        return Ok((hasher.finish(), offset));
                    }
                    hasher.update(
                        buffer
                            .get(..n)
                            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?,
                    );
                    offset += n as u64;
                }
            })
        })
        .await?
    }
}

#[cfg(test)]
mod tests;
