//! A retry delay that doubles after each consecutive failure, shared by the mirror and the export retries.

use std::time::Duration;

/// The retry delay cap, also the run length after which a failure starts the backoff over.
pub(in crate::sync) const RETRY_DELAY_MAX: Duration = Duration::from_secs(30);

/// How many attempts a bounded retry gets in all, the first one included, before its failure
/// stands. One budget for the export steps and the restores keeps a step that fails
/// transiently and a restore whose tree keeps changing under it from trying for different
/// lengths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) struct RetryBudget {
    attempts: u32,
}

impl RetryBudget {
    /// Three attempts. An export step that failed transiently waits 1 s and then 2 s between
    /// them, after which a third failure pauses synchronization; a restore whose tree changed
    /// under its installation plans again from the tree at once.
    pub(in crate::sync) const EXPORT: Self = Self { attempts: 3 };

    /// Whether a step retried `retries` times so far may be retried once more.
    pub(in crate::sync) fn allows_retry(self, retries: u32) -> bool {
        retries.saturating_add(1) < self.attempts
    }

    /// The attempts in all, the first one included.
    pub(in crate::sync) fn attempts(self) -> u32 {
        self.attempts
    }
}

/// A retry delay that doubles after each consecutive failure, up to `max`.
#[derive(Clone, Copy, Debug)]
pub(in crate::sync) struct Backoff {
    first: Duration,
    max: Duration,
    next: Duration,
    retries: u32,
}

impl Backoff {
    pub(in crate::sync) const fn new(first: Duration, max: Duration) -> Self {
        Self {
            first,
            max,
            next: first,
            retries: 0,
        }
    }

    /// The wait before the next retry; the one after it is twice as long, up to `max`.
    pub(in crate::sync) fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = delay.saturating_mul(2).min(self.max);
        self.retries = self.retries.saturating_add(1);
        delay
    }

    /// How many retries this backoff has scheduled.
    pub(in crate::sync) fn retries(&self) -> u32 {
        self.retries
    }

    /// Starts over, after a run that had recovered.
    pub(in crate::sync) fn reset(&mut self) {
        *self = Self::new(self.first, self.max);
    }
}
