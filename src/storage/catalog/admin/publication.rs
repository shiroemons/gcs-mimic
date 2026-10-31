#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::objects::object_unavailable as objects_error;
use super::*;
use crate::{fsutil, hashing::Hasher, storage::failure::BackendCause};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::mpsc::TrySendError,
};
use tokio::sync::oneshot;

#[cfg(test)]
mod tests;

mod reseed;
pub(crate) use reseed::{SeedBatch, SeedMode};

#[cfg(test)]
thread_local! {
    static COPY_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static FORCE_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

type FileJob = Box<dyn FnOnce() + Send>;

const FILE_WORKER: &str = "publication-file";

/// The send failure without the rejected job, which is not `Sync`, so it can be kept as a
/// source.
fn without_job(error: &TrySendError<FileJob>) -> TrySendError<()> {
    match error {
        TrySendError::Full(_) => TrySendError::Full(()),
        TrySendError::Disconnected(_) => TrySendError::Disconnected(()),
    }
}

pub(crate) struct FileWorkers {
    sender: std::sync::mpsc::SyncSender<FileJob>,
    slots: Arc<Semaphore>,
}

impl FileWorkers {
    pub(crate) fn new() -> io::Result<Self> {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<FileJob>(2);
        let receiver = Arc::new(std::sync::Mutex::new(receiver));
        for _ in 0..2 {
            let receiver = receiver.clone();
            crate::task::spawn_thread(FILE_WORKER, move || {
                loop {
                    // Jobs run after the guard is released, so the lock is never poisoned by
                    // a job; recovering keeps the pool whole even if it were.
                    let job = receiver
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .recv();
                    match job {
                        Ok(job) => job(),
                        Err(_) => return,
                    }
                }
            })?;
        }
        Ok(Self {
            sender,
            slots: Arc::new(Semaphore::new(2)),
        })
    }

    pub(crate) async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> Result<T, AdminError> + Send + 'static,
    ) -> Result<T, AdminError> {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AdminError::Closed)?;
        let (sender, receiver) = oneshot::channel();
        self.sender
            .try_send(Box::new(move || {
                let _permit = permit;
                // A panicking job must neither end its worker, which would shrink the pool for
                // every later job, nor leave its caller without a reply.
                let result = crate::task::catch_job(FILE_WORKER, job)
                    .unwrap_or_else(|panicked| Err(AdminError::backend(panicked)));
                let _ = sender.send(result);
            }))
            .map_err(|error| AdminError::backend(without_job(&error)))?;
        receiver.await.map_err(AdminError::backend)?
    }
}

/// Verified private bytes; dropping schedules owned cleanup, never synchronous I/O.
pub(crate) struct StagedObject {
    catalog: Catalog,
    id: Option<String>,
    permit: Option<OwnedSemaphorePermit>,
    size: u64,
    hashes: crate::hashing::Hashes,
    published: bool,
    sync_update: Option<super::super::sync_state::SyncUpdate>,
    sync_resolution: Option<super::super::sync_state::conflicts::ResolutionBinding>,
    import_identity: Option<ImportedIdentity>,
    runtime: tokio::runtime::Handle,
    activity: Option<super::super::Activity>,
    batch_ownership: Option<Arc<OwnedSemaphorePermit>>,
}

impl StagedObject {
    pub(crate) fn with_sync_resolution(
        mut self,
        binding: super::super::sync_state::conflicts::ResolutionBinding,
    ) -> Self {
        self.sync_resolution = Some(binding);
        self
    }
    pub(crate) fn with_sync_state(mut self, update: super::super::sync_state::SyncUpdate) -> Self {
        self.sync_update = Some(update);
        self
    }
    pub(crate) async fn discard(self) -> Result<(), AdminError> {
        let runtime = self.runtime.clone();
        crate::task::spawn_supervised_on(&runtime, "staged-object-discard", self.cleanup())
            .await
            .map_err(AdminError::backend)?
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn hashes(&self) -> crate::hashing::Hashes {
        self.hashes
    }

    async fn cleanup(mut self) -> Result<(), AdminError> {
        if let Some(id) = self.id.take() {
            cleanup(&self.catalog, id).await?;
        }
        Ok(())
    }
}

impl Drop for StagedObject {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else { return };
        let catalog = self.catalog.clone();
        let permit = self.permit.take();
        let activity = self.activity.take();
        let batch_ownership = self.batch_ownership.take();
        crate::task::spawn_supervised_on(&self.runtime, "staged-object-cleanup", async move {
            let (_permit, _activity, _batch_ownership) = (permit, activity, batch_ownership);
            if let Err(error) = cleanup(&catalog, id).await {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Error,
                    "publication_cleanup_failed",
                    serde_json::json!({"message": error.to_string()}),
                );
            }
        });
    }
}

pub(crate) struct BatchPublication {
    pub bucket: BucketName,
    pub name: ObjectName,
    pub request: Publication,
    pub staged: StagedObject,
}

pub(crate) struct BatchResult {
    pub results: Vec<Result<ObjectRecord, AdminError>>,
    pub unattempted: Vec<BatchPublication>,
    pub budget_exceeded: bool,
}

use super::super::import_journal::{self, ImportBinding, ImportElement};

pub(crate) struct ImportedPublication {
    pub publication: BatchPublication,
    pub identity: ImportedIdentity,
    pub element: ImportElement,
}

pub(crate) struct ImportedBatchResult {
    pub results: Vec<Result<ObjectRecord, AdminError>>,
    pub unattempted: Vec<ImportedPublication>,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "only unit tests read this field")
    )]
    pub budget_exceeded: bool,
}

struct ImportBatch {
    binding: ImportBinding,
    cursor: i64,
    elements: Vec<ImportElement>,
}

fn fs_error(error: fsutil::FsError) -> AdminError {
    match error {
        fsutil::FsError::Io(error) => error.into(),
        fsutil::FsError::Unsupported => {
            AdminError::Invalid("unsupported filesystem operation".into())
        }
    }
}

fn changed(before: fsutil::FileIdentity, after: fsutil::FileIdentity) -> Result<(), AdminError> {
    let fields = [
        ("device", before.device != after.device),
        ("inode", before.inode != after.inode),
        ("size", before.size != after.size),
        ("mtime_ns", before.modified_ns != after.modified_ns),
        ("ctime_ns", before.changed_ns != after.changed_ns),
    ]
    .into_iter()
    .filter(|(_, changed)| *changed)
    .map(|(field, _)| field.into())
    .collect::<Vec<_>>();
    if fields.is_empty() {
        Ok(())
    } else {
        Err(AdminError::SourceChanged { fields })
    }
}

fn stage_file(
    source: &File,
    stage: &Path,
    expected: ExpectedHashes,
    max_bytes: u64,
) -> Result<(u64, crate::hashing::Hashes), AdminError> {
    if !source.metadata()?.is_file() {
        return Err(AdminError::Invalid("expected a regular source file".into()));
    }
    let before = fsutil::stat_identity(source).map_err(fs_error)?;
    if before.size > max_bytes {
        return Err(AdminError::TooLarge);
    }
    let parent = stage
        .parent()
        .ok_or_else(|| AdminError::Invalid("missing staging directory".into()))?;
    let name = stage
        .file_name()
        .ok_or_else(|| AdminError::Invalid("missing staging name".into()))?;
    let mut hasher = Hasher::default();
    let mut size = 0u64;
    let mut buffer = [0; 64 * 1024];
    let clone = || fsutil::clone_into(source, &fsutil::open_directory(parent)?, name);
    #[cfg(test)]
    let cloned = if FORCE_COPY.get() {
        Err(fsutil::FsError::Unsupported)
    } else {
        clone()
    };
    #[cfg(not(test))]
    let cloned = clone();
    let cloned = match cloned {
        Ok(()) => true,
        Err(fsutil::FsError::Unsupported) => false,
        Err(error) => return Err(fs_error(error)),
    };
    let mut target = if cloned {
        File::open(stage)?
    } else {
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(stage)?
    };
    loop {
        let count = if cloned {
            target.read(&mut buffer)
        } else {
            fsutil::read_at(source, &mut buffer, size)
        };
        let count = match count {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        size = size.checked_add(count as u64).ok_or(AdminError::TooLarge)?;
        if size > max_bytes {
            return Err(AdminError::TooLarge);
        }
        if !cloned {
            #[cfg(test)]
            super::super::fault::hit(
                stage
                    .parent()
                    .and_then(Path::parent)
                    .ok_or_else(|| AdminError::Invalid("missing data directory".into()))?,
                super::super::fault::Step::BodyWrite,
            )
            .map_err(objects_error)?;
            target.write_all(&buffer[..count])?;
        }
        hasher.update(&buffer[..count]);
        #[cfg(test)]
        COPY_HOOK.with_borrow_mut(|hook| {
            if let Some(hook) = hook.take() {
                hook();
            }
        });
    }
    #[cfg(test)]
    let data_dir = stage
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| AdminError::Invalid("missing data directory".into()))?;
    #[cfg(test)]
    super::super::fault::hit(data_dir, super::super::fault::Step::BodyFlush)
        .map_err(objects_error)?;
    target.flush()?;
    #[cfg(test)]
    super::super::fault::hit(data_dir, super::super::fault::Step::BodySync)
        .map_err(objects_error)?;
    target.sync_all()?;
    #[cfg(test)]
    super::super::fault::file_synced(data_dir, &target.metadata()?);
    changed(before, fsutil::stat_identity(source).map_err(fs_error)?)?;
    let hashes = hasher.finish();
    if expected.md5.is_some_and(|md5| md5 != hashes.md5)
        || expected.crc32c.is_some_and(|crc| crc != hashes.crc32c)
    {
        return Err(AdminError::Checksum);
    }
    Ok((size, hashes))
}

async fn cleanup(catalog: &Catalog, id: String) -> Result<(), AdminError> {
    let probe = id.clone();
    let referenced = catalog
        .run_upload(move |connection| {
            // Shares the cached statement of the upload path's unreferenced-stage probe.
            connection
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)")
                .and_then(|mut statement| statement.query_row([probe], |row| row.get::<_, bool>(0)))
                .map_err(objects_error)
        })
        .await
        .map_err(AdminError::from)?;
    if let Some(memory) = &catalog.0.memory {
        if !referenced {
            memory.remove(&id)?;
        }
        return Ok(());
    }
    let stage = catalog.0.data_dir.join("staging").join(&id);
    let blob = objects::blob_path(&catalog.0.data_dir, &id).map_err(AdminError::from)?;
    catalog
        .0
        .file_workers
        .run(move || {
            objects::unlink(&stage)?;
            if !referenced {
                objects::unlink(&blob)?;
                #[cfg(unix)]
                if let Some(shard) = blob.parent()
                    && shard.exists()
                {
                    fsutil::sync_dir(shard)?;
                }
            }
            Ok(())
        })
        .await
}

impl Admin {
    pub(crate) async fn stage_from_file(
        &self,
        source: File,
        expected: ExpectedHashes,
        max_bytes: u64,
    ) -> Result<StagedObject, AdminError> {
        let activity = self.catalog.activity()?;
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        let permit = self
            .catalog
            .wait_admin(self.catalog.0.staged_handles.clone(), deadline)
            .await?;
        let worker = self
            .catalog
            .wait_admin(self.catalog.0.admin_uploads.clone(), deadline)
            .await?;
        let catalog = self.catalog.clone();
        // Owned work returns a handle even if its receiving future is cancelled; Drop then cleans it.
        crate::task::spawn_supervised("admin-stage-object", async move {
            let _worker = worker;
            let mut random = [0; 16];
            getrandom::fill(&mut random).map_err(AdminError::backend)?;
            let id = crate::hashing::encode_lower(&random);
            let mut handle = StagedObject {
                catalog: catalog.clone(),
                id: None,
                permit: Some(permit),
                size: 0,
                hashes: crate::hashing::Hashes {
                    md5: [0; 16],
                    crc32c: 0,
                },
                published: false,
                sync_update: None,
                sync_resolution: None,
                import_identity: None,
                runtime: tokio::runtime::Handle::current(),
                activity: Some(activity),
                batch_ownership: None,
            };
            let stage = catalog.0.data_dir.join("staging").join(&id);
            let blob = objects::blob_path(&catalog.0.data_dir, &id).map_err(AdminError::from)?;
            let memory = catalog.0.memory.clone();
            let memory_id = id.clone();
            let result = catalog
                .0
                .file_workers
                .run(move || {
                    if let Some(memory) = memory {
                        return Ok(stage_memory(
                            &memory, &source, memory_id, expected, max_bytes,
                        ));
                    }
                    if blob.try_exists()? || stage.try_exists()? {
                        return Err(AdminError::Invalid("blob ID collision".into()));
                    }
                    Ok(stage_file(&source, &stage, expected, max_bytes))
                })
                .await?;
            handle.id = Some(id);
            match result {
                Ok((size, hashes)) => {
                    handle.size = size;
                    handle.hashes = hashes;
                    handle.published = catalog.0.memory.is_some();
                    Ok(handle)
                }
                Err(error) => {
                    if error == AdminError::Io(io::ErrorKind::AlreadyExists) {
                        handle.id = None;
                    }
                    handle.cleanup().await?;
                    Err(error)
                }
            }
        })
        .await
        .map_err(AdminError::backend)?
    }

    pub async fn publish_from_file(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        request: Publication,
        source: File,
    ) -> Result<ObjectRecord, AdminError> {
        let staged = self
            .stage_from_file(source, request.expected, request.max_bytes)
            .await?;
        let mut result = self
            .publish_objects(vec![BatchPublication {
                bucket: bucket.clone(),
                name: name.clone(),
                request,
                staged,
            }])
            .await?;
        result
            .results
            .pop()
            .ok_or_else(|| AdminError::Backend("single publication was not attempted".into()))?
    }

    pub(crate) async fn publish_objects(
        &self,
        entries: Vec<BatchPublication>,
    ) -> Result<BatchResult, AdminError> {
        self.publish_objects_with_budget(entries, Duration::from_millis(20))
            .await
    }

    // The budget bounds real write-transaction time, so tests that require every element to be
    // attempted widen it instead of relying on an unloaded machine finishing within 20 ms.
    pub(in crate::storage) async fn publish_objects_with_budget(
        &self,
        entries: Vec<BatchPublication>,
        budget: Duration,
    ) -> Result<BatchResult, AdminError> {
        self.publish_batch(entries, budget, None).await
    }

    pub(crate) async fn publish_import_objects(
        &self,
        binding: ImportBinding,
        expected_cursor: i64,
        entries: Vec<ImportedPublication>,
    ) -> Result<ImportedBatchResult, AdminError> {
        self.publish_import_objects_with_budget(
            binding,
            expected_cursor,
            entries,
            Duration::from_millis(20),
        )
        .await
    }

    pub(in crate::storage) async fn publish_import_objects_with_budget(
        &self,
        binding: ImportBinding,
        expected_cursor: i64,
        entries: Vec<ImportedPublication>,
        budget: Duration,
    ) -> Result<ImportedBatchResult, AdminError> {
        if entries.is_empty() || entries.len() > 256 {
            return Err(AdminError::Invalid(
                "import batch requires 1..=256 entries".into(),
            ));
        }
        let elements = entries
            .iter()
            .map(|entry| entry.element.clone())
            .collect::<Vec<_>>();
        let publications = entries
            .into_iter()
            .map(|mut entry| {
                entry.publication.staged.import_identity = Some(entry.identity);
                entry.publication
            })
            .collect();
        let batch = self
            .publish_batch(
                publications,
                budget,
                Some(ImportBatch {
                    binding,
                    cursor: expected_cursor,
                    elements: elements.clone(),
                }),
            )
            .await?;
        let mut unattempted = Vec::new();
        for (mut publication, element) in batch
            .unattempted
            .into_iter()
            .zip(elements.into_iter().skip(batch.results.len()))
        {
            let identity = publication
                .staged
                .import_identity
                .take()
                .ok_or_else(|| AdminError::Backend("missing imported identity".into()))?;
            unattempted.push(ImportedPublication {
                publication,
                identity,
                element,
            });
        }
        Ok(ImportedBatchResult {
            results: batch.results,
            unattempted,
            budget_exceeded: batch.budget_exceeded,
        })
    }

    async fn publish_batch(
        &self,
        entries: Vec<BatchPublication>,
        budget: Duration,
        import: Option<ImportBatch>,
    ) -> Result<BatchResult, AdminError> {
        if entries.len() > 256 {
            return Err(AdminError::Invalid(
                "publication batch exceeds 256 entries".into(),
            ));
        }
        for entry in &entries {
            if !Arc::ptr_eq(&entry.staged.catalog.0, &self.catalog.0) {
                return Err(AdminError::Invalid(
                    "staged handle belongs to another catalog".into(),
                ));
            }
        }
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        let request = self
            .catalog
            .0
            .publication_requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| AdminError::Busy {
                retry_after: Duration::from_secs(1),
            })?;
        let batch = self
            .catalog
            .wait_admin(self.catalog.0.publication_batch.clone(), deadline)
            .await?;
        crate::task::spawn_supervised(
            "admin-publication-batch",
            run_batch(
                self.catalog.clone(),
                self.grant.clone(),
                entries,
                deadline,
                budget,
                (batch, request),
                import,
            ),
        )
        .await
        .map_err(AdminError::backend)?
    }
}

fn stage_memory(
    memory: &Arc<crate::storage::memory::Store>,
    source: &File,
    id: String,
    expected: ExpectedHashes,
    max_bytes: u64,
) -> Result<(u64, crate::hashing::Hashes), AdminError> {
    if !source.metadata()?.is_file() {
        return Err(AdminError::Invalid("expected a regular source file".into()));
    }
    let before = fsutil::stat_identity(source).map_err(fs_error)?;
    let mut stage = memory.stage()?;
    let mut hasher = Hasher::default();
    let mut size = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = match fsutil::read_at(source, &mut buffer, size) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .filter(|size| *size <= max_bytes)
            .ok_or(AdminError::TooLarge)?;
        stage.write(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    changed(before, fsutil::stat_identity(source).map_err(fs_error)?)?;
    let hashes = hasher.finish();
    if expected.md5.is_some_and(|hash| hash != hashes.md5)
        || expected.crc32c.is_some_and(|hash| hash != hashes.crc32c)
    {
        return Err(AdminError::Checksum);
    }
    stage.publish(id)?;
    Ok((size, hashes))
}

async fn cleanup_entries(entries: Vec<BatchPublication>) -> Result<(), AdminError> {
    let mut failure = None;
    for entry in entries {
        if let Err(error) = entry.staged.cleanup().await {
            failure = Some(error);
        }
    }
    failure.map_or(Ok(()), Err)
}

async fn run_batch(
    catalog: Catalog,
    grant: AdminGrant,
    mut entries: Vec<BatchPublication>,
    deadline: tokio::time::Instant,
    budget: Duration,
    _ownership: (OwnedSemaphorePermit, OwnedSemaphorePermit),
    import: Option<ImportBatch>,
) -> Result<BatchResult, AdminError> {
    let prepared = async {
        for entry in &mut entries {
            if entry.staged.published {
                continue;
            }
            let id = entry
                .staged
                .id
                .as_ref()
                .ok_or_else(|| AdminError::Invalid("resolved staging handle".into()))?;
            let stage = catalog.0.data_dir.join("staging").join(id);
            let blob = objects::blob_path(&catalog.0.data_dir, id).map_err(AdminError::from)?;
            catalog
                .0
                .file_workers
                .run(move || {
                    objects::publish_blob(&stage, &blob)?;
                    Ok(())
                })
                .await?;
            entry.staged.published = true;
        }
        // Accepted batches retain ownership through shutdown; only new admission is closed.
        tokio::time::timeout_at(deadline, catalog.0.execution.clone().acquire_owned())
            .await
            .map_err(|_| AdminError::Busy {
                retry_after: Duration::from_secs(1),
            })?
            .map_err(|_| AdminError::Closed)
    }
    .await;
    let execution = match prepared {
        Ok(execution) => execution,
        Err(error) => {
            cleanup_entries(entries).await?;
            return Err(error);
        }
    };
    let clock = catalog.0.clock.clone();
    let (outcome, mut entries) = catalog
        .run_upload_admitted(execution, move |connection| {
            let outcome = commit_batch(
                connection,
                &entries,
                &grant,
                clock.as_ref(),
                budget,
                import.as_ref(),
            );
            Ok((outcome, entries))
        })
        .await
        .map_err(AdminError::from)?;
    let (results, attempted, exceeded) = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            cleanup_entries(entries).await?;
            return Err(AdminError::from(error));
        }
    };
    let unattempted = entries.split_off(attempted);
    cleanup_entries(entries).await?;
    catalog.reclaim_after_commit().await;
    Ok(BatchResult {
        results,
        unattempted,
        budget_exceeded: exceeded,
    })
}

type CommittedBatch = (Vec<Result<ObjectRecord, AdminError>>, usize, bool);

fn commit_batch(
    connection: &mut Connection,
    entries: &[BatchPublication],
    grant: &AdminGrant,
    clock: &dyn Clock,
    budget: Duration,
    import: Option<&ImportBatch>,
) -> Result<CommittedBatch, ObjectError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(objects_error)?;
    let started = std::time::Instant::now();
    if let Some(import) = import {
        import_journal::check_publication(&transaction, &import.binding, import.cursor).map_err(
            |error| match error {
                CatalogError::PreconditionFailed => ObjectError::PreconditionFailed,
                error => objects_error(error),
            },
        )?;
    }
    let mut results = Vec::new();
    for entry in entries {
        if !results.is_empty() && started.elapsed() >= budget {
            break;
        }
        transaction
            .execute_batch("SAVEPOINT publication_element")
            .map_err(objects_error)?;
        match publish_entry(&transaction, entry, grant, clock) {
            Ok(record) => {
                transaction
                    .execute_batch("RELEASE publication_element")
                    .map_err(objects_error)?;
                results.push(Ok(record));
            }
            Err(error) => match AdminError::from(error) {
                error @ (AdminError::Precondition
                | AdminError::Hold
                | AdminError::NotFound
                | AdminError::Invalid(_)
                | AdminError::Forbidden
                | AdminError::Checksum
                | AdminError::TooLarge) => {
                    transaction
                        .execute_batch(
                            "ROLLBACK TO publication_element; RELEASE publication_element",
                        )
                        .map_err(objects_error)?;
                    results.push(Err(error));
                }
                error => return Err(objects_error_admin(error)),
            },
        }
    }
    if let Some(import) = import {
        import_journal::record_outcomes(
            &transaction,
            &import.binding,
            import.cursor,
            &import.elements[..results.len()],
            &results.iter().map(Result::is_ok).collect::<Vec<_>>(),
        )
        .map_err(objects_error)?;
    }
    transaction.commit().map_err(objects_error)?;
    let attempted = results.len();
    Ok((results, attempted, started.elapsed() > budget))
}

fn objects_error_admin(error: AdminError) -> ObjectError {
    match error {
        AdminError::DiskFull => ObjectError::Io(io::ErrorKind::StorageFull.into()),
        AdminError::Io(kind) => ObjectError::Io(kind.into()),
        AdminError::Corrupt(reason) => ObjectError::Corrupt(reason.into()),
        error => ObjectError::Unavailable(BackendCause::from_error(error)),
    }
}

/// Checks the request's metadata, size limit and declared hashes against the staged body.
fn validate_staged_entry(entry: &BatchPublication) -> Result<(), ObjectError> {
    crate::object::validate_fixed_metadata("contentType", &entry.request.content_type)?;
    entry.request.metadata.validate()?;
    if entry.staged.size > entry.request.max_bytes {
        return Err(ObjectError::TooLarge);
    }
    if entry
        .request
        .expected
        .md5
        .is_some_and(|hash| hash != entry.staged.hashes.md5)
        || entry
            .request
            .expected
            .crc32c
            .is_some_and(|hash| hash != entry.staged.hashes.crc32c)
    {
        return Err(ObjectError::ChecksumMismatch);
    }
    Ok(())
}

fn publication_record(
    entry: &BatchPublication,
    generation: i64,
    defaults: objects::PublicationDefaults,
) -> Result<ObjectRecord, ObjectError> {
    let mut record = ObjectRecord {
        bucket: entry.bucket.as_str().into(),
        name: entry.name.as_str().into(),
        generation,
        metageneration: 1,
        owner: defaults.owner,
        content_type: entry.request.content_type.clone(),
        size: entry.staged.size,
        md5: entry.staged.hashes.md5,
        md5_availability: Default::default(),
        crc32c: entry.staged.hashes.crc32c,
        component_count: None,
        storage_class: defaults.storage_class,
        content_encoding: None,
        content_disposition: None,
        content_language: None,
        cache_control: None,
        metadata: Default::default(),
        time_created: defaults.time,
        updated: defaults.time,
        time_deleted: None,
        soft_delete_time: None,
        hard_delete_time: None,
        event_based_hold: defaults.event_based_hold,
        temporary_hold: false,
        custom_time: None,
        retention_expiration_time: None,
        retention: None,
        customer_key_sha256: None,
    };
    entry.request.metadata.clone().apply(&mut record)?;
    if let Some(identity) = entry.staged.import_identity {
        record.time_created = identity.time_created();
        record.updated = identity.updated();
    }
    Ok(record)
}

fn publish_entry(
    transaction: &rusqlite::Transaction<'_>,
    entry: &BatchPublication,
    grant: &AdminGrant,
    clock: &dyn Clock,
) -> Result<ObjectRecord, ObjectError> {
    validate_staged_entry(entry)?;
    if let Some(binding) = &entry.staged.sync_resolution {
        super::super::sync_state::conflicts::validate_restore(transaction, binding)?;
    }
    let access = BucketAccess::Admin(grant.clone());
    let bucket = entry.bucket.as_str();
    let name = entry.name.as_str();
    let (storage_class, delete_permission) =
        objects::authorized_bucket(transaction, bucket, &access)?;
    let storage_class = super::super::bucket_autoclass::object_storage_class(
        transaction,
        bucket,
        entry.request.metadata.storage_class.clone(),
        storage_class,
    )
    .map_err(ObjectError::from)?;
    let (versioning, default_hold) =
        Catalog::bucket_settings_in_transaction(transaction, bucket).map_err(objects_error)?;
    let previous = objects::lookup(transaction, bucket, name)?;
    objects::check_publication(
        previous.as_ref(),
        entry
            .request
            .condition
            .map(GenerationCondition::conditions)
            .unwrap_or_default(),
        delete_permission,
        clock.now(),
    )?;
    let staged = &entry.staged;
    if staged.import_identity.is_some()
        && (entry.request.expected.md5.is_none()
            || entry.request.expected.crc32c.is_none()
            || entry.request.mode != PublishMode::Replace
            || entry.request.condition != Some(GenerationCondition::Absent))
    {
        return Err(ObjectError::InvalidMetadata);
    }
    if objects::skip_same(
        entry.request.mode,
        previous.as_ref(),
        staged.size,
        staged.hashes.md5,
        staged.hashes.crc32c,
    ) {
        let record = previous
            .map(|(record, _)| record)
            .ok_or(ObjectError::ObjectNotFound)?;
        record_staged_state(transaction, staged, &record)?;
        return Ok(record);
    }
    let now = clock.now();
    now.duration_since(std::time::UNIX_EPOCH)
        .map_err(objects_error)?;
    let micros = super::super::unix_micros(now).map_err(objects_error)?;
    let time = super::super::system_time_micros(micros).map_err(objects_error)?;
    let id = staged
        .id
        .as_deref()
        .ok_or_else(|| objects_error("resolved staged handle"))?;
    let defaults = objects::PublicationDefaults {
        storage_class,
        time,
        event_based_hold: staged.import_identity.is_none() && default_hold,
        owner: if staged.import_identity.is_some() {
            None
        } else {
            Some(super::super::object_owner::for_publication(
                transaction,
                bucket,
                &access,
            )?)
        },
    };
    let (mut record, _, ()) = objects::publish_in_transaction(
        transaction,
        previous.as_ref(),
        versioning,
        objects::NewVersion {
            blob_id: id,
            assignment: staged
                .import_identity
                .and_then(|identity| identity.generation())
                .map_or(objects::GenerationAssignment::Automatic, |generation| {
                    objects::GenerationAssignment::Imported(generation)
                }),
            micros,
            origin: grant.origin(),
            notification_mode: grant.notification_mode(),
            owner_profile: None,
            acl: super::super::acl::PublicationAcl::InheritDefault,
        },
        |generation| {
            let mut record = publication_record(entry, generation, defaults)?;
            objects::inherit_metadata(entry.request.mode, previous.as_ref(), &mut record);
            Ok((record, ()))
        },
    )?;
    record_publication_metadata(transaction, entry, &access, &mut record)?;
    Ok(record)
}

fn record_publication_metadata(
    transaction: &rusqlite::Transaction<'_>,
    entry: &BatchPublication,
    access: &BucketAccess,
    record: &mut ObjectRecord,
) -> Result<(), ObjectError> {
    if let Some(deleted) = entry
        .staged
        .import_identity
        .and_then(ImportedIdentity::time_deleted)
    {
        let micros = super::super::unix_micros(deleted).map_err(objects_error)?;
        transaction.prepare_cached("UPDATE objects SET time_deleted = ?4 WHERE bucket = ?1 AND name = ?2 AND generation = ?3")
            .and_then(|mut statement| statement.execute(rusqlite::params![record.bucket, record.name, record.generation, micros]))
            .map_err(objects_error)?;
        record.time_deleted = Some(deleted);
    }
    apply_acl(
        transaction,
        entry.request.metadata.acl.as_deref(),
        access,
        record,
    )?;
    record_staged_state(transaction, &entry.staged, record)
}

fn record_staged_state(
    transaction: &rusqlite::Transaction<'_>,
    staged: &StagedObject,
    record: &ObjectRecord,
) -> Result<(), ObjectError> {
    if let Some(update) = &staged.sync_update {
        super::super::sync_state::record_in_transaction(transaction, update, record)?;
    }
    if let Some(binding) = &staged.sync_resolution {
        super::super::sync_state::conflicts::complete_restore(transaction, binding, record)?;
    }
    Ok(())
}

pub(in crate::storage::catalog) fn apply_acl(
    transaction: &rusqlite::Transaction<'_>,
    entries: Option<&[crate::acl::AclEntry]>,
    access: &BucketAccess,
    record: &ObjectRecord,
) -> Result<(), ObjectError> {
    if let Some(entries) = entries {
        super::super::acl::replace_resource_acl(
            transaction,
            super::super::acl::ResolvedAclTarget::Object {
                bucket: &record.bucket,
                name: &record.name,
                generation: record.generation,
            },
            entries,
            access,
            None,
        )
        .map_err(|error| match error {
            CatalogError::Forbidden => ObjectError::Forbidden,
            CatalogError::PublicAccessPrevention => ObjectError::PublicAccessPrevention,
            CatalogError::InvalidMetadata(reason) => ObjectError::InvalidFixedMetadata(reason),
            error => objects_error(error),
        })?;
    }
    Ok(())
}
