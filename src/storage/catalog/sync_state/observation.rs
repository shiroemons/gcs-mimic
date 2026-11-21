//! Revision-bound observations include durable mapping findings and unfinished write-back work.

use super::{
    OpaqueId, Ordinal, PageLimit,
    conflicts::RecoveryPhase,
    findings::{FindingEffect, FindingKind},
    root::{RootError, RootId, RootLease},
    writeback,
};
use crate::storage::catalog::Incarnation;
use crate::storage::catalog::sql;
use crate::{
    bucket::CatalogError,
    storage::{
        admin::AdminError,
        failure::{IoCause, unavailable},
    },
};
use rusqlite::{Connection, OptionalExtension, params};

pub(in crate::storage) const SCHEMA: &str = include_str!("observation/schema.sql");

/// Schema 59 stops advancing the observation revision for state the findings view does not
/// list: the baseline flag on `sync_roots`, the deletion sets and their members, and the
/// tracked files. The revision is the `findings_revision` a client pages the findings view by,
/// so a change it cannot see there made a page it had just been given stale: a late directory
/// notification invalidated and then confirmed an unchanged deletion set, and the findings view
/// a wait had named answered 409. The deletion view pages by the set's own revision.
pub(in crate::storage) const REVISION_SCHEMA_VERSION: i64 = 59;
pub(in crate::storage) const REVISION_SCHEMA: &str = "DROP TRIGGER sync_observation_root_update;
DROP TRIGGER sync_observation_deletion_insert;
DROP TRIGGER sync_observation_deletion_update;
DROP TRIGGER sync_observation_deletion_delete;
DROP TRIGGER sync_observation_member_update;
DROP TRIGGER sync_observation_object_insert;
DROP TRIGGER sync_observation_object_update;
DROP TRIGGER sync_observation_object_delete;
PRAGMA user_version = 59;";

pub(super) mod publication;
pub(crate) use publication::Publication;
mod stops;
pub(crate) use stops::{AliasEdit, ListedStop, PathStop, TransientStop, Transition};
pub(in crate::storage) use stops::{CLEAR_STOP, STOPPED};
mod types;
pub(crate) use types::{FindingCategory, FindingReason, PauseReason};

/// Why the last scan ended without pausing: the root keeps running and the next scan tries
/// again. Only these two source conditions end a scan this way; anything else pauses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScanStop {
    SourceChanging,
    SourceUnrepresentable,
}
sql::text_enum!(pub(crate) ScanStop, "invalid synchronization scan stop", {
    SourceChanging => "SourceChanging", SourceUnrepresentable => "SourceUnrepresentable",
});

crate::serde_util::serialize_as_str!(ScanStop);

/// A scan stop and a pause name the same source condition alike.
impl From<ScanStop> for PauseReason {
    fn from(stop: ScanStop) -> Self {
        match stop {
            ScanStop::SourceChanging => Self::SourceChanging,
            ScanStop::SourceUnrepresentable => Self::SourceUnrepresentable,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Summary {
    pub revision: i64,
    pub baseline_complete: bool,
    pub tracked_objects: u64,
    /// Every mapping finding, whatever it does to `sync wait`.
    pub mapping_findings: u64,
    /// The mapping findings that stop `sync wait`.
    pub blocking_findings: u64,
    /// The mapping findings that `sync wait` reports without stopping: objects the API holds
    /// whose names the tree cannot.
    pub unmapped_findings: u64,
    pub pending_writeback: u64,
    /// The files of a rejected deletion set not written back yet. The mirror writes them back
    /// right after the rejection, and after a stop or a failure before it looks for deleted
    /// files again, so a count that stays names a write-back that keeps failing, which the
    /// pause it leaves explains.
    pub rejected_writeback: u64,
    pub path_stops: u64,
    pub unresolved_conflicts: u64,
    pub paused_reason: Option<PauseReason>,
    pub scan_stop: Option<ScanStop>,
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

/// Where a findings page resumes: after this category's ordinal, in `PAGE` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObservationCursor {
    pub category: FindingCategory,
    pub ordinal: Ordinal,
}

impl ObservationCursor {
    /// Before every finding.
    pub(crate) const START: Self = Self {
        category: FindingCategory::Mapping,
        ordinal: Ordinal::START,
    };
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Item {
    #[serde(skip)]
    pub ordinal: Ordinal,
    pub category: FindingCategory,
    pub bucket: Option<String>,
    pub object: Option<String>,
    pub bucket_incarnation: Option<Incarnation>,
    pub generation: Option<i64>,
    pub reason: FindingReason,
    pub operation_id: Option<OpaqueId>,
    pub owner_project: Option<String>,
    pub recovery: Option<RecoveryDetails>,
    pub conflict_id: Option<OpaqueId>,
    pub conflict_revision: Option<OpaqueId>,
    /// The path under which the tree holds the file of an alias finding's object, or the file
    /// whose edit an `AliasEdited` stop keeps out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_path: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct RecoveryDetails {
    pub recovery_revision: OpaqueId,
    pub target_identity: OpaqueId,
    pub phase: RecoveryPhase,
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
    let recovery = if matches!(
        reason,
        FindingReason::Writeback(writeback::Phase::Protected)
    ) {
        let bytes: Vec<u8> = row.get(12)?;
        let observed: RetainedProjection = serde_json::from_slice(&bytes)
            .map_err(|error| sql::conversion_error(12, rusqlite::types::Type::Blob, error))?;
        Some(RecoveryDetails {
            recovery_revision: row.get(10)?,
            target_identity: row.get(11)?,
            phase: row.get(13)?,
            retained_bytes: observed.size,
            hashes: observed.hashes,
        })
    } else {
        None
    };
    // Column 0 is `PAGE`'s sort key, the category's rank, which `category` already names.
    Ok(Item {
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
        tree_path: row.get(16)?,
    })
}

fn revision(connection: &Connection, root: RootId) -> Result<i64, CatalogError> {
    sql::scalar(
        connection,
        "SELECT revision FROM sync_observations WHERE root_id=?1",
        [root],
    )
    .map_err(unavailable)
}

/// Advances only when a mapping finding of `root` (a `sync_findings` row) is added, changed or
/// removed. The observation revision, which the status `findings_revision` reports, also follows
/// every stop, write-back operation and conflict that the findings view lists, so an export
/// moves it while its write-back is under way.
fn mapping_findings_revision(connection: &Connection, root: RootId) -> Result<i64, CatalogError> {
    sql::scalar(
        connection,
        "SELECT mapping_findings_revision FROM sync_observations WHERE root_id=?1",
        [root],
    )
    .map_err(unavailable)
}

/// The observation counters `read_summary` reads in one statement.
struct Totals {
    baseline_complete: bool,
    tracked_objects: u64,
    mapping_findings: u64,
    pending_writeback: u64,
    paused_reason: Option<PauseReason>,
    path_stops: u64,
    unresolved_conflicts: u64,
    scan_stop: Option<ScanStop>,
}

impl Totals {
    const SELECT: &str = "SELECT (SELECT complete FROM sync_roots WHERE root_id=?1),(SELECT count(*) FROM sync_objects),(SELECT count(*) FROM sync_findings WHERE root_id=?1),(SELECT count(*) FROM sync_writeback_ops WHERE root_id=?1 AND phase!=4),paused_reason,(SELECT count(*) FROM sync_path_stops WHERE root_id=?1),(SELECT count(*) FROM sync_conflicts WHERE root_id=?1 AND state!=2),scan_stop FROM sync_observations WHERE root_id=?1";

    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            baseline_complete: row.get(0)?,
            tracked_objects: row.get(1)?,
            mapping_findings: row.get(2)?,
            pending_writeback: row.get(3)?,
            paused_reason: row.get(4)?,
            path_stops: row.get(5)?,
            unresolved_conflicts: row.get(6)?,
            scan_stop: row.get(7)?,
        })
    }
}

/// A root's mapping findings counted by what each does to `sync wait`. The statement groups by
/// the stored reason and [`FindingKind::effect`] decides each group, so the decision lives in one
/// exhaustive match and a new kind needs no change here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct EffectCounts {
    blocking: u64,
    unmapped: u64,
}

impl EffectCounts {
    /// Each group with its count and how many of its findings name an object the API still
    /// holds. The API deletes objects in many transactions, and the mirror removes a name finding
    /// only when it replays the deletion, so a finding can outlive its object until then.
    const SELECT: &str = "SELECT f.reason,count(*),sum(EXISTS(SELECT 1 FROM objects o WHERE o.bucket=f.bucket AND o.name=f.name AND o.time_deleted IS NULL)) FROM sync_findings f WHERE f.root_id=?1 GROUP BY f.reason";

    fn read(connection: &Connection, root: RootId) -> Result<Self, CatalogError> {
        let mut counts = Self::default();
        let mut statement = connection.prepare_cached(Self::SELECT)?;
        let groups = statement.query_map([root], |row| {
            Ok((
                row.get::<_, FindingKind>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, u64>(2)?,
            ))
        })?;
        for group in groups {
            let (kind, total, live) = group.map_err(unavailable)?;
            counts.add(kind.effect(), total, live);
        }
        Ok(counts)
    }

    /// Adds a group of `total` findings, `live` of which name an object the API holds. A finding
    /// that stops a wait counts until it is resolved, whether or not its object remains; one
    /// that reports a name the tree cannot hold counts only while the object it names remains,
    /// since a deleted object is not one the API keeps out of the tree.
    fn add(&mut self, effect: FindingEffect, total: u64, live: u64) {
        match effect {
            FindingEffect::Blocks => self.blocking += total,
            FindingEffect::Reports => self.unmapped += live,
            FindingEffect::Silent => {}
        }
    }
}

fn read_summary(
    connection: &Connection,
    root: super::root::RootId,
) -> Result<Summary, CatalogError> {
    let revision = revision(connection, root)?;
    let totals = sql::row(connection, Totals::SELECT, [root], Totals::read)?;
    let effects = EffectCounts::read(connection, root)?;
    let exists: bool = sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM sync_writeback_roots WHERE root_id=?1)",
        [root],
    )?;
    let writeback = if exists {
        Some(
            writeback::observation(connection, root)
                .map_err(|error| CatalogError::Io(IoCause::other(error)))?,
        )
    } else {
        None
    };
    Ok(Summary {
        revision,
        baseline_complete: totals.baseline_complete,
        tracked_objects: totals.tracked_objects,
        mapping_findings: totals.mapping_findings,
        blocking_findings: effects.blocking,
        unmapped_findings: effects.unmapped,
        pending_writeback: totals.pending_writeback,
        rejected_writeback: super::deletions::rejected_remaining(connection, root)?,
        path_stops: totals.path_stops,
        unresolved_conflicts: totals.unresolved_conflicts,
        paused_reason: totals.paused_reason,
        scan_stop: totals.scan_stop,
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
                authority.authorize(&tx, root)?;
                read_summary(&tx, root)
            })
            .await
            .map_err(RootError::from)
    }

    /// The revision of this root's mapping findings, read under `authority`.
    #[cfg_attr(
        not(all(test, unix)),
        expect(
            dead_code,
            reason = "only the Unix unit tests read it until a reader of mapping findings is bound to it"
        )
    )]
    pub(crate) async fn mapping_findings_revision_for(
        &self,
        authority: super::authority::ControlAuthority,
    ) -> Result<i64, RootError> {
        let root = self.status().root_id;
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                authority.authorize(&tx, root)?;
                mapping_findings_revision(&tx, root)
            })
            .await
            .map_err(RootError::from)
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn observation_page(
        &self,
        expected: i64,
        after: ObservationCursor,
        limit: PageLimit<257>,
    ) -> Result<Vec<Item>, RootError> {
        self.observation_page_for(expected, after, limit, self.authority())
            .await
    }

    /// The limit admits one item past the largest page, which a caller reads to learn whether
    /// the findings continue.
    pub(crate) async fn observation_page_for(
        &self,
        expected: i64,
        after: ObservationCursor,
        limit: PageLimit<257>,
        authority: super::authority::ControlAuthority,
    ) -> Result<Vec<Item>, RootError> {
        let root = self.status().root_id;
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                authority.authorize(&tx, root)?;
                if revision(&tx, root)? != expected {
                    return Err(CatalogError::PreconditionFailed);
                }
                tx.prepare_cached(PAGE)?
                    .query_map(
                        params![root, after.category.rank(), after.ordinal, limit],
                        read_item,
                    )?
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
        let root = self.status().root_id;
        self.catalog().run_admin(move |connection| {
            let updated=sql::exec(connection, "UPDATE sync_observations SET revision=revision+1,paused_reason=?2 WHERE root_id=?1 AND paused_reason IS NOT ?2", params![root,reason])?;
            if updated==0 && sql::row(connection, "SELECT 1 FROM sync_observations WHERE root_id=?1", [root],|row|row.get::<_,i64>(0)).optional()?.is_none() {return Err(CatalogError::PreconditionFailed)}
            Ok(())
        }).await.map_err(RootError::from)
    }

    /// Records why the scan that just finished left the tree unsettled, or that nothing did.
    pub(crate) async fn observation_scan(&self, reason: Option<ScanStop>) -> Result<(), RootError> {
        let root = self.status().root_id;
        let scope = self.control_scope();
        self.catalog().run_admin(move |connection| {
            let tx = connection.transaction()?;
            scope.authorize(&tx)?;
            sql::exec(&tx, "UPDATE sync_observations SET revision=revision+1,scan_stop=?2 WHERE root_id=?1 AND scan_stop IS NOT ?2", params![root,reason])?;
            tx.commit().map_err(unavailable)
        }).await.map_err(RootError::from)
    }
}

const PAGE:&str="SELECT kind,ordinal,category,bucket,name,incarnation,generation,reason,id,owner,recovery_revision,target_identity,observation,recovery_phase,conflict_id,conflict_revision,tree_path FROM (
 SELECT 0 AS kind,ordinal,'mapping' AS category,bucket,name,incarnation,generation,reason,NULL AS id,owner_project AS owner,NULL AS recovery_revision,NULL AS target_identity,NULL AS observation,NULL AS recovery_phase,NULL AS conflict_id,NULL AS conflict_revision,(SELECT spelling FROM sync_finding_aliases a WHERE a.ordinal=sync_findings.ordinal) AS tree_path FROM sync_findings WHERE root_id=?1
 UNION ALL SELECT 1,w.ordinal,'writeback',w.bucket,w.name,w.incarnation,w.generation,w.phase,w.id,w.owner,r.revision,r.target_identity,r.observation,r.phase,NULL,NULL,NULL FROM sync_writeback_ops w LEFT JOIN sync_recovery_workspaces r ON r.root_id=w.root_id AND r.id=w.id WHERE w.root_id=?1 AND w.phase!=4
 UNION ALL SELECT 2,0,'root-stop',NULL,NULL,NULL,NULL,paused_reason,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL FROM sync_observations WHERE root_id=?1 AND paused_reason IS NOT NULL
 UNION ALL SELECT 2,1,'root-stop',NULL,NULL,NULL,NULL,scan_stop,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL FROM sync_observations WHERE root_id=?1 AND scan_stop IS NOT NULL
 UNION ALL SELECT 3,ordinal,'path-stop',bucket,name,incarnation,generation,reason,NULL,owner_project,NULL,NULL,NULL,NULL,NULL,NULL,CASE WHEN reason='AliasEdited' THEN (SELECT s.path FROM sync_objects s WHERE s.bucket=p.bucket AND s.name=p.name AND s.incarnation=p.incarnation) END FROM sync_path_stops p WHERE root_id=?1
 UNION ALL SELECT 4,ordinal,'conflict',bucket,name,incarnation,generation,state,id,owner,NULL,NULL,NULL,NULL,id,revision,NULL FROM sync_conflicts WHERE root_id=?1 AND state!=2
) WHERE kind>?2 OR (kind=?2 AND ordinal>?3) ORDER BY kind,ordinal LIMIT ?4";

#[cfg(all(test, unix))]
mod tests;
