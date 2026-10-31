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

/// Stored at microsecond precision; responses display the millisecond truncation.
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
fn truncate_to_millis(time: SystemTime) -> SystemTime {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => {
            SystemTime::UNIX_EPOCH
                + Duration::from_millis(since.as_millis().try_into().unwrap_or(u64::MAX))
        }
        // Times before the epoch are not valid retention times; compare them unchanged.
        Err(_) => time,
    }
}

/// Resolves a Locked configuration echoed with the millisecond time a response displayed.
///
/// When the requested time is exactly the stored time cut to milliseconds, the stored time is
/// kept unchanged, so the echo is neither a shortening nor an overwrite. Cloud Storage was
/// observed (2026-10-31) to refuse the same echo for an Unlocked configuration without
/// `overrideUnlockedRetention`, so Unlocked is compared exactly; the Locked echo was not
/// observed (a Locked lock is irreversible) and keeping it accepted is a local choice. Any other
/// time is returned as requested and is then compared exactly by [`check_transition`].
pub(crate) fn keep_stored_time(
    current: Option<&ObjectRetention>,
    next: Option<ObjectRetention>,
) -> Option<ObjectRetention> {
    match (current, next) {
        (Some(current), Some(next))
            if current.mode == RetentionMode::Locked
                && truncate_to_millis(current.retain_until) == next.retain_until =>
        {
            Some(ObjectRetention {
                mode: next.mode,
                retain_until: current.retain_until,
            })
        }
        (_, next) => next,
    }
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
    // Exact comparison: a Locked time must never move earlier, not even by a microsecond.
    // `keep_stored_time` has already resolved the echo of a displayed millisecond value.
    let shortened = next.is_none_or(|next| next.retain_until < current.retain_until);
    match current.mode {
        RetentionMode::Locked => {
            if shortened || next.is_some_and(|next| next.mode != RetentionMode::Locked) {
                return Err(invalid(
                    "A Locked retention configuration cannot be removed, unlocked or shortened; supply mode Locked with the current or a later retainUntilTime",
                ));
            }
        }
        RetentionMode::Unlocked => {
            let locking = next.is_some_and(|next| next.mode == RetentionMode::Locked);
            if unlocked == UnlockedOverride::NotRequested {
                // Cloud Storage answered 403 for shortening or removing (observed 2026-10-31);
                // locking without the override was not observed and stays a 400.
                if shortened {
                    return Err(ObjectError::UnlockedRetentionOverrideRequired);
                }
                if locking {
                    return Err(invalid(
                        "Locking an Unlocked retention configuration requires overrideUnlockedRetention=true",
                    ));
                }
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
        let resolved = keep_stored_time(Some(current), Some(next));
        check_transition(Some(current), resolved.as_ref(), unlocked)?;
        Ok(resolved)
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
    fn echoing_the_displayed_millisecond_time_keeps_a_locked_microsecond_value() {
        let stored = micros(RetentionMode::Locked, 5_000, 456);
        let kept = write(
            &stored,
            micros(RetentionMode::Locked, 5_000, 0),
            UnlockedOverride::NotRequested,
        )
        .unwrap();
        assert_eq!(kept, Some(stored));
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
