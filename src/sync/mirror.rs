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
        sync_state::{
            root::RootLease,
            writeback::{self, Limits, Stop, Writeback},
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
async fn export_checkpoint(phase: &str) {
    if let Ok(hook) = EXPORT_HOOK.try_with(std::sync::Arc::clone)
        && hook.phase == phase
    {
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}

#[cfg(test)]
#[path = "mirror_tests.rs"]
mod tests;

#[derive(Default, Debug, serde::Serialize)]
pub struct Reconciliation {
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
}
impl Task {
    pub fn stop(&self) {
        self.stop.send_replace(true);
    }
    pub async fn finish(self) -> io::Result<()> {
        self.stop();
        self.done.await.map_err(io::Error::other)
    }
}

pub fn start(catalog: Catalog, root: RootLease, _config: &Config) -> io::Result<Task> {
    let runtime = tokio::runtime::Handle::current();
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let (done, completion) = tokio::sync::oneshot::channel();
    std::thread::Builder::new().name("sync-mirror".into()).spawn(move || {
        runtime.block_on(async move {
            let result = reconcile_until(&catalog, &root, &stopping).await;
            match result {
                Ok(summary) => diagnostics::emit(LogLevel::Info, "sync_reconciled", &summary),
                Err(error) => diagnostics::emit(LogLevel::Error, "sync_stopped", serde_json::json!({
                    "reason": error.to_string(),
                    "remedy": "Preserve the working tree and workspace. Correct unsafe names or permissions; for unresolved write-back recovery, back up the tree and select a fresh unassociated sync_dir. Journal replay, conflict controls and retries are pending."
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
    })
}

fn fs_error(error: fsutil::FsError) -> AdminError {
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

fn parent(root: &File, path: &Path, create: bool) -> Result<File, Reason> {
    let mut directory = root.try_clone().map_err(|_| Reason::UnsafeEntry)?;
    let mut depth = 0;
    for component in path.iter() {
        depth += 1;
        if depth > 128 || component.as_encoded_bytes().len() > 255 {
            return Err(Reason::HostRestriction);
        }
        let exact = exact_entry(&directory, component).map_err(|_| Reason::UnsafeEntry)?;
        match fsutil::directory_at(&directory, component, false) {
            Ok(next) => {
                if !exact {
                    return Err(Reason::Collision);
                }
                directory = next;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                let next = fsutil::directory_at(&directory, component, true)
                    .map_err(|_| Reason::UnsafeEntry)?;
                next.sync_all().map_err(|_| Reason::UnsafeEntry)?;
                directory.sync_all().map_err(|_| Reason::UnsafeEntry)?;
                directory = next;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(Reason::HostRestriction);
            }
            Err(_) if fsutil::open_regular_at(&directory, component).is_ok() => {
                return Err(Reason::Collision);
            }
            Err(_) => return Err(Reason::UnsafeEntry),
        }
    }
    directory.sync_all().map_err(|_| Reason::UnsafeEntry)?;
    root.sync_all().map_err(|_| Reason::UnsafeEntry)?;
    Ok(directory)
}

enum Target {
    Absent,
    Identical,
    Conflicting,
    Finding(Reason),
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
        Err(reason) => return Ok(Target::Finding(reason)),
    };
    let exact = exact_entry(&directory, name)?;
    let file = match fsutil::open_regular_at(&directory, name) {
        Ok(file) if exact => file,
        Ok(_) => return Ok(Target::Finding(Reason::Collision)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Target::Absent),
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
        .map_err(|_| AdminError::Precondition)?;
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

async fn hierarchy_collision(
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
    reconcile_until(catalog, root, &stopping).await
}

async fn reconcile_until(
    catalog: &Catalog,
    root: &RootLease,
    stopping: &tokio::sync::watch::Receiver<bool>,
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
            let bucket_directory = root
                .file_job(move |root| Ok(parent(root, &directory, true).map(|_| ())))
                .await
                .map_err(io::Error::other)?;
            if bucket_directory.is_err() {
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
                    let target = root
                        .file_job(move |root| inspect(root, &selected, &observed))
                        .await
                        .map_err(io::Error::other)?;
                    if *stopping.borrow() {
                        return Ok(summary);
                    }
                    match target {
                        Target::Finding(reason) => {
                            finding(catalog, root, &bucket, &name, &record, reason, &mut summary)
                                .await?
                        }
                        Target::Conflicting => summary.conflicting += 1,
                        Target::Identical => {
                            if export(&admin, &writeback, &bucket, &name, None, &record, fence)
                                .await?
                            {
                                summary.identical += 1;
                            } else {
                                summary.deferred += 1;
                            }
                        }
                        Target::Absent => {
                            if export(
                                &admin,
                                &writeback,
                                &bucket,
                                &name,
                                Some(path),
                                &record,
                                fence,
                            )
                            .await?
                            {
                                summary.exported += 1;
                            } else {
                                summary.deferred += 1;
                            }
                        }
                    }
                }
                match objects.next {
                    Some(next) => query.cursor = Some(next),
                    None => break,
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
    if summary.deferred == 0
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
