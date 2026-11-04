//! Missing tracked files become candidates only after an error-free complete scan.

use super::interrupt::{Interrupt, Interrupted};
use super::{Reconciliation, mapping};
use crate::{
    config::Config,
    fsutil,
    storage::{
        Catalog,
        admin::{AdminError, BucketName, GenerationCondition, ObjectName},
        sync_state::{
            SyncBinding,
            deletions::{Candidate, DeletionSet},
            observation::PathStop,
            root::{RootError, RootLease},
        },
    },
};
use std::{io, path::Path};

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

enum Deleted {
    Yes(SyncBinding),
    Stale,
    Protected(SyncBinding),
}

pub(super) fn stale(error: &io::Error) -> bool {
    error
        .get_ref()
        .and_then(|error| error.downcast_ref::<RootError>())
        .is_some_and(|error| {
            matches!(
                error,
                RootError::Admin(AdminError::Precondition | AdminError::NotFound)
            )
        })
}

pub(super) async fn absent(root: &RootLease, path: String) -> io::Result<bool> {
    root.file_job(move |root| {
        let path = Path::new(&path);
        if path.is_absolute()
            || path.iter().count() > 128
            || path
                .iter()
                .any(|part| part == "." || part == ".." || part.as_encoded_bytes().len() > 255)
        {
            return Err(AdminError::Precondition);
        }
        let parent_path = path.parent().ok_or(AdminError::Precondition)?;
        let directory =
            match super::tree::parent(root, parent_path, super::tree::ParentOpen::Existing) {
                Ok(directory) => directory,
                Err(super::tree::ParentError::Missing) => return Ok(true),
                Err(super::tree::ParentError::Finding(_)) => return Err(AdminError::Precondition),
                Err(super::tree::ParentError::Io(error)) => return Err(error.into()),
            };
        let name = path.file_name().ok_or(AdminError::Precondition)?;
        match fsutil::child_regular(&directory, name) {
            Ok(fsutil::RegularChild::File(_)) => Ok(false),
            Ok(fsutil::RegularChild::Missing) => Ok(true),
            Ok(fsutil::RegularChild::Alias) => Err(AdminError::Precondition),
            Err(error) => Err(AdminError::backend(error)),
        }
    })
    .await
    .map_err(io::Error::other)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(super) async fn inventory(
    root: &RootLease,
    config: &Config,
    summary: &Reconciliation,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> io::Result<Option<DeletionSet>> {
    inventory_with(root, config, summary, Interrupt::new(stopping)).await
}
pub(super) async fn inventory_with(
    root: &RootLease,
    config: &Config,
    summary: &Reconciliation,
    interrupt: Interrupt<'_>,
) -> io::Result<Option<DeletionSet>> {
    if interrupt.check() != Interrupted::No {
        return Ok(None);
    }
    root.invalidate_deletions()
        .await
        .map_err(io::Error::other)?;
    if !summary.baseline_complete
        || summary.deferred != 0
        || summary.unrepresentable != 0
        || summary.conflicting != 0
        || interrupt.check() != Interrupted::No
    {
        return Ok(None);
    }
    let inventory = root
        .begin_deletion_inventory(config.sync_delete_guard.into())
        .await
        .map_err(io::Error::other)?;
    let mut after = String::new();
    loop {
        if interrupt.check() != Interrupted::No {
            return Ok(None);
        }
        let page = root
            .deletion_inventory_page(inventory.clone(), after.clone())
            .await
            .map_err(io::Error::other)?;
        if page.is_empty() {
            break;
        }
        let baseline = page.len() as u64;
        let mut candidates = Vec::new();
        for candidate in page {
            if interrupt.check() != Interrupted::No {
                return Ok(None);
            }
            after = candidate.path.clone();
            let Ok((bucket, name)) = mapping::path_to_name(Path::new(&candidate.path)) else {
                return Ok(None);
            };
            if bucket.as_str() != candidate.bucket || name.as_str() != candidate.name {
                return Ok(None);
            }
            if absent(root, candidate.path.clone()).await? {
                candidates.push(candidate)
            }
        }
        root.record_deletion_candidates(inventory.clone(), baseline, candidates)
            .await
            .map_err(io::Error::other)?;
    }
    if interrupt.check() != Interrupted::No {
        return Ok(None);
    }
    Ok(Some(
        root.finish_deletion_inventory(inventory)
            .await
            .map_err(io::Error::other)?,
    ))
}

pub(super) async fn execute(
    catalog: &Catalog,
    root: &RootLease,
    set: DeletionSet,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> io::Result<u64> {
    execute_with(catalog, root, set, Interrupt::new(stopping)).await
}
pub(super) async fn execute_with(
    catalog: &Catalog,
    root: &RootLease,
    set: DeletionSet,
    interrupt: Interrupt<'_>,
) -> io::Result<u64> {
    if set.authorization != 1 {
        return Ok(0);
    }
    let mut after = 0;
    let mut completed = 0;
    let mut restored_buckets = std::collections::BTreeSet::new();
    loop {
        if interrupt.check() != Interrupted::No {
            return Ok(completed);
        }
        let page = match root
            .deletion_page(set.revision.clone(), after, 256)
            .await
            .map_err(io::Error::other)
        {
            Ok(page) => page,
            Err(error) if stale(&error) => {
                root.invalidate_deletions()
                    .await
                    .map_err(io::Error::other)?;
                return Ok(completed);
            }
            Err(error) => return Err(error),
        };
        if page.is_empty() {
            break;
        }
        for candidate in page {
            if interrupt.check() != Interrupted::No {
                return Ok(completed);
            }
            after = candidate.ordinal;
            if !absent(root, candidate.path.clone()).await? {
                root.invalidate_deletions()
                    .await
                    .map_err(io::Error::other)?;
                return Ok(completed);
            }
            if interrupt.check() != Interrupted::No {
                return Ok(completed);
            }
            match delete(root, &set, &candidate).await? {
                Deleted::Yes(binding) => {
                    let reasons = [
                        PathStop::ProtectedDeletion,
                        PathStop::SourceChanging,
                        PathStop::IoFailure,
                    ];
                    root.clear_path_stops(
                        binding,
                        ObjectName::new(candidate.name.clone()).map_err(io::Error::other)?,
                        &reasons,
                    )
                    .await
                    .map_err(io::Error::other)?;
                }
                Deleted::Stale => {
                    root.invalidate_deletions()
                        .await
                        .map_err(io::Error::other)?;
                    return Ok(completed);
                }
                Deleted::Protected(binding) => {
                    root.record_path_stop(
                        binding,
                        ObjectName::new(candidate.name.clone()).map_err(io::Error::other)?,
                        PathStop::ProtectedDeletion,
                    )
                    .await
                    .map_err(io::Error::other)?;
                    root.invalidate_deletions()
                        .await
                        .map_err(io::Error::other)?;
                    super::mirror::restore_from_catalog(
                        catalog,
                        root,
                        &BucketName::new(candidate.bucket.clone()).map_err(io::Error::other)?,
                        &ObjectName::new(candidate.name.clone()).map_err(io::Error::other)?,
                    )
                    .await?;
                    crate::diagnostics::emit(
                        crate::diagnostics::LogLevel::Warn,
                        "sync_protected_deletion",
                        serde_json::json!({"bucket":candidate.bucket,"object":candidate.name,"generation":candidate.generation,"remedy":"The API object remains protected. Its file was restored where safe; release the hold or retention through the API, then delete the file again to retry"}),
                    );
                    return Ok(completed);
                }
            }
            if restored_buckets.insert(candidate.bucket.clone()) {
                super::mirror::create_directory(root, std::path::PathBuf::from(&candidate.bucket))
                    .await?;
            }
            completed += 1;
        }
    }
    Ok(completed)
}

async fn delete(root: &RootLease, set: &DeletionSet, candidate: &Candidate) -> io::Result<Deleted> {
    let bucket = BucketName::new(candidate.bucket.clone()).map_err(io::Error::other)?;
    let name = ObjectName::new(candidate.name.clone()).map_err(io::Error::other)?;
    let binding = match root.sync_binding(&bucket).await {
        Ok(binding) => binding,
        Err(AdminError::NotFound | AdminError::Precondition) => return Ok(Deleted::Stale),
        Err(error) => return Err(io::Error::other(error)),
    };
    let binding = root.deletion_binding(binding, set.revision.clone(), candidate.ordinal);
    match root
        .sync_admin()
        .delete_synced_object(
            &bucket,
            &name,
            GenerationCondition::Is(
                crate::storage::admin::Generation::new(candidate.generation)
                    .map_err(io::Error::other)?,
            ),
            binding.clone(),
        )
        .await
    {
        Ok(()) => Ok(Deleted::Yes(binding)),
        Err(AdminError::Hold) => Ok(Deleted::Protected(binding)),
        Err(AdminError::Precondition | AdminError::NotFound) => Ok(Deleted::Stale),
        Err(error) => Err(io::Error::other(error)),
    }
}
