//! Per-object retention (Object Retention Lock) state shared by every protocol adapter.
//!
//! Sources checked 2026-10-29: the Object Retention Lock overview and usage guides, the
//! objects resource/insert/patch references and Discovery revision 20261020. Retention is
//! independent of the bucket retention policy: an object is protected until both are met.

use super::{ObjectError, ObjectRecord};
use std::time::{Duration, SystemTime};

/// The official guide caps retain-until at 3,155,760,000 seconds (100 years) from now.
pub(crate) const MAX_RETENTION: Duration = Duration::from_secs(3_155_760_000);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum RetentionMode {
    Unlocked,
    Locked,
}

impl RetentionMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unlocked => "Unlocked",
            Self::Locked => "Locked",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "Unlocked" => Some(Self::Unlocked),
            "Locked" => Some(Self::Locked),
            _ => None,
        }
    }
}

/// Held to the nanosecond, as Cloud Storage compared it; responses display the millisecond
/// truncation. The catalog stores whole microseconds plus a sub-microsecond remainder column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ObjectRetention {
    pub mode: RetentionMode,
    pub retain_until: SystemTime,
}

impl ObjectRetention {
    pub(crate) fn active(&self, now: SystemTime) -> bool {
        now < self.retain_until
    }
}

/// Drops the sub-millisecond part, the precision JSON responses display.
pub(crate) fn truncate_to_millis(time: SystemTime) -> SystemTime {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => {
            SystemTime::UNIX_EPOCH
                + Duration::from_millis(since.as_millis().try_into().unwrap_or(u64::MAX))
        }
        // Times before the epoch are not valid retention times; compare them unchanged.
        Err(_) => time,
    }
}

/// Formats a time as RFC 3339 in US Pacific time with the fraction trimmed of trailing zeros,
/// the form Cloud Storage used in the retained-object denial (observed 2026-10-31:
/// `…T10:03:59.123456793-07:00` for 17:03:59 UTC while daylight time applied). Google's
/// frontends use the `America/Los_Angeles` zone, so the offset follows the US rule in force
/// since 2007 (second Sunday of March 02:00 to first Sunday of November 02:00); earlier years
/// use the same rule, which is an inference, not an observation; so is trimming the fraction
/// when it is zero or ends in zeros (only nine-digit fractions were seen).
pub(crate) fn pacific_rfc3339(time: SystemTime) -> String {
    let since = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = i64::try_from(since.as_secs()).unwrap_or(i64::MAX / 2);
    let offset_hours = if pacific_daylight_time(seconds) { 7 } else { 8 };
    let local = seconds - offset_hours * 3600;
    let days = local.div_euclid(86_400);
    let day_seconds = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let mut fraction = format!("{:09}", since.subsec_nanos());
    while fraction.ends_with('0') {
        fraction.pop();
    }
    let fraction = if fraction.is_empty() {
        String::new()
    } else {
        format!(".{fraction}")
    };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{fraction}-{offset_hours:02}:00",
        day_seconds / 3600,
        day_seconds / 60 % 60,
        day_seconds % 60
    )
}

fn pacific_daylight_time(utc_seconds: i64) -> bool {
    let (year, _, _) = civil_from_days((utc_seconds - 8 * 3600).div_euclid(86_400));
    let nth_sunday = |month: u32, nth: i64| {
        let first = days_from_civil(year, month, 1);
        let weekday = (first + 4).rem_euclid(7);
        first + (7 - weekday) % 7 + 7 * (nth - 1)
    };
    // 02:00 PST is 10:00 UTC; 02:00 PDT is 09:00 UTC.
    let start = nth_sunday(3, 2) * 86_400 + 10 * 3600;
    let end = nth_sunday(11, 1) * 86_400 + 9 * 3600;
    (start..end).contains(&utc_seconds)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_from_march + 2) / 5 + 1) as u32;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn invalid(message: &str) -> ObjectError {
    ObjectError::InvalidFixedMetadata(message.into())
}

/// Whether the bucket was created with object retention enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BucketRetention {
    Enabled,
    Disabled,
}

/// Whether an object update set `overrideUnlockedRetention=true`. The server checks the
/// override permission before the catalog applies a requested override.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum UnlockedOverride {
    Requested,
    #[default]
    NotRequested,
}

/// Validates a configuration that a publication or metadata change would store.
///
/// The event-based-hold conflict and the 100-year cap come from the official guide.
/// Requiring the bucket feature is the only way the provider accepts any configuration.
pub(crate) fn validate_stored(
    record: &ObjectRecord,
    bucket: BucketRetention,
    now: SystemTime,
) -> Result<(), ObjectError> {
    let Some(retention) = &record.retention else {
        return Ok(());
    };
    if bucket == BucketRetention::Disabled {
        return Err(invalid(
            "Object retention is not enabled for this bucket; create the bucket with enableObjectRetention=true before setting retention",
        ));
    }
    if record.event_based_hold {
        return Err(invalid(
            "A retention configuration cannot be set on an object subject to an event-based hold; release eventBasedHold first",
        ));
    }
    if now
        .checked_add(MAX_RETENTION)
        .is_none_or(|limit| retention.retain_until > limit)
    {
        return Err(invalid(
            "retention.retainUntilTime must be at most 3155760000 seconds (100 years) in the future",
        ));
    }
    Ok(())
}

/// Checks a metadata change from `current` to `next`.
///
/// Locked configurations only accept an equal or later retain-until time in Locked mode.
/// Unlocked configurations need `overrideUnlockedRetention=true` to lock, shorten or remove.
pub(crate) fn check_transition(
    current: Option<&ObjectRetention>,
    next: Option<&ObjectRetention>,
    unlocked: UnlockedOverride,
) -> Result<(), ObjectError> {
    let Some(current) = current else {
        return Ok(());
    };
    // Exact comparison at nanosecond precision: the displayed millisecond value of a stored
    // time is a shortening, for Locked and Unlocked alike (observed 2026-10-31).
    let shortened = next.is_none_or(|next| next.retain_until < current.retain_until);
    match current.mode {
        RetentionMode::Locked => {
            // The override never applies to a Locked configuration.
            if shortened || next.is_some_and(|next| next.mode != RetentionMode::Locked) {
                return Err(ObjectError::LockedRetention);
            }
        }
        RetentionMode::Unlocked => {
            let locking = next.is_some_and(|next| next.mode == RetentionMode::Locked);
            if (shortened || locking) && unlocked == UnlockedOverride::NotRequested {
                return Err(ObjectError::UnlockedRetentionOverrideRequired);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn config(mode: RetentionMode, seconds: u64) -> ObjectRetention {
        ObjectRetention {
            mode,
            retain_until: UNIX_EPOCH + Duration::from_secs(seconds),
        }
    }

    #[test]
    fn object_retention_transitions_follow_mode_rules() {
        use RetentionMode::{Locked, Unlocked};
        let unlocked = config(Unlocked, 100);
        let locked = config(Locked, 100);
        // Any first configuration is allowed; permissions and bucket state are checked elsewhere.
        assert!(check_transition(None, Some(&locked), UnlockedOverride::NotRequested).is_ok());
        assert!(check_transition(None, None, UnlockedOverride::NotRequested).is_ok());
        for (next, allowed_without_override) in [
            (Some(config(Unlocked, 100)), true),
            (Some(config(Unlocked, 200)), true),
            (Some(config(Unlocked, 99)), false),
            (Some(config(Locked, 200)), false),
            (None, false),
        ] {
            assert_eq!(
                check_transition(
                    Some(&unlocked),
                    next.as_ref(),
                    UnlockedOverride::NotRequested
                )
                .is_ok(),
                allowed_without_override,
                "{next:?}"
            );
            assert!(
                check_transition(Some(&unlocked), next.as_ref(), UnlockedOverride::Requested)
                    .is_ok()
            );
        }
        for (next, allowed) in [
            (Some(config(Locked, 100)), true),
            (Some(config(Locked, 200)), true),
            (Some(config(Locked, 99)), false),
            (Some(config(Unlocked, 200)), false),
            (None, false),
        ] {
            for unlocked in [UnlockedOverride::NotRequested, UnlockedOverride::Requested] {
                assert_eq!(
                    check_transition(Some(&locked), next.as_ref(), unlocked).is_ok(),
                    allowed,
                    "{next:?}"
                );
            }
        }
    }

    /// Applies a write the way the catalog does: resolve the echo, then check the transition.
    fn write(
        current: &ObjectRetention,
        next: ObjectRetention,
        unlocked: UnlockedOverride,
    ) -> Result<Option<ObjectRetention>, ObjectError> {
        check_transition(Some(current), Some(&next), unlocked)?;
        Ok(Some(next))
    }

    #[test]
    fn pacific_time_follows_daylight_saving_and_trims_the_fraction() {
        let at = |seconds: u64, nanos: u32| UNIX_EPOCH + Duration::new(seconds, nanos);
        // 2026-10-31T17:03:59.123456793Z, the instant Cloud Storage printed with -07:00.
        assert_eq!(
            pacific_rfc3339(at(1_793_466_239, 123_456_793)),
            "2026-10-31T10:03:59.123456793-07:00"
        );
        assert_eq!(
            pacific_rfc3339(at(1_793_466_239, 123_000_000)),
            "2026-10-31T10:03:59.123-07:00"
        );
        assert_eq!(
            pacific_rfc3339(at(1_793_466_239, 0)),
            "2026-10-31T10:03:59-07:00"
        );
        // Daylight time ends 2026-11-01 02:00 PDT = 09:00 UTC.
        assert_eq!(
            pacific_rfc3339(at(1_793_523_599, 0)),
            "2026-11-01T01:59:59-07:00"
        );
        assert_eq!(
            pacific_rfc3339(at(1_793_523_600, 0)),
            "2026-11-01T01:00:00-08:00"
        );
        // Daylight time starts 2026-03-08 02:00 PST = 10:00 UTC.
        assert_eq!(
            pacific_rfc3339(at(1_772_964_000 - 1, 0)),
            "2026-03-08T01:59:59-08:00"
        );
        assert_eq!(
            pacific_rfc3339(at(1_772_964_000, 0)),
            "2026-03-08T03:00:00-07:00"
        );
    }

    /// A retention whose stored time carries microseconds beyond its displayed millisecond.
    fn micros(mode: RetentionMode, millis: u64, extra_micros: u64) -> ObjectRetention {
        ObjectRetention {
            mode,
            retain_until: UNIX_EPOCH
                + Duration::from_millis(millis)
                + Duration::from_micros(extra_micros),
        }
    }

    #[test]
    fn echoing_the_displayed_millisecond_time_of_a_locked_value_is_refused_even_with_the_override()
    {
        let stored = micros(RetentionMode::Locked, 5_000, 456);
        for unlocked in [UnlockedOverride::NotRequested, UnlockedOverride::Requested] {
            assert_eq!(
                write(&stored, micros(RetentionMode::Locked, 5_000, 0), unlocked),
                Err(ObjectError::LockedRetention)
            );
            // The same value is not a change, and a later one extends.
            assert_eq!(write(&stored, stored, unlocked), Ok(Some(stored)));
        }
    }

    /// Cloud Storage refused the displayed millisecond time of an Unlocked configuration whose
    /// stored time had a finer fraction, unless the override was requested (observed 2026-10-31).
    #[test]
    fn echoing_the_displayed_millisecond_time_of_an_unlocked_value_is_a_shortening() {
        let stored = micros(RetentionMode::Unlocked, 5_000, 456);
        let echo = micros(RetentionMode::Unlocked, 5_000, 0);
        assert!(write(&stored, echo, UnlockedOverride::NotRequested).is_err());
        assert_eq!(
            write(&stored, echo, UnlockedOverride::Requested).unwrap(),
            Some(echo)
        );
    }

    #[test]
    fn a_time_even_one_millisecond_shorter_is_refused() {
        let locked = micros(RetentionMode::Locked, 5_000, 456);
        let unlocked = micros(RetentionMode::Unlocked, 5_000, 456);
        for next in [4_999, 1_000] {
            assert!(
                write(
                    &locked,
                    micros(RetentionMode::Locked, next, 0),
                    UnlockedOverride::Requested
                )
                .is_err()
            );
            let shorter = micros(RetentionMode::Unlocked, next, 0);
            assert!(write(&unlocked, shorter, UnlockedOverride::NotRequested).is_err());
            assert!(write(&unlocked, shorter, UnlockedOverride::Requested).is_ok());
        }
    }

    #[test]
    fn a_longer_time_is_accepted_and_stored_as_requested() {
        for mode in [RetentionMode::Locked, RetentionMode::Unlocked] {
            let stored = micros(mode, 5_000, 456);
            let longer = micros(mode, 5_001, 0);
            assert_eq!(
                write(&stored, longer, UnlockedOverride::NotRequested).unwrap(),
                Some(longer)
            );
        }
    }

    #[test]
    fn retention_mode_names_are_exact() {
        for mode in [RetentionMode::Unlocked, RetentionMode::Locked] {
            assert_eq!(RetentionMode::parse(mode.as_str()), Some(mode));
        }
        for invalid in ["unlocked", "LOCKED", "GOVERNANCE", "COMPLIANCE", ""] {
            assert_eq!(RetentionMode::parse(invalid), None);
        }
    }
}
