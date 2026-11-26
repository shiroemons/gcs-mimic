//! The schedule of the pass that replays the seed directory while the server runs, and the board
//! where each pass reports what it found and where a request waits for one.
//!
//! One task runs the passes, one at a time, every interval on the catalog's clock, and at once
//! when a request waits for a pass. It holds only a weak reference to the catalog between passes,
//! and ends once the catalog closes.

use super::{LiveOutcome, replay::ABSENCE_CONFIRMED_AFTER};
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
    schedule: Schedule,
    /// How many passes have started.
    started: u64,
    /// The number of the pass a waiting request needs: the task starts it at once while fewer
    /// passes have started. A count rather than a wakeup, so that a pass the interval started
    /// meanwhile serves the request and no second pass follows for nothing.
    wanted: u64,
    /// The last pass that completed, by the number it started as.
    last: Option<(u64, PassReport)>,
}

/// Whether a task runs the passes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Schedule {
    /// None runs: the configuration names no seed directory or no interval.
    #[default]
    Off,
    /// A task runs one pass every interval.
    Every(Duration),
    /// The catalog closed, and the task ended or is ending.
    Closed,
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
    started_at: Timestamp,
    completed_at: Timestamp,
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
    /// The report of a pass that ran from `started_at` to `completed_at` and committed `outcome`,
    /// or could not run for the reason `outcome` gives, which changed nothing.
    pub(crate) fn new(
        (started_at, completed_at): (SystemTime, SystemTime),
        outcome: Result<LiveOutcome, String>,
    ) -> Self {
        let (outcome, error) = match outcome {
            Ok(outcome) => (outcome, None),
            Err(error) => (LiveOutcome::default(), Some(error)),
        };
        let LiveOutcome {
            complete,
            counts,
            problems,
        } = outcome;
        let clean = complete && error.is_none() && problems.is_empty();
        Self {
            started_at: Timestamp(started_at),
            completed_at: Timestamp(completed_at),
            complete,
            counts,
            problems,
            error,
            clean,
        }
    }
}

/// A time in a report, written in RFC 3339 as the JSON API writes its times.
#[derive(Clone, Copy, Debug)]
struct Timestamp(SystemTime);

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let formatted = crate::time_format::timestamp(self.0).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&formatted)
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
    /// Directories whose bucket was deleted, or created again, after seeding took it, which
    /// passes leave alone until a reseed or a restart.
    pub(crate) buckets_kept_deleted: usize,
}

impl LiveCounts {
    /// Whether the pass left a proof of absence waiting for the later proof that confirms it.
    pub(crate) fn waiting(&self) -> bool {
        self.pending_deletions > 0
    }
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
    /// No pass runs while the server runs: the configuration names no seed directory or no
    /// interval, which a request cannot change.
    NotConfigured,
    /// The catalog closed, so no further pass runs.
    Closed,
    /// The deadline passed first.
    TimedOut,
}

impl SeedWatch {
    pub(crate) fn status(&self) -> Status {
        let board = self.state.borrow();
        let interval = match board.schedule {
            Schedule::Every(interval) => Some(interval),
            Schedule::Off | Schedule::Closed => None,
        };
        Status {
            enabled: interval.is_some(),
            interval_ms: interval.map(|interval| interval.as_millis()),
            last_pass: board.last.as_ref().map(|(_, report)| report.clone()),
        }
    }

    /// Starts a pass now, and returns the report of the first pass that started after this call
    /// and completed without leaving a proof of absence waiting for its later proof. A file or a
    /// directory removed before the call is then dealt with, or reported, by the time the wait
    /// returns.
    pub(crate) async fn wait(&self, until: tokio::time::Instant) -> Result<PassReport, WaitError> {
        self.wait_until(until, |report| !report.counts.waiting())
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
        let mut after = Err(WaitError::NotConfigured);
        self.state.send_if_modified(|board| match board.schedule {
            Schedule::Every(_) => {
                after = Ok(board.started);
                board.wanted = board.wanted.max(board.started + 1);
                true
            }
            Schedule::Off => false,
            Schedule::Closed => {
                after = Err(WaitError::Closed);
                false
            }
        });
        let after = after?;
        let mut board = self.state.subscribe();
        let passed = board.wait_for(|board| {
            board.schedule == Schedule::Closed
                || board
                    .last
                    .as_ref()
                    .is_some_and(|(number, report)| *number > after && settled(report))
        });
        // The sender lives in `self`, so the channel stays open while the wait borrows it; a
        // closed one could only mean that no further pass runs.
        let board = tokio::time::timeout_at(until, passed)
            .await
            .map_err(|_| WaitError::TimedOut)?
            .map_err(|_| WaitError::Closed)?;
        match (board.schedule, &board.last) {
            (Schedule::Every(_), Some((_, report))) => Ok(report.clone()),
            // A running schedule changes only to closed, and the wait ended on a report.
            (Schedule::Off | Schedule::Closed, _) | (Schedule::Every(_), None) => {
                Err(WaitError::Closed)
            }
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
        self.state
            .send_modify(|board| board.schedule = Schedule::Closed);
    }
}

impl Catalog {
    /// The board of the passes over the seed directory.
    pub(crate) fn seed_watch(&self) -> &SeedWatch {
        &self.0.admin.seed_watch
    }

    /// Runs `pass` every `interval` on the catalog's clock until the catalog closes, and at once
    /// when a request waits for a pass, with the number the pass started as. A pass that left a
    /// proof of absence waiting for its later proof is followed after the wait that proof needs,
    /// if that is sooner. A panic restarts the task, which readiness reports down until a pass of
    /// the replacement completes.
    pub(crate) fn start_seed_watch<P, F>(&self, interval: Duration, pass: P)
    where
        P: Fn(Catalog, u64) -> F + Send + Sync + 'static,
        F: Future<Output = PassReport> + Send + 'static,
    {
        let watch = &self.0.admin.seed_watch;
        let mut started = false;
        watch.state.send_if_modified(|board| {
            started = board.schedule == Schedule::Off;
            if started {
                board.schedule = Schedule::Every(interval);
            }
            started
        });
        if !started {
            return;
        }
        let weak = Arc::downgrade(&self.0);
        let passes = Passes {
            interval,
            run: Arc::new(pass),
            health: crate::task::TaskHealth::default(),
        };
        // A panic during a pass leaves the fixture tree only partly applied while the server
        // keeps serving it, and a request may wait for the pass the panic ended, so readiness
        // reports the gap as it does for the other background loops.
        self.watch_background_task(passes.health.clone());
        let mut restarted = false;
        crate::task::spawn_restarting("seed-watch", passes.health.clone(), move || {
            open(&weak)?;
            // The startup replay just ran, so the first run waits an interval like any later
            // pass; a replacement passes at once to close the gap.
            let delay = if std::mem::replace(&mut restarted, true) {
                Duration::ZERO
            } else {
                interval
            };
            Some(watch_loop(weak.clone(), passes.clone(), delay))
        });
    }
}

/// What a run of the task needs to run the passes.
struct Passes<P> {
    interval: Duration,
    run: Arc<P>,
    health: crate::task::TaskHealth,
}

impl<P> Clone for Passes<P> {
    fn clone(&self) -> Self {
        Self {
            interval: self.interval,
            run: Arc::clone(&self.run),
            health: self.health.clone(),
        }
    }
}

/// Runs the passes, the first after `delay`.
async fn watch_loop<P, F>(weak: Weak<Inner>, passes: Passes<P>, mut delay: Duration)
where
    P: Fn(Catalog, u64) -> F + Send + Sync + 'static,
    F: Future<Output = PassReport> + Send + 'static,
{
    let Passes {
        interval,
        run: pass,
        health,
    } = passes;
    loop {
        let Some(inner) = open(&weak) else {
            return;
        };
        let watch = Arc::clone(&inner.admin.seed_watch);
        let clock = Arc::clone(&inner.policy.clock);
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
        let report = pass(Catalog(inner), number).await;
        // A pass that met an error still ran to its end and reported it: an unreadable tree or a
        // failing catalog is the report's to tell, while readiness tells whether passes run.
        health.pass_succeeded();
        delay = if report.counts.waiting() {
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
    if inner.admission.slots.is_closed() {
        inner.admin.seed_watch.stop();
        return None;
    }
    Some(inner)
}

#[cfg(test)]
mod tests;
