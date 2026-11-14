//! Frozen root-wide deletion inventories; membership is consumed in the object-delete transaction.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::super::unavailable;
use super::{
    OpaqueId, Ordinal, PageLimit, SyncBinding,
    root::{RootError, RootId, RootLease},
};
use crate::storage::catalog::Incarnation;
use crate::storage::catalog::sql;
use crate::{
    bucket::CatalogError,
    object::{ObjectError, ObjectRecord},
    storage::{
        admin::{AdminError, BucketName, Generation, ObjectName},
        change_journal::JournalFence,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(in crate::storage) mod history;
mod scan;
#[cfg(test)]
mod tests;

pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE sync_deletion_sets (
 root_id BLOB PRIMARY KEY REFERENCES sync_roots(root_id),
 revision TEXT NOT NULL UNIQUE CHECK(length(revision)=32),
 scan_revision TEXT NOT NULL CHECK(length(scan_revision)=32),
 sequence INTEGER NOT NULL CHECK(sequence>=0),
 epoch INTEGER NOT NULL CHECK(epoch>=0),
 execution_sequence INTEGER NOT NULL CHECK(execution_sequence>=sequence),
 baseline_count INTEGER NOT NULL CHECK(baseline_count>=0),
 candidate_count INTEGER NOT NULL CHECK(candidate_count BETWEEN 0 AND baseline_count),
 guard_enabled INTEGER NOT NULL CHECK(guard_enabled IN (0,1)),
 guarded INTEGER NOT NULL CHECK(guarded IN (0,1)),
 complete INTEGER NOT NULL CHECK(complete IN (0,1)),
 valid INTEGER NOT NULL CHECK(valid IN (0,1)),
 authorization INTEGER NOT NULL CHECK(authorization IN (0,1,2))
) WITHOUT ROWID;
CREATE TABLE sync_deletion_members (
 root_id BLOB NOT NULL REFERENCES sync_deletion_sets(root_id) ON DELETE CASCADE,
 ordinal INTEGER NOT NULL CHECK(ordinal>0),
 bucket TEXT NOT NULL COLLATE BINARY,
 name TEXT NOT NULL COLLATE BINARY,
 owner_project TEXT NOT NULL,
 incarnation TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation>0),
 path TEXT NOT NULL COLLATE BINARY,
 done INTEGER NOT NULL CHECK(done IN (0,1)),
 PRIMARY KEY(root_id,ordinal),UNIQUE(root_id,bucket,name)
) WITHOUT ROWID;
ALTER TABLE sync_findings ADD COLUMN owner_project TEXT CHECK(owner_project IS NULL OR length(owner_project) BETWEEN 1 AND 256);
UPDATE sync_findings SET owner_project=(SELECT b.project FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=sync_findings.bucket AND i.incarnation=sync_findings.incarnation);
CREATE TABLE sync_finding_owners (
 ordinal INTEGER NOT NULL REFERENCES sync_findings(ordinal) ON DELETE CASCADE,
 incarnation TEXT NOT NULL CHECK(length(incarnation)=32),
 owner_project TEXT CHECK(owner_project IS NULL OR length(owner_project) BETWEEN 1 AND 256),
 PRIMARY KEY(ordinal,incarnation)
) WITHOUT ROWID;
INSERT INTO sync_finding_owners SELECT ordinal,incarnation,owner_project FROM sync_findings;
PRAGMA user_version=40;";

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct DeletionSet {
    pub revision: OpaqueId,
    pub scan_revision: OpaqueId,
    pub baseline_count: u64,
    pub candidate_count: u64,
    pub guard_enabled: bool,
    pub guarded: bool,
    pub completed_count: u64,
    pub journal_fence: DeletionFence,
    #[serde(skip)]
    pub(crate) authorization: DeletionAuthorization,
}

/// Whether a completed deletion set may run, as stored in `sync_deletion_sets.authorization`.
/// No release writes the third value the column check allows, so reading it fails as corrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeletionAuthorization {
    /// Not yet approved (a guarded set) or invalidated.
    Withheld,
    /// Approved by an operator, or never guarded.
    Granted,
}
sql::integer_enum!(DeletionAuthorization, { Withheld => 0, Granted => 1 });

impl DeletionAuthorization {
    /// A guarded set waits for approval; an unguarded one is authorized when it completes.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "forwards the stored guarded column written beside this value"
    )]
    pub(super) fn on_completion(guarded: bool) -> Self {
        if guarded {
            Self::Withheld
        } else {
            Self::Granted
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct DeletionFence {
    pub sequence: u64,
    pub epoch: i64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Candidate {
    pub bucket: BucketName,
    #[serde(rename = "object")]
    pub name: ObjectName,
    pub owner_project: String,
    pub bucket_incarnation: Incarnation,
    pub generation: Generation,
    #[serde(skip)]
    pub path: String,
    #[serde(skip)]
    pub ordinal: Ordinal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Permit {
    root: RootId,
    revision: OpaqueId,
    ordinal: Ordinal,
}

#[derive(Clone, Copy)]
pub(crate) enum Guard {
    Enabled,
    Disabled,
}
impl From<bool> for Guard {
    fn from(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

pub(crate) fn guarded(guard: Guard, baseline: u64, candidates: u64) -> bool {
    matches!(guard, Guard::Enabled)
        && candidates >= 100
        && u128::from(candidates) * 100 > u128::from(baseline) * 50
}

fn fence(connection: &Connection) -> Result<JournalFence, CatalogError> {
    super::enabled_fence(connection).map_err(unavailable)
}

fn matching(connection: &Connection, root: RootId, revision: OpaqueId) -> Result<(), CatalogError> {
    let valid: bool = sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM sync_deletion_sets d JOIN sync_roots r ON r.root_id=d.root_id JOIN change_journal_state j ON j.singleton=1 WHERE d.root_id=?1 AND d.revision=?2 AND d.valid=1 AND d.complete=1 AND r.complete=1 AND j.enabled=1 AND j.epoch=d.epoch AND j.sequence=d.execution_sequence)",
        params![root, revision],
    )?;
    if valid {
        Ok(())
    } else {
        Err(CatalogError::PreconditionFailed)
    }
}

pub(super) fn read_set(
    connection: &Connection,
    root: RootId,
) -> Result<Option<DeletionSet>, CatalogError> {
    sql::row(connection, "SELECT revision,scan_revision,baseline_count,candidate_count,guard_enabled,guarded,authorization,d.sequence,d.epoch,(SELECT count(*) FROM sync_deletion_members m WHERE m.root_id=d.root_id AND m.done=1) FROM sync_deletion_sets d JOIN change_journal_state j ON j.singleton=1 WHERE root_id=?1 AND valid=1 AND complete=1 AND j.enabled=1 AND j.epoch=d.epoch AND j.sequence=d.execution_sequence", [root],|row|Ok(DeletionSet {
            revision:row.get(0)?,scan_revision:row.get(1)?,baseline_count:row.get(2)?,candidate_count:row.get(3)?,guard_enabled:row.get(4)?,guarded:row.get(5)?,authorization:row.get(6)?,journal_fence:DeletionFence {sequence:row.get(7)?,epoch:row.get(8)?},completed_count:row.get(9)?,
        })).optional().map_err(unavailable)
}

fn read_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<Candidate> {
    Ok(Candidate {
        bucket: row.get(0)?,
        name: row.get(1)?,
        owner_project: row.get(2)?,
        bucket_incarnation: row.get(3)?,
        generation: row.get(4)?,
        path: row.get(5)?,
        ordinal: row.get(6)?,
    })
}

impl RootLease {
    pub(crate) async fn invalidate_deletions(&self) -> Result<(), RootError> {
        let root = self.status().root_id;
        let scope = self.control_scope();
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                scope.authorize(&tx)?;
                sql::exec(
                    &tx,
                    "UPDATE sync_deletion_sets SET valid=0,authorization=0 WHERE root_id=?1",
                    [root],
                )?;
                tx.commit().map_err(unavailable)
            })
            .await?;
        Ok(())
    }

    pub(crate) async fn deletion_set(&self) -> Result<Option<DeletionSet>, RootError> {
        self.deletion_set_for(self.authority()).await
    }

    pub(crate) async fn deletion_set_for(
        &self,
        authority: super::authority::ControlAuthority,
    ) -> Result<Option<DeletionSet>, RootError> {
        let root = self.status().root_id;
        Ok(self
            .catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                authority.authorize(&tx, root)?;
                read_set(&tx, root)
            })
            .await?)
    }

    pub(crate) async fn deletion_page(
        &self,
        revision: OpaqueId,
        after: Ordinal,
        limit: PageLimit<256>,
    ) -> Result<Vec<Candidate>, RootError> {
        self.deletion_page_for(revision, after, limit, self.authority())
            .await
    }

    pub(crate) async fn deletion_page_for(
        &self,
        revision: OpaqueId,
        after: Ordinal,
        limit: PageLimit<256>,
        authority: super::authority::ControlAuthority,
    ) -> Result<Vec<Candidate>, RootError> {
        let root = self.status().root_id;
        Ok(self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;
            authority.authorize(&tx,root)?;
            matching(&tx,root,revision)?;
            tx.prepare_cached("SELECT bucket,name,owner_project,incarnation,generation,path,ordinal FROM sync_deletion_members WHERE root_id=?1 AND ordinal>?2 ORDER BY ordinal LIMIT ?3")
                .and_then(|mut statement|statement.query_map(params![root,after,limit],read_candidate)?.collect()).map_err(unavailable)
        }).await?)
    }

    pub(crate) async fn approve_deletions(
        &self,
        revision: OpaqueId,
    ) -> Result<DeletionSet, RootError> {
        self.revalidate().await?;
        let root = self.status().root_id;
        let scope = self.control_scope();
        Ok(self.catalog().run_admin(move |connection| {
            let transaction=connection.transaction()?;
            scope.authorize(&transaction)?;
            matching(&transaction,root,revision)?;
            let count=sql::exec(&transaction, "UPDATE sync_deletion_sets SET authorization=1 WHERE root_id=?1 AND revision=?2 AND authorization=0 AND guarded=1", params![root,revision])?;
            if count!=1 {return Err(CatalogError::PreconditionFailed)}
            sql::exec(&transaction,"UPDATE sync_deletion_history SET automatic_count=0 WHERE root_id=?1",[root])?;
            let snapshot=read_set(&transaction,root)?.ok_or(CatalogError::PreconditionFailed)?;
            transaction.commit()?;
            Ok(snapshot)
        }).await?)
    }

    pub(crate) fn deletion_binding(
        &self,
        binding: SyncBinding,
        revision: OpaqueId,
        ordinal: Ordinal,
    ) -> SyncBinding {
        binding.with_deletion(Permit {
            root: self.status().root_id,
            revision,
            ordinal,
        })
    }
}

/// A deletion permit that no longer matches its revision refuses the delete as a failed
/// precondition.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other catalog error keeps the shared object mapping"
)]
fn permit_error(error: CatalogError) -> ObjectError {
    match error {
        CatalogError::PreconditionFailed => ObjectError::PreconditionFailed,
        error => error.into(),
    }
}

pub(super) fn consume(
    transaction: &Transaction<'_>,
    permit: &Permit,
    record: &ObjectRecord,
) -> Result<(), ObjectError> {
    matching(transaction, permit.root, permit.revision).map_err(permit_error)?;
    let count = sql::exec(
        transaction,
        "UPDATE sync_deletion_members SET done=1 WHERE root_id=?1 AND ordinal=?2 AND bucket=?3 AND name=?4 AND generation=?5 AND done=0 AND EXISTS(SELECT 1 FROM sync_deletion_sets d WHERE d.root_id=?1 AND d.authorization=1)",
        params![
            permit.root,
            permit.ordinal,
            record.bucket,
            record.name,
            record.generation
        ],
    )?;
    if count != 1 {
        return Err(ObjectError::PreconditionFailed);
    }
    let guarded: bool = sql::scalar(
        transaction,
        "SELECT guarded FROM sync_deletion_sets WHERE root_id=?1",
        [permit.root],
    )
    .map_err(super::super::objects::object_unavailable)?;
    if !guarded {
        let changed=sql::exec(transaction,"UPDATE sync_deletion_history SET automatic_count=automatic_count+1 WHERE root_id=?1 AND automatic_count<9223372036854775807",[permit.root]).map_err(super::super::objects::object_unavailable)?;
        if changed != 1 {
            return Err(ObjectError::PreconditionFailed);
        }
    }
    sql::exec(
        transaction,
        "UPDATE sync_deletion_sets SET execution_sequence=execution_sequence+1 WHERE root_id=?1 AND execution_sequence<9223372036854775807",
        [permit.root],
    )?;
    Ok(())
}
