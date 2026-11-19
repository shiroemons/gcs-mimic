//! Durable observations for names the working tree cannot represent.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::super::{objects, unavailable};
use super::{
    CatalogId, Ordinal, PageLimit, SyncBinding, matching,
    root::{RootError, RootId, RootLease},
};
use crate::storage::catalog::Incarnation;
use crate::storage::catalog::sql;
use crate::{
    bucket::CatalogError,
    storage::admin::{AdminError, Generation, ObjectName},
    sync::mapping::Reason,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE sync_findings (
    ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
    root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    reason TEXT NOT NULL CHECK(reason IN ('invalid-name','host-restriction','reserved','ignored','collision','unsafe-entry')),
    UNIQUE(root_id,bucket,name)
);
CREATE INDEX sync_findings_page ON sync_findings(root_id,ordinal);
PRAGMA user_version = 21;";

/// Schema 58 gives an export that met an edited file its own finding reason, apart from a name
/// collision, and gives these findings a revision of their own, which tracked files, stops,
/// write-backs and conflicts do not advance. The finding reasons also admit `alias` and the
/// path stops `EntryReplaced` and `AliasEdited`, which no code writes yet, so that the changes
/// using them need no second rebuild of these tables.
pub(in crate::storage) const REASON_SCHEMA_VERSION: i64 = 58;
/// Rebuilds `sync_findings` and `sync_path_stops` under the same names, since their checks can
/// only change with the tables. The rows, the finding owners and both `sqlite_sequence` rows
/// are kept aside in TEMP tables first: dropping `sync_findings` deletes its owners through the
/// foreign key, and dropping either table deletes its sequence row, after which a removed
/// highest ordinal would be handed out again and an old finding ID would name another finding.
/// Before schema 58 one reason covered both conditions, and a row cannot tell which it was:
/// every `collision` row becomes `unclassified-collision`, which keeps that uncertainty apart
/// from the export conflicts schema 58 records until the object is observed again.
pub(in crate::storage) const REASON_SCHEMA: &str = include_str!("findings/reason_schema.sql");

/// Schema 60 admits `directory-occupied`, the finding reason of an object whose file name a
/// directory with entries holds, apart from the name collisions an API name causes.
pub(in crate::storage) const OCCUPIED_SCHEMA_VERSION: i64 = 60;
/// Rebuilds `sync_findings` under the same name for the wider check, keeping its rows, its owners
/// and its `sqlite_sequence` row aside first for the same reasons schema 58 does. Every row keeps
/// its reason.
pub(in crate::storage) const OCCUPIED_SCHEMA: &str = include_str!("findings/occupied_schema.sql");

/// Counts the rows schemas 58 and 60 rebuild; a migration commits only if they all come back.
const REBUILT_ROWS: &str = "SELECT (SELECT count(*) FROM sync_findings),(SELECT count(*) FROM sync_finding_owners),(SELECT count(*) FROM sync_path_stops)";
/// Whether a rebuilt table, or the owners that refer to one, now breaks a foreign key.
const REBUILT_FOREIGN_KEYS: &str = "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check('sync_findings') UNION ALL SELECT 1 FROM pragma_foreign_key_check('sync_finding_owners') UNION ALL SELECT 1 FROM pragma_foreign_key_check('sync_path_stops'))";

#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration at open"
)]
fn rebuilt_rows(transaction: &Transaction<'_>) -> Result<[i64; 3], CatalogError> {
    Ok(transaction.query_row(REBUILT_ROWS, [], |row| {
        Ok([row.get(0)?, row.get(1)?, row.get(2)?])
    })?)
}

/// Upgrades to schema 58 in one transaction with foreign keys left on.
pub(in crate::storage) fn migrate_reasons(connection: &mut Connection) -> Result<(), CatalogError> {
    rebuild(
        connection,
        REASON_SCHEMA,
        Rebuild {
            lost: "schema 58 migration did not restore every finding, owner and path stop",
            orphaned: "schema 58 migration left a finding, owner or path stop without its parent row",
        },
    )
}

/// Upgrades to schema 60 in one transaction with foreign keys left on.
pub(in crate::storage) fn migrate_occupied(
    connection: &mut Connection,
) -> Result<(), CatalogError> {
    rebuild(
        connection,
        OCCUPIED_SCHEMA,
        Rebuild {
            lost: "schema 60 migration did not restore every finding, owner and path stop",
            orphaned: "schema 60 migration left a finding, owner or path stop without its parent row",
        },
    )
}

/// What a table rebuild reports when its checks fail.
#[derive(Clone, Copy)]
struct Rebuild {
    /// A row did not come back.
    lost: &'static str,
    /// A row came back without the row it refers to.
    orphaned: &'static str,
}

/// Runs `schema`, which rebuilds finding tables, in one transaction. The checks after the rebuild
/// catch a row the statements failed to restore before anything commits.
#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration at open"
)]
fn rebuild(
    connection: &mut Connection,
    schema: &str,
    failures: Rebuild,
) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    let before = rebuilt_rows(&transaction)?;
    transaction.execute_batch(schema)?;
    if rebuilt_rows(&transaction)? != before {
        return Err(unavailable(failures.lost));
    }
    let broken: bool = transaction.query_row(REBUILT_FOREIGN_KEYS, [], |row| row.get(0))?;
    if broken {
        return Err(unavailable(failures.orphaned));
    }
    transaction.commit().map_err(unavailable)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub catalog_id: CatalogId,
    pub root_id: RootId,
    pub ordinal: i64,
    pub bucket: String,
    pub name: String,
    pub(crate) incarnation: Incarnation,
    pub generation: Generation,
    pub(crate) reason: FindingKind,
}

sql::text_enum!(pub(crate) Reason, "invalid synchronization finding", {
    InvalidName => "invalid-name", HostRestriction => "host-restriction",
    Reserved => "reserved", Ignored => "ignored", Collision => "collision",
    UnsafeEntry => "unsafe-entry", DirectoryOccupied => "directory-occupied",
});

/// Why a `sync_findings` row keeps its object out of the working tree. A name the tree cannot
/// hold and an export that met an edited file are different conditions: only the second leaves
/// an external edit and an API generation that nobody has reconciled yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FindingKind {
    /// The object's name has no file in this tree.
    Unmapped(Reason),
    /// Export stopped because the file at the object's path no longer held the bytes this tree
    /// last tracked while the API object changed or was deleted; the file stays as it was.
    ExportConflict,
    /// A schema 57 `collision` row, which does not say whether it was a name collision or an
    /// export conflict. It stops the wait like an export conflict until the object is observed
    /// again, and either kind of finding then replaces it.
    UnclassifiedCollision,
}

impl FindingKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unmapped(reason) => reason.as_str(),
            Self::ExportConflict => "export-conflict",
            Self::UnclassifiedCollision => "unclassified-collision",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "export-conflict" => Some(Self::ExportConflict),
            "unclassified-collision" => Some(Self::UnclassifiedCollision),
            unmapped => Reason::parse(unmapped).map(Self::Unmapped),
        }
    }

    /// What this finding does to `sync wait`. Every kind decides here, so a new kind cannot
    /// reach the wait without a decision.
    pub(crate) const fn effect(self) -> FindingEffect {
        match self {
            Self::ExportConflict | Self::UnclassifiedCollision => FindingEffect::Blocks,
            Self::Unmapped(Reason::Ignored) => FindingEffect::Silent,
            Self::Unmapped(
                Reason::InvalidName
                | Reason::HostRestriction
                | Reason::Reserved
                | Reason::Collision
                | Reason::UnsafeEntry
                | Reason::DirectoryOccupied,
            ) => FindingEffect::Reports,
        }
    }

    /// Whether each ingestion observes this finding's object again, because its cause can go
    /// without a change of the object itself: another API object or an entry in the tree stands
    /// in its way, or a schema 57 row still waits to be classified. A name the tree cannot hold
    /// stays so until the object changes, and an export conflict waits for its resolution.
    pub(crate) const fn observed_again(self) -> bool {
        match self {
            Self::Unmapped(Reason::Collision | Reason::UnsafeEntry | Reason::DirectoryOccupied)
            | Self::UnclassifiedCollision => true,
            Self::Unmapped(
                Reason::InvalidName | Reason::HostRestriction | Reason::Reserved | Reason::Ignored,
            )
            | Self::ExportConflict => false,
        }
    }
}

sql::text_enum!(
    FindingKind,
    "invalid synchronization finding",
    FindingKind::parse
);

/// What a finding does to `sync wait`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FindingEffect {
    /// The wait cannot succeed while the finding remains: an external edit and an API
    /// generation are, or may be, both unreconciled, so the tree is not known to be up to date.
    Blocks,
    /// The wait may succeed and reports the finding: the object stays reachable through the
    /// API, and the tree cannot hold its name.
    Reports,
    /// The wait neither stops for nor reports the finding: the tree ignores the name by design,
    /// as with `.DS_Store`.
    Silent,
}

/// What a finding row says about its object: the incarnation and generation it describes, why
/// the object stays out of the tree, and the bucket project that owns the row.
#[derive(PartialEq, Eq)]
struct FindingRow {
    incarnation: Incarnation,
    generation: Generation,
    kind: FindingKind,
    owner: Option<String>,
}

/// A finding row as it stands before a name finding is recorded over it.
struct StoredFinding {
    row: FindingRow,
    /// Whether `sync_finding_owners` holds the row's owner for the row's incarnation.
    owner_recorded: bool,
}

impl StoredFinding {
    /// The current row of one object, and whether its owner is recorded for its incarnation.
    const SELECT: &str = "SELECT f.incarnation,f.generation,f.reason,f.owner_project,EXISTS(SELECT 1 FROM sync_finding_owners o WHERE o.ordinal=f.ordinal AND o.incarnation=f.incarnation AND o.owner_project IS f.owner_project) FROM sync_findings f WHERE f.root_id=?1 AND f.bucket=?2 AND f.name=?3";

    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            row: FindingRow {
                incarnation: row.get(0)?,
                generation: row.get(1)?,
                kind: row.get(2)?,
                owner: row.get(3)?,
            },
            owner_recorded: row.get(4)?,
        })
    }
}

/// What recording a name finding writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FindingWrite {
    /// The row and its owner already say what the finding would: the transaction only reads,
    /// so it neither moves the findings revision nor waits for the write lock.
    Nothing,
    /// The row stays, but its owner is missing for its incarnation.
    Owner,
    /// The row is missing or says something else.
    Row,
}

impl FindingWrite {
    /// The one decision of what recording `observed` writes over `stored`. An export conflict on
    /// the same incarnation keeps its row, whatever the generation or reason observed: the edit
    /// it holds is in neither the API nor the tree's record, and a name finding only reports.
    fn of(stored: Option<&StoredFinding>, observed: &FindingRow) -> Self {
        let Some(stored) = stored else {
            return Self::Row;
        };
        let kept = stored.row == *observed
            || stored.row.kind == FindingKind::ExportConflict
                && stored.row.incarnation == observed.incarnation;
        match (kept, stored.owner_recorded) {
            (false, _) => Self::Row,
            (true, true) => Self::Nothing,
            (true, false) => Self::Owner,
        }
    }
}

fn remember_owner(
    transaction: &rusqlite::Transaction<'_>,
    root: RootId,
    bucket: &str,
    name: &str,
) -> Result<(), CatalogError> {
    // One logical finding may survive several bucket incarnations; replacement cannot erase earlier authority.
    sql::exec(
        transaction,
        "INSERT INTO sync_finding_owners(ordinal,incarnation,owner_project) SELECT ordinal,incarnation,owner_project FROM sync_findings WHERE root_id=?1 AND bucket=?2 AND name=?3 ON CONFLICT(ordinal,incarnation) DO UPDATE SET owner_project=excluded.owner_project WHERE owner_project IS NOT excluded.owner_project",
        params![root, bucket, name],
    )?;
    Ok(())
}

impl RootLease {
    /// Conflicts may refer to a deleted API object whose synchronized body is still tracked.
    /// Recording the same conflict again writes nothing, so findings keep their revision.
    pub(crate) async fn record_export_conflict(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        generation: Generation,
    ) -> Result<(), RootError> {
        self.revalidate().await?;
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog().run_admin(move |c| {
            let tx = c.transaction()?;
            lease.control_scope().authorize(&tx)?;
            matching(&tx,&binding,binding.bucket.as_str()).map_err(unavailable)?;
            let live = objects::lookup(&tx,binding.bucket.as_str(),name.as_str()).map_err(unavailable)?.is_some_and(|stored| stored.record.generation == generation.get());
            let tracked: bool = sql::scalar(&tx, "SELECT EXISTS(SELECT 1 FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?4 AND generation=?3)", params![binding.bucket,name,generation,binding.incarnation])?;
            let valid = live || tracked;
            if valid {
                sql::exec(&tx, "INSERT INTO sync_findings(root_id,bucket,name,incarnation,generation,reason,owner_project) VALUES(?1,?2,?3,?4,?5,?6,(SELECT b.project FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?2 AND i.incarnation=?4))
                    ON CONFLICT(root_id,bucket,name) DO UPDATE SET incarnation=excluded.incarnation,generation=excluded.generation,reason=excluded.reason,owner_project=excluded.owner_project
                    WHERE incarnation!=excluded.incarnation OR generation!=excluded.generation OR reason!=excluded.reason OR owner_project IS NOT excluded.owner_project", params![root,binding.bucket,name,binding.incarnation,generation,FindingKind::ExportConflict])?;
                remember_owner(&tx,root,binding.bucket.as_str(),name.as_str())?;
            }
            tx.commit().map_err(unavailable)
        }).await?;
        Ok(())
    }

    /// Records only the selected live incarnation/generation; no filesystem path is required.
    /// An export conflict on the same incarnation keeps its row whatever the generation: the
    /// edit it holds is in neither the API nor the tree's record, and a name reason, which a
    /// wait only reports, would let the wait succeed with that edit unresolved. Resolving the
    /// conflict removes the findings of its incarnation, and a newer incarnation replaces it.
    /// An unclassified collision carried over from schema 57 is replaced, because the name
    /// reason classifies it; an edited file behind it is recorded as an export conflict again by
    /// the next ingestion. Recording what the row already says only reads, so the findings keep
    /// their revision and the mirror can observe every finding again on each pass without
    /// contending with API writes for the write lock.
    pub(crate) async fn record_finding(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        generation: Generation,
        reason: Reason,
    ) -> Result<(), RootError> {
        self.revalidate().await?;
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog().run_admin_with_error(move |c| {
            let tx = c.transaction().map_err(unavailable)?;
            lease.control_scope().authorize(&tx)?;
            matching(&tx, &binding, binding.bucket.as_str())?;
            let record = objects::lookup(&tx, binding.bucket.as_str(), name.as_str())?
                .ok_or(AdminError::Precondition)?.record;
            if record.generation != generation.get() {
                return Err(AdminError::Precondition);
            }
            let observed = FindingRow {
                incarnation: binding.incarnation,
                generation,
                kind: FindingKind::Unmapped(reason),
                owner: sql::optional_scalar(&tx, "SELECT b.project FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?1 AND i.incarnation=?2", params![binding.bucket, binding.incarnation]).map_err(unavailable)?,
            };
            let stored = sql::row(&tx, StoredFinding::SELECT, params![root, binding.bucket, name], StoredFinding::read).optional().map_err(unavailable)?;
            match FindingWrite::of(stored.as_ref(), &observed) {
                FindingWrite::Nothing => {}
                FindingWrite::Owner => remember_owner(&tx, root, binding.bucket.as_str(), name.as_str())?,
                FindingWrite::Row => {
                    sql::exec(&tx, "INSERT INTO sync_findings(root_id,bucket,name,incarnation,generation,reason,owner_project) VALUES(?1,?2,?3,?4,?5,?6,?7)
                        ON CONFLICT(root_id,bucket,name) DO UPDATE SET incarnation=excluded.incarnation,generation=excluded.generation,reason=excluded.reason,owner_project=excluded.owner_project",
                        params![root, binding.bucket, name, observed.incarnation, observed.generation, observed.kind, observed.owner]).map_err(unavailable)?;
                    remember_owner(&tx, root, binding.bucket.as_str(), name.as_str())?;
                }
            }
            tx.commit().map_err(unavailable)?;
            Ok::<_, AdminError>(())
        }).await?;
        Ok(())
    }

    /// Removes the name finding of `name` once the API no longer holds the object, checked in
    /// the same transaction: a name finding describes a live generation, and one left behind
    /// would keep counting an object that is gone. With no live object, every generation the
    /// row can name is a deleted one. An export conflict stays, since the deletion does not
    /// settle the external edit it holds, and so does a row of another incarnation. A live
    /// object, written again since the caller read it, keeps its row for the replay of that
    /// generation. Removing nothing writes nothing, so findings keep their revision.
    pub(crate) async fn remove_deleted_finding(
        &self,
        binding: SyncBinding,
        name: ObjectName,
    ) -> Result<(), RootError> {
        self.revalidate().await?;
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog().run_admin_with_error(move |c| {
            let tx = c.transaction().map_err(unavailable)?;
            lease.control_scope().authorize(&tx)?;
            matching(&tx, &binding, binding.bucket.as_str())?;
            if objects::lookup(&tx, binding.bucket.as_str(), name.as_str())?.is_some() {
                return Ok(());
            }
            sql::exec(&tx, "DELETE FROM sync_findings WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4 AND reason!=?5", params![root,binding.bucket,name,binding.incarnation,FindingKind::ExportConflict]).map_err(unavailable)?;
            tx.commit().map_err(unavailable)?;
            Ok::<_, AdminError>(())
        }).await?;
        Ok(())
    }

    /// Continue after the last ordinal; pages are bounded observations, not frozen snapshots.
    pub async fn findings_page(&self, after: i64, limit: usize) -> Result<Vec<Finding>, RootError> {
        let (Ok(after), Ok(limit)) = (
            Ordinal::try_from(after),
            PageLimit::<{ super::MAX_PAGE }>::try_from(limit),
        ) else {
            return Err(AdminError::Invalid("invalid finding page".into()).into());
        };
        self.revalidate().await?;
        let lease = self.clone();
        let status = self.status();
        self.catalog().run_admin(move |c| {
            let tx = c.transaction()?;
            lease.control_scope().authorize(&tx)?;
            tx.prepare_cached("SELECT ordinal,bucket,name,incarnation,generation,reason FROM sync_findings INDEXED BY sync_findings_page WHERE root_id=?1 AND ordinal>?2 ORDER BY ordinal LIMIT ?3")?
                .query_map(params![status.root_id,after,limit], |r| {
                    Ok(Finding {catalog_id:status.catalog_id,root_id:status.root_id,ordinal:r.get(0)?,bucket:r.get(1)?,name:r.get(2)?,incarnation:r.get(3)?,generation:r.get(4)?,reason:r.get(5)?})
                })?.collect::<rusqlite::Result<Vec<_>>>().map_err(unavailable)
        }).await.map_err(RootError::from)
    }

    /// Removal remains possible after deletion, but cannot erase a newer incarnation or observation.
    #[cfg(all(test, unix))]
    pub(crate) async fn remove_finding(&self, finding: Finding) -> Result<bool, RootError> {
        if finding.catalog_id != self.status().catalog_id
            || finding.root_id != self.status().root_id
        {
            return Err(AdminError::Precondition.into());
        }
        self.revalidate().await?;
        let lease = self.clone();
        self.catalog().run_admin(move |c| {
            let _lease = lease;
            sql::exec(c, "DELETE FROM sync_findings WHERE root_id=?1 AND ordinal=?2 AND bucket=?3 AND name=?4 AND incarnation=?5 AND generation=?6 AND reason=?7", params![finding.root_id,finding.ordinal,finding.bucket,finding.name,finding.incarnation,finding.generation,finding.reason])
                .map(|count|count == 1).map_err(unavailable)
        }).await.map_err(RootError::from)
    }

    /// Gives the finding for `name` the reason schema 58 gives a schema 57 `collision` row, as
    /// a migrated catalog holds it.
    #[cfg(all(test, unix))]
    pub(crate) async fn unclassify_finding(
        &self,
        bucket: &str,
        name: &str,
    ) -> Result<bool, RootError> {
        let root = self.status().root_id;
        let (bucket, name) = (bucket.to_owned(), name.to_owned());
        self.catalog()
            .run_admin(move |c| {
                sql::exec(
                    c,
                    "UPDATE sync_findings SET reason=?4 WHERE root_id=?1 AND bucket=?2 AND name=?3",
                    params![root, bucket, name, FindingKind::UnclassifiedCollision],
                )
                .map(|count| count == 1)
                .map_err(unavailable)
            })
            .await
            .map_err(RootError::from)
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
