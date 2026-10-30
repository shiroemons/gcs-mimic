//! The dedicated OS thread that runs every job on the catalog connection.
//!
//! Catalog jobs used to run on Tokio's shared blocking pool after taking the one execution
//! permit. When file I/O or gzip work filled that pool (8 threads by default, 1 at minimum), the
//! job waited in the pool queue while holding the permit and stalled every other catalog call.
//! A thread owned by the catalog keeps metadata work independent of the pool's load. The
//! execution permit still serializes jobs, so the queue holds at most one job in practice.

use tokio::sync::{mpsc, oneshot};

/// What the thread does after a job: closing the catalog runs the last job.
enum Flow {
    Continue,
    Stop,
}

type Job = Box<dyn FnOnce() -> Flow + Send>;

/// Why a job produced no result.
#[derive(Debug)]
pub(super) enum ExecutorError {
    /// The thread has stopped after closing the catalog.
    Closed,
    /// The job panicked; the thread keeps serving later jobs.
    Panicked,
}

impl std::fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Closed => "catalog is closed",
            Self::Panicked => "catalog operation panicked",
        })
    }
}

impl std::error::Error for ExecutorError {}

impl crate::storage::failure::StorageCause for ExecutorError {
    fn into_cause(self) -> crate::storage::failure::BackendCause {
        crate::storage::failure::BackendCause::from_error(self)
    }
}

pub(super) struct Executor {
    jobs: mpsc::Sender<Job>,
}

impl Executor {
    /// Starts the thread. It exits after a [`Self::run_last`] job or once the catalog is dropped.
    pub(super) fn start() -> std::io::Result<Self> {
        let (jobs, mut queue) = mpsc::channel::<Job>(1);
        crate::task::spawn_thread("gcs-mimic-catalog", move || {
            while let Some(job) = queue.blocking_recv() {
                if let Flow::Stop = job() {
                    break;
                }
            }
        })?;
        Ok(Self { jobs })
    }

    /// Runs `job` on the catalog thread. Once queued, the job runs to completion even if the
    /// caller is dropped, so a cancelled request never leaves its catalog change half applied.
    pub(super) async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, ExecutorError> {
        self.submit(move || (job(), Flow::Continue)).await
    }

    /// Runs `job` as the thread's final job; later calls fail with [`ExecutorError::Closed`].
    pub(super) async fn run_last<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, ExecutorError> {
        self.submit(move || (job(), Flow::Stop)).await
    }

    async fn submit<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> (T, Flow) + Send + 'static,
    ) -> Result<T, ExecutorError> {
        let (reply, result) = oneshot::channel();
        let job: Job = Box::new(move || {
            // The job's captured state is dropped before the reply is sent, so a caller woken by
            // the reply observes released resources, such as the directory lock after close.
            match crate::task::catch_job("gcs-mimic-catalog", job) {
                Ok((value, flow)) => {
                    let _ = reply.send(Ok(value));
                    flow
                }
                Err(_) => {
                    let _ = reply.send(Err(ExecutorError::Panicked));
                    Flow::Continue
                }
            }
        });
        self.jobs
            .send(job)
            .await
            .map_err(|_| ExecutorError::Closed)?;
        // A queued job is dropped unrun only when the thread stops after closing the catalog.
        result.await.map_err(|_| ExecutorError::Closed)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn jobs_run_in_order_and_a_panic_does_not_stop_the_thread() {
        let executor = Executor::start().unwrap();
        let first = executor.run(|| std::thread::current().id()).await.unwrap();
        assert_ne!(first, std::thread::current().id());
        assert!(matches!(
            executor.run(|| panic!("executor fixture panic")).await,
            Err(ExecutorError::Panicked)
        ));
        let second = executor.run(|| std::thread::current().id()).await.unwrap();
        assert_eq!(first, second, "jobs keep one dedicated thread");
        assert_eq!(executor.run_last(|| 7).await.unwrap(), 7);
        assert!(matches!(
            executor.run(|| ()).await,
            Err(ExecutorError::Closed)
        ));
    }

    #[tokio::test]
    async fn a_dropped_caller_does_not_cancel_a_queued_job() {
        let executor = Executor::start().unwrap();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let (done, finished) = std::sync::mpsc::channel();
        let (started, running) = oneshot::channel();
        let call = executor.run(move || {
            started.send(()).unwrap();
            held.recv().unwrap();
            done.send(()).unwrap();
        });
        let mut call = Box::pin(call);
        tokio::select! {
            _ = &mut call => unreachable!("the job is still held"),
            started = running => started.unwrap(),
        }
        drop(call);
        release.send(()).unwrap();
        finished
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
    }

    #[test]
    fn jobs_run_while_the_blocking_pool_is_saturated() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let executor = Executor::start().unwrap();
            let (release, held) = std::sync::mpsc::channel::<()>();
            let blocker = tokio::task::spawn_blocking(move || held.recv());
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(5), executor.run(|| 1)).await;
            release.send(()).unwrap();
            blocker.await.unwrap().unwrap();
            assert_eq!(result.unwrap().unwrap(), 1);
        });
    }
}
