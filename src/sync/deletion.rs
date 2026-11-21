//! Missing tracked files become candidates only after an error-free complete scan.

use super::interrupt::{Interrupt, Interrupted};
use super::{
    SyncError,
    export::retry::{DirectoryCreation, create_directory},
    mapping,
    mirror::PassGate,
    report::{
        command::{AtHand, AwaitingSet},
        remedy::{MOUNT_FIRST, deletion_approval_remedy},
    },
};
use crate::{
    config::Config,
    diagnostics::{self, LogLevel},
    fsutil,
    storage::{
        Catalog,
        admin::{AdminError, BucketName, GenerationCondition},
        sync_state::{
            OpaqueId, Ordinal, PageLimit, SyncBinding,
            deletions::{
                Begun, BucketGone, Candidate, Completion, Confirmed, DeletionAuthorization,
                DeletionSet, EmptiedAtStart, Guard, Inventory, Named, PendingRejection, Recheck,
            },
            observation::PathStop,
            root::{Baseline, RootError, RootLease},
        },
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

mod decide;
mod presence;
pub(super) use decide::{follow, record};
pub(crate) use presence::Missing;
use presence::Removal;
pub(super) use presence::{Observed, Presence, Unobservable, observe, presence};

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

enum Deleted {
    Yes(SyncBinding),
    Stale,
    Protected(SyncBinding),
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(super) async fn inventory(
    root: &RootLease,
    config: &Config,
    pass: PassGate,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> Result<Option<DeletionSet>, SyncError> {
    Ok(inventory_with(root, config, pass, Interrupt::new(stopping))
        .await?
        .into_set())
}

/// What counting an inventory to its end ended in.
#[derive(Debug)]
pub(in crate::sync) enum Taken {
    /// A new set was written.
    Finished(DeletionSet),
    /// The root's earlier set was confirmed, under its own revision.
    Confirmed(DeletionSet),
    /// A stop or a deadline ended the inventory before it compared every file.
    Interrupted,
    /// A rejection whose files are not all written back leaves nothing to inventory until the
    /// write-back that starts each pass finishes them.
    RejectionPending,
}

/// What a pass's inventory ended in.
#[derive(Debug)]
pub(in crate::sync) enum InventoryOutcome {
    /// The pass took an inventory, which ended as [`Taken`] says.
    Taken(Taken),
    /// The pass does not let deletions be inferred, so the root's set was withdrawn instead.
    NotInferred,
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
impl InventoryOutcome {
    fn into_set(self) -> Option<DeletionSet> {
        match self {
            Self::Taken(Taken::Finished(set) | Taken::Confirmed(set)) => Some(set),
            Self::Taken(Taken::Interrupted | Taken::RejectionPending) | Self::NotInferred => None,
        }
    }
}

/// Inventories the tracked files missing after `pass`, and returns the complete set. A set the
/// root already holds keeps its revision when this inventory finds exactly its members again, so
/// that a pass which changed nothing does not withdraw a revision someone is inspecting. A new
/// set that waits for approval is warned about once, under its new revision.
pub(super) async fn inventory_with(
    root: &RootLease,
    config: &Config,
    pass: PassGate,
    interrupt: Interrupt<'_>,
) -> Result<InventoryOutcome, SyncError> {
    if interrupt.check() != Interrupted::No {
        return Ok(InventoryOutcome::Taken(Taken::Interrupted));
    }
    if !pass.hold().infers_deletions() {
        root.invalidate_deletions().await?;
        return Ok(InventoryOutcome::NotInferred);
    }
    let guard = Guard::from(config.sync_delete_guard);
    let taken = match root.begin_deletion_inventory(guard).await? {
        Begun::Started(inventory) => take_inventory(root, guard, inventory, interrupt).await?,
        Begun::RejectionPending => Taken::RejectionPending,
    };
    Ok(InventoryOutcome::Taken(taken))
}

/// Counts `inventory` to its end and returns its set, starting a new one whenever the earlier set
/// it confirms turns out to differ. The pass may end interrupted, or with a rejection that left
/// files to write back, which only a decision between two inventories can, so never here.
async fn take_inventory(
    root: &RootLease,
    guard: Guard,
    mut inventory: Inventory,
    interrupt: Interrupt<'_>,
) -> Result<Taken, SyncError> {
    loop {
        let mut unobserved = Unobserved::default();
        match count_candidates(root, &mut inventory, interrupt, &mut unobserved).await? {
            Counted::Interrupted => return Ok(Taken::Interrupted),
            Counted::Same => {
                unobserved.warn();
                match root.finish_deletion_inventory(inventory).await? {
                    Completion::Finished(set) => {
                        warn_awaiting_approval(root, &set).await?;
                        return Ok(Taken::Finished(set));
                    }
                    Completion::Confirmed(set) => return Ok(Taken::Confirmed(set)),
                    Completion::Diverged => {}
                }
            }
            Counted::Diverged => {}
        }
        // The root's earlier set differs from what the tree shows now, so a new one replaces it.
        match root.begin_new_deletion_inventory(guard).await? {
            Begun::Started(next) => inventory = next,
            Begun::RejectionPending => return Ok(Taken::RejectionPending),
        }
    }
}

/// Warns that `set` waits for a decision, with the paths of its first files, the buckets that
/// went whole and the commands that inspect, reject and approve it, which a wait gives too.
/// Nothing else reports a guarded set until someone asks for the status.
async fn warn_awaiting_approval(root: &RootLease, set: &DeletionSet) -> Result<(), SyncError> {
    let Some(awaiting) = AwaitingSet::of(set) else {
        return Ok(());
    };
    let paths = root.named_deletion_paths(set).await?;
    let at_hand = AtHand {
        deletion: Some(awaiting),
        ..AtHand::default()
    };
    let removed = &set.seen.removed_buckets;
    diagnostics::emit(
        LogLevel::Warn,
        "sync_deletion_approval_required",
        serde_json::json!({
            "revision":set.revision.to_hex(),"candidates":set.candidate_count,
            "baseline":set.baseline_count,"paths":paths.names,"omitted":paths.omitted,
            "removed_buckets":removed.names,"removed_buckets_omitted":removed.omitted,
            "remedy":super::report::command::logged(&deletion_approval_remedy(at_hand)),
        }),
    );
    Ok(())
}

/// What a deletion set deleted: how many objects, and the paths of the first few, which the log
/// names. Nothing else reports an automatic deletion after it ran.
#[derive(Debug)]
pub(super) struct Applied {
    revision: OpaqueId,
    paths: Named<String>,
}

impl Applied {
    fn of(set: &DeletionSet) -> Self {
        Self {
            revision: set.revision,
            paths: Named::default(),
        }
    }

    fn record(&mut self, path: &str) {
        self.paths.push(path.to_owned());
    }

    /// Logs the deletions, if there were any, at `level`.
    pub(super) fn report(&self, level: LogLevel) {
        let deleted = self.paths.count();
        if deleted == 0 {
            return;
        }
        diagnostics::emit(
            level,
            "sync_deletions_applied",
            serde_json::json!({
                "revision":self.revision.to_hex(),"deleted":deleted,
                "paths":self.paths.names,"omitted":self.paths.omitted,
            }),
        );
    }
}

/// How walking the tracked files ended.
#[derive(Debug, PartialEq, Eq)]
enum Counted {
    /// Every file was walked and the inventory still matches the set it confirms, if any.
    Same,
    /// The earlier set this inventory confirms differs from what the tree shows.
    Diverged,
    /// The pass was interrupted before the walk ended.
    Interrupted,
}

/// Walks every tracked live file and records the absent ones in `inventory`.
async fn count_candidates(
    root: &RootLease,
    inventory: &mut Inventory,
    interrupt: Interrupt<'_>,
    unobserved: &mut Unobserved,
) -> Result<Counted, SyncError> {
    let naming = root.naming()?;
    let lookup = root.lookup()?;
    let mut after = String::new();
    let mut gone = GoneBuckets::new(root);
    loop {
        if interrupt.check() != Interrupted::No {
            return Ok(Counted::Interrupted);
        }
        let page = root
            .deletion_inventory_page(inventory.clone(), after.clone())
            .await?;
        if page.is_empty() {
            gone.take(root, inventory);
            return Ok(Counted::Same);
        }
        let baseline = page.len() as u64;
        let mut missing = Vec::new();
        for candidate in page {
            #[cfg(test)]
            super::mirror::export_checkpoint("deletion-candidate").await;
            if interrupt.check() != Interrupted::No {
                return Ok(Counted::Interrupted);
            }
            after.clone_from(&candidate.path);
            let observed = match mapping::path_to_name(Path::new(&candidate.path), naming) {
                Ok((bucket, name)) if bucket == candidate.bucket && name == candidate.name => {
                    observe(root, &lookup, candidate).await?
                }
                Ok(_) | Err(_) => Observed::Unobservable(candidate, Unobservable::NameMismatch),
            };
            match observed {
                Observed::Absent(missing_file) => {
                    gone.saw_absent(missing_file.candidate(), missing_file.removal());
                    missing.push(missing_file);
                }
                Observed::Present(candidate) => gone.saw_present(&candidate),
                Observed::Unobservable(candidate, reason) => {
                    unobservable(root, &candidate, &reason, unobserved).await?;
                }
            }
        }
        // Only a path seen empty becomes a candidate.
        if root
            .record_deletion_candidates(inventory, baseline, missing)
            .await?
            == Confirmed::Diverged
        {
            return Ok(Counted::Diverged);
        }
    }
}

/// What an inventory finds of the buckets whose tracked files may all have gone at once: a
/// bucket whose directory is missing, or one that none of its files the first complete inventory
/// of this process found gone has come back to since. Both look exactly like the user deleting
/// the files when a mount did not come up or came up empty. A bucket emptied while the mirror
/// runs is left to the counts.
struct GoneBuckets {
    /// What the earlier inventories of this process kept; `None` while this one is the first.
    at_start: Option<EmptiedAtStart>,
    buckets: BTreeMap<BucketName, BucketTally>,
}

#[derive(Default)]
struct BucketTally {
    directory_missing: bool,
    /// Whether a file that counts was there, which ends the bucket's emptiness for good.
    returned: bool,
    /// The paths of the files that count and were gone, while none was there.
    absent: BTreeSet<String>,
}

impl GoneBuckets {
    fn new(root: &RootLease) -> Self {
        Self {
            at_start: root.emptied_at_start(),
            buckets: BTreeMap::new(),
        }
    }

    /// Whether the file of `candidate` tells whether its bucket was emptied at the start: every
    /// tracked file in the first inventory, and later only the files that inventory found gone.
    /// A file the tree gained since, such as the export of an object written through the API,
    /// does not; exporting it must not let the files gone at the start be deleted unasked.
    fn counts(&self, candidate: &Candidate) -> bool {
        self.at_start.as_ref().is_none_or(|at_start| {
            at_start
                .get(&candidate.bucket)
                .is_some_and(|paths| paths.contains(&candidate.path))
        })
    }

    fn saw_absent(&mut self, candidate: &Candidate, removal: Removal) {
        let counts = self.counts(candidate);
        let tally = self.buckets.entry(candidate.bucket.clone()).or_default();
        match removal {
            Removal::BucketDirectory => tally.directory_missing = true,
            Removal::Files => {}
        }
        if counts && !tally.returned {
            tally.absent.insert(candidate.path.clone());
        }
    }

    fn saw_present(&mut self, candidate: &Candidate) {
        if self.counts(candidate) {
            let tally = self.buckets.entry(candidate.bucket.clone()).or_default();
            tally.returned = true;
            tally.absent = BTreeSet::new();
        }
    }

    /// Takes to approval, whatever the counts, the files of each bucket that went all at once,
    /// warns about each such bucket, and keeps the emptied ones for the next inventory.
    fn take(self, root: &RootLease, inventory: &mut Inventory) {
        let mut kept = EmptiedAtStart::new();
        for (bucket, tally) in self.buckets {
            let emptied = !tally.returned && !tally.absent.is_empty();
            let gone = match (tally.directory_missing, emptied) {
                (true, _) => BucketGone::Missing,
                (false, true) => BucketGone::Emptied,
                (false, false) => continue,
            };
            warn_bucket_gone(&bucket, gone);
            inventory.saw_bucket_gone(&bucket, gone);
            if emptied {
                kept.insert(bucket, tally.absent);
            }
        }
        root.keep_emptied_at_start(kept);
    }
}

/// Reports a bucket whose tracked files all went at once. The inventory takes them to approval
/// whatever their number; nothing recreates a missing directory meanwhile.
fn warn_bucket_gone(bucket: &BucketName, gone: BucketGone) {
    let (event, reason, remedy) = bucket_gone_report(gone);
    diagnostics::emit(
        LogLevel::Warn,
        event,
        serde_json::json!({"bucket":bucket,"path":bucket,"reason":reason,"remedy":remedy}),
    );
}

/// The event, reason and remedy of the warning on a bucket whose tracked files all went at once.
/// The reason says what the inventory saw, never when or why the files went: a bind mount that
/// came up empty or did not come up looks the same as files deleted on purpose. The remedy puts
/// the steps in the order the advice on the set does, a failed mount before anything else, then
/// the rejection, restoring by hand, and last the approval that deletes.
fn bucket_gone_report(gone: BucketGone) -> (&'static str, &'static str, String) {
    let (event, reason, what, rejection) = match gone {
        BucketGone::Missing => (
            "sync_bucket_directory_missing",
            "The directory of a bucket with synchronized files is missing",
            "the directory",
            "recreates the directory and writes the files back into it from the objects",
        ),
        BucketGone::Emptied => (
            "sync_bucket_directory_emptied",
            "The directory of a bucket has shown none of its synchronized files since the server started",
            "the files",
            "writes the files back from the objects",
        ),
    };
    let remedy = format!(
        "Inspect the pending deletion set first; {MOUNT_FIRST}; if {what} went by mistake, reject the set, which {rejection}; or put {what} back yourself and wait again; if you deleted {what} on purpose, approve the set, which deletes the objects and keeps the bucket"
    );
    (event, reason, remedy)
}

/// Whether nothing may create the directory of `bucket` now: it is missing while the inventory
/// takes tracked files in it to approval. Recreating it, for a reconciliation or for the export
/// of another object alike, would turn those files into files deleted one by one, which the
/// guard lets through below its threshold. Exports into the bucket wait until the set is
/// approved and its files are deleted, or until the directory is back.
pub(super) async fn bucket_directory_held(
    root: &RootLease,
    bucket: &BucketName,
) -> Result<bool, SyncError> {
    let selected = bucket.clone();
    let lookup = root.lookup()?;
    let missing = root
        .file_job(move |root| {
            Ok(matches!(
                lookup.directory(
                    root,
                    selected.as_str().as_ref(),
                    fsutil::DirectoryOpen::Existing,
                ),
                Ok(fsutil::Child::Missing(_))
            ))
        })
        .await?;
    Ok(missing
        && root.current_baseline().await? == Baseline::Complete
        && root.inventories_in(bucket).await?)
}

/// Counts a tracked file whose path tells nothing about it. Its object is not deleted, and the
/// inventory goes on with the others. A file replaced by a directory or a special file also
/// stops its own path, where the findings name it until the path is empty or regular again.
async fn unobservable(
    root: &RootLease,
    candidate: &Candidate,
    reason: &Unobservable,
    unobserved: &mut Unobserved,
) -> Result<(), SyncError> {
    if reason.stops_path() {
        record_replaced(root, &candidate.bucket, &candidate.name).await?;
    }
    unobserved.record(candidate, reason);
    Ok(())
}

/// The tracked paths one inventory could not observe: how many, and the first few with their
/// reasons and remedies. Every inventory meets a path again while it stays unobservable, and a
/// warning for each on every pass buried the rest of the log, so an inventory warns once.
#[derive(Debug, Default)]
struct Unobserved {
    count: u64,
    first: Vec<UnobservedPath>,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
struct UnobservedPath {
    bucket: BucketName,
    object: crate::storage::admin::ObjectName,
    path: String,
    reason: String,
    remedy: String,
}

impl Unobserved {
    /// Paths the warning names; the rest are only counted.
    const NAMED: usize = 10;

    fn record(&mut self, candidate: &Candidate, reason: &Unobservable) {
        self.count += 1;
        if self.first.len() < Self::NAMED {
            self.first.push(UnobservedPath {
                bucket: candidate.bucket.clone(),
                object: candidate.name.clone(),
                path: candidate.path.clone(),
                reason: reason.to_string(),
                remedy: reason.remedy(),
            });
        }
    }

    /// Warns about the paths, if there were any, in one line.
    fn warn(&self) {
        if self.count == 0 {
            return;
        }
        let named = u64::try_from(self.first.len()).unwrap_or(u64::MAX);
        diagnostics::emit(
            LogLevel::Warn,
            "sync_deletion_unobservable",
            serde_json::json!({
                "count":self.count,"paths":self.first,"omitted":self.count.saturating_sub(named),
            }),
        );
    }
}

/// Stops the path of `name`, whose tracked file a directory or special file replaced. A bucket
/// recreated or deleted meanwhile has no such path any more.
pub(super) async fn record_replaced(
    root: &RootLease,
    bucket: &BucketName,
    name: &crate::storage::admin::ObjectName,
) -> Result<(), SyncError> {
    let binding = match root.sync_binding(bucket).await {
        Ok(binding) => binding,
        Err(AdminError::NotFound | AdminError::Precondition) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    super::error::unless_superseded(
        root.record_path_stop(binding, name.clone(), PathStop::EntryReplaced)
            .await,
    )?;
    Ok(())
}

/// Runs `set` and returns how many objects it deleted.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(super) async fn execute(
    catalog: &Catalog,
    root: &RootLease,
    set: DeletionSet,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> Result<u64, SyncError> {
    Ok(execute_with(catalog, root, set, Interrupt::new(stopping))
        .await?
        .paths
        .count())
}

/// Deletes the objects of an authorized set whose paths are still empty, and returns what it
/// deleted. A set that waits for approval deletes nothing.
pub(super) async fn execute_with(
    catalog: &Catalog,
    root: &RootLease,
    set: DeletionSet,
    interrupt: Interrupt<'_>,
) -> Result<Applied, SyncError> {
    if set.authorization != DeletionAuthorization::Granted {
        return Ok(Applied::of(&set));
    }
    let mut after = Ordinal::START;
    let mut applied = Applied::of(&set);
    let lookup = root.lookup()?;
    let mut restored_buckets = std::collections::BTreeSet::new();
    loop {
        if interrupt.check() != Interrupted::No {
            return Ok(applied);
        }
        let page = match root
            .deletion_page(set.revision, after, PageLimit::LARGEST)
            .await
        {
            Ok(page) => page,
            Err(RootError::Admin(AdminError::Precondition | AdminError::NotFound)) => {
                root.invalidate_deletions().await?;
                return Ok(applied);
            }
            Err(error) => return Err(error.into()),
        };
        if page.is_empty() {
            break;
        }
        for candidate in page {
            if interrupt.check() != Interrupted::No {
                return Ok(applied);
            }
            after = candidate.ordinal;
            // A file that came back, or a path that no longer tells, ends this set; the next
            // inventory leaves that member out and keeps the others.
            let missing = match observe(root, &lookup, candidate).await? {
                Observed::Absent(missing) => missing,
                Observed::Present(_) | Observed::Unobservable(..) => {
                    root.invalidate_deletions().await?;
                    return Ok(applied);
                }
            };
            if interrupt.check() != Interrupted::No {
                return Ok(applied);
            }
            let candidate = missing.candidate();
            match delete(root, &set, &missing).await? {
                Deleted::Yes(binding) => {
                    root.clear_path_stops(binding, candidate.name.clone(), &PathStop::ALL)
                        .await?;
                }
                Deleted::Stale => {
                    root.invalidate_deletions().await?;
                    return Ok(applied);
                }
                Deleted::Protected(binding) => {
                    root.record_path_stop(
                        binding,
                        candidate.name.clone(),
                        PathStop::ProtectedDeletion,
                    )
                    .await?;
                    root.invalidate_deletions().await?;
                    super::export::restore::restore_from_catalog(
                        catalog,
                        root,
                        &candidate.bucket,
                        &candidate.name,
                    )
                    .await?;
                    crate::diagnostics::emit(
                        crate::diagnostics::LogLevel::Warn,
                        "sync_protected_deletion",
                        serde_json::json!({"bucket":candidate.bucket,"object":candidate.name,"generation":candidate.generation,"remedy":"The API object remains protected. Its file was restored where safe; release the hold or retention through the API, then delete the file again to retry"}),
                    );
                    return Ok(applied);
                }
            }
            if restored_buckets.insert(candidate.bucket.clone()) {
                match create_directory(root, std::path::PathBuf::from(candidate.bucket.as_str()))
                    .await?
                {
                    // An entry that fails to open is still there, and a later pass's
                    // reconciliation makes the directory if it is not one.
                    DirectoryCreation::Created
                    | DirectoryCreation::Finding
                    | DirectoryCreation::Deferred => {}
                }
            }
            applied.record(&candidate.path);
        }
    }
    Ok(applied)
}

/// Writes the files of the root's rejected set back from their objects, the way the restore
/// after a protected deletion does: through the ordinary write-back, and only into a path that
/// is still empty, so a file put there meanwhile stays as it is and is ingested as any file is.
/// A bucket directory that went with the files is created again first. Each member is marked
/// done once its write-back ended, and the others stay in the catalog: a stop or a failure
/// leaves them to the next start, retry or rescan, which resume here before any inventory.
/// Nothing is left to do for a root without a rejected set.
pub(super) async fn write_back_rejected(
    catalog: &Catalog,
    root: &RootLease,
    interrupt: Interrupt<'_>,
) -> Result<(), SyncError> {
    let Some(pending) = root.pending_rejection().await? else {
        return Ok(());
    };
    let mut ended = WriteBack::default();
    let finished = write_back_members(catalog, root, interrupt, &mut ended).await;
    ended.report(&pending, &finished);
    finished.map(drop)
}

/// Writes back the members not done yet, in order, counting how each ended in `ended`, and
/// says whether `interrupt` stopped it first. The first failure ends it: the member it failed
/// at and those after it stay for the next pass.
async fn write_back_members(
    catalog: &Catalog,
    root: &RootLease,
    interrupt: Interrupt<'_>,
    ended: &mut WriteBack,
) -> Result<Interrupted, SyncError> {
    let mut after = Ordinal::START;
    let mut buckets = BTreeSet::new();
    loop {
        let page = root.rejected_page(after, PageLimit::LARGEST).await?;
        let Some(last) = page.last() else {
            return Ok(Interrupted::No);
        };
        after = last.ordinal;
        for candidate in page {
            #[cfg(test)]
            super::mirror::export_checkpoint("rejected-member").await;
            let interrupted = interrupt.check();
            if interrupted != Interrupted::No {
                return Ok(interrupted);
            }
            if !buckets.contains(&candidate.bucket) {
                match create_directory(root, std::path::PathBuf::from(candidate.bucket.as_str()))
                    .await?
                {
                    DirectoryCreation::Created | DirectoryCreation::Finding => {}
                    // A restore into a directory that does not open passes its file over, and
                    // the inventory after this write-back would take that file for one the user
                    // deleted. The write-back fails as the failed open does instead, which
                    // leaves this member and those after it for the next pass.
                    DirectoryCreation::Deferred => {
                        return Err(SyncError::Writeback(
                            crate::storage::sync_state::writeback::Error::Root(RootError::Admin(
                                std::io::Error::from(fsutil::Unsettled).into(),
                            )),
                        ));
                    }
                }
                buckets.insert(candidate.bucket.clone());
            }
            ended.record(
                super::export::restore::restore_from_catalog(
                    catalog,
                    root,
                    &candidate.bucket,
                    &candidate.name,
                )
                .await?,
            );
            root.settle_rejected_member(candidate.ordinal).await?;
        }
    }
}

/// How the members a write-back reached ended, which its log line counts.
#[derive(Debug, Default)]
struct WriteBack {
    restored: u64,
    kept: u64,
    skipped: u64,
}

impl WriteBack {
    fn record(&mut self, restored: super::export::restore::Restored) {
        use super::export::restore::Restored;
        match restored {
            Restored::Written => self.restored += 1,
            Restored::Kept => self.kept += 1,
            Restored::Skipped => self.skipped += 1,
        }
    }

    /// Logs what this write-back of `pending` did, which ended as `finished`: at the info level
    /// with nothing remaining once every file is back, which is when the waits it held back go
    /// on, and as a warning when it stopped or failed with members left, which the next start,
    /// retry or rescan writes back before it looks for deleted files. A failure names its cause
    /// and asks for it to be corrected first, since a write-back that keeps failing keeps
    /// deletions from being detected.
    fn report(&self, pending: &PendingRejection, finished: &Result<Interrupted, SyncError>) {
        let revision = pending.revision.to_hex();
        let (reason, remedy) = match finished {
            Ok(Interrupted::No) => {
                diagnostics::emit(
                    LogLevel::Info,
                    "sync_deletions_rejected",
                    serde_json::json!({
                        "revision":revision,"restored":self.restored,"kept":self.kept,
                        "skipped":self.skipped,"remaining":0,
                    }),
                );
                return;
            }
            Ok(Interrupted::Stopped | Interrupted::Deadline) => (
                None,
                "The next start, retry or rescan writes the remaining files back before it looks for deleted files",
            ),
            Err(error) => (
                Some(crate::error::Report(error).to_string()),
                "Correct what the reason names, such as a missing permission on the directory or a full disk; the next start, retry or rescan writes the remaining files back before it looks for deleted files, and no deletion is detected until they are back",
            ),
        };
        let ended = self.restored + self.kept + self.skipped;
        diagnostics::emit(
            LogLevel::Warn,
            "sync_deletions_rejected",
            serde_json::json!({
                "revision":revision,"restored":self.restored,"kept":self.kept,
                "skipped":self.skipped,"remaining":pending.remaining.saturating_sub(ended),
                "reason":reason,"remedy":remedy,
            }),
        );
    }
}

/// Deletes the object of a member whose path was just seen empty.
async fn delete(
    root: &RootLease,
    set: &DeletionSet,
    missing: &Missing,
) -> Result<Deleted, SyncError> {
    let candidate = missing.candidate();
    let binding = match root.sync_binding(&candidate.bucket).await {
        Ok(binding) => binding,
        Err(AdminError::NotFound | AdminError::Precondition) => return Ok(Deleted::Stale),
        Err(error) => return Err(error.into()),
    };
    let binding = root.deletion_binding(binding, set.revision, candidate.ordinal);
    match root
        .sync_admin()
        .delete_synced_object(
            &candidate.bucket,
            &candidate.name,
            GenerationCondition::Is(candidate.generation),
            binding.clone(),
        )
        .await
    {
        Ok(()) => Ok(Deleted::Yes(binding)),
        Err(AdminError::Hold) => Ok(Deleted::Protected(binding)),
        Err(AdminError::Precondition | AdminError::NotFound) => Ok(Deleted::Stale),
        Err(error) => Err(error.into()),
    }
}
