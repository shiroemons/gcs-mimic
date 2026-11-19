use super::interrupt::{Interrupt, Interrupted};
use super::tree::TreeScan as Scan;
use super::{
    Pass, SyncError,
    deletion::Presence,
    entries::{EntryClass, EntryProblem, MappedFile, classify},
    mapping,
    mirror::Reconciliation,
    naming::RootNaming,
    outstanding::TreeSide,
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
            Ordinal, SyncBinding, SyncFingerprint, SyncPath, SyncRecord, SyncUpdate,
            durable::{self, Durable, NotDurable, SyncedDirectories},
            observation::{PathStop, TransientStop},
            root::{RootError, RootLease},
        },
    },
};
use std::{fs::File, io, path::PathBuf};

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

/// What a file's stat observation shows about the body its tracked fingerprint recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackedCheck {
    /// Size, inode, mtime and ctime match a stable fingerprint: the body is unchanged.
    Unchanged,
    /// Only hashing the file can tell: the fingerprint is racy, or the inode number alone
    /// differs. Docker Desktop's shared folders can report a new number for an unchanged
    /// file, so that difference alone does not show an edit.
    Hash,
    /// Size, mtime or ctime differ: the file is an ingestion candidate.
    Changed,
}

impl TrackedCheck {
    fn new(fingerprint: &SyncFingerprint, identity: FileIdentity) -> Self {
        let changes = fingerprint.changed_fields(identity);
        if changes.is_empty() && !fingerprint.racy {
            Self::Unchanged
        } else if changes.is_empty() || changes.is_inode_only() {
            Self::Hash
        } else {
            Self::Changed
        }
    }
}

/// What hashing a file showed about its tracked body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HashCheck {
    /// The file holds the tracked body, and its observed identity is now recorded.
    Recorded,
    /// The file holds the tracked body, but the API generation moved on or the object is gone,
    /// so the observed identity is not recorded. The file is no external edit, and the pass
    /// handles it like an unchanged file: the export or removal of the API change replaces it.
    /// Ingesting it would report that change as a conflict. Counting it as deferred would keep
    /// a wait reporting the source as changing, because only that export records a new
    /// fingerprint, and a finding can hold the export back indefinitely.
    Superseded,
    /// The bytes differ, or the file changed while it was read.
    Differs,
    /// The file holds the tracked body, but it could not be made durable, so nothing is recorded
    /// and it counts as deferred, as an ingested file would.
    Deferred,
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
    naming: RootNaming,
    path: &std::path::Path,
    expected: FileIdentity,
) -> Result<(File, FileIdentity), AdminError> {
    let directory = parent(
        root,
        naming,
        path.parent().ok_or(AdminError::Precondition)?,
        ParentOpen::Existing,
    )
    .map_err(|error| match error {
        ParentError::Io(error) => AdminError::from(error),
        ParentError::Finding(_) | ParentError::Missing => AdminError::Precondition,
    })?;
    let fsutil::RegularChild::File(file) = fsutil::child_regular(
        &directory,
        path.file_name().ok_or(AdminError::Precondition)?,
    )?
    else {
        return Err(AdminError::Precondition);
    };
    if fsutil::stat_identity(&file)? != expected {
        return Err(AdminError::Precondition);
    }
    Ok((file, fsutil::stat_identity(&directory)?))
}

/// The second half of reading a source: re-resolves `path` below `root` and fails with
/// `Precondition` unless it still names the unchanged file `expected` in the directory `parent`.
/// `open_source` took both identities before the read, so a file or any of its ancestors
/// replaced or renamed while it was read is detected.
fn recheck_source(
    root: &File,
    naming: RootNaming,
    path: &std::path::Path,
    expected: FileIdentity,
    parent: FileIdentity,
) -> Result<(), AdminError> {
    let (_, current) = open_source(root, naming, path, expected)?;
    if current.node() == parent.node() {
        Ok(())
    } else {
        Err(AdminError::Precondition)
    }
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
        config,
        admin: root.sync_admin(),
        summary: Reconciliation::default(),
        synced: SyncedDirectories::default(),
    };
    forget_missing_directories(root, interrupt).await?;
    let mut scan = Scan::new(root.tree_limits());
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
                    ingest.unrepresentable(&entry.path, problem);
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

/// The path stops that what the tree holds at a path decides, which a scan clears once the path
/// holds the regular file again or nothing at all.
const TREE_STOPS: [PathStop; 3] = [
    PathStop::SourceChanging,
    PathStop::IoFailure,
    PathStop::EntryReplaced,
];

async fn clear_missing_observations(
    root: &RootLease,
    interrupt: Interrupt<'_>,
) -> Result<(), SyncError> {
    let naming = root.naming()?;
    let mut after = Ordinal::START;
    while interrupt.check() == Interrupted::No {
        let page = root.transient_stop_page(after).await?;
        if page.is_empty() {
            break;
        }
        for TransientStop {
            ordinal,
            binding,
            name,
        } in page
        {
            after = ordinal;
            if let mapping::PathMapping::File(path) =
                mapping::name_to_path(binding.bucket(), &name, naming)
                && let Some(path) = path.to_str()
                && let Presence::Absent(_) =
                    super::deletion::presence(root, path.to_owned()).await?
            {
                root.clear_path_stops(binding, name, &TREE_STOPS).await?;
            }
        }
    }
    Ok(())
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
                    Ok(Some(name)) if passed_over(&directory, &bucket, &name, naming) => {}
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
) -> bool {
    let file = match fsutil::child_regular(directory, name) {
        Ok(fsutil::RegularChild::File(file)) => file,
        // Removed since the listing: it holds nothing any more.
        Ok(fsutil::RegularChild::Missing) => return true,
        Ok(fsutil::RegularChild::Alias) | Err(_) => return false,
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
                "path":path,"reason":kind.to_string(),
                "remedy":"Restore access to this file and its parents (for example, chmod u+r FILE and chmod u+rx DIRECTORY), free disk space if needed, then request another rescan"
            }),
        );
    }

    /// Counts an entry no object can take, and names an unsafe one in the log.
    fn unrepresentable(&mut self, path: &std::path::Path, problem: EntryProblem) {
        self.summary.count_unrepresentable_entry(path, problem);
        if problem == EntryProblem::UnsafeEntry {
            diagnostics::emit(
                LogLevel::Warn,
                "sync_unsafe_entry",
                serde_json::json!({"path":path,"remedy":"Replace the symlink or special file with a regular file; for example, copy its intended bytes into a new regular file"}),
            );
        }
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
        diagnostics::emit(
            LogLevel::Warn,
            "sync_bucket_missing",
            serde_json::json!({
                "path":path,"reason":"The previously synchronized bucket was deleted through the API",
                "remedy":"Remove or move this directory, or recreate the bucket through the API (for example, create the same bucket name in the intended project); then request another rescan"
            }),
        );
        Ok(BucketDirectory::Blocked)
    }

    /// Selects a regular file whose observed state differs from its tracked fingerprint;
    /// `None` means it is unchanged or was counted as unrepresentable or deferred.
    async fn candidate(
        &mut self,
        path: PathBuf,
        file: MappedFile,
    ) -> Result<Option<Candidate>, SyncError> {
        let MappedFile {
            bucket,
            name,
            identity,
        } = file;
        let state = self.catalog.sync_state();
        let binding = match self.root.sync_binding(&bucket).await {
            Ok(binding) => binding,
            Err(AdminError::NotFound | AdminError::Precondition) => {
                self.summary.count_deferred(&path);
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let text = path
            .to_str()
            .ok_or(SyncError::Invariant("invalid sync path"))?
            .to_owned();
        // The tracked state's lookup checks the same spelling, so this fails exactly where it would.
        let key = SyncPath::try_from(text.clone())?;
        let tracked = match state.get_path(binding.clone(), text).await {
            Ok(tracked) => tracked,
            Err(AdminError::NotFound | AdminError::Precondition) => {
                self.summary.count_deferred(&path);
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(tracked) = &tracked {
            let unchanged = match TrackedCheck::new(&tracked.fingerprint, identity) {
                TrackedCheck::Unchanged => true,
                TrackedCheck::Hash => match self
                    .hash_tracked(&key, identity, tracked, &binding, &name)
                    .await?
                {
                    HashCheck::Recorded => {
                        self.summary.identical += 1;
                        true
                    }
                    HashCheck::Superseded => true,
                    HashCheck::Differs => false,
                    HashCheck::Deferred => return Ok(None),
                },
                TrackedCheck::Changed => false,
            };
            if unchanged {
                self.clear_observation(binding, name).await?;
                return Ok(None);
            }
        }
        let live = match self.admin.get_object(&bucket, &name, None).await {
            Ok(record) => Some(record),
            Err(AdminError::NotFound) => None,
            Err(error) => return Err(error.into()),
        };
        if super::mirror::hierarchy_collision(&self.admin, &bucket, &name).await? {
            self.summary
                .count_unrepresentable_entry(&path, EntryProblem::PathCollision);
            diagnostics::emit(
                LogLevel::Warn,
                "sync_path_collision",
                serde_json::json!({"path":path,"remedy":"Rename the external file or the API object so a file is not also a parent directory; for example, use files/object instead of files"}),
            );
            return Ok(None);
        }
        Ok(Some(Candidate {
            path,
            key,
            identity,
            bucket,
            name,
            binding,
            tracked,
            live,
        }))
    }

    /// Hashes the file at `path`, observed as `expected`, and compares it with the tracked body.
    /// A match records the observed identity, so a later stable pass skips the hash. The file
    /// may be a new one with the same bytes, such as an editor's save through a rename, whose
    /// entry is as volatile as an ingested file's, so it is made durable first like one.
    async fn hash_tracked(
        &mut self,
        path: &SyncPath,
        expected: FileIdentity,
        tracked: &SyncRecord,
        binding: &SyncBinding,
        name: &ObjectName,
    ) -> Result<HashCheck, SyncError> {
        let selected = PathBuf::from(path.as_str());
        let naming = self.naming;
        let observed = self
            .root
            .file_job(move |root| {
                let (file, parent) = open_source(root, naming, &selected, expected)?;
                let hashes = fsutil::hash_exact(&file, expected.size, |_| Ok::<_, io::Error>(()))?;
                recheck_source(root, naming, &selected, expected, parent)?;
                Ok(hashes)
            })
            .await;
        let hashes = match observed {
            Ok(fsutil::HashOutcome::Complete(hashes)) => hashes,
            Ok(_) | Err(RootError::Admin(AdminError::Precondition | AdminError::NotFound)) => {
                return Ok(HashCheck::Differs);
            }
            Err(error) => return Err(error.into()),
        };
        if hashes.md5 != tracked.fingerprint.md5 || hashes.crc32c != tracked.fingerprint.crc32c {
            return Ok(HashCheck::Differs);
        }
        let fingerprint =
            SyncFingerprint::observed(expected, hashes, self.root.observation_time_ns()?);
        let Some(durable) = self.make_durable(path, expected, binding, name).await? else {
            return Ok(HashCheck::Deferred);
        };
        let update = SyncUpdate::new(binding.clone(), durable, fingerprint)?;
        match self
            .catalog
            .sync_state()
            .acknowledge(name, tracked.generation, update)
            .await
        {
            Ok(()) => Ok(HashCheck::Recorded),
            Err(AdminError::Precondition | AdminError::NotFound) => Ok(HashCheck::Superseded),
            Err(error) => Err(error.into()),
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
        let naming = self.naming;
        let (file, directory) = self
            .root
            .file_job(move |root| open_source(root, naming, &path, identity))
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
            .file_job(move |root| recheck_source(root, naming, &selected, identity, directory))
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
        &mut self,
        candidate: &Candidate,
    ) -> Result<Option<(StagedObject, SyncFingerprint)>, SyncError> {
        let staged = match self.stage(candidate.path.clone(), candidate.identity).await {
            Ok(staged) => staged,
            Err(RootError::Admin(error))
                if matches!(
                    &error,
                    AdminError::Precondition
                        | AdminError::NotFound
                        | AdminError::SourceChanged { .. }
                ) || matches!(&error, AdminError::Io(cause) if cause.kind() == io::ErrorKind::NotFound) =>
            {
                self.summary.count_deferred(&candidate.path);
                self.record_observation(
                    &candidate.binding,
                    &candidate.name,
                    PathStop::SourceChanging,
                )
                .await?;
                return Ok(None);
            }
            Err(RootError::Admin(AdminError::Io(error))) => {
                self.defer(&candidate.path, error.kind());
                self.record_observation(&candidate.binding, &candidate.name, PathStop::IoFailure)
                    .await?;
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let hashes = staged.hashes();
        self.clear_observation(candidate.binding.clone(), candidate.name.clone())
            .await?;
        let fingerprint =
            SyncFingerprint::observed(candidate.identity, hashes, self.root.observation_time_ns()?);
        Ok(Some((staged, fingerprint)))
    }

    async fn record_observation(
        &self,
        binding: &SyncBinding,
        name: &ObjectName,
        reason: PathStop,
    ) -> Result<(), SyncError> {
        super::error::unless_superseded(
            self.root
                .record_path_stop(binding.clone(), name.clone(), reason)
                .await,
        )?;
        Ok(())
    }

    /// Makes the file at `path`, observed as `identity`, durable together with the directory
    /// entries above it, before the catalog records it. `None` means it could not: the file is
    /// counted as deferred with the path stop that names why, nothing is recorded for it, and the
    /// next scan tries again.
    async fn make_durable(
        &mut self,
        path: &SyncPath,
        identity: FileIdentity,
        binding: &SyncBinding,
        name: &ObjectName,
    ) -> Result<Option<Durable>, SyncError> {
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        crate::storage::fault::hit_io(
            &self.config.data_dir,
            crate::storage::fault::Step::SyncSourceSync,
        )?;
        let selected = path.clone();
        let mut synced = std::mem::take(&mut self.synced);
        let (synced, outcome) = self
            .root
            .file_job(move |root| {
                let outcome = durable::make_durable(root, &selected, identity, &mut synced);
                Ok((synced, outcome))
            })
            .await?;
        self.synced = synced;
        match outcome {
            Ok(durable) => Ok(Some(durable)),
            Err(NotDurable::Changed) => {
                self.summary.count_deferred(path.as_ref());
                self.record_observation(binding, name, PathStop::SourceChanging)
                    .await?;
                Ok(None)
            }
            Err(NotDurable::Io(error)) => {
                self.defer(path.as_ref(), error.kind());
                self.record_observation(binding, name, PathStop::IoFailure)
                    .await?;
                Ok(None)
            }
        }
    }

    async fn clear_observation(
        &self,
        binding: SyncBinding,
        name: ObjectName,
    ) -> Result<(), SyncError> {
        self.root
            .clear_path_stops(binding, name, &TREE_STOPS)
            .await?;
        Ok(())
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

    /// Tracks the candidate's file, which holds the bytes of the live generation `generation`.
    async fn acknowledge_identical(
        &mut self,
        candidate: &Candidate,
        generation: Generation,
        fingerprint: SyncFingerprint,
    ) -> Result<(), SyncError> {
        let Some(durable) = self
            .make_durable(
                &candidate.key,
                candidate.identity,
                &candidate.binding,
                &candidate.name,
            )
            .await?
        else {
            return Ok(());
        };
        let update = SyncUpdate::new(candidate.binding.clone(), durable, fingerprint)?;
        match self
            .catalog
            .sync_state()
            .acknowledge(&candidate.name, generation, update)
            .await
        {
            Ok(()) => {
                self.summary.identical += 1;
                #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                self.recorded().await?;
            }
            Err(AdminError::Precondition | AdminError::NotFound) => {
                self.summary.count_deferred(&candidate.path);
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    /// Publishes the candidate's staged bytes as the object's next generation and tracks its file.
    async fn publish(
        &mut self,
        candidate: Candidate,
        staged: StagedObject,
        fingerprint: SyncFingerprint,
    ) -> Result<(), SyncError> {
        #[cfg(test)]
        checkpoint("publish").await;
        let Candidate {
            path,
            key,
            identity,
            bucket,
            name,
            binding,
            live,
            ..
        } = candidate;
        let content_type = self
            .config
            .seed_content_type
            .for_name(
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default(),
            )
            .into();
        let condition = live
            .as_ref()
            .map_or(Ok(GenerationCondition::Absent), |record| {
                Generation::new(record.generation).map(GenerationCondition::Is)
            })?;
        let Some(durable) = self.make_durable(&key, identity, &binding, &name).await? else {
            staged.discard().await?;
            return Ok(());
        };
        let update = SyncUpdate::new(binding, durable, fingerprint)?;
        self.root.revalidate().await?;
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
            .await?;
        for result in result.results {
            match result {
                Ok(_) => {
                    self.summary.ingested += 1;
                    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                    self.recorded().await?;
                }
                Err(AdminError::Precondition | AdminError::NotFound | AdminError::Hold) => {
                    self.summary.count_deferred(&path)
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    /// The fault seam after ingestion recorded a file: the tree and the catalog are snapshotted
    /// together there.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    async fn recorded(&self) -> Result<(), SyncError> {
        use crate::storage::fault::{self, Step};
        let data = self.config.data_dir.clone();
        self.root
            .file_job(move |root| {
                fault::hit_io(&fault::descriptor_path(root)?, Step::SyncSourceRecorded)?;
                fault::hit_io(&data, Step::SyncSourceRecorded)?;
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
