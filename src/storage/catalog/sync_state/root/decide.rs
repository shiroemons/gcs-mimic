//! The one place that decides how startup takes the selected directory, by the rules
//! [`switch`](super::switch) describes.
//!
//! [`decide`] is pure: it reads the facts [`DiskFacts`] and [`CatalogFacts`] hold, writes
//! nothing and returns what the caller does. A refusal returns before `reserve` writes the
//! catalog and before startup writes a marker.

use super::{
    Activation, RootError, RootId,
    facts::{CatalogFacts, DiskFacts},
    switch,
};
use crate::storage::sync_state::CatalogId;

/// What startup does with the selected directory.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Decision {
    /// Opens the registered root as it is, with its tracked files and its baseline.
    Open { root: RootId, complete: bool },
    /// Builds the baseline of the registered root again, keeping its tracked files.
    Reinitialize(RootId),
    /// Registers the data directory's first root.
    First,
    /// Changes the synchronized directory to the selected one under the root its marker names,
    /// or that `--sync-reinitialize` named, forgetting every registered root and tracked file.
    Rebind {
        reason: switch::Reason,
        root: RootId,
    },
    /// Switches to the selected directory under a new root when it holds only root files.
    Replace,
}

/// Whose marker the selected directory holds.
#[derive(Clone, Copy)]
enum Mark {
    Absent,
    /// This data directory's marker, for the root it names.
    Own(RootId),
    /// Another data directory's.
    Foreign,
}

impl Mark {
    fn of(disk: &DiskFacts<'_>, catalog: CatalogId) -> Self {
        match &disk.marker {
            None => Self::Absent,
            Some(marker) if marker.catalog_id == catalog => Self::Own(marker.root_id),
            Some(_) => Self::Foreign,
        }
    }
}

/// How startup takes the directory `disk` describes, given what `catalog` registers and the
/// `activation` the operator asked for.
pub(super) fn decide(
    disk: &DiskFacts<'_>,
    catalog: &CatalogFacts,
    activation: Activation,
) -> Result<Decision, RootError> {
    let registered = &catalog.registered;
    let at = registered
        .iter()
        .find(|root| root.location == disk.location && root.node == disk.node);
    let near = registered
        .iter()
        .any(|root| root.location == disk.location || root.node == disk.node);
    let alone = registered.len() == 1;
    match (activation, Mark::of(disk, catalog.id), at) {
        (_, Mark::Foreign, _) => Err(RootError::ForeignMarker),
        (Activation::Open, Mark::Own(id), Some(root)) if id == root.root_id && alone => {
            Ok(Decision::Open {
                root: id,
                complete: root.complete,
            })
        }
        (Activation::Open, Mark::Own(id), Some(root)) if id == root.root_id => {
            Ok(Decision::Rebind {
                reason: switch::Reason::SeveralRoots,
                root: id,
            })
        }
        (Activation::Open, Mark::Own(id), _) => Ok(Decision::Rebind {
            reason: switch::Reason::MarkedDirectory,
            root: id,
        }),
        (Activation::Open, Mark::Absent, Some(_)) => Err(RootError::MissingMarker),
        (Activation::Open, Mark::Absent, None) if registered.is_empty() => Ok(Decision::First),
        (Activation::Open, Mark::Absent, None) => Ok(Decision::Replace),
        (Activation::Reinitialize { catalog_id }, Mark::Absent, Some(root))
            if catalog_id == catalog.id && alone =>
        {
            Ok(Decision::Reinitialize(root.root_id))
        }
        (Activation::Reinitialize { catalog_id }, Mark::Absent, Some(root))
            if catalog_id == catalog.id =>
        {
            Ok(Decision::Rebind {
                reason: switch::Reason::SeveralRoots,
                root: root.root_id,
            })
        }
        (Activation::Reinitialize { .. }, Mark::Absent, None) if near => {
            Err(RootError::RootChanged)
        }
        (Activation::Reinitialize { .. }, Mark::Absent | Mark::Own(_), _) => {
            Err(RootError::ReinitializeDenied)
        }
    }
}
