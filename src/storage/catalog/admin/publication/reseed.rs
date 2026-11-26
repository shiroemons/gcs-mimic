//! A prepared fixture namespace has one catalog publication boundary.

use super::*;
use crate::{
    fsutil::Vacant,
    storage::catalog::{
        admin::{
            initialization::Initialization,
            replay::{
                self, Attributes, Body, Fingerprint, LiveObject, Phase, Protection, Replay,
                SeedRecord, Unproven, Vanished, Version,
            },
            seed_records::{self, Mark, RecordKey},
        },
        sql,
    },
};
use std::{collections::HashMap, num::NonZeroUsize};

mod live;
pub(crate) use live::{LiveOutcome, LiveWalk};

#[derive(Clone, Copy)]
pub(crate) enum SeedMode {
    /// Publishes over what the catalog holds: the startup replay and a disk reseed.
    Merge,
    /// Clears the catalog first unless an object is protected: a RAM reseed.
    Replace,
    /// Clears the catalog first whatever protects its objects, as a reset does, then applies
    /// the batch's bucket initialization before publishing: a reset to the startup state.
    Restore,
}

impl SeedMode {
    fn clears(self) -> bool {
        match self {
            Self::Merge => false,
            Self::Replace | Self::Restore => true,
        }
    }
}

#[derive(Default)]
pub(crate) struct Counts {
    pub buckets: usize,
    pub objects: usize,
    pub replaced: usize,
    pub unchanged: usize,
    /// What a clearing commit removed before it published; zero for a merge.
    pub removed: Removed,
    /// Whether a clearing commit left the bodies it removed for maintenance to reclaim.
    pub cleanup_pending: bool,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub(crate) struct Removed {
    pub buckets: i64,
    pub objects: i64,
}

/// One staged fixture file.
struct SeedEntry {
    publication: BatchPublication,
    /// What the fixture file was when staging read the bytes the entry publishes, which the
    /// record of the replay keeps.
    fingerprint: Fingerprint,
}

/// The publication of a staged fixture, for the cleanup that reclaims what it staged.
impl From<SeedEntry> for BatchPublication {
    fn from(entry: SeedEntry) -> Self {
        entry.publication
    }
}

#[must_use = "dropping an unfinished batch schedules cleanup of its staged data"]
pub(crate) struct SeedBatch {
    admin: Admin,
    buckets: Vec<BucketDefinition>,
    entries: Vec<SeedEntry>,
    prepared: Vec<SeedEntry>,
    activity: Option<crate::storage::catalog::Activity>,
    _admission: OwnedSemaphorePermit,
    request: Arc<OwnedSemaphorePermit>,
    runtime: tokio::runtime::Handle,
    metadata_bytes: usize,
    metadata_budget: Option<usize>,
    request_deadline: Option<tokio::time::Instant>,
    /// What seeding recorded when the batch read the records, which only a batch that walks a
    /// fixture tree does; `None` for one that does not, which keeps the records and marks as
    /// they are.
    records: Option<HashMap<RecordKey, SeedRecord>>,
    /// The directories whose buckets seeding took, as the batch read them with the records.
    marks: HashMap<String, Mark>,
    /// The recorded names whose fixture files a lookup proved absent.
    absences: Vec<(RecordKey, Vacant)>,
    /// The recorded names the walk did not list where the lookup after it could not tell
    /// whether the fixture file is there, each with the reason.
    unproven: Vec<(RecordKey, String)>,
}

/// A batch holds every object's metadata in memory until it commits. A limit the operator chose
/// bounds that memory, scaled so that fixed bytes do not refuse a tree long before its entry
/// limit; without a limit, the size of the tree is all that bounds it.
fn metadata_budget(max_entries: Option<NonZeroUsize>) -> Option<usize> {
    const BYTES_PER_10K_ENTRIES: usize = 8 * 1024 * 1024;
    max_entries.map(|entries| {
        entries
            .get()
            .div_ceil(10_000)
            .saturating_mul(BYTES_PER_10K_ENTRIES)
    })
}

/// A caller with its own budget (an HTTP request) supplies a limit so that waiting ends as a
/// retryable no-changes `Busy` before the caller gives up; startup waits like other publication.
/// When a wait that starts now must end. Each wait gets a full budget, so a tree that takes long to
/// stage leaves its commit as long to wait as a small one; expiring before exclusive ownership can
/// still confirm that nothing changed.
fn wait_deadline(
    catalog: &Catalog,
    request_deadline: Option<tokio::time::Instant>,
) -> tokio::time::Instant {
    let budget = catalog.admin_deadline();
    request_deadline.map_or(budget, |deadline| deadline.min(budget))
}

impl Admin {
    #[cfg(test)]
    pub(crate) async fn seed_batch(&self) -> Result<SeedBatch, AdminError> {
        self.seed_batch_until(None, None).await
    }

    pub(crate) async fn seed_batch_until(
        &self,
        request_deadline: Option<tokio::time::Instant>,
        max_entries: Option<NonZeroUsize>,
    ) -> Result<SeedBatch, AdminError> {
        let deadline = wait_deadline(&self.catalog, request_deadline);
        let request = Arc::new(
            self.catalog
                .wait_admin(self.catalog.0.publication_requests.clone(), deadline)
                .await?,
        );
        let admission = self
            .catalog
            .wait_admin(self.catalog.0.publication_batch.clone(), deadline)
            .await?;
        // Taken after the wait so that a waiting seed never holds back an exclusive reset.
        let activity = self.catalog.admin_activity(Work::New, deadline).await?;
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
            metadata_budget: metadata_budget(max_entries),
            request_deadline,
            records: None,
            marks: HashMap::new(),
            absences: Vec::new(),
            unproven: Vec::new(),
        })
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
        let Some(budget) = self.metadata_budget else {
            return Ok(());
        };
        self.metadata_bytes = self
            .metadata_bytes
            .checked_add(bytes)
            .filter(|size| *size <= budget)
            .ok_or(AdminError::SeedMetadataBudget {
                mib: budget / (1024 * 1024),
            })?;
        Ok(())
    }

    /// Opens the directory `root` without following a symlink, on a file worker, with the node
    /// the descriptor holds.
    #[cfg(unix)]
    pub(crate) async fn open_root(
        &self,
        root: std::path::PathBuf,
    ) -> Result<(Arc<File>, fsutil::NodeId), AdminError> {
        self.admin
            .catalog
            .0
            .file_workers
            .run(move || {
                let file = fsutil::open_directory_nofollow(&root)?;
                let node = fsutil::NodeId::from(&file.metadata()?);
                Ok((Arc::new(file), node))
            })
            .await
    }

    /// Reads what seeding recorded and the buckets it took, for a batch that walks the fixture
    /// tree, and returns the recorded names, which the walk looks up where it does not list them.
    pub(crate) async fn read_records(&mut self) -> Result<Vec<RecordKey>, AdminError> {
        let (records, marks) = self
            .admin
            .catalog
            .run::<_, CatalogError>(|connection| {
                Ok((
                    seed_records::all(connection)?,
                    seed_records::mark_states(connection)?,
                ))
            })
            .await?;
        let recorded = records.keys().cloned().collect();
        self.records = Some(records);
        self.marks = marks;
        Ok(recorded)
    }

    /// What seeding recorded at `bucket`/`name` when the batch read the records.
    pub(crate) fn record(&self, bucket: &str, name: &str) -> Option<&SeedRecord> {
        self.records
            .as_ref()?
            .get(&(bucket.to_owned(), name.to_owned()))
    }

    /// Whether seeding took the bucket of the directory `bucket`, as the batch read it with the
    /// records.
    pub(crate) fn mark(&self, bucket: &str) -> Mark {
        self.marks.get(bucket).copied().unwrap_or(Mark::Unmarked)
    }

    /// Takes `proof` that the fixture file of `bucket`/`name` is gone, and returns whether the
    /// name has a record to decide from; a name without one owns nothing, so the caller only
    /// reports it.
    pub(crate) fn absent(&mut self, bucket: &str, name: &str, proof: Vacant) -> bool {
        let recorded = self.record(bucket, name).is_some();
        if recorded {
            self.absences
                .push(((bucket.to_owned(), name.to_owned()), proof));
        }
        recorded
    }

    /// Takes the recorded name `bucket`/`name`, which the walk did not list and where the lookup
    /// after it could not tell whether the fixture file is there for `reason`. The commit decides
    /// with [`replay::unproven`] whether the name needs reporting.
    pub(crate) fn unproven(&mut self, bucket: &str, name: &str, reason: &dyn std::fmt::Display) {
        if self.record(bucket, name).is_some() {
            self.unproven
                .push(((bucket.to_owned(), name.to_owned()), reason.to_string()));
        }
    }

    pub(crate) fn bucket(&mut self, definition: BucketDefinition) -> Result<(), AdminError> {
        self.charge_metadata(metadata_size(&definition.0)?)?;
        self.buckets.push(definition);
        Ok(())
    }

    /// Stages the fixture file `source` with what it holds when staging reads it, even where it
    /// changed after the walk opened it, as a startup replay always has. The replay keeps the
    /// fingerprint the staging read took, which describes exactly the bytes staged. A file that
    /// changes while it is read fails with `SourceChanged`, which a pass while running takes for
    /// an unknown name.
    pub(crate) async fn fixture(
        &mut self,
        bucket: BucketName,
        name: ObjectName,
        request: Publication,
        source: File,
    ) -> Result<(), AdminError> {
        request.metadata.validate()?;
        self.charge_metadata(
            bucket.as_str().len()
                + name.as_str().len()
                + request.content_type.len()
                + metadata_size(&request.metadata)?,
        )?;
        // The batch owns one activity/admission lease; individual 256-handle permits must not deadlock a larger tree.
        let staged = self
            .admin
            .stage_from_file(source, request.expected, request.max_bytes)
            .await?
            .into_batch(self.request.clone());
        let fingerprint = staged.fingerprint();
        self.entries.push(SeedEntry {
            publication: BatchPublication {
                bucket,
                name,
                request,
                staged,
            },
            fingerprint,
        });
        Ok(())
    }

    pub(crate) async fn commit(self, mode: SeedMode) -> Result<Counts, AdminError> {
        self.commit_with(mode, None).await
    }

    /// Clears the catalog whatever protects its objects, applies `initialization`, then
    /// publishes the batch, all in one transaction: the instance returns to its startup state
    /// with no moment in which the startup buckets are missing.
    pub(crate) async fn restore(
        self,
        initialization: Option<Initialization>,
    ) -> Result<Counts, AdminError> {
        self.commit_with(SeedMode::Restore, initialization).await
    }

    async fn commit_with(
        mut self,
        mode: SeedMode,
        initialization: Option<Initialization>,
    ) -> Result<Counts, AdminError> {
        let catalog = self.admin.catalog.clone();
        self.prepare_entries().await?;
        let deadline = wait_deadline(&catalog, self.request_deadline);
        // Taken while the lease still keeps a reset out, so the two together leave it no gap.
        let committing = catalog
            .wait_admin(catalog.0.seed_commit.clone(), deadline)
            .await?;
        // The batch's own read lease would otherwise keep the exclusive gate from ever opening.
        self.activity.take();
        let exclusive = catalog.wait_exclusive(deadline).await?;
        let inner = catalog.0.clone();
        let (outcome, mut batch) = catalog
            .0
            .executor
            .run(move || {
                let (_exclusive, _committing) = (exclusive, committing);
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Debug,
                    crate::diagnostics::Event::SeedCommitStarted,
                    serde_json::json!({}),
                );
                let outcome = inner
                    .with_connection(|connection| commit(connection, &self, mode, initialization))
                    .unwrap_or_else(|error| Err(error.into()));
                (outcome, self)
            })
            .await
            .map_err(AdminError::backend)?;
        batch.clean_up_staged().await?;
        // Any element may have replaced a version in a bucket that retains deletions.
        catalog.0.maintenance_wake.notify_one();
        let (counts, journaled) = outcome?;
        catalog.reclaim_if(journaled).await;
        Ok(counts)
    }

    /// Publishes the staged body of each entry, in staging order, before the commit that may
    /// reference it.
    async fn prepare_entries(&mut self) -> Result<(), AdminError> {
        let catalog = self.admin.catalog.clone();
        self.entries.reverse();
        while let Some(entry) = self.entries.pop() {
            self.prepared
                .push(prepare_entry(catalog.clone(), entry).await?);
        }
        std::mem::swap(&mut self.entries, &mut self.prepared);
        Ok(())
    }

    /// Removes each staged body the commit left unreferenced. Reference-aware cleanup also
    /// handles a commit whose caller was cancelled or whose result was ambiguous.
    async fn clean_up_staged(&mut self) -> Result<(), AdminError> {
        let entries: Vec<_> = std::mem::take(&mut self.entries)
            .into_iter()
            .map(BatchPublication::from)
            .collect();
        let ownership = self.request.clone();
        let cleanup = crate::task::spawn_supervised("seed-batch-cleanup", async move {
            let _ownership = ownership;
            cleanup_entries(entries).await
        });
        if let Err(error) = cleanup.await.map_err(AdminError::backend)? {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                crate::diagnostics::Event::SeedCleanupDeferred,
                serde_json::json!({"message":error.to_string()}),
            );
        }
        Ok(())
    }
}

impl Drop for SeedBatch {
    fn drop(&mut self) {
        let mut entries = std::mem::take(&mut self.entries);
        entries.append(&mut self.prepared);
        if entries.is_empty() {
            return;
        }
        let entries: Vec<_> = entries.into_iter().map(BatchPublication::from).collect();
        let ownership = self.request.clone();
        let activity = self.activity.take();
        crate::task::spawn_supervised_on(&self.runtime, "seed-batch-abort-cleanup", async move {
            let (_ownership, _activity) = (ownership, activity);
            if let Err(error) = cleanup_entries(entries).await {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    crate::diagnostics::Event::SeedCleanupDeferred,
                    serde_json::json!({"message":error.to_string()}),
                );
            }
        });
    }
}

async fn prepare_entry(catalog: Catalog, mut entry: SeedEntry) -> Result<SeedEntry, AdminError> {
    if entry.publication.staged.published {
        return Ok(entry);
    }
    crate::task::spawn_supervised("seed-prepare-object", async move {
        let _activity = catalog.activity()?;
        let staged = &mut entry.publication.staged;
        let id = staged.blob_id();
        let source = id.staging_path(&catalog.0.data_dir);
        let destination = id.blob_path(&catalog.0.data_dir);
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
                Ok::<_, AdminError>(())
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
    now: SystemTime,
) -> Result<(), AdminError> {
    let importing: bool = sql::scalar(
        transaction,
        "SELECT EXISTS(SELECT 1 FROM import_journal WHERE active = 1)",
        [],
    )
    .map_err(objects_error)?;
    if importing {
        return Err(AdminError::Precondition);
    }
    if !mode.clears() {
        return Ok(());
    }
    match mode {
        SeedMode::Replace => {
            let now = crate::storage::catalog::unix_micros(now)?;
            let held: bool = sql::scalar(
                transaction,
                concat!(
                    "SELECT EXISTS(SELECT 1 FROM objects JOIN buckets ON objects.bucket = buckets.name WHERE event_based_hold = 1 OR temporary_hold = 1 OR ",
                    crate::storage::catalog::objects::retention_start!("objects."),
                    " + json_extract(buckets.retention_policy, '$.retention_period') * 1000000 > ?1 OR retain_until_time > ?1)"
                ),
                [now],
            )
            .map_err(objects_error)?;
            if held {
                return Err(AdminError::Hold);
            }
            Ok(())
        }
        // Returning to the startup state is a reset, which no protection refuses.
        SeedMode::Merge | SeedMode::Restore => Ok(()),
    }
}

fn commit(
    connection: &mut Connection,
    batch: &SeedBatch,
    mode: SeedMode,
    initialization: Option<Initialization>,
) -> Result<(Counts, objects::Journaled), AdminError> {
    let transaction = sql::immediate(connection).map_err(objects_error)?;
    let catalog = &batch.admin.catalog;
    // One instant for the whole commit, at the microsecond that publication records: the
    // guards, the replay's protection decisions and each publication's own check then all
    // judge protection at the time the published records keep.
    let now = super::recorded_instant(catalog.0.clock.now())?;
    check_guards(&transaction, mode, now)?;
    let mut counts = Counts::default();
    let mut journaled = objects::Journaled::NOTHING;
    let epoch = if mode.clears() {
        let reclamation = super::super::reset::Reclamation::for_backend(&catalog.0.backend);
        let cleared = super::super::reset::clear_in_transaction(&transaction, reclamation)?;
        journaled |= cleared.journaled;
        counts.removed = Removed {
            buckets: cleared.buckets,
            objects: cleared.objects,
        };
        Some(cleared.epoch)
    } else {
        None
    };
    if let Some(initialization) = initialization {
        counts.buckets += initialization.apply(&transaction, catalog)?;
    }
    for definition in &batch.buckets {
        match create_bucket(&transaction, catalog, &definition.0)? {
            Bucket::Created => counts.buckets += 1,
            Bucket::Existing => {}
            Bucket::OtherProject => return Err(AdminError::SeedProjectMismatch),
        }
    }
    let published = replay_files(&transaction, batch, now, &mut counts, &mut journaled)?;
    if batch.records.is_some() {
        replay_absences(&transaction, batch, now, &mut journaled)?;
        replay_unproven(&transaction, batch, now, unproven_kept)?;
        seed_records::clear_absences(&transaction)?;
        mark_buckets(&transaction, batch)?;
    }
    sql::exec(
        &transaction,
        "UPDATE startup_seed SET complete = 1 WHERE singleton = 1",
        [],
    )
    .map_err(objects_error)?;
    if let Err(error) = objects::commit_publication(transaction)
        && !confirmed(connection, batch, &published, epoch)?
    {
        return Err(error.into());
    }
    if mode.clears() {
        counts.cleanup_pending = !clear_replaced(connection, batch);
        if !counts.cleanup_pending {
            journaled = objects::Journaled::NOTHING;
        }
    }
    Ok((counts, journaled))
}

/// What a replay found under the name of a top-level directory's bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Bucket {
    Created,
    /// A bucket of the seed project, which the replay uses.
    Existing,
    /// A bucket of another project, which the replay leaves alone: a startup replay or reseed
    /// fails, and a pass leaves the directory out.
    OtherProject,
}

/// Creates the bucket `record` names unless a bucket has the name, in the caller's transaction.
pub(super) fn create_bucket(
    transaction: &rusqlite::Transaction<'_>,
    catalog: &Catalog,
    record: &BucketRecord,
) -> Result<Bucket, AdminError> {
    match crate::storage::catalog::select_bucket(transaction, &record.name)
        .map_err(objects_error)?
    {
        Some(existing) if existing.project != record.project => Ok(Bucket::OtherProject),
        Some(_) => Ok(Bucket::Existing),
        None => {
            let creation = crate::storage::catalog::BucketCreation::default_policy(
                catalog.default_soft_delete_retention(),
            );
            crate::storage::catalog::create_bucket_in_transaction(
                transaction,
                &mut record.clone(),
                creation,
            )?;
            Ok(Bucket::Created)
        }
    }
}

/// Replays each staged file as a startup replay does, recording it with the version seeding then
/// owns, and returns the bodies it published.
fn replay_files(
    transaction: &rusqlite::Transaction<'_>,
    batch: &SeedBatch,
    now: SystemTime,
    counts: &mut Counts,
    journaled: &mut objects::Journaled,
) -> Result<Vec<BlobId>, AdminError> {
    let mut published = Vec::new();
    for SeedEntry {
        publication: entry,
        fingerprint,
    } in &batch.entries
    {
        validate_staged_entry(entry)?;
        let (bucket, name) = (entry.bucket.as_str(), entry.name.as_str());
        let previous = objects::lookup(transaction, bucket, name)?;
        let live = previous
            .as_ref()
            .map(|stored| live_object(transaction, &stored.record, entry, now))
            .transpose()?;
        let file = staged_body(entry);
        // A clearing commit already dropped the records, so it decides as for a new tree.
        let record = seed_records::get(transaction, bucket, name)?;
        let owned = match replay::startup_file(&file, record.as_ref(), live.as_ref()) {
            Replay::Publish => {
                let object = publish_entry(transaction, entry, &batch.admin.grant, now, journaled)?;
                published.push(entry.staged.blob_id());
                if previous.is_some() {
                    counts.replaced += 1;
                } else {
                    counts.objects += 1;
                }
                Some(version_of(&object))
            }
            Replay::Record { owned } => {
                counts.unchanged += 1;
                owned
            }
            // The refusal the replacement itself would meet, before anything is written.
            Replay::Protected => return Err(AdminError::Hold),
        };
        seed_records::save(
            transaction,
            bucket,
            name,
            &SeedRecord::new(owned, file, *fingerprint),
        )?;
    }
    Ok(published)
}

/// What staging read from a fixture file.
pub(super) fn staged_body(entry: &BatchPublication) -> Body {
    Body {
        size: entry.staged.size,
        hashes: entry.staged.hashes,
    }
}

/// The version of `object`, the generation at its metageneration.
pub(super) fn version_of(object: &ObjectRecord) -> Version {
    Version {
        generation: object.generation,
        metageneration: object.metageneration,
    }
}

/// Applies a startup replay's decision at each recorded name a lookup proved absent: deletes an
/// object seeding purely owns on that one proof, keeps a protected one, and forgets the rest.
fn replay_absences(
    transaction: &rusqlite::Transaction<'_>,
    batch: &SeedBatch,
    now: SystemTime,
    journaled: &mut objects::Journaled,
) -> Result<(), AdminError> {
    let access = batch.admin.grant.access();
    for ((bucket, name), proof) in &batch.absences {
        let Some(record) = seed_records::get(transaction, bucket, name)? else {
            continue;
        };
        let live = absent_object(transaction, bucket, name, now)?;
        match replay::absent(Phase::Startup, proof, &record, live.as_ref()) {
            Vanished::Delete { owned } => {
                if delete_owned(transaction, (bucket, name), owned, now, &access, journaled)? {
                    seed_records::save(transaction, bucket, name, &record.after_deleting(owned))?;
                    deleted(bucket, name);
                } else {
                    kept(bucket, name);
                }
            }
            Vanished::Forget => seed_records::forget(transaction, bucket, name)?,
            Vanished::Tombstone => {
                seed_records::save(transaction, bucket, name, &record.tombstone())?;
            }
            Vanished::Protected => kept(bucket, name),
        }
    }
    Ok(())
}

/// Decides each recorded name the lookup after the walk could not tell about with
/// [`replay::unproven`]: keeps a tombstone where seeding no longer purely owns the live object,
/// and hands every other name, with the reason, to `report`. Returns how many tombstones it kept.
pub(super) fn replay_unproven(
    transaction: &rusqlite::Transaction<'_>,
    batch: &SeedBatch,
    now: SystemTime,
    mut report: impl FnMut(&str, &str, &str),
) -> Result<usize, AdminError> {
    let mut tombstones = 0;
    for ((bucket, name), reason) in &batch.unproven {
        let Some(record) = seed_records::get(transaction, bucket, name)? else {
            continue;
        };
        let live = absent_object(transaction, bucket, name, now)?;
        match replay::unproven(&record, live.as_ref()) {
            Unproven::Report => report(bucket, name, reason),
            Unproven::Tombstone => {
                tombstones += 1;
                let kept = record.tombstone();
                if kept != record {
                    seed_records::save(transaction, bucket, name, &kept)?;
                }
            }
        }
    }
    Ok(tombstones)
}

/// Warns that a startup replay or a reseed kept the object seeding owns at `bucket`/`name`, whose
/// fixture file the walk did not list, because the lookup after the walk could not tell whether
/// the file is there, as the walk warns about an entry it skips.
fn unproven_kept(bucket: &str, name: &str, reason: &str) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        crate::diagnostics::Event::SeedSkipped,
        serde_json::json!({"path": format!("{bucket}/{name}"), "reason": reason}),
    );
}

/// Reports the object a replay deleted at `bucket`/`name` because its fixture file is gone.
pub(super) fn deleted(bucket: &str, name: &str) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Info,
        crate::diagnostics::Event::SeedObjectDeleted,
        serde_json::json!({"bucket": bucket, "object": name}),
    );
}

/// Reports the object a replay kept at `bucket`/`name` although its fixture file is gone,
/// because a hold or an unmet retention period refuses the deletion.
fn kept(bucket: &str, name: &str) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        crate::diagnostics::Event::SeedObjectKept,
        serde_json::json!({
            "bucket": bucket,
            "object": name,
            "reason": "a hold or retention period protects it",
        }),
    );
}

/// What the catalog holds at `bucket`/`name` for a decision about a name the walk did not find,
/// which reads only the version, the body and whether deleting it is refused.
pub(super) fn absent_object(
    transaction: &rusqlite::Transaction<'_>,
    bucket: &str,
    name: &str,
    now: SystemTime,
) -> Result<Option<LiveObject>, AdminError> {
    let Some(stored) = objects::lookup(transaction, bucket, name)? else {
        return Ok(None);
    };
    let live = &stored.record;
    let versioning = objects::BucketPublication::load(transaction, bucket)?.versioning;
    Ok(Some(LiveObject {
        version: version_of(live),
        body: stored_body(live),
        // A deletion never compares attributes.
        attributes: Attributes::Edited,
        protection: protection(&objects::check_replaceable(live, versioning, now)),
    }))
}

/// Deletes the live object at `name`, a bucket and an object name, only while it still is the
/// version `owned`: the `ifGenerationMatch` and `ifMetagenerationMatch` conditions are checked
/// in the transaction that deletes it. Returns whether it deleted; a failed condition, a hold or
/// an unmet retention period deletes nothing.
pub(super) fn delete_owned(
    transaction: &rusqlite::Transaction<'_>,
    (bucket, name): (&str, &str),
    owned: Version,
    now: SystemTime,
    access: &BucketAccess,
    journaled: &mut objects::Journaled,
) -> Result<bool, AdminError> {
    let target = ObjectTarget {
        bucket: bucket.to_owned(),
        name: name.to_owned(),
        generation: None,
    };
    let context = objects::DeleteContext {
        now,
        conditions: ObjectConditions {
            if_generation_match: Some(owned.generation),
            if_metageneration_match: Some(owned.metageneration),
            ..Default::default()
        },
        access,
    };
    sql::exec(transaction, "SAVEPOINT seed_delete", []).map_err(objects_error)?;
    let outcome = objects::delete_in_transaction(transaction, &target, context);
    // A test fails the deletion here, after it wrote, as a full disk would.
    #[cfg(test)]
    let outcome = outcome.and_then(|deleted| {
        crate::storage::catalog::fault::hit_transaction(
            transaction,
            crate::storage::catalog::fault::Step::SeedDelete,
        )
        .map_err(objects_error)?;
        Ok(deleted)
    });
    match outcome {
        Ok(deleted) => {
            sql::exec(transaction, "RELEASE seed_delete", []).map_err(objects_error)?;
            *journaled |= deleted.retired.into();
            Ok(true)
        }
        Err(error) if refused(&error) => {
            sql::exec(transaction, "ROLLBACK TO seed_delete", []).map_err(objects_error)?;
            sql::exec(transaction, "RELEASE seed_delete", []).map_err(objects_error)?;
            Ok(false)
        }
        // The replay fails and its transaction ends uncommitted, which drops the savepoint.
        // SQLite may already have rolled the whole transaction back, as after a full disk, so
        // releasing the savepoint here would fail too and hide the cause the caller needs.
        Err(error) => Err(error.into()),
    }
}

/// Whether a deletion failed only because the object is not the version it named or is
/// protected, which keeps the object and the record. Every other failure fails the replay that
/// tried the deletion.
fn refused(error: &ObjectError) -> bool {
    matches!(
        error,
        ObjectError::PreconditionFailed
            | ObjectError::ObjectNotFound
            | ObjectError::ActiveHold(_)
            | ObjectError::ObjectRetentionNotMet(_)
    )
}

/// Marks the bucket of each top-level directory the replay walked, and forgets the marks of
/// directories the tree no longer holds: a startup replay lists the whole tree or fails.
fn mark_buckets(
    transaction: &rusqlite::Transaction<'_>,
    batch: &SeedBatch,
) -> Result<(), AdminError> {
    let walked: std::collections::HashSet<&str> = batch
        .buckets
        .iter()
        .map(|definition| definition.0.name.as_str())
        .collect();
    forget_marks(transaction, |bucket| walked.contains(bucket))?;
    for bucket in walked {
        seed_records::mark(transaction, bucket)?;
    }
    Ok(())
}

/// Forgets the mark of each directory a startup replay or a reseed, which list the whole tree or
/// fail, did not find there: one whose bucket name `listed` does not accept.
fn forget_marks(
    transaction: &rusqlite::Transaction<'_>,
    listed: impl Fn(&str) -> bool,
) -> Result<(), AdminError> {
    for marked in seed_records::marks(transaction)? {
        if !listed(&marked) {
            seed_records::unmark(transaction, &marked)?;
        }
    }
    Ok(())
}

/// Removes what a clearing commit replaced while the commit still holds exclusive ownership, and
/// returns whether nothing was left for maintenance. RAM keeps only the bodies the batch
/// published. A disk reclaims every journaled ID as a reset does, staging and appendable files
/// included: the reference-checked reclamation after commit removes blobs alone, so those files
/// would otherwise wait for a restart. The batch's own bodies have new IDs outside the journal.
fn clear_replaced(connection: &mut Connection, batch: &SeedBatch) -> bool {
    let backend = &batch.admin.catalog.0.backend;
    match backend {
        crate::storage::catalog::BlobBackend::Memory(_) => {
            // Only the bounded prepared namespace is indexed; a full old catalog needs no growing
            // deletion journal.
            let selected: Vec<_> = batch
                .entries
                .iter()
                .map(|entry| entry.publication.staged.blob_id().to_hex())
                .collect();
            let Err(error) =
                backend.retain_memory_ids(&selected.iter().map(String::as_str).collect())
            else {
                return true;
            };
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Error,
                crate::diagnostics::Event::SeedCleanupDeferred,
                serde_json::json!({"message":error.to_string()}),
            );
            false
        }
        crate::storage::catalog::BlobBackend::Disk(data_dir) => {
            let Err(error) = super::super::reset::reclaim(connection, data_dir) else {
                return true;
            };
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                crate::diagnostics::Event::SeedCleanupDeferred,
                serde_json::json!({"message":error.to_string()}),
            );
            false
        }
    }
}

fn confirmed(
    connection: &Connection,
    batch: &SeedBatch,
    ids: &[BlobId],
    epoch: Option<i64>,
) -> Result<bool, AdminError> {
    let (complete, current): (bool, i64) = sql::row(connection, "SELECT complete, epoch FROM startup_seed, reset_epoch WHERE startup_seed.singleton = 1 AND reset_epoch.singleton = 1", [], |row| Ok((row.get(0)?, row.get(1)?))).map_err(objects_error)?;
    if !complete || epoch.is_some_and(|epoch| epoch != current) {
        return Ok(false);
    }
    for id in ids {
        let exists: bool = objects::blob_referenced(connection, id).map_err(objects_error)?;
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

/// What a replay of `entry` at `now` finds in `live`, the object the catalog holds at its name.
/// The protection is the refusal publishing the replacement at `now` would meet.
pub(super) fn live_object(
    transaction: &rusqlite::Transaction<'_>,
    live: &ObjectRecord,
    entry: &BatchPublication,
    now: SystemTime,
) -> Result<LiveObject, AdminError> {
    let versioning =
        objects::BucketPublication::load(transaction, entry.bucket.as_str())?.versioning;
    Ok(LiveObject {
        version: version_of(live),
        body: stored_body(live),
        attributes: if seeded_attributes(live, entry) {
            Attributes::AsSeeded
        } else {
            Attributes::Edited
        },
        protection: protection(&objects::check_replaceable(live, versioning, now)),
    })
}

/// The body the catalog holds in `live`.
fn stored_body(live: &ObjectRecord) -> Body {
    Body {
        size: live.size,
        hashes: crate::hashing::Hashes {
            md5: live.md5,
            crc32c: live.crc32c,
        },
    }
}

/// Whether a hold or an unmet retention period refuses the change, as `replaceable`, the check
/// of the live object, found.
fn protection(replaceable: &Result<(), ObjectError>) -> Protection {
    match replaceable {
        Ok(()) => Protection::Unprotected,
        Err(_) => Protection::Protected,
    }
}

/// Whether `live` carries only what publishing `entry` writes besides the body.
fn seeded_attributes(live: &ObjectRecord, entry: &BatchPublication) -> bool {
    // Seed requests carry no metadata; adding sidecars must extend this comparison.
    live.content_type == entry.request.content_type
        && live.content_encoding.is_none()
        && live.content_disposition.is_none()
        && live.content_language.is_none()
        && live.cache_control.is_none()
        && live.custom_time.is_none()
        && live.metadata.is_empty()
}

#[cfg(test)]
mod tests;
