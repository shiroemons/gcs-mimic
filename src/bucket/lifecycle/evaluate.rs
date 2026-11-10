//! Pure lifecycle selection for one live or noncurrent object version.

use super::LifecycleRule;
use crate::bucket::StorageClass;
use std::time::SystemTime;

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
    _rules: &[LifecycleRule],
    _facts: &VersionFacts<'_>,
    _now: SystemTime,
) -> Evaluation {
    Evaluation {
        action: None,
        next_due: None,
    }
}
