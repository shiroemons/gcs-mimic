use super::interrupt::{Interrupt, Interrupted};
use super::tree::TreeScan as Scan;
use super::{
    Pass, SyncError,
    entries::{EntryClass, EntryProblem, classify, path_text},
    mirror::Reconciliation,
    naming::RootNaming,
    report::{remedy::Correction, verdict::TreeSide},
};
use crate::{
    config::Config,
    diagnostics::{self, LogLevel},
    fsutil::{self, FileIdentity},
    storage::{
        Catalog,
        admin::{
            Admin, AdminError, BatchPublication, BucketName, Generation, GenerationCondition,
            ObjectName,
        },
        sync_state::{
            SyncBinding, SyncFingerprint, SyncPath, SyncRecord,
            durable::SyncedDirectories,
            observation::{AliasEdit, Transition},
            root::{RootError, RootLease},
        },
    },
};
use std::{fs::File, io, path::PathBuf};

mod candidate;
mod publish;
mod stage;
mod stops;

use stops::clear_missing_observations;

/// A changed regular file selected for ingestion, with what `candidate` already parsed from it.
struct Candidate {
    /// The file below the sync root, for staging and diagnostics.
    path: PathBuf,
    /// The same path as the tracked state's key, checked once here.
    key: SyncPath,
    /// The identity the tree scan observed; staging requires the file still to have it.
    identity: FileIdentity,
    bucket: BucketName,
    name: ObjectName,
    binding: SyncBinding,
    tracked: Option<SyncRecord>,
    live: Option<crate::object::ObjectRecord>,
}

/// What ingestion does with a changed tracked file that an alias may stand for.
enum AliasOutcome {
    /// No alias holds it: the file is taken in for its own object.
    Free,
    /// The alias holds it, or a concurrent change defers it: nothing is taken in now.
    Kept,
    /// Its object went, and the one object left of the alias takes the edit in.
    HandOver(Heir),
}

/// The one object left of an alias whose file holds an edit, and the generation of it that the
/// edit may replace; see [`AliasEdit::HandOver`].
struct Heir {
    name: ObjectName,
    base: Option<Generation>,
}

async fn forget_missing_directories(
    root: &RootLease,
    interrupt: Interrupt<'_>,
) -> Result<(), SyncError> {
    let mut after = None;
    loop {
        if interrupt.check() != Interrupted::No {
            break;
        }
        let page = root
            .sync_buckets_page(
                after.clone(),
                crate::storage::sync_state::PageLimit::LARGEST,
            )
            .await?;
        if page.is_empty() {
            break;
        }
        for bucket in page {
            if interrupt.check() != Interrupted::No {
                return Ok(());
            }
            after = Some(bucket.name.clone());
            let selected = bucket.name.clone();
            let absent = root
                .file_job(move |root| {
                    match fsutil::directory_at(
                        root,
                        selected.as_str().as_ref(),
                        fsutil::DirectoryOpen::Existing,
                    ) {
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
                        Ok(_) | Err(_) => Ok(false),
                    }
                })
                .await?;
            if absent {
                root.forget_sync_bucket(&bucket.name, bucket.incarnation)
                    .await?;
            }
        }
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(super) async fn rescan(
    catalog: &Catalog,
    root: &RootLease,
    config: &Config,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> Result<Reconciliation, SyncError> {
    let writeback = root.default_writeback().await?;
    rescan_with(catalog, root, config, &writeback, Interrupt::new(stopping)).await
}
pub(super) async fn rescan_with(
    catalog: &Catalog,
    root: &RootLease,
    config: &Config,
    writeback: &crate::storage::sync_state::writeback::Writeback,
    interrupt: Interrupt<'_>,
) -> Result<Reconciliation, SyncError> {
    if writeback.status().await?.stop() != crate::storage::sync_state::writeback::Stop::Ready {
        return Err(SyncError::WritebackUnfinished(Pass::Ingest));
    }
    let mut ingest = Ingest {
        catalog,
        root,
        naming: root.naming()?,
        lookup: root.lookup()?,
        config,
        admin: root.sync_admin(),
        summary: Reconciliation::default(),
        synced: SyncedDirectories::default(),
    };
    forget_missing_directories(root, interrupt).await?;
    let mut scan = Scan::new(root.tree_limits(), ingest.lookup.clone());
    while !scan.is_empty() && interrupt.check() == Interrupted::No {
        let (next, entries) = root.file_job(move |root| scan.page(root)).await?;
        scan = next;
        let mut candidates = Vec::new();
        let mut blocked = Vec::<PathBuf>::new();
        for entry in entries {
            if interrupt.check() != Interrupted::No {
                return Ok(ingest.interrupted());
            }
            if blocked.iter().any(|path| entry.path.starts_with(path)) {
                continue;
            }
            match classify(&entry, ingest.naming) {
                EntryClass::Unreadable(kind) => ingest.defer(&entry.path, kind),
                EntryClass::Bucket(bucket) => {
                    if ingest.bucket_directory(&entry.path, bucket).await?
                        == BucketDirectory::Blocked
                    {
                        scan.skip(&entry.path);
                        blocked.push(entry.path);
                    }
                }
                EntryClass::Directory => {}
                EntryClass::Skipped(skip) => ingest.summary.count_skipped(&entry.path, skip),
                EntryClass::Unrepresentable(problem) => {
                    ingest
                        .summary
                        .count_unrepresentable_entry(&entry.path, problem);
                }
                EntryClass::File(file) => {
                    if let Some(candidate) = ingest.candidate(entry.path, file).await? {
                        candidates.push(candidate);
                    }
                }
            }
        }
        // Stable files need no timer or body read; one quiet interval covers changed candidates.
        if !candidates.is_empty() {
            #[cfg(test)]
            checkpoint("settle").await;
            if interrupt
                .sleep(std::time::Duration::from_millis(super::SETTLE_MS))
                .await
                != Interrupted::No
            {
                return Ok(ingest.interrupted());
            }
        }
        // This page's listing observed every candidate, so a directory synced from here on covers
        // their entries; the next listing may observe entries made after these syncs.
        ingest.synced = SyncedDirectories::default();
        for candidate in candidates {
            if interrupt.check() != Interrupted::No {
                return Ok(ingest.interrupted());
            }
            ingest.ingest(candidate).await?;
        }
    }
    let mut summary = ingest.summary;
    summary.interrupted = interrupt.check() != Interrupted::No;
    if !summary.interrupted {
        clear_missing_observations(root, interrupt).await?;
        summary.interrupted = interrupt.check() != Interrupted::No;
    }
    if !summary.interrupted {
        // The status reads the tree side from this record, which the same hold as a wait's
        // decides, so the two put the same condition first.
        let side = summary.gate().hold().tree_side();
        root.observation_scan(side.and_then(TreeSide::scan_stop))
            .await?;
    }
    Ok(summary)
}

/// Whether a top-level directory may be ingested or belongs to a bucket deleted through the
/// API, whose subtree is not taken in until the user resolves it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BucketDirectory {
    Ready,
    Blocked,
}

/// Whether the directory of `bucket` holds anything besides the operating system and editor
/// files a scan ignores. One that cannot be read counts as holding entries, so that what it
/// may hold is never passed over.
async fn holds_entries(
    root: &RootLease,
    bucket: BucketName,
    naming: RootNaming,
) -> Result<bool, SyncError> {
    let lookup = root.lookup()?;
    Ok(root
        .file_job(move |root| {
            let directory = match fsutil::directory_at(
                root,
                bucket.as_str().as_ref(),
                fsutil::DirectoryOpen::Existing,
            ) {
                Ok(directory) => directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(_) => return Ok(true),
            };
            let mut names = fsutil::DirectoryNames::default();
            loop {
                match names.next_name(&directory, usize::MAX) {
                    Ok(Some(name)) if passed_over(&directory, &bucket, &name, naming, &lookup) => {}
                    Ok(Some(_)) | Err(_) => return Ok(true),
                    Ok(None) => return Ok(false),
                }
            }
        })
        .await?)
}

/// Whether the scan passes over the entry `name` of the directory of `bucket`, as [`classify`]
/// decides for the scan itself: only a regular file with an ignored name. A directory, symlink
/// or special file so named is one the scan descends into or stops at, so it holds entries.
fn passed_over(
    directory: &File,
    bucket: &BucketName,
    name: &std::ffi::OsStr,
    naming: RootNaming,
    lookup: &fsutil::Lookup,
) -> bool {
    let file = match lookup.regular(directory, name) {
        Ok(fsutil::RegularChild::File(file)) => file,
        // Removed since the listing: it holds nothing any more.
        Ok(fsutil::RegularChild::Missing(_)) => return true,
        Ok(fsutil::RegularChild::Respelled(_) | fsutil::RegularChild::Transient(_)) | Err(_) => {
            return false;
        }
    };
    let Ok(identity) = fsutil::stat_identity(&file) else {
        return false;
    };
    let entry = super::tree::TreeEntry {
        path: std::path::Path::new(bucket.as_str()).join(name),
        identity: Some(identity),
        directory: false,
        error: None,
    };
    matches!(
        classify(&entry, naming),
        EntryClass::Skipped(super::entries::Skip::IgnoredName)
    )
}

/// Shared state of one ingestion rescan.
struct Ingest<'a> {
    catalog: &'a Catalog,
    root: &'a RootLease,
    naming: RootNaming,
    /// The lookups that reopen what the scan listed.
    lookup: fsutil::Lookup,
    config: &'a Config,
    admin: Admin,
    /// The counts of this rescan; `rescan_with` returns them.
    summary: Reconciliation,
    /// The directories already made durable for the current scan page's files.
    synced: SyncedDirectories,
}

impl Ingest<'_> {
    /// The counts so far of a rescan that stopped before it finished.
    fn interrupted(mut self) -> Reconciliation {
        self.summary.interrupted = true;
        self.summary
    }

    /// Counts a file or directory that could not be read this time and says how to restore it.
    /// The gate decides from `kind` whether a later pass may read it or an operator must act.
    fn defer(&mut self, path: &std::path::Path, kind: io::ErrorKind) {
        let path = if path.as_os_str().is_empty() {
            std::path::Path::new(".")
        } else {
            path
        };
        self.summary.count_read_failure(path, kind);
        diagnostics::emit(
            LogLevel::Warn,
            "sync_source_deferred",
            serde_json::json!({
                "path":path_text(path),"reason":kind.to_string(),
                "remedy":super::report::command::logged(&Correction::UnreadableFile.remedy())
            }),
        );
    }

    async fn bucket_directory(
        &mut self,
        path: &std::path::Path,
        bucket: BucketName,
    ) -> Result<BucketDirectory, SyncError> {
        if self
            .root
            .ensure_sync_bucket(&bucket, &self.config.seed_project)
            .await?
        {
            return Ok(BucketDirectory::Ready);
        }
        // Deleting a bucket through the API leaves its directory empty. Removing it here would
        // let the next scan create the bucket again, and keeping it holds nothing back.
        if !holds_entries(self.root, bucket, self.naming).await? {
            return Ok(BucketDirectory::Blocked);
        }
        self.summary.blocked_buckets += 1;
        self.summary
            .count_unrepresentable_entry(path, EntryProblem::DeletedBucket);
        Ok(BucketDirectory::Blocked)
    }

    /// What becomes of the changed tracked file of `name` that an alias may stand for. While it
    /// does, ingestion leaves the edit out: it may be meant for either object, and taking it in
    /// would pick one silently. The edit stops the path until the alias ends or the file goes
    /// back, and is reported once. Once `name` went and one object is left, the edit is that
    /// object's.
    async fn alias_edit(
        &mut self,
        binding: &SyncBinding,
        name: &ObjectName,
        path: &std::path::Path,
        key: &SyncPath,
    ) -> Result<AliasOutcome, SyncError> {
        match self
            .root
            .observe_alias_edit(binding.clone(), name.clone(), key.clone())
            .await
        {
            Ok(AliasEdit::Free) => Ok(AliasOutcome::Free),
            Ok(AliasEdit::HandOver { heir, base }) => {
                Ok(AliasOutcome::HandOver(Heir { name: heir, base }))
            }
            Ok(AliasEdit::Frozen {
                transition,
                partner,
            }) => {
                if transition == Transition::New {
                    diagnostics::emit(
                        LogLevel::Warn,
                        "sync_alias_edited",
                        serde_json::json!({
                            "path":path_text(path),"bucket":binding.bucket().as_str(),
                            "object":name.as_str(),"partner":partner.as_str(),
                            "remedy":Correction::EditedAlias.remedy()
                        }),
                    );
                }
                Ok(AliasOutcome::Kept)
            }
            Err(error) if super::error::superseded(&error) => {
                self.summary.count_deferred(path);
                Ok(AliasOutcome::Kept)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Takes the edit in the file of the deleted object `holder` in for `heir`, the one object
    /// left of the alias that the file stood for, as the edit's remedy promises: the bytes
    /// become the heir's next generation, and the holder's record takes the file as staged, so
    /// that the holder's deletion removes it unless it changed again and the heir gets its own
    /// file. A file that changed or could not be read waits for the next pass. An heir the API
    /// wrote since the edit was first seen keeps what it wrote: the edit goes with the holder's
    /// deletion, which keeps it as a conflict, as an export does with any edit it would replace.
    async fn hand_over(&mut self, holder: Candidate, heir: Heir) -> Result<(), SyncError> {
        let Heir { name: heir, base } = heir;
        let live = match self.admin.get_object(&holder.bucket, &heir, None).await {
            Ok(record) => record,
            Err(AdminError::NotFound) => {
                self.summary.count_deferred(&holder.path);
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let staged = match self.stage(holder.path.clone(), holder.identity).await {
            Ok(staged) => staged,
            Err(RootError::Admin(AdminError::Io(error)))
                if error.kind() != io::ErrorKind::NotFound =>
            {
                self.defer(&holder.path, error.kind());
                return Ok(());
            }
            Err(RootError::Admin(
                AdminError::Precondition
                | AdminError::NotFound
                | AdminError::SourceChanged { .. }
                | AdminError::Io(_),
            )) => {
                self.summary.count_deferred(&holder.path);
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let hashes = staged.hashes();
        let fingerprint =
            SyncFingerprint::observed(holder.identity, hashes, self.root.observation_time_ns());
        let generation = Generation::new(live.generation)?;
        // An heir that already holds the bytes took the edit in before a restart or a failed
        // record, whatever its generation is now.
        if live.size == staged.size() && live.md5 == hashes.md5 && live.crc32c == hashes.crc32c {
            staged.discard().await?;
        } else if base != Some(generation) {
            staged.discard().await?;
            return self.abandon_hand_over(holder, heir).await;
        } else {
            let condition = GenerationCondition::Is(generation);
            self.root.revalidate().await?;
            let result = self
                .admin
                .publish_objects(vec![BatchPublication {
                    bucket: holder.bucket.clone(),
                    request: self.publication(&holder.path, condition),
                    name: heir.clone(),
                    staged,
                }])
                .await?;
            for result in result.results {
                match result {
                    Ok(_) => self.summary.ingested += 1,
                    Err(AdminError::Precondition | AdminError::NotFound | AdminError::Hold) => {
                        self.summary.count_deferred(&holder.path);
                        return Ok(());
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        self.reached(crate::storage::fault::Step::SyncAliasPublished)
            .await?;
        // The record tells deletion detection that the file was in the tree, as a new one does,
        // and the edit may have reached it under a new directory entry, as an editor's save does.
        let Some(durable) = self
            .make_durable(&holder.key, holder.identity, &holder.binding, &holder.name)
            .await?
        else {
            return Ok(());
        };
        match self
            .root
            .hand_over_alias(holder.binding, holder.name, durable, fingerprint)
            .await
        {
            Ok(()) => {
                #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                self.reached(crate::storage::fault::Step::SyncAliasHandedOver)
                    .await?;
                self.summary.heirs.push((holder.bucket, heir));
                Ok(())
            }
            Err(error) if super::error::superseded(&error) => {
                self.summary.count_deferred(&holder.path);
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Leaves the edit in the file of the deleted object `holder` out of `heir`, which the API
    /// wrote since the edit was first seen. Observing `heir` again in this pass removes the
    /// holder's file first, which keeps the edit as a conflict, and then gives `heir` its own.
    async fn abandon_hand_over(
        &mut self,
        holder: Candidate,
        heir: ObjectName,
    ) -> Result<(), SyncError> {
        match self
            .root
            .abandon_alias_edit(holder.binding, holder.name)
            .await
        {
            Ok(()) => {
                self.summary.heirs.push((holder.bucket, heir));
                Ok(())
            }
            Err(error) if super::error::superseded(&error) => {
                self.summary.count_deferred(&holder.path);
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn ingest(&mut self, candidate: Candidate) -> Result<(), SyncError> {
        let Some((staged, fingerprint)) = self.stage_update(&candidate).await? else {
            return Ok(());
        };
        let hashes = staged.hashes();
        if let Some(live) = &candidate.live {
            let generation = Generation::new(live.generation)?;
            if live.size == staged.size() && live.md5 == hashes.md5 && live.crc32c == hashes.crc32c
            {
                staged.discard().await?;
                return self
                    .acknowledge_identical(&candidate, generation, fingerprint)
                    .await;
            }
            if candidate
                .tracked
                .as_ref()
                .is_none_or(|record| record.generation != generation)
            {
                staged.discard().await?;
                self.root
                    .record_export_conflict(candidate.binding, candidate.name, generation)
                    .await?;
                self.summary.conflicting += 1;
                return Ok(());
            }
        } else if let Some(tracked) = &candidate.tracked {
            let size = staged.size();
            staged.discard().await?;
            if tracked.fingerprint.size == size
                && tracked.fingerprint.md5 == hashes.md5
                && tracked.fingerprint.crc32c == hashes.crc32c
            {
                // A native notification can race API deletion before its unchanged output is removed.
                self.summary.count_deferred(&candidate.path);
                return Ok(());
            }
            self.root
                .record_export_conflict(candidate.binding, candidate.name, tracked.generation)
                .await?;
            self.summary.conflicting += 1;
            return Ok(());
        }
        self.publish(candidate, staged, fingerprint).await
    }

    /// A fault seam of ingestion at `step`, where the tree and the catalog are snapshotted
    /// together.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    async fn reached(&self, step: crate::storage::fault::Step) -> Result<(), SyncError> {
        use crate::storage::fault;
        let data = self.config.data_dir.clone();
        self.root
            .file_job(move |root| {
                fault::hit_io(&fault::descriptor_path(root)?, step)?;
                fault::hit_io(&data, step)?;
                Ok(())
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
use super::Hook;
#[cfg(test)]
tokio::task_local! { static HOOK: std::sync::Arc<Hook>; }
#[cfg(test)]
tokio::task_local! { pub(super) static STAGES: std::sync::Arc<std::sync::atomic::AtomicUsize>; }
#[cfg(test)]
async fn checkpoint(phase: &str) {
    if phase == "stage" {
        let _ = STAGES.try_with(|count| count.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
    }
    if let Ok(hook) = HOOK.try_with(std::sync::Arc::clone)
        && hook.phase == phase
    {
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
