//! A prepared fixture namespace has one catalog publication boundary.

use super::*;
use crate::storage::catalog::ConnectionUnavailable;

#[derive(Clone, Copy)]
pub(crate) enum SeedMode {
    Merge,
    Replace,
}

#[derive(Default)]
pub(crate) struct Counts {
    pub buckets: usize,
    pub objects: usize,
    pub replaced: usize,
    pub unchanged: usize,
}

pub(crate) struct SeedBatch {
    admin: Admin,
    buckets: Vec<BucketDefinition>,
    entries: Vec<BatchPublication>,
    prepared: Vec<BatchPublication>,
    activity: Option<crate::storage::catalog::Activity>,
    _admission: OwnedSemaphorePermit,
    request: Arc<OwnedSemaphorePermit>,
    runtime: tokio::runtime::Handle,
    metadata_bytes: usize,
}

impl Admin {
    pub(crate) fn seed_batch(&self) -> Result<SeedBatch, AdminError> {
        if self.catalog.sync_controls()?.is_some() {
            return Err(AdminError::Precondition);
        }
        let activity = self.catalog.activity()?;
        let request = Arc::new(
            self.catalog
                .0
                .publication_requests
                .clone()
                .try_acquire_owned()
                .map_err(|_| busy())?,
        );
        let admission = self
            .catalog
            .0
            .publication_batch
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy())?;
        Ok(SeedBatch {
            admin: self.clone(),
            buckets: Vec::new(),
            entries: Vec::new(),
            prepared: Vec::new(),
            activity: Some(activity),
            _admission: admission,
            request,
            runtime: tokio::runtime::Handle::current(),
            metadata_bytes: 0,
        })
    }
}

fn busy() -> AdminError {
    AdminError::Busy {
        retry_after: Duration::from_secs(1),
    }
}

fn metadata_size(value: &impl std::fmt::Debug) -> Result<usize, AdminError> {
    struct Size(usize);
    impl std::fmt::Write for Size {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            self.0 = self
                .0
                .checked_add(value.len())
                .filter(|size| *size <= 8 * 1024 * 1024)
                .ok_or(std::fmt::Error)?;
            Ok(())
        }
    }
    let mut size = Size(0);
    std::fmt::write(&mut size, format_args!("{value:?}"))
        .map_err(|_| AdminError::Invalid("seed metadata exceeds 8 MiB".into()))?;
    Ok(size.0)
}

impl SeedBatch {
    fn charge_metadata(&mut self, bytes: usize) -> Result<(), AdminError> {
        self.metadata_bytes = self
            .metadata_bytes
            .checked_add(bytes)
            .filter(|size| *size <= 8 * 1024 * 1024)
            .ok_or_else(|| AdminError::Invalid("seed metadata exceeds 8 MiB".into()))?;
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) async fn open_root(
        &self,
        root: std::path::PathBuf,
        inspected: std::fs::Metadata,
    ) -> Result<Arc<File>, AdminError> {
        self.admin
            .catalog
            .0
            .file_workers
            .run(move || {
                use std::os::unix::fs::MetadataExt;
                let file = fsutil::open_directory_nofollow(&root)?;
                let pinned = file.metadata()?;
                if (pinned.dev(), pinned.ino()) != (inspected.dev(), inspected.ino()) {
                    return Err(AdminError::Precondition);
                }
                Ok(Arc::new(file))
            })
            .await
    }

    #[cfg(unix)]
    pub(crate) async fn open_source(
        &self,
        root: Arc<File>,
        relative: std::path::PathBuf,
    ) -> Result<File, AdminError> {
        self.admin
            .catalog
            .0
            .file_workers
            .run(move || {
                let mut directory = root.try_clone()?;
                let mut components = relative.iter().peekable();
                while let Some(name) = components.next() {
                    if components.peek().is_none() {
                        return Ok(fsutil::open_regular_at(&directory, name)?);
                    }
                    directory =
                        fsutil::directory_at(&directory, name, fsutil::DirectoryOpen::Existing)?;
                }
                Err(AdminError::Invalid(
                    "seed source must be a regular file".into(),
                ))
            })
            .await
    }

    pub(crate) fn bucket(&mut self, definition: BucketDefinition) -> Result<(), AdminError> {
        if self.buckets.len() >= MAX_BUCKETS {
            return Err(AdminError::Invalid("seed exceeds 1000 buckets".into()));
        }
        self.charge_metadata(metadata_size(&definition.0)?)?;
        self.buckets.push(definition);
        Ok(())
    }

    pub(crate) async fn object(
        &mut self,
        bucket: BucketName,
        name: ObjectName,
        request: Publication,
        source: File,
    ) -> Result<(), AdminError> {
        if self.entries.len() >= 10_000 {
            return Err(AdminError::Invalid("seed exceeds 10000 objects".into()));
        }
        request.metadata.validate()?;
        self.charge_metadata(
            bucket.as_str().len()
                + name.as_str().len()
                + request.content_type.len()
                + metadata_size(&request.metadata)?,
        )?;
        let mut staged = self
            .admin
            .stage_from_file(source, request.expected, request.max_bytes)
            .await?;
        // The batch owns one activity/admission lease; individual 256-handle permits must not deadlock a larger tree.
        staged.permit.take();
        staged.activity.take();
        staged.batch_ownership = Some(self.request.clone());
        self.entries.push(BatchPublication {
            bucket,
            name,
            request,
            staged,
        });
        Ok(())
    }

    pub(crate) async fn commit(mut self, mode: SeedMode) -> Result<Counts, AdminError> {
        let catalog = self.admin.catalog.clone();
        while let Some(entry) = self.entries.pop() {
            self.prepared
                .push(prepare_entry(catalog.clone(), entry).await?);
        }
        std::mem::swap(&mut self.entries, &mut self.prepared);
        self.entries.reverse();
        let execution =
            tokio::time::timeout(WAIT_TIMEOUT, catalog.0.execution.clone().acquire_owned())
                .await
                .map_err(|_| busy())?
                .map_err(|_| AdminError::Closed)?;
        self.activity.take();
        let exclusive = catalog
            .0
            .activity
            .clone()
            .try_write_owned()
            .map_err(|_| busy())?;
        let inner = catalog.0.clone();
        let (outcome, mut batch) = catalog
            .0
            .executor
            .run(move || {
                let (_execution, _exclusive) = (execution, exclusive);
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Debug,
                    "seed_commit_started",
                    serde_json::json!({}),
                );
                let outcome =
                    match inner.with_connection(|connection| commit(connection, &self, mode)) {
                        Ok(outcome) => outcome,
                        Err(ConnectionUnavailable::Closed) => Err(AdminError::Closed),
                        Err(error) => Err(AdminError::backend(error)),
                    };
                (outcome, self)
            })
            .await
            .map_err(AdminError::backend)?;
        // Reference-aware cleanup also handles a commit whose caller was cancelled or whose result was ambiguous.
        let entries = std::mem::take(&mut batch.entries);
        let ownership = batch.request.clone();
        let cleanup = crate::task::spawn_supervised("seed-batch-cleanup", async move {
            let _ownership = ownership;
            cleanup_entries(entries).await
        });
        if let Err(error) = cleanup.await.map_err(AdminError::backend)? {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "seed_cleanup_deferred",
                serde_json::json!({"message":error.to_string()}),
            );
        }
        catalog.reclaim_after_commit().await;
        outcome
    }
}

impl Drop for SeedBatch {
    fn drop(&mut self) {
        let mut entries = std::mem::take(&mut self.entries);
        entries.append(&mut self.prepared);
        if entries.is_empty() {
            return;
        }
        let ownership = self.request.clone();
        let activity = self.activity.take();
        crate::task::spawn_supervised_on(&self.runtime, "seed-batch-abort-cleanup", async move {
            let (_ownership, _activity) = (ownership, activity);
            if let Err(error) = cleanup_entries(entries).await {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    "seed_cleanup_deferred",
                    serde_json::json!({"message":error.to_string()}),
                );
            }
        });
    }
}

async fn prepare_entry(
    catalog: Catalog,
    mut entry: BatchPublication,
) -> Result<BatchPublication, AdminError> {
    if entry.staged.published {
        return Ok(entry);
    }
    crate::task::spawn_supervised("seed-prepare-object", async move {
        let _activity = catalog.activity()?;
        let staged = &mut entry.staged;
        let id = staged
            .id
            .as_ref()
            .ok_or_else(|| AdminError::Invalid("resolved staging handle".into()))?;
        let source = catalog.0.data_dir.join("staging").join(id);
        let destination = objects::blob_path(&catalog.0.data_dir, id).map_err(AdminError::from)?;
        #[cfg(test)]
        super::super::super::fault::hit(
            &catalog.0.data_dir,
            super::super::super::fault::Step::BeforePublish,
        )
        .map_err(objects_error)?;
        catalog
            .0
            .file_workers
            .run(move || {
                objects::publish_blob(&source, &destination)?;
                Ok(())
            })
            .await?;
        #[cfg(test)]
        super::super::super::fault::hit(
            &catalog.0.data_dir,
            super::super::super::fault::Step::AfterPublish,
        )
        .map_err(objects_error)?;
        staged.published = true;
        Ok(entry)
    })
    .await
    .map_err(AdminError::backend)?
}

fn check_guards(
    transaction: &rusqlite::Transaction<'_>,
    mode: SeedMode,
    clock: &dyn Clock,
) -> Result<(), AdminError> {
    let importing: bool = transaction
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM import_journal WHERE active = 1)")
        .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
        .map_err(objects_error)?;
    if importing {
        return Err(AdminError::Precondition);
    }
    if matches!(mode, SeedMode::Replace) {
        let syncing: bool = transaction
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_roots)")
            .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
            .map_err(objects_error)?;
        if syncing {
            return Err(AdminError::Precondition);
        }
        let now = crate::storage::catalog::unix_micros(clock.now())?;
        let held: bool = transaction.prepare_cached("SELECT EXISTS(SELECT 1 FROM objects JOIN buckets ON objects.bucket = buckets.name WHERE event_based_hold = 1 OR temporary_hold = 1 OR objects.time_created + json_extract(buckets.retention_policy, '$.retention_period') * 1000000 > ?1 OR retain_until_time > ?1)")
            .and_then(|mut statement| statement.query_row([now], |row| row.get(0))).map_err(objects_error)?;
        if held {
            return Err(AdminError::Hold);
        }
    }
    Ok(())
}

fn commit(
    connection: &mut Connection,
    batch: &SeedBatch,
    mode: SeedMode,
) -> Result<Counts, AdminError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(objects_error)?;
    let clock = batch.admin.catalog.0.clock.as_ref();
    check_guards(&transaction, mode, clock)?;
    let epoch = if matches!(mode, SeedMode::Replace) {
        let reclamation = if batch.admin.catalog.0.memory.is_some() {
            super::super::reset::Reclamation::ClearMemory
        } else {
            super::super::reset::Reclamation::Journal
        };
        Some(super::super::reset::clear_in_transaction(&transaction, reclamation)?.2)
    } else {
        None
    };
    let mut counts = Counts::default();
    for definition in &batch.buckets {
        let record = &definition.0;
        match crate::storage::catalog::select_bucket(&transaction, &record.name)
            .map_err(objects_error)?
        {
            Some(existing) if existing.project != record.project => {
                return Err(AdminError::Invalid(
                    "existing seed bucket belongs to another project".into(),
                ));
            }
            Some(_) => {}
            None => {
                crate::storage::catalog::insert_bucket_in_transaction(
                    &transaction,
                    &mut record.clone(),
                )?;
                crate::storage::catalog::change_journal::append(
                    &transaction,
                    batch.admin.origin(),
                    crate::storage::catalog::change_journal::ChangeKind::BucketCreate,
                    &record.name,
                    None,
                )?;
                counts.buckets += 1;
            }
        }
    }
    let mut published = Vec::new();
    for entry in &batch.entries {
        validate_staged_entry(entry)?;
        let previous = objects::lookup(&transaction, entry.bucket.as_str(), entry.name.as_str())?;
        if previous
            .as_ref()
            .is_some_and(|(live, _)| matches_seed(live, entry))
        {
            counts.unchanged += 1;
            continue;
        }
        publish_entry(&transaction, entry, &batch.admin.grant, clock)?;
        published.push(
            entry
                .staged
                .id
                .as_deref()
                .ok_or_else(|| AdminError::Invalid("resolved staging handle".into()))?,
        );
        if previous.is_some() {
            counts.replaced += 1;
        } else {
            counts.objects += 1;
        }
    }
    transaction
        .prepare_cached("UPDATE startup_seed SET complete = 1 WHERE singleton = 1")
        .and_then(|mut statement| statement.execute([]))
        .map_err(objects_error)?;
    if let Err(error) = objects::commit_publication(transaction)
        && !confirmed(connection, batch, &published, epoch)?
    {
        return Err(error.into());
    }
    if matches!(mode, SeedMode::Replace)
        && let Some(memory) = &batch.admin.catalog.0.memory
    {
        // Only the bounded prepared namespace is indexed; a full old catalog needs no growing deletion journal.
        let selected = batch
            .entries
            .iter()
            .filter_map(|entry| entry.staged.id.as_deref())
            .collect();
        if let Err(error) = memory.retain_ids(&selected) {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Error,
                "seed_cleanup_deferred",
                serde_json::json!({"message":error.to_string()}),
            );
        }
    }
    Ok(counts)
}

fn confirmed(
    connection: &Connection,
    batch: &SeedBatch,
    ids: &[&str],
    epoch: Option<i64>,
) -> Result<bool, AdminError> {
    let (complete, current): (bool, i64) = connection.prepare_cached("SELECT complete, epoch FROM startup_seed, change_journal_state WHERE startup_seed.singleton = 1 AND change_journal_state.singleton = 1")
        .and_then(|mut statement| statement.query_row([], |row| Ok((row.get(0)?, row.get(1)?)))).map_err(objects_error)?;
    if !complete || epoch.is_some_and(|epoch| epoch != current) {
        return Ok(false);
    }
    for id in ids {
        let exists: bool = connection
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)")
            .and_then(|mut statement| statement.query_row([id], |row| row.get(0)))
            .map_err(objects_error)?;
        if !exists {
            return Ok(false);
        }
    }
    for bucket in &batch.buckets {
        if crate::storage::catalog::select_bucket(connection, &bucket.0.name)
            .map_err(objects_error)?
            .is_none()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn matches_seed(live: &ObjectRecord, entry: &BatchPublication) -> bool {
    live.size == entry.staged.size
        && live.md5 == entry.staged.hashes.md5
        && live.crc32c == entry.staged.hashes.crc32c
        && live.content_type == entry.request.content_type
        && live.content_encoding.is_none()
        && live.content_disposition.is_none()
        && live.content_language.is_none()
        && live.cache_control.is_none()
        && live.custom_time.is_none()
        && live.metadata.is_empty()
}

#[cfg(test)]
mod tests;
