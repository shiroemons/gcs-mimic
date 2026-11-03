use super::*;

#[derive(Clone)]
pub(super) enum Data {
    Disk {
        file: Arc<fs::File>,
        path: PathBuf,
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
        fs::create_dir_all(&directory)?;
        let path = data_path(root, id);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        #[cfg(unix)]
        crate::fsutil::sync_dir(&directory)?;
        Ok(Self::Disk {
            file: Arc::new(file),
            path,
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
        file.sync_data()?;
        Ok(Self::Disk {
            file: Arc::new(file),
            path,
        })
    }

    pub(super) fn write(&self, start: u64, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Disk { file, .. } => {
                let mut file = &**file;
                file.seek(io::SeekFrom::Start(start))?;
                file.write_all(bytes)
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
            Self::Disk { file, .. } => file.sync_data().map(|()| None),
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
