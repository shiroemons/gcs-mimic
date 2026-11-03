use super::*;

#[derive(Clone)]
pub(super) enum Data {
    Disk {
        file: Arc<fs::File>,
        path: PathBuf,
        #[cfg(test)]
        data_dir: PathBuf,
    },
    Memory {
        store: Arc<crate::storage::memory::Store>,
        id: BlobId,
    },
}

impl Data {
    pub(super) fn create(backend: &super::super::BlobBackend, id: &BlobId) -> io::Result<Self> {
        let root = match backend {
            super::super::BlobBackend::Disk(root) => root,
            super::super::BlobBackend::Memory(store) => {
                return Ok(Self::Memory {
                    store: store.clone(),
                    id: *id,
                });
            }
        };
        let directory = root.join("appendable");
        let path = data_path(root, id);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        #[cfg(all(test, unix))]
        super::super::fault::file_created(&path);
        #[cfg(unix)]
        super::super::objects::sync_dir(&directory)?;
        Ok(Self::Disk {
            file: Arc::new(file),
            path,
            #[cfg(test)]
            data_dir: root.clone(),
        })
    }

    pub(super) fn take_over(
        backend: &super::super::BlobBackend,
        id: &BlobId,
        size: u64,
    ) -> Result<Self, ObjectError> {
        let root = match backend {
            super::super::BlobBackend::Disk(root) => root,
            super::super::BlobBackend::Memory(store) => {
                let mut draft = store.draft(&id.to_hex(), size)?;
                draft.acknowledge(size);
                return Ok(Self::Memory {
                    store: store.clone(),
                    id: *id,
                });
            }
        };
        let path = data_path(root, id);
        let file = fs::OpenOptions::new().write(true).open(&path)?;
        if file.metadata()?.len() < size {
            return Err(ObjectError::Corrupt(
                "appendable data is shorter than its persisted size".into(),
            ));
        }
        file.set_len(size)?;
        #[cfg(test)]
        super::super::fault::truncated(root, &file.metadata()?);
        file.sync_data()?;
        #[cfg(test)]
        super::super::fault::file_synced(root, &file.metadata()?);
        Ok(Self::Disk {
            file: Arc::new(file),
            path,
            #[cfg(test)]
            data_dir: root.clone(),
        })
    }

    pub(super) fn write(&self, start: u64, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Disk { file, .. } => {
                #[cfg(unix)]
                {
                    std::os::unix::fs::FileExt::write_all_at(&**file, bytes, start)
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::FileExt;
                    let mut bytes = bytes;
                    let mut offset = start;
                    while !bytes.is_empty() {
                        let written = file.seek_write(bytes, offset)?;
                        if written == 0 {
                            return Err(io::ErrorKind::WriteZero.into());
                        }
                        bytes = &bytes[written..];
                        offset += written as u64;
                    }
                    Ok(())
                }
            }
            Self::Memory { store, id } => {
                let mut draft = store.draft(&id.to_hex(), start)?;
                draft.write(bytes)?;
                draft.acknowledge(start + bytes.len() as u64);
                Ok(())
            }
        }
    }

    pub(super) fn sync(&self) -> io::Result<Option<crate::storage::memory::Checkpoint>> {
        match self {
            Self::Disk {
                file,
                #[cfg(test)]
                data_dir,
                ..
            } => {
                file.sync_data()?;
                #[cfg(test)]
                super::super::fault::file_synced(data_dir, &file.metadata()?);
                Ok(None)
            }
            Self::Memory { store, id } => store.checkpoint_draft(&id.to_hex()).map(Some),
        }
    }

    pub(super) fn reader(&self) -> io::Result<Box<dyn Read + Send>> {
        match self {
            Self::Disk { path, .. } => Ok(Box::new(fs::File::open(path)?)),
            Self::Memory { store, id } => Ok(Box::new(store.open(&id.to_hex())?)),
        }
    }
}

pub(super) fn remove(catalog: &Catalog, id: &BlobId) -> Result<(), ObjectError> {
    catalog
        .0
        .backend
        .remove_appendable(id)
        .map_err(object_unavailable)
}
