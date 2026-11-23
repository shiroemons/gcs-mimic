//! The schema steps that released directory synchronization added to the catalog, and the step
//! that retires them. Synchronization was removed, so nothing reads or writes its tables; the
//! steps stay so that a data directory from any release up to 0.27.0 still upgrades and
//! validates. Released schema text is frozen history: a change to these files breaks the
//! validation of catalogs that ran them. Test support rebuilds schema 57 from the same files.
//!
//! The triggers these steps create fire only on the synchronization tables themselves, so API
//! writes to buckets and objects never touched them.
//!
//! Schema 58 drops every synchronization table, and with them their indexes and triggers. Rows
//! there may name files a user made: copies of edits under `sync-conflicts/` in the data
//! directory, and entries left in the private workspace of a formerly synchronized folder.
//! [`preserved`] lists them in a durable manifest before the transaction that drops the rows, and
//! warns at every start while copies remain. Nothing deletes, moves or changes those files.

use super::{CatalogError, sql, unavailable};
use rusqlite::Connection;
use std::path::Path;

mod preserved;
#[cfg(test)]
mod tests;

pub(super) use preserved::warn_preserved_copies;

/// Schema 17: the catalog's identity and the files synchronization tracked. The identity is not
/// synchronization state: reset, reseed and import keep it, and schema 58 leaves it in place.
pub(super) const IDENTITY_SCHEMA: &str = concat!(
    include_str!("retired_sync/identity.sql"),
    include_str!("retired_sync/objects.sql")
);

/// Schema 19: the synchronized directory roots.
pub(super) const ROOT_SCHEMA: &str = include_str!("retired_sync/roots.sql");

/// Schema 20: the write-back of objects into a root.
pub(super) const WRITEBACK_SCHEMA: &str = include_str!("retired_sync/writeback.sql");

/// Schema 21: the objects a root could not hold.
pub(super) const FINDINGS_SCHEMA: &str = include_str!("retired_sync/findings.sql");

/// Schema 27: the bucket incarnations a root mapped.
pub(super) const BUCKETS_SCHEMA: &str = include_str!("retired_sync/buckets.sql");

/// Schema 40: the deletion sets waiting for a decision, and the owner of each finding, which the
/// step derives from the bucket incarnation the finding names.
pub(super) const DELETIONS_SCHEMA: &str = include_str!("retired_sync/deletions.sql");

/// Schema 41: what the synchronization status showed, with one observation row per root.
pub(super) const OBSERVATION_SCHEMA: &str = include_str!("retired_sync/observation.sql");

/// Schema 42: the user edits a write-back kept aside and the recovery workspaces.
pub(super) const CONFLICTS_SCHEMA: &str = include_str!("retired_sync/conflicts.sql");

/// Schema 50: how many deletions a root carried out without a decision, with one row per root.
pub(super) const DELETION_HISTORY_SCHEMA: &str = include_str!("retired_sync/deletion_history.sql");

pub(super) const RETIREMENT_SCHEMA_VERSION: i64 = 58;

/// Schema 58: creates `seed_objects` and drops the synchronization tables. Each table goes
/// before the tables its foreign keys name, because with foreign keys enforced a `DROP TABLE`
/// first deletes its rows, and a parent's rows cannot go while a child still names them.
///
/// `seed_objects` holds, per object, what the seed directory last published there: the
/// generation the seed owns, if any, and the size, digests and file fingerprint it observed.
/// The live seed watcher will read and write it; nothing does yet, and no release has shipped
/// it, so its columns may still change in place.
pub(super) const RETIREMENT_SCHEMA: &str = include_str!("retired_sync/retirement.sql");

/// The tables schema 58 adds for the seed watcher. Reset and purge have no policy for them
/// until the watcher writes them; their classification tests list them here meanwhile.
#[cfg(test)]
pub(super) const SEED_TABLES: &[&str] = &["seed_objects"];

/// The tables the released steps created and schema 58 drops.
#[cfg(test)]
pub(super) const TABLES: &[&str] = &[
    "sync_buckets",
    "sync_conflict_operations",
    "sync_conflict_roots",
    "sync_conflicts",
    "sync_deletion_history",
    "sync_deletion_members",
    "sync_deletion_sets",
    "sync_finding_owners",
    "sync_findings",
    "sync_objects",
    "sync_observations",
    "sync_path_stops",
    "sync_recovery_workspaces",
    "sync_roots",
    "sync_writeback_ops",
    "sync_writeback_roots",
];

/// Upgrades to schema 17, giving the catalog a random identity in the same transaction.
#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration step"
)]
pub(super) fn migrate_identity(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(IDENTITY_SCHEMA)?;
    let mut id = [0; 16];
    getrandom::fill(&mut id).map_err(unavailable)?;
    transaction.execute("INSERT INTO catalog_identity VALUES (1, ?1)", [id])?;
    transaction.commit().map_err(unavailable)
}

/// Upgrades to schema 58. A manifest of the files the retired rows name is durable in
/// `data_dir` before the transaction that drops those rows starts, so a crash between the two
/// leaves schema 57 and the next start writes the same manifest again. A RAM catalog (`None`)
/// starts empty, so its rows never name a file.
pub(super) fn retire(
    connection: &mut Connection,
    data_dir: Option<&Path>,
) -> Result<(), CatalogError> {
    if preserved::named(connection)? {
        let data_dir = data_dir.ok_or_else(|| {
            unavailable(
                "retired synchronization rows name files, but the catalog has no data directory",
            )
        })?;
        preserved::write_manifest(connection, data_dir)?;
    }
    let transaction = connection.transaction()?;
    transaction.execute_batch(RETIREMENT_SCHEMA)?;
    #[cfg(test)]
    super::fault::hit_transaction(&transaction, super::fault::Step::RetiredSyncCommit)
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

#[cfg(test)]
impl super::Catalog {
    /// The identity schema 17 gave the catalog, which reset, reseed and reopening keep.
    pub(crate) async fn identity(&self) -> Result<[u8; 16], super::admin::AdminError> {
        self.run_admin(|connection| {
            sql::scalar(
                connection,
                "SELECT id FROM catalog_identity WHERE singleton = 1",
                [],
            )
            .map_err(unavailable)
        })
        .await
    }
}

/// Checks that a catalog from schema 17 on holds exactly one 16-byte identity.
pub(super) fn validate_identity(connection: &Connection) -> Result<(), CatalogError> {
    sql::validate(
        connection,
        "SELECT count(*) = 1 AND min(singleton) = 1 FROM catalog_identity",
        "invalid catalog identity",
    )?;
    sql::scalar::<[u8; 16]>(
        connection,
        "SELECT id FROM catalog_identity WHERE singleton = 1",
        [],
    )
    .map_err(unavailable)?;
    Ok(())
}
