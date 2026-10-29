use crate::bucket::CatalogError;
use crate::storage::failure::BackendCause;
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

/// Keeps `error` as the source of a message that already describes it.
fn lock_failure(message: String, error: io::Error) -> CatalogError {
    CatalogError::Unavailable(BackendCause::with_source(message, error))
}

fn in_use(data_dir: &Path) -> CatalogError {
    CatalogError::Unavailable(
        format!("data directory {} is already in use", data_dir.display()).into(),
    )
}

pub(super) fn acquire(data_dir: &Path) -> Result<DirectoryLock, CatalogError> {
    fs::create_dir_all(data_dir).map_err(|e| {
        lock_failure(
            format!("create data directory {}: {e}", data_dir.display()),
            e,
        )
    })?;
    // A directory inode lock also survives replacement of catalog.lock on Unix.
    #[cfg(unix)]
    let mut directory = acquire_directory(
        &data_dir
            .canonicalize()
            .map_err(|error| lock_failure(format!("resolve data directory: {error}"), error))?,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::WouldBlock {
            in_use(data_dir)
        } else {
            lock_failure(
                format!("lock data directory {}: {error}", data_dir.display()),
                error,
            )
        }
    })?;
    #[cfg(not(unix))]
    let mut directory = DirectoryLock(Vec::new());
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join("catalog.lock"))
        .map_err(|e| lock_failure(format!("open data directory lock: {e}"), e))?;
    file.try_lock().map_err(|e| match e {
        TryLockError::WouldBlock => in_use(data_dir),
        TryLockError::Error(e) => lock_failure(
            format!("lock data directory {}: {e}", data_dir.display()),
            e,
        ),
    })?;
    // Keep the file in place: unlinking it would let another process lock a different inode.
    directory.0.push(file);
    Ok(directory)
}
