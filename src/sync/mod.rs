#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Filesystem synchronization adapters and shared reversible name mapping.

pub mod mapping;
mod mirror;
pub use mirror::{Reconciliation, Task, reconcile, start};

pub const SETTLE_MS: u64 = 1000;
pub const DELETE_MIN_OBJECTS: u64 = 100;
pub const DELETE_PERCENT: u64 = 50;
pub const CONFLICT_MAX_BYTES: u64 = 1_073_741_824;
pub const CONFLICT_MAX_ENTRIES: u64 = 1000;

use crate::{
    config::Config,
    fsutil,
    storage::{
        Catalog,
        sync_state::{
            CatalogId,
            root::{Activation, RootLease},
        },
    },
};
use std::{io, path::Path};

pub fn parse_catalog_id(value: &str) -> Result<CatalogId, String> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("sync_reinitialize requires the exact lowercase 32-digit catalog ID; inspect the root marker or startup diagnostic".into());
    }
    let mut bytes = [0; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "sync_reinitialize requires a lowercase 32-digit catalog ID")?;
    }
    Ok(CatalogId::from_bytes(bytes))
}

pub fn validate_directory(root: &Path, data: &Path, seed: Option<&Path>) -> io::Result<()> {
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
        if crate::source_tree::paths_overlap(root, path).map_err(|_| io::Error::other(
            "Cannot inspect sync_dir overlap; select accessible sync_dir/data_dir/seed_dir paths"))? {
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
    state.activate_root(root, activation).await.map(Some).map_err(|error| io::Error::other(format!(
        "{error}; restore the intended mount/root and stop other writers. For an associated root with a missing marker only, back up the tree and restart with --sync-reinitialize {id}. Preserve foreign/malformed markers and select a fresh unassociated directory; never overwrite them. A foreign .gcs-mimic-sync.json may have been committed to Git or copied from another environment: add it to .gitignore, preserve the tree and select a fresh directory, or select the intended data_dir."
    )))
}
