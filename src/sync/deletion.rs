//! Missing tracked files become candidates only after an error-free complete scan.

use super::interrupt::{Interrupt, Interrupted};
use super::{SyncError, advice::AtHand, control::WaitReason, mapping, mirror::PassGate};
use crate::{
    config::Config,
    diagnostics::{self, LogLevel},
    fsutil,
    storage::{
        Catalog,
        admin::{AdminError, BucketName, GenerationCondition},
        sync_state::{
            OpaqueId, Ordinal, PageLimit, SyncBinding,
            deletions::{Candidate, Confirmed, DeletionAuthorization, DeletionSet, Guard, Removal},
            observation::PathStop,
            root::{Baseline, RootError, RootLease},
        },
    },
};
use std::path::Path;

mod presence;
pub(super) use presence::{Missing, Presence, Unobservable, presence};

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
    inventory_with(root, config, pass, Interrupt::new(stopping)).await
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
) -> Result<Option<DeletionSet>, SyncError> {
    if interrupt.check() != Interrupted::No {
        return Ok(None);
    }
    if !pass.hold().infers_deletions() {
        root.invalidate_deletions().await?;
        return Ok(None);
    }
    let guard = Guard::from(config.sync_delete_guard);
    let mut inventory = root.begin_deletion_inventory(guard).await?;
    loop {
        let mut unobserved = Unobserved::default();
        let Some(counted) =
            count_candidates(root, &mut inventory, interrupt, &mut unobserved).await?
        else {
            return Ok(None);
        };
        if counted == Confirmed::Same {
            unobserved.warn();
        }
        let new_set = inventory.writes_new_set();
        if counted == Confirmed::Same
            && let Some(set) = root.finish_deletion_inventory(inventory.clone()).await?
        {
            if new_set {
                warn_awaiting_approval(&set);
            }
            return Ok(Some(set));
        }
        // The root's earlier set differs from what the tree shows now, so a new one replaces it.
        inventory = root.begin_new_deletion_inventory(guard).await?;
    }
}

/// Warns that `set` waits for approval, with the commands that inspect and approve it, which a
/// wait gives too. Nothing else reports a guarded set until someone asks for the status.
fn warn_awaiting_approval(set: &DeletionSet) {
    match set.authorization {
        DeletionAuthorization::Granted => {}
        DeletionAuthorization::Withheld => {
            let revision = set.revision.to_hex();
            let at_hand = AtHand {
                deletion_revision: Some(&revision),
                ..AtHand::default()
            };
            diagnostics::emit(
                LogLevel::Warn,
                "sync_deletion_approval_required",
                serde_json::json!({
                    "revision":revision,"candidates":set.candidate_count,"baseline":set.baseline_count,
                    "remedy":WaitReason::DeletionApproval.remedy(None, at_hand),
                }),
            );
        }
    }
}

/// What a deletion set deleted: how many objects, and the paths of the first few, which the log
/// names. Nothing else reports an automatic deletion after it ran.
#[derive(Debug)]
pub(super) struct Applied {
    revision: OpaqueId,
    count: u64,
    paths: Vec<String>,
}

impl Applied {
    /// Paths the log names; the rest are only counted.
    const NAMED: usize = 10;

    fn of(set: &DeletionSet) -> Self {
        Self {
            revision: set.revision,
            count: 0,
            paths: Vec::new(),
        }
    }

    fn record(&mut self, path: &str) {
        self.count += 1;
        if self.paths.len() < Self::NAMED {
            self.paths.push(path.to_owned());
        }
    }

    /// Logs the deletions, if there were any, at `level`.
    pub(super) fn report(&self, level: LogLevel) {
        if self.count == 0 {
            return;
        }
        let named = u64::try_from(self.paths.len()).unwrap_or(u64::MAX);
        diagnostics::emit(
            level,
            "sync_deletions_applied",
            serde_json::json!({
                "revision":self.revision.to_hex(),"deleted":self.count,
                "paths":self.paths,"omitted":self.count.saturating_sub(named),
            }),
        );
    }
}

/// Walks every tracked live file and records the absent ones in `inventory`. `None` means the
/// pass was interrupted; `Diverged` that the earlier set this inventory confirms differs.
async fn count_candidates(
    root: &RootLease,
    inventory: &mut crate::storage::sync_state::deletions::Inventory,
    interrupt: Interrupt<'_>,
    unobserved: &mut Unobserved,
) -> Result<Option<Confirmed>, SyncError> {
    let naming = root.naming()?;
    let mut after = String::new();
    let mut gone_buckets = std::collections::BTreeSet::new();
    loop {
        if interrupt.check() != Interrupted::No {
            return Ok(None);
        }
        let page = root
            .deletion_inventory_page(inventory.clone(), after.clone())
            .await?;
        if page.is_empty() {
            return Ok(Some(Confirmed::Same));
        }
        let baseline = page.len() as u64;
        let mut missing = Vec::new();
        for candidate in page {
            if interrupt.check() != Interrupted::No {
                return Ok(None);
            }
            after.clone_from(&candidate.path);
            let presence = match mapping::path_to_name(Path::new(&candidate.path), naming) {
                Ok((bucket, name)) if bucket == candidate.bucket && name == candidate.name => {
                    presence(root, candidate.path.clone()).await?
                }
                Ok(_) | Err(_) => Presence::Unobservable(Unobservable::NameMismatch),
            };
            match presence {
                Presence::Absent(absence) => {
                    let missing_file = absence.of(candidate);
                    if missing_file.removal() == Removal::BucketDirectory
                        && gone_buckets.insert(missing_file.candidate().bucket.clone())
                    {
                        bucket_directory_gone(&missing_file.candidate().bucket);
                    }
                    missing.push(missing_file);
                }
                Presence::Present => {}
                Presence::Unobservable(reason) => {
                    unobservable(root, &candidate, &reason, unobserved).await?;
                }
            }
        }
        if record_missing(root, inventory, baseline, missing).await? == Confirmed::Diverged {
            return Ok(Some(Confirmed::Diverged));
        }
    }
}

/// Records one page of the inventory: `baseline` tracked live files, of which `missing` were
/// seen absent. Only a path seen empty becomes a candidate.
async fn record_missing(
    root: &RootLease,
    inventory: &mut crate::storage::sync_state::deletions::Inventory,
    baseline: u64,
    missing: Vec<Missing>,
) -> Result<Confirmed, SyncError> {
    let removal = missing
        .iter()
        .map(Missing::removal)
        .fold(Removal::Files, Removal::and);
    let candidates = missing.into_iter().map(Missing::into_candidate).collect();
    Ok(root
        .record_deletion_candidates(inventory, baseline, candidates, removal)
        .await?)
}

/// Reports a bucket whose whole directory is gone while the tree tracks files in it. The
/// inventory takes those files to approval whatever their number, and nothing recreates the
/// directory meanwhile.
fn bucket_directory_gone(bucket: &BucketName) {
    diagnostics::emit(
        LogLevel::Warn,
        "sync_bucket_directory_missing",
        serde_json::json!({
            "bucket":bucket,"path":bucket,
            "reason":"The directory of a bucket with synchronized files is missing",
            "remedy":"Put the directory, or the mount that holds it, back to keep the objects; to delete them, approve the pending deletion set",
        }),
    );
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
    let missing = root
        .file_job(move |root| {
            Ok(matches!(
                fsutil::child_directory(
                    root,
                    selected.as_str().as_ref(),
                    fsutil::DirectoryOpen::Existing,
                ),
                Ok(fsutil::Child::Missing)
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
    remedy: &'static str,
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
        .count)
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
            let missing = match presence(root, candidate.path.clone()).await? {
                Presence::Absent(absence) => absence.of(candidate),
                Presence::Present | Presence::Unobservable(_) => {
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
                    super::mirror::restore_from_catalog(
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
                super::mirror::create_directory(
                    root,
                    std::path::PathBuf::from(candidate.bucket.as_str()),
                )
                .await?;
            }
            applied.record(&candidate.path);
        }
    }
    Ok(applied)
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
