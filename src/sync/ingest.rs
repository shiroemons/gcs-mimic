use super::{
    mapping,
    mirror::{ParentError, Reconciliation, fs_error, parent},
};
use crate::{
    config::Config,
    diagnostics::{self, LogLevel},
    fsutil::{self, FileIdentity},
    storage::{
        Catalog,
        admin::{
            Admin, AdminError, BatchPublication, BucketName, Generation, GenerationCondition,
            ObjectName, Origin, Publication, PublishMode, StagedObject,
        },
        sync_state::{
            SyncBinding, SyncFingerprint, SyncRecord, SyncUpdate,
            root::{RootError, RootLease},
        },
    },
};
use std::{ffi::OsString, fs::File, io, path::PathBuf};

struct Scan {
    stack: Vec<(PathBuf, Option<OsString>)>,
}
struct Entry {
    path: PathBuf,
    identity: Option<FileIdentity>,
    directory: bool,
    error: Option<io::ErrorKind>,
}

struct Candidate {
    entry: Entry,
    bucket: BucketName,
    name: ObjectName,
    binding: SyncBinding,
    tracked: Option<SyncRecord>,
    live: Option<crate::object::ObjectRecord>,
}

impl Scan {
    fn page(mut self, root: &File) -> Result<(Self, Vec<Entry>), AdminError> {
        let mut entries = Vec::new();
        while entries.len() < 256 {
            let Some((path, after)) = self.stack.last_mut() else {
                break;
            };
            let directory = match parent(root, path, fsutil::DirectoryOpen::Existing) {
                Ok(directory) => directory,
                Err(ParentError::Finding(_)) => {
                    self.stack.pop();
                    continue;
                }
                Err(ParentError::Io(error)) => {
                    entries.push(Entry {
                        path: path.clone(),
                        identity: None,
                        directory: true,
                        error: Some(error.kind()),
                    });
                    self.stack.pop();
                    continue;
                }
            };
            let names = match fsutil::directory_names_after(&directory, after.as_deref(), 1) {
                Ok(names) => names,
                Err(error) if error.kind() != io::ErrorKind::InvalidData => {
                    entries.push(Entry {
                        path: path.clone(),
                        identity: None,
                        directory: true,
                        error: Some(error.kind()),
                    });
                    self.stack.pop();
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let Some(name) = names.into_iter().next() else {
                self.stack.pop();
                continue;
            };
            *after = Some(name.clone());
            if path.as_os_str().is_empty()
                && matches!(name.to_str(), Some(mapping::MARKER | mapping::WORKSPACE))
            {
                continue;
            }
            let relative = path.join(&name);
            if let Ok(child) =
                fsutil::directory_at(&directory, &name, fsutil::DirectoryOpen::Existing)
            {
                drop(child);
                if self.stack.len() >= 128 {
                    return Err(AdminError::Invalid(
                        "sync tree exceeds 128 levels; reduce its depth".into(),
                    ));
                }
                self.stack.push((relative.clone(), None));
                entries.push(Entry {
                    path: relative,
                    identity: None,
                    directory: true,
                    error: None,
                });
            } else {
                let mut failure = None;
                let identity = match fsutil::open_regular_at(&directory, &name) {
                    Ok(file) => match fsutil::stat_identity(&file).map_err(fs_error) {
                        Ok(identity) => Some(identity),
                        Err(AdminError::Io(error)) => {
                            failure = Some(error);
                            None
                        }
                        Err(error) => return Err(error),
                    },
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) if !super::mirror::structural_entry_error(&error) => {
                        failure = Some(error.kind());
                        None
                    }
                    Err(_) => None,
                };
                entries.push(Entry {
                    path: relative,
                    identity,
                    directory: false,
                    error: failure,
                });
            }
        }
        Ok((self, entries))
    }
}

fn same_stat(fp: &SyncFingerprint, stat: FileIdentity) -> bool {
    !fp.racy
        && fp.size == stat.size
        && fp.inode == stat.inode
        && fp.mtime_ns == stat.modified_ns
        && fp.ctime_ns == stat.changed_ns
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

async fn forget_missing_directories(
    root: &RootLease,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> io::Result<()> {
    let mut after = String::new();
    loop {
        if *stopping.borrow() {
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
            if *stopping.borrow() {
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

pub(super) async fn rescan(
    catalog: &Catalog,
    root: &RootLease,
    config: &Config,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> io::Result<Reconciliation> {
    let writeback = root
        .writeback(
            crate::storage::sync_state::writeback::Limits::new(
                super::CONFLICT_MAX_BYTES,
                super::CONFLICT_MAX_ENTRIES,
            )
            .map_err(io::Error::other)?,
        )
        .await
        .map_err(io::Error::other)?;
    if writeback.status().await.map_err(io::Error::other)?.stop
        != crate::storage::sync_state::writeback::Stop::Ready
    {
        return Err(io::Error::other(
            "Unfinished write-back blocks ingestion; preserve the working tree/workspace and select a fresh sync_dir after backup until recovery controls are available",
        ));
    }
    let ingest = Ingest {
        catalog,
        root,
        config,
        admin: catalog.admin(Origin::SyncIngest),
    };
    let mut summary = Reconciliation::default();
    forget_missing_directories(root, stopping).await?;
    let mut scan = Scan {
        stack: vec![(PathBuf::new(), None)],
    };
    while !scan.stack.is_empty() && !*stopping.borrow() {
        let (next, entries) = root
            .file_job(move |root| scan.page(root))
            .await
            .map_err(io::Error::other)?;
        scan = next;
        let mut candidates = Vec::new();
        let mut blocked = Vec::<PathBuf>::new();
        for entry in entries {
            if *stopping.borrow() {
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
                    scan.stack
                        .retain(|(path, _)| !path.starts_with(&entry.path));
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
            let mut stopping = stopping.clone();
            if *stopping.borrow() {
                return Ok(summary);
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(super::SETTLE_MS)) => {},
                _ = stopping.changed() => return Ok(summary),
            }
        }
        for candidate in candidates {
            if *stopping.borrow() {
                return Ok(summary);
            }
            ingest.ingest(candidate, &mut summary).await?;
        }
    }
    Ok(summary)
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
        let binding = match state.binding(&bucket).await {
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
            return Ok(None);
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

    /// Stages the file's current bytes, verifying that its identity and parent did not change
    /// while it was read.
    async fn stage(
        &self,
        selected: PathBuf,
        identity: FileIdentity,
    ) -> Result<StagedObject, RootError> {
        let staging_admin = self.admin.clone();
        let runtime = tokio::runtime::Handle::current();
        let max = self.config.max_object_bytes;
        self.root
            .file_job(move |root| {
                let directory = parent(
                    root,
                    selected.parent().ok_or(AdminError::Precondition)?,
                    fsutil::DirectoryOpen::Existing,
                )
                .map_err(|_| AdminError::Precondition)?;
                let leaf = selected.file_name().ok_or(AdminError::Precondition)?;
                let file = fsutil::open_regular_at(&directory, leaf)?;
                if fsutil::stat_identity(&file).map_err(fs_error)? != identity {
                    return Err(AdminError::Precondition);
                }
                let staged = runtime.block_on(staging_admin.stage_from_file(
                    file,
                    Default::default(),
                    max,
                ))?;
                let current = parent(
                    root,
                    selected.parent().ok_or(AdminError::Precondition)?,
                    fsutil::DirectoryOpen::Existing,
                )
                .map_err(|_| AdminError::Precondition)?;
                let file = fsutil::open_regular_at(&current, leaf)?;
                if fsutil::stat_identity(&file).map_err(fs_error)? != identity {
                    return Err(AdminError::Precondition);
                }
                Ok(staged)
            })
            .await
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
            Err(RootError::Admin(
                AdminError::Precondition
                | AdminError::NotFound
                | AdminError::SourceChanged { .. }
                | AdminError::Io(io::ErrorKind::NotFound),
            )) => {
                summary.deferred += 1;
                return Ok(None);
            }
            Err(RootError::Admin(AdminError::Io(error))) => {
                deferred(&entry.path, error, summary);
                return Ok(None);
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        let hashes = staged.hashes();
        let fingerprint = SyncFingerprint {
            size: staged.size(),
            mtime_ns: identity.modified_ns,
            ctime_ns: identity.changed_ns,
            inode: identity.inode,
            md5: hashes.md5,
            crc32c: hashes.crc32c,
            racy: false,
        };
        let update =
            SyncUpdate::new(binding.clone(), path, fingerprint).map_err(io::Error::other)?;
        Ok(Some((staged, update)))
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
            staged.discard().await.map_err(io::Error::other)?;
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
struct Hook {
    phase: &'static str,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[cfg(test)]
tokio::task_local! { static HOOK: std::sync::Arc<Hook>; }
#[cfg(test)]
async fn checkpoint(phase: &str) {
    if let Ok(hook) = HOOK.try_with(std::sync::Arc::clone)
        && hook.phase == phase
    {
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
