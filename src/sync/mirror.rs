#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
use super::MirrorState;
use super::control::PauseChange;
use super::control::{
    Command, ControlError, Decision, Reply, export_retry_remedy, newer_edit_remedy,
};
use super::entries::{EntryClass, classify};
use super::error::{Recorded, StoppedRescan, unless_superseded};
use super::interrupt::{Interrupt, Interrupted};
use super::mapping::{self, PathMapping, Reason};
use super::naming::RootNaming;
pub(super) use super::tree::{ParentError, ParentOpen, parent, structural_entry_error};
use super::{EntryReportSender, MirrorStatus, Pass, SyncError};
use crate::{
    config::Config,
    diagnostics::{self, LogLevel},
    fsutil,
    object::ObjectRecord,
    source_tree::watch,
    storage::{
        Catalog, ObjectListQuery,
        admin::{Admin, AdminError, BucketName, Generation, ObjectName, Origin},
        change_journal::{ChangeKind, ChangePage, ChangeRead, JournalFence},
        sync_state::{
            SyncBinding, SyncPath, SyncRecord,
            observation::PauseReason,
            root::{RootError, RootLease},
            writeback::{self, LiveExport, LiveResult, LiveSource, Stop, Writeback},
        },
    },
};
use lifecycle::{Event, Lifecycle, Reconciled};
use std::{
    fs::File,
    future::Future,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(test)]
use super::Hook;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
use crate::storage::sync_state::writeback::Limits;
#[cfg(test)]
tokio::task_local! { static EXPORT_HOOK: std::sync::Arc<Hook>; }
#[cfg(test)]
tokio::task_local! { static LIVE_EXPORTS: std::sync::Arc<std::sync::atomic::AtomicUsize>; }
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

mod descriptor;
mod gate;
mod lifecycle;
mod observation;
mod wait;

use gate::BaselineGate;
pub(super) use gate::PassGate;

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

/// The first retry delay after a transient failure; it doubles after each consecutive one.
/// Tests shorten it so a retry does not dominate their run time.
const RETRY_DELAY_MIN: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(1)
};
/// The retry delay cap, also the run length after which a failure starts the backoff over.
const RETRY_DELAY_MAX: Duration = Duration::from_secs(30);

/// A retry delay that doubles after each consecutive failure, up to `max`.
#[derive(Clone, Copy, Debug)]
struct Backoff {
    first: Duration,
    max: Duration,
    next: Duration,
    retries: u32,
}

impl Backoff {
    const fn new(first: Duration, max: Duration) -> Self {
        Self {
            first,
            max,
            next: first,
            retries: 0,
        }
    }

    /// The wait before the next retry; the one after it is twice as long, up to `max`.
    fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = delay.saturating_mul(2).min(self.max);
        self.retries = self.retries.saturating_add(1);
        delay
    }

    /// How many retries this backoff has scheduled.
    fn retries(&self) -> u32 {
        self.retries
    }

    /// Starts over, after a run that had recovered.
    fn reset(&mut self) {
        *self = Self::new(self.first, self.max);
    }
}

/// The mirror's restart and watch-rebuild backoff.
const MIRROR_BACKOFF: Backoff = Backoff::new(RETRY_DELAY_MIN, RETRY_DELAY_MAX);
/// A failed export step waits 1 s, then 2 s; a third failure pauses synchronization.
const EXPORT_BACKOFF: Backoff = Backoff::new(Duration::from_secs(1), RETRY_DELAY_MAX);
const EXPORT_RETRIES: u32 = 2;

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
                Command::Wait {reply,..}=>{let _=reply.send(Ok(super::control::WaitResult::held(super::outstanding::Holdup::Paused(paused_for),None,None)));}
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
            skipped.record((bucket, name), outcome);
        } else {
            if !root.remember_sync_bucket(&bucket).await? {
                summary.count_deferred(Path::new(bucket.as_str()));
                continue;
            }
            if !super::deletion::bucket_directory_held(root, &bucket).await? {
                create_directory(root, PathBuf::from(bucket.as_str())).await?;
            }
        }
    }
    Ok(summary)
}

struct Initialized {
    state: LiveState,
    watcher: DirectoryWatch,
    writeback: Writeback,
}

async fn initialize_live(
    run: LiveRun<'_>,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> Result<Initialized, SyncError> {
    let LiveRun {
        catalog,
        root,
        config,
        status,
        resume,
    } = run;
    let initial_root = resume
        .as_ref()
        .map(|request| root.with_authority(request.authority.clone()));
    let operation_root = initial_root.as_ref().unwrap_or(root);
    catalog.enable_change_journal().await?;
    let cursor = catalog.current_fence().await?;
    let watcher = DirectoryWatch::new(root).await?;
    let mut writeback = recover_writeback(operation_root).await?;
    super::naming::activate(&writeback).await?;
    // Before the inventory, which would take the files a rejection has not written back yet,
    // after a stop, a failure or a power loss, for files the user deleted.
    super::deletion::write_back_rejected(catalog, operation_root, Interrupt::new(stopping)).await?;
    root.invalidate_deletions().await?;
    let ingested = super::ingest::rescan_with(
        catalog,
        operation_root,
        config,
        &writeback,
        Interrupt::new(stopping),
    )
    .await?;
    diagnostics::emit(LogLevel::Info, "sync_ingested", &ingested);
    report_entries(status.entries(), &ingested);
    let ingest_complete = BaselineGate::after_ingest(ingested.gate());
    let mut skipped = SkippedExports::default();
    let mut summary = reconcile_with(
        catalog,
        operation_root,
        Interrupt::new(stopping),
        ingest_complete,
        &writeback,
        &mut skipped,
    )
    .await?;
    summary.ingested = ingested.ingested;
    let baseline_complete = summary.baseline_complete;
    diagnostics::emit(LogLevel::Info, "sync_reconciled", &summary);
    observe_deletions(
        catalog,
        operation_root,
        config,
        summary.gate(),
        Interrupt::new(stopping),
        Inventorying::RunStart,
    )
    .await?;
    // Running only now, after this run reconciled the whole tree: until then a starting mirror
    // stays starting, a retrying one retrying and a paused one paused, even while its explicit
    // rescan runs.
    let reconciled = if resume.is_some() {
        Reconciled::Rescan
    } else {
        Reconciled::Run
    };
    status.reconciled(root, reconciled).await?;
    if let Some(request) = resume.take() {
        let _ = request.reply.send(Ok(summary));
        writeback = writeback.with_root(root)?;
    }
    #[cfg(test)]
    export_checkpoint("observation-ready").await;
    Ok(Initialized {
        state: LiveState {
            cursor,
            baseline: BaselineProgress {
                gate: ingest_complete,
                complete: baseline_complete,
            },
            skipped,
        },
        watcher,
        writeback,
    })
}

async fn recover_writeback(root: &RootLease) -> Result<Writeback, SyncError> {
    let writeback = root.default_writeback().await?;
    writeback.recover_pending().await?;
    let recovered = writeback.preserve_protected().await?;
    if recovered.stop() != Stop::Ready {
        return Err(SyncError::Recovery(recovered.stop()));
    }
    Ok(writeback)
}

async fn ingest_external(
    scope: LiveScope<'_>,
    baseline: &mut BaselineProgress,
    skipped: &mut SkippedExports,
) -> Result<Reconciliation, SyncError> {
    ingest_external_with(scope, Interrupt::new(scope.stopping), baseline, skipped).await
}
async fn ingest_external_with(
    scope: LiveScope<'_>,
    interrupt: Interrupt<'_>,
    baseline: &mut BaselineProgress,
    skipped: &mut SkippedExports,
) -> Result<Reconciliation, SyncError> {
    let LiveScope {
        catalog,
        root,
        config,
        writeback,
        entries,
        ..
    } = scope;
    let _publication = root.begin_observation_publication().await?;
    // A rejection the run could not finish waits in the catalog; see `initialize_live`.
    super::deletion::write_back_rejected(catalog, root, interrupt).await?;
    let mut summary =
        super::ingest::rescan_with(catalog, root, config, writeback, interrupt).await?;
    report_entries(entries, &summary);
    baseline.gate = BaselineGate::after_ingest(summary.gate());
    if !baseline.complete
        && baseline.gate == BaselineGate::Allowed
        && interrupt.check() == Interrupted::No
    {
        // Nested control rescans otherwise retain reconciliation state on the observer's stack.
        let merged = Box::pin(reconcile_with(
            catalog,
            root,
            interrupt,
            BaselineGate::Allowed,
            writeback,
            skipped,
        ))
        .await?;
        baseline.complete = merged.baseline_complete;
        // What kept the reconciliation from completing the baseline is this pass's hold too, so
        // a wait names it; with nothing of its own, only an API change during it remains.
        summary.absorb_hold(&merged);
    }
    root.revalidate().await?;
    summary.baseline_complete = baseline.complete;
    summary.interrupted |= interrupt.check() != Interrupted::No;
    if summary.interrupted {
        return Ok(summary);
    }
    observe_deletions(
        catalog,
        root,
        config,
        summary.gate(),
        interrupt,
        Inventorying::Running,
    )
    .await?;
    // Boxed like the reconciliation above, which also runs exports inside this pass.
    let retried = Box::pin(skipped.retry(scope, interrupt)).await?;
    summary.carry_held(&retried);
    summary.interrupted |= interrupt.check() != Interrupted::No;
    Ok(summary)
}

/// Hands the entries an ingestion could not take in to the mirror's status, replacing those of
/// the previous one. An interrupted ingestion saw only part of the tree, so it keeps the last
/// complete list. The status gets a copy: the pass's own report is its count of those entries,
/// which still decides what the pass holds back.
fn report_entries(entries: &EntryReportSender, ingested: &Reconciliation) {
    if !ingested.interrupted {
        entries.report(ingested.entries().clone());
    }
}

/// Which inventory of a run deletes objects, which decides how loudly the log names them.
#[derive(Clone, Copy, Debug)]
enum Inventorying {
    /// The first of a run, after a start, a retry or a resumed rescan. It deletes what was
    /// deleted from the tree while the mirror was not watching, such as while the server was
    /// stopped, which nobody may expect, so it warns.
    RunStart,
    /// A later one, which deletes what was just deleted from the tree.
    Running,
}

impl Inventorying {
    fn applied_level(self) -> LogLevel {
        match self {
            Self::RunStart => LogLevel::Warn,
            Self::Running => LogLevel::Info,
        }
    }
}

async fn observe_deletions(
    catalog: &Catalog,
    root: &RootLease,
    config: &Config,
    pass: PassGate,
    interrupt: Interrupt<'_>,
    inventorying: Inventorying,
) -> Result<(), SyncError> {
    let inventory = super::deletion::inventory_with(root, config, pass, interrupt).await;
    match inventory {
        Ok(Some(set)) => {
            super::deletion::execute_with(catalog, root, set, interrupt)
                .await?
                .report(inventorying.applied_level());
        }
        Ok(None) => {}
        // An API change moved the journal, or a bucket or the root changed, during the
        // inventory. A change that writes the tree raises native events for those writes, which
        // nothing filters out as the mirror's own, and the pass they bring inventories again. A
        // change to metadata alone writes nothing, so the missing files wait for the next change
        // to the tree, a rescan or a wait, and the log says why they still wait. The deletions
        // of a set end on such a change by themselves and leave it to the next inventory too.
        Err(error) if error.is_stale_deletion() => {
            diagnostics::emit(
                LogLevel::Info,
                "sync_deletion_inventory_superseded",
                serde_json::json!({
                    "reason":"An API or bucket change during the deletion inventory made it stale",
                    "next":"The next change to the directory, a rescan or a wait inventories the missing files again",
                }),
            );
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

/// What the live observer knows about the baseline: whether the latest ingest lets a
/// reconciliation complete it, and whether one already has.
#[derive(Clone, Copy, Debug)]
struct BaselineProgress {
    gate: BaselineGate,
    complete: bool,
}

/// State a live run carries from one control to the next.
struct LiveState {
    cursor: JournalFence,
    baseline: BaselineProgress,
    skipped: SkippedExports,
}

/// What every control of one live run borrows. Each request copies it with its own scoped root.
#[derive(Clone, Copy)]
struct LiveScope<'a> {
    catalog: &'a Catalog,
    root: &'a RootLease,
    config: &'a Config,
    writeback: &'a Writeback,
    stopping: &'a tokio::sync::watch::Receiver<bool>,
    /// Where an ingestion names the entries it could not take in. It changes nothing else of the
    /// mirror's status, whose state changes only through the lifecycle events a run reports.
    entries: &'a EntryReportSender,
}

struct LiveControl<'a> {
    scope: LiveScope<'a>,
    state: &'a mut LiveState,
}
impl LiveControl<'_> {
    async fn rescan(&mut self) -> Result<Reconciliation, SyncError> {
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        tests::state::injected_rescan_failure(self.scope.config)?;
        let writeback = recover_writeback(self.scope.root).await?;
        let scope = LiveScope {
            writeback: &writeback,
            ..self.scope
        };
        let summary =
            ingest_external(scope, &mut self.state.baseline, &mut self.state.skipped).await?;
        Ok(summary)
    }

    fn handle<'a>(
        &'a mut self,
        request: Command,
        quiet: observation::Quiet,
        watcher: &'a mut DirectoryWatch,
    ) -> impl Future<Output = Result<(), SyncError>> + 'a {
        // The single worker boxes one control future to bound nested reconciliation stack use.
        Box::pin(async move {
            let history = matches!(&request, Command::Wait { .. } | Command::Rescan { .. })
                .then(|| watcher.history_scan(quiet));
            let identity = match &request {
                Command::Rescan { identity, .. }
                | Command::Decide { identity, .. }
                | Command::Wait { identity, .. }
                | Command::Resolve { identity, .. } => identity.clone(),
            };
            let scoped = self.scope.root.with_authority(
                crate::storage::sync_state::authority::ControlAuthority::new(
                    self.scope.config.seed_project.clone(),
                    identity,
                ),
            );
            let writeback = self.scope.writeback.with_root(&scoped)?;
            let mut control = LiveControl {
                scope: LiveScope {
                    root: &scoped,
                    writeback: &writeback,
                    ..self.scope
                },
                state: &mut *self.state,
            };
            control.handle_with_history(request, history).await
        })
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    async fn handle_authorized(&mut self, request: Command) -> Result<(), SyncError> {
        self.handle_with_history(request, None).await
    }
    async fn handle_with_history(
        &mut self,
        request: Command,
        history: Option<observation::HistoryScan<'_>>,
    ) -> Result<(), SyncError> {
        match request {
            Command::Resolve {
                request,
                reply,
                identity,
            } => {
                let authority = crate::storage::sync_state::authority::ControlAuthority::new(
                    self.scope.config.seed_project.clone(),
                    identity,
                );
                let result = self
                    .scope
                    .root
                    .resolve_conflict(request, authority, self.scope.config.max_object_bytes)
                    .await;
                let _ = reply.send(result);
            }
            Command::Wait {
                deadline,
                reply,
                identity,
                ..
            } => {
                let result = match self
                    .scope
                    .root
                    .authorize_controls(self.scope.config.seed_project.clone(), identity)
                    .await
                {
                    Ok(()) => match self.wait(deadline).await {
                        Ok(result) => {
                            if let (Some(history), Some(summary)) =
                                (history, &result.reconciliation)
                            {
                                history.finish(self.scope.root, summary).await;
                            }
                            Ok(result)
                        }
                        Err(error) => Err(ControlError::Failed(error)),
                    },
                    Err(error) => Err(error.into()),
                };
                let _ = reply.send(result);
            }
            Command::Rescan { reply, identity } => {
                let summary = match self
                    .scope
                    .root
                    .authorize_controls(self.scope.config.seed_project.clone(), identity)
                    .await
                {
                    Ok(()) => match self.rescan().await {
                        Ok(summary) => {
                            if let Some(history) = history {
                                history.finish(self.scope.root, &summary).await;
                            }
                            Ok(summary)
                        }
                        Err(error) => Err(error),
                    },
                    Err(error) => {
                        let _ = reply.send(Err(error.into()));
                        return Ok(());
                    }
                };
                match summary {
                    // A failure that pauses ends the run, and the mirror answers the rescan only
                    // after it paused, so that whoever hears the answer finds the pause.
                    Err(error) if !error.is_transient() => {
                        let reason = error.pause_reason();
                        return Err(SyncError::RescanStopped(Box::new(StoppedRescan {
                            reason,
                            error,
                            reply,
                        })));
                    }
                    summary => {
                        let _ = reply.send(summary.map_err(ControlError::Failed));
                    }
                }
            }
            Command::Decide {
                decision,
                revision,
                reply,
                identity,
            } => self.decide(decision, revision, reply, identity).await?,
        }
        Ok(())
    }

    /// Authorizes a decision on the deletion set `revision` for `identity`, then inventories the
    /// tree again if the set still waits for the decision, so that a native notification since
    /// the last pass that left its members as they were does not make it stale. A stop during
    /// that inventory refuses the decision: the set was not compared with the tree, so
    /// recording a decision on it would apply to members nobody confirmed.
    async fn decidable(
        &self,
        revision: crate::storage::sync_state::OpaqueId,
        identity: Option<crate::auth::Identity>,
    ) -> Result<(), ControlError> {
        use super::deletion::Rechecked;
        self.scope
            .root
            .authorize_controls(self.scope.config.seed_project.clone(), identity)
            .await?;
        match super::deletion::recheck(
            self.scope.root,
            self.scope.config,
            revision,
            Interrupt::new(self.scope.stopping),
        )
        .await
        .map_err(ControlError::Failed)?
        {
            Rechecked::Interrupted => Err(ControlError::Shutdown),
            // The decision itself refuses a set that is no longer the one waiting.
            Rechecked::Confirmed | Rechecked::Replaced | Rechecked::NotAwaiting => Ok(()),
        }
    }

    /// Approves or rejects the deletion set `revision`, as `decision` says, answers, and then
    /// runs the approved set or writes the rejected set's files back from their objects.
    async fn decide(
        &mut self,
        decision: Decision,
        revision: crate::storage::sync_state::OpaqueId,
        reply: Reply<crate::storage::sync_state::deletions::DeletionSet>,
        identity: Option<crate::auth::Identity>,
    ) -> Result<(), SyncError> {
        let root = self.scope.root;
        let decided = match self.decidable(revision, identity).await {
            Ok(()) => match decision {
                Decision::Approve => root.approve_deletions(revision).await,
                Decision::Reject => root.reject_deletions(revision).await,
            }
            .map_err(ControlError::from),
            Err(error) => Err(error),
        };
        let set = decided.as_ref().ok().cloned();
        let _ = reply.send(decided);
        let Some(set) = set else {
            return Ok(());
        };
        let interrupt = Interrupt::new(self.scope.stopping);
        match decision {
            Decision::Approve => {
                super::deletion::execute_with(self.scope.catalog, root, set, interrupt)
                    .await?
                    .report(LogLevel::Info);
            }
            Decision::Reject => {
                super::deletion::write_back_rejected(self.scope.catalog, root, interrupt).await?;
            }
        }
        // The members held back the exports into their bucket's missing directory, which the
        // deletions or the write-back brought back and no tree change would run again.
        Box::pin(self.state.skipped.retry(self.scope, interrupt))
            .await
            .map(drop)
    }
}

/// Exports one object's live state; `Some` is an export skipped for a retry to finish.
async fn reconcile_live_object(
    catalog: &Catalog,
    root: &RootLease,
    writeback: &Writeback,
    bucket: &BucketName,
    name: &ObjectName,
    fence: JournalFence,
    summary: &mut Reconciliation,
) -> Result<Option<SkippedExport>, SyncError> {
    let mut object = LiveObject {
        catalog,
        root,
        bucket,
        name,
        summary,
        missing: MissingFile::Inventory,
    };
    let Some(plan) = object.plan().await? else {
        return Ok(None);
    };
    object.export(writeback, fence, plan).await
}

/// A live export skipped because preserving the file it would replace failed when that file or
/// the API object changed meanwhile, or because that file changed again after preservation, by
/// the generations it was planned with. Its journal change is already behind the cursor: a later
/// API change is replayed, but nothing else exports it again after a changed file.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SkippedExport {
    /// The API generation the export brings to the tree; `None` removes the tracked file.
    api: Option<Generation>,
    /// The generation the tree tracked.
    tracked: Option<Generation>,
}

/// The skipped exports of a live run, by object, which it runs again once the tree settles.
#[derive(Default)]
struct SkippedExports(std::collections::BTreeMap<(BucketName, ObjectName), SkippedExport>);

impl SkippedExports {
    /// Whether every export of this run reached the tree or was superseded.
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Keeps the outcome of an object's latest export: a skipped one to run again, or none.
    fn record(&mut self, object: (BucketName, ObjectName), outcome: Option<SkippedExport>) {
        match outcome {
            Some(skipped) => {
                self.0.insert(object, skipped);
            }
            None => {
                self.0.remove(&object);
            }
        }
    }

    /// Runs each skipped export again while the API and the tree still hold the generations it
    /// was planned with. Another change settled the rest: a journal replay exports a newer API
    /// state, and an ingested edit or a finished export moves the tracked generation. An
    /// interruption or a failure keeps the exports it did not reach for the next pass. Returns
    /// the counts of the retried exports, of which a pass takes only those held for approval
    /// ([`Reconciliation::carry_held`]).
    async fn retry(
        &mut self,
        scope: LiveScope<'_>,
        interrupt: Interrupt<'_>,
    ) -> Result<Reconciliation, SyncError> {
        let mut uncounted = Reconciliation::default();
        if self.is_empty() {
            return Ok(uncounted);
        }
        let fence = scope.catalog.current_fence().await?;
        let pending: Vec<_> = self
            .0
            .iter()
            .map(|(object, skipped)| (object.clone(), *skipped))
            .collect();
        for (object, skipped) in pending {
            if interrupt.check() != Interrupted::No {
                break;
            }
            let (bucket, name) = &object;
            let mut live = LiveObject {
                catalog: scope.catalog,
                root: scope.root,
                bucket,
                name,
                summary: &mut uncounted,
                missing: MissingFile::Inventory,
            };
            let outcome = match live.plan().await? {
                Some(plan) if plan.skipped() == Some(skipped) => {
                    live.export(scope.writeback, fence, plan).await?
                }
                Some(_) | None => None,
            };
            self.record(object, outcome);
        }
        Ok(uncounted)
    }
}

/// What a restore from the catalog left at the object's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Restored {
    /// The object's file, written back from the object.
    Written,
    /// What was put at the path meanwhile: a file, which is taken in as any file is, or an
    /// entry that keeps the object out of the tree as a finding.
    Kept,
    /// Nothing was written: the object, its bucket or its tracked file changed or went
    /// meanwhile, and the export of that change takes over.
    Skipped,
}

pub(super) async fn restore_from_catalog(
    catalog: &Catalog,
    root: &RootLease,
    bucket: &BucketName,
    name: &ObjectName,
) -> Result<Restored, SyncError> {
    let fence = catalog.current_fence().await?;
    let writeback = root.default_writeback().await?;
    // A restore reports no counts of its own.
    let mut uncounted = Reconciliation::default();
    let mut object = LiveObject {
        catalog,
        root,
        bucket,
        name,
        summary: &mut uncounted,
        missing: MissingFile::Restore,
    };
    let Some(plan) = object.plan().await? else {
        return Ok(Restored::Skipped);
    };
    let Some(source) = plan.source else {
        return Ok(Restored::Skipped);
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
        Err(error) if concurrent_change(&error) => return Ok(Restored::Skipped),
        Err(error) => return Err(error.into()),
    };
    loop {
        match advance_with_retry(&writeback, operation.clone()).await? {
            LiveResult::Pending => {}
            LiveResult::Complete => return Ok(Restored::Written),
            LiveResult::Conflict | LiveResult::Collision(_) => return Ok(Restored::Kept),
            LiveResult::Obsolete => return Ok(Restored::Skipped),
        }
    }
}

/// One object whose catalog state a live export reconciles with the working tree.
struct LiveObject<'a> {
    catalog: &'a Catalog,
    root: &'a RootLease,
    bucket: &'a BucketName,
    name: &'a ObjectName,
    /// The counts of the pass this object belongs to.
    summary: &'a mut Reconciliation,
    missing: MissingFile,
}

/// What an export does when the tracked file of an API generation that has not changed since is
/// missing from the tree.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MissingFile {
    /// Writes it back, as the restore after a protected deletion must.
    Restore,
    /// Leaves it to the deletion inventory once the baseline is complete. The file was deleted
    /// from the tree, whether the mirror ran, was paused or the server was stopped meanwhile;
    /// writing it back would undo that deletion without asking anyone.
    Inventory,
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

impl LivePlan {
    /// The API generation this export brings to the tree; `None` removes the tracked file.
    fn api_generation(&self) -> Option<Generation> {
        // Only a removal has neither a body to copy nor an identical one to observe.
        (self.content == Content::Identical || self.source.is_some()).then_some(self.generation)
    }

    /// This export as a skipped one to run again; `None` when the tree already tracks the API
    /// generation, because a divergent file there is ingestion's to publish.
    fn skipped(&self) -> Option<SkippedExport> {
        let skipped = SkippedExport {
            api: self.api_generation(),
            tracked: self.tracked.as_ref().map(|record| record.generation),
        };
        (skipped.api != skipped.tracked).then_some(skipped)
    }
}

impl LiveObject<'_> {
    /// Counts this export as left for a later pass, named by the path the object maps to most
    /// plainly, which a wait that ends meanwhile names.
    fn defer(&mut self) {
        let path = Path::new(self.bucket.as_str()).join(self.name.as_str());
        self.summary.count_deferred(&path);
    }

    /// Records why this object stays out of the working tree and counts it. A generation that
    /// changed or went since this export read it is the next replay's to record.
    async fn finding(&mut self, record: &ObjectRecord, reason: Reason) -> Result<(), SyncError> {
        let binding = self.catalog.sync_state().binding(self.bucket).await?;
        let recorded = unless_superseded(
            self.root
                .record_finding(
                    binding,
                    self.name.clone(),
                    Generation::new(record.generation)?,
                    reason,
                )
                .await,
        )?;
        if recorded == Recorded::Superseded {
            return Ok(());
        }
        if reason == Reason::Ignored {
            self.summary.ignored += 1;
        } else {
            self.summary.count_unmapped_object();
        }
        Ok(())
    }

    /// Reads current state once and decides what to export; `None` means nothing to do now,
    /// with any finding or deferral already counted in `summary`.
    async fn plan(&mut self) -> Result<Option<LivePlan>, SyncError> {
        let (catalog, bucket, name) = (self.catalog, self.bucket, self.name);
        let admin = self.root.sync_admin();
        #[cfg(test)]
        export_checkpoint("before-binding").await;
        let binding = match self.root.sync_binding(bucket).await {
            Ok(binding) => binding,
            Err(AdminError::NotFound) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        #[cfg(test)]
        export_checkpoint("after-binding").await;
        let record = match admin.get_object(bucket, name, None).await {
            Ok(record) => Some(record),
            Err(AdminError::NotFound) => None,
            Err(error) => return Err(error.into()),
        };
        let mapped = match mapping::name_to_path(bucket, name, self.root.naming()?) {
            PathMapping::File(path) if self.root.tree_limits().admits_file(&path) => Ok(path),
            // Exporting a level the scans refuse would stop the whole root, so this object alone
            // stays out of the tree.
            PathMapping::File(_) => Err(Reason::HostRestriction),
            PathMapping::Unrepresentable(reason) => Err(reason),
        };
        let path = match mapped {
            Ok(path) => path,
            Err(reason) => {
                match record {
                    Some(record) => self.finding(&record, reason).await?,
                    None => {
                        unless_superseded(
                            self.root
                                .remove_deleted_finding(binding, name.clone())
                                .await,
                        )?;
                    }
                }
                return Ok(None);
            }
        };
        let tracked = match catalog
            .sync_state()
            .get_path(
                binding.clone(),
                path.to_str()
                    .ok_or(SyncError::Invariant("invalid export path"))?
                    .to_owned(),
            )
            .await
        {
            Ok(tracked) => tracked,
            Err(AdminError::Precondition | AdminError::NotFound) => {
                self.defer();
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let Some(record) = record else {
            let Some(tracked) = tracked else {
                // Nothing to remove from the tree, but a finding that kept the deleted object
                // out of it would still count it.
                unless_superseded(
                    self.root
                        .remove_deleted_finding(binding, name.clone())
                        .await,
                )?;
                return Ok(None);
            };
            if self.replaced(path).await? {
                return Ok(None);
            }
            return Ok(Some(LivePlan {
                binding,
                generation: tracked.generation,
                tracked: Some(tracked),
                source: None,
                content: Content::Changed,
            }));
        };
        let Some(content) = self
            .check_target(&admin, &record, path, tracked.as_ref())
            .await?
        else {
            return Ok(None);
        };
        let generation = Generation::new(record.generation)?;
        #[cfg(test)]
        export_checkpoint("before-open").await;
        let opened = match content {
            Content::Identical => admin
                .get_object(bucket, name, Some(generation))
                .await
                .map(|_| None),
            Content::Changed => admin
                .open_object(bucket, name, Some(generation))
                .await
                .map(|(_, reader)| Some(reader)),
        };
        let source = match opened {
            Ok(Some(reader)) => Some(reader.into_file()?),
            Ok(None) => None,
            Err(AdminError::NotFound | AdminError::Precondition) => {
                self.defer();
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        Ok(Some(LivePlan {
            binding,
            tracked,
            generation,
            source,
            content,
        }))
    }

    /// Whether a directory or special file took the place of the tracked file at `path`, which
    /// then stops that path alone: removing or replacing what is there now would destroy it.
    async fn replaced(&self, path: PathBuf) -> Result<bool, SyncError> {
        let path = path
            .into_os_string()
            .into_string()
            .map_err(|_| SyncError::Invariant("invalid export path"))?;
        match super::deletion::presence(self.root, &self.root.lookup()?, path).await? {
            super::deletion::Presence::Unobservable(reason) if reason.stops_path() => {
                super::deletion::record_replaced(self.root, self.bucket, self.name).await?;
                Ok(true)
            }
            super::deletion::Presence::Absent(_)
            | super::deletion::Presence::Present
            | super::deletion::Presence::Unobservable(_) => Ok(false),
        }
    }

    /// Whether the tracked file of `record`, missing from the tree, is left to the deletion
    /// inventory instead of being written back: only where the inventory looks at it, so an
    /// object it skips, such as one with a finding, is written back rather than left to nobody.
    async fn missing_is_deleted(
        &self,
        record: &ObjectRecord,
        tracked: Option<&SyncRecord>,
    ) -> Result<bool, SyncError> {
        let Some(tracked) = tracked else {
            return Ok(false);
        };
        Ok(self.missing == MissingFile::Inventory
            && self.root.current_baseline().await?
                == crate::storage::sync_state::root::Baseline::Complete
            && self
                .root
                .inventories(self.bucket, tracked, record.generation)
                .await?)
    }

    /// Compares the live object with its working-tree path; `None` means a finding, a path stop
    /// or a deferral was recorded, or a deletion left to the inventory, instead of an export.
    async fn check_target(
        &mut self,
        admin: &Admin,
        record: &ObjectRecord,
        path: PathBuf,
        tracked: Option<&SyncRecord>,
    ) -> Result<Option<Content>, SyncError> {
        let collision = match hierarchy_collision(admin, self.bucket, self.name).await {
            Ok(collision) => collision,
            Err(AdminError::Precondition | AdminError::NotFound) => {
                self.defer();
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if collision {
            self.finding(record, Reason::Collision).await?;
            return Ok(None);
        }
        let observed = record.clone();
        let naming = self.root.naming()?;
        let lookup = self.root.lookup()?;
        match file_job_retry(self.root, move |root| {
            inspect(root, naming, &lookup, &path, &observed)
        })
        .await?
        {
            Target::Directory | Target::Replaced(_) if tracked.is_some() => {
                super::deletion::record_replaced(self.root, self.bucket, self.name).await?;
                Ok(None)
            }
            Target::Finding(reason) | Target::Replaced(reason) => {
                self.finding(record, reason).await?;
                Ok(None)
            }
            Target::Identical => Ok(Some(Content::Identical)),
            Target::Absent if self.missing_is_deleted(record, tracked).await? => Ok(None),
            Target::Absent | Target::Conflicting | Target::Directory => Ok(Some(Content::Changed)),
        }
    }

    /// Exports `plan`; `Some` is the export, skipped because preserving the file it would replace
    /// failed or that file changed again after preservation, or because the bucket's directory
    /// is held for a deletion approval, for a retry to finish.
    async fn export(
        &mut self,
        writeback: &Writeback,
        fence: JournalFence,
        mut plan: LivePlan,
    ) -> Result<Option<SkippedExport>, SyncError> {
        let (bucket, name) = (self.bucket, self.name);
        // Write-back creates the directories above the file it installs, and even a removal opens
        // them, so any export would bring the held directory back.
        if super::deletion::bucket_directory_held(self.root, bucket).await? {
            self.summary.hold_for_approval();
            return Ok(plan.skipped());
        }
        #[cfg(test)]
        export_checkpoint("after-get-path").await;
        let Some(mut operation) = self.begin_export(writeback, fence, &plan, None).await? else {
            self.defer();
            return Ok(None);
        };
        #[cfg(test)]
        export_checkpoint("after-prepare").await;
        #[cfg(test)]
        export_checkpoint("before-arm").await;
        #[cfg(test)]
        export_checkpoint("before-publish").await;
        let mut preserved = false;
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        let mut durable_checkpoint = false;
        loop {
            match advance_with_retry(writeback, operation.clone()).await? {
                LiveResult::Pending => {
                    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                    if !durable_checkpoint
                        && writeback::live_pending(&operation).await.phase
                            == writeback::Phase::Armed
                    {
                        durable_checkpoint = true;
                        export_checkpoint("before-durable").await;
                    }
                }
                LiveResult::Complete => {
                    if plan.content == Content::Identical {
                        self.summary.identical += 1;
                    } else {
                        #[cfg(test)]
                        let _ = LIVE_EXPORTS.try_with(|count| {
                            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        });
                        self.summary.exported += 1;
                    }
                    break;
                }
                LiveResult::Obsolete => {
                    self.defer();
                    break;
                }
                LiveResult::Collision(reason) => {
                    self.root
                        .record_finding(plan.binding.clone(), name.clone(), plan.generation, reason)
                        .await?;
                    // The object is what the tree cannot hold, as for the collision a target
                    // check finds: the directory's own entries are scanned like any other, and
                    // a tracked file it replaced stops only its own path in the inventory.
                    self.summary.count_unmapped_object();
                    break;
                }
                LiveResult::Conflict => {
                    self.summary.conflicting += 1;
                    if !preserved {
                        let Some(retry) = self.preserve_export(writeback, fence, &mut plan).await?
                        else {
                            self.defer();
                            return Ok(plan.skipped());
                        };
                        operation = retry;
                        preserved = true;
                        continue;
                    }
                    self.root
                        .record_export_conflict(plan.binding.clone(), name.clone(), plan.generation)
                        .await?;
                    diagnostics::emit(
                        LogLevel::Warn,
                        "sync_external_conflict",
                        serde_json::json!({"bucket":bucket.as_str(),"object":name.as_str(),"remedy":newer_edit_remedy()}),
                    );
                    // Ingestion publishes the newer edit only while the tree tracks the API
                    // generation; otherwise the rescan the remedy names runs this export again.
                    return Ok(plan.skipped());
                }
            }
        }
        Ok(None)
    }

    async fn begin_export(
        &self,
        writeback: &Writeback,
        fence: JournalFence,
        plan: &LivePlan,
        captured: Option<crate::storage::sync_state::conflicts::CapturedConflict>,
    ) -> Result<Option<LiveExport>, SyncError> {
        let mut retry = EXPORT_BACKOFF;
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
                live_source(plan.source.as_ref(), captured.clone())?
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
                    if !wait_retry(&error, &mut retry, self.root).await {
                        return Err(error.into());
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    async fn preserve_export(
        &self,
        writeback: &Writeback,
        fence: JournalFence,
        plan: &mut LivePlan,
    ) -> Result<Option<LiveExport>, SyncError> {
        let expected_generation = plan.api_generation();
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
                    AdminError::Precondition | AdminError::NotFound,
                ),
            ) => return Ok(None),
            Err(crate::storage::sync_state::conflicts::Error::Admin(AdminError::Io(cause)))
                if cause.kind() == io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
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
                Ok((_, reader)) => plan.source = Some(reader.into_file()?),
                Err(AdminError::Precondition | AdminError::NotFound) => return Ok(None),
                Err(error) => return Err(error.into()),
            }
        }
        self.begin_export(writeback, fence, plan, Some(captured))
            .await
    }
}

fn live_source(
    source: Option<&File>,
    captured: Option<crate::storage::sync_state::conflicts::CapturedConflict>,
) -> Result<LiveSource, SyncError> {
    let source = source.map(File::try_clone).transpose()?;
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
        writeback::Error::Root(
            RootError::InUse
            | RootError::Unsupported
            | RootError::RootChanged
            | RootError::MissingMarker
            | RootError::InvalidMarker
            | RootError::ForeignMarker
            | RootError::ReinitializeDenied
            | RootError::ReplacementOccupied { .. },
        )
        // A displaced target belongs to recovery, so neither is retried automatically.
        | writeback::Error::Conflict(_)
        | writeback::Error::Displaced(_)
        | writeback::Error::Capacity
        | writeback::Error::RecoveryRequired
        | writeback::Error::Protected
        | writeback::Error::WorkspaceChanged => return false,
    };
    match error {
        AdminError::DiskFull(_) => true,
        AdminError::Io(cause) => {
            matches!(
                cause.kind(),
                std::io::ErrorKind::StorageFull | std::io::ErrorKind::PermissionDenied
            ) || super::error::transient_io(&io::Error::from(cause.kind()))
                || super::error::resource_pressure(error)
        }
        AdminError::Busy { .. }
        | AdminError::Closed
        | AdminError::Precondition
        | AdminError::Hold
        | AdminError::SeedProjectMismatch
        | AdminError::SeedMetadataBudget { .. }
        | AdminError::SourceChanged { .. }
        | AdminError::Corrupt(_)
        | AdminError::NotFound
        | AdminError::Invalid(_)
        | AdminError::Checksum
        | AdminError::TooLarge
        | AdminError::Forbidden
        | AdminError::BodyAborted(_)
        | AdminError::Backend(_) => false,
    }
}

async fn wait_retry(error: &writeback::Error, retry: &mut Backoff, root: &RootLease) -> bool {
    if retry.retries() >= EXPORT_RETRIES {
        return false;
    }
    #[cfg(test)]
    export_checkpoint("retry").await;
    let delay = retry.next_delay();
    diagnostics::emit(
        LogLevel::Error,
        "sync_export_retry",
        serde_json::json!({
            "reason":crate::error::Report(error).to_string(),"retry_seconds":delay.as_secs(),
            "remedy":export_retry_remedy()
        }),
    );
    if let Some(mut stopping) = root.stop_signal() {
        if *stopping.borrow() {
            return false;
        }
        tokio::select! {_=stopping.changed()=>false,()=tokio::time::sleep(delay)=>true}
    } else {
        tokio::time::sleep(delay).await;
        true
    }
}

async fn file_job_retry<T: Send + 'static>(
    root: &RootLease,
    job: impl Fn(&File) -> Result<T, AdminError> + Clone + Send + 'static,
) -> Result<T, SyncError> {
    with_retry(root, || {
        let job = job.clone();
        async move { root.file_job(job).await.map_err(writeback::Error::Root) }
    })
    .await
}

pub(super) async fn create_directory(
    root: &RootLease,
    directory: PathBuf,
) -> Result<bool, SyncError> {
    let naming = root.naming()?;
    let lookup = root.lookup()?;
    file_job_retry(root, move |root| {
        match parent(root, naming, &lookup, &directory, ParentOpen::Create) {
            Ok(_) => Ok(true),
            Err(ParentError::Finding(_) | ParentError::Missing) => Ok(false),
            Err(ParentError::Io(error)) => Err(error.into()),
        }
    })
    .await
}

async fn advance_with_retry(
    writeback: &Writeback,
    operation: LiveExport,
) -> Result<LiveResult, SyncError> {
    with_retry(writeback.root_lease(), || {
        writeback.advance_live_export(operation.clone())
    })
    .await
}

async fn with_retry<T, F: Future<Output = Result<T, writeback::Error>>>(
    root: &RootLease,
    mut operation: impl FnMut() -> F,
) -> Result<T, SyncError> {
    let mut retry = EXPORT_BACKOFF;
    loop {
        // Keep nested export futures off the mirror worker's bounded thread stack.
        match Box::pin(operation()).await {
            Ok(result) => return Ok(result),
            Err(error) if retryable(&error) => {
                if !wait_retry(&error, &mut retry, root).await {
                    return Err(error.into());
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn same_inode(left: &File, right: &File) -> Result<bool, AdminError> {
    let a = fsutil::stat_identity(left)?;
    let b = fsutil::stat_identity(right)?;
    Ok(a.node() == b.node())
}

enum Target {
    Absent,
    Identical,
    Conflicting,
    /// A directory holds the name; installation removes it if it may and it is empty.
    Directory,
    Finding(Reason),
    /// A directory or special file holds the path: a tracked file replaced, or a name the tree
    /// cannot hold for an object it never tracked, for the reason given.
    Replaced(Reason),
}

fn inspect(
    root: &File,
    naming: RootNaming,
    lookup: &fsutil::Lookup,
    path: &Path,
    record: &ObjectRecord,
) -> Result<Target, AdminError> {
    let Some(name) = path.file_name() else {
        return Ok(Target::Finding(Reason::InvalidName));
    };
    if !naming.admits(name) {
        return Ok(Target::Finding(Reason::HostRestriction));
    }
    let directory = match parent(
        root,
        naming,
        lookup,
        path.parent()
            .ok_or_else(|| AdminError::Invalid("missing parent".into()))?,
        ParentOpen::Existing,
    ) {
        Ok(directory) => directory,
        Err(ParentError::Missing) => return Ok(Target::Absent),
        Err(ParentError::Finding(reason)) => return Ok(Target::Finding(reason)),
        Err(ParentError::Io(error)) => return Err(error.into()),
    };
    let file = match lookup.regular(&directory, name) {
        Ok(fsutil::RegularChild::File(file)) => file,
        Ok(fsutil::RegularChild::Alias) => return Ok(Target::Finding(Reason::Collision)),
        Ok(fsutil::RegularChild::Missing) => return Ok(Target::Absent),
        Err(error) if !structural_entry_error(&error) => return Err(error.into()),
        Err(_)
            if fsutil::directory_at(&directory, name, fsutil::DirectoryOpen::Existing).is_ok() =>
        {
            return Ok(Target::Directory);
        }
        Err(_) => return Ok(Target::Replaced(Reason::UnsafeEntry)),
    };
    let before = fsutil::stat_identity(&file)?;
    if before.size != record.size {
        return Ok(Target::Conflicting);
    }
    let fsutil::HashOutcome::Complete(hashes) =
        fsutil::hash_exact(&file, record.size, |_| Ok::<_, io::Error>(()))?
    else {
        return Ok(Target::Conflicting);
    };
    let after = fsutil::stat_identity(&file)?;
    let reopened = fsutil::open_regular_at(&directory, name)?;
    let current_parent = parent(
        root,
        naming,
        lookup,
        path.parent().ok_or(AdminError::Precondition)?,
        ParentOpen::Existing,
    )
    .map_err(|error| match error {
        ParentError::Finding(_) | ParentError::Missing => AdminError::Precondition,
        ParentError::Io(error) => error.into(),
    })?;
    if before != after
        || !same_inode(&file, &reopened)?
        || !same_inode(&directory, &current_parent)?
    {
        return Err(AdminError::Precondition);
    }
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

fn concurrent_change(error: &writeback::Error) -> bool {
    match error {
        writeback::Error::Admin(AdminError::Precondition | AdminError::NotFound) => true,
        writeback::Error::Admin(AdminError::Io(cause)) => {
            cause.kind() == io::ErrorKind::AlreadyExists
        }
        writeback::Error::Admin(
            AdminError::Busy { .. }
            | AdminError::DiskFull(_)
            | AdminError::Closed
            | AdminError::Hold
            | AdminError::SeedProjectMismatch
            | AdminError::SeedMetadataBudget { .. }
            | AdminError::SourceChanged { .. }
            | AdminError::Corrupt(_)
            | AdminError::Invalid(_)
            | AdminError::Checksum
            | AdminError::TooLarge
            | AdminError::Forbidden
            | AdminError::BodyAborted(_)
            | AdminError::Backend(_),
        )
        | writeback::Error::Root(_)
        | writeback::Error::Conflict(_)
        | writeback::Error::Displaced(_)
        | writeback::Error::Capacity
        | writeback::Error::RecoveryRequired
        | writeback::Error::Protected
        | writeback::Error::WorkspaceChanged => false,
    }
}

pub async fn reconcile(catalog: &Catalog, root: &RootLease) -> io::Result<Reconciliation> {
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    reconcile_until(catalog, root, &stopping, BaselineGate::Allowed)
        .await
        .map_err(io::Error::from)
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
    interrupt: Interrupt<'a>,
    admin: Admin,
    writeback: Writeback,
    fence: JournalFence,
    /// The counts of this pass; `reconcile_with` completes and returns them.
    summary: Reconciliation,
    /// The live run's skipped exports, which this pass's exports update.
    skipped: &'a mut SkippedExports,
}

async fn reconcile_until(
    catalog: &Catalog,
    root: &RootLease,
    stopping: &tokio::sync::watch::Receiver<bool>,
    baseline: BaselineGate,
) -> Result<Reconciliation, SyncError> {
    let writeback = root.default_writeback().await?;
    super::naming::activate(&writeback).await?;
    // A single reconciliation has no later pass to run a skipped export in.
    reconcile_with(
        catalog,
        root,
        Interrupt::new(stopping),
        baseline,
        &writeback,
        &mut SkippedExports::default(),
    )
    .await
}
async fn reconcile_with(
    catalog: &Catalog,
    root: &RootLease,
    interrupt: Interrupt<'_>,
    baseline: BaselineGate,
    writeback: &Writeback,
    skipped: &mut SkippedExports,
) -> Result<Reconciliation, SyncError> {
    catalog.enable_change_journal().await?;
    let fence = catalog.current_fence().await?;
    if writeback.status().await?.stop() != Stop::Ready {
        return Err(SyncError::WritebackUnfinished(Pass::Reconcile));
    }
    let mut reconciler = Reconciler {
        catalog,
        root,
        interrupt,
        admin: root.sync_admin(),
        writeback: writeback.clone(),
        fence,
        summary: Reconciliation::default(),
        skipped,
    };
    if reconciler.buckets().await? == Step::Stopped || interrupt.check() != Interrupted::No {
        reconciler.summary.interrupted = true;
        return Ok(reconciler.summary);
    }
    let mut summary = reconciler.summary;
    external_files(root, &reconciler.admin, interrupt, &mut summary).await?;
    let end = catalog.current_fence().await?;
    summary.interrupted = interrupt.check() != Interrupted::No;
    // An API change during the pass may not have reached the tree yet, which the pass itself
    // cannot see; everything else that holds the baseline back is the pass's own hold.
    if baseline == BaselineGate::Allowed
        && summary.gate().hold().completes_baseline()
        && end == fence
    {
        root.complete_baseline().await?;
        summary.baseline_complete = true;
    }
    Ok(summary)
}

impl Reconciler<'_> {
    fn stopped(&self) -> bool {
        self.interrupt.check() != Interrupted::No
    }

    async fn live_object(
        &mut self,
        bucket: &BucketName,
        name: &ObjectName,
    ) -> Result<(), SyncError> {
        let outcome = reconcile_live_object(
            self.catalog,
            self.root,
            &self.writeback,
            bucket,
            name,
            self.fence,
            &mut self.summary,
        )
        .await?;
        self.skipped.record((bucket.clone(), name.clone()), outcome);
        Ok(())
    }

    async fn buckets(&mut self) -> Result<Step, SyncError> {
        let mut after = None;
        loop {
            if self.stopped() {
                return Ok(Step::Stopped);
            }
            let page = self.admin.list_all_buckets(after.as_deref(), 256).await?;
            let next = page.items.last().map(|entry| entry.bucket.name.clone());
            for entry in page.items {
                let Ok(bucket) = BucketName::new(entry.bucket.name.clone()) else {
                    self.summary.count_unmapped_object();
                    continue;
                };
                let directory = PathBuf::from(bucket.as_str());
                if !self.root.remember_sync_bucket(&bucket).await? {
                    self.summary.count_deferred(&directory);
                    continue;
                }
                // What occupies the directory's place is a tree entry, which the scans count. A
                // directory gone with files the inventory takes to approval stays gone, and so do
                // the exports into it that `LiveObject::export` holds back.
                if !super::deletion::bucket_directory_held(self.root, &bucket).await?
                    && !create_directory(self.root, directory).await?
                {
                    self.summary.count_unmapped_object();
                }
                if self.objects(&bucket).await? == Step::Stopped
                    || self.tracked_deletions(&bucket).await? == Step::Stopped
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

    async fn objects(&mut self, bucket: &BucketName) -> Result<Step, SyncError> {
        let mut query = ObjectListQuery::default();
        loop {
            let objects = self.admin.list_objects(bucket, query.clone(), 256).await?;
            for record in objects.items {
                if self.object(bucket, &record).await? == Step::Stopped {
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
        &mut self,
        bucket: &BucketName,
        record: &ObjectRecord,
    ) -> Result<Step, SyncError> {
        if self.stopped() {
            return Ok(Step::Stopped);
        }
        let name = ObjectName::new(record.name.clone())?;
        self.live_object(bucket, &name).await?;
        Ok(Step::Continue)
    }

    /// Reconciles tracked paths whose objects no longer exist, exporting their deletion.
    async fn tracked_deletions(&mut self, bucket: &BucketName) -> Result<Step, SyncError> {
        let binding = self.root.sync_binding(bucket).await?;
        let mut after_path: Option<String> = None;
        loop {
            let after = after_path.take().map(SyncPath::try_from).transpose()?;
            let tracked = self
                .catalog
                .sync_state()
                .page(
                    binding.clone(),
                    after,
                    crate::storage::sync_state::PageLimit::LARGEST,
                )
                .await?;
            if tracked.is_empty() {
                return Ok(Step::Continue);
            }
            for record in tracked {
                if self.stopped() {
                    return Ok(Step::Stopped);
                }
                after_path = Some(record.path);
                let name = ObjectName::new(record.name)?;
                match self.admin.get_object(bucket, &name, None).await {
                    Err(AdminError::NotFound) => self.live_object(bucket, &name).await?,
                    Ok(_) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
}

/// Counts into `counts` the files the working tree holds that no live object accounts for, and
/// the entries it holds that no object could.
async fn external_files(
    root: &RootLease,
    admin: &Admin,
    interrupt: Interrupt<'_>,
    counts: &mut Reconciliation,
) -> Result<(), SyncError> {
    let naming = root.naming()?;
    let mut scan = super::tree::TreeScan::new(root.tree_limits(), root.lookup()?);
    while !scan.is_empty() && interrupt.check() == Interrupted::No {
        let (next, page) = root.file_job(move |root| scan.page(root)).await?;
        scan = next;
        for entry in page {
            if interrupt.check() != Interrupted::No {
                return Ok(());
            }
            match classify(&entry, naming) {
                EntryClass::Unreadable(io::ErrorKind::ResourceBusy) => {
                    return Err(io::Error::from(io::ErrorKind::ResourceBusy).into());
                }
                EntryClass::Unreadable(kind) => counts.count_read_failure(&entry.path, kind),
                EntryClass::Bucket(_) | EntryClass::Directory => {}
                EntryClass::Skipped(skip) => counts.count_skipped(&entry.path, skip),
                EntryClass::Unrepresentable(problem) => {
                    counts.count_unrepresentable_entry(&entry.path, problem);
                }
                EntryClass::File(file) => {
                    match admin.get_object(&file.bucket, &file.name, None).await {
                        Ok(_) => {}
                        Err(AdminError::NotFound) => counts.external_only += 1,
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
    }
    Ok(())
}
