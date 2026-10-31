use super::*;

#[derive(Clone)]
pub(super) enum Data {
    Disk(Arc<fs::File>),
    Memory {
        store: Arc<crate::storage::memory::Store>,
        id: String,
    },
}

impl Data {
    pub(super) fn create(catalog: &Catalog, id: &str, path: &Path) -> io::Result<Self> {
        if let Some(store) = &catalog.0.memory {
            return Ok(Self::Memory {
                store: store.clone(),
                id: id.into(),
            });
        }
        let directory = catalog.0.data_dir.join("appendable");
        fs::create_dir_all(&directory)?;
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        #[cfg(unix)]
        crate::fsutil::sync_dir(&directory)?;
        Ok(Self::Disk(Arc::new(file)))
    }

    pub(super) fn take_over(
        catalog: &Catalog,
        id: &str,
        path: &Path,
        size: u64,
    ) -> Result<Self, ObjectError> {
        if let Some(store) = &catalog.0.memory {
            let mut draft = store.draft(id, size).map_err(object_unavailable)?;
            draft.acknowledge(size);
            return Ok(Self::Memory {
                store: store.clone(),
                id: id.into(),
            });
        }
        let file = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(object_unavailable)?;
        if file.metadata().map_err(object_unavailable)?.len() < size {
            return Err(ObjectError::Corrupt(
                "appendable data is shorter than its persisted size".into(),
            ));
        }
        file.set_len(size).map_err(object_unavailable)?;
        file.sync_data().map_err(object_unavailable)?;
        Ok(Self::Disk(Arc::new(file)))
    }

    pub(super) fn write(&self, start: u64, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Disk(file) => {
                let mut file = &**file;
                file.seek(io::SeekFrom::Start(start))?;
                file.write_all(bytes)
            }
            Self::Memory { store, id } => {
                let mut draft = store.draft(id, start)?;
                draft.write(bytes)?;
                draft.acknowledge(start + bytes.len() as u64);
                Ok(())
            }
        }
    }

    pub(super) fn sync(&self) -> io::Result<Option<crate::storage::memory::Checkpoint>> {
        match self {
            Self::Disk(file) => file.sync_data().map(|()| None),
            Self::Memory { store, id } => store.checkpoint_draft(id).map(Some),
        }
    }

    pub(super) fn reader(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        match self {
            Self::Disk(_) => Ok(Box::new(fs::File::open(path)?)),
            Self::Memory { store, id } => Ok(Box::new(store.open(id)?)),
        }
    }
}

pub(super) fn remove(catalog: &Catalog, id: &str) -> Result<(), ObjectError> {
    if let Some(store) = &catalog.0.memory {
        store.remove_draft(id).map_err(object_unavailable)?;
        store.remove(id).map_err(object_unavailable)
    } else {
        unlink(&data_path(&catalog.0.data_dir, id)?).map_err(object_unavailable)
    }
}
