//! A point in time held as whole seconds and nanoseconds since the Unix epoch.
//!
//! Object retention is held and compared to the nanosecond, as Cloud Storage does. A
//! `SystemTime` cannot carry that on every platform: Windows counts 100 ns intervals and floors
//! any finer value, so two retention times a nanosecond apart would collapse into one. This
//! value type has the same resolution everywhere, and its derived order is the time order
//! because `nanos` is always below one second.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NANOS_PER_SECOND: u32 = 1_000_000_000;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "RawUnixNanos")]
pub(crate) struct UnixNanos {
    secs: i64,
    nanos: u32,
}

/// The serialized form, checked on the way in so a decoded value keeps `nanos < 1e9`.
#[derive(serde::Deserialize)]
struct RawUnixNanos {
    secs: i64,
    nanos: u32,
}

impl TryFrom<RawUnixNanos> for UnixNanos {
    type Error = &'static str;

    fn try_from(raw: RawUnixNanos) -> Result<Self, Self::Error> {
        Self::new(raw.secs, raw.nanos).ok_or("nanos must be below one second")
    }
}

impl UnixNanos {
    /// `None` when `nanos` is not below one second, which no time can have.
    pub(crate) fn new(secs: i64, nanos: u32) -> Option<Self> {
        (nanos < NANOS_PER_SECOND).then_some(Self { secs, nanos })
    }

    /// Whole microseconds since the epoch (the catalog column) plus the 0 to 999 nanoseconds
    /// past them. `None` for a remainder of a microsecond or more.
    pub(crate) fn from_micros_and_remainder(micros: i64, remainder_nanos: u32) -> Option<Self> {
        if remainder_nanos >= 1000 {
            return None;
        }
        let nanos = u32::try_from(micros.rem_euclid(1_000_000)).ok()? * 1000 + remainder_nanos;
        Self::new(micros.div_euclid(1_000_000), nanos)
    }

    pub(crate) fn secs(self) -> i64 {
        self.secs
    }

    pub(crate) fn subsec_nanos(self) -> u32 {
        self.nanos
    }

    /// Whole microseconds since the epoch, flooring, and the nanoseconds past them. `None`
    /// when the microsecond count does not fit an `i64`.
    pub(crate) fn micros_and_remainder(self) -> Option<(i64, u32)> {
        let micros = self
            .secs
            .checked_mul(1_000_000)?
            .checked_add(i64::from(self.nanos / 1000))?;
        Some((micros, self.nanos % 1000))
    }

    /// Drops the sub-millisecond part, the precision JSON responses display.
    pub(crate) fn floor_to_millis(self) -> Self {
        Self {
            secs: self.secs,
            nanos: self.nanos / 1_000_000 * 1_000_000,
        }
    }

    pub(crate) fn checked_add(self, duration: Duration) -> Option<Self> {
        let nanos = u64::from(self.nanos) + u64::from(duration.subsec_nanos());
        let carry = i64::from(nanos >= u64::from(NANOS_PER_SECOND));
        let secs = self
            .secs
            .checked_add(i64::try_from(duration.as_secs()).ok()?)?
            .checked_add(carry)?;
        Some(Self {
            secs,
            nanos: u32::try_from(nanos % u64::from(NANOS_PER_SECOND)).ok()?,
        })
    }
}

/// No precision is lost converting a `SystemTime` (its own resolution depends on the OS, up to
/// a nanosecond), so clock readings compare with retention times at full precision on every
/// platform. Only a time beyond the `i64` second range saturates.
impl From<SystemTime> for UnixNanos {
    fn from(time: SystemTime) -> Self {
        match time.duration_since(UNIX_EPOCH) {
            Ok(since) => Self {
                secs: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
                nanos: since.subsec_nanos(),
            },
            Err(error) => {
                let before = error.duration();
                let (secs, nanos) = match (before.as_secs(), before.subsec_nanos()) {
                    (secs, 0) => (i64::try_from(secs).map(|s| -s), 0),
                    (secs, sub) => (i64::try_from(secs + 1).map(|s| -s), NANOS_PER_SECOND - sub),
                };
                Self {
                    secs: secs.unwrap_or(i64::MIN),
                    nanos,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_by_time_including_before_the_epoch() {
        let at = |secs, nanos| UnixNanos::new(secs, nanos).unwrap();
        assert!(at(5, 123_456_788) < at(5, 123_456_789));
        assert!(at(5, 999_999_999) < at(6, 0));
        assert!(at(-1, 500_000_000) < at(0, 0));
        assert!(UnixNanos::new(0, NANOS_PER_SECOND).is_none());
    }

    #[test]
    fn keeps_nanoseconds_that_are_not_a_multiple_of_100() {
        // Windows floors a SystemTime to 100 ns, so this must never pass through one.
        let a = UnixNanos::new(1_900_000_000, 123_456_788).unwrap();
        let b = UnixNanos::new(1_900_000_000, 123_456_789).unwrap();
        assert_ne!(a, b);
        assert_eq!(b.subsec_nanos(), 123_456_789);
        assert_eq!(b.micros_and_remainder(), Some((1_900_000_000_123_456, 789)));
        assert_eq!(
            UnixNanos::from_micros_and_remainder(1_900_000_000_123_456, 789),
            Some(b)
        );
        assert_eq!(b.floor_to_millis().subsec_nanos(), 123_000_000);
    }

    #[test]
    fn micros_round_trip_before_the_epoch() {
        let time = UnixNanos::from_micros_and_remainder(-1_500_001, 7).unwrap();
        assert_eq!((time.secs(), time.subsec_nanos()), (-2, 499_999_007));
        assert_eq!(time.micros_and_remainder(), Some((-1_500_001, 7)));
        assert!(UnixNanos::from_micros_and_remainder(0, 1000).is_none());
    }

    #[test]
    fn converts_a_system_time_without_loss() {
        let after = UNIX_EPOCH + Duration::new(10, 999_999_900);
        assert_eq!(
            UnixNanos::from(after),
            UnixNanos::new(10, 999_999_900).unwrap()
        );
        let before = UNIX_EPOCH - Duration::new(1, 250_000_000);
        assert_eq!(
            UnixNanos::from(before),
            UnixNanos::new(-2, 750_000_000).unwrap()
        );
        assert_eq!(
            UnixNanos::from(UNIX_EPOCH - Duration::from_secs(3)),
            UnixNanos::new(-3, 0).unwrap()
        );
    }

    #[test]
    fn deserializing_rejects_a_fraction_of_a_second_or_more() {
        let ok: UnixNanos = serde_json::from_str(r#"{"secs":5,"nanos":999999999}"#).unwrap();
        assert_eq!(ok, UnixNanos::new(5, 999_999_999).unwrap());
        assert!(serde_json::from_str::<UnixNanos>(r#"{"secs":5,"nanos":1000000000}"#).is_err());
    }

    #[test]
    fn adds_a_duration_with_carry() {
        let time = UnixNanos::new(1, 900_000_000).unwrap();
        assert_eq!(
            time.checked_add(Duration::new(2, 200_000_000)),
            UnixNanos::new(4, 100_000_000)
        );
        assert_eq!(
            UnixNanos::new(i64::MAX, 999_999_999)
                .unwrap()
                .checked_add(Duration::from_nanos(1)),
            None
        );
    }
}
