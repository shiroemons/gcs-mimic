use crate::bucket::CatalogError;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::Path,
};

pub(super) struct DirectoryLock(Option<File>);

impl DirectoryLock {
    pub(super) fn release(mut self) -> io::Result<()> {
        self.0.take().expect("owned directory lock").unlock()
    }
}

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        // Closing alone leaves flock held by descriptors inherited between fork and exec.
        if let Some(file) = &self.0
            && let Err(error) = file.unlock()
        {
            eprintln!("release data directory lock: {error}");
        }
    }
}

pub(super) fn acquire(data_dir: &Path) -> Result<DirectoryLock, CatalogError> {
    fs::create_dir_all(data_dir).map_err(|e| {
        CatalogError::Unavailable(format!("create data directory {}: {e}", data_dir.display()))
    })?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join("catalog.lock"))
        .map_err(|e| CatalogError::Unavailable(format!("open data directory lock: {e}")))?;
    file.try_lock().map_err(|e| {
        CatalogError::Unavailable(match e {
            TryLockError::WouldBlock => {
                format!("data directory {} is already in use", data_dir.display())
            }
            TryLockError::Error(e) => format!("lock data directory {}: {e}", data_dir.display()),
        })
    })?;
    // Keep the file in place: unlinking it would let another process lock a different inode.
    Ok(DirectoryLock(Some(file)))
}
