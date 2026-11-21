#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
use super::MirrorState;
use super::control::PauseChange;
use super::control::{Command, ControlError, Decision, Reply};
use super::error::StoppedRescan;
use super::interrupt::{Interrupt, Interrupted};
use super::{MirrorStatus, SyncError};
use crate::{
    config::Config,
    diagnostics::{self, LogLevel},
    source_tree::watch,
    storage::{
        Catalog,
        admin::{AdminError, BucketName, ObjectName, Origin},
        change_journal::{ChangeKind, ChangePage, ChangeRead},
        sync_state::{
            observation::PauseReason,
            root::{AtStage, RootError, RootLease, Stage},
            writeback::Writeback,
        },
    },
};
use lifecycle::{Event, Lifecycle, Reconciled};
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

/// The first retry delay after a transient failure; it doubles after each consecutive one.
/// Tests shorten it so a retry does not dominate their run time.
const RETRY_DELAY_MIN: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(1)
};

/// The mirror's restart and watch-rebuild backoff.
const MIRROR_BACKOFF: Backoff = Backoff::new(RETRY_DELAY_MIN, RETRY_DELAY_MAX);
#[cfg(test)]
use super::Hook;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
use crate::storage::sync_state::writeback::Limits;
#[cfg(test)]
tokio::task_local! { static EXPORT_HOOK: std::sync::Arc<Hook>; }
#[cfg(test)]
tokio::task_local! { pub(super) static LIVE_EXPORTS: std::sync::Arc<std::sync::atomic::AtomicUsize>; }
#[cfg(test)]
pub(super) async fn export_checkpoint(phase: &str) {
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
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use tests::state::{RescanFault, arm_rescan_fault};

mod controls;
mod descriptor;
mod gate;
mod lifecycle;
mod live;
mod observation;
mod wait;

use super::backoff::{Backoff, RETRY_DELAY_MAX};
use super::export::{
    SkippedExports,
    reconcile::Step,
    reconcile::reconcile_with,
    reconcile_live_object,
    retry::{DirectoryCreation, create_directory},
};
use super::reevaluate::{Changed, Observing};
pub(super) use controls::LiveScope;
use controls::{LiveControl, LiveState};
pub(super) use gate::BaselineGate;
pub(super) use gate::PassGate;
use live::{
    BaselineProgress, Initialized, ingest_external, ingest_external_with, initialize_live,
    recover_writeback,
};
#[derive(Default, Debug, serde::Serialize)]
pub struct Reconciliation {
    pub source_changing: bool,
    pub interrupted: bool,
    pub ingested: u64,
    /// The directories of buckets deleted through the API that still hold entries. Each is also
    /// one of the tree's unrepresentable entries, and the entries below it are not counted, so
    /// this only breaks that count down.
    pub blocked_buckets: u64,
    pub exported: u64,
    pub identical: u64,
    pub conflicting: u64,
    pub external_only: u64,
    pub ignored: u64,
    /// Every name this pass could not map, on either side, and the entries it could not read for
    /// a reason waiting does not clear. The objects and buckets the API holds and the tree cannot
    /// are reported only; the tree's entries alone hold back the baseline and deletions. Visible
    /// for the struct literals of tests; only its own methods change it.
    #[serde(flatten)]
    pub(crate) unrepresentable: gate::Unrepresentable,
    pub baseline_complete: bool,
    /// Files left for a later pass because they were still changing or raced an API write.
    pub deferred: u64,
    /// The exports among `deferred` that wait for a deletion approval, because they would
    /// recreate a bucket directory the inventory takes to approval.
    #[serde(skip)]
    pub(crate) held_for_approval: u64,
    /// The first of the `deferred` files that this pass names.
    #[serde(skip)]
    pub(crate) deferred_files: gate::DeferredFiles,
    /// The objects left of an alias that ingestion took an edit of its file in for, whose file
    /// the same pass settles.
    #[serde(skip)]
    pub(crate) heirs: Vec<(BucketName, ObjectName)>,
}

pub struct Task {
    stop: tokio::sync::watch::Sender<bool>,
    done: tokio::sync::oneshot::Receiver<()>,
    status: MirrorStatus,
    // Holding the sender keeps the worker's rescan channel open for the task's lifetime.
    #[cfg_attr(
        not(all(test, any(target_os = "linux", target_os = "macos"))),
        expect(dead_code, reason = "only unit tests request a rescan")
    )]
    rescan: tokio::sync::mpsc::Sender<Command>,
}

struct DirectoryWatch {
    watch: watch::Watch,
    // Vnode descriptors must close before the lease can release exclusive root ownership.
    _root: RootLease,
    pending: Option<observation::Pending>,
    generation: u64,
}

impl DirectoryWatch {
    async fn new(root: &RootLease) -> Result<Self, SyncError> {
        let lease = root.clone();
        let path = root.watch_path().to_owned();
        let limits = root.tree_limits();
        let native = root
            .file_job(move |root| {
                // A backend cause prints as the failure it holds and leaves that failure below
                // it for the retry decision, so every kind keeps the same wrapping.
                watch::build(root, &path, limits).map_err(AdminError::backend)
            })
            .await?;
        Ok(Self {
            watch: watch::Watch::new(native)?,
            _root: lease,
            pending: None,
            generation: 0,
        })
    }

    async fn rebuild(
        &mut self,
        root: &RootLease,
        stopping: &tokio::sync::watch::Receiver<bool>,
        status: &Lifecycle,
    ) -> Result<(), SyncError> {
        let mut backoff = MIRROR_BACKOFF;
        let mut failures = 0;
        let mut stopping = stopping.clone();
        loop {
            if *stopping.borrow() {
                return Ok(());
            }
            match Self::new(root).await {
                Ok(mut watcher) => {
                    self.poll_changes()?;
                    watcher.watch.inherit(&self.watch);
                    watcher.generation = self.generation;
                    watcher.pending = self.pending.take();
                    *self = watcher;
                    status.on(root, Event::WatchRebuilt).await;
                    return Ok(());
                }
                Err(error) if error.is_watch_retryable() => {
                    failures += 1;
                    status.on(root, Event::WatchRetrying).await;
                    #[cfg(test)]
                    export_checkpoint("retry").await;
                    if failures >= 3 {
                        return Err(error);
                    }
                    let delay = backoff.next_delay();
                    diagnostics::emit(
                        LogLevel::Error,
                        "sync_watch_retry",
                        descriptor::diagnostic(
                            &error,
                            serde_json::json!({
                                "reason":crate::error::Report(&error).to_string(),"delay_ms":delay.as_millis(),
                                "remedy":"Restore read/search permissions on the sync tree; synchronization is retrying until every subtree is watched, and pauses until an explicit rescan when the retries fail"
                            }),
                        ),
                    );
                    tokio::select! {
                        _ = stopping.changed() => return Ok(()),
                        () = tokio::time::sleep(delay) => {},
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Task {
    /// A closed queue or a dropped reply means the mirror stopped before it answered.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) async fn rescan(&self) -> Result<Reconciliation, ControlError> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.rescan
            .send(Command::Rescan {
                reply: sender,
                identity: None,
            })
            .await
            .map_err(|_| ControlError::Shutdown)?;
        receiver.await.map_err(|_| ControlError::Shutdown)?
    }
    pub(crate) fn stop(&self) {
        self.stop.send_replace(true);
    }
    /// What the mirror is doing, for the readiness probe.
    pub fn status(&self) -> MirrorStatus {
        self.status.clone()
    }
    pub async fn finish(self) -> io::Result<()> {
        self.stop();
        self.done.await.map_err(io::Error::other)
    }
}

pub fn start(catalog: Catalog, root: RootLease, config: &Config) -> io::Result<Task> {
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let root = std::sync::Arc::new(root.with_shutdown(stopping.clone()));
    let (done, completion) = tokio::sync::oneshot::channel();
    let (rescan, requests) = tokio::sync::mpsc::channel(1);
    // The mirror reports its own state instead of joining the catalog's background tasks, so
    // that a stopped mirror stops synchronization but never the Storage API's readiness. The
    // controls and the readiness probe read the one handle made here.
    let lifecycle = Lifecycle::new(config);
    let status = lifecycle.subscribe();
    catalog
        .register_sync_controls(&root, rescan.clone(), status.clone())
        .map_err(io::Error::other)?;
    let mirror = Mirror {
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        _release_hold: tests::state::ReleaseHold::armed(config),
        catalog,
        root,
        config: config.clone(),
        runtime: tokio::runtime::Handle::current(),
        lifecycle,
        stopping,
        requests,
        controls: observation::Controls::default(),
    };
    crate::task::spawn_thread("sync-mirror", move || mirror.run(done))?;
    Ok(Task {
        stop,
        done: completion,
        status,
        rescan,
    })
}

/// What the mirror thread owns. A transient failure (catalog contention, a timeout) is retried
/// with backoff. A persistent failure pauses intake but keeps ownership and the controls; only
/// an explicit rescan after repair may resume synchronization.
struct Mirror {
    /// Holds the fields after it, where a test arms it, until the test lets them drop, so that
    /// the test sees whether `Task::finish` returns before the root is released.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    _release_hold: tests::state::ReleaseHold,
    catalog: Catalog,
    root: std::sync::Arc<RootLease>,
    config: Config,
    runtime: tokio::runtime::Handle,
    lifecycle: Lifecycle,
    stopping: tokio::sync::watch::Receiver<bool>,
    requests: tokio::sync::mpsc::Receiver<Command>,
    controls: observation::Controls,
}

/// What follows a failed run.
enum AfterFailure {
    /// Run again after the retry delay.
    Retry(SyncError),
    /// The explicit rescan that resumes the pause arrived.
    Resume(Resume),
    /// The mirror stops while paused.
    Stop,
}

impl Mirror {
    fn run(self, done: tokio::sync::oneshot::Sender<()>) {
        // Locals drop in reverse order, also while a panic unwinds this thread: the guard reports
        // `Stopped` first, the mirror then releases the root, and `done` goes last, so that whoever
        // `Task::finish` wakes can activate the root at once.
        let done = done;
        let mut mirror = self;
        let _stopped = StopOnExit {
            lifecycle: mirror.lifecycle.clone(),
            catalog: mirror.catalog.clone(),
        };
        let mut backoff = MIRROR_BACKOFF;
        let mut resume = None;
        loop {
            let started = std::time::Instant::now();
            let Err(error) = mirror.live(&mut resume) else {
                break;
            };
            match mirror.fail(error, resume.take()) {
                AfterFailure::Retry(error) => {
                    if mirror.retry(&error, &mut backoff, started) == Step::Stopped {
                        break;
                    }
                }
                AfterFailure::Resume(request) => {
                    resume = Some(request);
                    backoff.reset();
                }
                AfterFailure::Stop => break,
            }
        }
        mirror.shut_down(done);
    }

    /// One supervised run, which takes `resume` once it has resumed synchronization.
    fn live(&mut self, resume: &mut Option<Resume>) -> Result<(), SyncError> {
        let result = crate::task::catch_job("sync-mirror", || {
            #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
            tests::injected_live_failure(&self.config)?;
            let run = run_live_config(
                LiveRun {
                    catalog: &self.catalog,
                    root: &self.root,
                    config: &self.config,
                    status: &self.lifecycle,
                    resume,
                },
                &mut self.stopping,
                &mut self.requests,
                &mut self.controls,
            );
            #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
            let run = tests::state::failing_on_signal(&self.config, run);
            self.runtime.block_on(run)
        });
        result.unwrap_or_else(|panicked| Err(SyncError::Panicked(panicked)))
    }

    /// Reports `event` to the lifecycle, which alone changes the state and the recorded pause.
    fn on(&self, event: Event) -> Option<crate::sync::status::Transition> {
        self.runtime.block_on(self.lifecycle.on(&self.root, event))
    }

    /// Moves the mirror on after a run failed with `error`. `resume` is the explicit rescan that
    /// was to resume a pause when its run failed; a running rescan whose failure stopped the run
    /// comes inside `error`. Either hears the failure once the pause it leaves is recorded. A
    /// failed resume keeps the mirror paused even when the failure is transient: a retry would
    /// resume synchronization without a rescan that succeeded.
    fn fail(&mut self, error: SyncError, resume: Option<Resume>) -> AfterFailure {
        if let SyncError::RescanStopped(stopped) = error {
            let StoppedRescan {
                reason,
                error,
                reply,
            } = *stopped;
            let change = self.pause_for(reason);
            return self.paused(error, reason, change, Some(reply));
        }
        let Some(resume) = resume else {
            if error.is_transient() {
                self.on(Event::FailedTransiently);
                return AfterFailure::Retry(error);
            }
            let reason = error.pause_reason();
            let change = self.pause_for(reason);
            return self.paused(error, reason, change, None);
        };
        if error.is_transient() {
            self.on(Event::FailedTransiently);
            return self.paused(
                error,
                resume.paused_for,
                PauseChange::Kept,
                Some(resume.reply),
            );
        }
        let reason = error.pause_reason();
        let change = self.pause_for(reason);
        self.paused(error, reason, change, Some(resume.reply))
    }

    /// Pauses for `reason`, and says whether that began the pause or recorded a new reason for
    /// one already in place.
    fn pause_for(&self, reason: PauseReason) -> PauseChange {
        match self.on(Event::FailedPersistently(reason)) {
            Some(_) => PauseChange::Began,
            None => PauseChange::Recorded,
        }
    }

    /// Reports the pause for `reason` that `error` left, answers `waiting`, the rescan that failed
    /// with it, and waits for the explicit rescan that resumes synchronization.
    fn paused(
        &mut self,
        error: SyncError,
        reason: PauseReason,
        change: PauseChange,
        waiting: Option<Reply<Reconciliation>>,
    ) -> AfterFailure {
        match change {
            PauseChange::Kept => descriptor::resume_failed(&error),
            PauseChange::Began | PauseChange::Recorded => descriptor::paused(&error, reason),
        }
        if let Some(reply) = waiting {
            let _ = reply.send(Err(ControlError::RescanFailed {
                reason,
                change,
                error,
            }));
        }
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        tests::injected_pause_panic(&self.config);
        let resume = self.runtime.block_on(await_resume(
            &self.catalog,
            &self.root,
            &self.config,
            &mut self.requests,
            &mut self.stopping,
            &mut self.controls,
            reason,
        ));
        resume.map_or(AfterFailure::Stop, AfterFailure::Resume)
    }

    /// Waits out the next retry delay after a transient `error`, unless the mirror stops first.
    fn retry(
        &mut self,
        error: &SyncError,
        backoff: &mut Backoff,
        started: std::time::Instant,
    ) -> Step {
        // A run that lasted longer than the cap had recovered, so its failure starts over.
        if started.elapsed() >= RETRY_DELAY_MAX {
            backoff.reset();
        }
        let delay = backoff.next_delay();
        descriptor::retrying(error, delay);
        let stopping = &mut self.stopping;
        self.runtime.block_on(async {
            tokio::select! {
                _ = stopping.wait_for(|stop| *stop) => Step::Stopped,
                () = tokio::time::sleep(delay) => Step::Continue,
            }
        })
    }

    fn shut_down(mut self, done: tokio::sync::oneshot::Sender<()>) {
        // In the guard's order: whoever sees `Stopped` finds the controls gone, and the queued
        // controls hear that synchronization stopped only after that.
        withdraw_controls(&self.catalog);
        self.lifecycle.stop();
        self.controls.stop();
        drop(self.requests);
        let (stopping, catalog) = (&mut self.stopping, &self.catalog);
        self.runtime.block_on(async {
            if !*stopping.borrow() {
                tokio::select! {
                    _=stopping.changed()=>{},
                    ()=catalog.wait_for_sync_shutdown()=>{},
                }
            }
        });
        drop(self.root);
        let _ = done.send(());
    }
}

/// Withdraws the controls this mirror registered.
fn withdraw_controls(catalog: &Catalog) {
    if let Err(error) = catalog.clear_sync_controls() {
        diagnostics::emit(
            LogLevel::Error,
            "sync_controls_closed",
            serde_json::json!({"reason":error.to_string()}),
        );
    }
}

/// Reports the mirror stopped however its thread ends. A panic outside a supervised run skips
/// [`Mirror::shut_down`], so the guard then also withdraws the controls: a status that still read
/// as paused, behind controls that can never resume it, would mislead whoever reads it. It
/// withdraws them before reporting `Stopped`, so whoever sees that state finds them gone.
struct StopOnExit {
    lifecycle: Lifecycle,
    catalog: Catalog,
}

impl Drop for StopOnExit {
    fn drop(&mut self) {
        if std::thread::panicking() {
            withdraw_controls(&self.catalog);
        }
        self.lifecycle.stop();
    }
}

struct LiveRun<'a> {
    catalog: &'a Catalog,
    root: &'a RootLease,
    config: &'a Config,
    /// Where the run reports what changes the mirror's state.
    status: &'a Lifecycle,
    /// The explicit rescan that resumes a paused mirror; a run that succeeds answers and takes
    /// it, and one that fails leaves it to [`Mirror::fail`].
    resume: &'a mut Option<Resume>,
}

struct Resume {
    reply: Reply<Reconciliation>,
    authority: crate::storage::sync_state::authority::ControlAuthority,
    /// The reason the pause this rescan is to end recorded, which a transient failure keeps.
    paused_for: PauseReason,
}

/// Answers controls while paused for `paused_for` until an explicit rescan arrives, and returns
/// it; `None` means the mirror is stopping.
async fn await_resume(
    catalog: &Catalog,
    root: &RootLease,
    config: &Config,
    requests: &mut tokio::sync::mpsc::Receiver<Command>,
    stopping: &mut tokio::sync::watch::Receiver<bool>,
    controls: &mut observation::Controls,
    paused_for: PauseReason,
) -> Option<Resume> {
    if let Err(error) = root
        .observe_recoveries(crate::storage::sync_state::authority::ControlAuthority::Permissive)
        .await
    {
        // Explicit rescan remains available because it reruns recovery after the operator repairs access.
        diagnostics::emit(
            LogLevel::Error,
            "sync_recovery_observation_failed",
            serde_json::json!({"reason":crate::error::Report(&error).to_string(),"remedy":"Preserve the workspace and inspect the retained entry before attempting recovery"}),
        );
    }
    while !*stopping.borrow() {
        tokio::select! {
            biased;
            _=stopping.changed()=>return None,
            request=controls.receive(requests)=>match request? {
                Command::Rescan {reply,identity}=> {
                    match root.authorize_controls(config.seed_project.clone(),identity.clone()).await {
                        Ok(())=>return Some(Resume {reply,authority:crate::storage::sync_state::authority::ControlAuthority::new(config.seed_project.clone(),identity),paused_for}),
                        Err(error)=>{let _=reply.send(Err(error.into()));}
                    }
                }
                Command::Decide {reply,..}=>{let _=reply.send(Err(ControlError::Paused));}
                Command::Wait {reply,..}=>{let _=reply.send(Ok(super::report::ending::WaitResult::held(super::report::verdict::Holdup::Paused(paused_for),None,None)));}
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
    controls: &mut observation::Controls,
) -> Result<(), SyncError> {
    let (catalog, root, config, status) = (run.catalog, run.root, run.config, run.status);
    let Initialized {
        mut state,
        mut watcher,
        writeback,
    } = initialize_live(run, stopping).await?;
    let mut unsettled = observation::Unsettled::new();
    if controls.has_waiters() {
        watcher.continue_quiet();
        unsettled.unsettle(root).await?;
    }
    while !*stopping.borrow() {
        observation::refresh(root, &mut watcher, &mut unsettled, controls).await?;
        let request = controls.ready(&watcher, unsettled.quiet).or_else(|| {
            if controls.available() {
                requests.try_recv().ok()
            } else {
                None
            }
        });
        if let Some(request) = request {
            let Some(request) = controls.admit(request, &watcher, unsettled.quiet) else {
                continue;
            };
            let Some(request) = controls.running(request, stopping) else {
                break;
            };
            LiveControl {
                scope: LiveScope {
                    catalog,
                    root,
                    config,
                    writeback: &writeback,
                    stopping,
                    entries: status.entries(),
                },
                state: &mut state,
            }
            .handle(request, unsettled.quiet, &mut watcher)
            .await?;
            continue;
        }
        let page = match catalog.read_changes(state.cursor, None, 256).await? {
            ChangeRead::Rescan { fence } => {
                state.cursor = fence;
                let summary = reconcile_with(
                    catalog,
                    root,
                    Interrupt::new(stopping),
                    state.baseline.gate,
                    &writeback,
                    &mut state.skipped,
                )
                .await?;
                diagnostics::emit(LogLevel::Info, "sync_reconciled", &summary);
                continue;
            }
            ChangeRead::Page(page) => page,
        };
        if page.items.is_empty() {
            #[cfg(test)]
            export_checkpoint("idle").await;
            tokio::select! {
                biased;
                _ = stopping.changed() => break,
                observed = watcher.next_observation() => {
                    match observed? {
                        observation::Observed::Pending => {
                            unsettled.quiet=observation::Quiet::Unverified;
                            unsettled.unsettle(root).await?;
                            #[cfg(test)]
                            export_checkpoint("observation-invalidated").await;
                        }
                        observation::Observed::Settled(observed,settled) => {
                            if settled==observation::Quiet::Established {controls.settled();}
                            unsettled.quiet=settled;
                            if observed==watch::Change::Structure {watcher.rebuild(root,stopping,status).await?;}
                            if *stopping.borrow() {break}
                            let history=watcher.history_scan(unsettled.quiet);
                            let scope=LiveScope {catalog,root,config,writeback:&writeback,stopping,entries:status.entries()};
                            let summary=ingest_external(scope,&mut state.baseline,&mut state.skipped).await?;
                            history.finish(root,&summary).await;
                            unsettled.settle();
                            if unsettled.quiet==observation::Quiet::Capped {
                                if watcher.pending.is_none() {watcher.continue_quiet();}
                                unsettled.unsettle(root).await?;
                            }
                            diagnostics::emit(LogLevel::Info,"sync_ingested",&summary);
                        }
                    }
                }
                ()=controls.expired()=>{controls.expire(root,config).await?;}
                Some(request) = requests.recv(), if controls.available() => {
                    let Some(request)=controls.admit(request,&watcher,unsettled.quiet) else {continue};
                    let Some(request)=controls.running(request,stopping) else {break};
                    let scope=LiveScope {catalog,root,config,writeback:&writeback,stopping,entries:status.entries()};
                    LiveControl {scope,state:&mut state}.handle(request,unsettled.quiet,&mut watcher).await?;
                }
                result = catalog.wait_for_changes(state.cursor) => { result?; }
            }
            continue;
        }
        state.cursor = page.next;
        let summary = replay_page(
            catalog,
            root,
            &writeback,
            page,
            stopping,
            &mut state.skipped,
        )
        .await?;
        diagnostics::emit(LogLevel::Info, "sync_exported", &summary);
    }
    Ok(())
}

async fn replay_page(
    catalog: &Catalog,
    root: &RootLease,
    writeback: &Writeback,
    page: ChangePage,
    stopping: &tokio::sync::watch::Receiver<bool>,
    skipped: &mut SkippedExports,
) -> Result<Reconciliation, SyncError> {
    replay_page_with(
        catalog,
        root,
        writeback,
        page,
        Interrupt::new(stopping),
        skipped,
    )
    .await
}
async fn replay_page_with(
    catalog: &Catalog,
    root: &RootLease,
    writeback: &Writeback,
    page: ChangePage,
    interrupt: Interrupt<'_>,
    skipped: &mut SkippedExports,
) -> Result<Reconciliation, SyncError> {
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
    let mut changed = std::collections::BTreeMap::<BucketName, Changed>::new();
    for (bucket, name) in keys {
        if interrupt.check() != Interrupted::No {
            summary.interrupted = true;
            return Ok(summary);
        }
        let Ok(bucket) = BucketName::new(bucket) else {
            continue;
        };
        if let Some(name) = name {
            let name = ObjectName::new(name)?;
            // Read before the change, which can remove the finding that names it.
            let spelling = root.alias_group(bucket.clone(), name.clone()).await?;
            let outcome = reconcile_live_object(
                catalog,
                root,
                writeback,
                &bucket,
                &name,
                page.fence,
                &mut summary,
            )
            .await?;
            skipped.record((bucket.clone(), name.clone()), outcome);
            let changed = changed.entry(bucket).or_default();
            changed.spellings.extend(spelling);
            changed.names.push(name);
        } else {
            if !root.remember_sync_bucket(&bucket).await? {
                summary.count_deferred(Path::new(bucket.as_str()));
                continue;
            }
            if !super::deletion::bucket_directory_held(root, &bucket).await? {
                match create_directory(root, PathBuf::from(bucket.as_str())).await? {
                    // The scans count what occupies the directory's place.
                    DirectoryCreation::Present | DirectoryCreation::Finding => {}
                    DirectoryCreation::Deferred => {
                        summary.count_deferred(Path::new(bucket.as_str()));
                    }
                }
            }
        }
    }
    let mut observing = Observing {
        catalog,
        root,
        writeback,
        fence: page.fence,
        summary: &mut summary,
    };
    for (bucket, changed) in &changed {
        skipped
            .observe_changed(&mut observing, bucket, changed, interrupt)
            .await?;
    }
    Ok(summary)
}
