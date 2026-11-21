//! Writing an object's file back from the catalog.

use super::{
    LiveObject, Occasion,
    retry::{advance_with_retry, concurrent_change},
};
use crate::{
    storage::{
        Catalog,
        admin::{BucketName, ObjectName},
        sync_state::{
            root::RootLease,
            writeback::{InstallOutcome, LiveSource},
        },
    },
    sync::{Reconciliation, SyncError},
};

/// What a restore from the catalog left at the object's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) enum Restored {
    /// The object's file, written back from the object.
    Written,
    /// What was put at the path meanwhile: a file, which is taken in as any file is, or an
    /// entry that keeps the object out of the tree as a finding.
    Kept,
    /// Nothing was written: the object, its bucket or its tracked file changed or went
    /// meanwhile, and the export of that change takes over; or the tree changed under every
    /// attempt, and the next inventory takes in what it holds then.
    Skipped,
}

/// How many times a restore plans again from the tree when the tree changed under its
/// installation ([`InstallOutcome::Raced`]). Each attempt reads the tree afresh and runs every check
/// of an export, so a retry never writes over what the user put at the path meanwhile.
const RESTORE_ATTEMPTS: usize = 3;

/// Writes the object's file back from the catalog where its path is still empty, as the restore
/// after a protected or rejected deletion does.
pub(in crate::sync) async fn restore_from_catalog(
    catalog: &Catalog,
    root: &RootLease,
    bucket: &BucketName,
    name: &ObjectName,
) -> Result<Restored, SyncError> {
    for _ in 0..RESTORE_ATTEMPTS {
        if let Some(restored) = restore_once(catalog, root, bucket, name).await? {
            return Ok(restored);
        }
    }
    Ok(Restored::Skipped)
}

/// One restore attempt; `None` when the tree changed under its installation.
async fn restore_once(
    catalog: &Catalog,
    root: &RootLease,
    bucket: &BucketName,
    name: &ObjectName,
) -> Result<Option<Restored>, SyncError> {
    let fence = catalog.current_fence().await?;
    let writeback = root.default_writeback().await?;
    // A restore reports no counts of its own.
    let mut uncounted = Reconciliation::default();
    let mut object = LiveObject {
        catalog,
        root,
        bucket,
        name,
        summary: &mut uncounted,
        occasion: Occasion::Restore,
    };
    let Some(plan) = object.plan().await? else {
        return Ok(Some(Restored::Skipped));
    };
    let Some(source) = plan.source else {
        return Ok(Some(Restored::Skipped));
    };
    let operation = match writeback
        .begin_live_export(
            plan.binding,
            name.clone(),
            plan.generation,
            plan.tracked,
            fence,
            LiveSource::ProtectedRestore(source),
        )
        .await
    {
        Ok(operation) => operation,
        Err(error) if concurrent_change(&error) => return Ok(Some(Restored::Skipped)),
        Err(error) => return Err(error.into()),
    };
    loop {
        match advance_with_retry(&writeback, operation.clone()).await? {
            InstallOutcome::Pending => {}
            InstallOutcome::Complete => return Ok(Some(Restored::Written)),
            InstallOutcome::Conflict | InstallOutcome::Collision(_) => {
                return Ok(Some(Restored::Kept));
            }
            InstallOutcome::Obsolete => return Ok(Some(Restored::Skipped)),
            InstallOutcome::Raced => return Ok(None),
        }
    }
}
