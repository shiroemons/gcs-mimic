//! What a wait's answer attaches to its reason, and how much of each list it names.

use crate::storage::sync_state::{findings::alias::FrozenFile, observation::ListedStop};

/// The findings a wait leaves behind without stopping, read with the observation it decided on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WaitFindings {
    /// The objects the API holds whose names the tree cannot.
    pub unmapped: u64,
    /// The observation revision these counts were read at, which binds the findings view.
    pub observation_revision: i64,
    /// For a wait that ended on findings, the first path stops, which its answer names.
    pub stops: Listed<ListedStop>,
    /// For a wait that succeeded, the files that stand for several objects and stay frozen,
    /// which its answer names, since edits to them are not uploaded.
    pub frozen: Listed<FrozenFile>,
}

/// The first items of a listing a wait's answer names, and how many there are in all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed<T> {
    pub first: Vec<T>,
    pub total: u64,
}

impl<T> Default for Listed<T> {
    fn default() -> Self {
        Self {
            first: Vec::new(),
            total: 0,
        }
    }
}

impl<T> Listed<T> {
    /// The items a wait's answer names, at most this many; the rest are only counted.
    pub(crate) const NAMED: usize = 5;

    /// Names the first [`Self::NAMED`] of `items`, of `total` in all.
    pub(crate) fn of(mut items: Vec<T>, total: u64) -> Self {
        items.truncate(Self::NAMED);
        Self {
            first: items,
            total,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.total == 0
    }
}
