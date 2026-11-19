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
    /// Like [`Self::hit`], for a step whose injected failure stands in for an I/O call's own.
    pub(in crate::storage::catalog::sync_state::writeback) fn hit_io(
        &self,
        step: Step,
    ) -> std::io::Result<()> {
        fault::hit_io(&self.tree, step)?;
        fault::hit_io(&self.data, step)
    }
    /// Records that installation removed the empty directory at `item`'s target, then stops at
    /// the step before its parent's sync makes that durable.
    pub(in crate::storage::catalog::sync_state::writeback) fn directory_removed(
        &self,
        item: &Pending,
    ) -> Result<(), Error> {
        #[cfg(unix)]
        fault::dir_removed(&self.tree.join(&item.path));
        #[cfg(not(unix))]
        let _ = item;
        self.hit(Step::SyncEmptyDirectoryRemoved)
    }
    pub(in crate::storage::catalog::sync_state::writeback) fn synced(
        &self,
        file: &File,
    ) -> Result<(), Error> {
        fault::file_synced(&self.tree, &file.metadata()?);
        Ok(())
    }
    pub(in crate::storage::catalog::sync_state::writeback) fn truncated(
        &self,
        file: &File,
    ) -> Result<(), Error> {
        fault::truncated(&self.tree, &file.metadata()?);
        Ok(())
    }
    /// Records that the copy step created `item`'s private entry, which stays volatile until the
    /// workspace is synced.
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unused_self,
            reason = "only Unix models new entries under the fault roots"
        )
    )]
    pub(super) fn copy_created(&self, item: &Pending) {
        #[cfg(unix)]
        fault::file_created(&self.copy(item));
        #[cfg(not(unix))]
        let _ = item;
    }

    /// Stops at the step after installation's rename. `fsutil::durable_replace_between` records
    /// the rename in the model before the parent syncs inside it, so a power loss here keeps the
    /// rename only when those syncs ran.
    pub(super) fn renamed(&self) -> Result<(), Error> {
        self.hit(Step::SyncCopyRenamed)
    }

    /// Records that installation unlinks `file`, the body left in `item`'s private entry, before
    /// the unlink, so a power loss restores its durable bytes until the workspace is synced.
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unused_self,
            reason = "only Unix models unlinks under the fault roots"
        )
    )]
    pub(in crate::storage::catalog::sync_state::writeback) fn unlinking(
        &self,
        item: &Pending,
        file: &File,
    ) -> Result<(), Error> {
        #[cfg(unix)]
        fault::unlinked(&self.copy(item), file)?;
        #[cfg(not(unix))]
        let _ = (item, file);
        Ok(())
    }

    /// Records the sync of the directory that holds `item`'s target, after the real sync.
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unused_self,
            reason = "only Unix models directory syncs under the fault roots"
        )
    )]
    pub(super) fn target_dir_synced(&self, item: &Pending) {
        #[cfg(unix)]
        if let Some(parent) = self.tree.join(&item.path).parent() {
            fault::dir_synced(parent);
        }
        #[cfg(not(unix))]
        let _ = item;
    }

    /// Records the sync of the private workspace, after the real sync.
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unused_self,
            reason = "only Unix models directory syncs under the fault roots"
        )
    )]
    pub(in crate::storage::catalog::sync_state::writeback) fn workspace_synced(&self) {
        #[cfg(unix)]
        fault::dir_synced(&self.tree.join(mapping::WORKSPACE));
    }

    /// Records the sync of the tree's root directory, after the real sync.
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unused_self,
            reason = "only Unix models directory syncs under the fault roots"
        )
    )]
    pub(in crate::storage::catalog::sync_state::writeback) fn tree_synced(&self) {
        #[cfg(unix)]
        fault::dir_synced(&self.tree);
    }

    /// The fault model's path of `item`'s private workspace entry.
    #[cfg(unix)]
    fn copy(&self, item: &Pending) -> PathBuf {
        self.tree
            .join(mapping::WORKSPACE)
            .join(item.token.id.to_hex())
    }
}

/// Rewrites the stored identity of each tracked file that a snapshot restore copied into `tree`
/// with its tracked bytes. The copy keeps its times but has a new inode number and change time,
/// so ingestion would otherwise take an unchanged file, such as the old target a power loss
/// brought back, for an external edit. A file whose bytes differ keeps its stored identity, so
/// ingestion still sees the difference.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) async fn retrack_restored(
    catalog: &crate::storage::Catalog,
    tree: &std::path::Path,
) -> Result<(), Error> {
    let tree = tree.to_owned();
    catalog
        .run_admin_with_error(move |connection| {
            let tracked: Vec<(String, String, String, u64, [u8; 16], u32)> = connection
                .prepare_cached("SELECT bucket,name,path,size,md5,crc32c FROM sync_objects")?
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                })?
                .collect::<Result<_, _>>()?;
            for (bucket, name, path, size, md5, crc32c) in tracked {
                let file = match File::open(tree.join(&path)) {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                if !content_matches(&file, size, md5, crc32c)? {
                    continue;
                }
                let identity = fsutil::stat_identity(&file)?;
                connection
                    .prepare_cached(
                        "UPDATE sync_objects SET mtime_ns=?3,ctime_ns=?4,inode=?5 WHERE bucket=?1 AND name=?2",
                    )?
                    .execute(params![
                        bucket,
                        name,
                        identity.modified_ns.to_be_bytes(),
                        identity.changed_ns.to_be_bytes(),
                        identity.inode.to_be_bytes()
                    ])?;
            }
            Ok(())
        })
        .await
}

impl Writeback {
    pub(in crate::storage::catalog::sync_state::writeback) fn fault_paths(&self) -> Paths {
        Paths {
            data: self.lease.catalog().0.data_dir.clone(),
            tree: self.lease.fault_tree(),
        }
    }
}
