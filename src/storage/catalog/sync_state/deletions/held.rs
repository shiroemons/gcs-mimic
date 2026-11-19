//! The tracked files of the set that waits for a decision, kept apart from the set. An
//! inventory that finds other members replaces the set, and once replaced it with one below
//! the threshold whenever some files came back or others joined the tree, which then ran
//! without anyone deciding. The replacing set keeps waiting while it holds any of these files.
use super::*;

/// The schema that records the files of the set that waits, by bucket incarnation and name.
/// The number is written only here: [`migrate`] stores it, so that reordering it against
/// another lane's schema changes one line.
pub(in crate::storage) const SCHEMA_VERSION: i64 = 61;
pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE sync_deletion_held (
 root_id BLOB NOT NULL REFERENCES sync_roots(root_id) ON DELETE CASCADE,
 bucket TEXT NOT NULL COLLATE BINARY,
 incarnation TEXT NOT NULL,
 name TEXT NOT NULL COLLATE BINARY,
 PRIMARY KEY(root_id,bucket,incarnation,name)
) WITHOUT ROWID;";

/// Upgrades to [`SCHEMA_VERSION`] in one transaction, copying the files of a set that waits
/// when the catalog is upgraded, so that the set keeps waiting.
#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration at open"
)]
pub(in crate::storage) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SCHEMA)?;
    transaction.execute(
        "INSERT INTO sync_deletion_held SELECT m.root_id,m.bucket,m.incarnation,m.name FROM sync_deletion_members m JOIN sync_deletion_sets d ON d.root_id=m.root_id WHERE d.complete=1 AND d.guarded=1 AND d.authorization=0 AND m.done=0",
        [],
    )?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit().map_err(unavailable)
}

/// Whether the root's set holds a file of the set that waited before it.
pub(super) fn holds_waiting(connection: &Connection, root: RootId) -> Result<bool, CatalogError> {
    Ok(sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM sync_deletion_members m JOIN sync_deletion_held h ON h.root_id=m.root_id AND h.bucket=m.bucket AND h.incarnation=m.incarnation AND h.name=m.name WHERE m.root_id=?1 AND m.done=0)",
        [root],
    )?)
}

/// Records the members of the root's new set as the files that wait when it waits, and
/// forgets the files that waited before it otherwise: none of them is missing any more, so
/// nothing is left to decide about.
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "forwards the guard decision stored beside the set"
)]
pub(super) fn keep(
    connection: &Connection,
    root: RootId,
    waiting: bool,
) -> Result<(), CatalogError> {
    release(connection, root)?;
    if waiting {
        sql::exec(
            connection,
            "INSERT INTO sync_deletion_held SELECT root_id,bucket,incarnation,name FROM sync_deletion_members WHERE root_id=?1 AND done=0",
            [root],
        )?;
    }
    Ok(())
}

/// Forgets the files that wait, once someone approved or rejected their set.
pub(super) fn release(connection: &Connection, root: RootId) -> Result<(), CatalogError> {
    sql::exec(
        connection,
        "DELETE FROM sync_deletion_held WHERE root_id=?1",
        [root],
    )?;
    Ok(())
}
