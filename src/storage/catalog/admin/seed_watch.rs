//! The schedule of the pass that replays the seed directory while the server runs, and the board
//! where each pass reports what it found and where a request waits for one.
//!
//! One task runs the passes, one at a time, every interval on the catalog's clock, and at once
//! when a request waits for a pass. It holds only a weak reference to the catalog between passes,
//! and ends once the catalog closes.

use super::replay::ABSENCE_CONFIRMED_AFTER;
use crate::storage::catalog::{Catalog, Inner};
use serde::{Serialize, Serializer};
use std::{
    future::Future,
    sync::{Arc, Weak},
    time::{Duration, SystemTime},
};
use tokio::sync::{Notify, watch};

/// How many problems one pass reports by path; the rest are only counted.
const MAX_PROBLEMS: usize = 100;

/// The board of the seed directory's passes, which the catalog keeps for its lifetime.
#[derive(Debug)]
pub(crate) struct SeedWatch {
    /// Ends the task's wait for a closing catalog.
    wake: Notify,
    state: watch::Sender<Board>,
}

#[derive(Clone, Debug, Default)]
struct Board {
    /// The interval of the running passes; `None` while no task runs them.
    interval: Option<Duration>,
    /// How many passes have started.
    started: u64,
    /// The number of the pass a waiting request needs: the task starts it at once while fewer
    /// passes have started. A count rather than a wakeup, so that a pass the interval started
    /// meanwhile serves the request and no second pass follows for nothing.
    wanted: u64,
    /// The last pass that completed, by the number it started as.
    last: Option<(u64, PassReport)>,
}

impl Default for SeedWatch {
    fn default() -> Self {
        Self {
            wake: Notify::new(),
            state: watch::Sender::new(Board::default()),
        }
    }
}

/// What one pass found and changed.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PassReport {
    #[serde(serialize_with = "timestamp")]
    pub(crate) started_at: SystemTime,
    #[serde(serialize_with = "timestamp")]
    pub(crate) completed_at: SystemTime,
    /// Whether the walk listed the whole tree and ended with its final check of the root. Only
    /// such a pass acts on a proof that a file is gone.
    pub(crate) complete: bool,
    #[serde(flatten)]
    pub(crate) counts: LiveCounts,
    #[serde(flatten)]
    pub(crate) problems: Problems,
    /// Why the pass changed nothing at all, such as a seed directory that went away.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    /// Whether the pass ran, listed the whole tree and left nothing aside.
    clean: bool,
}

impl PassReport {
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "forwards whether the walk was complete, which the walk observed, without selecting a mode"
    )]
    pub(crate) fn new(
        (started_at, completed_at): (SystemTime, SystemTime),
        complete: bool,
        counts: LiveCounts,
        problems: Problems,
        error: Option<String>,
    ) -> Self {
        let clean = complete && error.is_none() && problems.is_empty();
        Self {
            started_at,
            completed_at,
            complete,
            counts,
            problems,
            error,
            clean,
        }
    }
}

/// What a pass changed in the catalog.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LiveCounts {
    /// Buckets created for new top-level directories.
    pub(crate) buckets_created: usize,
    /// Files published as new generations.
    pub(crate) published: usize,
    /// Files read again whose record changed without a publication: the body was what the
    /// catalog or the record already held, or an API write keeps the name.
    pub(crate) recorded: usize,
    /// Objects deleted because their files are gone.
    pub(crate) deleted: usize,
    /// Objects whose files a pass proved gone, waiting for the later proof that deletes them.
    pub(crate) pending_deletions: usize,
    /// Recorded names proven gone where seeding owns nothing to delete. Each is looked up on
    /// every pass until a file returns there, so the count shows that cost.
    pub(crate) tombstones: usize,
    /// Directories whose bucket was deleted after seeding took it, which stay deleted until the
    /// directory is removed and added again, a reseed or a restart.
    pub(crate) buckets_kept_deleted: usize,
}

/// An entry a pass left as it was, or a reason the whole tree was not listed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Problem {
    /// The entry's path below the seed directory; none where the reason concerns the walk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    pub(crate) reason: String,
}

/// The problems of one pass, the first [`MAX_PROBLEMS`] by path and the rest counted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Problems {
    problems: Vec<Problem>,
    problems_omitted: usize,
}

impl Problems {
    pub(crate) fn report(&mut self, path: Option<&str>, reason: &dyn std::fmt::Display) {
        if self.problems.len() < MAX_PROBLEMS {
            self.problems.push(Problem {
                path: path.map(str::to_owned),
                reason: reason.to_string(),
            });
        } else {
            self.problems_omitted += 1;
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.problems.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn listed(&self) -> &[Problem] {
        &self.problems
    }
}

fn timestamp<S: Serializer>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
    let formatted = crate::time_format::timestamp(*time).map_err(serde::ser::Error::custom)?;
    serializer.serialize_str(&formatted)
}

/// What the status route reports.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Status {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    interval_ms: Option<u128>,
    last_pass: Option<PassReport>,
}

/// Why a wait for a pass ended without one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitError {
    /// No task runs passes: none is configured, or the catalog closed.
    Disabled,
    /// The deadline passed first.
    TimedOut,
}

impl SeedWatch {
    pub(crate) fn status(&self) -> Status {
        let board = self.state.borrow();
        Status {
            enabled: board.interval.is_some(),
            interval_ms: board.interval.map(|interval| interval.as_millis()),
            last_pass: board.last.as_ref().map(|(_, report)| report.clone()),
        }
    }

    /// Starts a pass now, and returns the report of the first pass that started after this call
    /// and completed without leaving a deletion waiting for its later proof. A file removed
    /// before the call is then deleted, or reported, by the time the wait returns.
    pub(crate) async fn wait(&self, until: tokio::time::Instant) -> Result<PassReport, WaitError> {
        self.wait_until(until, |report| report.counts.pending_deletions == 0)
            .await
    }

    /// Starts a pass now and returns its report, whatever it left waiting.
    #[cfg(test)]
    pub(crate) async fn next_pass(&self) -> PassReport {
        self.wait_until(crate::test_support::hang_deadline(), |_| true)
            .await
            .unwrap()
    }

    /// Starts a pass now, and returns the report of the first pass that started after this call
    /// and that `settled` accepts.
    async fn wait_until(
        &self,
        until: tokio::time::Instant,
        settled: impl Fn(&PassReport) -> bool,
    ) -> Result<PassReport, WaitError> {
        let mut after = None;
        self.state.send_if_modified(|board| {
            let running = board.interval.is_some();
            if running {
                after = Some(board.started);
                board.wanted = board.wanted.max(board.started + 1);
            }
            running
        });
        let after = after.ok_or(WaitError::Disabled)?;
        let mut board = self.state.subscribe();
        let passed = board.wait_for(|board| {
            board.interval.is_none()
                || board
                    .last
                    .as_ref()
                    .is_some_and(|(number, report)| *number > after && settled(report))
        });
        let board = tokio::time::timeout_at(until, passed)
            .await
            .map_err(|_| WaitError::TimedOut)?
            .map_err(|_| WaitError::Disabled)?;
        match (&board.interval, &board.last) {
            (Some(_), Some((_, report))) => Ok(report.clone()),
            (None, _) | (Some(_), None) => Err(WaitError::Disabled),
        }
    }

    /// Wakes the task, which then sees a closed catalog and ends.
    pub(in crate::storage::catalog) fn wake(&self) {
        self.wake.notify_one();
    }

    /// Waits until a request needs a pass that has not started yet.
    async fn wanted(&self) {
        let mut board = self.state.subscribe();
        // The sender lives in `self`, so the wait ends only when a request needs a pass.
        board
            .wait_for(|board| board.wanted > board.started)
            .await
            .ok();
    }

    fn begin(&self) -> u64 {
        let mut number = 0;
        self.state.send_modify(|board| {
            board.started += 1;
            number = board.started;
        });
        number
    }

    fn finish(&self, number: u64, report: PassReport) {
        self.state
            .send_modify(|board| board.last = Some((number, report)));
    }

    fn stop(&self) {
        self.state.send_modify(|board| board.interval = None);
    }
}

impl Catalog {
    /// The board of the passes over the seed directory.
    pub(crate) fn seed_watch(&self) -> &SeedWatch {
        &self.0.seed_watch
    }

    /// Runs `pass` every `interval` on the catalog's clock until the catalog closes, and at once
    /// when a request waits for a pass. A pass that left a deletion waiting for its later proof
    /// is followed after the wait that proof needs, if that is sooner. A panic restarts the task.
    pub(crate) fn start_seed_watch<P, F>(&self, interval: Duration, pass: P)
    where
        P: Fn(Catalog) -> F + Send + Sync + 'static,
        F: Future<Output = PassReport> + Send + 'static,
    {
        let watch = &self.0.seed_watch;
        let mut started = false;
        watch.state.send_if_modified(|board| {
            started = board.interval.is_none();
            board.interval = Some(interval);
            started
        });
        if !started {
            return;
        }
        let weak = Arc::downgrade(&self.0);
        let pass = Arc::new(pass);
        crate::task::spawn_restarting(
            "seed-watch",
            crate::task::TaskHealth::default(),
            move || {
                open(&weak)?;
                Some(watch_loop(weak.clone(), interval, Arc::clone(&pass)))
            },
        );
    }
}

async fn watch_loop<P, F>(weak: Weak<Inner>, interval: Duration, pass: Arc<P>)
where
    P: Fn(Catalog) -> F + Send + Sync + 'static,
    F: Future<Output = PassReport> + Send + 'static,
{
    // The startup replay just ran, so the first pass waits like any other.
    let mut delay = interval;
    loop {
        let Some(inner) = open(&weak) else {
            return;
        };
        let watch = Arc::clone(&inner.seed_watch);
        let clock = Arc::clone(&inner.clock);
        drop(inner);
        // A request that began waiting while the last pass ran is served at once. A closing
        // catalog's wake leaves a permit, so a close during a pass ends the task at once.
        tokio::select! {
            () = clock.sleep(delay) => {}
            () = watch.wanted() => {}
            () = watch.wake.notified() => {}
        }
        let Some(inner) = open(&weak) else {
            return;
        };
        let number = watch.begin();
        let report = pass(Catalog(inner)).await;
        delay = if report.counts.pending_deletions > 0 {
            interval.min(ABSENCE_CONFIRMED_AFTER)
        } else {
            interval
        };
        watch.finish(number, report);
    }
}

/// The catalog while it is open; once it closed, the board stops taking waits.
fn open(weak: &Weak<Inner>) -> Option<Arc<Inner>> {
    let inner = weak.upgrade()?;
    if inner.admission.is_closed() {
        inner.seed_watch.stop();
        return None;
    }
    Some(inner)
}

#[cfg(test)]
mod tests;
