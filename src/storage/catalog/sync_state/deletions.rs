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

/// The set `d` waits for an approval or rejection: complete, valid, guarded and neither
/// approved nor rejected.
macro_rules! awaiting {
    () => {
        "d.complete=1 AND d.valid=1 AND d.guarded=1 AND d.authorization=0"
    };
}

/// No API change since the set `d`, joined with the journal state `j`, was taken may have made
/// deleting its members do more than the approver saw: an update of a member's bucket in that
/// incarnation, where versioning and soft delete decide whether a deleted object stays
/// recoverable. The journal does not record which setting an update changed, so every such
/// update counts, a label as much as versioning; a journal compacted past the set counts too.
/// A hold or retention on a member's object only stops its deletion, which then restores the
/// file, so a metadata change of a member does not count; a publish or delete of one changes
/// its generation or liveness, which the inventory compares itself.
macro_rules! unaffected {
    () => {
        "j.floor<=d.sequence AND NOT EXISTS(SELECT 1 FROM change_journal c WHERE c.sequence>d.sequence AND c.kind='bucket-update' AND EXISTS(SELECT 1 FROM sync_deletion_members m WHERE m.root_id=d.root_id AND m.done=0 AND m.bucket=c.bucket AND m.incarnation=c.incarnation))"
    };
}

/// The set `d`, joined with the journal state `j`, can be read: taken in the current journal
/// epoch and, unless it waits for a decision, at the journal's current sequence. A set that
/// waits stays readable after an API write moved the journal, so that the status and the
/// deletions view keep naming the revision a decision is asked for; the decision inventories
/// the tree at the current fence before it applies (`begin_deletion_recheck`), and only a set
/// carried to that fence is decided ([`matching`]).
macro_rules! readable {
    () => {
        concat!(
            "d.valid=1 AND d.complete=1 AND j.enabled=1 AND j.epoch=d.epoch AND (j.sequence=d.execution_sequence OR (",
            awaiting!(),
            " AND d.execution_sequence=d.sequence AND d.sequence<j.sequence))"
        )
    };
}

pub(in crate::storage) mod held;
pub(in crate::storage) mod history;
mod scan;
pub(crate) use scan::{Confirmed, Inventory, Recheck};
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeletionAuthorization {
    /// Not yet approved (a guarded set), or withdrawn when the catalog reopened. Invalidating a
    /// set keeps this value, but an invalid set runs nothing until an inventory confirms it.
    Withheld,
    /// Approved by an operator, or never guarded.
    Granted,
    /// Rejected by an operator. The set is invalid and never runs; its members are files to
    /// write back from their objects, each marked done once its write-back ended. The value
    /// survives a restart, and no inventory replaces the set while a member is not done: one
    /// that ran in between would take the files not yet written back for files the user
    /// deleted.
    Rejected,
}
sql::integer_enum!(DeletionAuthorization, { Withheld => 0, Granted => 1, Rejected => 2 });

/// What the write-back of a rejected set has left: its revision, and how many of its members
/// are not written back yet.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingRejection {
    pub revision: OpaqueId,
    pub remaining: u64,
}

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

/// What an inventory saw gone above its candidates. Every tracked file of a bucket gone at once
/// needs approval whatever the counts: a mount missing at startup, one that came up empty, or a
/// directory moved away looks exactly like the user deleting the files, and putting it back
/// brings every file back. Deleting files inside a bucket directory needs approval only by the
/// counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Removal {
    #[default]
    Files,
    Bucket(BucketGone),
}

/// The tracked files, by path, of each bucket that the first complete inventory while this
/// process owned the root found all gone, as later inventories still find them. Only these files
/// tell whether such a bucket stays emptied: a file the tree gained since, such as the export of
/// an object written through the API, does not.
pub(crate) type EmptiedAtStart =
    std::collections::BTreeMap<BucketName, std::collections::BTreeSet<String>>;

/// How every tracked file of one bucket went at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BucketGone {
    /// The bucket's directory is missing.
    Missing,
    /// None of the files the tree tracked in the bucket when the first inventory of this process
    /// ran, which takes in what changed while the server was stopped, has been there since. A
    /// bucket emptied while the mirror runs is a deletion the user made in front of it, which
    /// only the counts take to approval.
    Emptied,
}

impl Removal {
    /// Whether this removal alone takes the set to approval under `guard`: the one decision for
    /// every way a bucket's files go at once, which the reason does not change.
    fn guarded(self, guard: Guard) -> bool {
        match (guard, self) {
            (Guard::Enabled, Self::Bucket(BucketGone::Missing | BucketGone::Emptied)) => true,
            (Guard::Enabled, Self::Files) | (Guard::Disabled, _) => false,
        }
    }

    /// What two observations saw gone together.
    #[must_use]
    pub(crate) fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Files, Self::Files) => Self::Files,
            (Self::Bucket(gone), _) | (_, Self::Bucket(gone)) => Self::Bucket(gone),
        }
    }
}

fn fence(connection: &Connection) -> Result<JournalFence, CatalogError> {
    super::enabled_fence(connection).map_err(unavailable)
}

/// Fails unless the root's set is `revision`, valid and at the journal's current sequence, as a
/// decision and an execution need: the inventory before a decision carries a set that waits to
/// that sequence, so nothing is decided about members the tree was not compared with since
/// the last API write.
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

/// Fails unless the root's set is `revision`, [`readable!`] and [`unaffected!`], as a page of
/// the deletions view needs: a set that waits for a decision stays inspectable after an API
/// write that touched none of its members, and the pages of one whose members' bucket was
/// updated are refused, since the decision on them will be.
fn inspectable(
    connection: &Connection,
    root: RootId,
    revision: OpaqueId,
) -> Result<(), CatalogError> {
    let valid: bool = sql::scalar(
        connection,
        concat!(
            "SELECT EXISTS(SELECT 1 FROM sync_deletion_sets d JOIN sync_roots r ON r.root_id=d.root_id JOIN change_journal_state j ON j.singleton=1 WHERE d.root_id=?1 AND d.revision=?2 AND r.complete=1 AND ",
            readable!(),
            " AND ",
            unaffected!(),
            ")"
        ),
        params![root, revision],
    )?;
    if valid {
        Ok(())
    } else {
        Err(CatalogError::PreconditionFailed)
    }
}

/// Whether the root's set is rejected and has a member not written back yet.
fn rejection_pending(connection: &Connection, root: RootId) -> Result<bool, CatalogError> {
    Ok(sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM sync_deletion_sets d JOIN sync_deletion_members m ON m.root_id=d.root_id WHERE d.root_id=?1 AND d.authorization=2 AND m.done=0)",
        [root],
    )?)
}

/// The root's set while it is [`readable!`]: at the journal's current sequence, or waiting for a
/// decision in the current epoch.
pub(super) fn read_set(
    connection: &Connection,
    root: RootId,
) -> Result<Option<DeletionSet>, CatalogError> {
    sql::row(connection, concat!("SELECT revision,scan_revision,baseline_count,candidate_count,guard_enabled,guarded,authorization,d.sequence,d.epoch,(SELECT count(*) FROM sync_deletion_members m WHERE m.root_id=d.root_id AND m.done=1) FROM sync_deletion_sets d JOIN change_journal_state j ON j.singleton=1 WHERE d.root_id=?1 AND ", readable!()), [root],|row|Ok(DeletionSet {
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
    /// Withdraws the root's set from inspection, approval and execution. Only a valid set is
    /// written, so invalidating twice changes nothing the second time. The set keeps whether it
    /// was approved: an inventory that finds the same members may restore an unapproved set, but
    /// never an approved one, which would let its approval be used twice.
    pub(crate) async fn invalidate_deletions(&self) -> Result<(), RootError> {
        self.withdraw_deletions(
            "UPDATE sync_deletion_sets SET valid=0 WHERE root_id=?1 AND valid=1",
        )
        .await
    }

    /// Withdraws the root's set because the tree changed, unless the set waits for an approval
    /// or rejection. Such a set stays inspectable and decidable: a decision inventories the tree
    /// again before it applies ([`Self::begin_deletion_recheck`]), and the pass the change brings
    /// keeps the revision when it finds the same members. Withdrawing it made the command that
    /// the status had just named fail as stale until that pass, whenever a late notification
    /// about some other path arrived. Every other set is withdrawn as by
    /// [`Self::invalidate_deletions`].
    pub(crate) async fn unsettle_deletions(&self) -> Result<(), RootError> {
        self.withdraw_deletions("UPDATE sync_deletion_sets SET valid=0 WHERE root_id=?1 AND valid=1 AND (guarded=0 OR authorization!=0)")
            .await
    }

    async fn withdraw_deletions(&self, statement: &'static str) -> Result<(), RootError> {
        let root = self.status().root_id;
        let scope = self.control_scope();
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                scope.authorize(&tx)?;
                sql::exec(&tx, statement, [root])?;
                tx.commit().map_err(unavailable)
            })
            .await?;
        Ok(())
    }

    /// The root's baseline as the catalog holds it now. The lease's status keeps what it was when
    /// the lease was activated, which a baseline completed since then does not change.
    pub(crate) async fn current_baseline(&self) -> Result<super::root::Baseline, RootError> {
        let root = self.status().root_id;
        let complete: bool = self
            .catalog()
            .run_admin(move |connection| {
                Ok(sql::scalar(
                    connection,
                    "SELECT complete FROM sync_roots WHERE root_id=?1",
                    [root],
                )?)
            })
            .await?;
        Ok(if complete {
            super::root::Baseline::Complete
        } else {
            super::root::Baseline::Activating
        })
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
            inspectable(&tx,root,revision)?;
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
            held::release(&transaction,root)?;
            sql::exec(&transaction,"UPDATE sync_deletion_history SET automatic_count=0 WHERE root_id=?1",[root])?;
            let snapshot=read_set(&transaction,root)?.ok_or(CatalogError::PreconditionFailed)?;
            transaction.commit()?;
            Ok(snapshot)
        }).await?)
    }

    /// Rejects the set `revision` that waits for approval, and returns it as it was. Once this
    /// commits nothing approves or runs the set, and its members wait in the catalog for the
    /// write-back that follows ([`Self::rejected_page`]), across a stop or a restart. A stale
    /// revision, or one already approved or rejected, fails as a failed precondition, as an
    /// approval does.
    pub(crate) async fn reject_deletions(
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
            let snapshot=read_set(&transaction,root)?.ok_or(CatalogError::PreconditionFailed)?;
            let count=sql::exec(&transaction, "UPDATE sync_deletion_sets SET valid=0,authorization=2 WHERE root_id=?1 AND revision=?2 AND authorization=0 AND guarded=1", params![root,revision])?;
            if count!=1 {return Err(CatalogError::PreconditionFailed)}
            held::release(&transaction,root)?;
            transaction.commit()?;
            Ok(snapshot)
        }).await?)
    }

    /// The write-back the root's rejected set has left, if any of its members is not done.
    pub(crate) async fn pending_rejection(&self) -> Result<Option<PendingRejection>, RootError> {
        let root = self.status().root_id;
        let authority = self.authority();
        Ok(self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;
            authority.authorize(&tx,root)?;
            sql::row(&tx, "SELECT d.revision,count(*) FROM sync_deletion_sets d JOIN sync_deletion_members m ON m.root_id=d.root_id WHERE d.root_id=?1 AND d.authorization=2 AND m.done=0 GROUP BY d.revision", [root], |row| Ok(PendingRejection{revision:row.get(0)?,remaining:row.get(1)?}))
                .optional().map_err(unavailable)
        }).await?)
    }

    /// The members after `after` of the root's rejected set that are not written back yet. The
    /// set is no longer valid, so neither its journal fence nor its validity is checked.
    pub(crate) async fn rejected_page(
        &self,
        after: Ordinal,
        limit: PageLimit<256>,
    ) -> Result<Vec<Candidate>, RootError> {
        let root = self.status().root_id;
        let authority = self.authority();
        Ok(self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;
            authority.authorize(&tx,root)?;
            tx.prepare_cached("SELECT m.bucket,m.name,m.owner_project,m.incarnation,m.generation,m.path,m.ordinal FROM sync_deletion_members m JOIN sync_deletion_sets d ON d.root_id=m.root_id WHERE m.root_id=?1 AND d.authorization=2 AND m.done=0 AND m.ordinal>?2 ORDER BY m.ordinal LIMIT ?3")
                .and_then(|mut statement|statement.query_map(params![root,after,limit],read_candidate)?.collect()).map_err(unavailable)
        }).await?)
    }

    /// Marks the member `ordinal` of the root's rejected set done: its file is written back, a
    /// file or entry put at its path meanwhile stays instead, or its object changed and the
    /// export of the change takes over.
    pub(crate) async fn settle_rejected_member(&self, ordinal: Ordinal) -> Result<(), RootError> {
        let root = self.status().root_id;
        let scope = self.control_scope();
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                scope.authorize(&tx)?;
                sql::exec(&tx, "UPDATE sync_deletion_members SET done=1 WHERE root_id=?1 AND ordinal=?2 AND done=0 AND EXISTS(SELECT 1 FROM sync_deletion_sets d WHERE d.root_id=?1 AND d.authorization=2)", params![root, ordinal])?;
                tx.commit().map_err(unavailable)
            })
            .await?;
        Ok(())
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
