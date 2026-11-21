//! Writing an object's file back from the catalog.

use super::{Content, LiveObject, Occasion, retry::advance_with_retry};
use crate::{
    storage::{
        Catalog,
        admin::{AdminError, BucketName, ObjectName},
        sync_state::{
            root::RootLease,
            writeback::{InstallOutcome, LiveSource},
        },
    },
    sync::{Reconciliation, SyncError, backoff::RetryBudget, failure::FailureClass},
};

/// What a restore from the catalog left at the object's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sync) enum Restored {
    /// The object's file is at its path: written back from the object, or put back with the
    /// object's bytes meanwhile.
    Written,
    /// What was put at the path meanwhile: a file, which is taken in as any file is, or an
    /// entry that keeps the object out of the tree as a finding.
    Kept,
    /// The object is no longer in the API, so nothing is left to write back.
    Gone,
    /// Nothing was written and the object is still in the API: a directory on its path kept
    /// failing to open, the object or its tracked file changed meanwhile, the export found itself
    /// obsolete or one of its invariants broken, an entry the export does not replace holds the
    /// path, or the tree changed under every attempt. A later restore reads the tree and the
    /// object again.
    Skipped,
}

/// Writes the object's file back from the catalog where its path is still empty, as the restore
/// after a protected or rejected deletion does.
pub(in crate::sync) async fn restore_from_catalog(
    catalog: &Catalog,
    root: &RootLease,
    bucket: &BucketName,
    name: &ObjectName,
) -> Result<Restored, SyncError> {
    // A restore plans again from the tree when the tree changed under its installation
    // ([`InstallOutcome::Raced`]). Each attempt reads the tree afresh and runs every check of an
    // export, so a retry never writes over what the user put at the path meanwhile.
    for _ in 0..RetryBudget::EXPORT.attempts() {
        match restore_once(catalog, root, bucket, name).await? {
            Some(restored @ (Restored::Written | Restored::Kept | Restored::Gone)) => {
                return Ok(restored);
            }
            Some(Restored::Skipped) => break,
            None => {}
        }
    }
    // Nothing was written. Only the API tells an object that went, whose file nothing is left
    // to write back, from one whose write-back a later restore tries again.
    match root.sync_admin().get_object(bucket, name, None).await {
        Ok(_) => Ok(Restored::Skipped),
        Err(AdminError::NotFound) => Ok(Restored::Gone),
        Err(error) => Err(error.into()),
    }
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
        return Ok(Some(match plan.content {
            Content::Identical => Restored::Written,
            // A removal: the object went, which the check after the attempts reads.
            Content::Changed => Restored::Skipped,
        }));
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
        // A superseded step skips the restore, a broken invariant among them, as a race does.
        Err(error) if FailureClass::of_writeback(&error) == Some(FailureClass::Superseded) => {
            return Ok(Some(Restored::Skipped));
        }
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
