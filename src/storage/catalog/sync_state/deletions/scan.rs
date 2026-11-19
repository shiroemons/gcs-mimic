use super::*;
use crate::storage::{catalog::sql, sync_state::SyncRecord};

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
/// complete, taken at the same journal fence, never executed from, under the same guard setting,
/// and not approved. An approved revision is never handed out again, so approval stays one-shot.
fn reusable(
    connection: &Connection,
    root: RootId,
    current: &JournalFence,
    guard: Guard,
) -> Result<Option<OpaqueId>, CatalogError> {
    sql::optional_scalar(
        connection,
        "SELECT revision FROM sync_deletion_sets WHERE root_id=?1 AND complete=1 AND epoch=?2 AND sequence=?3 AND execution_sequence=sequence AND guard_enabled=?4 AND (guarded=0 OR authorization=0)",
        params![root, current.revision, current.sequence, matches!(guard, Guard::Enabled)],
    )
    .map_err(unavailable)
}

/// Whether `candidates` are the members of the set `revision` that follow ordinal `after`.
fn same_members(
    connection: &Connection,
    root: RootId,
    revision: OpaqueId,
    after: u64,
    candidates: &[Candidate],
) -> Result<bool, CatalogError> {
    let members: Vec<Candidate> = connection
        .prepare_cached("SELECT m.bucket,m.name,m.owner_project,m.incarnation,m.generation,m.path,m.ordinal FROM sync_deletion_members m JOIN sync_deletion_sets d ON d.root_id=m.root_id WHERE m.root_id=?1 AND d.revision=?2 AND m.ordinal>?3 AND m.done=0 ORDER BY m.ordinal LIMIT ?4")
        .and_then(|mut statement| {
            statement
                .query_map(
                    params![root, revision, after, candidates.len()],
                    read_candidate,
                )?
                .collect()
        })
        .map_err(unavailable)?;
    Ok(members.len() == candidates.len()
        && members.iter().zip(candidates).all(|(member, candidate)| {
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
    /// until the inventory ends; otherwise the earlier set is replaced by a new draft.
    pub(crate) async fn begin_deletion_inventory(
        &self,
        guard: Guard,
    ) -> Result<Inventory, RootError> {
        self.start_deletion_inventory(guard, Start::ConfirmEarlier)
            .await
    }

    /// Starts an inventory that replaces the root's earlier set, after one that tried to
    /// confirm it found other members.
    pub(crate) async fn begin_new_deletion_inventory(
        &self,
        guard: Guard,
    ) -> Result<Inventory, RootError> {
        self.start_deletion_inventory(guard, Start::New).await
    }

    async fn start_deletion_inventory(
        &self,
        guard: Guard,
        start: Start,
    ) -> Result<Inventory, RootError> {
        self.revalidate().await?;
        let root = self.status().root_id;
        let scope = self.control_scope();
        Ok(self
            .catalog()
            .run_admin(move |connection| {
                let transaction = connection.transaction()?;
                scope.authorize(&transaction)?;
                let complete: bool = sql::scalar(
                    &transaction,
                    "SELECT complete FROM sync_roots WHERE root_id=?1",
                    [root],
                )?;
                if !complete {
                    return Err(CatalogError::PreconditionFailed);
                }
                let current = fence(&transaction)?;
                if start == Start::ConfirmEarlier
                    && let Some(revision) = reusable(&transaction, root, &current, guard)?
                {
                    return Ok(Inventory {
                        revision,
                        root,
                        fence: current,
                        draft: Draft::Confirming {
                            baseline: 0,
                            candidates: 0,
                        },
                        removal: Removal::Files,
                    });
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
                Ok(Inventory {
                    revision,
                    root,
                    fence: current,
                    draft: Draft::New,
                    removal: Removal::Files,
                })
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

    /// Adds one page of the inventory: `baseline` tracked live files, of which `candidates` are
    /// absent, with what `removal` saw gone above them. A new set records them; a confirming
    /// inventory only compares them with the earlier set and writes nothing.
    pub(crate) async fn record_deletion_candidates(
        &self,
        inventory: &mut Inventory,
        baseline: u64,
        candidates: Vec<Candidate>,
        removal: Removal,
    ) -> Result<Confirmed, RootError> {
        inventory.removal = inventory.removal.and(removal);
        let found = candidates.len() as u64;
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
                let same = same_members(&transaction, selected.root, selected.revision, counted, &candidates)?;
                return Ok(if same { Confirmed::Same } else { Confirmed::Diverged });
            }
            let ordinal:u64=sql::scalar(&transaction, "SELECT candidate_count FROM sync_deletion_sets WHERE root_id=?1 AND revision=?2 AND complete=0", params![selected.root,selected.revision])?;
            for (index,candidate) in candidates.iter().enumerate() {
                sql::exec(&transaction, "INSERT INTO sync_deletion_members VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0)", params![selected.root,ordinal+u64::try_from(index).map_err(unavailable)?+1,candidate.bucket,candidate.name,candidate.owner_project,candidate.bucket_incarnation,candidate.generation,candidate.path])?;
            }
            sql::exec(&transaction, "UPDATE sync_deletion_sets SET baseline_count=baseline_count+?3,candidate_count=candidate_count+?4 WHERE root_id=?1 AND revision=?2 AND complete=0", params![selected.root,selected.revision,baseline,candidates.len()])?;
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

    /// Completes the inventory and returns its set. A new set takes its guard decision now. A
    /// confirming inventory returns the earlier set, made valid again when a change invalidated
    /// it meanwhile, or `None` when its totals or guard decision differ from the earlier set's.
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
            let (baseline,candidates,enabled,stored):(u64,u64,bool,bool)=sql::row(&transaction, "SELECT baseline_count,candidate_count,guard_enabled,guarded FROM sync_deletion_sets WHERE root_id=?1 AND revision=?2 AND complete=?3", params![inventory.root,inventory.revision,complete],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
            let automatic=history::automatic_count(&transaction,inventory.root)?;
            let guard=guarded(Guard::from(enabled),baseline+automatic,candidates+automatic)
                || inventory.removal.guarded(Guard::from(enabled));
            match inventory.draft {
                Draft::New => {
                    sql::exec(&transaction, "UPDATE sync_deletion_sets SET complete=1,valid=1,guarded=?3,authorization=?4 WHERE root_id=?1 AND revision=?2", params![inventory.root,inventory.revision,guard,DeletionAuthorization::on_completion(guard)])?;
                }
                Draft::Confirming { baseline: counted_baseline, candidates: counted } => {
                    if (counted_baseline,counted)!=(baseline,candidates) || guard!=stored {
                        return Ok(None);
                    }
                    // Only a set that a change invalidated is written; an unchanged one stays as it was.
                    sql::exec(&transaction, "UPDATE sync_deletion_sets SET valid=1 WHERE root_id=?1 AND revision=?2 AND valid=0", params![inventory.root,inventory.revision])?;
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
