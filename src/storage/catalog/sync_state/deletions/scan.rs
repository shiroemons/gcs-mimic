use super::*;
use crate::{
    storage::{catalog::sql, sync_state::SyncRecord},
    sync::Missing,
};

/// The tracked files a deletion inventory looks at, as the `FROM … WHERE` of a query whose `?1`
/// is the root: each tracks its object's live generation in a bucket incarnation that still
/// exists, and the object has no finding. A finding means the tree could not hold or show the
/// object as tracked, so its missing file is written back, never inferred deleted; the
/// inventory's pages and the reconciliation's check of one object both read this condition.
macro_rules! inventoried {
    () => {
        "FROM sync_objects s JOIN bucket_incarnations i ON i.bucket=s.bucket AND i.incarnation=s.incarnation JOIN buckets b ON b.name=s.bucket JOIN objects o ON o.bucket=s.bucket AND o.name=s.name AND o.generation=s.generation AND o.time_deleted IS NULL WHERE NOT EXISTS(SELECT 1 FROM sync_findings f WHERE f.root_id=?1 AND f.bucket=s.bucket AND f.name=s.name)"
    };
}

#[derive(Clone)]
pub(crate) struct Inventory {
    pub revision: OpaqueId,
    root: RootId,
    fence: JournalFence,
    draft: Draft,
    /// What the pages recorded so far saw gone above their candidates.
    removal: Removal,
}

impl Inventory {
    /// Whether finishing this inventory writes a set under a revision nobody has seen yet,
    /// rather than confirming the root's earlier set under its own.
    pub(crate) fn writes_new_set(&self) -> bool {
        match self.draft {
            Draft::New => true,
            Draft::Confirming { .. } => false,
        }
    }

    /// Adds what the inventory saw gone that no single page shows, such as a bucket all of whose
    /// tracked files went, which the pages count across.
    pub(crate) fn saw_gone(&mut self, removal: Removal) {
        self.removal = self.removal.and(removal);
    }

    /// An inventory at `fence` that confirms the root's earlier set `revision`.
    fn confirming(revision: OpaqueId, root: RootId, fence: JournalFence) -> Self {
        Self {
            revision,
            root,
            fence,
            draft: Draft::Confirming {
                baseline: 0,
                candidates: 0,
            },
            removal: Removal::Files,
        }
    }
}

/// What an inventory writes: a new set, or nothing while it confirms the root's earlier one.
#[derive(Clone, Copy)]
enum Draft {
    /// A new set at `Inventory::revision`, filled page by page.
    New,
    /// The root's earlier complete set at `Inventory::revision`, taken at this same journal fence
    /// and never used. The counts are what this inventory found so far; each page must repeat
    /// that set's members in order, and the totals must match at the end.
    Confirming { baseline: u64, candidates: u64 },
}

/// Whether an inventory still matches the earlier set it confirms. A new set always does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Confirmed {
    Same,
    Diverged,
}

/// Reads the root's set when an inventory at `current` may confirm it instead of replacing it:
/// complete, taken in the same journal epoch at or before `current`, never executed from, under
/// the same guard setting, neither approved nor rejected, and [`unaffected!`] by an update of a
/// member's bucket. A decided revision is never handed out again, so a
/// decision stays one-shot, and a restart starts a new epoch. A journal that moved since does
/// not rule the set out otherwise: the inventory compares every member, generation and
/// incarnation included, so a write that changed none of them, such as one to an unrelated
/// object, keeps the revision someone may be about to approve or reject.
fn reusable(
    connection: &Connection,
    root: RootId,
    current: &JournalFence,
    guard: Guard,
) -> Result<Option<OpaqueId>, CatalogError> {
    sql::optional_scalar(
        connection,
        concat!(
            "SELECT d.revision FROM sync_deletion_sets d JOIN change_journal_state j ON j.singleton=1 WHERE d.root_id=?1 AND d.complete=1 AND d.epoch=?2 AND d.sequence<=?3 AND d.execution_sequence=d.sequence AND d.guard_enabled=?4 AND (d.guarded=0 OR d.authorization=0) AND ",
            unaffected!()
        ),
        params![root, current.revision, current.sequence, matches!(guard, Guard::Enabled)],
    )
    .map_err(unavailable)
}

/// How the inventory before a decision on a set starts.
pub(crate) enum Recheck {
    /// The set waits and may be confirmed: the inventory compares its members with the tree.
    Confirm(Inventory),
    /// The set waits, but a member's bucket was updated since, which may make its deletions
    /// unrecoverable, so a new set must replace it whatever the tree shows.
    Replace,
    /// The set does not wait for a decision.
    NotAwaiting,
}

/// Whether the candidates of `missing` are the members of the set `revision` that follow ordinal
/// `after`.
fn same_members(
    connection: &Connection,
    root: RootId,
    revision: OpaqueId,
    after: u64,
    missing: &[Missing],
) -> Result<bool, CatalogError> {
    let members: Vec<Candidate> = connection
        .prepare_cached("SELECT m.bucket,m.name,m.owner_project,m.incarnation,m.generation,m.path,m.ordinal FROM sync_deletion_members m JOIN sync_deletion_sets d ON d.root_id=m.root_id WHERE m.root_id=?1 AND d.revision=?2 AND m.ordinal>?3 AND m.done=0 ORDER BY m.ordinal LIMIT ?4")
        .and_then(|mut statement| {
            statement
                .query_map(
                    params![root, revision, after, missing.len()],
                    read_candidate,
                )?
                .collect()
        })
        .map_err(unavailable)?;
    Ok(members.len() == missing.len()
        && members
            .iter()
            .zip(missing.iter().map(Missing::candidate))
            .all(|(member, candidate)| {
                member.bucket == candidate.bucket
                    && member.name == candidate.name
                    && member.owner_project == candidate.owner_project
                    && member.bucket_incarnation == candidate.bucket_incarnation
                    && member.generation == candidate.generation
                    && member.path == candidate.path
            }))
}

impl RootLease {
    /// Starts an inventory. When the root's earlier set may be confirmed, nothing is written
    /// until the inventory ends; otherwise the earlier set is replaced by a new draft. `None`
    /// while the root's rejected set has files left to write back, which no inventory may
    /// take for deleted ones.
    pub(crate) async fn begin_deletion_inventory(
        &self,
        guard: Guard,
    ) -> Result<Option<Inventory>, RootError> {
        self.start_deletion_inventory(guard, Start::ConfirmEarlier)
            .await
    }

    /// Starts an inventory that replaces the root's earlier set, after one that tried to
    /// confirm it found other members; `None` as for [`Self::begin_deletion_inventory`].
    pub(crate) async fn begin_new_deletion_inventory(
        &self,
        guard: Guard,
    ) -> Result<Option<Inventory>, RootError> {
        self.start_deletion_inventory(guard, Start::New).await
    }

    /// Starts the inventory before an approval or rejection of the set `revision`, when that set
    /// still waits for one in this journal epoch, at the current journal fence. An API write
    /// since the set was taken does not rule it out by itself: the inventory compares every
    /// member, generation, bucket incarnation and path included, so a write that touched a
    /// member replaces the set and one that touched none keeps the revision. An update of a
    /// member's bucket ([`unaffected!`]) replaces it whatever the tree shows.
    pub(crate) async fn begin_deletion_recheck(
        &self,
        guard: Guard,
        revision: OpaqueId,
    ) -> Result<Recheck, RootError> {
        self.revalidate().await?;
        let root = self.status().root_id;
        let scope = self.control_scope();
        Ok(self
            .catalog()
            .run_admin(move |connection| {
                let transaction = connection.transaction()?;
                scope.authorize(&transaction)?;
                let current = fence(&transaction)?;
                let waiting: bool = sql::scalar(
                    &transaction,
                    concat!(
                        "SELECT EXISTS(SELECT 1 FROM sync_deletion_sets d WHERE d.root_id=?1 AND d.revision=?2 AND d.epoch=?3 AND d.guard_enabled=?4 AND ",
                        awaiting!(),
                        ")"
                    ),
                    params![root, revision, current.revision, matches!(guard, Guard::Enabled)],
                )?;
                Ok(if !waiting {
                    Recheck::NotAwaiting
                } else if reusable(&transaction, root, &current, guard)? == Some(revision) {
                    Recheck::Confirm(Inventory::confirming(revision, root, current))
                } else {
                    Recheck::Replace
                })
            })
            .await?)
    }

    async fn start_deletion_inventory(
        &self,
        guard: Guard,
        start: Start,
    ) -> Result<Option<Inventory>, RootError> {
        self.revalidate().await?;
        let root = self.status().root_id;
        let scope = self.control_scope();
        Ok(self
            .catalog()
            .run_admin(move |connection| {
                let transaction = connection.transaction()?;
                scope.authorize(&transaction)?;
                if Baseline::read(&transaction, root)? != Baseline::Complete {
                    return Err(CatalogError::PreconditionFailed);
                }
                if rejected_remaining(&transaction, root)? > 0 {
                    return Ok(None);
                }
                let current = fence(&transaction)?;
                if start == Start::ConfirmEarlier
                    && let Some(revision) = reusable(&transaction, root, &current, guard)?
                {
                    return Ok(Some(Inventory::confirming(revision, root, current)));
                }
                let revision = OpaqueId::generate().map_err(unavailable)?;
                let scan_revision = OpaqueId::generate().map_err(unavailable)?;
                sql::exec(
                    &transaction,
                    "DELETE FROM sync_deletion_sets WHERE root_id=?1",
                    [root],
                )?;
                sql::exec(
                    &transaction,
                    "INSERT INTO sync_deletion_sets VALUES(?1,?2,?3,?4,?5,?4,0,0,?6,0,0,0,0)",
                    params![
                        root,
                        revision,
                        scan_revision,
                        current.sequence,
                        current.revision,
                        matches!(guard, Guard::Enabled)
                    ],
                )?;
                transaction.commit()?;
                Ok(Some(Inventory {
                    revision,
                    root,
                    fence: current,
                    draft: Draft::New,
                    removal: Removal::Files,
                }))
            })
            .await?)
    }

    pub(crate) async fn deletion_inventory_page(
        &self,
        inventory: Inventory,
        after: String,
    ) -> Result<Vec<Candidate>, RootError> {
        let scope = self.control_scope();
        Ok(self
            .catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                scope.authorize(&tx)?;
                if fence(&tx)? != inventory.fence {
                    return Err(CatalogError::PreconditionFailed);
                }
                tx.prepare_cached(concat!(
                    "SELECT s.bucket,s.name,b.project,s.incarnation,s.generation,s.path,0 ",
                    inventoried!(),
                    " AND s.path>?2 ORDER BY s.path LIMIT 256"
                ))
                .and_then(|mut statement| {
                    statement
                        .query_map(params![inventory.root, after], read_candidate)?
                        .collect()
                })
                .map_err(unavailable)
            })
            .await?)
    }

    /// Whether the deletion inventory looks at `tracked`, the file of an object in `bucket`
    /// whose live generation is `generation`, so that a reconciliation finding that file missing
    /// leaves it to the inventory instead of writing it back. Both read [`inventoried!`], so no
    /// object is left to an inventory that skips it.
    pub(crate) async fn inventories(
        &self,
        bucket: &BucketName,
        tracked: &SyncRecord,
        generation: i64,
    ) -> Result<bool, RootError> {
        let root = self.status().root_id;
        let bucket = bucket.clone();
        let (name, path) = (tracked.name.clone(), tracked.path.clone());
        Ok(self
            .catalog()
            .run_admin(move |connection| {
                connection
                    .prepare_cached(concat!(
                        "SELECT EXISTS(SELECT 1 ",
                        inventoried!(),
                        " AND s.bucket=?2 AND s.name=?3 AND s.path=?4 AND s.generation=?5)"
                    ))
                    .and_then(|mut statement| {
                        statement.query_row(params![root, bucket, name, path, generation], |row| {
                            row.get(0)
                        })
                    })
                    .map_err(unavailable)
            })
            .await?)
    }

    /// Whether the deletion inventory looks at any tracked file of `bucket`: those are the files
    /// whose deletion a missing bucket directory takes to approval. It reads [`inventoried!`]
    /// like the inventory, so it names exactly the files the inventory would take.
    pub(crate) async fn inventories_in(&self, bucket: &BucketName) -> Result<bool, RootError> {
        let root = self.status().root_id;
        let bucket = bucket.clone();
        Ok(self
            .catalog()
            .run_admin(move |connection| {
                connection
                    .prepare_cached(concat!(
                        "SELECT EXISTS(SELECT 1 ",
                        inventoried!(),
                        " AND s.bucket=?2)"
                    ))
                    .and_then(|mut statement| {
                        statement.query_row(params![root, bucket], |row| row.get(0))
                    })
                    .map_err(unavailable)
            })
            .await?)
    }

    /// Adds one page of the inventory: `baseline` tracked live files, of which `missing` were seen
    /// absent, with what was gone above them. A new set records them; a confirming inventory only
    /// compares them with the earlier set and writes nothing. Only candidates whose own paths
    /// were seen empty reach here, which is all a [`Missing`] can hold.
    pub(crate) async fn record_deletion_candidates(
        &self,
        inventory: &mut Inventory,
        baseline: u64,
        missing: Vec<Missing>,
    ) -> Result<Confirmed, RootError> {
        inventory.removal = missing
            .iter()
            .map(Missing::removal)
            .fold(inventory.removal, Removal::and);
        let found = missing.len() as u64;
        if found > 256 || baseline > 256 || baseline < found {
            return Err(AdminError::Invalid("invalid deletion inventory page".into()).into());
        }
        let selected = inventory.clone();
        let scope = self.control_scope();
        let confirmed = self.catalog().run_admin(move |connection| {
            let transaction=connection.transaction()?;
            scope.authorize(&transaction)?;
            if fence(&transaction)?!=selected.fence {return Err(CatalogError::PreconditionFailed)}
            if let Draft::Confirming { candidates: counted, .. } = selected.draft {
                let same = same_members(&transaction, selected.root, selected.revision, counted, &missing)?;
                return Ok(if same { Confirmed::Same } else { Confirmed::Diverged });
            }
            let ordinal:u64=sql::scalar(&transaction, "SELECT candidate_count FROM sync_deletion_sets WHERE root_id=?1 AND revision=?2 AND complete=0", params![selected.root,selected.revision])?;
            for (index,candidate) in missing.iter().map(Missing::candidate).enumerate() {
                sql::exec(&transaction, "INSERT INTO sync_deletion_members VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0)", params![selected.root,ordinal+u64::try_from(index).map_err(unavailable)?+1,candidate.bucket,candidate.name,candidate.owner_project,candidate.bucket_incarnation,candidate.generation,candidate.path])?;
            }
            sql::exec(&transaction, "UPDATE sync_deletion_sets SET baseline_count=baseline_count+?3,candidate_count=candidate_count+?4 WHERE root_id=?1 AND revision=?2 AND complete=0", params![selected.root,selected.revision,baseline,missing.len()])?;
            transaction.commit()?;
            Ok(Confirmed::Same)
        }).await?;
        if let Draft::Confirming {
            baseline: counted_baseline,
            candidates: counted,
        } = &mut inventory.draft
        {
            *counted_baseline += baseline;
            *counted += found;
        }
        Ok(confirmed)
    }

    /// Completes the inventory and returns its set. A new set takes its guard decision now: by
    /// the counts, by a bucket gone at once, or because it holds a file of the set that waited
    /// before it ([`held`]), which keeps a wait going until someone decides or every file of
    /// it is back. A confirming inventory returns the earlier set, made valid again when a
    /// change invalidated it meanwhile and carried to this inventory's journal fence, or `None`
    /// when its member count or guard decision differs from the earlier set's. The members
    /// themselves were compared page by page; the denominator may differ, as when a file the
    /// set does not hold was added, and the set keeps the one it was taken with.
    pub(crate) async fn finish_deletion_inventory(
        &self,
        inventory: Inventory,
    ) -> Result<Option<DeletionSet>, RootError> {
        self.revalidate().await?;
        let scope = self.control_scope();
        Ok(self.catalog().run_admin(move |connection| {
            let transaction=connection.transaction()?;
            scope.authorize(&transaction)?;
            if fence(&transaction)?!=inventory.fence {return Err(CatalogError::PreconditionFailed)}
            let complete = match inventory.draft { Draft::New => 0, Draft::Confirming { .. } => 1 };
            let (stored_baseline,stored_candidates,enabled,stored):(u64,u64,bool,bool)=sql::row(&transaction, "SELECT baseline_count,candidate_count,guard_enabled,guarded FROM sync_deletion_sets WHERE root_id=?1 AND revision=?2 AND complete=?3", params![inventory.root,inventory.revision,complete],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
            // The guard decides on what this inventory counted, so that a confirmation whose
            // decision would differ replaces the set instead.
            let (baseline,candidates) = match inventory.draft {
                Draft::New => (stored_baseline,stored_candidates),
                Draft::Confirming { baseline, candidates } => (baseline,candidates),
            };
            let automatic=history::automatic_count(&transaction,inventory.root)?;
            let setting=Guard::from(enabled);
            let guard=guarded(setting,baseline+automatic,candidates+automatic)
                || inventory.removal.guarded(setting)
                || (matches!(setting,Guard::Enabled) && held::holds_waiting(&transaction,inventory.root)?);
            match inventory.draft {
                Draft::New => {
                    sql::exec(&transaction, "UPDATE sync_deletion_sets SET complete=1,valid=1,guarded=?3,authorization=?4 WHERE root_id=?1 AND revision=?2", params![inventory.root,inventory.revision,guard,DeletionAuthorization::on_completion(guard)])?;
                    held::keep(&transaction,inventory.root,guard)?;
                }
                Draft::Confirming { .. } => {
                    if candidates!=stored_candidates || guard!=stored {
                        return Ok(None);
                    }
                    // Only a set that a change invalidated, or that the journal moved past, is
                    // written; an unchanged one stays as it was.
                    sql::exec(&transaction, "UPDATE sync_deletion_sets SET valid=1,sequence=?3,execution_sequence=?3 WHERE root_id=?1 AND revision=?2 AND (valid=0 OR sequence!=?3)", params![inventory.root,inventory.revision,inventory.fence.sequence])?;
                }
            }
            let snapshot=read_set(&transaction,inventory.root)?.ok_or(CatalogError::PreconditionFailed)?;
            transaction.commit()?;
            Ok(Some(snapshot))
        }).await?)
    }
}

/// Whether an inventory may confirm the root's earlier set or must replace it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Start {
    ConfirmEarlier,
    New,
}
