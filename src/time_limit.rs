//! Configured time limits, which a setting of zero disables.
//!
//! Timeout settings accept any `u64`, so neither a zero nor a huge value may reach time
//! arithmetic directly: `Duration::ZERO` expires at once instead of disabling a wait, and
//! `Instant + Duration` panics on overflow. Settings become a `TimeLimit` where they are parsed.

use serde::{Serialize, Serializer};
use std::{num::NonZeroU64, time::Duration};
use tokio::time::Instant;

/// How far ahead a disabled or oversized limit ends. It outlives any process, and tokio uses
/// the same span for its own unlimited sleeps, so its timer wheel handles it.
const FAR_FUTURE: Duration = Duration::from_secs(30 * 365 * 24 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeLimit(Option<NonZeroU64>);

impl TimeLimit {
    pub const DISABLED: Self = Self(None);

    pub const fn from_millis(millis: u64) -> Self {
        Self(NonZeroU64::new(millis))
    }

    /// The setting's own form, where zero means disabled.
    pub fn as_millis(self) -> u64 {
        self.0.map_or(0, NonZeroU64::get)
    }

    /// The limit, or `None` when disabled. It stays small enough for code outside this crate
    /// to add it to an `Instant`, as hyper does with its keep-alive settings.
    pub(crate) fn duration(self) -> Option<Duration> {
        self.0
            .map(|millis| Duration::from_millis(millis.get()).min(FAR_FUTURE))
    }

    /// Twice the limit, for a wait that contains a wait of this limit and must leave a full
    /// budget after it. A disabled limit stays disabled.
    pub(crate) fn doubled(self) -> Self {
        Self(self.0.map(|millis| millis.saturating_add(millis.get())))
    }

    /// When a wait starting now must end. A disabled limit lands in the far future, so callers
    /// keep one `Instant` type and `min` or `timeout_at` need no special case.
    pub(crate) fn deadline(self) -> Instant {
        self.deadline_from(Instant::now())
    }

    pub(crate) fn deadline_from(self, start: Instant) -> Instant {
        let ahead = self.duration().unwrap_or(FAR_FUTURE);
        // The representable `Instant` range differs by platform, so the span shrinks until it
        // fits. Adding zero always succeeds, so the fallback only satisfies the type.
        std::iter::successors(Some(ahead), |ahead| (!ahead.is_zero()).then(|| *ahead / 2))
            .find_map(|ahead| start.checked_add(ahead))
            .unwrap_or(start)
    }
}

/// Startup reports limits in the form operators set them.
impl Serialize for TimeLimit {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const YEAR: Duration = Duration::from_secs(365 * 24 * 60 * 60);

    #[test]
    fn zero_disables_the_limit() {
        let limit = TimeLimit::from_millis(0);
        assert_eq!(limit, TimeLimit::DISABLED);
        assert_eq!(limit.duration(), None);
        let start = Instant::now();
        assert!(limit.deadline_from(start) >= start + YEAR);
    }

    #[test]
    fn one_millisecond_stays_a_real_limit() {
        let limit = TimeLimit::from_millis(1);
        assert_eq!(limit.duration(), Some(Duration::from_millis(1)));
        let start = Instant::now();
        assert_eq!(limit.deadline_from(start), start + Duration::from_millis(1));
    }

    #[test]
    fn the_largest_value_neither_panics_nor_expires() {
        let limit = TimeLimit::from_millis(u64::MAX);
        let start = Instant::now();
        assert!(limit.deadline_from(start) >= start + YEAR);
        // hyper computes `Instant + Duration` from this value without a checked add.
        assert!(start.checked_add(limit.duration().unwrap()).is_some());
    }

    #[test]
    fn doubling_keeps_a_disabled_limit_disabled_and_saturates() {
        assert_eq!(TimeLimit::DISABLED.doubled(), TimeLimit::DISABLED);
        assert_eq!(TimeLimit::from_millis(3).doubled().as_millis(), 6);
        assert_eq!(
            TimeLimit::from_millis(u64::MAX).doubled().as_millis(),
            u64::MAX
        );
    }

    #[test]
    fn the_setting_value_survives_the_round_trip() {
        for millis in [0, 1, 30_000, u64::MAX] {
            let limit = TimeLimit::from_millis(millis);
            assert_eq!(limit.as_millis(), millis);
            assert_eq!(serde_json::to_value(limit).unwrap(), millis);
        }
    }

    #[tokio::test]
    async fn a_disabled_limit_works_with_the_tokio_timer() {
        let wait = tokio::time::timeout_at(TimeLimit::DISABLED.deadline(), async { 1 });
        assert_eq!(wait.await.unwrap(), 1);
        let sleep = tokio::time::sleep_until(TimeLimit::from_millis(u64::MAX).deadline());
        tokio::pin!(sleep);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut sleep)
                .await
                .is_err()
        );
    }
}
