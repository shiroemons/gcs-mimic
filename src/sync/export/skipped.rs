//! The exports a live run skipped for a later pass to finish.

use super::{LiveObject, MissingFile, Occasion};
use crate::sync::mirror::LiveScope;
use crate::{
    storage::admin::{BucketName, Generation, ObjectName},
    sync::{
        Reconciliation, SyncError,
        interrupt::{Interrupt, Interrupted},
    },
};

/// A live export skipped because preserving the file it would replace failed when that file or
/// the API object changed meanwhile, or because that file changed again after preservation, by
/// the generations it was planned with. Its journal change is already behind the cursor: a later
/// API change is replayed, but nothing else exports it again after a changed file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::sync) struct SkippedExport {
    /// The API generation the export brings to the tree; `None` removes the tracked file.
    pub(in crate::sync) api: Option<Generation>,
    /// The generation the tree tracked.
    pub(in crate::sync) tracked: Option<Generation>,
}

/// The skipped exports of a live run, by object, which it runs again once the tree settles.
#[derive(Default)]
pub(in crate::sync) struct SkippedExports(
    std::collections::BTreeMap<(BucketName, ObjectName), SkippedExport>,
);

impl SkippedExports {
    /// Whether every export of this run reached the tree or was superseded.
    pub(in crate::sync) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Keeps the outcome of an object's latest export: a skipped one to run again, or none.
    pub(in crate::sync) fn record(
        &mut self,
        object: (BucketName, ObjectName),
        outcome: Option<SkippedExport>,
    ) {
        match outcome {
            Some(skipped) => {
                self.0.insert(object, skipped);
            }
            None => {
                self.0.remove(&object);
            }
        }
    }

    /// Runs each skipped export again while the API and the tree still hold the generations it
    /// was planned with. Another change settled the rest: a journal replay exports a newer API
    /// state, and an ingested edit or a finished export moves the tracked generation. An
    /// interruption or a failure keeps the exports it did not reach for the next pass. Returns
    /// the counts of the retried exports, of which a pass takes only those held for approval
    /// ([`Reconciliation::carry_held`]).
    pub(in crate::sync) async fn retry(
        &mut self,
        scope: LiveScope<'_>,
        interrupt: Interrupt<'_>,
    ) -> Result<Reconciliation, SyncError> {
        let mut uncounted = Reconciliation::default();
        if self.is_empty() {
            return Ok(uncounted);
        }
        let fence = scope.catalog.current_fence().await?;
        let pending: Vec<_> = self
            .0
            .iter()
            .map(|(object, skipped)| (object.clone(), *skipped))
            .collect();
        for (object, skipped) in pending {
            if interrupt.check() != Interrupted::No {
                break;
            }
            let (bucket, name) = &object;
            let mut live = LiveObject {
                catalog: scope.catalog,
                root: scope.root,
                bucket,
                name,
                summary: &mut uncounted,
                missing: MissingFile::Inventory,
                occasion: Occasion::Change,
            };
            let outcome = match live.plan().await? {
                Some(plan) if plan.skipped() == Some(skipped) => {
                    live.export(scope.writeback, fence, plan).await?
                }
                Some(_) | None => None,
            };
            self.record(object, outcome);
        }
        Ok(uncounted)
    }
}
