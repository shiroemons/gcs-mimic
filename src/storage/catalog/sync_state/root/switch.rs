//! One synchronized directory per data directory, and what startup does when it is given another.
//!
//! A root is registered under its path and its directory's identity, and the catalog tracks the
//! files of one tree: a tracked file carries no root. Ingestion takes a file whose content differs
//! from its tracked file while the object kept its generation for an edit, and a baseline takes a
//! tracked file it lacks for a deletion, so tracked files are only ever compared with the tree
//! they were recorded in. Whenever the data directory changes the directory it synchronizes, one
//! transaction forgets every registered root and every tracked file and registers the directory
//! it changes to with its baseline still to build ([`rebind`]). Its first pass then merges it as a
//! first start does: a file that differs from its object is kept as a conflict, an object without
//! a file is written into the directory, and nothing is deleted.
//!
//! Startup changes the directory in three cases. A directory that holds this data directory's
//! marker, which it synchronized before at that path or another, is merged under the root its
//! marker names, which keeps the marker ([`Reason::MarkedDirectory`]). A directory without a
//! marker that holds no subdirectory, no symlink and no special file, only files directly in it
//! that a scan never makes objects of, such as a `.gitkeep` or a `README.md` ([`RootFiles`]), gets
//! a new root ([`Reason::EmptyDirectory`]): startup proves it and stages the new marker, and the
//! transaction rechecks that the registrations are still those it replaces ([`switch`]). A catalog
//! that an earlier release let register several roots keeps only the one it opens
//! ([`Reason::SeveralRoots`]). Any other directory without a marker, which holds files whose
//! absence from the registered tree could mean anything, is refused.
//!
//! A crash before the transaction commits leaves the previous registrations; for a new root, the
//! next start removes the staged marker, which no registered root owns, and switches again. A
//! crash after it leaves the new registration and the marker it owns, staged or published, which
//! the next start publishes. A previous directory is never opened, and no object changes. What the
//! catalog forgot of the previous roots, among them the conflicts and recovery workspaces whose
//! bytes stay where they were, is logged ([`Change::report`]).

use super::{
    super::{CatalogId, SyncState},
    AtStage, Baseline, Marker, MarkerState, Ownership, RootError, RootId, RootStatus, Stage,
    remove_staging, stage_marker,
};
#[cfg(test)]
use crate::storage::catalog::fault::{self, Step};
use crate::{
    fsutil::{self, NodeId},
    storage::{
        admin::AdminError,
        catalog::{sql, unavailable},
    },
};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::{fs::File, io, sync::Arc};

/// A registered root as the catalog records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Registered {
    pub(super) root_id: RootId,
    pub(super) location: Vec<u8>,
    pub(super) node: NodeId,
    pub(super) complete: bool,
}

impl Registered {
    pub(super) fn path(&self) -> String {
        String::from_utf8_lossy(&self.location).into_owned()
    }

    fn logged(&self) -> serde_json::Value {
        serde_json::json!({ "root_id": self.root_id.to_string(), "path": self.path() })
    }
}

/// Every registered root, in root order. A catalog of an earlier release may register more than
/// one.
pub(super) fn registered(connection: &Connection) -> Result<Vec<Registered>, RootError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT root_id, location, device, inode, complete FROM sync_roots ORDER BY root_id",
        )
        .map_err(unavailable)?;
    let roots = statement
        .query_map([], |row| {
            Ok(Registered {
                root_id: row.get(0)?,
                location: row.get(1)?,
                node: NodeId::from_be_bytes(row.get(2)?, row.get(3)?),
                complete: row.get(4)?,
            })
        })
        .and_then(Iterator::collect)
        .map_err(unavailable)?;
    Ok(roots)
}

/// The remedy sentence every reason shares, about what the catalog forgot.
macro_rules! forgotten_remedy {
    () => {
        concat!(
            "sync conflicts no longer lists the conflicts and recovery workspaces of the previous ",
            "directories: their bytes stay at the listed paths, a conflict's under the data ",
            "directory and a recovery workspace's under its previous directory, until you copy ",
            "or remove them. Resolve them before switching on purpose."
        )
    };
}

/// Why the data directory changed the directory it synchronizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    /// The selected directory had no marker and held nothing a scan makes objects of.
    EmptyDirectory,
    /// The selected directory held this data directory's marker for a root it no longer
    /// synchronized at that path.
    MarkedDirectory,
    /// The catalog registered more than one root, and startup kept only the one it opened.
    SeveralRoots,
}

impl Reason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::EmptyDirectory => "empty_directory",
            Self::MarkedDirectory => "marked_directory",
            Self::SeveralRoots => "several_roots",
        }
    }

    fn remedy(self) -> &'static str {
        match self {
            Self::EmptyDirectory => concat!(
                "Startup now synchronizes this directory in place of the previous one and writes ",
                "every object into it; no object was deleted and the previous directory is as it ",
                "was. To go back, stop the server and start it with the previous directory: ",
                "startup merges it again. ",
                forgotten_remedy!()
            ),
            Self::MarkedDirectory => concat!(
                "Startup merges this directory, which this data directory synchronized before, in ",
                "place of the previous one: a file whose content differs from its object is kept ",
                "as a conflict, an object without a file is written into the directory, a file ",
                "without an object becomes one, including the file of an object deleted since, ",
                "and nothing is deleted. ",
                forgotten_remedy!()
            ),
            Self::SeveralRoots => concat!(
                "This data directory registered several synchronized directories, of which this ",
                "release keeps one. Startup merges this one as on a first start: a file whose ",
                "content differs from its object is kept as a conflict, an object without a file ",
                "is written into the directory, and nothing is deleted. ",
                forgotten_remedy!()
            ),
        }
    }
}

/// What the status reports about a change of the synchronized directory this process made at
/// startup.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RootChange {
    pub(crate) reason: &'static str,
    pub(crate) previous_paths: Vec<String>,
}

/// The registered roots that the selected directory takes the place of when it holds nothing, as
/// the reservation found them: every one, because a data directory synchronizes one directory.
pub(super) struct Replaced(pub(super) Vec<Registered>);

/// The most names startup reads to tell that a directory holds only root files. One that lists
/// more is refused, and the bound keeps a large directory from costing a full listing.
const ROOT_FILE_LIMIT: usize = 1024;

/// Proof that the selected directory held only regular files directly in it, which a scan never
/// makes objects of, when startup listed it. Only [`RootFiles::prove`] makes one, and [`switch`]
/// registers the directory it names.
struct RootFiles {
    location: Vec<u8>,
    node: NodeId,
    /// How many files it held, for the log.
    files: usize,
}

impl RootFiles {
    /// Lists the directory `owns` holds. One that holds anything else is no proof.
    fn prove(owns: &Ownership) -> Result<Option<Self>, RootError> {
        owns.validate()?;
        let listed = root_files(owns.lock.file()).at(Stage::ListRootFiles)?;
        Ok(listed.map(|files| Self {
            location: owns.path.as_os_str().as_encoded_bytes().to_vec(),
            node: owns.identity.node(),
            files,
        }))
    }
}

/// Lists `directory`, never below it and at most [`ROOT_FILE_LIMIT`] names: how many regular files
/// it holds when it holds nothing else, and `None` when it holds a directory, a symlink, a special
/// file or more names. Unlike the definition an export uses before it removes a directory
/// (`empty::contents`), any regular file counts: startup removes nothing, and a scan makes no
/// object of a file directly in the root.
fn root_files(directory: &File) -> io::Result<Option<usize>> {
    let mut names = fsutil::DirectoryNames::default();
    let mut files = 0;
    loop {
        let name = match names.next_name(directory, ROOT_FILE_LIMIT) {
            Ok(Some(name)) => name,
            Ok(None) => return Ok(Some(files)),
            // More names than are read.
            Err(error) if error.kind() == io::ErrorKind::InvalidData => return Ok(None),
            Err(error) => return Err(error),
        };
        match fsutil::Lookup::unprobed().regular(directory, &name) {
            Ok(fsutil::RegularChild::File(_)) => files += 1,
            // Gone since the listing.
            Ok(fsutil::RegularChild::Missing(_)) => {}
            // An entry that kept failing to open may be of any kind.
            Ok(fsutil::RegularChild::Respelled(_) | fsutil::RegularChild::Transient) => {
                return Ok(None);
            }
            Err(error) if crate::sync::tree::structural_entry_error(&error) => return Ok(None),
            Err(error) => return Err(error),
        }
    }
}

/// How many conflicts and recovery workspaces of the previous roots the log lists; it counts the
/// rest.
const LISTED: usize = 10;

/// What forgetting roots discarded, for the log.
#[derive(Default)]
struct Forgotten {
    tracked_files: usize,
    findings: u64,
    path_stops: u64,
    deletion_candidates: u64,
    retained_conflicts: u64,
    recovery_workspaces: u64,
    writebacks: u64,
    /// The first [`LISTED`] retained conflicts, with the path of their bytes under the data
    /// directory, which nothing leads to once their rows are gone.
    conflicts: Vec<serde_json::Value>,
    /// The first [`LISTED`] recovery workspaces, with their previous directory and the path of
    /// their bytes under it.
    recoveries: Vec<serde_json::Value>,
}

/// A committed change of the synchronized directory, which [`Change::report`] logs.
pub(super) struct Change {
    reason: Reason,
    previous: Vec<Registered>,
    root_id: RootId,
    location: Vec<u8>,
    /// The files directly in a new root's directory, which startup leaves as they are.
    root_files: Option<usize>,
    forgotten: Forgotten,
}

impl Change {
    /// Warns that the data directory now synchronizes another directory, so that an operator who
    /// did not mean to change it, such as one whose mount failed or who named another directory
    /// by mistake, learns which roots the catalog forgot and what went with them.
    pub(super) fn report(&self) {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            "sync_root_switched",
            self.event(),
        );
    }

    /// The fields of the `sync_root_switched` warning.
    pub(super) fn event(&self) -> serde_json::Value {
        let forgotten = &self.forgotten;
        serde_json::json!({
            "reason": self.reason.as_str(),
            "previous_roots": self.previous.iter().map(Registered::logged).collect::<Vec<_>>(),
            "root_id": self.root_id.to_string(),
            "path": String::from_utf8_lossy(&self.location),
            "root_files": self.root_files,
            "forgotten_tracked_files": forgotten.tracked_files,
            "forgotten_findings": forgotten.findings,
            "forgotten_path_stops": forgotten.path_stops,
            "forgotten_deletion_candidates": forgotten.deletion_candidates,
            "forgotten_writebacks": forgotten.writebacks,
            "forgotten_retained_conflicts": forgotten.retained_conflicts,
            "forgotten_conflicts": forgotten.conflicts,
            "forgotten_recovery_workspaces": forgotten.recovery_workspaces,
            "forgotten_recoveries": forgotten.recoveries,
            "remedy": self.reason.remedy(),
        })
    }

    pub(super) fn summary(&self) -> RootChange {
        RootChange {
            reason: self.reason.as_str(),
            previous_paths: self.previous.iter().map(Registered::path).collect(),
        }
    }
}

/// Forgets every root of `previous` and every tracked file, and registers the directory at
/// `location` as the root `root_id` with its baseline still to build, inside `transaction`.
pub(super) fn rebind(
    transaction: &Connection,
    reason: Reason,
    previous: Vec<Registered>,
    root_id: RootId,
    location: &[u8],
    node: NodeId,
) -> Result<Change, RootError> {
    let mut forgotten = Forgotten::default();
    for root in &previous {
        forget_root(transaction, root, &mut forgotten).map_err(unavailable)?;
    }
    forgotten.tracked_files =
        sql::exec(transaction, "DELETE FROM sync_objects", []).map_err(unavailable)?;
    super::insert_root(transaction, root_id, location, node)?;
    Ok(Change {
        reason,
        previous,
        root_id,
        location: location.to_vec(),
        root_files: None,
        forgotten,
    })
}

impl SyncState {
    /// Registers the directory `ownership` holds as a new root in place of `previous` when it
    /// holds only root files, and returns the new root's status, its staged marker for
    /// publication and the change to log. A directory that holds anything else is refused.
    pub(super) async fn switch_root(
        &self,
        ownership: &Arc<Ownership>,
        catalog: CatalogId,
        previous: Replaced,
    ) -> Result<(RootStatus, MarkerState, Change), RootError> {
        let root_id =
            RootId::generate().map_err(|error| RootError::Admin(AdminError::backend(error)))?;
        let status = RootStatus {
            catalog_id: catalog,
            root_id,
            baseline: Baseline::Activating,
        };
        let occupied = RootError::ReplacementOccupied {
            synchronized: previous.0.iter().map(Registered::path).collect(),
        };
        let owns = ownership.clone();
        let (proof, staged) = self
            .0
            .0
            .file_workers
            .run(move || {
                let proof = RootFiles::prove(&owns)?.ok_or(occupied)?;
                let staged = stage_marker(&owns, Marker::new(status))?;
                #[cfg(test)]
                fault::hit_io(&owns.path, Step::SyncRootSwitchStaged).at(Stage::WriteMarker)?;
                Ok::<_, RootError>((proof, staged))
            })
            .await?;
        let marker = staged.0;
        let switched = self
            .0
            .run_admin_with_error(move |connection| switch(connection, &proof, previous, &staged))
            .await?;
        let Switch::Committed(change) = switched else {
            // Nothing registers the staged marker, which would otherwise wait for the next start.
            let owns = ownership.clone();
            self.0
                .0
                .file_workers
                .run(move || remove_staging(owns.lock.file()))
                .await?;
            return Err(RootError::RootChanged);
        };
        #[cfg(test)]
        {
            let owns = ownership.clone();
            self.0
                .0
                .file_workers
                .run(move || {
                    fault::hit_io(&owns.path, Step::SyncRootSwitched).at(Stage::WriteMarker)
                })
                .await?;
        }
        Ok((status, MarkerState::Staged(marker), change))
    }
}

/// A marker that [`stage_marker`] wrote and synced under the staging name, which only the switch
/// that registers its root publishes.
pub(super) struct Staged(pub(super) Marker);

/// How a switch transaction ended.
enum Switch {
    Committed(Change),
    /// The registrations changed since the reservation read them, and nothing was written.
    Raced,
}

/// Registers the directory `proof` names as the root of the `staged` marker in place of
/// `previous`, in one transaction that first checks the registrations are still as the
/// reservation found them. A failure of the database leaves it unknown whether the transaction
/// committed, so only a recheck that failed reports [`Switch::Raced`].
fn switch(
    connection: &mut Connection,
    proof: &RootFiles,
    previous: Replaced,
    staged: &Staged,
) -> Result<Switch, RootError> {
    let transaction = sql::immediate(connection).map_err(unavailable)?;
    if registered(&transaction)? != previous.0 {
        return Ok(Switch::Raced);
    }
    let mut change = rebind(
        &transaction,
        Reason::EmptyDirectory,
        previous.0,
        staged.0.root_id,
        &proof.location,
        proof.node,
    )?;
    change.root_files = Some(proof.files);
    #[cfg(test)]
    fault::hit_io(
        &fault::database_dir(&transaction),
        Step::SyncRootSwitchStaged,
    )
    .map_err(|error| RootError::Admin(error.into()))?;
    transaction.commit().map_err(unavailable)?;
    #[cfg(test)]
    fault::hit_io(&fault::database_dir(connection), Step::SyncRootSwitched)
        .map_err(|error| RootError::Admin(error.into()))?;
    Ok(Switch::Committed(change))
}

/// The statements that forget one root, rows before the rows they reference. The rows that
/// [`CASCADED`] names go with their parents.
pub(super) const FORGET: &[&str] = &[
    "DELETE FROM sync_findings WHERE root_id=?1",
    "DELETE FROM sync_path_stops WHERE root_id=?1",
    "DELETE FROM sync_conflict_operations WHERE root_id=?1",
    "DELETE FROM sync_recovery_workspaces WHERE root_id=?1",
    "DELETE FROM sync_conflicts WHERE root_id=?1",
    "DELETE FROM sync_conflict_roots WHERE root_id=?1",
    "DELETE FROM sync_writeback_ops WHERE root_id=?1",
    "DELETE FROM sync_writeback_roots WHERE root_id=?1",
    "DELETE FROM sync_deletion_sets WHERE root_id=?1",
    "DELETE FROM sync_buckets WHERE root_id=?1",
    "DELETE FROM sync_roots WHERE root_id=?1",
];

/// The tables with a `root_id` column whose rows a foreign key deletes together with the rows
/// [`FORGET`] deletes.
#[cfg(all(test, unix))]
pub(super) const CASCADED: &[&str] = &[
    "sync_deletion_members",
    "sync_observations",
    "sync_deletion_history",
    "sync_deletion_held",
];

/// Deletes the rows that belong to `root` and the root itself, and adds what they held to
/// `forgotten`.
fn forget_root(
    transaction: &Connection,
    root: &Registered,
    forgotten: &mut Forgotten,
) -> rusqlite::Result<()> {
    let id = root.root_id;
    let counts: [u64; 6] = sql::row(
        transaction,
        "SELECT (SELECT count(*) FROM sync_findings WHERE root_id=?1),
                (SELECT count(*) FROM sync_path_stops WHERE root_id=?1),
                (SELECT count(*) FROM sync_deletion_members WHERE root_id=?1),
                (SELECT count(*) FROM sync_conflicts WHERE root_id=?1 AND state!=2),
                (SELECT count(*) FROM sync_writeback_ops WHERE root_id=?1 AND phase=2),
                (SELECT count(*) FROM sync_writeback_ops WHERE root_id=?1)",
        [id],
        |row| {
            Ok([
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ])
        },
    )?;
    let [
        findings,
        path_stops,
        candidates,
        conflicts,
        recoveries,
        writebacks,
    ] = counts;
    forgotten.findings += findings;
    forgotten.path_stops += path_stops;
    forgotten.deletion_candidates += candidates;
    forgotten.retained_conflicts += conflicts;
    forgotten.recovery_workspaces += recoveries;
    forgotten.writebacks += writebacks;
    list_retained(transaction, root, forgotten)?;
    for statement in FORGET {
        sql::exec(transaction, statement, [id])?;
    }
    Ok(())
}

/// Adds the first retained conflicts and recovery workspaces of `root` to `forgotten`, up to
/// [`LISTED`] of each in all.
fn list_retained(
    transaction: &Connection,
    root: &Registered,
    forgotten: &mut Forgotten,
) -> rusqlite::Result<()> {
    let room = LISTED.saturating_sub(forgotten.conflicts.len());
    let mut statement = transaction.prepare_cached(
        "SELECT bucket, name, generation, id FROM sync_conflicts
         WHERE root_id=?1 AND state!=2 ORDER BY ordinal LIMIT ?2",
    )?;
    let conflicts = statement.query_map(params![root.root_id, room], |row| {
        Ok(serde_json::json!({
            "bucket": row.get::<_, String>(0)?,
            "object": row.get::<_, String>(1)?,
            "generation": row.get::<_, Option<i64>>(2)?,
            "path": format!("sync-conflicts/{}", row.get::<_, String>(3)?),
        }))
    })?;
    for conflict in conflicts {
        forgotten.conflicts.push(conflict?);
    }
    let room = LISTED.saturating_sub(forgotten.recoveries.len());
    let mut statement = transaction.prepare_cached(
        "SELECT bucket, name, generation, id FROM sync_writeback_ops
         WHERE root_id=?1 AND phase=2 ORDER BY ordinal LIMIT ?2",
    )?;
    let recoveries = statement.query_map(params![root.root_id, room], |row| {
        Ok(serde_json::json!({
            "bucket": row.get::<_, String>(0)?,
            "object": row.get::<_, String>(1)?,
            "generation": row.get::<_, i64>(2)?,
            "root_path": root.path(),
            "path": format!("{}/{}", crate::sync::mapping::WORKSPACE, row.get::<_, String>(3)?),
        }))
    })?;
    for recovery in recoveries {
        forgotten.recoveries.push(recovery?);
    }
    Ok(())
}
