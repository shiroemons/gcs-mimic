//! Pure lifecycle selection for one live or noncurrent object version.

use super::{
    LifecycleAction, LifecycleCondition, LifecycleDate, LifecycleMatchClass, LifecycleRule,
};
use crate::bucket::StorageClass;
use crate::object::unix_nanos::UnixNanos;
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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Priority {
    Delete,
    Archive,
    Coldline,
    Nearline,
    Legacy,
    // Only evaluate assigns this; PlannedAction::priority cannot inspect a rule's condition.
    ClassDependentDelete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Evaluation {
    /// Selects Delete or a supported transition to a different class; the caller re-evaluates at the same `now`
    /// after applying it, or determines its own next deadline. Act carries no wakeup deadline.
    Act(PlannedAction),
    /// No action is due; any deadline is strictly later than `now` for the unchanged facts.
    Wait(Option<SystemTime>),
}

pub(crate) fn evaluate<'a>(
    rules: impl IntoIterator<Item = &'a LifecycleRule>,
    facts: &VersionFacts<'_>,
    now: SystemTime,
) -> Evaluation {
    let mut selected: Option<(Priority, PlannedAction)> = None;
    let mut next_due = None;
    for rule in rules {
        let Some(action) = planned_action(rule.action, facts.storage_class) else {
            continue;
        };
        let Some(due) = eligible_at(&rule.condition, facts, now) else {
            continue;
        };
        let priority = match action {
            // Current metadata cannot distinguish a delayed transition from the intended class history.
            // Re-evaluation after the transition still applies a Delete matching the resulting class.
            PlannedAction::Delete if !rule.condition.matches_storage_class.is_empty() => {
                Priority::ClassDependentDelete
            }
            PlannedAction::Delete | PlannedAction::SetStorageClass(_) => action.priority(),
        };
        if due > now {
            next_due = Some(next_due.map_or(due, |next: SystemTime| next.min(due)));
        } else if selected.is_none_or(|(current, _)| priority < current) {
            selected = Some((priority, action));
        }
    }
    selected.map_or(Evaluation::Wait(next_due), |(_, action)| {
        Evaluation::Act(action)
    })
}

impl PlannedAction {
    fn priority(self) -> Priority {
        match self {
            Self::Delete => Priority::Delete,
            Self::SetStorageClass(class) => match class {
                StorageClass::Archive => Priority::Archive,
                StorageClass::Coldline => Priority::Coldline,
                StorageClass::Nearline => Priority::Nearline,
                StorageClass::MultiRegional
                | StorageClass::Standard
                | StorageClass::DurableReducedAvailability => Priority::Legacy,
            },
        }
    }
}

fn planned_action(action: LifecycleAction, current: StorageClass) -> Option<PlannedAction> {
    match action {
        LifecycleAction::Delete => Some(PlannedAction::Delete),
        LifecycleAction::SetStorageClass(target) => {
            supported_transition(current, target).then_some(PlannedAction::SetStorageClass(target))
        }
        LifecycleAction::AbortIncompleteMultipartUpload => None,
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
                .contains(&LifecycleMatchClass::Supported(facts.storage_class)))
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
    let deadline = origin.checked_add(Duration::from_micros(u64::try_from(micros).ok()?))?;
    catalog_deadline(deadline)
}

fn catalog_deadline(deadline: SystemTime) -> Option<SystemTime> {
    let timestamp = UnixNanos::from(deadline);
    let micros =
        i128::from(timestamp.secs()) * 1_000_000 + i128::from(timestamp.subsec_nanos() / 1000);
    i64::try_from(micros).ok().map(|_| deadline)
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
    catalog_deadline(time.checked_add(remaining)?)
}
