use crate::bucket::CatalogError;
use crate::storage::failure::BackendCause;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::Path,
};

pub(super) struct DirectoryLock {
    directory: Option<File>,
    file: File,
}

impl DirectoryLock {
    pub(super) fn release(self) -> io::Result<()> {
        self.file.unlock()?;
        if let Some(directory) = &self.directory {
            directory.unlock()?;
        }
        Ok(())
    }

    pub(super) fn file(&self) -> &File {
        &self.file
    }
}

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        // Closing alone leaves flock held by descriptors inherited between fork and exec.
        for file in std::iter::once(&self.file).chain(self.directory.as_ref()) {
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
    Ok(DirectoryLock {
        directory: None,
        file: acquire_directory_file(path)?,
    })
}

fn acquire_directory_file(path: &Path) -> io::Result<File> {
    let file = crate::fsutil::open_directory_nofollow(path)?;
    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
        TryLockError::Error(error) => error,
    })?;
    Ok(file)
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
    let directory = acquire_directory_file(
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
    let directory = None;
    #[cfg(unix)]
    let directory = Some(directory);
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
    Ok(DirectoryLock { directory, file })
}
