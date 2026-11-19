//! Findings whose cause can go without a journal change of their own object, such as another
//! API object or an entry in the tree that stands in its way. Each ingestion observes those
//! objects again and exports the ones whose cause went; a finding that still holds only reads.
use super::{
    LiveObject, LiveScope, MissingFile, Occasion, Reconciliation, SkippedExports, SyncError,
};
use crate::storage::{
    admin::{BucketName, ObjectName},
    sync_state::MAX_PAGE,
};
use crate::sync::interrupt::{Interrupt, Interrupted};

impl SkippedExports {
    /// Observes again every finding that [`observed_again`] admits and exports each object
    /// whose cause went, so that removing what stood in its way needs no restart. A finding
    /// that cannot be observed now, such as behind a directory the pass may not open, keeps its
    /// row for a later pass and stops nothing else. Returns the counts of the exports this ran;
    /// the findings it records again are counted by the pass that first met them.
    ///
    /// [`observed_again`]: crate::storage::sync_state::findings::FindingKind::observed_again
    pub(super) async fn observe_findings(
        &mut self,
        scope: LiveScope<'_>,
        interrupt: Interrupt<'_>,
    ) -> Result<Reconciliation, SyncError> {
        let mut observed = Reconciliation::default();
        let fence = scope.catalog.current_fence().await?;
        let mut after = 0;
        loop {
            let page = scope.root.findings_page(after, MAX_PAGE).await?;
            let Some(last) = page.last() else {
                break;
            };
            after = last.ordinal;
            let full = page.len() == MAX_PAGE;
            for finding in page {
                if !finding.reason.observed_again() {
                    continue;
                }
                if interrupt.check() != Interrupted::No {
                    observed.interrupted = true;
                    return Ok(observed);
                }
                // A finding names a bucket and an object the API accepted.
                let (Ok(bucket), Ok(name)) = (
                    BucketName::new(finding.bucket),
                    ObjectName::new(finding.name),
                ) else {
                    continue;
                };
                let mut object = LiveObject {
                    catalog: scope.catalog,
                    root: scope.root,
                    bucket: &bucket,
                    name: &name,
                    summary: &mut observed,
                    missing: MissingFile::Inventory,
                    occasion: Occasion::Finding,
                };
                let outcome = match object.plan().await? {
                    Some(plan) => object.export(scope.writeback, fence, plan).await?,
                    None => None,
                };
                self.record((bucket, name), outcome);
            }
            if !full {
                break;
            }
        }
        Ok(observed)
    }
}
