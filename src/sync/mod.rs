#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Filesystem synchronization adapters and shared reversible name mapping.

pub(crate) mod control;
pub use control::{Decision, UnknownDecision};
mod backoff;
pub(crate) mod correspond;
mod deletion;
pub(crate) mod entries;
mod error;
mod export;
mod failure;
pub mod handshake;
mod ingest;
mod interrupt;
pub mod mapping;
mod mirror;
pub(crate) mod naming;
mod reevaluate;
pub mod report;
pub use report::{
    outcome::{Outcome, UnknownWaitReason, Unsettled, WaitReason},
    verdict::UnmappedObjects,
};
mod status;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) mod test_support;
pub(crate) mod tree;
pub(crate) use deletion::Missing;
pub(crate) use error::{Cause, Pass, SyncError};
pub use export::reconcile::reconcile;
pub use mirror::{Reconciliation, Task, start};
use status::EntryReportSender;
pub use status::{MirrorState, MirrorStatus, UnknownMirrorState};

#[cfg(test)]
struct Hook {
    phase: &'static str,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

pub(crate) const SETTLE_MS: u64 = 1000;
pub(crate) const CONFLICT_MAX_BYTES: u64 = crate::storage::sync_state::conflicts::MAX_BYTES;
pub(crate) const CONFLICT_MAX_ENTRIES: u64 = crate::storage::sync_state::conflicts::MAX_ENTRIES;

use crate::{
    config::Config,
    fsutil,
    storage::{
        Catalog,
        failure::BackendCause,
        sync_state::{
            CatalogId,
            root::{Activation, RootError, RootLease},
        },
    },
};
use std::{io, path::Path};

/// A `sync_reinitialize` value that is not a lowercase 32-digit hexadecimal catalog ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidCatalogId;

impl std::fmt::Display for InvalidCatalogId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("sync_reinitialize requires the exact lowercase 32-digit catalog ID; inspect the root marker or startup diagnostic")
    }
}

impl std::error::Error for InvalidCatalogId {}

pub(crate) fn parse_catalog_id(value: &str) -> Result<CatalogId, InvalidCatalogId> {
    value.parse().map_err(|_| InvalidCatalogId)
}

pub(crate) fn validate_directory(root: &Path, data: &Path, seed: Option<&Path>) -> io::Result<()> {
    fsutil::open_directory_nofollow(root).and_then(|root| fsutil::directory_writable(&root)).map_err(|error| {
        io::Error::other(if error.kind() == io::ErrorKind::Unsupported {
            "directory synchronization is supported on Linux and macOS only; remove sync_dir on this platform"
        } else {
            "sync_dir must be an existing writable directory, not a symlink; create/select the intended root"
        })
    })?;
    for (key, path) in
        std::iter::once(("data_dir", data)).chain(seed.map(|path| ("seed_dir", path)))
    {
        if crate::source_tree::paths_overlap(root, path).map_err(|error| io::Error::other(BackendCause::with_source(
            "Cannot inspect sync_dir overlap; select accessible sync_dir/data_dir/seed_dir paths", error)))? {
            return Err(io::Error::other(format!("sync_dir must not overlap {key}; select separate directories, including resolved aliases")));
        }
    }
    Ok(())
}

pub async fn preflight(catalog: &Catalog, config: &Config) -> io::Result<Option<RootLease>> {
    let Some(root) = &config.sync_dir else {
        return Ok(None);
    };
    validate_directory(root, &config.data_dir, config.seed_dir.as_deref())?;
    let activation = config
        .sync_reinitialize
        .map_or(Activation::Open, |catalog_id| Activation::Reinitialize {
            catalog_id,
        });
    let state = catalog.sync_state();
    let id = state.catalog_id().await.map_err(io::Error::other)?;
    let limits = tree::TreeLimits {
        entries: config.max_sync_entries,
        depth: config.max_sync_depth,
    };
    state
        .activate_root(root, activation)
        .await
        .map(|lease| Some(lease.with_tree_limits(limits)))
        .map_err(|error| {
            let remedy = activation_remedy(&error, id);
            io::Error::other(BackendCause::with_source(
                format!("{error}; {remedy}"),
                error,
            ))
        })
}

/// What an operator does about a root that startup refused. A data directory synchronizes one
/// directory: an unmarked directory that holds files is refused because its files could be
/// anything, so its remedy names the directories to select instead.
fn activation_remedy(error: &RootError, catalog: CatalogId) -> String {
    match error {
        RootError::ReplacementOccupied { synchronized } => format!(
            "sync_dir has no .gcs-mimic-sync.json and holds more than files directly in it, and this data directory synchronizes {}. A data directory synchronizes one directory: select that one, or select an empty directory (files directly in it such as .gitkeep or README.md, no subdirectory) to switch to it; startup writes every object into it, deletes no object and leaves the previous directory as it is. To use files from this directory, start with the synchronized directory or an empty one and copy them into it while the server runs.",
            synchronized.join(", ")
        ),
        RootError::RootChanged => "sync_dir was replaced while startup checked it, overlaps the data directory, or is not the registered directory that --sync-reinitialize names. Keep sync_dir in place and apart from data_dir, and use --sync-reinitialize only for the registered directory whose marker is missing. Without it, startup takes a directory this data directory synchronized before, or an empty one, in place of the registered directory.".to_owned(),
        RootError::Admin(_)
        | RootError::InUse
        | RootError::Unsupported { .. }
        | RootError::MissingMarker
        | RootError::InvalidMarker
        | RootError::UnopenableMarker
        | RootError::ForeignMarker
        | RootError::ReinitializeDenied => format!(
            "restore the intended mount/root and stop other writers. For an associated root with a missing marker only, back up the tree and restart with --sync-reinitialize {catalog}. Preserve foreign/malformed markers and select a fresh unassociated directory; never overwrite them. A foreign .gcs-mimic-sync.json may have been committed to Git or copied from another environment: add it to .gitignore, preserve the tree and select a fresh directory, or select the intended data_dir."
        ),
    }
}
