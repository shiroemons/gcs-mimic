//! What startup read before it decides how to take the selected directory.
//!
//! [`decide`](super::decide::decide) takes these values and nothing else, so what it does with a
//! directory never depends on a read it makes itself, and a test builds any combination of the
//! two without a directory or a catalog.

use super::{Marker, switch::Registered};
use crate::{fsutil::NodeId, storage::sync_state::CatalogId};

/// What startup found in the selected directory.
pub(super) struct DiskFacts<'a> {
    /// The canonical path of the selected directory, as the catalog stores a root's location.
    pub(super) location: &'a [u8],
    /// The identity of the directory the lock holds.
    pub(super) node: NodeId,
    /// The marker the directory holds, published or staged, when it holds one that a catalog
    /// may have written. A staged marker that no registered root owns is not one.
    pub(super) marker: Option<Marker>,
}

/// What the catalog records about the synchronized directory.
pub(super) struct CatalogFacts {
    /// The identity of this data directory's catalog.
    pub(super) id: CatalogId,
    /// Every registered root. A catalog of an earlier release may register more than one.
    pub(super) registered: Vec<Registered>,
}
