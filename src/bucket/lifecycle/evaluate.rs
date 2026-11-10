//! Pure lifecycle selection for one live or noncurrent object version.

use super::{LifecycleAction, LifecycleCondition, LifecycleDate, LifecycleRule};
use crate::bucket::StorageClass;
use std::{
    io::Write,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
mod tests;

/// What the sweep knows about one live or noncurrent object version.
/// Soft-deleted versions are never passed in.
pub(crate) struct VersionFacts<'a> {
    pub name: &'a str,
    pub size: u64,
    pub storage_class: StorageClass,
    pub time_created: SystemTime,
    /// Parsed Custom-Time; `None` when unset.
    pub custom_time: Option<SystemTime>,
    /// When the version became noncurrent; `None` for a live version.
    pub noncurrent_time: Option<SystemTime>,
    /// Versions of the same name newer than this one, counting the live one.
    pub newer_versions: u64,
}

/// The single action chosen for a version. Abort rules never select one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlannedAction {
    Delete,
    SetStorageClass(StorageClass),
}

pub(crate) struct Evaluation {
    /// The satisfied action after Delete and lowest-storage-price precedence.
    /// Unsupported or unchanged storage-class transitions are never selected.
    pub action: Option<PlannedAction>,
    /// The earliest instant strictly after `now` when unchanged facts could
    /// select a different action; `None` when time cannot change the outcome.
    pub next_due: Option<SystemTime>,
}

pub(crate) fn evaluate(
    rules: &[LifecycleRule],
    facts: &VersionFacts<'_>,
    now: SystemTime,
) -> Evaluation {
    let candidates = rules.iter().filter_map(|rule| {
        let action = planned_action(&rule.action, facts.storage_class)?;
        let due = eligible_at(&rule.condition, facts, now)?;
        Some((action, due))
    });
    let action = candidates
        .clone()
        .filter(|(_, due)| *due <= now)
        .map(|(action, _)| action)
        .min_by_key(|action| action.priority());
    // Conditions only become true as time advances, so only a higher-priority action can win.
    let next_due = candidates
        .filter(|(candidate, due)| {
            *due > now && action.is_none_or(|current| candidate.priority() < current.priority())
        })
        .map(|(_, due)| due)
        .min();
    Evaluation { action, next_due }
}

impl PlannedAction {
    fn priority(self) -> u8 {
        match self {
            Self::Delete => 0,
            Self::SetStorageClass(class) => match class {
                StorageClass::Archive => 1,
                StorageClass::Coldline => 2,
                StorageClass::Nearline => 3,
                StorageClass::MultiRegional
                | StorageClass::Standard
                | StorageClass::DurableReducedAvailability => 4,
            },
        }
    }
}

fn planned_action(action: &LifecycleAction, current: StorageClass) -> Option<PlannedAction> {
    match action.kind.as_str() {
        "Delete" => Some(PlannedAction::Delete),
        "SetStorageClass" => {
            let target = action.storage_class.as_deref()?.parse().ok()?;
            supported_transition(current, target).then_some(PlannedAction::SetStorageClass(target))
        }
        "AbortIncompleteMultipartUpload" => None,
        _ => None,
    }
}

fn supported_transition(current: StorageClass, target: StorageClass) -> bool {
    use StorageClass::{
        Archive, Coldline, DurableReducedAvailability, MultiRegional, Nearline, Standard,
    };
    match current {
        DurableReducedAvailability => {
            matches!(target, Nearline | Coldline | Archive | MultiRegional)
        }
        Standard | MultiRegional => matches!(target, Nearline | Coldline | Archive),
        Nearline => matches!(target, Coldline | Archive),
        Coldline => target == Archive,
        Archive => false,
    }
}

/// The earliest matching instant at or after `now`, or no instant for unchanged facts.
fn eligible_at(
    condition: &LifecycleCondition,
    facts: &VersionFacts<'_>,
    now: SystemTime,
) -> Option<SystemTime> {
    if !static_conditions_match(condition, facts) {
        return None;
    }
    let mut due = now;
    if let Some(age) = condition.age {
        let deadline = if age == 0 {
            next_midnight(facts.time_created)?
        } else {
            after_days(facts.time_created, age)?
        };
        due = due.max(deadline);
    }
    for (days, origin) in [
        (condition.days_since_custom_time, facts.custom_time),
        (condition.days_since_noncurrent_time, facts.noncurrent_time),
    ] {
        if let Some(days) = days {
            due = due.max(after_days(origin?, days)?);
        }
    }
    Some(due)
}

fn static_conditions_match(condition: &LifecycleCondition, facts: &VersionFacts<'_>) -> bool {
    condition
        .is_live
        .is_none_or(|live| live == facts.noncurrent_time.is_none())
        && condition.num_newer_versions.is_none_or(|count| {
            u64::try_from(count).is_ok_and(|count| facts.newer_versions >= count)
        })
        && before_date(condition.created_before, Some(facts.time_created))
        && before_date(condition.custom_time_before, facts.custom_time)
        && before_date(condition.noncurrent_time_before, facts.noncurrent_time)
        && (condition.matches_prefix.is_empty()
            || condition
                .matches_prefix
                .iter()
                .any(|prefix| facts.name.starts_with(prefix)))
        && (condition.matches_suffix.is_empty()
            || condition
                .matches_suffix
                .iter()
                .any(|suffix| facts.name.ends_with(suffix)))
        && (condition.matches_storage_class.is_empty()
            || condition
                .matches_storage_class
                .iter()
                .any(|class| class == facts.storage_class.as_str()))
        && condition
            .size_above_bytes
            .is_none_or(|size| facts.size > size.bytes())
        && condition
            .size_below_bytes
            .is_none_or(|size| facts.size < size.bytes())
}

fn before_date(date: Option<LifecycleDate>, time: Option<SystemTime>) -> bool {
    date.is_none_or(|date| {
        time.zip(date.midnight())
            .is_some_and(|(time, midnight)| time < midnight)
    })
}

impl LifecycleDate {
    fn midnight(self) -> Option<SystemTime> {
        // Reuse calendar validation on a stack buffer; evaluation must not allocate per version.
        let mut timestamp = [0_u8; 20];
        write!(
            timestamp.as_mut_slice(),
            "{:04}-{:02}-{:02}T00:00:00Z",
            self.year,
            self.month,
            self.day
        )
        .ok()?;
        let timestamp = std::str::from_utf8(&timestamp).ok()?;
        let micros = crate::import::codec::parse_timestamp(timestamp).ok()?;
        crate::clock::system_time_from_unix_seconds(micros / 1_000_000)
    }
}

fn after_days(origin: SystemTime, days: i64) -> Option<SystemTime> {
    let micros = days.checked_mul(86_400_000_000)?;
    origin.checked_add(Duration::from_micros(u64::try_from(micros).ok()?))
}

fn next_midnight(time: SystemTime) -> Option<SystemTime> {
    let remaining = match time.duration_since(UNIX_EPOCH) {
        Ok(since) => Duration::from_secs(86_400 - since.as_secs() % 86_400)
            .checked_sub(Duration::from_nanos(u64::from(since.subsec_nanos())))?,
        Err(error) => {
            let before = error.duration();
            let remainder = Duration::new(before.as_secs() % 86_400, before.subsec_nanos());
            if remainder.is_zero() {
                Duration::from_secs(86_400)
            } else {
                remainder
            }
        }
    };
    time.checked_add(remaining)
}
