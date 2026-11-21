//! The one decision on whether a completed deletion inventory runs on its own or waits for an
//! operator. It reads facts only, so that every way to wait is a row of one table and the
//! catalog transaction that completes the inventory neither decides nor repeats any of it.

use super::{BucketGone, Guard, WaitCause, guarded};
use crate::storage::admin::BucketName;

#[cfg(test)]
mod tests;

/// Whether any member of the set is a file of the set that waited before it, which nobody
/// decided, as `sync_deletion_held` records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeldFiles {
    Present,
    Absent,
}

impl From<bool> for HeldFiles {
    fn from(held: bool) -> Self {
        if held { Self::Present } else { Self::Absent }
    }
}

/// What an inventory saw, which alone decides its set's verdict.
#[derive(Clone)]
pub(crate) struct SetFacts {
    pub(crate) guard: Guard,
    /// Tracked live files the inventory counted.
    pub(crate) baseline: u64,
    /// Those of them seen gone.
    pub(crate) candidates: u64,
    /// Objects the root deleted on its own since an operator last decided, which count on both
    /// sides of the threshold so that split scans cannot stay under it.
    pub(crate) automatic: u64,
    /// The first bucket all of whose tracked files went at once, if one did.
    pub(crate) gone: Option<(BucketName, BucketGone)>,
    pub(crate) held: HeldFiles,
}

/// What becomes of the set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SetVerdict {
    /// The set may delete its members without a decision.
    Run,
    /// The set waits for an operator to approve or reject it, for the first cause that applies.
    Wait(WaitCause),
}

/// Decides `facts`. With the guard off the set always runs. With it on, the causes are tried
/// from the one most likely to be a mistake: a bucket that went whole, whatever the counts; the
/// counts themselves; files of the set that waited before.
pub(crate) fn verdict(facts: SetFacts) -> SetVerdict {
    match facts.guard {
        Guard::Disabled => SetVerdict::Run,
        Guard::Enabled => {
            if let Some((bucket, gone)) = facts.gone {
                return SetVerdict::Wait(WaitCause::Bucket { bucket, gone });
            }
            let automatic = facts.automatic;
            if guarded(
                facts.guard,
                facts.baseline + automatic,
                facts.candidates + automatic,
            ) {
                return SetVerdict::Wait(WaitCause::Threshold);
            }
            match facts.held {
                HeldFiles::Present => SetVerdict::Wait(WaitCause::Held),
                HeldFiles::Absent => SetVerdict::Run,
            }
        }
    }
}
