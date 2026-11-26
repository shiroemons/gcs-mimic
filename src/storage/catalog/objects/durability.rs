use super::*;

#[cfg(unix)]
#[cfg_attr(test, track_caller)]
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

#[cfg_attr(test, track_caller)]
pub(in crate::storage::catalog) fn publish_blob(
    source: &Path,
    destination: &Path,
) -> io::Result<()> {
    #[cfg(test)]
    fault::record_catalog_thread_io(source);
    let shard = blob_shard(destination)?;
    {
        // A concurrent publisher must not pass a creator whose parent fsync is unfinished.
        let creation = crate::sync::lock_checked(&SHARD_CREATION);
        // The lock guards no data, so a creator that unwound must not stop all
        // later publishing. It may have left a shard whose parent entry was never
        // synced, so the next publisher syncs the parent before clearing the poison.
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
        if unwound {
            SHARD_CREATION.clear_poison();
            crate::sync::report_recovery();
        }
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
            crate::diagnostics::Event::BlobShardCleanupDeferred,
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

#[cfg_attr(test, track_caller)]
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
    lease: StageLease,
    #[cfg(test)] data_dir: PathBuf,
) -> Result<StagedFile, ObjectError> {
    crate::task::run_blocking("upload-stage", move || -> io::Result<_> {
        Ok(BatchWriter::create(
            stage,
            lease,
            #[cfg(test)]
            &data_dir,
        )?
        .into())
    })
    .await?
    .map_err(object_unavailable)
}

/// How many written bytes a staged body may hold before they are sent to storage.
const WRITE_BEHIND_BYTES: u64 = 8 * 1024 * 1024;

/// A staged body on disk that never holds more than [`WRITE_BEHIND_BYTES`] unsynced.
///
/// The kernel charges unwritten page cache to a container's memory limit, and through
/// overlayfs it does not slow the writer down to that limit. A large body written with one
/// final sync therefore filled a 128 MiB container with dirty pages whenever the disk fell
/// behind, and the kernel killed a process that itself held about 16 MiB.
pub(in crate::storage::catalog) struct StagedFile {
    writer: Option<BatchWriter>,
    filling: Vec<u8>,
    pending: Option<WriteJob>,
    failed: bool,
}

/// The caller's upload admission or exclusive writer lease. The worker owns it with
/// the descriptor, so cancellation cannot let another writer reuse the stage mid-write.
pub(in crate::storage::catalog) type StageLease = Arc<dyn StageOwner>;

pub(in crate::storage::catalog) trait StageOwner: Send + Sync {}
impl StageOwner for crate::storage::catalog::UploadGuard {}
impl StageOwner for crate::storage::catalog::resumable::WriterGuard {}
#[cfg(test)]
impl StageOwner for () {}

pub(in crate::storage::catalog) const WRITE_BATCH_BYTES: usize = 64 * 1024;

type WriteJob = tokio::task::JoinHandle<
    Result<(BatchWriter, Result<(), ObjectError>), crate::task::JobPanicked>,
>;

pub(in crate::storage::catalog) struct BatchWriter {
    file: File,
    _lease: StageLease,
    bytes: Vec<u8>,
    unsynced: u64,
    #[cfg(test)]
    data_dir: PathBuf,
}

impl BatchWriter {
    pub(in crate::storage::catalog) fn create(
        stage: PathBuf,
        lease: StageLease,
        #[cfg(test)] data_dir: &Path,
    ) -> io::Result<Self> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage)?;
        Ok(Self::new(
            file,
            lease,
            #[cfg(test)]
            data_dir,
        ))
    }

    pub(in crate::storage::catalog) fn new(
        file: File,
        lease: StageLease,
        #[cfg(test)] data_dir: &Path,
    ) -> Self {
        Self {
            file,
            _lease: lease,
            bytes: Vec::new(),
            unsynced: 0,
            #[cfg(test)]
            data_dir: data_dir.to_owned(),
        }
    }

    pub(in crate::storage::catalog) fn write(
        &mut self,
        mut bytes: &[u8],
    ) -> Result<(), ObjectError> {
        while !bytes.is_empty() {
            bytes = fill_batch(&mut self.bytes, bytes);
            if self.bytes.len() == WRITE_BATCH_BYTES {
                self.write_batch()?;
            }
        }
        Ok(())
    }

    fn write_batch(&mut self) -> Result<(), ObjectError> {
        use std::io::Write;

        if self.bytes.is_empty() {
            return Ok(());
        }
        #[cfg(test)]
        fault::hit(&self.data_dir, Step::BodyWrite).map_err(object_unavailable)?;
        self.file.write_all(&self.bytes)?;
        self.unsynced += self.bytes.len() as u64;
        self.bytes.clear();
        // Only Linux charges page cache against the container limit this way.
        if cfg!(any(target_os = "linux", test)) && self.unsynced >= WRITE_BEHIND_BYTES {
            #[cfg(test)]
            fault::record_catalog_thread_io(&self.data_dir);
            #[cfg(test)]
            fault::hit_io(&self.data_dir, Step::BodyWriteBehind)?;
            self.file.sync_data()?;
            #[cfg(test)]
            fault::file_synced(&self.data_dir, &self.file.metadata()?);
            self.unsynced = 0;
        }
        Ok(())
    }

    pub(in crate::storage::catalog) fn make_durable(&mut self) -> Result<(), ObjectError> {
        #[cfg(test)]
        fault::hit(&self.data_dir, Step::BodyFlush).map_err(object_unavailable)?;
        self.write_batch()?;
        #[cfg(test)]
        fault::record_catalog_thread_io(&self.data_dir);
        #[cfg(test)]
        fault::hit(&self.data_dir, Step::BodySync).map_err(object_unavailable)?;
        #[cfg(unix)]
        crate::fsutil::sync_ordered(&self.file)?;
        #[cfg(not(unix))]
        self.file.sync_all()?;
        #[cfg(test)]
        fault::file_synced(&self.data_dir, &self.file.metadata()?);
        self.unsynced = 0;
        Ok(())
    }
}

fn fill_batch<'a>(batch: &mut Vec<u8>, bytes: &'a [u8]) -> &'a [u8] {
    let count = bytes.len().min(WRITE_BATCH_BYTES - batch.len());
    let (part, rest) = bytes.split_at(count);
    let needed = batch.len() + count;
    if needed > batch.capacity() {
        let capacity = (2 * batch.capacity()).clamp(needed, WRITE_BATCH_BYTES);
        batch.reserve_exact(capacity - batch.len());
    }
    batch.extend_from_slice(part);
    rest
}

impl From<BatchWriter> for StagedFile {
    fn from(writer: BatchWriter) -> Self {
        Self {
            writer: Some(writer),
            filling: Vec::new(),
            pending: None,
            failed: false,
        }
    }
}

impl StagedFile {
    #[cfg(test)]
    pub(in crate::storage::catalog) fn new(file: File, lease: StageLease, data_dir: &Path) -> Self {
        BatchWriter::new(file, lease, data_dir).into()
    }

    /// A lease stays with its supervised upload until the last submitted write has returned.
    async fn settle(&mut self) -> Result<(), ObjectError> {
        let Some(job) = &mut self.pending else {
            return Ok(());
        };
        let joined = crate::task::join_blocking(job).await;
        self.pending = None;
        let result = match joined {
            Ok((mut writer, result)) => {
                writer.bytes.clear();
                self.writer = Some(writer);
                result
            }
            Err(error) => Err(error.into()),
        };
        self.failed |= result.is_err();
        result
    }

    pub(in crate::storage::catalog) async fn settle_result<T, E: From<ObjectError>>(
        &mut self,
        result: Result<T, E>,
    ) -> Result<T, E> {
        self.settle().await?;
        result
    }

    fn submit(
        &mut self,
        name: &'static str,
        action: impl FnOnce(&mut BatchWriter) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<(), ObjectError> {
        let mut writer = self.writer.take().ok_or_else(staged_writer_unavailable)?;
        std::mem::swap(&mut self.filling, &mut writer.bytes);
        self.pending = Some(crate::task::spawn_blocking_on(
            &tokio::runtime::Handle::current(),
            name,
            move || {
                let result = action(&mut writer);
                (writer, result)
            },
        ));
        Ok(())
    }

    pub(in crate::storage::catalog) async fn write(
        &mut self,
        mut bytes: &[u8],
    ) -> Result<(), ObjectError> {
        self.ensure_writable()?;
        while !bytes.is_empty() {
            bytes = fill_batch(&mut self.filling, bytes);
            if self.filling.len() == WRITE_BATCH_BYTES {
                self.settle().await?;
                self.submit("staged-write", BatchWriter::write_batch)?;
            }
        }
        Ok(())
    }

    pub(in crate::storage::catalog) async fn make_durable(&mut self) -> Result<(), ObjectError> {
        self.ensure_writable()?;
        self.settle().await?;
        self.submit("staged-sync", BatchWriter::make_durable)?;
        self.settle().await
    }

    pub(in crate::storage::catalog) async fn rollback(
        &mut self,
        len: u64,
    ) -> Result<(), ObjectError> {
        let written = self.settle().await;
        self.filling.clear();
        self.submit("staged-truncate", move |writer| {
            writer.file.set_len(len)?;
            #[cfg(test)]
            fault::truncated(&writer.data_dir, &writer.file.metadata()?);
            std::io::Seek::seek(&mut writer.file, io::SeekFrom::Start(len))?;
            Ok(())
        })?;
        self.settle().await?;
        self.submit("staged-sync", BatchWriter::make_durable)?;
        self.settle().await?;
        written
    }

    fn ensure_writable(&self) -> Result<(), ObjectError> {
        if self.failed {
            return Err(object_unavailable("staged file write previously failed"));
        }
        Ok(())
    }

    pub(in crate::storage::catalog) fn file(&self) -> Result<&File, ObjectError> {
        self.ensure_writable()?;
        self.writer
            .as_ref()
            .map(|writer| &writer.file)
            .ok_or_else(staged_writer_unavailable)
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        // An unstarted job can be revoked. An executing job releases the file and its
        // lease together on the blocking pool, even during unwind or runtime shutdown.
        if let Some(job) = &self.pending {
            job.abort();
        }
    }
}

fn staged_writer_unavailable() -> ObjectError {
    object_unavailable("staged file worker did not return")
}
