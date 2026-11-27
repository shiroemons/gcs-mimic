//! The one module that spawns Tokio tasks and OS threads.
//!
//! Tokio keeps a task's panic in its `JoinHandle`; a fire-and-forget spawn drops the handle and
//! with it the only record of the failure. [`spawn_supervised`] reports every panic and
//! cancellation as a diagnostic event first, then resumes the panic, so a caller that awaits the
//! handle still receives the same `JoinError` as from a plain `tokio::spawn`. Long-lived loops use
//! [`spawn_restarting`], which restarts them after a panic and reports the gap through
//! [`TaskHealth`]. [`spawn_thread`] does the same reporting for a dedicated OS thread,
//! [`catch_job`] for one job that a long-lived worker thread runs without dying with it, and
//! [`SupervisedExecutor`] for the tasks hyper spawns for HTTP/2 streams.

use std::{
    any::Any,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::task::JoinHandle;

/// A unit of work handed to a child before spawning it, including detached cleanup.
#[must_use = "keep the token until the admitted work has finished"]
#[derive(Debug)]
pub(crate) struct TaskToken {
    _receiver: tokio::sync::watch::Receiver<()>,
}

/// Tracks ownership without enabling tokio-util's `rt` feature or changing supervised spawning.
pub(crate) struct TaskTracker {
    root: std::sync::Mutex<Option<Arc<TaskToken>>>,
    work: std::sync::Weak<TaskToken>,
    drained: tokio::sync::watch::Sender<()>,
}

impl Default for TaskTracker {
    fn default() -> Self {
        let (drained, receiver) = tokio::sync::watch::channel(());
        let root = Arc::new(TaskToken {
            _receiver: receiver,
        });
        Self {
            work: Arc::downgrade(&root),
            root: std::sync::Mutex::new(Some(root)),
            drained,
        }
    }
}

impl TaskTracker {
    pub(crate) fn track(&self) -> Option<Arc<TaskToken>> {
        crate::sync::lock(&self.root).clone()
    }

    /// A committed transaction can join cleanup until the last owner releases the catalog.
    /// The journal also serves maintenance passes, which have no request token to inherit.
    /// Never use this for new admission: only work already journaled can join during drain.
    pub(crate) fn track_cleanup(&self) -> Option<Arc<TaskToken>> {
        self.work.upgrade()
    }

    pub(crate) async fn drain(&self) {
        crate::sync::lock(&self.root).take();
        self.drained.closed().await;
    }
}

/// Consecutive failed passes after which a loop that keeps running counts as down. One failure
/// is often transient and retried; a loop that fails every pass is not doing its work.
const FAILED_PASSES_BEFORE_DOWN: u32 = 3;

/// The first restart delay; it doubles after each consecutive panic.
const RESTART_DELAY_MIN: Duration = Duration::from_millis(100);
/// The restart delay cap. A run that lasted at least this long resets the delay, so a rare panic
/// restarts quickly while a loop that panics immediately cannot spin.
const RESTART_DELAY_MAX: Duration = Duration::from_secs(30);

/// Spawns `future` on the current runtime and reports its panic or cancellation.
///
/// Await the returned handle when the caller needs the result; dropping it detaches the task,
/// which keeps running and still reports a panic. `name` identifies the task in diagnostics.
#[expect(
    clippy::disallowed_methods,
    reason = "the supervised wrapper that every other spawn goes through"
)]
pub(crate) fn spawn_supervised<F>(name: &'static str, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(Supervised::new(name, future))
}

/// A hyper executor that spawns through [`spawn_supervised`]. hyper's HTTP/2 server runs each
/// stream in a task of its own and keeps no handle to it, so with `TokioExecutor` a panicking
/// handler would vanish unreported. A panic still ends only its own task: hyper resets that
/// stream and the connection keeps serving the others.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SupervisedExecutor {
    name: &'static str,
}

impl SupervisedExecutor {
    /// `name` identifies the spawned tasks in diagnostics.
    pub(crate) const fn new(name: &'static str) -> Self {
        Self { name }
    }
}

impl<F> hyper::rt::Executor<F> for SupervisedExecutor
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, future: F) {
        // Detached on purpose: the task reports its own panic, and hyper drives it to its end.
        drop(spawn_supervised(self.name, future));
    }
}

/// [`spawn_supervised`] on an explicit runtime, for `Drop` impls that may run outside one.
#[expect(
    clippy::disallowed_methods,
    reason = "the supervised wrapper for spawns on an explicit runtime handle"
)]
pub(crate) fn spawn_supervised_on<F>(
    runtime: &tokio::runtime::Handle,
    name: &'static str,
    future: F,
) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime.spawn(Supervised::new(name, future))
}

/// Spawns a named OS thread and reports its panic before the thread ends.
///
/// A dedicated thread is for work that would otherwise hold a bounded blocking-pool slot for
/// its whole life, such as the catalog executor or a directory walk feeding a channel. Like
/// [`spawn_supervised`], the panic is resumed afterwards, so `join` still returns it.
///
/// The unwind drops what `body` captured before the report, so a captured channel sender closes
/// first and its receiver can act on the failure, or let the process exit, before the panic is
/// logged. A thread that signals its end that way keeps the signal outside a [`catch_job`] that
/// borrows it.
#[expect(
    clippy::disallowed_methods,
    reason = "the supervised wrapper that every other OS thread goes through"
)]
pub(crate) fn spawn_thread<F, T>(
    name: &'static str,
    body: F,
) -> std::io::Result<std::thread::JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || match catch_unwind(AssertUnwindSafe(body)) {
            Ok(value) => value,
            Err(payload) => {
                report_panic(name, payload.as_ref());
                resume_unwind(payload)
            }
        })
}

/// A job that panicked; [`catch_job`] has already reported the panic message.
#[derive(Debug, thiserror::Error)]
#[error("{0} job panicked")]
pub(crate) struct JobPanicked(&'static str);

/// Runs one job of a long-lived worker thread and reports its panic.
///
/// The worker keeps serving later jobs, and the job's caller receives [`JobPanicked`] instead
/// of a reply that never comes. The job's state is dropped during the unwind and never observed
/// again, so catching the unwind cannot expose it half-updated.
pub(crate) fn catch_job<T>(name: &'static str, job: impl FnOnce() -> T) -> Result<T, JobPanicked> {
    catch_unwind(AssertUnwindSafe(job)).map_err(|payload| {
        report_panic(name, payload.as_ref());
        JobPanicked(name)
    })
}

/// Spawns a supervised blocking job on a retained runtime, including from `Drop`.
#[expect(
    clippy::disallowed_methods,
    reason = "The shared blocking entry point reports panics through catch_job."
)]
pub(crate) fn spawn_blocking_on<F, T>(
    runtime: &tokio::runtime::Handle,
    name: &'static str,
    job: F,
) -> JoinHandle<Result<T, JobPanicked>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    runtime.spawn_blocking(move || catch_job(name, job))
}

/// A blocking job that was cancelled before execution or panicked while running.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BlockingFailed {
    #[error("blocking job cancelled")]
    Cancelled(#[source] tokio::task::JoinError),
    #[error("blocking job panicked")]
    Panicked(#[source] JobPanicked),
}

/// Awaits a supervised blocking job with one typed worker-failure result.
pub(crate) async fn run_blocking<F, T>(name: &'static str, job: F) -> Result<T, BlockingFailed>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    run_blocking_on(&tokio::runtime::Handle::current(), name, job).await
}

/// Awaits a supervised job on the runtime that owns the resource being cleaned up.
pub(crate) async fn run_blocking_on<F, T>(
    runtime: &tokio::runtime::Handle,
    name: &'static str,
    job: F,
) -> Result<T, BlockingFailed>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    join_blocking(&mut spawn_blocking_on(runtime, name, job)).await
}

/// Dropping the future cancels the job if it is still queued.
pub(crate) async fn run_blocking_cancellable<F, T>(
    name: &'static str,
    job: F,
) -> Result<T, BlockingFailed>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let mut job = spawn_blocking_on(&tokio::runtime::Handle::current(), name, job);
    let _guard = AbortOnDrop(job.abort_handle());
    join_blocking(&mut job).await
}

#[must_use = "the guard aborts the queued blocking job when dropped"]
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) async fn join_blocking<T>(
    job: &mut JoinHandle<Result<T, JobPanicked>>,
) -> Result<T, BlockingFailed> {
    job.await
        .map_err(BlockingFailed::Cancelled)?
        .map_err(BlockingFailed::Panicked)
}

/// Whether a background task is doing its work, as readiness reports it.
///
/// A task is down while it waits to restart after a panic and until a pass of the replacement
/// succeeds, and while [`FAILED_PASSES_BEFORE_DOWN`] or more consecutive passes have failed. A
/// successful pass clears both.
#[derive(Clone, Debug, Default)]
pub(crate) struct TaskHealth(Arc<HealthState>);

#[derive(Debug, Default)]
struct HealthState {
    restarting: AtomicBool,
    failed_passes: AtomicU32,
}

impl TaskHealth {
    pub(crate) fn is_down(&self) -> bool {
        self.0.restarting.load(Ordering::Acquire)
            || self.0.failed_passes.load(Ordering::Acquire) >= FAILED_PASSES_BEFORE_DOWN
    }

    /// Records a pass that did its work; it ends a restart gap and a run of failed passes.
    pub(crate) fn pass_succeeded(&self) {
        self.0.failed_passes.store(0, Ordering::Release);
        self.0.restarting.store(false, Ordering::Release);
    }

    /// Records a pass that failed and will be retried.
    pub(crate) fn pass_failed(&self) {
        let _ = self
            .0
            .failed_passes
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            });
    }

    pub(crate) fn mark_restarting(&self) {
        self.0.restarting.store(true, Ordering::Release);
    }

    /// Supervision stops only when the owner has closed, which readiness reports itself.
    fn supervision_ended(&self) {
        self.0.failed_passes.store(0, Ordering::Release);
        self.0.restarting.store(false, Ordering::Release);
    }
}

/// Runs a long-lived loop and restarts it after a panic, with exponential backoff.
///
/// `start` builds the next run; returning `None` stops supervision, for example once the owner
/// has closed. A run that returns normally is not restarted. Aborting supervision drops its
/// current run; dropping the handle alone detaches supervision as with any Tokio task. After a panic,
/// `health` stays down until a pass of a replacement run calls [`TaskHealth::pass_succeeded`],
/// so a run that panics again right after starting never looks healthy in between. The first
/// run starts healthy: its owner has just opened and recovered the state the loop maintains.
pub(crate) fn spawn_restarting<S, F>(
    name: &'static str,
    health: TaskHealth,
    mut start: S,
) -> JoinHandle<()>
where
    S: FnMut() -> Option<F> + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    spawn_supervised(name, async move {
        let mut delay = RESTART_DELAY_MIN;
        while let Some(run) = start() {
            let started = tokio::time::Instant::now();
            if (CatchPanic {
                name,
                future: Box::pin(run),
            })
            .await
            .is_ok()
            {
                break;
            }
            health.mark_restarting();
            if started.elapsed() >= RESTART_DELAY_MAX {
                delay = RESTART_DELAY_MIN;
            }
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                crate::diagnostics::Event::BackgroundTaskRestarting,
                serde_json::json!({
                    "message": format!("background task {name} panicked; restarting in {} ms", delay.as_millis()),
                    "task": name,
                    "delay_ms": delay.as_millis(),
                }),
            );
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2).min(RESTART_DELAY_MAX);
        }
        health.supervision_ended();
    })
}

/// Polls the boxed future inside `catch_unwind` so a panic is reported before Tokio stores it.
struct CatchPanic<F> {
    name: &'static str,
    future: Pin<Box<F>>,
}

type PanicPayload = Box<dyn Any + Send>;

fn poll_catching<F: Future>(
    name: &'static str,
    mut future: Pin<&mut F>,
    context: &mut Context<'_>,
) -> Poll<Result<F::Output, PanicPayload>> {
    match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(context))) {
        Ok(Poll::Pending) => Poll::Pending,
        Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
        Err(payload) => {
            report_panic(name, payload.as_ref());
            Poll::Ready(Err(payload))
        }
    }
}

impl<F: Future> Future for CatchPanic<F> {
    type Output = Result<F::Output, PanicPayload>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        poll_catching(this.name, this.future.as_mut(), context)
    }
}

#[must_use = "dropping an unfinished supervised task cancels it"]
struct Supervised<F> {
    name: &'static str,
    future: Pin<Box<F>>,
    finished: bool,
}

impl<F> Supervised<F> {
    fn new(name: &'static str, future: F) -> Self {
        Self {
            name,
            future: Box::pin(future),
            finished: false,
        }
    }
}

impl<F: Future> Future for Supervised<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<F::Output> {
        let this = &mut *self;
        // The future is never polled again after it panics, so observing it mid-unwind is harmless.
        match poll_catching(this.name, this.future.as_mut(), context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(value)) => {
                this.finished = true;
                Poll::Ready(value)
            }
            Poll::Ready(Err(payload)) => {
                this.finished = true;
                resume_unwind(payload)
            }
        }
    }
}

impl<F> Drop for Supervised<F> {
    fn drop(&mut self) {
        // Aborts and runtime shutdown drop unfinished tasks; shutdown makes this routine.
        if !self.finished {
            crate::diagnostics::emit_lazy(
                crate::diagnostics::LogLevel::Debug,
                crate::diagnostics::Event::BackgroundTaskCancelled,
                || {
                    serde_json::json!({
                        "message": format!("background task {} was cancelled", self.name),
                        "task": self.name,
                    })
                },
            );
        }
    }
}

fn report_panic(name: &'static str, payload: &(dyn Any + Send)) {
    let message = panic_message(payload);
    #[cfg(test)]
    tests::record_panic(name, message);
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Error,
        crate::diagnostics::Event::BackgroundTaskPanicked,
        serde_json::json!({
            "message": format!("background task {name} panicked: {message}"),
            "task": name,
        }),
    );
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        // xtask-lint: allow(text::downcast) -- panic payloads arrive as Box<dyn Any + Send>
        .downcast_ref::<&str>()
        .copied()
        // xtask-lint: allow(text::downcast) -- panic payloads arrive as Box<dyn Any + Send>
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::assert_matches;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn draining_waits_for_handed_off_work_and_never_reopens() {
        let tracker = TaskTracker::default();
        let parent = tracker.track().unwrap();
        let draining = tracker.drain();
        tokio::pin!(draining);
        tokio::select! {
            biased;
            () = &mut draining => panic!("active parent was skipped"),
            () = std::future::ready(()) => {}
        }
        assert!(tracker.track().is_none());
        let cleanup = tracker.track_cleanup().unwrap();
        let child = Arc::clone(&parent);
        drop(parent);
        drop(cleanup);
        tokio::select! {
            biased;
            () = &mut draining => panic!("detached child was skipped"),
            () = std::future::ready(()) => {}
        }
        drop(child);
        draining.await;
        assert!(tracker.track().is_none());
        assert!(tracker.track_cleanup().is_none());
        tracker.drain().await;
    }

    #[tokio::test]
    async fn a_panicking_supervised_task_releases_its_shutdown_token() {
        let tracker = TaskTracker::default();
        let token = tracker.track().unwrap();
        let panicking = spawn_supervised("test-tracked-panic", async move {
            let _token = token;
            panic!("injected tracked panic");
        });
        assert!(panicking.await.unwrap_err().is_panic());
        tokio::time::timeout(Duration::from_secs(5), tracker.drain())
            .await
            .unwrap();
    }

    static PANICS: Mutex<Vec<(&'static str, String)>> = Mutex::new(Vec::new());

    #[tokio::test]
    async fn blocking_job_reports_panics_to_the_caller() {
        let error = run_blocking("blocking-panic-test", || panic!("blocking job failed"))
            .await
            .unwrap_err();
        assert_matches!(
            error,
            BlockingFailed::Panicked(JobPanicked("blocking-panic-test"))
        );
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .is::<JobPanicked>()
        );
        assert_eq!(
            reported_panics("blocking-panic-test"),
            ["blocking job failed"]
        );
    }

    pub(super) fn record_panic(name: &'static str, message: &str) {
        PANICS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((name, message.to_owned()));
    }

    /// The panic messages reported for `name`; tests use names unique to themselves.
    pub(crate) fn reported_panics(name: &str) -> Vec<String> {
        PANICS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(task, _)| *task == name)
            .map(|(_, message)| message.clone())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_detached_panic_is_reported_and_the_runtime_keeps_serving() {
        const NAME: &str = "test-detached-panic";
        drop(spawn_supervised(NAME, async {
            panic!("detached fixture panic");
        }));
        crate::test_support::eventually_async("the detached panic to be reported", || async {
            !reported_panics(NAME).is_empty()
        })
        .await;
        assert_eq!(reported_panics(NAME), ["detached fixture panic"]);
        let next = spawn_supervised("test-after-panic", async { 7 });
        assert_eq!(next.await.unwrap(), 7);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_hyper_executor_reports_a_panic_and_keeps_spawning() {
        use hyper::rt::Executor as _;
        const NAME: &str = "test-executor-panic";
        let executor = SupervisedExecutor::new(NAME);
        executor.execute(async {
            panic!("executor fixture panic");
        });
        crate::test_support::eventually_async("the executor panic to be reported", || async {
            !reported_panics(NAME).is_empty()
        })
        .await;
        assert_eq!(reported_panics(NAME), ["executor fixture panic"]);
        let (done, finished) = tokio::sync::oneshot::channel();
        executor.execute(async move {
            let _ = done.send(7);
        });
        assert_eq!(finished.await.unwrap(), 7);
    }

    #[tokio::test]
    async fn an_awaited_panic_still_surfaces_as_a_join_error() {
        const NAME: &str = "test-awaited-panic";
        let error = spawn_supervised(NAME, async {
            panic!("{}", String::from("owned fixture panic"));
        })
        .await
        .unwrap_err();
        assert!(error.is_panic());
        assert_eq!(reported_panics(NAME), ["owned fixture panic"]);
    }

    #[tokio::test]
    async fn an_aborted_task_is_cancelled_without_a_panic_report() {
        const NAME: &str = "test-aborted";
        let task = spawn_supervised(NAME, std::future::pending::<()>());
        task.abort();
        let cause = task.await.unwrap_err();
        assert!(cause.is_cancelled());
        let error = BlockingFailed::Cancelled(cause);
        assert_eq!(error.to_string(), "blocking job cancelled");
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .is::<tokio::task::JoinError>()
        );
        assert!(reported_panics(NAME).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_restarting_loop_restarts_after_panics_and_reports_the_gap() {
        const NAME: &str = "test-restarting";
        let health = TaskHealth::default();
        let runs = Arc::new(AtomicUsize::new(0));
        let (finished, done) = tokio::sync::oneshot::channel();
        let mut finished = Some(finished);
        let started = runs.clone();
        let supervisor = spawn_restarting(NAME, health.clone(), move || {
            let run = started.fetch_add(1, Ordering::SeqCst);
            let finished = if run == 2 { finished.take() } else { None };
            Some(async move {
                if let Some(finished) = finished {
                    let _ = finished.send(());
                    return;
                }
                panic!("restart fixture panic {run}");
            })
        });
        // Paused time auto-advances through the backoff once every task is idle.
        done.await.unwrap();
        supervisor.await.unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 3);
        assert_eq!(
            reported_panics(NAME),
            ["restart fixture panic 0", "restart fixture panic 1"]
        );
        assert!(!health.is_down());
    }

    #[tokio::test(start_paused = true)]
    async fn a_restarted_loop_is_down_until_its_first_successful_pass() {
        const NAME: &str = "test-restart-first-pass";
        let health = TaskHealth::default();
        let (started, mut replacement_started) = tokio::sync::mpsc::unbounded_channel();
        let (proceed, proceeding) = tokio::sync::oneshot::channel::<()>();
        let mut proceeding = Some(proceeding);
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        let reported = health.clone();
        let supervisor = spawn_restarting(NAME, health.clone(), move || {
            let run = counted.fetch_add(1, Ordering::SeqCst);
            let started = started.clone();
            let proceeding = if run == 1 { proceeding.take() } else { None };
            let reported = reported.clone();
            Some(async move {
                let Some(proceeding) = proceeding else {
                    panic!("first-pass fixture panic {run}");
                };
                let _ = started.send(());
                let _ = proceeding.await;
                reported.pass_succeeded();
                std::future::pending::<()>().await;
            })
        });
        replacement_started.recv().await.unwrap();
        // The replacement has started but not yet finished a pass, so it has proven nothing.
        for _ in 0..10 {
            tokio::task::yield_now().await;
            assert!(health.is_down(), "a started replacement is not yet healthy");
        }
        proceed.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while health.is_down() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert_eq!(reported_panics(NAME), ["first-pass fixture panic 0"]);
        supervisor.abort();
        assert!(supervisor.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn aborting_supervision_drops_its_active_run() {
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let observed = dropped.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let mut started = Some(started);
        let supervisor = spawn_restarting("test-abort-run", TaskHealth::default(), move || {
            let started = started.take()?;
            let guard = Dropped(observed.clone());
            Some(async move {
                let _guard = guard;
                started.send(()).unwrap();
                std::future::pending::<()>().await;
            })
        });
        ready.await.unwrap();
        supervisor.abort();
        assert!(supervisor.await.unwrap_err().is_cancelled());
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn consecutive_failed_passes_are_down_until_a_pass_succeeds() {
        let health = TaskHealth::default();
        for _ in 1..FAILED_PASSES_BEFORE_DOWN {
            health.pass_failed();
            assert!(!health.is_down(), "a retried failure is transient");
        }
        health.pass_failed();
        assert!(health.is_down(), "failing every pass is down");
        health.pass_failed();
        assert!(health.is_down());
        health.pass_succeeded();
        assert!(!health.is_down(), "a successful pass recovers");
        health.pass_failed();
        assert!(
            !health.is_down(),
            "the failure count restarts after a success"
        );
    }

    #[test]
    fn a_thread_panic_is_reported_and_still_reaches_join() {
        const NAME: &str = "test-thread-panic";
        let thread = spawn_thread(NAME, || -> u8 { panic!("thread fixture panic") }).unwrap();
        assert!(thread.join().is_err());
        assert_eq!(reported_panics(NAME), ["thread fixture panic"]);
        let next = spawn_thread("test-thread-after-panic", || 7).unwrap();
        assert_eq!(next.join().unwrap(), 7);
    }

    #[test]
    fn a_job_panic_is_reported_and_returned_as_an_error() {
        const NAME: &str = "test-job-panic";
        let error = catch_job(NAME, || -> u8 { panic!("job fixture panic") }).unwrap_err();
        assert_eq!(error.to_string(), "test-job-panic job panicked");
        assert_eq!(reported_panics(NAME), ["job fixture panic"]);
        assert_eq!(catch_job(NAME, || 7).unwrap(), 7);
    }

    #[tokio::test(start_paused = true)]
    async fn health_is_down_after_a_panic_until_supervision_stops() {
        const NAME: &str = "test-restart-health";
        let health = TaskHealth::default();
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = attempts.clone();
        let supervisor = spawn_restarting(NAME, health.clone(), move || {
            // The second start stops supervision instead of restarting.
            (counted.fetch_add(1, Ordering::SeqCst) == 0).then_some(async {
                panic!("health fixture panic");
            })
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !health.is_down() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        supervisor.await.unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(!health.is_down(), "a stopped supervisor is not down");
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = JobPanicked("worker");
    assert_eq!(error.to_string(), "worker job panicked");
    assert!(std::error::Error::source(&error).is_none());
    let error = BlockingFailed::Panicked(JobPanicked("worker"));
    assert_eq!(error.to_string(), "blocking job panicked");
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<JobPanicked>()
    );
}
