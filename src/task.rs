//! The one module that spawns Tokio tasks.
//!
//! Tokio keeps a task's panic in its `JoinHandle`; a fire-and-forget spawn drops the handle and
//! with it the only record of the failure. [`spawn_supervised`] reports every panic and
//! cancellation as a diagnostic event first, then resumes the panic, so a caller that awaits the
//! handle still receives the same `JoinError` as from a plain `tokio::spawn`. Long-lived loops use
//! [`spawn_restarting`], which restarts them after a panic and reports the gap through
//! [`TaskHealth`].

use std::{
    any::Any,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::task::JoinHandle;

/// The first restart delay; it doubles after each consecutive panic.
const RESTART_DELAY_MIN: Duration = Duration::from_millis(100);
/// The restart delay cap. A run that lasted at least this long resets the delay, so a rare panic
/// restarts quickly while a loop that panics immediately cannot spin.
const RESTART_DELAY_MAX: Duration = Duration::from_secs(30);

/// Spawns `future` on the current runtime and reports its panic or cancellation.
///
/// Await the returned handle when the caller needs the result; dropping it detaches the task,
/// which keeps running and still reports a panic. `name` identifies the task in diagnostics.
pub(crate) fn spawn_supervised<F>(name: &'static str, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(Supervised::new(name, future))
}

/// [`spawn_supervised`] on an explicit runtime, for `Drop` impls that may run outside one.
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

/// Whether a restartable background task is down after a panic and waiting to restart.
#[derive(Clone, Debug, Default)]
pub(crate) struct TaskHealth(Arc<AtomicBool>);

impl TaskHealth {
    pub(crate) fn is_down(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn mark_down(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn mark_running(&self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Runs a long-lived loop and restarts it after a panic, with exponential backoff.
///
/// `start` builds the next run; returning `None` stops supervision, for example once the owner
/// has closed. A run that returns normally or is cancelled is not restarted. `health` reports
/// down from a panic until the replacement run starts.
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
            health.mark_running();
            let started = tokio::time::Instant::now();
            match spawn_supervised(name, run).await {
                Ok(()) => break,
                Err(error) if error.is_cancelled() => break,
                Err(_) => {}
            }
            health.mark_down();
            if started.elapsed() >= RESTART_DELAY_MAX {
                delay = RESTART_DELAY_MIN;
            }
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "background_task_restarting",
                serde_json::json!({
                    "message": format!("background task {name} panicked; restarting in {} ms", delay.as_millis()),
                    "task": name,
                    "delay_ms": delay.as_millis(),
                }),
            );
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2).min(RESTART_DELAY_MAX);
        }
        health.mark_running();
    })
}

/// Polls the boxed future inside `catch_unwind` so a panic is reported before Tokio stores it.
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
        match catch_unwind(AssertUnwindSafe(|| this.future.as_mut().poll(context))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(ready) => {
                this.finished = true;
                ready
            }
            Err(payload) => {
                this.finished = true;
                report_panic(this.name, payload.as_ref());
                resume_unwind(payload)
            }
        }
    }
}

impl<F> Drop for Supervised<F> {
    fn drop(&mut self) {
        // Aborts and runtime shutdown drop unfinished tasks; shutdown makes this routine.
        if !self.finished {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Debug,
                "background_task_cancelled",
                serde_json::json!({
                    "message": format!("background task {} was cancelled", self.name),
                    "task": self.name,
                }),
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
        "background_task_panicked",
        serde_json::json!({
            "message": format!("background task {name} panicked: {message}"),
            "task": name,
        }),
    );
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    static PANICS: Mutex<Vec<(&'static str, String)>> = Mutex::new(Vec::new());

    pub(super) fn record_panic(name: &'static str, message: &str) {
        PANICS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((name, message.to_owned()));
    }

    /// The panic messages reported for `name`; tests use names unique to themselves.
    pub(crate) fn reported_panics(name: &str) -> Vec<String> {
        PANICS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
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
        tokio::time::timeout(Duration::from_secs(5), async {
            while reported_panics(NAME).is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(reported_panics(NAME), ["detached fixture panic"]);
        let next = spawn_supervised("test-after-panic", async { 7 });
        assert_eq!(next.await.unwrap(), 7);
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
        assert!(task.await.unwrap_err().is_cancelled());
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
    async fn health_is_down_until_the_replacement_starts_and_none_stops() {
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
