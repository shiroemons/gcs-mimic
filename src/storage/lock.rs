use crate::bucket::CatalogError;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::Path,
};

pub(super) struct DirectoryLock(Vec<File>);

impl DirectoryLock {
    pub(super) fn release(mut self) -> io::Result<()> {
        while let Some(file) = self.0.pop() {
            file.unlock()?;
        }
        Ok(())
    }

    pub(super) fn file(&self) -> &File {
        &self.0[0]
    }
}

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        // Closing alone leaves flock held by descriptors inherited between fork and exec.
        for file in &self.0 {
            if let Err(error) = file.unlock() {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Error,
                    "data_lock_release_failed",
                    serde_json::json!({"message":format!("release data directory lock: {error}")}),
                );
            }
        }
    }
}

pub(super) fn acquire_directory(path: &Path) -> io::Result<DirectoryLock> {
    let file = crate::fsutil::open_directory_nofollow(path)?;
    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
        TryLockError::Error(error) => error,
    })?;
    Ok(DirectoryLock(vec![file]))
}

pub(super) fn acquire(data_dir: &Path) -> Result<DirectoryLock, CatalogError> {
    fs::create_dir_all(data_dir).map_err(|e| {
        CatalogError::Unavailable(format!("create data directory {}: {e}", data_dir.display()))
    })?;
    // A directory inode lock also survives replacement of catalog.lock on Unix.
    #[cfg(unix)]
    let mut directory =
        acquire_directory(&data_dir.canonicalize().map_err(|error| {
            CatalogError::Unavailable(format!("resolve data directory: {error}"))
        })?)
        .map_err(|error| {
            CatalogError::Unavailable(if error.kind() == io::ErrorKind::WouldBlock {
                format!("data directory {} is already in use", data_dir.display())
            } else {
                format!("lock data directory {}: {error}", data_dir.display())
            })
        })?;
    #[cfg(not(unix))]
    let mut directory = DirectoryLock(Vec::new());
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
    directory.0.push(file);
    Ok(directory)
}
