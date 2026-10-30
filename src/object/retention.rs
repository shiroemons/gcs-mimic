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

/// Microsecond precision, matching other catalog timestamps.
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
            if (shortened || locking) && unlocked == UnlockedOverride::NotRequested {
                return Err(invalid(
                    "Removing, shortening or locking an Unlocked retention configuration requires overrideUnlockedRetention=true",
                ));
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
