use super::control::Command;
use super::mapping::{self, PathMapping, Reason};
use crate::{
    bucket::CatalogError,
    config::Config,
    diagnostics::{self, LogLevel},
    fsutil,
    hashing::Hasher,
    object::ObjectRecord,
    storage::{
        Catalog, ObjectListQuery,
        admin::{Admin, AdminError, BucketName, Generation, ObjectName, Origin},
        change_journal::{ChangeKind, ChangePage, JournalFence},
        sync_state::{
            SyncBinding, SyncRecord,
            root::{RootError, RootLease},
            writeback::{self, Limits, LiveExport, LiveResult, LiveSource, Stop, Writeback},
        },
    },
};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    future::Future,
    io,
    path::{Path, PathBuf},
    time::Duration,
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
mod tests;

mod wait;

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
    // Holding the sender keeps the worker's rescan channel open for the task's lifetime.
    #[cfg_attr(
        not(all(test, any(target_os = "linux", target_os = "macos"))),
        expect(dead_code, reason = "only unit tests request a rescan")
    )]
    rescan: tokio::sync::mpsc::Sender<Command>,
}

struct DirectoryWatch {
    watch: crate::source_tree::watch::Watch,
    // Vnode descriptors must close before the lease can release exclusive root ownership.
    _root: RootLease,
}

impl DirectoryWatch {
    async fn new(root: &RootLease) -> io::Result<Self> {
        let lease = root.clone();
        let native = root
            .file_job(|root| crate::source_tree::watch::build(root).map_err(AdminError::backend))
            .await
            .map_err(io::Error::other)?;
        Ok(Self {
            watch: crate::source_tree::watch::Watch::new(native)?,
            _root: lease,
        })
    }

    async fn rebuild(
        &mut self,
        root: &RootLease,
        stopping: &tokio::sync::watch::Receiver<bool>,
        health: &crate::task::TaskHealth,
    ) -> io::Result<()> {
        let mut delay = RETRY_DELAY_MIN;
        let mut failures = 0;
        let mut stopping = stopping.clone();
        loop {
            if *stopping.borrow() {
                return Ok(());
            }
            match Self::new(root).await {
                Ok(watcher) => {
                    *self = watcher;
                    health.pass_succeeded();
                    return Ok(());
                }
                Err(error) if watch_retryable(&error) => {
                    failures += 1;
                    health.pass_failed();
                    #[cfg(test)]
                    export_checkpoint("retry").await;
                    if failures >= 3 {
                        return Err(error);
                    }
                    diagnostics::emit(
                        LogLevel::Error,
                        "sync_watch_retry",
                        serde_json::json!({
                            "reason":error.to_string(),"delay_ms":delay.as_millis(),
                            "remedy":"Restore read/search permissions on the sync tree; observation remains unhealthy until every subtree is watched"
                        }),
                    );
                    tokio::select! {
                        _ = stopping.changed() => return Ok(()),
                        () = tokio::time::sleep(delay) => {},
                    }
                    delay = delay.saturating_mul(2).min(RETRY_DELAY_MAX);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn watch_retryable(error: &io::Error) -> bool {
    if is_transient(error) {
        return true;
    }
    let mut next = Some(error as &(dyn std::error::Error + 'static));
    while let Some(cause) = next {
        if cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::PermissionDenied)
        {
            return true;
        }
        next = cause.source();
    }
    false
}
impl Task {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) async fn rescan(&self) -> io::Result<Reconciliation> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.rescan
            .send(Command::Rescan {
                reply: sender,
                identity: None,
            })
            .await
            .map_err(io::Error::other)?;
        receiver.await.map_err(io::Error::other)?
    }
    pub(crate) fn stop(&self) {
        self.stop.send_replace(true);
    }
    pub async fn finish(self) -> io::Result<()> {
        self.stop();
        self.done.await.map_err(io::Error::other)
    }
}

/// The first retry delay after a transient failure; it doubles after each consecutive one.
/// Tests shorten it so a retry does not dominate their run time.
const RETRY_DELAY_MIN: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(1)
};
/// The retry delay cap, also the run length after which a failure starts the backoff over.
const RETRY_DELAY_MAX: Duration = Duration::from_secs(30);

/// Whether a mirror failure may clear on its own: catalog contention, an admission timeout or
/// an interrupted or timed-out I/O call. Everything else, such as an invalid name, a permission
/// error or a corrupt catalog, needs an operator and stops the mirror.
fn is_transient(error: &io::Error) -> bool {
    fn transient_kind(kind: io::ErrorKind) -> bool {
        matches!(
            kind,
            io::ErrorKind::TimedOut
                | io::ErrorKind::WouldBlock
                | io::ErrorKind::Interrupted
                | io::ErrorKind::ResourceBusy
        )
    }
    if transient_kind(error.kind()) {
        return true;
    }
    // `io::Error::source` skips a wrapped error itself, so start the walk at the wrapped one.
    let mut next = error
        .get_ref()
        .map(|inner| inner as &(dyn std::error::Error + 'static));
    while let Some(cause) = next {
        let transient = if let Some(admin) = cause.downcast_ref::<AdminError>() {
            match admin {
                AdminError::Busy { .. } => true,
                AdminError::Io(kind) => transient_kind(*kind),
                _ => false,
            }
        } else if let Some(catalog) = cause.downcast_ref::<CatalogError>() {
            matches!(catalog, CatalogError::Unavailable(_))
        } else if let Some(io) = cause.downcast_ref::<io::Error>() {
            transient_kind(io.kind())
        } else if let Some(sqlite) = cause.downcast_ref::<rusqlite::Error>() {
            matches!(
                sqlite.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            )
        } else {
            false
        };
        if transient {
            return true;
        }
        next = cause.source();
    }
    false
}

pub fn start(catalog: Catalog, root: RootLease, config: &Config) -> io::Result<Task> {
    let config = config.clone();
    let runtime = tokio::runtime::Handle::current();
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let root = std::sync::Arc::new(root.with_shutdown(stopping.clone()));
    let (done, completion) = tokio::sync::oneshot::channel();
    let (rescan, mut requests) = tokio::sync::mpsc::channel(1);
    catalog
        .register_sync_controls(&root, rescan.clone())
        .map_err(io::Error::other)?;
    // A transient failure (catalog contention, a timeout) is retried with backoff and counts as
    // a failed pass, so readiness drops only once failures repeat and recovers with the next
    // successful pass. Persistent failures pause intake but retain ownership and controls;
    // only an explicit rescan after repair may restore synchronization readiness.
    let health = crate::task::TaskHealth::default();
    catalog.watch_background_task(health.clone());
    crate::task::spawn_thread("sync-mirror", move || {
        let mut delay = RETRY_DELAY_MIN;
        let mut resume = None;
        loop {
            let started = std::time::Instant::now();
            let result = crate::task::catch_job("sync-mirror", || {
                #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                tests::injected_live_failure(&config)?;
                runtime.block_on(run_live_config(
                    LiveRun {
                        catalog: &catalog,
                        root: &root,
                        config: &config,
                        health: &health,
                        resume: resume.take(),
                    },
                    &mut stopping,
                    &mut requests,
                ))
            });
            let error = match result {
                Ok(Ok(())) => break,
                Ok(Err(error)) => error,
                Err(error) => io::Error::other(error),
            };
            if !is_transient(&error) {
                health.mark_restarting();
                diagnostics::emit(
                    LogLevel::Error,
                    "sync_paused",
                    serde_json::json!({"reason":error.to_string(),"remedy":"Preserve pending data, correct the reported condition and request an explicit rescan"}),
                );
                if let Err(error) =
                    runtime.block_on(root.observation_pause(Some(pause_reason(&error).into())))
                {
                    diagnostics::emit(
                        LogLevel::Error,
                        "sync_pause_unrecorded",
                        serde_json::json!({"reason":error.to_string()}),
                    );
                }
                resume = runtime.block_on(await_resume(
                    &catalog,
                    &root,
                    &config,
                    &mut requests,
                    &mut stopping,
                ));
                if resume.is_none() {
                    break;
                }
                delay = RETRY_DELAY_MIN;
                continue;
            }
            health.pass_failed();
            // A run that lasted longer than the cap had recovered, so its failure starts over.
            if started.elapsed() >= RETRY_DELAY_MAX {
                delay = RETRY_DELAY_MIN;
            }
            diagnostics::emit(
                LogLevel::Warn,
                "sync_retrying",
                serde_json::json!({
                    "reason": error.to_string(),
                    "delay_ms": delay.as_millis(),
                }),
            );
            let stopped = runtime.block_on(async {
                tokio::select! {
                    _ = stopping.wait_for(|stop| *stop) => true,
                    () = tokio::time::sleep(delay) => false,
                }
            });
            if stopped {
                break;
            }
            delay = delay.saturating_mul(2).min(RETRY_DELAY_MAX);
        }
        drop(requests);
        if let Err(error) = catalog.clear_sync_controls() {
            health.mark_stopped();
            diagnostics::emit(
                LogLevel::Error,
                "sync_controls_closed",
                serde_json::json!({"reason":error.to_string()}),
            );
        }
        if health.is_down() {
            health.mark_stopped();
        }
        runtime.block_on(async {
            if !*stopping.borrow() {
                tokio::select! {
                    _=stopping.changed()=>{},
                    ()=catalog.wait_for_sync_shutdown()=>{},
                }
            }
        });
        drop(root);
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
    let health = crate::task::TaskHealth::default();
    let config = Config::default();
    run_live_config(
        LiveRun {
            catalog,
            root,
            config: &config,
            health: &health,
            resume: None,
        },
        stopping,
        &mut receiver,
    )
    .await
}

struct LiveRun<'a> {
    catalog: &'a Catalog,
    root: &'a RootLease,
    config: &'a Config,
    health: &'a crate::task::TaskHealth,
    resume: Option<Resume>,
}

struct Resume {
    reply: tokio::sync::oneshot::Sender<io::Result<Reconciliation>>,
    authority: crate::storage::sync_state::authority::ControlAuthority,
}

async fn await_resume(
    catalog: &Catalog,
    root: &RootLease,
    config: &Config,
    requests: &mut tokio::sync::mpsc::Receiver<Command>,
    stopping: &mut tokio::sync::watch::Receiver<bool>,
) -> Option<Resume> {
    let mut after = 0;
    loop {
        match root
            .recovery_page(
                after,
                128,
                crate::storage::sync_state::authority::ControlAuthority::Permissive,
            )
            .await
        {
            Ok(page) if page.is_empty() => break,
            Ok(page) => after = page.last().map_or(after, |item| item.ordinal),
            Err(error) => {
                diagnostics::emit(
                    LogLevel::Error,
                    "sync_recovery_observation_failed",
                    serde_json::json!({"reason":error.to_string(),"remedy":"Preserve the workspace and inspect the retained entry before attempting recovery"}),
                );
                break;
            }
        }
        if *stopping.borrow() {
            return None;
        }
    }
    while !*stopping.borrow() {
        tokio::select! {
            biased;
            _=stopping.changed()=>return None,
            request=requests.recv()=>match request? {
                Command::Rescan {reply,identity}=> {
                    match root.authorize_controls(config.seed_project.clone(),identity.clone()).await {
                        Ok(())=>return Some(Resume {reply,authority:crate::storage::sync_state::authority::ControlAuthority::new(config.seed_project.clone(),identity)}),
                        Err(error)=>{let _=reply.send(Err(io::Error::other(error)));}
                    }
                }
                Command::Approve {reply,..}=>{let _=reply.send(Err(io::Error::new(io::ErrorKind::WouldBlock,"Synchronization is paused; correct the condition and request a rescan")));}
                Command::Wait {reply,..}=>{let _=reply.send(Ok(super::control::WaitResult {completed:false,timed_out:false,journal_fence:None,reconciliation:None,reason:"Paused"}));}
                Command::Resolve {request,reply,identity}=> {
                    let authority=crate::storage::sync_state::authority::ControlAuthority::new(config.seed_project.clone(),identity);
                    let result=root.resolve_conflict(request,authority,config.max_object_bytes).await;
                    let _=reply.send(result);
                }
            },
            ()=catalog.wait_for_sync_shutdown()=>return None,
        }
    }
    None
}

async fn run_live_config(
    run: LiveRun<'_>,
    stopping: &mut tokio::sync::watch::Receiver<bool>,
    requests: &mut tokio::sync::mpsc::Receiver<Command>,
) -> io::Result<()> {
    let (catalog, root, config, health) = (run.catalog, run.root, run.config, run.health);
    let Initialized {
        mut cursor,
        mut watcher,
        mut ingest_complete,
        mut baseline_complete,
        writeback,
    } = initialize_live(run, stopping).await?;
    while !*stopping.borrow() {
        if let Ok(request) = requests.try_recv() {
            if matches!(&request, Command::Rescan { .. } | Command::Wait { .. }) {
                watcher.rebuild(root, stopping, health).await?;
            }
            if *stopping.borrow() {
                break;
            }
            LiveControl {
                catalog,
                root,
                config,
                stopping,
                ingest_complete: &mut ingest_complete,
                baseline_complete: &mut baseline_complete,
                cursor: &mut cursor,
            }
            .handle(request)
            .await?;
            continue;
        }
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
                biased;
                _ = stopping.changed() => break,
                observed = watcher.watch.changed() => {
                    observed?;
                    root.invalidate_deletions().await.map_err(io::Error::other)?;
                    // Register before scanning so edits during reconciliation cause another pass.
                    watcher.rebuild(root,stopping,health).await?;
                    if *stopping.borrow() { break }
                    let summary = ingest_external(catalog, root, config, stopping, &mut ingest_complete, &mut baseline_complete).await?;
                    diagnostics::emit(LogLevel::Info, "sync_ingested", &summary);
                    health.pass_succeeded();
                }
                Some(request) = requests.recv() => {
                    if matches!(&request,Command::Rescan {..}|Command::Wait {..}) {
                        watcher.rebuild(root,stopping,health).await?;
                    }
                    if *stopping.borrow() { break }
                    LiveControl {catalog,root,config,stopping,ingest_complete:&mut ingest_complete,baseline_complete:&mut baseline_complete,cursor:&mut cursor}.handle(request).await?;
                }
                result = catalog.wait_for_changes(cursor) => { result.map_err(io::Error::other)?; }
            }
            continue;
        }
        cursor = page.next;
        let summary = replay_page(catalog, root, &writeback, page, stopping).await?;
        diagnostics::emit(LogLevel::Info, "sync_exported", &summary);
        health.pass_succeeded();
    }
    Ok(())
}

async fn replay_page(
    catalog: &Catalog,
    root: &RootLease,
    writeback: &Writeback,
    page: ChangePage,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> io::Result<Reconciliation> {
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
            return Ok(summary);
        }
        let Ok(bucket) = BucketName::new(bucket) else {
            continue;
        };
        if let Some(name) = name {
            reconcile_live_object(
                catalog,
                root,
                writeback,
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
    Ok(summary)
}

struct Initialized {
    cursor: JournalFence,
    watcher: DirectoryWatch,
    ingest_complete: BaselineGate,
    baseline_complete: bool,
    writeback: Writeback,
}

fn pause_reason(error: &io::Error) -> &'static str {
    let mut next = Some(error as &(dyn std::error::Error + 'static));
    while let Some(cause) = next {
        if matches!(
            cause.downcast_ref::<crate::storage::sync_state::conflicts::Error>(),
            Some(crate::storage::sync_state::conflicts::Error::Capacity)
        ) || matches!(
            cause.downcast_ref::<writeback::Error>(),
            Some(writeback::Error::Capacity)
        ) {
            return "ConflictCapacity";
        }
        if matches!(
            cause.downcast_ref::<writeback::Error>(),
            Some(writeback::Error::RecoveryRequired | writeback::Error::Protected)
        ) {
            return "RecoveryRequired";
        }
        if matches!(
            cause.downcast_ref::<AdminError>(),
            Some(AdminError::DiskFull | AdminError::Io(_))
        ) {
            return "IoFailure";
        }
        next = cause
            .downcast_ref::<io::Error>()
            .and_then(|error| {
                error
                    .get_ref()
                    .map(|cause| cause as &(dyn std::error::Error + 'static))
            })
            .or_else(|| cause.source());
    }
    "observer-failure"
}

async fn initialize_live(
    run: LiveRun<'_>,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> io::Result<Initialized> {
    let LiveRun {
        catalog,
        root,
        config,
        health,
        resume,
    } = run;
    let initial_root = resume
        .as_ref()
        .map(|request| root.with_authority(request.authority.clone()));
    let operation_root = initial_root.as_ref().unwrap_or(root);
    catalog
        .enable_change_journal()
        .await
        .map_err(io::Error::other)?;
    let cursor = catalog
        .read_changes(None, None, 1)
        .await
        .map_err(io::Error::other)?
        .fence;
    let watcher = DirectoryWatch::new(root).await?;
    let mut writeback = recover_writeback(operation_root).await?;
    root.invalidate_deletions()
        .await
        .map_err(io::Error::other)?;
    let ingested = super::ingest::rescan(catalog, operation_root, config, stopping).await?;
    diagnostics::emit(LogLevel::Info, "sync_ingested", &ingested);
    let ingest_complete = BaselineGate::after_ingest(&ingested);
    let mut summary = reconcile_until(catalog, operation_root, stopping, ingest_complete).await?;
    summary.ingested = ingested.ingested;
    let baseline_complete = summary.baseline_complete;
    diagnostics::emit(LogLevel::Info, "sync_reconciled", &summary);
    observe_deletions(catalog, operation_root, config, &summary, stopping).await?;
    root.observation_pause(None)
        .await
        .map_err(io::Error::other)?;
    health.pass_succeeded();
    if let Some(request) = resume {
        let _ = request.reply.send(Ok(summary));
        writeback = root
            .writeback(
                Limits::new(super::CONFLICT_MAX_BYTES, super::CONFLICT_MAX_ENTRIES)
                    .map_err(io::Error::other)?,
            )
            .await
            .map_err(io::Error::other)?;
    }
    Ok(Initialized {
        cursor,
        watcher,
        ingest_complete,
        baseline_complete,
        writeback,
    })
}

async fn recover_writeback(root: &RootLease) -> io::Result<Writeback> {
    let writeback = root
        .writeback(
            Limits::new(super::CONFLICT_MAX_BYTES, super::CONFLICT_MAX_ENTRIES)
                .map_err(io::Error::other)?,
        )
        .await
        .map_err(io::Error::other)?;
    writeback
        .recover_pending()
        .await
        .map_err(io::Error::other)?;
    let recovered = writeback
        .preserve_protected()
        .await
        .map_err(io::Error::other)?;
    if recovered.stop != Stop::Ready {
        return Err(io::Error::other(match recovered.stop {
            Stop::ConflictCapacity => writeback::Error::Capacity,
            Stop::IoFailure => writeback::Error::Admin(AdminError::Io(io::ErrorKind::Other)),
            _ => writeback::Error::RecoveryRequired,
        }));
    }
    Ok(writeback)
}

async fn ingest_external(
    catalog: &Catalog,
    root: &RootLease,
    config: &Config,
    stopping: &tokio::sync::watch::Receiver<bool>,
    ingest_complete: &mut BaselineGate,
    baseline_complete: &mut bool,
) -> io::Result<Reconciliation> {
    root.file_job(|_| Ok(())).await.map_err(io::Error::other)?;
    let mut summary = super::ingest::rescan(catalog, root, config, stopping).await?;
    *ingest_complete = BaselineGate::after_ingest(&summary);
    if !*baseline_complete && *ingest_complete == BaselineGate::Allowed && !*stopping.borrow() {
        // Nested control rescans otherwise retain reconciliation state on the observer's stack.
        let merged = Box::pin(reconcile_until(
            catalog,
            root,
            stopping,
            BaselineGate::Allowed,
        ))
        .await?;
        *baseline_complete = merged.baseline_complete;
    }
    root.file_job(|_| Ok(())).await.map_err(io::Error::other)?;
    summary.baseline_complete = *baseline_complete;
    observe_deletions(catalog, root, config, &summary, stopping).await?;
    Ok(summary)
}

async fn observe_deletions(
    catalog: &Catalog,
    root: &RootLease,
    config: &Config,
    summary: &Reconciliation,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> io::Result<()> {
    let inventory = super::deletion::inventory(root, config, summary, stopping).await;
    match inventory {
        Ok(Some(set)) => {
            super::deletion::execute(catalog, root, set, stopping).await?;
        }
        Ok(None) => {}
        Err(error) if super::deletion::stale(&error) => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

struct LiveControl<'a> {
    catalog: &'a Catalog,
    root: &'a RootLease,
    config: &'a Config,
    stopping: &'a tokio::sync::watch::Receiver<bool>,
    ingest_complete: &'a mut BaselineGate,
    baseline_complete: &'a mut bool,
    cursor: &'a mut JournalFence,
}
impl LiveControl<'_> {
    async fn rescan(&mut self) -> io::Result<Reconciliation> {
        let _writeback = recover_writeback(self.root).await?;
        ingest_external(
            self.catalog,
            self.root,
            self.config,
            self.stopping,
            self.ingest_complete,
            self.baseline_complete,
        )
        .await
    }

    fn handle(&mut self, request: Command) -> impl Future<Output = io::Result<()>> + '_ {
        // The single worker boxes one control future to bound nested reconciliation stack use.
        Box::pin(async move {
            let identity = match &request {
                Command::Rescan { identity, .. }
                | Command::Approve { identity, .. }
                | Command::Wait { identity, .. }
                | Command::Resolve { identity, .. } => identity.clone(),
            };
            let scoped = self.root.with_authority(
                crate::storage::sync_state::authority::ControlAuthority::new(
                    self.config.seed_project.clone(),
                    identity,
                ),
            );
            LiveControl {
                catalog: self.catalog,
                root: &scoped,
                config: self.config,
                stopping: self.stopping,
                ingest_complete: self.ingest_complete,
                baseline_complete: self.baseline_complete,
                cursor: self.cursor,
            }
            .handle_authorized(request)
            .await
        })
    }

    async fn handle_authorized(&mut self, request: Command) -> io::Result<()> {
        match request {
            Command::Resolve {
                request,
                reply,
                identity,
            } => {
                let authority = crate::storage::sync_state::authority::ControlAuthority::new(
                    self.config.seed_project.clone(),
                    identity,
                );
                let result = self
                    .root
                    .resolve_conflict(request, authority, self.config.max_object_bytes)
                    .await;
                let _ = reply.send(result);
            }
            Command::Wait {
                deadline,
                reply,
                identity,
            } => {
                let result = match self
                    .root
                    .authorize_controls(self.config.seed_project.clone(), identity)
                    .await
                {
                    Ok(()) => self.wait(deadline).await,
                    Err(error) => Err(io::Error::other(error)),
                };
                let _ = reply.send(result);
            }
            Command::Rescan { reply, identity } => {
                let summary = match self
                    .root
                    .authorize_controls(self.config.seed_project.clone(), identity)
                    .await
                {
                    Ok(()) => self.rescan().await,
                    Err(error) => Err(io::Error::other(error)),
                };
                let _ = reply.send(summary);
            }
            Command::Approve {
                revision,
                reply,
                identity,
            } => {
                let approved = match self
                    .root
                    .authorize_controls(self.config.seed_project.clone(), identity)
                    .await
                {
                    Ok(()) => self
                        .root
                        .approve_deletions(revision)
                        .await
                        .map_err(io::Error::other),
                    Err(error) => Err(io::Error::other(error)),
                };
                let set = approved.as_ref().ok().cloned();
                let _ = reply.send(approved);
                if let Some(set) = set {
                    super::deletion::execute(self.catalog, self.root, set, self.stopping).await?;
                }
            }
        }
        Ok(())
    }
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
    let object = LiveObject {
        catalog,
        root,
        bucket,
        name,
    };
    let Some(plan) = object.plan(summary).await? else {
        return Ok(());
    };
    object.export(writeback, fence, plan, summary).await
}

pub(super) async fn restore_from_catalog(
    catalog: &Catalog,
    root: &RootLease,
    bucket: &BucketName,
    name: &ObjectName,
) -> io::Result<()> {
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
    let object = LiveObject {
        catalog,
        root,
        bucket,
        name,
    };
    let Some(plan) = object.plan(&mut Reconciliation::default()).await? else {
        return Ok(());
    };
    let Some(source) = plan.source else {
        return Ok(());
    };
    let operation = match writeback
        .begin_live_export(
            plan.binding,
            name.clone(),
            plan.generation,
            plan.tracked,
            fence,
            LiveSource::ProtectedRestore(source),
        )
        .await
    {
        Ok(operation) => operation,
        Err(error) if concurrent_change(&error) => return Ok(()),
        Err(error) => return Err(io::Error::other(error)),
    };
    loop {
        match advance_with_retry(&writeback, operation.clone()).await? {
            LiveResult::Pending => {}
            LiveResult::Complete | LiveResult::Conflict | LiveResult::Obsolete => return Ok(()),
        }
    }
}

/// One object whose catalog state a live export reconciles with the working tree.
struct LiveObject<'a> {
    catalog: &'a Catalog,
    root: &'a RootLease,
    bucket: &'a BucketName,
    name: &'a ObjectName,
}

/// Whether the working-tree file already holds the catalog generation's bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Content {
    Identical,
    Changed,
}

/// A live export decided from one read of the binding, tracked path and live object.
struct LivePlan {
    binding: SyncBinding,
    tracked: Option<SyncRecord>,
    generation: Generation,
    source: Option<File>,
    content: Content,
}

impl LiveObject<'_> {
    async fn finding(
        &self,
        record: &ObjectRecord,
        reason: Reason,
        summary: &mut Reconciliation,
    ) -> io::Result<()> {
        finding(
            self.catalog,
            self.root,
            self.bucket,
            self.name,
            record,
            reason,
            summary,
        )
        .await
    }

    /// Reads current state once and decides what to export; `None` means nothing to do now,
    /// with any finding or deferral already counted in `summary`.
    async fn plan(&self, summary: &mut Reconciliation) -> io::Result<Option<LivePlan>> {
        let (catalog, bucket, name) = (self.catalog, self.bucket, self.name);
        let admin = self.root.sync_admin();
        #[cfg(test)]
        export_checkpoint("before-binding").await;
        let binding = match self.root.sync_binding(bucket).await {
            Ok(binding) => binding,
            Err(AdminError::NotFound) => return Ok(None),
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
                    self.finding(&record, reason, summary).await?;
                }
                return Ok(None);
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
                return Ok(None);
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        let Some(record) = record else {
            let Some(tracked) = tracked else {
                return Ok(None);
            };
            return Ok(Some(LivePlan {
                binding,
                generation: tracked.generation,
                tracked: Some(tracked),
                source: None,
                content: Content::Changed,
            }));
        };
        let Some(content) = self.check_target(&admin, &record, path, summary).await? else {
            return Ok(None);
        };
        let generation = Generation::new(record.generation).map_err(io::Error::other)?;
        let source = match admin.open_object(bucket, name, Some(generation)).await {
            Ok((_, reader)) => match content {
                Content::Identical => None,
                Content::Changed => Some(reader.into_file().await?),
            },
            Err(AdminError::NotFound | AdminError::Precondition) => {
                summary.deferred += 1;
                return Ok(None);
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        Ok(Some(LivePlan {
            binding,
            tracked,
            generation,
            source,
            content,
        }))
    }

    /// Compares the live object with its working-tree path; `None` means a finding or
    /// deferral was recorded instead of an export.
    async fn check_target(
        &self,
        admin: &Admin,
        record: &ObjectRecord,
        path: PathBuf,
        summary: &mut Reconciliation,
    ) -> io::Result<Option<Content>> {
        let collision = match hierarchy_collision(admin, self.bucket, self.name).await {
            Ok(collision) => collision,
            Err(AdminError::Precondition | AdminError::NotFound) => {
                summary.deferred += 1;
                return Ok(None);
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        if collision {
            self.finding(record, Reason::Collision, summary).await?;
            return Ok(None);
        }
        let observed = record.clone();
        match file_job_retry(self.root, move |root| inspect(root, &path, &observed)).await? {
            Target::Finding(reason) => {
                self.finding(record, reason, summary).await?;
                Ok(None)
            }
            Target::Identical => Ok(Some(Content::Identical)),
            _ => Ok(Some(Content::Changed)),
        }
    }

    async fn export(
        &self,
        writeback: &Writeback,
        fence: JournalFence,
        mut plan: LivePlan,
        summary: &mut Reconciliation,
    ) -> io::Result<()> {
        let (bucket, name) = (self.bucket, self.name);
        #[cfg(test)]
        export_checkpoint("after-get-path").await;
        let Some(mut operation) = self.begin_export(writeback, fence, &plan, None).await? else {
            summary.deferred += 1;
            return Ok(());
        };
        #[cfg(test)]
        export_checkpoint("after-prepare").await;
        let mut preserved = false;
        loop {
            match advance_with_retry(writeback, operation.clone()).await? {
                LiveResult::Pending => {}
                LiveResult::Complete => {
                    if plan.content == Content::Identical {
                        summary.identical += 1;
                    } else {
                        #[cfg(test)]
                        let _ = LIVE_EXPORTS.try_with(|count| {
                            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        });
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
                    if !preserved {
                        let Some(retry) = self.preserve_export(writeback, fence, &mut plan).await?
                        else {
                            summary.deferred += 1;
                            return Ok(());
                        };
                        operation = retry;
                        preserved = true;
                        continue;
                    }
                    self.root
                        .record_export_conflict(plan.binding.clone(), name.clone(), plan.generation)
                        .await
                        .map_err(io::Error::other)?;
                    diagnostics::emit(
                        LogLevel::Warn,
                        "sync_external_conflict",
                        serde_json::json!({"bucket":bucket.as_str(),"object":name.as_str(),"remedy":"A newer external edit arrived after preservation; stop the writer and rescan. Inspect retained conflicts before restoring or discarding a revision"}),
                    );
                    break;
                }
            }
        }
        Ok(())
    }

    async fn begin_export(
        &self,
        writeback: &Writeback,
        fence: JournalFence,
        plan: &LivePlan,
        captured: Option<crate::storage::sync_state::conflicts::CapturedConflict>,
    ) -> io::Result<Option<LiveExport>> {
        let mut seconds = 1;
        let mut protected = false;
        loop {
            let source = if protected {
                plan.source
                    .as_ref()
                    .map(File::try_clone)
                    .transpose()?
                    .map_or(
                        LiveSource::ProtectedObservation,
                        LiveSource::ProtectedRestore,
                    )
            } else {
                live_source(&plan.source, captured.clone())?
            };
            match writeback
                .begin_live_export(
                    plan.binding.clone(),
                    self.name.clone(),
                    plan.generation,
                    plan.tracked.clone(),
                    fence,
                    source,
                )
                .await
            {
                Ok(operation) => return Ok(Some(operation)),
                Err(error) if concurrent_change(&error) => return Ok(None),
                Err(writeback::Error::Admin(AdminError::Hold))
                    if !protected && captured.is_none() =>
                {
                    protected = true;
                }
                Err(error) if retryable(&error) => {
                    if !wait_retry(&error, &mut seconds, self.root).await {
                        return Err(io::Error::other(error));
                    }
                }
                Err(error) => return Err(io::Error::other(error)),
            }
        }
    }

    async fn preserve_export(
        &self,
        writeback: &Writeback,
        fence: JournalFence,
        plan: &mut LivePlan,
    ) -> io::Result<Option<LiveExport>> {
        let expected_generation = (plan.content == Content::Identical || plan.source.is_some())
            .then_some(plan.generation);
        let captured = match self
            .root
            .capture_path(crate::storage::sync_state::conflicts::CaptureRequest {
                binding: plan.binding.clone(),
                name: self.name.clone(),
                expected_generation,
                source_op: None,
            })
            .await
        {
            Ok(captured) => captured,
            Err(
                crate::storage::sync_state::conflicts::Error::TargetChanged
                | crate::storage::sync_state::conflicts::Error::Admin(
                    AdminError::Precondition
                    | AdminError::NotFound
                    | AdminError::Io(io::ErrorKind::NotFound),
                ),
            ) => return Ok(None),
            Err(error) => return Err(io::Error::other(error)),
        };
        diagnostics::emit(
            LogLevel::Warn,
            "sync_conflict_preserved",
            serde_json::json!({"bucket":self.bucket.as_str(),"object":self.name.as_str(),"conflict_id":captured.id(),"remedy":"Inspect this retained revision before restoring it or explicitly discarding it"}),
        );
        if expected_generation.is_some() && plan.source.is_none() {
            match self
                .root
                .sync_admin()
                .open_object(self.bucket, self.name, expected_generation)
                .await
            {
                Ok((_, reader)) => plan.source = Some(reader.into_file().await?),
                Err(AdminError::Precondition | AdminError::NotFound) => return Ok(None),
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        self.begin_export(writeback, fence, plan, Some(captured))
            .await
    }
}

fn live_source(
    source: &Option<File>,
    captured: Option<crate::storage::sync_state::conflicts::CapturedConflict>,
) -> io::Result<LiveSource> {
    let source = source.as_ref().map(File::try_clone).transpose()?;
    Ok(match captured {
        Some(conflict) => LiveSource::Preserved {
            source,
            conflict: Box::new(conflict),
        },
        None => source.map_or(LiveSource::Observe, LiveSource::ApiCopy),
    })
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

async fn wait_retry(error: &writeback::Error, seconds: &mut u64, root: &RootLease) -> bool {
    if *seconds >= 4 {
        return false;
    }
    #[cfg(test)]
    export_checkpoint("retry").await;
    diagnostics::emit(
        LogLevel::Error,
        "sync_export_retry",
        serde_json::json!({
            "reason":error.to_string(),"retry_seconds":*seconds,
            "remedy":"A transient failure is retried twice. Persistent failure pauses synchronization; free disk space or restore permissions, inspect retained work and request a rescan"
        }),
    );
    let delay = std::time::Duration::from_secs(*seconds);
    let completed = if let Some(mut stopping) = root.stop_signal() {
        if *stopping.borrow() {
            return false;
        }
        tokio::select! {_=stopping.changed()=>false,()=tokio::time::sleep(delay)=>true}
    } else {
        tokio::time::sleep(delay).await;
        true
    };
    *seconds = (*seconds * 2).min(30);
    completed
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
                if !wait_retry(&error, &mut seconds, root).await {
                    return Err(io::Error::other(error));
                }
            }
        }
    }
}

pub(super) async fn create_directory(root: &RootLease, directory: PathBuf) -> io::Result<bool> {
    file_job_retry(root, move |root| {
        match parent(root, &directory, fsutil::DirectoryOpen::CreateIfMissing) {
            Ok(_) => Ok(true),
            Err(ParentError::Finding(_)) => Ok(false),
            Err(ParentError::Io(error)) => Err(error.into()),
        }
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
            Err(error) if retryable(&error) => {
                if !wait_retry(&error, &mut seconds, writeback.root_lease()).await {
                    return Err(io::Error::other(error));
                }
            }
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

pub(super) fn exact_entry(directory: &File, name: &OsStr) -> io::Result<bool> {
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

pub(super) fn parent(
    root: &File,
    path: &Path,
    open: fsutil::DirectoryOpen,
) -> Result<File, ParentError> {
    let mut directory = root.try_clone()?;
    let mut depth = 0;
    for component in path.iter() {
        depth += 1;
        if depth > 128 || component.as_encoded_bytes().len() > 255 {
            return Err(ParentError::Finding(Reason::HostRestriction));
        }
        let exact = exact_entry(&directory, component)?;
        match fsutil::directory_at(&directory, component, fsutil::DirectoryOpen::Existing) {
            Ok(next) => {
                if !exact {
                    return Err(ParentError::Finding(Reason::Collision));
                }
                directory = next;
            }
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && open == fsutil::DirectoryOpen::CreateIfMissing =>
            {
                let next = fsutil::directory_at(
                    &directory,
                    component,
                    fsutil::DirectoryOpen::CreateIfMissing,
                )?;
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
        fsutil::DirectoryOpen::CreateIfMissing,
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
        Err(_)
            if fsutil::directory_at(&directory, name, fsutil::DirectoryOpen::Existing).is_ok() =>
        {
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
    let current_parent = parent(
        root,
        path.parent().ok_or(AdminError::Precondition)?,
        fsutil::DirectoryOpen::Existing,
    )
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
    let source = reader.into_file().await?;
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
                    let directory = parent(root, relative, fsutil::DirectoryOpen::Existing)
                        .map_err(|_| AdminError::Precondition)?;
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
                    let current = parent(root, relative, fsutil::DirectoryOpen::Existing)
                        .map_err(|_| AdminError::Precondition)?;
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
    reconcile_until(catalog, root, &stopping, BaselineGate::Allowed).await
}

/// Whether a reconciliation may record the baseline as complete. It is deferred while the
/// working tree still has entries that ingestion could not take in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BaselineGate {
    Allowed,
    Deferred,
}

impl BaselineGate {
    fn after_ingest(ingested: &Reconciliation) -> Self {
        if ingested.deferred == 0 && ingested.unrepresentable == 0 && ingested.conflicting == 0 {
            Self::Allowed
        } else {
            Self::Deferred
        }
    }
}

/// Whether a reconciliation step finished or saw the stop signal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Continue,
    Stopped,
}

/// Shared state of one full reconciliation pass.
struct Reconciler<'a> {
    catalog: &'a Catalog,
    root: &'a RootLease,
    stopping: &'a tokio::sync::watch::Receiver<bool>,
    admin: Admin,
    writeback: Writeback,
    fence: JournalFence,
}

async fn reconcile_until(
    catalog: &Catalog,
    root: &RootLease,
    stopping: &tokio::sync::watch::Receiver<bool>,
    baseline: BaselineGate,
) -> io::Result<Reconciliation> {
    catalog
        .enable_change_journal()
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
            "Unfinished write-back requires recovery; inspect retained conflicts and workspace findings, resolve the exact retained revision and rescan",
        ));
    }
    let reconciler = Reconciler {
        catalog,
        root,
        stopping,
        admin: root.sync_admin(),
        writeback,
        fence,
    };
    let mut summary = Reconciliation::default();
    if reconciler.buckets(&mut summary).await? == Step::Stopped || *stopping.borrow() {
        return Ok(summary);
    }
    let scan_admin = reconciler.admin.clone();
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
    if baseline == BaselineGate::Allowed
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

impl Reconciler<'_> {
    fn stopped(&self) -> bool {
        *self.stopping.borrow()
    }

    async fn live_object(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        summary: &mut Reconciliation,
    ) -> io::Result<()> {
        reconcile_live_object(
            self.catalog,
            self.root,
            &self.writeback,
            bucket,
            name,
            self.fence,
            summary,
        )
        .await
    }

    async fn buckets(&self, summary: &mut Reconciliation) -> io::Result<Step> {
        let mut after = None;
        loop {
            if self.stopped() {
                return Ok(Step::Stopped);
            }
            let page = self
                .admin
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
                if !self
                    .root
                    .remember_sync_bucket(&bucket)
                    .await
                    .map_err(io::Error::other)?
                {
                    summary.deferred += 1;
                    continue;
                }
                if !create_directory(self.root, directory).await? {
                    summary.unrepresentable += 1;
                }
                if self.objects(&bucket, summary).await? == Step::Stopped
                    || self.tracked_deletions(&bucket, summary).await? == Step::Stopped
                {
                    return Ok(Step::Stopped);
                }
            }
            if !page.has_more {
                return Ok(Step::Continue);
            }
            after = next;
        }
    }

    async fn objects(&self, bucket: &BucketName, summary: &mut Reconciliation) -> io::Result<Step> {
        let mut query = ObjectListQuery::default();
        loop {
            let objects = self
                .admin
                .list_objects(bucket, query.clone(), 256)
                .await
                .map_err(io::Error::other)?;
            for record in objects.items {
                if self.object(bucket, &record, summary).await? == Step::Stopped {
                    return Ok(Step::Stopped);
                }
            }
            match objects.next {
                Some(next) => query.cursor = Some(next),
                None => return Ok(Step::Continue),
            }
        }
    }

    async fn object(
        &self,
        bucket: &BucketName,
        record: &ObjectRecord,
        summary: &mut Reconciliation,
    ) -> io::Result<Step> {
        let catalog = self.catalog;
        if self.stopped() {
            return Ok(Step::Stopped);
        }
        let name = ObjectName::new(record.name.clone()).map_err(io::Error::other)?;
        if let PathMapping::File(path) = mapping::name_to_path(bucket, &name) {
            let binding = self
                .root
                .sync_binding(bucket)
                .await
                .map_err(io::Error::other)?;
            if catalog
                .sync_state()
                .get_path(binding, path.to_string_lossy().into_owned())
                .await
                .map_err(io::Error::other)?
                .is_some()
            {
                self.live_object(bucket, &name, summary).await?;
                return Ok(Step::Continue);
            }
        }
        let path = match mapping::name_to_path(bucket, &name) {
            PathMapping::Unrepresentable(reason) => {
                finding(catalog, self.root, bucket, &name, record, reason, summary).await?;
                return Ok(Step::Continue);
            }
            PathMapping::File(path) => path,
        };
        if hierarchy_collision(&self.admin, bucket, &name)
            .await
            .map_err(io::Error::other)?
        {
            finding(
                catalog,
                self.root,
                bucket,
                &name,
                record,
                Reason::Collision,
                summary,
            )
            .await?;
            return Ok(Step::Continue);
        }
        let observed = record.clone();
        let target = file_job_retry(self.root, move |root| inspect(root, &path, &observed)).await?;
        if self.stopped() {
            return Ok(Step::Stopped);
        }
        match target {
            Target::Finding(reason) => {
                finding(catalog, self.root, bucket, &name, record, reason, summary).await?
            }
            Target::Conflicting => self.live_object(bucket, &name, summary).await?,
            Target::Identical | Target::Absent => {
                self.live_object(bucket, &name, summary).await?;
            }
        }
        Ok(Step::Continue)
    }

    /// Reconciles tracked paths whose objects no longer exist, exporting their deletion.
    async fn tracked_deletions(
        &self,
        bucket: &BucketName,
        summary: &mut Reconciliation,
    ) -> io::Result<Step> {
        let binding = self
            .root
            .sync_binding(bucket)
            .await
            .map_err(io::Error::other)?;
        let mut after_path = String::new();
        loop {
            let tracked = self
                .catalog
                .sync_state()
                .page(binding.clone(), after_path.clone(), 256)
                .await
                .map_err(io::Error::other)?;
            if tracked.is_empty() {
                return Ok(Step::Continue);
            }
            for record in tracked {
                if self.stopped() {
                    return Ok(Step::Stopped);
                }
                after_path = record.path;
                let name = ObjectName::new(record.name).map_err(io::Error::other)?;
                match self.admin.get_object(bucket, &name, None).await {
                    Err(AdminError::NotFound) => self.live_object(bucket, &name, summary).await?,
                    Ok(_) => {}
                    Err(error) => return Err(io::Error::other(error)),
                }
            }
        }
    }
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
            if let Ok(child) =
                fsutil::directory_at(directory, name, fsutil::DirectoryOpen::Existing)
            {
                let child_counts = external_files(&child, &path, admin, runtime, depth + 1)?;
                let checked =
                    fsutil::directory_at(directory, name, fsutil::DirectoryOpen::Existing)?;
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
