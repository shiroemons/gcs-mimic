//! The copied tree and its catalog are snapshotted at the same cooperative test boundary.

use super::*;
use crate::storage::catalog::fault::{self, Step};
use std::path::PathBuf;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod tests;

pub(in crate::storage::catalog::sync_state::writeback) struct Paths {
    pub(in crate::storage::catalog::sync_state::writeback) data: PathBuf,
    pub(in crate::storage::catalog::sync_state::writeback) tree: PathBuf,
}
impl Paths {
    pub(in crate::storage::catalog::sync_state::writeback) fn hit(
        &self,
        step: Step,
    ) -> Result<(), Error> {
        fault::hit(&self.tree, step).map_err(objects::object_unavailable)?;
        fault::hit(&self.data, step).map_err(objects::object_unavailable)?;
        Ok(())
    }
    pub(in crate::storage::catalog::sync_state::writeback) fn synced(
        &self,
        file: &File,
    ) -> Result<(), Error> {
        fault::file_synced(&self.tree, &file.metadata()?);
        Ok(())
    }
    pub(super) fn installed(&self, item: &Pending) {
        #[cfg(unix)]
        if item.kind == Kind::Publish {
            let workspace = self.tree.join(mapping::WORKSPACE);
            let target = self.tree.join(&item.path);
            fault::renamed(&workspace.join(&item.token.id), &target);
            fault::dir_synced(&workspace);
            if let Some(parent) = target.parent() {
                fault::dir_synced(parent);
            }
            fault::dir_synced(&self.tree);
        }
        #[cfg(not(unix))]
        let _ = item;
    }
}
impl Writeback {
    pub(in crate::storage::catalog::sync_state::writeback) fn fault_paths(&self) -> Paths {
        Paths {
            data: self.lease.catalog().0.data_dir.clone(),
            tree: self.lease.fault_tree(),
        }
    }
}
