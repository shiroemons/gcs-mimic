//! Frozen root-wide deletion inventories; membership is consumed in the object-delete transaction.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::super::unavailable;
use super::{
    SyncBinding,
    root::{RootError, RootLease},
};
use crate::{
    bucket::CatalogError,
    object::{ObjectError, ObjectRecord},
    storage::{
        admin::AdminError,
        change_journal::{JournalFence, JournalSeq},
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

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
    pub revision: String,
    pub scan_revision: String,
    pub baseline_count: u64,
    pub candidate_count: u64,
    pub guard_enabled: bool,
    pub guarded: bool,
    pub completed_count: u64,
    pub journal_fence: DeletionFence,
    #[serde(skip)]
    pub(crate) authorization: i64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct DeletionFence {
    pub sequence: u64,
    pub epoch: i64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Candidate {
    pub bucket: String,
    #[serde(rename = "object")]
    pub name: String,
    pub owner_project: String,
    pub bucket_incarnation: String,
    pub generation: i64,
    #[serde(skip)]
    pub path: String,
    #[serde(skip)]
    pub ordinal: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Permit {
    root: [u8; 16],
    revision: String,
    ordinal: i64,
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

fn new_revision() -> Result<String, CatalogError> {
    let mut value = [0; 16];
    getrandom::fill(&mut value).map_err(unavailable)?;
    Ok(crate::hashing::encode_lower(&value))
}

fn fence(connection: &Connection) -> Result<JournalFence, CatalogError> {
    connection
        .prepare_cached(
            "SELECT sequence,epoch FROM change_journal_state WHERE singleton=1 AND enabled=1",
        )
        .and_then(|mut statement| {
            statement.query_row([], |row| {
                Ok(JournalFence {
                    sequence: JournalSeq(row.get::<_, i64>(0)? as u64),
                    revision: row.get(1)?,
                })
            })
        })
        .map_err(unavailable)
}

fn matching(connection: &Connection, root: [u8; 16], revision: &str) -> Result<(), CatalogError> {
    let valid:bool=connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_deletion_sets d JOIN sync_roots r ON r.root_id=d.root_id JOIN change_journal_state j ON j.singleton=1 WHERE d.root_id=?1 AND d.revision=?2 AND d.valid=1 AND d.complete=1 AND r.complete=1 AND j.enabled=1 AND j.epoch=d.epoch AND j.sequence=d.execution_sequence)")
        .and_then(|mut statement|statement.query_row(params![root,revision],|row|row.get(0))).map_err(unavailable)?;
    if valid {
        Ok(())
    } else {
        Err(CatalogError::PreconditionFailed)
    }
}

fn read_set(connection: &Connection, root: [u8; 16]) -> Result<Option<DeletionSet>, CatalogError> {
    connection.prepare_cached("SELECT revision,scan_revision,baseline_count,candidate_count,guard_enabled,guarded,authorization,d.sequence,d.epoch,(SELECT count(*) FROM sync_deletion_members m WHERE m.root_id=d.root_id AND m.done=1) FROM sync_deletion_sets d JOIN change_journal_state j ON j.singleton=1 WHERE root_id=?1 AND valid=1 AND complete=1 AND j.enabled=1 AND j.epoch=d.epoch AND j.sequence=d.execution_sequence")
        .and_then(|mut statement|statement.query_row([root],|row|Ok(DeletionSet {
            revision:row.get(0)?,scan_revision:row.get(1)?,baseline_count:row.get::<_,i64>(2)? as u64,candidate_count:row.get::<_,i64>(3)? as u64,guard_enabled:row.get(4)?,guarded:row.get(5)?,authorization:row.get(6)?,journal_fence:DeletionFence {sequence:row.get::<_,i64>(7)? as u64,epoch:row.get(8)?},completed_count:row.get::<_,i64>(9)? as u64,
        })).optional()).map_err(unavailable)
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
        let root = self.status().root_id.as_bytes();
        let scope = self.control_scope();
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction().map_err(unavailable)?;
                scope.authorize(&tx)?;
                tx.prepare_cached(
                    "UPDATE sync_deletion_sets SET valid=0,authorization=0 WHERE root_id=?1",
                )
                .and_then(|mut statement| statement.execute([root]))
                .map_err(unavailable)?;
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
        let root = self.status().root_id.as_bytes();
        Ok(self
            .catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction().map_err(unavailable)?;
                authority.authorize(&tx, root)?;
                read_set(&tx, root)
            })
            .await?)
    }

    pub(crate) async fn deletion_page(
        &self,
        revision: String,
        after: i64,
        limit: usize,
    ) -> Result<Vec<Candidate>, RootError> {
        self.deletion_page_for(revision, after, limit, self.authority())
            .await
    }

    pub(crate) async fn deletion_page_for(
        &self,
        revision: String,
        after: i64,
        limit: usize,
        authority: super::authority::ControlAuthority,
    ) -> Result<Vec<Candidate>, RootError> {
        if after < 0 || !(1..=256).contains(&limit) {
            return Err(AdminError::Invalid("invalid deletion page".into()).into());
        }
        let root = self.status().root_id.as_bytes();
        Ok(self.catalog().run_admin(move |connection| {
            let tx=connection.transaction().map_err(unavailable)?;
            authority.authorize(&tx,root)?;
            matching(&tx,root,&revision)?;
            tx.prepare_cached("SELECT bucket,name,owner_project,incarnation,generation,path,ordinal FROM sync_deletion_members WHERE root_id=?1 AND ordinal>?2 ORDER BY ordinal LIMIT ?3")
                .and_then(|mut statement|statement.query_map(params![root,after,limit as i64],read_candidate)?.collect()).map_err(unavailable)
        }).await?)
    }

    pub(crate) async fn approve_deletions(
        &self,
        revision: String,
    ) -> Result<DeletionSet, RootError> {
        self.file_job(|_| Ok(())).await?;
        let root = self.status().root_id.as_bytes();
        let scope = self.control_scope();
        Ok(self.catalog().run_admin(move |connection| {
            let transaction=connection.transaction().map_err(unavailable)?;
            scope.authorize(&transaction)?;
            matching(&transaction,root,&revision)?;
            let count=transaction.prepare_cached("UPDATE sync_deletion_sets SET authorization=1 WHERE root_id=?1 AND revision=?2 AND authorization=0 AND guarded=1")
                .and_then(|mut statement|statement.execute(params![root,revision])).map_err(unavailable)?;
            if count!=1 {return Err(CatalogError::PreconditionFailed)}
            let snapshot=read_set(&transaction,root)?.ok_or(CatalogError::PreconditionFailed)?;
            transaction.commit().map_err(unavailable)?;
            Ok(snapshot)
        }).await?)
    }

    pub(crate) fn deletion_binding(
        &self,
        mut binding: SyncBinding,
        revision: String,
        ordinal: i64,
    ) -> SyncBinding {
        binding.deletion = Some(Permit {
            root: self.status().root_id.as_bytes(),
            revision,
            ordinal,
        });
        binding
    }
}

pub(super) fn consume(
    transaction: &Transaction<'_>,
    permit: &Permit,
    record: &ObjectRecord,
) -> Result<(), ObjectError> {
    matching(transaction, permit.root, &permit.revision).map_err(|error| match error {
        CatalogError::PreconditionFailed => ObjectError::PreconditionFailed,
        error => super::super::objects::object_unavailable(error),
    })?;
    let count=transaction.prepare_cached("UPDATE sync_deletion_members SET done=1 WHERE root_id=?1 AND ordinal=?2 AND bucket=?3 AND name=?4 AND generation=?5 AND done=0 AND EXISTS(SELECT 1 FROM sync_deletion_sets d WHERE d.root_id=?1 AND d.authorization=1)")
        .and_then(|mut statement|statement.execute(params![permit.root,permit.ordinal,record.bucket,record.name,record.generation])).map_err(super::super::objects::object_unavailable)?;
    if count != 1 {
        return Err(ObjectError::PreconditionFailed);
    }
    transaction.prepare_cached("UPDATE sync_deletion_sets SET execution_sequence=execution_sequence+1 WHERE root_id=?1 AND execution_sequence<9223372036854775807")
        .and_then(|mut statement|statement.execute([permit.root])).map_err(super::super::objects::object_unavailable)?;
    Ok(())
}
