#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::objects::object_unavailable as objects_error;
use super::replay::Fingerprint;
use super::*;
use crate::storage::blob_id::BlobId;
use crate::storage::catalog::sql;
use crate::{fsutil, hashing::Hasher};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::mpsc::TrySendError,
    time::SystemTime,
};
use tokio::sync::oneshot;

#[cfg(test)]
mod tests;

mod reseed;
pub(crate) use reseed::{LiveOutcome, LiveWalk, Removed, SeedBatch, SeedMode};

#[cfg(test)]
thread_local! {
    static COPY_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static FORCE_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

type FileJob = Box<dyn FnOnce() + Send>;
thread_local! { static IN_FILE_JOB: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[derive(Debug)]
pub(in crate::storage) struct NestedFileJob;
impl std::fmt::Display for NestedFileJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("file worker job started another file worker job")
    }
}
impl std::error::Error for NestedFileJob {}
struct InFileJob;
impl Drop for InFileJob {
    fn drop(&mut self) {
        IN_FILE_JOB.set(false);
    }
}

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

    pub(crate) async fn run<T: Send + 'static, E: From<AdminError> + Send + 'static>(
        &self,
        job: impl FnOnce() -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        if IN_FILE_JOB.get() {
            return Err(AdminError::backend(NestedFileJob).into());
        }
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
                IN_FILE_JOB.set(true);
                let _mark = InFileJob;
                // A panicking job must neither end its worker, which would shrink the pool for
                // every later job, nor leave its caller without a reply.
                let result = crate::task::catch_job(FILE_WORKER, job)
                    .unwrap_or_else(|panicked| Err(AdminError::backend(panicked).into()));
                let _ = sender.send(result);
            }))
            .map_err(|error| AdminError::backend(without_job(&error)))?;
        receiver.await.map_err(AdminError::backend)?
    }
}

/// Verified private bytes; dropping schedules owned cleanup, never synchronous I/O.
pub(crate) struct StagedObject {
    catalog: Catalog,
    runtime: tokio::runtime::Handle,
    /// `Some` from staging until `cleanup` or `Drop` takes the bytes; both consume the handle.
    owned: Option<OwnedBlob>,
    size: u64,
    hashes: crate::hashing::Hashes,
    /// What the source file was when staging read it.
    source: Fingerprint,
    /// Whether the draft already sits at its blob path (always true in RAM).
    published: bool,
    tracking: Tracking,
}

/// The staged bytes and the admission that keeps them counted until publication or cleanup.
struct OwnedBlob {
    id: BlobId,
    _lease: StagedLease,
}

/// What keeps a staged body admitted.
enum StagedLease {
    /// `stage_from_file`: one staged-handle permit and one activity lease per object.
    Own {
        _permit: OwnedSemaphorePermit,
        _activity: super::super::Activity,
    },
    /// A seed batch: one admission shared by every object it staged.
    Batch {
        _admission: Arc<OwnedSemaphorePermit>,
    },
}

/// What a publication records besides the object, chosen by the caller that staged the body.
enum Tracking {
    /// An ordinary administrative publication.
    Plain,
    /// Offline import keeps the source's generation and timestamps.
    Imported(ImportedIdentity),
}

/// What staging produced.
#[derive(Clone, Copy)]
struct Staged {
    size: u64,
    hashes: crate::hashing::Hashes,
    /// What the source was just before staging read it. Staging fails unless the source still
    /// is that after the read, so this describes exactly the bytes staged.
    source: Fingerprint,
}

impl StagedObject {
    fn new(catalog: Catalog, id: BlobId, lease: StagedLease, staged: Staged) -> Self {
        let published = catalog.0.backend.is_memory();
        Self {
            catalog,
            runtime: tokio::runtime::Handle::current(),
            owned: Some(OwnedBlob { id, _lease: lease }),
            size: staged.size,
            hashes: staged.hashes,
            source: staged.source,
            published,
            tracking: Tracking::Plain,
        }
    }
    /// Seed batches share one admission so that 256 handle permits cannot deadlock a larger tree.
    pub(super) fn into_batch(mut self, admission: Arc<OwnedSemaphorePermit>) -> Self {
        if let Some(owned) = &mut self.owned {
            owned._lease = StagedLease::Batch {
                _admission: admission,
            };
        }
        self
    }
    #[expect(
        clippy::expect_used,
        reason = "only `cleanup` and `Drop` take the blob, and both consume the handle"
    )]
    fn blob_id(&self) -> BlobId {
        self.owned
            .as_ref()
            .map(|owned| owned.id)
            .expect("a live staged handle owns its blob")
    }
    fn import_identity(&self) -> Option<ImportedIdentity> {
        match &self.tracking {
            Tracking::Imported(identity) => Some(*identity),
            Tracking::Plain => None,
        }
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
    #[cfg_attr(not(test), expect(dead_code, reason = "the seed watcher reuses this"))]
    pub(crate) fn hashes(&self) -> crate::hashing::Hashes {
        self.hashes
    }
    /// What the source file was when staging read it, which describes exactly the bytes staged.
    pub(crate) fn fingerprint(&self) -> Fingerprint {
        self.source
    }

    async fn cleanup(mut self) -> Result<(), AdminError> {
        // The lease outlives the removal, so admission counts the bytes until they are gone.
        if let Some(OwnedBlob { id, _lease }) = self.owned.take() {
            cleanup(&self.catalog, id).await?;
        }
        Ok(())
    }
}

impl Drop for StagedObject {
    fn drop(&mut self) {
        let Some(owned) = self.owned.take() else {
            return;
        };
        let catalog = self.catalog.clone();
        crate::task::spawn_supervised_on(&self.runtime, "staged-object-cleanup", async move {
            let OwnedBlob { id, _lease } = owned;
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

fn changed(before: fsutil::FileIdentity, after: fsutil::FileIdentity) -> Result<(), AdminError> {
    let fields = before.changed_fields(&after);
    if fields.is_empty() {
        Ok(())
    } else {
        Err(AdminError::SourceChanged {
            fields: fields.iter().collect(),
        })
    }
}

#[derive(Debug)]
enum StageFailure {
    NotCreated(AdminError),
    Created(AdminError),
}

/// A failure before the staging file exists, including a failed file worker, leaves nothing for
/// the handle to clean up.
impl From<AdminError> for StageFailure {
    fn from(error: AdminError) -> Self {
        Self::NotCreated(error)
    }
}

struct StageRequest<'a> {
    source: &'a File,
    stage: &'a Path,
    expected: ExpectedHashes,
    max_bytes: u64,
}

/// How the staging file got its bytes.
#[derive(Clone, Copy)]
enum Fill {
    /// The filesystem cloned the source; the bytes are only read back and hashed.
    Cloned,
    /// The source is copied into a newly created file.
    Copied,
}

fn stage_file(
    source: &File,
    stage: &Path,
    expected: ExpectedHashes,
    max_bytes: u64,
) -> Result<Staged, StageFailure> {
    let request = StageRequest {
        source,
        stage,
        expected,
        max_bytes,
    };
    let (target, fill, before) = create_stage(&request)?;
    fill_stage(&request, target, fill, before).map_err(StageFailure::Created)
}

/// What the source is before staging reads it, once it is known to be a regular file within the
/// limit.
fn checked_source(request: &StageRequest<'_>) -> Result<Fingerprint, AdminError> {
    if !request.source.metadata()?.is_file() {
        return Err(AdminError::Invalid("expected a regular source file".into()));
    }
    let before = source_fingerprint(request.source)?;
    if before.identity().size > request.max_bytes {
        return Err(AdminError::TooLarge);
    }
    Ok(before)
}

/// What `source` is as its filesystem reports it now, past an attribute cache that a rename made
/// on another host does not invalidate. The system wall-clock time is read before the stat, as
/// [`Fingerprint::new`] requires. Taken just before a read that [`unchanged_since`] then
/// confirms, it describes exactly the bytes read.
fn source_fingerprint(source: &File) -> Result<Fingerprint, AdminError> {
    let observed = SystemTime::now();
    Ok(Fingerprint::new(
        fsutil::uncached_identity(source)?,
        observed,
    ))
}

/// Fails with `SourceChanged` unless `source` still is what `before` reported.
fn unchanged_since(source: &File, before: &Fingerprint) -> Result<(), AdminError> {
    changed(before.identity(), fsutil::uncached_identity(source)?)
}

/// Checks the source and creates the staging file; failures before creation leave nothing to clean.
fn create_stage(request: &StageRequest<'_>) -> Result<(File, Fill, Fingerprint), StageFailure> {
    let StageRequest { source, stage, .. } = *request;
    let before = checked_source(request)?;
    let parent = stage
        .parent()
        .ok_or_else(|| AdminError::Invalid("missing staging directory".into()))?;
    let name = stage
        .file_name()
        .ok_or_else(|| AdminError::Invalid("missing staging name".into()))?;
    let clone = || fsutil::clone_into(source, &fsutil::open_directory(parent)?, name);
    #[cfg(test)]
    let cloned = if FORCE_COPY.get() {
        Err(fsutil::CloneFailure::NotCreated(
            fsutil::FsError::Unsupported,
        ))
    } else {
        clone()
    };
    #[cfg(not(test))]
    let cloned = clone();
    let fill = match cloned {
        Ok(()) => Fill::Cloned,
        Err(fsutil::CloneFailure::NotCreated(fsutil::FsError::Unsupported)) => Fill::Copied,
        Err(fsutil::CloneFailure::NotCreated(error)) => return Err(AdminError::from(error).into()),
        Err(fsutil::CloneFailure::Created(error)) => {
            return Err(StageFailure::Created(error.into()));
        }
    };
    let target = match fill {
        Fill::Cloned => File::open(stage).map_err(|error| StageFailure::Created(error.into()))?,
        Fill::Copied => OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(stage)
            .map_err(AdminError::from)?,
    };
    Ok((target, fill, before))
}

/// Fills, flushes and syncs the created file, then checks the source and the hashes.
fn fill_stage(
    request: &StageRequest<'_>,
    mut target: File,
    fill: Fill,
    before: Fingerprint,
) -> Result<Staged, AdminError> {
    let StageRequest {
        source,
        expected,
        max_bytes,
        ..
    } = *request;
    let mut hasher = Hasher::default();
    let mut size = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = match fill {
            Fill::Cloned => target.read(&mut buffer),
            Fill::Copied => fsutil::read_at(source, &mut buffer, size),
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
        let bytes = buffer
            .get(..count)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        if matches!(fill, Fill::Copied) {
            #[cfg(test)]
            super::super::fault::hit(
                request
                    .stage
                    .parent()
                    .and_then(Path::parent)
                    .ok_or_else(|| AdminError::Invalid("missing data directory".into()))?,
                super::super::fault::Step::BodyWrite,
            )
            .map_err(objects_error)?;
            target.write_all(bytes)?;
        }
        hasher.update(bytes);
        #[cfg(test)]
        COPY_HOOK.with_borrow_mut(|hook| {
            if let Some(hook) = hook.take() {
                hook();
            }
        });
    }
    #[cfg(test)]
    let data_dir = request
        .stage
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
    unchanged_since(source, &before)?;
    let hashes = hasher.finish();
    expected.verify(&hashes).map_err(|_| AdminError::Checksum)?;
    Ok(Staged {
        size,
        hashes,
        source: before,
    })
}

impl super::super::BlobBackend {
    // A collision must not give the cleanup handle ownership of another stage.
    fn stage_admin_file(
        &self,
        source: &File,
        id: BlobId,
        expected: ExpectedHashes,
        max_bytes: u64,
    ) -> Result<Staged, StageFailure> {
        match self {
            Self::Memory(memory) => Ok(stage_memory(memory, source, id, expected, max_bytes)?),
            Self::Disk(root) => {
                let stage = id.staging_path(root);
                if id.blob_path(root).try_exists().map_err(AdminError::from)?
                    || stage.try_exists().map_err(AdminError::from)?
                {
                    return Err(AdminError::Invalid("blob ID collision".into()).into());
                }
                stage_file(source, &stage, expected, max_bytes)
            }
        }
    }
}

async fn cleanup(catalog: &Catalog, id: BlobId) -> Result<(), AdminError> {
    let probe = id;
    let referenced = catalog
        .run_upload(move |connection| {
            objects::blob_referenced(connection, &probe).map_err(objects_error)
        })
        .await?;
    if catalog.0.backend.is_memory() {
        if !referenced {
            catalog.0.backend.remove(&id)?;
        }
        return Ok(());
    }
    let backend = catalog.0.backend.clone();
    catalog
        .0
        .file_workers
        .run(move || {
            backend.remove_draft(&id)?;
            if !referenced {
                backend.remove(&id)?;
                #[cfg(unix)]
                backend.sync_blob_dir(&id)?;
            }
            Ok(())
        })
        .await
}

impl Admin {
    pub(crate) async fn stage_from_file<S: std::borrow::Borrow<File> + Send + 'static>(
        &self,
        source: S,
        expected: ExpectedHashes,
        max_bytes: u64,
    ) -> Result<StagedObject, AdminError> {
        let deadline = self.catalog.admin_deadline();
        // A seed batch or an import stages each file under the leases of the files before it, so
        // lining up behind a reservation here could wait for itself.
        let activity = self
            .catalog
            .admin_activity(Work::UnderWay, deadline)
            .await?;
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
            let id = BlobId::generate().map_err(AdminError::backend)?;
            let backend = catalog.0.backend.clone();
            let staged = catalog
                .0
                .file_workers
                .run(move || backend.stage_admin_file(source.borrow(), id, expected, max_bytes))
                .await;
            // The permit and activity lease stay held until any cleanup below has finished.
            match staged {
                Ok(staged) => {
                    let lease = StagedLease::Own {
                        _permit: permit,
                        _activity: activity,
                    };
                    Ok(StagedObject::new(catalog, id, lease, staged))
                }
                Err(StageFailure::NotCreated(error)) => Err(error),
                Err(StageFailure::Created(error)) => {
                    cleanup(&catalog, id).await?;
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
        for entry in &entries {
            entry
                .identity
                .validate_publication(&entry.publication.request)?;
        }
        let sources = entries
            .iter()
            .map(|entry| (entry.identity, entry.element.clone()))
            .collect::<Vec<_>>();
        let elements = sources
            .iter()
            .map(|(_, element)| element.clone())
            .collect::<Vec<_>>();
        let publications = entries
            .into_iter()
            .map(|mut entry| {
                entry.publication.staged.tracking = Tracking::Imported(entry.identity);
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
                    elements,
                }),
            )
            .await?;
        let mut unattempted = Vec::new();
        for (mut publication, (identity, element)) in batch
            .unattempted
            .into_iter()
            .zip(sources.into_iter().skip(batch.results.len()))
        {
            // A returned body carries no identity, so the caller can stage it again as given.
            publication.staged.tracking = Tracking::Plain;
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
        let deadline = self.catalog.admin_deadline();
        let request = self
            .catalog
            .wait_admin(self.catalog.0.publication_requests.clone(), deadline)
            .await?;
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
    id: BlobId,
    expected: ExpectedHashes,
    max_bytes: u64,
) -> Result<Staged, AdminError> {
    if !source.metadata()?.is_file() {
        return Err(AdminError::Invalid("expected a regular source file".into()));
    }
    let before = source_fingerprint(source)?;
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
        let bytes = buffer
            .get(..count)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        stage.write(bytes)?;
        hasher.update(bytes);
    }
    unchanged_since(source, &before)?;
    let hashes = hasher.finish();
    expected.verify(&hashes).map_err(|_| AdminError::Checksum)?;
    stage.publish(id.to_hex())?;
    Ok(Staged {
        size,
        hashes,
        source: before,
    })
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
            let id = entry.staged.blob_id();
            let backend = catalog.0.backend.clone();
            catalog
                .0
                .file_workers
                .run(move || {
                    backend.publish_draft(&id)?;
                    Ok::<_, AdminError>(())
                })
                .await?;
            entry.staged.published = true;
        }
        // Accepted batches retain ownership through shutdown; only new admission is closed.
        tokio::time::timeout_at(deadline, catalog.0.execution.clone().acquire_owned())
            .await
            .map_err(|_| AdminError::busy())?
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
        .run_upload_admitted::<_, ObjectError>(execution, move |connection| {
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
        .await?;
    let committed = match outcome {
        Ok(committed) => committed,
        Err(error) => {
            cleanup_entries(entries).await?;
            return Err(error.into());
        }
    };
    // Any element may have replaced a version in a bucket that retains deletions.
    catalog.0.maintenance_wake.notify_one();
    let unattempted = entries.split_off(committed.results.len());
    cleanup_entries(entries).await?;
    catalog.reclaim_after_commit().await;
    Ok(BatchResult {
        results: committed.results,
        unattempted,
        budget_exceeded: committed.budget_exceeded,
    })
}

/// Why a publication batch failed as a whole.
enum BatchError {
    /// The catalog failed; it is classified like any other storage failure when it leaves the
    /// batch.
    Store(ObjectError),
    /// An element failed in a way that aborts the batch; already classified.
    Element(AdminError),
}

impl From<ObjectError> for BatchError {
    fn from(error: ObjectError) -> Self {
        Self::Store(error)
    }
}

impl From<rusqlite::Error> for BatchError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.into())
    }
}

impl From<CatalogError> for BatchError {
    fn from(error: CatalogError) -> Self {
        Self::Store(error.into())
    }
}

impl From<BatchError> for AdminError {
    fn from(error: BatchError) -> Self {
        match error {
            BatchError::Store(error) => error.into(),
            BatchError::Element(error) => error,
        }
    }
}

/// An import journal that moved on refuses the batch as a failed precondition.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other catalog error keeps the shared object mapping"
)]
fn journal_check_error(error: CatalogError) -> ObjectError {
    match error {
        CatalogError::PreconditionFailed => ObjectError::PreconditionFailed,
        error => objects_error(error),
    }
}

/// The attempted prefix of a batch; `results.len()` elements were attempted.
struct Committed {
    results: Vec<Result<ObjectRecord, AdminError>>,
    budget_exceeded: bool,
}

fn commit_batch(
    connection: &mut Connection,
    entries: &[BatchPublication],
    grant: &AdminGrant,
    clock: &dyn Clock,
    budget: Duration,
    import: Option<&ImportBatch>,
) -> Result<Committed, BatchError> {
    let transaction = sql::immediate(connection)?;
    let started = std::time::Instant::now();
    if let Some(import) = import {
        import_journal::check_publication(&transaction, &import.binding, import.cursor)
            .map_err(journal_check_error)?;
    }
    let mut results = Vec::new();
    for entry in entries {
        if !results.is_empty() && started.elapsed() >= budget {
            break;
        }
        transaction.execute_batch("SAVEPOINT publication_element")?;
        match publish_entry(&transaction, entry, grant, clock.now()) {
            Ok(record) => {
                transaction.execute_batch("RELEASE publication_element")?;
                results.push(Ok(record));
            }
            Err(error) => match error {
                error @ (AdminError::Precondition
                | AdminError::Hold
                | AdminError::NotFound
                | AdminError::Invalid(_)
                | AdminError::Forbidden
                | AdminError::Checksum
                | AdminError::TooLarge) => {
                    transaction.execute_batch(
                        "ROLLBACK TO publication_element; RELEASE publication_element",
                    )?;
                    results.push(Err(error));
                }
                error @ (AdminError::Busy { .. }
                | AdminError::DiskFull(_)
                | AdminError::Closed
                | AdminError::SeedProjectMismatch
                | AdminError::SeedMetadataBudget { .. }
                | AdminError::SourceChanged { .. }
                | AdminError::Corrupt(_)
                | AdminError::Io(_)
                | AdminError::BodyAborted(_)
                | AdminError::Backend(_)) => return Err(BatchError::Element(error)),
            },
        }
    }
    if let Some(import) = import {
        let elements = import
            .elements
            .get(..results.len())
            .ok_or_else(|| ObjectError::Corrupt("missing import publication elements".into()))?;
        let outcomes = elements
            .iter()
            .zip(&results)
            .map(|(element, result)| import_journal::ElementResult {
                element,
                outcome: import_journal::ElementOutcome::of(result),
            })
            .collect::<Vec<_>>();
        import_journal::record_outcomes(&transaction, &import.binding, import.cursor, &outcomes)?;
    }
    transaction.commit()?;
    Ok(Committed {
        results,
        budget_exceeded: started.elapsed() > budget,
    })
}

/// Checks the request's metadata, size limit and declared hashes against the staged body.
fn validate_staged_entry(entry: &BatchPublication) -> Result<(), ObjectError> {
    crate::object::validate_fixed_metadata("contentType", &entry.request.content_type)?;
    entry.request.metadata.validate()?;
    if entry.staged.size > entry.request.max_bytes {
        return Err(ObjectError::TooLarge);
    }
    entry.request.expected.verify(&entry.staged.hashes)?;
    Ok(())
}

fn publication_record(
    entry: &BatchPublication,
    generation: i64,
    defaults: objects::PublicationDefaults,
) -> Result<ObjectRecord, ObjectError> {
    let mut record = defaults.record(
        ObjectTarget::new(entry.bucket.as_str(), entry.name.as_str(), generation),
        entry.request.content_type.clone(),
        entry.staged.size,
        entry.staged.hashes,
    );
    entry.request.metadata.clone().apply(&mut record)?;
    if let Some(identity) = entry.staged.import_identity() {
        record.time_created = identity.time_created();
        record.updated = identity.updated();
    }
    Ok(record)
}

/// Publishes one element at `now`. Its failure says whether only this element fails or the batch
/// stops, so it is an administrative failure.
fn publish_entry(
    transaction: &rusqlite::Transaction<'_>,
    entry: &BatchPublication,
    grant: &AdminGrant,
    now: SystemTime,
) -> Result<ObjectRecord, AdminError> {
    let micros = checked_entry_micros(entry, now)?;
    check_import_requirements(entry)?;
    Ok(publish_checked_entry(
        transaction,
        entry,
        grant,
        now,
        micros,
    )?)
}

/// The only request an import builds: it names both hashes and replaces an absent object. An
/// element marked imported that differs is refused before it publishes its recorded identity.
fn check_import_requirements(entry: &BatchPublication) -> Result<(), AdminError> {
    let request = &entry.request;
    if entry.staged.import_identity().is_some()
        && (request.expected.md5.is_none()
            || request.expected.crc32c.is_none()
            || request.mode != PublishMode::Replace
            || request.condition != Some(GenerationCondition::Absent))
    {
        return Err(AdminError::Invalid(
            "imported publication requires MD5, CRC32C, Replace mode and an absent destination"
                .into(),
        ));
    }
    Ok(())
}

/// `time` at the whole microsecond that a publication records. Publication checks protection at
/// the time it records, so a decision made at this instant is the one publication meets.
fn recorded_instant(time: SystemTime) -> Result<SystemTime, ObjectError> {
    let micros = super::super::unix_micros(time)?;
    Ok(super::super::system_time_micros(micros)?)
}

/// The publication time in catalog microseconds, once the staged body passed its checks.
fn checked_entry_micros(entry: &BatchPublication, now: SystemTime) -> Result<i64, ObjectError> {
    now.duration_since(std::time::UNIX_EPOCH)
        .map_err(objects_error)?;
    let micros = super::super::unix_micros(now)?;
    validate_staged_entry(entry)?;
    Ok(micros)
}

fn publish_checked_entry(
    transaction: &rusqlite::Transaction<'_>,
    entry: &BatchPublication,
    grant: &AdminGrant,
    now: SystemTime,
    micros: i64,
) -> Result<ObjectRecord, ObjectError> {
    let access = grant.access();
    let bucket = entry.bucket.as_str();
    let name = entry.name.as_str();
    let (_, delete_permission) = objects::authorized_bucket(transaction, bucket, &access)?;
    let publication = objects::BucketPublication::load(transaction, bucket)?;
    let storage_class = publication.storage_class(entry.request.metadata.storage_class)?;
    let previous = objects::lookup(transaction, bucket, name)?;
    objects::check_publication(
        previous.as_ref(),
        entry
            .request
            .condition
            .map_or_default(GenerationCondition::conditions),
        delete_permission,
        now,
    )?;
    let staged = &entry.staged;
    if objects::skip_same(
        entry.request.mode,
        previous.as_ref(),
        staged.size,
        staged.hashes.md5,
        staged.hashes.crc32c,
    ) {
        return previous
            .map(|stored| stored.record)
            .ok_or(ObjectError::ObjectNotFound);
    }
    let time = super::super::system_time_micros(micros)?;
    let defaults = objects::PublicationDefaults {
        storage_class,
        time,
        event_based_hold: staged.import_identity().is_none() && publication.default_event_hold,
        owner: if staged.import_identity().is_some() {
            None
        } else {
            Some(super::super::object_owner::for_publication(
                transaction,
                bucket,
                &access,
                None,
            )?)
        },
    };
    let objects::PublishedVersion { mut record, .. } = objects::publish_version(
        transaction,
        previous.as_ref(),
        &publication,
        objects::NewVersion {
            blob_id: staged.blob_id(),
            assignment: staged
                .import_identity()
                .and_then(ImportedIdentity::generation)
                .map_or(objects::GenerationAssignment::Automatic, |generation| {
                    objects::GenerationAssignment::Imported(generation)
                }),
            micros,
            notification_mode: grant.notification_mode(),
            owner_profile: None,
            acl: super::super::acl::PublicationAcl::InheritDefault,
        },
        |generation| {
            let mut record = publication_record(entry, generation, defaults)?;
            objects::inherit_metadata(entry.request.mode, previous.as_ref(), &mut record);
            Ok(record)
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
        .import_identity()
        .and_then(ImportedIdentity::time_deleted)
    {
        let micros = super::super::unix_micros(deleted)?;
        sql::exec(
            transaction,
            "UPDATE objects SET time_deleted = ?4 WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
            rusqlite::params![record.bucket, record.name, record.generation, micros],
        )?;
        record.time_deleted = Some(deleted);
    }
    apply_acl(
        transaction,
        entry.request.metadata.acl.entries(),
        access,
        record,
    )
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
        .map_err(objects::acl_update_error)?;
    }
    Ok(())
}
