//! Revision-bound observations include durable mapping findings and unfinished write-back work.

use super::{
    root::{RootError, RootLease},
    writeback,
};
use crate::storage::catalog::sql;
use crate::{
    bucket::CatalogError,
    storage::{admin::AdminError, failure::unavailable},
};
use rusqlite::{Connection, OptionalExtension, params};

pub(in crate::storage) const SCHEMA: &str = include_str!("observation/schema.sql");

pub(super) mod publication;
mod stops;
pub(crate) use stops::PathStop;
mod types;
pub(crate) use types::{FindingCategory, FindingReason, PauseReason};

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Summary {
    pub revision: i64,
    pub baseline_complete: bool,
    pub tracked_objects: u64,
    pub mapping_findings: u64,
    pub pending_writeback: u64,
    pub path_stops: u64,
    pub unresolved_conflicts: u64,
    pub paused_reason: Option<PauseReason>,
    pub scan_stop: Option<PauseReason>,
    pub writeback: Option<WritebackSummary>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct WritebackSummary {
    pub state: writeback::Stop,
    pub reserved_bytes: u128,
    pub retained_bytes: u128,
    pub entries: u64,
    pub protected_entries: u64,
    pub max_bytes: u64,
    pub max_entries: u64,
    pub over_limit: bool,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Item {
    #[serde(skip)]
    pub kind: i64,
    #[serde(skip)]
    pub ordinal: i64,
    pub category: FindingCategory,
    pub bucket: Option<String>,
    pub object: Option<String>,
    pub bucket_incarnation: Option<String>,
    pub generation: Option<i64>,
    pub reason: FindingReason,
    pub operation_id: Option<String>,
    pub owner_project: Option<String>,
    pub recovery: Option<RecoveryDetails>,
    pub conflict_id: Option<String>,
    pub conflict_revision: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct RecoveryDetails {
    pub recovery_revision: String,
    pub target_identity: String,
    pub phase: &'static str,
    pub retained_bytes: u64,
    pub hashes: Option<RetainedHashes>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct RetainedHashes {
    md5: [u8; 16],
    crc32c: u32,
}
#[derive(serde::Deserialize)]
struct RetainedProjection {
    size: u64,
    hashes: Option<RetainedHashes>,
}

#[derive(Debug, PartialEq, Eq)]
enum RecoveryPhase {
    Protected,
    DiscardAccepted,
}
sql::integer_enum!(RecoveryPhase, { Protected => 0, DiscardAccepted => 1 });

#[cfg(test)]
mod codec_tests;

fn read_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    let category = row.get(2)?;
    let reason = match category {
        FindingCategory::RootStop => FindingReason::RootStop(row.get(7)?),
        FindingCategory::Mapping => FindingReason::Mapping(row.get(7)?),
        FindingCategory::PathStop => FindingReason::PathStop(row.get(7)?),
        FindingCategory::Writeback => FindingReason::Writeback(row.get(7)?),
        FindingCategory::Conflict => FindingReason::Conflict(row.get(7)?),
    };
    let recovery = if matches!(&reason, FindingReason::Writeback(phase) if phase == "protected") {
        let bytes: Vec<u8> = row.get(12)?;
        let observed: RetainedProjection = serde_json::from_slice(&bytes)
            .map_err(|error| sql::conversion_error(12, rusqlite::types::Type::Blob, error))?;
        let phase = match row.get::<_, RecoveryPhase>(13)? {
            RecoveryPhase::Protected => "protected",
            RecoveryPhase::DiscardAccepted => "discard_accepted",
        };
        Some(RecoveryDetails {
            recovery_revision: row.get(10)?,
            target_identity: row.get(11)?,
            phase,
            retained_bytes: observed.size,
            hashes: observed.hashes,
        })
    } else {
        None
    };
    Ok(Item {
        kind: row.get(0)?,
        ordinal: row.get(1)?,
        category,
        bucket: row.get(3)?,
        object: row.get(4)?,
        bucket_incarnation: row.get(5)?,
        generation: row.get(6)?,
        reason,
        operation_id: row.get(8)?,
        owner_project: row.get(9)?,
        recovery,
        conflict_id: row.get(14)?,
        conflict_revision: row.get(15)?,
    })
}

fn revision(connection: &Connection, root: [u8; 16]) -> Result<i64, CatalogError> {
    sql::scalar(
        connection,
        "SELECT revision FROM sync_observations WHERE root_id=?1",
        [root],
    )
    .map_err(unavailable)
}

fn read_summary(
    connection: &Connection,
    root: super::root::RootId,
) -> Result<Summary, CatalogError> {
    let revision = revision(connection, root.as_bytes())?;
    let (
        baseline_complete,
        tracked_objects,
        mapping_findings,
        pending_writeback,
        paused_reason,
        path_stops,
        unresolved_conflicts,
        scan_stop,
    ) = sql::row(
        connection,
        "SELECT (SELECT complete FROM sync_roots WHERE root_id=?1),(SELECT count(*) FROM sync_objects),(SELECT count(*) FROM sync_findings WHERE root_id=?1),(SELECT count(*) FROM sync_writeback_ops WHERE root_id=?1 AND phase!=4),paused_reason,(SELECT count(*) FROM sync_path_stops WHERE root_id=?1),(SELECT count(*) FROM sync_conflicts WHERE root_id=?1 AND state!=2),scan_stop FROM sync_observations WHERE root_id=?1",
        [root.as_bytes()],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        },
    )?;
    let exists: bool = sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM sync_writeback_roots WHERE root_id=?1)",
        [root.as_bytes()],
    )?;
    let writeback = if exists {
        Some(
            writeback::observation(connection, root)
                .map_err(|error| unavailable(std::io::Error::other(error)))?,
        )
    } else {
        None
    };
    Ok(Summary {
        revision,
        baseline_complete,
        tracked_objects,
        mapping_findings,
        pending_writeback,
        path_stops,
        unresolved_conflicts,
        paused_reason,
        scan_stop,
        writeback,
    })
}

impl RootLease {
    pub(crate) async fn observation(&self) -> Result<Summary, RootError> {
        self.observation_for(self.authority()).await
    }

    pub(crate) async fn observation_for(
        &self,
        authority: super::authority::ControlAuthority,
    ) -> Result<Summary, RootError> {
        let root = self.status().root_id;
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                authority.authorize(&tx, root.as_bytes())?;
                read_summary(&tx, root)
            })
            .await
            .map_err(RootError::from)
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn observation_page(
        &self,
        expected: i64,
        after: (i64, i64),
        limit: usize,
    ) -> Result<Vec<Item>, RootError> {
        self.observation_page_for(expected, after, limit, self.authority())
            .await
    }

    pub(crate) async fn observation_page_for(
        &self,
        expected: i64,
        after: (i64, i64),
        limit: usize,
        authority: super::authority::ControlAuthority,
    ) -> Result<Vec<Item>, RootError> {
        if !(1..=257).contains(&limit) || !(0..=4).contains(&after.0) || after.1 < 0 {
            return Err(AdminError::Invalid("invalid observation page".into()).into());
        }
        let root = self.status().root_id.as_bytes();
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                authority.authorize(&tx, root)?;
                if revision(&tx, root)? != expected {
                    return Err(CatalogError::PreconditionFailed);
                }
                tx.prepare_cached(PAGE)?
                    .query_map(params![root, after.0, after.1, limit], read_item)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(unavailable)
            })
            .await
            .map_err(RootError::from)
    }

    pub(crate) async fn observation_pause(
        &self,
        reason: Option<PauseReason>,
    ) -> Result<(), RootError> {
        let root = self.status().root_id.as_bytes();
        self.catalog().run_admin(move |connection| {
            let updated=sql::exec(connection, "UPDATE sync_observations SET revision=revision+1,paused_reason=?2 WHERE root_id=?1 AND paused_reason IS NOT ?2", params![root,reason])?;
            if updated==0 && sql::row(connection, "SELECT 1 FROM sync_observations WHERE root_id=?1", [root],|row|row.get::<_,i64>(0)).optional()?.is_none() {return Err(CatalogError::PreconditionFailed)}
            Ok(())
        }).await.map_err(RootError::from)
    }

    pub(crate) async fn observation_scan(
        &self,
        unrepresentable: u64,
        deferred: u64,
    ) -> Result<(), RootError> {
        let reason = if unrepresentable > 0 {
            Some(PauseReason::SourceUnrepresentable)
        } else if deferred > 0 {
            Some(PauseReason::SourceChanging)
        } else {
            None
        };
        let root = self.status().root_id.as_bytes();
        let scope = self.control_scope();
        self.catalog().run_admin(move |connection| {
            let tx = connection.transaction()?;
            scope.authorize(&tx)?;
            sql::exec(&tx, "UPDATE sync_observations SET revision=revision+1,scan_stop=?2 WHERE root_id=?1 AND scan_stop IS NOT ?2", params![root,reason])?;
            tx.commit().map_err(unavailable)
        }).await.map_err(RootError::from)
    }
}

const PAGE:&str="SELECT kind,ordinal,category,bucket,name,incarnation,generation,reason,id,owner,recovery_revision,target_identity,observation,recovery_phase,conflict_id,conflict_revision FROM (
 SELECT 0 AS kind,ordinal,'mapping' AS category,bucket,name,incarnation,generation,reason,NULL AS id,owner_project AS owner,NULL AS recovery_revision,NULL AS target_identity,NULL AS observation,NULL AS recovery_phase,NULL AS conflict_id,NULL AS conflict_revision FROM sync_findings WHERE root_id=?1
 UNION ALL SELECT 1,w.ordinal,'writeback',w.bucket,w.name,w.incarnation,w.generation,CASE w.phase WHEN 0 THEN 'prepared' WHEN 1 THEN 'armed' WHEN 2 THEN 'protected' ELSE 'durable' END,w.id,w.owner,r.revision,r.target_identity,r.observation,r.phase,NULL,NULL FROM sync_writeback_ops w LEFT JOIN sync_recovery_workspaces r ON r.root_id=w.root_id AND r.id=w.id WHERE w.root_id=?1 AND w.phase!=4
 UNION ALL SELECT 2,0,'root-stop',NULL,NULL,NULL,NULL,paused_reason,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL FROM sync_observations WHERE root_id=?1 AND paused_reason IS NOT NULL
 UNION ALL SELECT 2,1,'root-stop',NULL,NULL,NULL,NULL,scan_stop,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL FROM sync_observations WHERE root_id=?1 AND scan_stop IS NOT NULL
 UNION ALL SELECT 3,ordinal,'path-stop',bucket,name,incarnation,generation,reason,NULL,owner_project,NULL,NULL,NULL,NULL,NULL,NULL FROM sync_path_stops WHERE root_id=?1
 UNION ALL SELECT 4,ordinal,'conflict',bucket,name,incarnation,generation,CASE state WHEN 0 THEN 'capturing' ELSE 'registered' END,id,owner,NULL,NULL,NULL,NULL,id,revision FROM sync_conflicts WHERE root_id=?1 AND state!=2
) WHERE kind>?2 OR (kind=?2 AND ordinal>?3) ORDER BY kind,ordinal LIMIT ?4";

#[cfg(all(test, unix))]
mod tests;
