use super::mapping::{self, PathMapping, Reason};
use crate::{
    config::Config,
    diagnostics::{self, LogLevel},
    fsutil,
    hashing::Hasher,
    object::ObjectRecord,
    storage::{
        Catalog, ObjectListQuery,
        admin::{Admin, AdminError, BucketName, Generation, ObjectName, Origin},
        change_journal::{ChangeKind, JournalFence},
        sync_state::{
            root::{RootError, RootLease},
            writeback::{self, Limits, LiveExport, LiveResult, Stop, Writeback},
        },
    },
};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::{Path, PathBuf},
};

#[cfg(test)]
struct ExportHook {
    phase: &'static str,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[cfg(test)]
tokio::task_local! { static EXPORT_HOOK: std::sync::Arc<ExportHook>; }
#[cfg(test)]
tokio::task_local! { static LIVE_EXPORTS: std::sync::Arc<std::sync::atomic::AtomicUsize>; }
#[cfg(test)]
async fn export_checkpoint(phase: &str) {
    if let Ok(hook) = EXPORT_HOOK.try_with(std::sync::Arc::clone)
        && hook.phase.split(',').any(|selected| selected == phase)
    {
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}

// Directory synchronization supports only Linux and macOS; elsewhere roots are rejected.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[path = "mirror_tests.rs"]
mod tests;

#[derive(Default, Debug, serde::Serialize)]
pub struct Reconciliation {
    pub ingested: u64,
    pub blocked_buckets: u64,
    pub exported: u64,
    pub identical: u64,
    pub conflicting: u64,
    pub external_only: u64,
    pub ignored: u64,
    pub unrepresentable: u64,
    pub baseline_complete: bool,
    pub deferred: u64,
}

pub struct Task {
    stop: tokio::sync::watch::Sender<bool>,
    done: tokio::sync::oneshot::Receiver<()>,
    rescan: tokio::sync::mpsc::Sender<tokio::sync::oneshot::Sender<io::Result<Reconciliation>>>,
}
impl Task {
    pub async fn rescan(&self) -> io::Result<Reconciliation> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.rescan.send(sender).await.map_err(io::Error::other)?;
        receiver.await.map_err(io::Error::other)?
    }
    pub fn stop(&self) {
        self.stop.send_replace(true);
    }
    pub async fn finish(self) -> io::Result<()> {
        self.stop();
        self.done.await.map_err(io::Error::other)
    }
}

pub fn start(catalog: Catalog, root: RootLease, config: &Config) -> io::Result<Task> {
    let config = config.clone();
    let runtime = tokio::runtime::Handle::current();
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let (done, completion) = tokio::sync::oneshot::channel();
    let (rescan, mut requests) = tokio::sync::mpsc::channel(1);
    std::thread::Builder::new().name("sync-mirror".into()).spawn(move || {
        runtime.block_on(async move {
            let result = run_live_config(&catalog, &root, &mut stopping, &config, &mut requests).await;
            drop(requests);
            match result {
                Ok(()) => {},
                Err(error) => diagnostics::emit(LogLevel::Error, "sync_stopped", serde_json::json!({
                    "reason": error.to_string(),
                    "remedy": "Preserve the working tree and workspace. Correct unsafe names or permissions; for unresolved write-back recovery, back up the tree and select a fresh unassociated sync_dir."
                })),
            }
            // Idle ownership must also release when the server closes after a listener error.
            let mut fence = catalog.read_changes(None, None, 1).await.ok().map(|p| p.fence);
            while !*stopping.borrow() {
                let Some(current) = fence else { break };
                tokio::select! {
                    _ = stopping.changed() => break,
                    observed = catalog.wait_for_changes(current) => match observed {
                        Ok(observed) if !observed.rescan_required => fence = Some(observed.fence),
                        _ => break,
                    }
                }
            }
            drop(root);
        });
        let _ = done.send(());
    })?;
    Ok(Task {
        stop,
        done: completion,
        rescan,
    })
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
async fn run_live(
    catalog: &Catalog,
    root: &RootLease,
    stopping: &mut tokio::sync::watch::Receiver<bool>,
) -> io::Result<()> {
    let (_sender, mut receiver) = tokio::sync::mpsc::channel(1);
    run_live_config(catalog, root, stopping, &Config::default(), &mut receiver).await
}

async fn run_live_config(
    catalog: &Catalog,
    root: &RootLease,
    stopping: &mut tokio::sync::watch::Receiver<bool>,
    config: &Config,
    requests: &mut tokio::sync::mpsc::Receiver<
        tokio::sync::oneshot::Sender<io::Result<Reconciliation>>,
    >,
) -> io::Result<()> {
    catalog
        .set_change_journal_enabled(true)
        .await
        .map_err(io::Error::other)?;
    let mut cursor = catalog
        .read_changes(None, None, 1)
        .await
        .map_err(io::Error::other)?
        .fence;
    let ingested = super::ingest::rescan(catalog, root, config, stopping).await?;
    diagnostics::emit(LogLevel::Info, "sync_ingested", &ingested);
    let mut ingest_complete =
        ingested.deferred == 0 && ingested.unrepresentable == 0 && ingested.conflicting == 0;
    let mut summary = reconcile_until(catalog, root, stopping, ingest_complete).await?;
    summary.ingested = ingested.ingested;
    let mut baseline_complete = summary.baseline_complete;
    diagnostics::emit(LogLevel::Info, "sync_reconciled", &summary);
    let writeback = root
        .writeback(
            Limits::new(super::CONFLICT_MAX_BYTES, super::CONFLICT_MAX_ENTRIES)
                .map_err(io::Error::other)?,
        )
        .await
        .map_err(io::Error::other)?;
    while !*stopping.borrow() {
        let page = catalog
            .read_changes(Some(cursor), None, 256)
            .await
            .map_err(io::Error::other)?;
        if page.rescan_required {
            cursor = page.fence;
            let summary = reconcile_until(catalog, root, stopping, ingest_complete).await?;
            diagnostics::emit(LogLevel::Info, "sync_reconciled", &summary);
            continue;
        }
        if page.items.is_empty() {
            #[cfg(test)]
            export_checkpoint("idle").await;
            tokio::select! {
                _ = stopping.changed() => break,
                Some(request) = requests.recv() => {
                    let summary = async {
                        let mut summary = super::ingest::rescan(catalog, root, config, stopping).await?;
                        ingest_complete = summary.deferred == 0 && summary.unrepresentable == 0 && summary.conflicting == 0;
                        if !baseline_complete && ingest_complete && !*stopping.borrow() {
                            let merged = reconcile_until(catalog, root, stopping, true).await?;
                            baseline_complete = merged.baseline_complete;
                        }
                        summary.baseline_complete = baseline_complete;
                        Ok::<_, io::Error>(summary)
                    }.await;
                    if let Ok(summary) = &summary {
                        diagnostics::emit(LogLevel::Info, "sync_ingested", summary);
                    }
                    let _ = request.send(summary);
                }
                result = catalog.wait_for_changes(cursor) => { result.map_err(io::Error::other)?; }
            }
            continue;
        }
        let mut summary = Reconciliation::default();
        // A page bounds the work set; each selected key reads current live state once.
        let mut keys = std::collections::BTreeSet::new();
        for change in page.items {
            if change.origin == Origin::SyncIngest {
                continue;
            }
            if matches!(
                change.kind,
                ChangeKind::Publish
                    | ChangeKind::Delete
                    | ChangeKind::Metadata
                    | ChangeKind::BucketCreate
            ) {
                keys.insert((change.bucket, change.object));
            }
        }
        for (bucket, name) in keys {
            if *stopping.borrow() {
                return Ok(());
            }
            let Ok(bucket) = BucketName::new(bucket) else {
                continue;
            };
            if let Some(name) = name {
                reconcile_live_object(
                    catalog,
                    root,
                    &writeback,
                    &bucket,
                    &ObjectName::new(name).map_err(io::Error::other)?,
                    page.fence,
                    &mut summary,
                )
                .await?;
            } else {
                if !root
                    .remember_sync_bucket(&bucket)
                    .await
                    .map_err(io::Error::other)?
                {
                    summary.deferred += 1;
                    continue;
                }
                let directory = PathBuf::from(bucket.as_str());
                create_directory(root, directory).await?;
            }
        }
        cursor = page.next;
        diagnostics::emit(LogLevel::Info, "sync_exported", &summary);
    }
    Ok(())
}

async fn reconcile_live_object(
    catalog: &Catalog,
    root: &RootLease,
    writeback: &Writeback,
    bucket: &BucketName,
    name: &ObjectName,
    fence: JournalFence,
    summary: &mut Reconciliation,
) -> io::Result<()> {
    let admin = catalog.admin(Origin::SyncIngest);
    #[cfg(test)]
    export_checkpoint("before-binding").await;
    let binding = match catalog.sync_state().binding(bucket).await {
        Ok(binding) => binding,
        Err(AdminError::NotFound) => return Ok(()),
        Err(error) => return Err(io::Error::other(error)),
    };
    #[cfg(test)]
    export_checkpoint("after-binding").await;
    let record = match admin.get_object(bucket, name, None).await {
        Ok(record) => Some(record),
        Err(AdminError::NotFound) => None,
        Err(error) => return Err(io::Error::other(error)),
    };
    let path = match mapping::name_to_path(bucket, name) {
        PathMapping::File(path) => path,
        PathMapping::Unrepresentable(reason) => {
            if let Some(record) = record {
                finding(catalog, root, bucket, name, &record, reason, summary).await?;
            }
            return Ok(());
        }
    };
    let tracked = match catalog
        .sync_state()
        .get_path(
            binding.clone(),
            path.to_str()
                .ok_or_else(|| io::Error::other("invalid export path"))?
                .to_owned(),
        )
        .await
    {
        Ok(tracked) => tracked,
        Err(AdminError::Precondition | AdminError::NotFound) => {
            summary.deferred += 1;
            return Ok(());
        }
        Err(error) => return Err(io::Error::other(error)),
    };
    if record.is_none() && tracked.is_none() {
        return Ok(());
    }
    let mut identical = false;
    if let Some(record) = &record {
        let collision = match hierarchy_collision(&admin, bucket, name).await {
            Ok(collision) => collision,
            Err(AdminError::Precondition | AdminError::NotFound) => {
                summary.deferred += 1;
                return Ok(());
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        if collision {
            finding(
                catalog,
                root,
                bucket,
                name,
                record,
                Reason::Collision,
                summary,
            )
            .await?;
            return Ok(());
        }
        let selected = path.clone();
        let observed = record.clone();
        match file_job_retry(root, move |root| inspect(root, &selected, &observed)).await? {
            Target::Finding(reason) => {
                finding(catalog, root, bucket, name, record, reason, summary).await?;
                return Ok(());
            }
            Target::Identical => {
                identical = true;
            }
            Target::Conflicting if tracked.is_none() => {
                finding(
                    catalog,
                    root,
                    bucket,
                    name,
                    record,
                    Reason::Collision,
                    summary,
                )
                .await?;
                summary.conflicting += 1;
                return Ok(());
            }
            _ => {}
        }
    }
    let (generation, source) = if let Some(record) = &record {
        let generation = Generation::new(record.generation).map_err(io::Error::other)?;
        match admin.open_object(bucket, name, Some(generation)).await {
            Ok((_, reader)) => (
                generation,
                if identical {
                    None
                } else {
                    Some(reader.into_file().await)
                },
            ),
            Err(AdminError::NotFound | AdminError::Precondition) => {
                summary.deferred += 1;
                return Ok(());
            }
            Err(error) => return Err(io::Error::other(error)),
        }
    } else {
        (
            tracked
                .as_ref()
                .ok_or_else(|| io::Error::other("missing tracked deletion"))?
                .generation,
            None,
        )
    };
    #[cfg(test)]
    export_checkpoint("after-get-path").await;
    let mut seconds = 1;
    let operation = loop {
        match writeback
            .begin_live_export(
                binding.clone(),
                name.clone(),
                generation,
                tracked.clone(),
                fence,
                source.as_ref().map(File::try_clone).transpose()?,
            )
            .await
        {
            Ok(operation) => break operation,
            Err(error) if concurrent_change(&error) => {
                summary.deferred += 1;
                return Ok(());
            }
            Err(error) if retryable(&error) => wait_retry(&error, &mut seconds).await,
            Err(error) => return Err(io::Error::other(error)),
        }
    };
    #[cfg(test)]
    export_checkpoint("after-prepare").await;
    loop {
        match advance_with_retry(writeback, operation.clone()).await? {
            LiveResult::Pending => {}
            LiveResult::Complete => {
                if identical {
                    summary.identical += 1;
                } else {
                    #[cfg(test)]
                    let _ = LIVE_EXPORTS
                        .try_with(|count| count.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
                    summary.exported += 1;
                }
                break;
            }
            LiveResult::Obsolete => {
                summary.deferred += 1;
                break;
            }
            LiveResult::Conflict => {
                summary.conflicting += 1;
                root.record_export_conflict(binding.clone(), name.clone(), generation)
                    .await
                    .map_err(io::Error::other)?;
                diagnostics::emit(
                    LogLevel::Warn,
                    "sync_external_conflict",
                    serde_json::json!({"bucket":bucket.as_str(),"object":name.as_str(),"remedy":"Preserve or back up the edited file, then restore its previously exported contents or select a fresh sync_dir; automatic conflict transfer is pending"}),
                );
                break;
            }
        }
    }
    Ok(())
}

fn retryable(error: &writeback::Error) -> bool {
    let error = match error {
        writeback::Error::Admin(error) | writeback::Error::Root(RootError::Admin(error)) => error,
        _ => return false,
    };
    matches!(
        error,
        AdminError::DiskFull
            | AdminError::Io(
                std::io::ErrorKind::StorageFull
                    | std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::Other
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::WouldBlock
            )
    )
}

async fn wait_retry(error: &writeback::Error, seconds: &mut u64) {
    diagnostics::emit(
        LogLevel::Error,
        "sync_export_retry",
        serde_json::json!({
            "reason":error.to_string(),"retry_seconds":*seconds,
            "remedy":"Free space on the sync_dir and data_dir volumes (for example, remove unrelated temporary files), restore write permission, and keep this server running; the pending object will retry automatically"
        }),
    );
    tokio::time::sleep(std::time::Duration::from_secs(*seconds)).await;
    *seconds = (*seconds * 2).min(30);
}

async fn file_job_retry<T: Send + 'static>(
    root: &RootLease,
    job: impl Fn(&File) -> Result<T, AdminError> + Clone + Send + 'static,
) -> io::Result<T> {
    let mut seconds = 1;
    loop {
        match root.file_job(job.clone()).await {
            Ok(result) => return Ok(result),
            Err(error) => {
                let error = writeback::Error::Root(error);
                if !retryable(&error) {
                    return Err(io::Error::other(error));
                }
                wait_retry(&error, &mut seconds).await;
            }
        }
    }
}

async fn create_directory(root: &RootLease, directory: PathBuf) -> io::Result<bool> {
    file_job_retry(root, move |root| match parent(root, &directory, true) {
        Ok(_) => Ok(true),
        Err(ParentError::Finding(_)) => Ok(false),
        Err(ParentError::Io(error)) => Err(error.into()),
    })
    .await
}

async fn advance_with_retry(
    writeback: &Writeback,
    operation: LiveExport,
) -> io::Result<LiveResult> {
    let mut seconds = 1;
    loop {
        match writeback.advance_live_export(operation.clone()).await {
            Ok(result) => return Ok(result),
            Err(error) if retryable(&error) => wait_retry(&error, &mut seconds).await,
            Err(error) => return Err(io::Error::other(error)),
        }
    }
}

pub(super) fn fs_error(error: fsutil::FsError) -> AdminError {
    match error {
        fsutil::FsError::Io(error) => error.into(),
        fsutil::FsError::Unsupported => AdminError::Io(io::ErrorKind::Unsupported),
    }
}

fn same_inode(left: &File, right: &File) -> Result<bool, AdminError> {
    let a = fsutil::stat_identity(left).map_err(fs_error)?;
    let b = fsutil::stat_identity(right).map_err(fs_error)?;
    Ok(a.device == b.device && a.inode == b.inode)
}

fn exact_entry(directory: &File, name: &OsStr) -> io::Result<bool> {
    let mut after = None;
    loop {
        let page = fsutil::directory_names_after(directory, after.as_deref(), 256)?;
        if page.iter().any(|entry| entry == name) {
            return Ok(true);
        }
        if page.len() < 256 {
            return Ok(false);
        }
        after = page.last().cloned();
    }
}

pub(super) enum ParentError {
    Finding(Reason),
    Io(io::Error),
}
impl From<io::Error> for ParentError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(super) fn parent(root: &File, path: &Path, create: bool) -> Result<File, ParentError> {
    let mut directory = root.try_clone()?;
    let mut depth = 0;
    for component in path.iter() {
        depth += 1;
        if depth > 128 || component.as_encoded_bytes().len() > 255 {
            return Err(ParentError::Finding(Reason::HostRestriction));
        }
        let exact = exact_entry(&directory, component)?;
        match fsutil::directory_at(&directory, component, false) {
            Ok(next) => {
                if !exact {
                    return Err(ParentError::Finding(Reason::Collision));
                }
                directory = next;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                let next = fsutil::directory_at(&directory, component, true)?;
                next.sync_all()?;
                directory.sync_all()?;
                directory = next;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ParentError::Finding(Reason::HostRestriction));
            }
            Err(error) if !structural_entry_error(&error) => return Err(error.into()),
            Err(_) if fsutil::open_regular_at(&directory, component).is_ok() => {
                return Err(ParentError::Finding(Reason::Collision));
            }
            Err(_) => return Err(ParentError::Finding(Reason::UnsafeEntry)),
        }
    }
    directory.sync_all()?;
    root.sync_all()?;
    Ok(directory)
}

enum Target {
    Absent,
    Identical,
    Conflicting,
    Finding(Reason),
}

pub(super) fn structural_entry_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotADirectory | io::ErrorKind::IsADirectory | io::ErrorKind::InvalidInput
    ) || matches!(
        error.raw_os_error(),
        Some(libc::ELOOP | libc::ENXIO | libc::ENODEV)
    )
}

fn inspect(root: &File, path: &Path, record: &ObjectRecord) -> Result<Target, AdminError> {
    let Some(name) = path.file_name() else {
        return Ok(Target::Finding(Reason::InvalidName));
    };
    if name.as_encoded_bytes().len() > 255 {
        return Ok(Target::Finding(Reason::HostRestriction));
    }
    let directory = match parent(
        root,
        path.parent()
            .ok_or_else(|| AdminError::Invalid("missing parent".into()))?,
        true,
    ) {
        Ok(directory) => directory,
        Err(ParentError::Finding(reason)) => return Ok(Target::Finding(reason)),
        Err(ParentError::Io(error)) => return Err(error.into()),
    };
    let exact = exact_entry(&directory, name)?;
    let file = match fsutil::open_regular_at(&directory, name) {
        Ok(file) if exact => file,
        Ok(_) => return Ok(Target::Finding(Reason::Collision)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Target::Absent),
        Err(error) if !structural_entry_error(&error) => return Err(error.into()),
        Err(_) if fsutil::directory_at(&directory, name, false).is_ok() => {
            return Ok(Target::Finding(Reason::Collision));
        }
        Err(_) => return Ok(Target::Finding(Reason::UnsafeEntry)),
    };
    let before = fsutil::stat_identity(&file).map_err(fs_error)?;
    if before.size != record.size {
        return Ok(Target::Conflicting);
    }
    let mut hasher = Hasher::default();
    let mut buffer = [0; 64 * 1024];
    let mut offset = 0;
    while offset < record.size {
        let count = fsutil::read_at(
            &file,
            &mut buffer[..(record.size - offset).min(65_536) as usize],
            offset,
        )?;
        if count == 0 {
            return Ok(Target::Conflicting);
        }
        hasher.update(&buffer[..count]);
        offset += count as u64;
    }
    let after = fsutil::stat_identity(&file).map_err(fs_error)?;
    let reopened = fsutil::open_regular_at(&directory, name)?;
    let current_parent = parent(root, path.parent().ok_or(AdminError::Precondition)?, false)
        .map_err(|error| match error {
            ParentError::Finding(_) => AdminError::Precondition,
            ParentError::Io(error) => error.into(),
        })?;
    if before != after
        || !same_inode(&file, &reopened)?
        || !same_inode(&directory, &current_parent)?
    {
        return Err(AdminError::Precondition);
    }
    let hashes = hasher.finish();
    Ok(
        if hashes.md5 == record.md5 && hashes.crc32c == record.crc32c {
            Target::Identical
        } else {
            Target::Conflicting
        },
    )
}

pub(super) async fn hierarchy_collision(
    admin: &Admin,
    bucket: &BucketName,
    name: &ObjectName,
) -> Result<bool, AdminError> {
    let mut prefix = String::new();
    for part in name
        .as_str()
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent)
        .split('/')
        .filter(|part| !part.is_empty())
    {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        match admin
            .get_object(bucket, &ObjectName::new(prefix.clone())?, None)
            .await
        {
            Ok(_) => return Ok(true),
            Err(AdminError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    let page = admin
        .list_objects(
            bucket,
            ObjectListQuery {
                prefix: format!("{}/", name.as_str()),
                ..Default::default()
            },
            1,
        )
        .await?;
    Ok(!page.items.is_empty())
}

async fn finding(
    catalog: &Catalog,
    root: &RootLease,
    bucket: &BucketName,
    name: &ObjectName,
    record: &ObjectRecord,
    reason: Reason,
    summary: &mut Reconciliation,
) -> io::Result<()> {
    let binding = catalog
        .sync_state()
        .binding(bucket)
        .await
        .map_err(io::Error::other)?;
    root.record_finding(
        binding,
        name.clone(),
        Generation::new(record.generation).map_err(io::Error::other)?,
        reason,
    )
    .await
    .map_err(io::Error::other)?;
    if reason == Reason::Ignored {
        summary.ignored += 1;
    } else {
        summary.unrepresentable += 1;
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
async fn export(
    admin: &Admin,
    writeback: &Writeback,
    bucket: &BucketName,
    name: &ObjectName,
    path: Option<PathBuf>,
    record: &ObjectRecord,
    fence: crate::storage::change_journal::JournalFence,
) -> io::Result<bool> {
    let generation = Generation::new(record.generation).map_err(io::Error::other)?;
    #[cfg(test)]
    export_checkpoint("before-open").await;
    let (_, reader) = match admin.open_object(bucket, name, Some(generation)).await {
        Ok(opened) => opened,
        Err(AdminError::NotFound | AdminError::Precondition) => return Ok(false),
        Err(error) => return Err(io::Error::other(error)),
    };
    let source = reader.into_file().await;
    let (pending, proof) = match writeback
        .prepare_publish_copy(
            bucket,
            name,
            generation,
            fence,
            path.is_some().then_some(source),
        )
        .await
    {
        Ok(prepared) => prepared,
        Err(error) if concurrent_change(&error) => return Ok(false),
        Err(error) => return Err(io::Error::other(error)),
    };
    #[cfg(test)]
    export_checkpoint("before-arm").await;
    let attempt = async {
        let armed = writeback.arm(pending.token).await?;
        #[cfg(test)]
        export_checkpoint("before-publish").await;
        if let Some(path) = path {
            let entry = armed.workspace_entry.name().to_owned();
            writeback
                .file_job(armed.token.clone(), move |root, workspace| {
                    let relative = path.parent().ok_or(AdminError::Precondition)?;
                    let directory =
                        parent(root, relative, false).map_err(|_| AdminError::Precondition)?;
                    let target = path.file_name().ok_or(AdminError::Precondition)?;
                    let result = fsutil::durable_replace_between(
                        workspace,
                        entry.as_ref(),
                        &directory,
                        target,
                        None,
                    )
                    .map_err(fs_error)?;
                    if !matches!(result, fsutil::ReplaceOutcome::Created) {
                        return Err(AdminError::Precondition);
                    }
                    let current =
                        parent(root, relative, false).map_err(|_| AdminError::Precondition)?;
                    if !same_inode(&directory, &current)? {
                        return Err(AdminError::Precondition);
                    }
                    Ok(())
                })
                .await?;
        }
        #[cfg(test)]
        export_checkpoint("before-durable").await;
        let receipt = writeback.durable(armed.token).await?;
        writeback.acknowledge(receipt, None).await?;
        Ok::<_, writeback::Error>(())
    }
    .await;
    let completed = match attempt {
        Ok(()) => true,
        Err(error) if concurrent_change(&error) => {
            match writeback.abandon_unpublished(proof.clone()).await {
                Ok(()) => {}
                Err(writeback::Error::Protected) => writeback
                    .finish_obsolete_publish(proof)
                    .await
                    .map_err(io::Error::other)?,
                Err(error) => return Err(io::Error::other(error)),
            }
            false
        }
        Err(error) => return Err(io::Error::other(error)),
    };
    writeback.resume().await.map_err(io::Error::other)?;
    Ok(completed)
}

fn concurrent_change(error: &writeback::Error) -> bool {
    matches!(
        error,
        writeback::Error::Admin(
            AdminError::Precondition
                | AdminError::NotFound
                | AdminError::Io(io::ErrorKind::AlreadyExists)
        )
    )
}

pub async fn reconcile(
    catalog: &Catalog,
    root: &RootLease,
    _config: &Config,
) -> io::Result<Reconciliation> {
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    reconcile_until(catalog, root, &stopping, true).await
}

async fn reconcile_until(
    catalog: &Catalog,
    root: &RootLease,
    stopping: &tokio::sync::watch::Receiver<bool>,
    allow_baseline: bool,
) -> io::Result<Reconciliation> {
    catalog
        .set_change_journal_enabled(true)
        .await
        .map_err(io::Error::other)?;
    let fence = catalog
        .read_changes(None, None, 1)
        .await
        .map_err(io::Error::other)?
        .fence;
    let writeback = root
        .writeback(
            Limits::new(super::CONFLICT_MAX_BYTES, super::CONFLICT_MAX_ENTRIES)
                .map_err(io::Error::other)?,
        )
        .await
        .map_err(io::Error::other)?;
    if writeback.status().await.map_err(io::Error::other)?.stop != Stop::Ready {
        return Err(io::Error::other(
            "Unfinished write-back requires recovery; preserve workspace entries and select a fresh root after backup until recovery controls are available",
        ));
    }
    let admin = catalog.admin(Origin::SyncIngest);
    let mut summary = Reconciliation::default();
    let mut after = None;
    loop {
        if *stopping.borrow() {
            return Ok(summary);
        }
        let page = admin
            .list_all_buckets(after.as_deref(), 256)
            .await
            .map_err(io::Error::other)?;
        let next = page.items.last().map(|entry| entry.bucket.name.clone());
        for entry in page.items {
            let bucket = match BucketName::new(entry.bucket.name.clone()) {
                Ok(bucket) => bucket,
                Err(_) => {
                    summary.unrepresentable += 1;
                    continue;
                }
            };
            let directory = PathBuf::from(bucket.as_str());
            if !root
                .remember_sync_bucket(&bucket)
                .await
                .map_err(io::Error::other)?
            {
                summary.deferred += 1;
                continue;
            }
            if !create_directory(root, directory).await? {
                summary.unrepresentable += 1;
            }
            let mut query = ObjectListQuery::default();
            loop {
                let objects = admin
                    .list_objects(&bucket, query.clone(), 256)
                    .await
                    .map_err(io::Error::other)?;
                for record in objects.items {
                    if *stopping.borrow() {
                        return Ok(summary);
                    }
                    let name = ObjectName::new(record.name.clone()).map_err(io::Error::other)?;
                    if let PathMapping::File(path) = mapping::name_to_path(&bucket, &name) {
                        let binding = catalog
                            .sync_state()
                            .binding(&bucket)
                            .await
                            .map_err(io::Error::other)?;
                        if catalog
                            .sync_state()
                            .get_path(binding, path.to_string_lossy().into_owned())
                            .await
                            .map_err(io::Error::other)?
                            .is_some()
                        {
                            reconcile_live_object(
                                catalog,
                                root,
                                &writeback,
                                &bucket,
                                &name,
                                fence,
                                &mut summary,
                            )
                            .await?;
                            continue;
                        }
                    }
                    let path = match mapping::name_to_path(&bucket, &name) {
                        PathMapping::Unrepresentable(reason) => {
                            finding(catalog, root, &bucket, &name, &record, reason, &mut summary)
                                .await?;
                            continue;
                        }
                        PathMapping::File(path) => path,
                    };
                    if hierarchy_collision(&admin, &bucket, &name)
                        .await
                        .map_err(io::Error::other)?
                    {
                        finding(
                            catalog,
                            root,
                            &bucket,
                            &name,
                            &record,
                            Reason::Collision,
                            &mut summary,
                        )
                        .await?;
                        continue;
                    }
                    let selected = path.clone();
                    let observed = record.clone();
                    let target =
                        file_job_retry(root, move |root| inspect(root, &selected, &observed))
                            .await?;
                    if *stopping.borrow() {
                        return Ok(summary);
                    }
                    match target {
                        Target::Finding(reason) => {
                            finding(catalog, root, &bucket, &name, &record, reason, &mut summary)
                                .await?
                        }
                        Target::Conflicting => summary.conflicting += 1,
                        Target::Identical | Target::Absent => {
                            reconcile_live_object(
                                catalog,
                                root,
                                &writeback,
                                &bucket,
                                &name,
                                fence,
                                &mut summary,
                            )
                            .await?;
                        }
                    }
                }
                match objects.next {
                    Some(next) => query.cursor = Some(next),
                    None => break,
                }
            }
            let binding = catalog
                .sync_state()
                .binding(&bucket)
                .await
                .map_err(io::Error::other)?;
            let mut after_path = String::new();
            loop {
                let tracked = catalog
                    .sync_state()
                    .page(binding.clone(), after_path.clone(), 256)
                    .await
                    .map_err(io::Error::other)?;
                if tracked.is_empty() {
                    break;
                }
                for record in tracked {
                    if *stopping.borrow() {
                        return Ok(summary);
                    }
                    after_path = record.path;
                    let name = ObjectName::new(record.name).map_err(io::Error::other)?;
                    match admin.get_object(&bucket, &name, None).await {
                        Err(AdminError::NotFound) => {
                            reconcile_live_object(
                                catalog,
                                root,
                                &writeback,
                                &bucket,
                                &name,
                                fence,
                                &mut summary,
                            )
                            .await?
                        }
                        Ok(_) => {}
                        Err(error) => return Err(io::Error::other(error)),
                    }
                }
            }
        }
        if !page.has_more {
            break;
        }
        after = next;
    }
    let scan_admin = admin.clone();
    if *stopping.borrow() {
        return Ok(summary);
    }
    let runtime = tokio::runtime::Handle::current();
    let external = root
        .file_job(move |root| external_files(root, Path::new(""), &scan_admin, &runtime, 0))
        .await
        .map_err(io::Error::other)?;
    summary.external_only = external.0;
    summary.ignored += external.1;
    summary.unrepresentable += external.2;
    let end = catalog
        .read_changes(None, None, 1)
        .await
        .map_err(io::Error::other)?
        .fence;
    if allow_baseline
        && summary.deferred == 0
        && !*stopping.borrow()
        && summary.conflicting == 0
        && summary.external_only == 0
        && summary.unrepresentable == 0
        && end == fence
    {
        root.complete_baseline().await.map_err(io::Error::other)?;
        summary.baseline_complete = true;
    }
    Ok(summary)
}

fn external_files(
    directory: &File,
    relative: &Path,
    admin: &Admin,
    runtime: &tokio::runtime::Handle,
    depth: usize,
) -> Result<(u64, u64, u64), AdminError> {
    if depth > 128 {
        return Err(AdminError::Invalid(
            "sync tree exceeds 128 directory levels; reduce its depth".into(),
        ));
    }
    let mut counts = (0, 0, 0);
    let mut after: Option<OsString> = None;
    loop {
        let page = fsutil::directory_names_after(directory, after.as_deref(), 256)?;
        for name in &page {
            if depth == 0 && matches!(name.to_str(), Some(mapping::MARKER | mapping::WORKSPACE)) {
                continue;
            }
            let path = relative.join(name);
            if let Ok(child) = fsutil::directory_at(directory, name, false) {
                let child_counts = external_files(&child, &path, admin, runtime, depth + 1)?;
                let checked = fsutil::directory_at(directory, name, false)?;
                if !same_inode(&child, &checked)? {
                    return Err(AdminError::Precondition);
                }
                counts.0 += child_counts.0;
                counts.1 += child_counts.1;
                counts.2 += child_counts.2;
            } else if fsutil::open_regular_at(directory, name).is_ok() {
                let known = match mapping::path_to_name(&path) {
                    Ok((bucket, object)) => {
                        match runtime.block_on(admin.get_object(&bucket, &object, None)) {
                            Ok(_) => true,
                            Err(AdminError::NotFound) => false,
                            Err(error) => return Err(error),
                        }
                    }
                    Err(_) => false,
                };
                if !known {
                    if name
                        .to_str()
                        .is_some_and(crate::source_tree::is_ignored_regular_file)
                    {
                        counts.1 += 1;
                    } else {
                        counts.0 += 1;
                    }
                }
            } else {
                counts.2 += 1;
            }
        }
        if page.len() < 256 {
            break;
        }
        after = page.last().cloned();
    }
    Ok(counts)
}
