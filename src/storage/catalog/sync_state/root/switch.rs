//! Switching startup to an empty directory that took a synchronized root's place.
//!
//! A root is registered under its path and its directory's identity, and startup refuses a
//! directory that matches one but not the other: a wrong mount must never pass for a tree whose
//! missing files are deletions. A directory that holds nothing, or only Finder's `.DS_Store`
//! ([`empty::contents`]), holds no file whose absence could mean anything, so startup takes it as
//! a new root, as on a first start, rather than refuse it. It proves the directory empty
//! ([`EmptyRoot::prove`]) and stages a marker for the new root; then one transaction forgets what
//! the catalog recorded for the previous root and registers the new one ([`switch`]), and
//! publishing the marker follows. A crash before that transaction commits leaves the previous
//! registration, and the next start removes the staged marker, which no registered root owns,
//! and switches again. A crash after it leaves the new registration and the staged marker it
//! owns, which the next start publishes. The previous directory is never opened: its files,
//! including the bytes its conflicts retained in its workspace, stay as they are, and no object
//! changes.

use super::{
    super::{
        CatalogId, SyncState,
        empty::{self, Contents},
    },
    Baseline, Marker, MarkerState, Ownership, RootError, RootId, RootStatus, stage_marker,
};
use crate::{
    fsutil::NodeId,
    storage::{
        admin::AdminError,
        catalog::{sql, unavailable},
    },
};
use rusqlite::{Connection, params};
use std::sync::Arc;

/// The registered root whose path or directory the selected directory took over, as the
/// reservation found it.
pub(super) struct Replaced {
    pub(super) root_id: RootId,
    pub(super) location: Vec<u8>,
    pub(super) node: NodeId,
}

/// Proof that the selected directory held nothing but Finder metadata files when startup
/// listed it. Only [`EmptyRoot::prove`] makes one, and [`switch`] registers the directory it
/// names.
struct EmptyRoot {
    location: Vec<u8>,
    node: NodeId,
    /// How many Finder metadata files it held, for the log.
    metadata: usize,
}

impl EmptyRoot {
    /// Lists the directory `owns` holds. One that holds anything else, or cannot be listed, is
    /// no proof, and startup refuses it as it refused every directory in a root's place before.
    fn prove(owns: &Ownership) -> Result<Option<Self>, RootError> {
        owns.validate()?;
        Ok(match empty::contents(owns.lock.file())? {
            Contents::Empty(metadata) => Some(Self {
                location: owns.path.as_os_str().as_encoded_bytes().to_vec(),
                node: owns.identity.node(),
                metadata: metadata.len(),
            }),
            Contents::Holds | Contents::Unlisted(_) => None,
        })
    }
}

/// What a switch forgot of the previous root, for the log.
struct Forgotten {
    tracked_files: usize,
    findings: usize,
    path_stops: usize,
    deletion_candidates: u64,
    retained_conflicts: u64,
}

/// A committed switch, which [`Switched::report`] logs.
struct Switched {
    previous: Replaced,
    proof: EmptyRoot,
    root_id: RootId,
    forgotten: Forgotten,
}

impl Switched {
    /// Warns that startup took an empty directory in place of the synchronized one, so that an
    /// operator who did not mean to replace it, such as one whose mount failed, learns which
    /// root the catalog forgot and what went with it.
    fn report(&self) {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            "sync_root_switched",
            serde_json::json!({
                "reason": "empty_directory",
                "previous_root_id": self.previous.root_id.to_string(),
                "previous_path": String::from_utf8_lossy(&self.previous.location),
                "root_id": self.root_id.to_string(),
                "path": String::from_utf8_lossy(&self.proof.location),
                "finder_metadata": self.proof.metadata,
                "forgotten_tracked_files": self.forgotten.tracked_files,
                "forgotten_findings": self.forgotten.findings,
                "forgotten_path_stops": self.forgotten.path_stops,
                "forgotten_deletion_candidates": self.forgotten.deletion_candidates,
                "forgotten_retained_conflicts": self.forgotten.retained_conflicts,
            }),
        );
    }
}

impl SyncState {
    /// Registers the directory `ownership` holds as a new root in place of `previous` when it is
    /// empty, and returns the new root's status and its staged marker for publication. A
    /// directory that holds anything is refused as before.
    pub(super) async fn switch_root(
        &self,
        ownership: &Arc<Ownership>,
        catalog: CatalogId,
        previous: Replaced,
    ) -> Result<(RootStatus, MarkerState), RootError> {
        let root_id =
            RootId::generate().map_err(|error| RootError::Admin(AdminError::backend(error)))?;
        let status = RootStatus {
            catalog_id: catalog,
            root_id,
            baseline: Baseline::Activating,
        };
        let marker = Marker::new(status);
        let owns = ownership.clone();
        let proof = self
            .0
            .0
            .file_workers
            .run(move || {
                let proof = EmptyRoot::prove(&owns)?.ok_or(RootError::RootChanged)?;
                stage_marker(&owns, marker)?;
                #[cfg(test)]
                crate::storage::catalog::fault::hit_io(
                    &owns.path,
                    crate::storage::catalog::fault::Step::SyncRootSwitchStaged,
                )?;
                Ok::<_, RootError>(proof)
            })
            .await?;
        let switched = self
            .0
            .run_admin_with_error(move |connection| switch(connection, proof, previous, root_id))
            .await?;
        #[cfg(test)]
        {
            let owns = ownership.clone();
            self.0
                .0
                .file_workers
                .run(move || {
                    crate::storage::catalog::fault::hit_io(
                        &owns.path,
                        crate::storage::catalog::fault::Step::SyncRootSwitched,
                    )
                    .map_err(RootError::from)
                })
                .await?;
        }
        switched.report();
        Ok((status, MarkerState::Staged(marker)))
    }
}

/// Forgets what the catalog records for `previous` and registers the directory `proof` names as
/// the root `root_id`, with its baseline still to build, in one transaction. The registrations
/// must still be as the reservation found them.
fn switch(
    connection: &mut Connection,
    proof: EmptyRoot,
    previous: Replaced,
    root_id: RootId,
) -> Result<Switched, RootError> {
    let transaction = sql::immediate(connection).map_err(unavailable)?;
    let (device, inode) = previous.node.to_be_bytes();
    let (new_device, new_inode) = proof.node.to_be_bytes();
    let unchanged: bool = sql::scalar(
        &transaction,
        "SELECT EXISTS(SELECT 1 FROM sync_roots WHERE root_id=?1 AND location=?2 AND device=?3 AND inode=?4)
         AND NOT EXISTS(SELECT 1 FROM sync_roots WHERE root_id!=?1 AND (location=?5 OR (device=?6 AND inode=?7)))",
        params![
            previous.root_id,
            previous.location,
            device,
            inode,
            proof.location,
            new_device,
            new_inode
        ],
    )
    .map_err(unavailable)?;
    if !unchanged {
        return Err(RootError::RootChanged);
    }
    let forgotten = forget(&transaction, previous.root_id).map_err(unavailable)?;
    sql::exec(
        &transaction,
        "INSERT INTO sync_roots VALUES (?1,?2,?3,?4,0)",
        params![root_id, proof.location, new_device, new_inode],
    )
    .map_err(unavailable)?;
    #[cfg(test)]
    crate::storage::catalog::fault::hit_io(
        &crate::storage::catalog::fault::database_dir(&transaction),
        crate::storage::catalog::fault::Step::SyncRootSwitchStaged,
    )?;
    transaction.commit().map_err(unavailable)?;
    #[cfg(test)]
    crate::storage::catalog::fault::hit_io(
        &crate::storage::catalog::fault::database_dir(connection),
        crate::storage::catalog::fault::Step::SyncRootSwitched,
    )?;
    Ok(Switched {
        previous,
        proof,
        root_id,
        forgotten,
    })
}

/// Deletes the rows that belong to `root`, the root itself, and every tracked file. Tracked
/// files carry no root, so none can stay without standing for a file the new root does not hold;
/// every other root therefore builds its baseline again, as a reinitialized one does, before a
/// missing file there counts as a deletion.
fn forget(transaction: &Connection, root: RootId) -> rusqlite::Result<Forgotten> {
    let (deletion_candidates, retained_conflicts) = sql::row(
        transaction,
        "SELECT (SELECT count(*) FROM sync_deletion_members WHERE root_id=?1),
                (SELECT count(*) FROM sync_conflicts WHERE root_id=?1 AND state!=2)",
        [root],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let forgotten = Forgotten {
        tracked_files: sql::exec(transaction, "DELETE FROM sync_objects", [])?,
        findings: sql::exec(
            transaction,
            "DELETE FROM sync_findings WHERE root_id=?1",
            [root],
        )?,
        path_stops: sql::exec(
            transaction,
            "DELETE FROM sync_path_stops WHERE root_id=?1",
            [root],
        )?,
        deletion_candidates,
        retained_conflicts,
    };
    // Rows before the rows they reference. Deletion members, finding owners, the observation
    // and the deletion history follow their parents by cascade.
    for statement in [
        "DELETE FROM sync_conflict_operations WHERE root_id=?1",
        "DELETE FROM sync_recovery_workspaces WHERE root_id=?1",
        "DELETE FROM sync_conflicts WHERE root_id=?1",
        "DELETE FROM sync_conflict_roots WHERE root_id=?1",
        "DELETE FROM sync_writeback_ops WHERE root_id=?1",
        "DELETE FROM sync_writeback_roots WHERE root_id=?1",
        "DELETE FROM sync_deletion_sets WHERE root_id=?1",
        "DELETE FROM sync_buckets WHERE root_id=?1",
        "DELETE FROM sync_roots WHERE root_id=?1",
    ] {
        sql::exec(transaction, statement, [root])?;
    }
    sql::exec(transaction, "UPDATE sync_roots SET complete=0", [])?;
    Ok(forgotten)
}
