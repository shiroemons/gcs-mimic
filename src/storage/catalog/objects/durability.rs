use super::*;

#[cfg(unix)]
pub(in crate::storage::catalog) fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    fault::record_catalog_thread_io(path);
    crate::fsutil::sync_dir(path)?;
    #[cfg(test)]
    fault::dir_synced(path);
    Ok(())
}

pub(super) static SHARD_CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The shard directory that holds a blob; every blob path built by `blob_path` has one.
pub(in crate::storage::catalog) fn blob_shard(path: &Path) -> io::Result<&Path> {
    path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "blob path has no shard directory",
        )
    })
}

pub(in crate::storage::catalog) fn publish_blob(
    source: &Path,
    destination: &Path,
) -> io::Result<()> {
    #[cfg(test)]
    fault::record_catalog_thread_io(source);
    let shard = blob_shard(destination)?;
    {
        // A concurrent publisher must not pass a creator whose parent fsync is unfinished.
        let creation = SHARD_CREATION.lock();
        // The lock guards no data, so a creator that unwound must not stop all
        // later publishing. It may have left a shard whose parent entry was never
        // synced, so the next publisher syncs the parent before clearing the poison.
        #[cfg(unix)]
        let unwound = creation.is_err();
        let _creation = creation.unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(unix)]
        let blobs = shard
            .parent()
            .ok_or_else(|| io::Error::other("blob shard has no parent directory"))?;
        match fs::create_dir(shard) {
            Ok(()) => {
                #[cfg(all(test, unix))]
                fault::dir_created(shard);
                #[cfg(unix)]
                if let Err(error) = sync_dir(blobs) {
                    return Err(cleanup_failed_shard(shard, error));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(shard)?.is_dir() {
                    return Err(io::Error::other("blob shard is not a directory"));
                }
                #[cfg(unix)]
                if unwound {
                    sync_dir(blobs)?;
                }
            }
            Err(error) => return Err(error),
        }
        SHARD_CREATION.clear_poison();
    }
    #[cfg(unix)]
    {
        crate::fsutil::rename_noreplace(source, destination)?;
        #[cfg(test)]
        fault::renamed(source, destination);
        sync_dir(shard)
    }
    #[cfg(windows)]
    {
        // Preserve extended paths and keep publication on the same volume. Windows
        // uses write-through publication, not Unix directory-fsync durability.
        let source = source.canonicalize()?;
        let file_name = destination.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "blob destination has no file name",
            )
        })?;
        let destination = shard.canonicalize()?.join(file_name);
        crate::fsutil::move_write_through(&source, &destination)
    }
}

#[cfg(unix)]
pub(super) fn cleanup_failed_shard(shard: &Path, error: io::Error) -> io::Error {
    if let Err(cleanup) = fs::remove_dir(shard) {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            "blob_shard_cleanup_deferred",
            serde_json::json!({"message": cleanup.to_string()}),
        );
    }
    error
}

/// Commits a publication transaction after its blob is durable in `blobs/`.
///
/// Every object publication commits here, so fault-injection tests can fail the commit or
/// report an error after it succeeded, the ambiguous outcome cleanup must survive.
pub(in crate::storage::catalog) fn commit_publication(
    transaction: Transaction<'_>,
) -> Result<(), ObjectError> {
    #[cfg(test)]
    let data_dir = fault::database_dir(&transaction);
    #[cfg(test)]
    fault::hit(&data_dir, Step::BeforeCommit).map_err(object_unavailable)?;
    transaction.commit()?;
    #[cfg(test)]
    fault::hit(&data_dir, Step::AfterCommit).map_err(object_unavailable)?;
    Ok(())
}

impl Catalog {
    /// Moves a durable staged body into its blob shard on the blocking pool.
    pub(in crate::storage::catalog) async fn publish_staged(
        &self,
        source: PathBuf,
        destination: PathBuf,
    ) -> Result<(), ObjectError> {
        #[cfg(test)]
        fault::hit(&self.0.data_dir, Step::BeforePublish).map_err(object_unavailable)?;
        crate::task::run_blocking("blob-publication", move || {
            publish_blob(&source, &destination)
        })
        .await?
        .map_err(object_unavailable)?;
        #[cfg(test)]
        fault::hit(&self.0.data_dir, Step::AfterPublish).map_err(object_unavailable)?;
        Ok(())
    }
}

pub(in crate::storage::catalog) fn unlink(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    fault::record_catalog_thread_io(path);
    #[cfg(all(test, unix))]
    let retained = fault::retain_before_unlink(path)?;
    // Windows may defer removal until the final reader closes. Recovery also
    // scans unreferenced blobs, so a deferred deletion cannot become live data.
    let result = match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    };
    #[cfg(all(test, unix))]
    if result.is_ok()
        && let Some(file) = retained
    {
        fault::unlinked(path, &file)?;
    }
    result
}

/// Creates an exclusive staging file; publication checks the final name atomically.
pub(in crate::storage::catalog) async fn create_upload_stage(
    stage: PathBuf,
) -> Result<File, ObjectError> {
    crate::task::run_blocking("upload-stage", move || -> io::Result<File> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage)
    })
    .await?
    .map_err(object_unavailable)
}

/// Makes every byte written to a staged body durable before it can be published.
///
/// Tests record the synced length for power-loss snapshots from the `sync_all` result
/// itself, so a write path that skips this call, or a sync dropped from it together with
/// the result it yields, leaves its bytes volatile and fails the power-loss crash tests.
pub(in crate::storage::catalog) async fn make_durable(
    file: &mut tokio::fs::File,
    #[cfg(test)] data_dir: &Path,
) -> Result<(), ObjectError> {
    #[cfg(test)]
    fault::hit(data_dir, Step::BodyFlush).map_err(object_unavailable)?;
    // tokio finishes the last write in the background, and sync_all drops its error;
    // flush reports it, so a failed tail cannot publish a short blob at the full size.
    file.flush().await?;
    #[cfg(test)]
    fault::hit(data_dir, Step::BodySync).map_err(object_unavailable)?;
    let synced = file.sync_all().await;
    #[cfg(test)]
    if synced.is_ok() {
        fault::file_synced(data_dir, &file.metadata().await?);
    }
    synced.map_err(object_unavailable)
}
