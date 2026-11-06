use super::interrupt::{Interrupt, Interrupted};
use super::tree::{TreeEntry as Entry, TreeScan as Scan};
use super::{
    mapping,
    mirror::{Reconciliation, fs_error},
    tree::{ParentError, ParentOpen, parent},
};
use crate::{
    config::Config,
    diagnostics::{self, LogLevel},
    fsutil::{self, FileIdentity},
    storage::{
        Catalog,
        admin::{
            Admin, AdminError, BatchPublication, BucketName, Generation, GenerationCondition,
            ObjectName, Publication, PublishMode, StagedObject,
        },
        sync_state::{
            SyncBinding, SyncFingerprint, SyncRecord, SyncUpdate,
            observation::PathStop,
            root::{RootError, RootLease},
        },
    },
};
use std::{fs::File, io, path::PathBuf};

struct Candidate {
    entry: Entry,
    bucket: BucketName,
    name: ObjectName,
    binding: SyncBinding,
    tracked: Option<SyncRecord>,
    live: Option<crate::object::ObjectRecord>,
}

struct PinnedSource {
    file: File,
    _lease: RootLease,
}
impl std::borrow::Borrow<File> for PinnedSource {
    fn borrow(&self) -> &File {
        &self.file
    }
}

fn open_source(
    root: &File,
    path: &std::path::Path,
    expected: FileIdentity,
) -> Result<(File, FileIdentity), AdminError> {
    let directory = parent(
        root,
        path.parent().ok_or(AdminError::Precondition)?,
        ParentOpen::Existing,
    )
    .map_err(|error| match error {
        ParentError::Io(error) => AdminError::from(error),
        _ => AdminError::Precondition,
    })?;
    let file = match fsutil::child_regular(
        &directory,
        path.file_name().ok_or(AdminError::Precondition)?,
    )? {
        fsutil::RegularChild::File(file) => file,
        _ => return Err(AdminError::Precondition),
    };
    if fsutil::stat_identity(&file).map_err(fs_error)? != expected {
        return Err(AdminError::Precondition);
    }
    Ok((file, fsutil::stat_identity(&directory).map_err(fs_error)?))
}

fn same_stat(fp: &SyncFingerprint, stat: FileIdentity) -> bool {
    fp.matches_identity(stat)
}

fn deferred(path: &std::path::Path, reason: impl std::fmt::Display, summary: &mut Reconciliation) {
    let path = if path.as_os_str().is_empty() {
        std::path::Path::new(".")
    } else {
        path
    };
    summary.deferred += 1;
    diagnostics::emit(
        LogLevel::Warn,
        "sync_source_deferred",
        serde_json::json!({
            "path":path,"reason":reason.to_string(),
            "remedy":"Restore access to this file and its parents (for example, chmod u+r FILE and chmod u+rx DIRECTORY), free disk space if needed, then request another rescan"
        }),
    );
}

async fn forget_missing_directories(root: &RootLease, interrupt: Interrupt<'_>) -> io::Result<()> {
    let mut after = String::new();
    loop {
        if interrupt.check() != Interrupted::No {
            break;
        }
        let page = root
            .sync_buckets_page(after.clone(), 256)
            .await
            .map_err(io::Error::other)?;
        if page.is_empty() {
            break;
        }
        for (name, incarnation) in page {
            if interrupt.check() != Interrupted::No {
                return Ok(());
            }
            after = name.clone();
            let selected = name.clone();
            let absent = root
                .file_job(move |root| {
                    match fsutil::directory_at(
                        root,
                        selected.as_ref(),
                        fsutil::DirectoryOpen::Existing,
                    ) {
                        Ok(_) => Ok(false),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
                        Err(_) => Ok(false),
                    }
                })
                .await
                .map_err(io::Error::other)?;
            if absent {
                root.forget_sync_bucket(name, incarnation)
                    .await
                    .map_err(io::Error::other)?;
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
) -> io::Result<Reconciliation> {
    let writeback = root.default_writeback().await.map_err(io::Error::other)?;
    rescan_with(catalog, root, config, &writeback, Interrupt::new(stopping)).await
}
pub(super) async fn rescan_with(
    catalog: &Catalog,
    root: &RootLease,
    config: &Config,
    writeback: &crate::storage::sync_state::writeback::Writeback,
    interrupt: Interrupt<'_>,
) -> io::Result<Reconciliation> {
    if writeback.status().await.map_err(io::Error::other)?.stop
        != crate::storage::sync_state::writeback::Stop::Ready
    {
        return Err(io::Error::other(
            "Unfinished write-back blocks ingestion; inspect retained conflicts and recovery findings, preserve the workspace and resolve the exact retained revision before rescanning",
        ));
    }
    let ingest = Ingest {
        catalog,
        root,
        config,
        admin: root.sync_admin(),
    };
    let mut summary = Reconciliation::default();
    forget_missing_directories(root, interrupt).await?;
    let mut scan = Scan::new(root.tree_limits());
    while !scan.is_empty() && interrupt.check() == Interrupted::No {
        let (next, entries) = root
            .file_job(move |root| scan.page(root))
            .await
            .map_err(io::Error::other)?;
        scan = next;
        let mut candidates = Vec::new();
        let mut blocked = Vec::<PathBuf>::new();
        for entry in entries {
            if interrupt.check() != Interrupted::No {
                summary.interrupted = true;
                return Ok(summary);
            }
            if blocked.iter().any(|path| entry.path.starts_with(path)) {
                continue;
            }
            if let Some(error) = entry.error {
                deferred(&entry.path, error, &mut summary);
                continue;
            }
            if entry.directory {
                if ingest.bucket_directory(&entry, &mut summary).await? == BucketDirectory::Blocked
                {
                    scan.skip(&entry.path);
                    blocked.push(entry.path.clone());
                }
                continue;
            }
            if let Some(candidate) = ingest.candidate(entry, &mut summary).await? {
                candidates.push(candidate);
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
                summary.interrupted = true;
                return Ok(summary);
            }
        }
        for candidate in candidates {
            if interrupt.check() != Interrupted::No {
                summary.interrupted = true;
                return Ok(summary);
            }
            ingest.ingest(candidate, &mut summary).await?;
        }
    }
    summary.interrupted = interrupt.check() != Interrupted::No;
    if !summary.interrupted {
        clear_missing_observations(root, interrupt).await?;
        summary.interrupted = interrupt.check() != Interrupted::No;
    }
    if !summary.interrupted {
        root.observation_scan(summary.unrepresentable, summary.deferred)
            .await
            .map_err(io::Error::other)?;
    }
    Ok(summary)
}

async fn clear_missing_observations(root: &RootLease, interrupt: Interrupt<'_>) -> io::Result<()> {
    let mut after = 0;
    while interrupt.check() == Interrupted::No {
        let page = root
            .transient_stop_page(after)
            .await
            .map_err(io::Error::other)?;
        if page.is_empty() {
            break;
        }
        for (ordinal, binding, name) in page {
            after = ordinal;
            let bucket = BucketName::new(binding.bucket_name()).map_err(io::Error::other)?;
            if let mapping::PathMapping::File(path) = mapping::name_to_path(&bucket, &name)
                && let Some(path) = path.to_str()
                && super::deletion::absent(root, path.to_owned()).await?
            {
                root.clear_path_stops(
                    binding,
                    name,
                    &[PathStop::SourceChanging, PathStop::IoFailure],
                )
                .await
                .map_err(io::Error::other)?;
            }
        }
    }
    Ok(())
}

/// Whether a top-level directory may be ingested or belongs to a bucket deleted through the
/// API, which blocks the whole subtree until the user resolves it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BucketDirectory {
    Ready,
    Blocked,
}

/// Shared state of one ingestion rescan.
struct Ingest<'a> {
    catalog: &'a Catalog,
    root: &'a RootLease,
    config: &'a Config,
    admin: Admin,
}

impl Ingest<'_> {
    async fn bucket_directory(
        &self,
        entry: &Entry,
        summary: &mut Reconciliation,
    ) -> io::Result<BucketDirectory> {
        if entry.path.components().count() != 1 {
            return Ok(BucketDirectory::Ready);
        }
        let Some(text) = entry.path.to_str() else {
            summary.unrepresentable += 1;
            return Ok(BucketDirectory::Ready);
        };
        let Ok(bucket) = BucketName::new(text) else {
            summary.unrepresentable += 1;
            return Ok(BucketDirectory::Ready);
        };
        if self
            .root
            .ensure_sync_bucket(&bucket, &self.config.seed_project)
            .await
            .map_err(io::Error::other)?
        {
            return Ok(BucketDirectory::Ready);
        }
        summary.blocked_buckets += 1;
        summary.deferred += 1;
        diagnostics::emit(
            LogLevel::Warn,
            "sync_bucket_missing",
            serde_json::json!({
                "path":entry.path,"reason":"The previously synchronized bucket was deleted through the API",
                "remedy":"Remove or move this directory, or recreate the bucket through the API (for example, create the same bucket name in the intended project); then request another rescan"
            }),
        );
        Ok(BucketDirectory::Blocked)
    }

    /// Selects a regular file whose observed state differs from its tracked fingerprint;
    /// `None` means it is unchanged or was counted as ignored, unrepresentable or deferred.
    async fn candidate(
        &self,
        entry: Entry,
        summary: &mut Reconciliation,
    ) -> io::Result<Option<Candidate>> {
        let (bucket, name) = match mapping::path_to_name(&entry.path) {
            Ok(mapped) => mapped,
            Err(mapping::Reason::Ignored) => {
                summary.ignored += 1;
                return Ok(None);
            }
            Err(_) => {
                summary.unrepresentable += 1;
                return Ok(None);
            }
        };
        let Some(identity) = entry.identity else {
            summary.unrepresentable += 1;
            diagnostics::emit(
                LogLevel::Warn,
                "sync_unsafe_entry",
                serde_json::json!({"path":entry.path,"remedy":"Replace the symlink or special file with a regular file; for example, copy its intended bytes into a new regular file"}),
            );
            return Ok(None);
        };
        let state = self.catalog.sync_state();
        let binding = match self.root.sync_binding(&bucket).await {
            Ok(binding) => binding,
            Err(AdminError::NotFound | AdminError::Precondition) => {
                summary.deferred += 1;
                return Ok(None);
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        let path = entry
            .path
            .to_str()
            .ok_or_else(|| io::Error::other("invalid sync path"))?
            .to_owned();
        let tracked = match state.get_path(binding.clone(), path.clone()).await {
            Ok(tracked) => tracked,
            Err(AdminError::NotFound | AdminError::Precondition) => {
                summary.deferred += 1;
                return Ok(None);
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        if tracked
            .as_ref()
            .is_some_and(|record| same_stat(&record.fingerprint, identity))
        {
            let tracked = tracked
                .as_ref()
                .ok_or_else(|| io::Error::other("missing tracked source"))?;
            if !tracked.fingerprint.racy {
                self.clear_observation(binding, name).await?;
                return Ok(None);
            }
            if self.racy_clean(&entry, tracked, &binding, &name).await? {
                summary.identical += 1;
                self.clear_observation(binding, name).await?;
                return Ok(None);
            }
        }
        let live = match self.admin.get_object(&bucket, &name, None).await {
            Ok(record) => Some(record),
            Err(AdminError::NotFound) => None,
            Err(error) => return Err(io::Error::other(error)),
        };
        if super::mirror::hierarchy_collision(&self.admin, &bucket, &name)
            .await
            .map_err(io::Error::other)?
        {
            summary.unrepresentable += 1;
            diagnostics::emit(
                LogLevel::Warn,
                "sync_path_collision",
                serde_json::json!({"path":entry.path,"remedy":"Rename the external file or the API object so a file is not also a parent directory; for example, use files/object instead of files"}),
            );
            return Ok(None);
        }
        Ok(Some(Candidate {
            entry,
            bucket,
            name,
            binding,
            tracked,
            live,
        }))
    }

    async fn racy_clean(
        &self,
        entry: &Entry,
        tracked: &SyncRecord,
        binding: &SyncBinding,
        name: &ObjectName,
    ) -> io::Result<bool> {
        let Some(expected) = entry.identity else {
            return Ok(false);
        };
        let path = entry.path.clone();
        let observed = self
            .root
            .file_job(move |root| {
                let (file, parent) = open_source(root, &path, expected)?;
                let hashes = fsutil::hash_exact(&file, expected.size, |_| Ok::<_, io::Error>(()))?;
                let (_, current) = open_source(root, &path, expected)?;
                if parent.device != current.device || parent.inode != current.inode {
                    return Err(AdminError::Precondition);
                }
                Ok(hashes)
            })
            .await;
        let hashes = match observed {
            Ok(fsutil::HashOutcome::Complete(hashes)) => hashes,
            Ok(_) | Err(RootError::Admin(AdminError::Precondition | AdminError::NotFound)) => {
                return Ok(false);
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        if hashes.md5 != tracked.fingerprint.md5 || hashes.crc32c != tracked.fingerprint.crc32c {
            return Ok(false);
        }
        let fingerprint = SyncFingerprint::observed(
            expected,
            hashes,
            self.root.observation_time_ns().map_err(io::Error::other)?,
        );
        let update = SyncUpdate::new(
            binding.clone(),
            entry.path.to_string_lossy().into_owned(),
            fingerprint,
        )
        .map_err(io::Error::other)?;
        match self
            .catalog
            .sync_state()
            .acknowledge(name, tracked.generation, update)
            .await
        {
            Ok(()) => Ok(true),
            Err(AdminError::Precondition | AdminError::NotFound) => Ok(false),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    /// Stages the file's current bytes, verifying that its identity and parent did not change
    /// while it was read.
    async fn stage(
        &self,
        selected: PathBuf,
        identity: FileIdentity,
    ) -> Result<StagedObject, RootError> {
        let path = selected.clone();
        let (file, directory) = self
            .root
            .file_job(move |root| open_source(root, &path, identity))
            .await?;
        #[cfg(test)]
        checkpoint("stage").await;
        let source = PinnedSource {
            file,
            _lease: self.root.clone(),
        };
        let staged = self
            .admin
            .stage_from_file(source, Default::default(), self.config.max_object_bytes)
            .await?;
        #[cfg(test)]
        checkpoint("staged").await;
        let checked = self
            .root
            .file_job(move |root| {
                let (_, current) = open_source(root, &selected, identity)?;
                if current.device != directory.device || current.inode != directory.inode {
                    return Err(AdminError::Precondition);
                }
                Ok(())
            })
            .await;
        if let Err(error) = checked {
            if let Err(cleanup) = staged.discard().await {
                diagnostics::emit(
                    LogLevel::Error,
                    "sync_staging_cleanup_failed",
                    serde_json::json!({"reason":crate::error::Report(&cleanup).to_string()}),
                );
            }
            return Err(error);
        }
        Ok(staged)
    }

    /// Stages a candidate's bytes with the fingerprint that tracks them; `None` means the
    /// file changed or became unreadable and was deferred.
    async fn stage_update(
        &self,
        entry: &Entry,
        binding: &SyncBinding,
        summary: &mut Reconciliation,
    ) -> io::Result<Option<(StagedObject, SyncUpdate)>> {
        let identity = entry
            .identity
            .ok_or_else(|| io::Error::other("missing observed file identity"))?;
        let path = entry
            .path
            .to_str()
            .ok_or_else(|| io::Error::other("invalid sync path"))?
            .to_owned();
        let staged = match self.stage(entry.path.clone(), identity).await {
            Ok(staged) => staged,
            Err(RootError::Admin(error))
                if matches!(
                    &error,
                    AdminError::Precondition
                        | AdminError::NotFound
                        | AdminError::SourceChanged { .. }
                ) || matches!(&error, AdminError::Io(cause) if cause.kind() == io::ErrorKind::NotFound) =>
            {
                summary.deferred += 1;
                self.record_observation(entry, binding, PathStop::SourceChanging)
                    .await?;
                return Ok(None);
            }
            Err(RootError::Admin(AdminError::Io(error))) => {
                deferred(&entry.path, error.kind(), summary);
                self.record_observation(entry, binding, PathStop::IoFailure)
                    .await?;
                return Ok(None);
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        let hashes = staged.hashes();
        if let Ok((_, name)) = mapping::path_to_name(&entry.path) {
            self.clear_observation(binding.clone(), name).await?;
        }
        let fingerprint = SyncFingerprint::observed(
            identity,
            hashes,
            self.root.observation_time_ns().map_err(io::Error::other)?,
        );
        let update =
            SyncUpdate::new(binding.clone(), path, fingerprint).map_err(io::Error::other)?;
        Ok(Some((staged, update)))
    }

    async fn record_observation(
        &self,
        entry: &Entry,
        binding: &SyncBinding,
        reason: PathStop,
    ) -> io::Result<()> {
        let (_, name) = mapping::path_to_name(&entry.path)
            .map_err(|_| io::Error::other("invalid deferred synchronization target"))?;
        match self
            .root
            .record_path_stop(binding.clone(), name, reason)
            .await
        {
            Ok(()) | Err(RootError::Admin(AdminError::Precondition | AdminError::NotFound)) => {
                Ok(())
            }
            Err(error) => Err(io::Error::other(error)),
        }
    }

    async fn clear_observation(&self, binding: SyncBinding, name: ObjectName) -> io::Result<()> {
        self.root
            .clear_path_stops(
                binding,
                name,
                &[PathStop::SourceChanging, PathStop::IoFailure],
            )
            .await
            .map_err(io::Error::other)?;
        Ok(())
    }

    async fn ingest(&self, candidate: Candidate, summary: &mut Reconciliation) -> io::Result<()> {
        let Candidate {
            entry,
            bucket,
            name,
            binding,
            tracked,
            live,
        } = candidate;
        let Some((staged, update)) = self.stage_update(&entry, &binding, summary).await? else {
            return Ok(());
        };
        let hashes = staged.hashes();
        if let Some(live) = &live {
            let generation = Generation::new(live.generation).map_err(io::Error::other)?;
            if live.size == staged.size() && live.md5 == hashes.md5 && live.crc32c == hashes.crc32c
            {
                staged.discard().await.map_err(io::Error::other)?;
                match self
                    .catalog
                    .sync_state()
                    .acknowledge(&name, generation, update)
                    .await
                {
                    Ok(()) => summary.identical += 1,
                    Err(AdminError::Precondition | AdminError::NotFound) => summary.deferred += 1,
                    Err(error) => return Err(io::Error::other(error)),
                }
                return Ok(());
            }
            if tracked
                .as_ref()
                .is_none_or(|record| record.generation != generation)
            {
                staged.discard().await.map_err(io::Error::other)?;
                self.root
                    .record_export_conflict(binding, name, generation)
                    .await
                    .map_err(io::Error::other)?;
                summary.conflicting += 1;
                return Ok(());
            }
        } else if let Some(tracked) = &tracked {
            let size = staged.size();
            staged.discard().await.map_err(io::Error::other)?;
            if tracked.fingerprint.size == size
                && tracked.fingerprint.md5 == hashes.md5
                && tracked.fingerprint.crc32c == hashes.crc32c
            {
                // A native notification can race API deletion before its unchanged output is removed.
                summary.deferred += 1;
                return Ok(());
            }
            self.root
                .record_export_conflict(binding, name, tracked.generation)
                .await
                .map_err(io::Error::other)?;
            summary.conflicting += 1;
            return Ok(());
        }
        #[cfg(test)]
        checkpoint("publish").await;
        let content_type = self
            .config
            .seed_content_type
            .for_name(
                entry
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default(),
            )
            .into();
        let condition = live
            .as_ref()
            .map_or(Ok(GenerationCondition::Absent), |record| {
                Generation::new(record.generation).map(GenerationCondition::Is)
            })
            .map_err(io::Error::other)?;
        self.root.revalidate().await.map_err(io::Error::other)?;
        let result = self
            .admin
            .publish_objects(vec![BatchPublication {
                bucket,
                name,
                request: Publication {
                    content_type,
                    metadata: Default::default(),
                    expected: Default::default(),
                    condition: Some(condition),
                    max_bytes: self.config.max_object_bytes,
                    mode: PublishMode::InheritAndSkipIfSameHash,
                },
                staged: staged.with_sync_state(update),
            }])
            .await
            .map_err(io::Error::other)?;
        for result in result.results {
            match result {
                Ok(_) => summary.ingested += 1,
                Err(AdminError::Precondition | AdminError::NotFound | AdminError::Hold) => {
                    summary.deferred += 1
                }
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
use super::Hook;
#[cfg(test)]
tokio::task_local! { static HOOK: std::sync::Arc<Hook>; }
#[cfg(test)]
tokio::task_local! { static STAGES: std::sync::Arc<std::sync::atomic::AtomicUsize>; }
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
